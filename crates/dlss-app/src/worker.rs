use crossbeam_channel::{Receiver, Sender, unbounded};
use dlss_catalog::{CatalogCacheIndex, GithubCatalogClient, OfficialAsset, ReleaseRefresh};
#[cfg(windows)]
use dlss_core::now_unix;
use dlss_core::{DllInspector, GameId, GameInstall, ReleaseId, TargetProfile};
#[cfg(windows)]
use dlss_core::{GameLocator, KnownDirectories, PrivilegeBroker};
use std::{collections::HashMap, path::PathBuf, thread};

pub enum Command {
    Scan,
    RefreshCatalog,
    InspectRelease(ReleaseId),
    UpgradeLatest(GameId),
    ApplyProfile(GameId, TargetProfile),
    UndoLast(GameId),
    AddRoot(PathBuf),
    RemoveRoot(PathBuf),
    // Constructed only in Windows builds; the field is still read cross-platform.
    #[cfg_attr(not(windows), allow(dead_code))]
    ChangeIndicator(IndicatorRequest),
    Shutdown,
}

/// A prepared request to change the DLSS indicator. The caller performs the
/// stale-hash confirmation on the UI thread and passes the resolved parameters
/// here so the slow, blocking elevation runs off the UI thread.
#[cfg_attr(not(windows), allow(dead_code))]
pub struct IndicatorRequest {
    pub desired: dlss_core::SystemToolState,
    /// A restore point to roll back to; `None` requests applying `desired`.
    pub restore_point: Option<dlss_core::ToolRestorePoint>,
    pub expected_current_hash: [u8; 32],
    pub allow_stale_restore: bool,
}

pub struct UpgradeReport {
    pub changed: usize,
    pub failed: usize,
    pub release: String,
    pub can_undo: bool,
    pub warning: Option<String>,
    undo_plan: Option<dlss_core::OperationPlan>,
}

pub struct CatalogSnapshot {
    pub latest: Option<String>,
    pub releases: Vec<dlss_core::CachedRelease>,
}

pub enum Event {
    ScanStarted,
    ScanFinished(Result<Vec<GameInstall>, String>),
    CatalogStarted,
    CatalogFinished(Result<CatalogSnapshot, String>),
    ReleaseFinished(Result<dlss_core::CachedRelease, String>),
    ReleaseProgress {
        id: ReleaseId,
        state: dlss_core::ReleaseState,
        received: u64,
        total: Option<u64>,
    },
    UpgradeStarted(GameId),
    UpgradeFinished {
        game_id: GameId,
        game: Option<GameInstall>,
        result: Result<UpgradeReport, String>,
    },
    IndicatorFinished(Result<dlss_core::ToolChangeResult, String>),
}

pub struct Worker {
    pub commands: Sender<Command>,
    pub events: Receiver<Event>,
}

/// Wraps the event channel so every send also wakes the egui UI thread. Without
/// the repaint request, events would sit in the channel until the next unrelated
/// input event, making scans and toasts appear frozen.
#[derive(Clone)]
struct EventSink {
    events: Sender<Event>,
    ctx: eframe::egui::Context,
}

impl EventSink {
    fn send(&self, event: Event) {
        let _ = self.events.send(event);
        self.ctx.request_repaint();
    }
}

impl Worker {
    pub fn start(custom_roots: Vec<PathBuf>, ctx: eframe::egui::Context) -> Self {
        let (commands_tx, commands_rx) = unbounded();
        let (events_tx, events_rx) = unbounded();
        let events = EventSink {
            events: events_tx,
            ctx,
        };
        thread::Builder::new()
            .name("dlss-background-worker".into())
            .spawn(move || run(commands_rx, events, custom_roots))
            .expect("background worker thread can start");
        Self {
            commands: commands_tx,
            events: events_rx,
        }
    }
}

fn run(commands: Receiver<Command>, events: EventSink, mut roots: Vec<PathBuf>) {
    let mut games: Vec<GameInstall> = Vec::new();
    let catalog_path = catalog_index_path();
    let mut catalog = catalog_path
        .as_deref()
        .map(CatalogCacheIndex::load)
        .transpose()
        .unwrap_or_default()
        .unwrap_or_default();
    let mut assets = catalog.assets.clone();
    let mut undo_plans = HashMap::new();
    while let Ok(command) = commands.recv() {
        let command_name = match &command {
            Command::Scan => "scan",
            Command::RefreshCatalog => "refresh_catalog",
            Command::InspectRelease(_) => "inspect_release",
            Command::UpgradeLatest(_) => "upgrade_latest",
            Command::ApplyProfile(_, _) => "apply_profile",
            Command::UndoLast(_) => "undo_last",
            Command::AddRoot(_) => "add_root",
            Command::RemoveRoot(_) => "remove_root",
            Command::ChangeIndicator(_) => "change_indicator",
            Command::Shutdown => "shutdown",
        };
        let span = tracing::info_span!("worker_command", command = command_name);
        let _entered = span.enter();
        tracing::info!("worker command started");
        match command {
            Command::Scan => scan(&events, &roots, &mut games),
            Command::RefreshCatalog => {
                refresh_catalog(&events, &mut assets, &mut catalog, catalog_path.as_deref())
            }
            Command::InspectRelease(release_id) => {
                let progress_events = events.clone();
                let progress_id = release_id.clone();
                let result = assets
                    .iter()
                    .find(|asset| asset.release.id == release_id)
                    .ok_or_else(|| "release is no longer in the official index".to_owned())
                    .and_then(|asset| {
                        inspect_release(asset, |state, received, total| {
                            progress_events.send(Event::ReleaseProgress {
                                id: progress_id.clone(),
                                state,
                                received,
                                total,
                            });
                        })
                    });
                if result.is_err() {
                    events.send(Event::ReleaseProgress {
                        id: release_id,
                        state: dlss_core::ReleaseState::Invalid,
                        received: 0,
                        total: None,
                    });
                }
                if let Ok(release) = &result {
                    catalog.upsert_release(release.clone());
                    if let Some(path) = &catalog_path {
                        let _ = catalog.save(path);
                    }
                }
                events.send(Event::ReleaseFinished(result));
            }
            Command::UpgradeLatest(game_id) => {
                events.send(Event::UpgradeStarted(game_id.clone()));
                let Some(game) = games.iter().find(|game| game.id == game_id).cloned() else {
                    events.send(Event::UpgradeFinished {
                        game_id,
                        game: None,
                        result: Err("game is no longer present in the scan".into()),
                    });
                    continue;
                };
                let progress_events = events.clone();
                let result = latest_asset(&assets)
                    .ok_or_else(|| "official release metadata is not available".to_owned())
                    .and_then(|asset| {
                        let id = asset.release.id.clone();
                        upgrade_game(&game, asset, |state, received, total| {
                            progress_events.send(Event::ReleaseProgress {
                                id: id.clone(),
                                state,
                                received,
                                total,
                            });
                        })
                    });
                if let Ok(report) = &result
                    && let Some(plan) = &report.undo_plan
                {
                    undo_plans.insert(game_id.clone(), plan.clone());
                }
                let rescanned = rescan_game(game).ok();
                if let Some(fresh) = &rescanned
                    && let Some(existing) = games.iter_mut().find(|game| game.id == fresh.id)
                {
                    *existing = fresh.clone();
                }
                events.send(Event::UpgradeFinished {
                    game_id,
                    game: rescanned,
                    result,
                });
            }
            Command::ApplyProfile(game_id, profile) => {
                events.send(Event::UpgradeStarted(game_id.clone()));
                let Some(game) = games.iter().find(|game| game.id == game_id).cloned() else {
                    events.send(Event::UpgradeFinished {
                        game_id,
                        game: None,
                        result: Err("game is no longer present in the scan".into()),
                    });
                    continue;
                };
                let progress_events = events.clone();
                let result = latest_asset(&assets)
                    .ok_or_else(|| "official release metadata is not available".to_owned())
                    .and_then(|asset| {
                        let cached: Vec<_> = catalog
                            .releases
                            .iter()
                            .flat_map(|release| release.dlls.iter().cloned())
                            .collect();
                        let id = asset.release.id.clone();
                        apply_profile(&game, asset, &cached, &profile, |state, received, total| {
                            progress_events.send(Event::ReleaseProgress {
                                id: id.clone(),
                                state,
                                received,
                                total,
                            });
                        })
                    });
                if let Ok(report) = &result
                    && let Some(plan) = &report.undo_plan
                {
                    undo_plans.insert(game_id.clone(), plan.clone());
                }
                let rescanned = rescan_game(game).ok();
                if let Some(fresh) = &rescanned
                    && let Some(existing) = games.iter_mut().find(|game| game.id == fresh.id)
                {
                    *existing = fresh.clone();
                }
                events.send(Event::UpgradeFinished {
                    game_id,
                    game: rescanned,
                    result,
                });
            }
            Command::UndoLast(game_id) => {
                events.send(Event::UpgradeStarted(game_id.clone()));
                let Some(game) = games.iter().find(|game| game.id == game_id).cloned() else {
                    events.send(Event::UpgradeFinished {
                        game_id,
                        game: None,
                        result: Err("game is no longer present in the scan".into()),
                    });
                    continue;
                };
                let result = undo_plans
                    .remove(&game_id)
                    .ok_or_else(|| "the immediate undo plan is no longer available".to_owned())
                    .and_then(|plan| undo_game(&game, plan));
                let rescanned = rescan_game(game).ok();
                if let Some(fresh) = &rescanned
                    && let Some(existing) = games.iter_mut().find(|game| game.id == fresh.id)
                {
                    *existing = fresh.clone();
                }
                events.send(Event::UpgradeFinished {
                    game_id,
                    game: rescanned,
                    result,
                });
            }
            Command::AddRoot(root) => {
                if !roots.contains(&root) {
                    roots.push(root);
                }
                scan(&events, &roots, &mut games);
            }
            Command::RemoveRoot(root) => {
                roots.retain(|candidate| candidate != &root);
                scan(&events, &roots, &mut games);
            }
            Command::ChangeIndicator(request) => {
                events.send(Event::IndicatorFinished(change_indicator(request)));
            }
            Command::Shutdown => break,
        }
    }
}

fn scan(events: &EventSink, roots: &[PathBuf], games: &mut Vec<GameInstall>) {
    events.send(Event::ScanStarted);
    let result = scan_roots(roots).map_err(|error| error.to_string());
    if let Ok(discovered) = &result {
        *games = discovered.clone();
    }
    events.send(Event::ScanFinished(result));
}

fn refresh_catalog(
    events: &EventSink,
    assets: &mut Vec<OfficialAsset>,
    catalog: &mut CatalogCacheIndex,
    catalog_path: Option<&std::path::Path>,
) {
    events.send(Event::CatalogStarted);
    let refresh =
        GithubCatalogClient::new().and_then(|client| client.refresh(catalog.etag.as_deref()));
    let result = merge_catalog_refresh(assets, catalog, catalog_path, refresh);
    if let Err(error) = &result {
        tracing::warn!(%error, "catalog refresh failed");
    }
    events.send(Event::CatalogFinished(result));
}

fn merge_catalog_refresh(
    assets: &mut Vec<OfficialAsset>,
    catalog: &mut CatalogCacheIndex,
    catalog_path: Option<&std::path::Path>,
    refresh: Result<ReleaseRefresh, dlss_catalog::GithubError>,
) -> Result<CatalogSnapshot, String> {
    let refreshed = refresh.and_then(|refresh| {
        match refresh {
            ReleaseRefresh::NotModified => {}
            ReleaseRefresh::Modified {
                assets: updated,
                etag,
            } => {
                catalog.etag = etag;
                catalog.assets = updated.clone();
                *assets = updated;
            }
        }
        if let Some(path) = catalog_path {
            catalog.save(path).map_err(|error| {
                dlss_catalog::GithubError::Io(std::io::Error::other(error.to_string()))
            })?;
        }
        Ok(catalog_snapshot(assets, catalog))
    });
    match refreshed {
        Ok(snapshot) => Ok(snapshot),
        Err(_) if !assets.is_empty() => Ok(catalog_snapshot(assets, catalog)),
        Err(error) => Err(error.to_string()),
    }
}

fn catalog_snapshot(assets: &[OfficialAsset], catalog: &CatalogCacheIndex) -> CatalogSnapshot {
    let mut ordered = assets.to_vec();
    ordered.sort_by(|left, right| {
        (right.release.published_unix, &right.release.tag)
            .cmp(&(left.release.published_unix, &left.release.tag))
    });
    let releases = ordered
        .iter()
        .map(|asset| {
            catalog
                .releases
                .iter()
                .find(|release| release.metadata.id == asset.release.id)
                .cloned()
                .unwrap_or_else(|| dlss_core::CachedRelease {
                    metadata: asset.release.clone(),
                    state: dlss_core::ReleaseState::MetadataOnly,
                    dlls: Vec::new(),
                })
        })
        .collect();
    CatalogSnapshot {
        latest: ordered.first().map(|asset| asset.release.tag.clone()),
        releases,
    }
}

fn latest_asset(assets: &[OfficialAsset]) -> Option<&OfficialAsset> {
    assets
        .iter()
        .max_by_key(|asset| (asset.release.published_unix, &asset.release.tag))
}

fn scan_roots(roots: &[PathBuf]) -> Result<Vec<GameInstall>, dlss_core::CoreError> {
    #[cfg(windows)]
    let inspector: Box<dyn DllInspector> = Box::new(dlss_platform::windows::WindowsDllInspector);
    #[cfg(not(windows))]
    let inspector: Box<dyn DllInspector> = Box::new(dlss_platform::PortablePeInspector);

    #[cfg(windows)]
    let mut games = dlss_platform::windows::WindowsGameLocator.discover()?;
    #[cfg(not(windows))]
    let mut games: Vec<GameInstall> = Vec::new();
    for root in roots {
        let mut game = match dlss_platform::manual_install(root) {
            Ok(game) => game,
            Err(_) => continue,
        };
        let inspected = dlss_platform::scan_game(&game.id, &game.root, inspector.as_ref());
        game.inspection_errors = inspected.iter().filter(|result| result.is_err()).count();
        game.dlls = inspected.into_iter().filter_map(Result::ok).collect();
        if let Some(existing) = games.iter_mut().find(|existing| existing.id == game.id) {
            *existing = game;
        } else {
            games.push(game);
        }
    }
    games.sort_by_key(|game| game.name.to_lowercase());
    Ok(games)
}

#[cfg(windows)]
fn catalog_index_path() -> Option<PathBuf> {
    dlss_platform::windows::WindowsKnownDirectories
        .local_app_data()
        .ok()
        .map(|base| base.join("DLSS Updater/cache/catalog.json"))
}

#[cfg(not(windows))]
fn catalog_index_path() -> Option<PathBuf> {
    None
}

#[cfg(windows)]
fn inspect_release(
    asset: &OfficialAsset,
    progress: impl FnMut(dlss_core::ReleaseState, u64, Option<u64>),
) -> Result<dlss_core::CachedRelease, String> {
    let (_, dlls) = prepare_release(asset, progress)?;
    Ok(dlss_core::CachedRelease {
        metadata: asset.release.clone(),
        state: dlss_core::ReleaseState::Ready,
        dlls,
    })
}

#[cfg(not(windows))]
fn inspect_release(
    _asset: &OfficialAsset,
    _progress: impl FnMut(dlss_core::ReleaseState, u64, Option<u64>),
) -> Result<dlss_core::CachedRelease, String> {
    Err("Authenticode release inspection is available only in Windows builds".into())
}

#[cfg(windows)]
fn upgrade_game(
    game: &GameInstall,
    asset: &OfficialAsset,
    progress: impl FnMut(dlss_core::ReleaseState, u64, Option<u64>),
) -> Result<UpgradeReport, String> {
    let (base, catalog_dlls) = prepare_release(asset, progress)?;
    let plan = dlss_core::plan_strict_upgrades(operation_nonce(), &game.dlls, &catalog_dlls);
    execute_game_plan(game, asset, base, plan)
}

#[cfg(windows)]
fn apply_profile(
    game: &GameInstall,
    asset: &OfficialAsset,
    cached: &[dlss_core::CatalogDll],
    profile: &TargetProfile,
    progress: impl FnMut(dlss_core::ReleaseState, u64, Option<u64>),
) -> Result<UpgradeReport, String> {
    let (base, catalog_dlls) = prepare_release(asset, progress)?;
    let backup_index = dlss_core::BackupStore::new(base.join("backups"))
        .load_index()
        .map_err(|error| error.to_string())?;
    let plan = dlss_core::plan_target_profile(
        operation_nonce(),
        &game.dlls,
        &catalog_dlls,
        cached,
        &backup_index.records,
        profile,
    )
    .map_err(|error| error.to_string())?;
    execute_game_plan(game, asset, base, plan)
}

#[cfg(windows)]
fn prepare_release(
    asset: &OfficialAsset,
    mut progress: impl FnMut(dlss_core::ReleaseState, u64, Option<u64>),
) -> Result<(PathBuf, Vec<dlss_core::CatalogDll>), String> {
    let component = safe_component(&asset.release.id.0)?;
    let directories = dlss_platform::windows::WindowsKnownDirectories;
    let base = directories
        .local_app_data()
        .map_err(|error| error.to_string())?
        .join("DLSS Updater");
    let archive = base.join("cache/archives").join(format!("{component}.zip"));
    let extracted = base.join("cache/releases").join(component);
    tracing::info!(release = %asset.release.tag, archive = %archive.display(), "preparing release");
    let client = GithubCatalogClient::new().map_err(|error| error.to_string())?;
    let mut downloaded_fresh = false;
    if !archive.exists() {
        progress(dlss_core::ReleaseState::Downloading, 0, Some(asset.size));
        client
            .download_with_progress(asset, &archive, |download| {
                progress(
                    dlss_core::ReleaseState::Downloading,
                    download.received,
                    download.total,
                );
            })
            .map_err(|error| error.to_string())?;
        downloaded_fresh = true;
    }
    progress(
        dlss_core::ReleaseState::Downloaded,
        asset.size,
        Some(asset.size),
    );
    let inspector = dlss_platform::windows::WindowsDllInspector;
    let verifier = dlss_platform::windows::WindowsTrustVerifier;
    let extract = |archive: &std::path::Path| {
        dlss_catalog::validate_and_extract(
            archive,
            &extracted,
            asset.release.id.clone(),
            &inspector,
            &verifier,
        )
    };
    progress(
        dlss_core::ReleaseState::Validating,
        asset.size,
        Some(asset.size),
    );
    tracing::info!(release = %asset.release.tag, "validating release archive");
    match extract(&archive) {
        Ok(catalog_dlls) => Ok((base, catalog_dlls)),
        // A cached archive that was never digest-verified may be corrupt or
        // truncated and would fail forever. Discard it and re-download once.
        Err(error)
            if !downloaded_fresh
                && matches!(
                    &error,
                    dlss_catalog::CatalogError::Zip(_)
                        | dlss_catalog::CatalogError::TooLarge
                        | dlss_catalog::CatalogError::InvalidPe(_)
                ) =>
        {
            std::fs::remove_file(&archive).map_err(|error| {
                format!("could not discard the corrupt cached archive: {error}")
            })?;
            progress(dlss_core::ReleaseState::Downloading, 0, Some(asset.size));
            client
                .download_with_progress(asset, &archive, |download| {
                    progress(
                        dlss_core::ReleaseState::Downloading,
                        download.received,
                        download.total,
                    );
                })
                .map_err(|error| error.to_string())?;
            progress(
                dlss_core::ReleaseState::Downloaded,
                asset.size,
                Some(asset.size),
            );
            progress(
                dlss_core::ReleaseState::Validating,
                asset.size,
                Some(asset.size),
            );
            let catalog_dlls = extract(&archive).map_err(|error| error.to_string())?;
            Ok((base, catalog_dlls))
        }
        Err(error) => Err(error.to_string()),
    }
}

#[cfg(windows)]
fn execute_game_plan(
    game: &GameInstall,
    asset: &OfficialAsset,
    base: PathBuf,
    plan: dlss_core::OperationPlan,
) -> Result<UpgradeReport, String> {
    if plan.swaps.is_empty() {
        return Ok(UpgradeReport {
            changed: 0,
            failed: 0,
            release: asset.release.tag.clone(),
            can_undo: false,
            warning: None,
            undo_plan: None,
        });
    }
    let inspector = dlss_platform::windows::WindowsDllInspector;
    let backups = dlss_core::BackupStore::new(base.join("backups"));
    let mut result = dlss_core::execute_plan(
        &plan,
        &inspector,
        &dlss_platform::windows::WindowsAtomicFileReplacer,
        &backups,
        now_unix(),
    );
    let denied: Vec<_> = plan
        .swaps
        .iter()
        .zip(&result.swaps)
        .filter(|(_, outcome)| outcome.denied)
        .map(|(swap, _)| swap.clone())
        .collect();
    let mut warning = None;
    if !denied.is_empty() {
        match elevate_failed(game, &base, denied) {
            Ok(elevated) => {
                for elevated_swap in elevated.swaps {
                    if let Some(existing) = result
                        .swaps
                        .iter_mut()
                        .find(|swap| swap.installation == elevated_swap.installation)
                    {
                        *existing = elevated_swap;
                    }
                }
            }
            Err(error) => warning = Some(error),
        }
    }
    let changed = result
        .swaps
        .iter()
        .filter(|swap| swap.result.is_ok())
        .count();
    let undo_swaps = plan
        .swaps
        .iter()
        .zip(&result.swaps)
        .filter_map(|(swap, outcome)| {
            let installed = outcome.result.as_ref().ok()?;
            let backup = outcome.backup.as_ref()?;
            Some(dlss_core::PlannedSwap {
                game: swap.game.clone(),
                installation: swap.installation.clone(),
                target_path: swap.target_path.clone(),
                expected_sha256: installed.sha256,
                source_path: backup.content_path.clone(),
                source_sha256: backup.sha256,
                comparison: match swap.comparison {
                    dlss_core::Comparison::Upgrade => dlss_core::Comparison::Downgrade,
                    dlss_core::Comparison::Downgrade => dlss_core::Comparison::Upgrade,
                    other => other,
                },
            })
        })
        .collect::<Vec<_>>();
    let can_undo = !undo_swaps.is_empty();
    Ok(UpgradeReport {
        changed,
        failed: result.swaps.len() - changed,
        release: asset.release.tag.clone(),
        can_undo,
        warning,
        undo_plan: can_undo.then(|| dlss_core::OperationPlan {
            nonce: operation_nonce(),
            swaps: undo_swaps,
        }),
    })
}

#[cfg(windows)]
fn elevate_failed(
    game: &GameInstall,
    base: &std::path::Path,
    swaps: Vec<dlss_core::PlannedSwap>,
) -> Result<dlss_core::BatchResult, String> {
    let nonce = operation_nonce();
    let plans = base.join("helper-plans");
    let results = base.join("helper-results");
    std::fs::create_dir_all(&plans).map_err(|error| error.to_string())?;
    std::fs::create_dir_all(&results).map_err(|error| error.to_string())?;
    let result_path = results.join(format!("{nonce}.json"));
    let plan_path = plans.join(format!("{nonce}.json"));
    let plan = dlss_core::ElevatedHelperPlan::FileSwap(dlss_core::ElevatedFilePlan {
        game_id: game.id.clone(),
        game_root: game.root.clone(),
        operation: dlss_core::OperationPlan {
            nonce: nonce.clone(),
            swaps,
        },
        nonce,
        result_path: result_path.clone(),
    });
    dlss_core::write_versioned_json(&plan_path, 1, &plan).map_err(|error| error.to_string())?;
    dlss_platform::windows::WindowsPrivilegeBroker
        .run_elevated(&plan_path)
        .map_err(elevation_error)?;
    let outcome: Result<dlss_core::BatchResult, String> =
        dlss_core::read_versioned_json(&result_path, 1)
            .map_err(|error| format!("the elevated helper did not report a result: {error}"))?;
    outcome.map_err(|message| format!("the elevated helper rejected the plan: {message}"))
}

/// Maps a UAC prompt cancellation to a friendly message and keeps other errors verbatim.
#[cfg(windows)]
fn elevation_error(error: dlss_core::CoreError) -> String {
    match error {
        dlss_core::CoreError::Cancelled => "elevation was cancelled".into(),
        other => other.to_string(),
    }
}

/// Runs the elevated DLSS-indicator change off the UI thread. The caller has
/// already resolved the stale-hash confirmation and target state.
#[cfg(windows)]
fn change_indicator(request: IndicatorRequest) -> Result<dlss_core::ToolChangeResult, String> {
    let base = dlss_platform::windows::WindowsKnownDirectories
        .local_app_data()
        .map_err(|error| error.to_string())?
        .join("DLSS Updater");
    let plans = base.join("helper-plans");
    let results = base.join("helper-results");
    std::fs::create_dir_all(&plans).map_err(|error| error.to_string())?;
    std::fs::create_dir_all(&results).map_err(|error| error.to_string())?;
    let nonce = operation_nonce();
    let result_path = results.join(format!("{nonce}.json"));
    let plan_path = plans.join(format!("{nonce}.json"));
    let plan = dlss_core::ToolChangePlan {
        tool_id: dlss_core::SystemToolId(dlss_core::DLSS_INDICATOR_TOOL_ID.into()),
        desired: request.desired,
        restore_point: request.restore_point,
        expected_current_hash: request.expected_current_hash,
        nonce,
        result_path: result_path.clone(),
        allow_stale_restore: request.allow_stale_restore,
    };
    dlss_core::write_versioned_json(
        &plan_path,
        1,
        &dlss_core::ElevatedHelperPlan::SystemTool(plan),
    )
    .map_err(|error| error.to_string())?;
    dlss_platform::windows::WindowsPrivilegeBroker
        .run_elevated(&plan_path)
        .map_err(elevation_error)?;
    let outcome: Result<dlss_core::ToolChangeResult, String> =
        dlss_core::read_versioned_json(&result_path, 1)
            .map_err(|error| format!("the elevated helper did not report a result: {error}"))?;
    outcome.map_err(|message| format!("the elevated helper rejected the plan: {message}"))
}

#[cfg(not(windows))]
fn change_indicator(_request: IndicatorRequest) -> Result<dlss_core::ToolChangeResult, String> {
    Err("Registry controls are available only in Windows builds".into())
}

#[cfg(windows)]
fn undo_game(_game: &GameInstall, plan: dlss_core::OperationPlan) -> Result<UpgradeReport, String> {
    let base = dlss_platform::windows::WindowsKnownDirectories
        .local_app_data()
        .map_err(|error| error.to_string())?
        .join("DLSS Updater");
    let result = dlss_core::execute_plan(
        &plan,
        &dlss_platform::windows::WindowsDllInspector,
        &dlss_platform::windows::WindowsAtomicFileReplacer,
        &dlss_core::BackupStore::new(base.join("backups")),
        now_unix(),
    );
    let changed = result
        .swaps
        .iter()
        .filter(|swap| swap.result.is_ok())
        .count();
    Ok(UpgradeReport {
        changed,
        failed: result.swaps.len() - changed,
        release: "Undo".into(),
        can_undo: false,
        warning: None,
        undo_plan: None,
    })
}

#[cfg(not(windows))]
fn upgrade_game(
    _game: &GameInstall,
    _asset: &OfficialAsset,
    _progress: impl FnMut(dlss_core::ReleaseState, u64, Option<u64>),
) -> Result<UpgradeReport, String> {
    Err("DLL replacement is available only in Windows builds".into())
}

#[cfg(not(windows))]
fn apply_profile(
    _game: &GameInstall,
    _asset: &OfficialAsset,
    _cached: &[dlss_core::CatalogDll],
    _profile: &TargetProfile,
    _progress: impl FnMut(dlss_core::ReleaseState, u64, Option<u64>),
) -> Result<UpgradeReport, String> {
    Err("DLL replacement is available only in Windows builds".into())
}

#[cfg(not(windows))]
fn undo_game(
    _game: &GameInstall,
    _plan: dlss_core::OperationPlan,
) -> Result<UpgradeReport, String> {
    Err("DLL replacement is available only in Windows builds".into())
}

fn rescan_game(mut game: GameInstall) -> Result<GameInstall, dlss_core::CoreError> {
    #[cfg(windows)]
    let inspector: Box<dyn DllInspector> = Box::new(dlss_platform::windows::WindowsDllInspector);
    #[cfg(not(windows))]
    let inspector: Box<dyn DllInspector> = Box::new(dlss_platform::PortablePeInspector);
    // Tolerate a DLL that momentarily cannot be inspected (e.g. locked by a
    // running game) rather than discarding the whole rescan, matching discovery.
    let inspected = dlss_platform::scan_game(&game.id, &game.root, inspector.as_ref());
    game.inspection_errors = inspected.iter().filter(|result| result.is_err()).count();
    game.dlls = inspected.into_iter().filter_map(Result::ok).collect();
    Ok(game)
}

#[cfg(windows)]
fn safe_component(value: &str) -> Result<&str, String> {
    (!value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_')))
    .then_some(value)
    .ok_or_else(|| "release tag is not a safe cache component".into())
}

#[cfg(windows)]
fn operation_nonce() -> String {
    format!("{:032x}-{:08x}", now_nanos(), std::process::id())
}

#[cfg(windows)]
fn now_nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default()
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Shutdown);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asset(tag: &str, published_unix: i64) -> OfficialAsset {
        OfficialAsset {
            release: dlss_core::ReleaseMetadata {
                id: ReleaseId(tag.into()),
                tag: tag.into(),
                asset_name: format!("streamline-sdk-{tag}.zip"),
                published_unix,
            },
            download_url: format!("https://example.invalid/{tag}.zip"),
            size: 1,
            digest: None,
        }
    }

    #[test]
    fn catalog_snapshot_merges_ready_cache_and_sorts_latest_explicitly() {
        let old = asset("v1", 1);
        let new = asset("v2", 2);
        let ready = dlss_core::CachedRelease {
            metadata: old.release.clone(),
            state: dlss_core::ReleaseState::Ready,
            dlls: Vec::new(),
        };
        let catalog = CatalogCacheIndex {
            releases: vec![ready],
            ..Default::default()
        };
        let snapshot = catalog_snapshot(&[old, new], &catalog);
        assert_eq!(snapshot.latest.as_deref(), Some("v2"));
        assert_eq!(snapshot.releases[0].metadata.tag, "v2");
        assert_eq!(snapshot.releases[1].state, dlss_core::ReleaseState::Ready);
    }

    #[test]
    fn catalog_refresh_falls_back_offline_only_when_cache_exists() {
        let cached = asset("v1", 1);
        let mut assets = vec![cached.clone()];
        let mut catalog = CatalogCacheIndex {
            assets: vec![cached],
            ..Default::default()
        };
        let offline = || {
            Err(dlss_catalog::GithubError::Io(std::io::Error::new(
                std::io::ErrorKind::ConnectionRefused,
                "offline",
            )))
        };
        let snapshot = merge_catalog_refresh(&mut assets, &mut catalog, None, offline()).unwrap();
        assert_eq!(snapshot.latest.as_deref(), Some("v1"));

        assets.clear();
        assert!(merge_catalog_refresh(&mut assets, &mut catalog, None, offline()).is_err());
    }
}
