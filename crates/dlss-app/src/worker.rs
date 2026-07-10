use crossbeam_channel::{Receiver, Sender, unbounded};
use dlss_catalog::{CatalogCacheIndex, GithubCatalogClient, OfficialAsset, ReleaseRefresh};
#[cfg(windows)]
use dlss_core::now_unix;
use dlss_core::{DllInspector, GameId, GameInstall, ReleaseId, TargetProfile};
#[cfg(windows)]
use dlss_core::{GameLocator, KnownDirectories, PrivilegeBroker};
use std::{collections::HashMap, path::PathBuf, thread};

#[derive(Debug, thiserror::Error)]
pub(crate) enum WorkerError {
    #[error(transparent)]
    Core(#[from] dlss_core::CoreError),
    #[error(transparent)]
    Catalog(#[from] dlss_catalog::CatalogError),
    #[error(transparent)]
    Github(#[from] dlss_catalog::GithubError),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[cfg(not(windows))]
    #[error("{0}")]
    Unavailable(&'static str),
    #[error("invalid worker state: {0}")]
    State(String),
    #[cfg(windows)]
    #[error("elevation failed: {0}")]
    Elevation(String),
}

type WorkerResult<T> = Result<T, WorkerError>;

pub(crate) enum Command {
    Scan,
    RefreshCatalog,
    InspectRelease(ReleaseId),
    UpgradeLatest(GameId),
    ApplyProfile(GameId, TargetProfile),
    UndoLast(GameId),
    AddRoot(PathBuf),
    #[cfg(windows)]
    ChangeIndicator(IndicatorRequest),
    Shutdown,
}

/// A prepared request to change the DLSS indicator. The caller performs the
/// stale-hash confirmation on the UI thread and passes the resolved parameters
/// here so the slow, blocking elevation runs off the UI thread.
#[cfg(windows)]
pub(crate) struct IndicatorRequest {
    pub desired: dlss_core::SystemToolState,
    /// A restore point to roll back to; `None` requests applying `desired`.
    pub restore_point: Option<dlss_core::ToolRestorePoint>,
    pub expected_current_hash: [u8; 32],
    pub allow_stale_restore: bool,
}

pub(crate) struct UpgradeReport {
    pub changed: usize,
    pub failed: usize,
    pub release: String,
    pub can_undo: bool,
    pub warning: Option<String>,
    undo_plan: Option<dlss_core::OperationPlan>,
}

pub(crate) struct CatalogSnapshot {
    pub latest: Option<String>,
    pub releases: Vec<dlss_core::CachedRelease>,
}

pub(crate) enum Event {
    Warning(String),
    ScanStarted,
    ScanFinished(WorkerResult<Vec<GameInstall>>),
    CatalogStarted,
    CatalogFinished(WorkerResult<CatalogSnapshot>),
    ReleaseFinished(WorkerResult<dlss_core::CachedRelease>),
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
        result: WorkerResult<UpgradeReport>,
    },
    #[cfg(windows)]
    IndicatorFinished(WorkerResult<dlss_core::ToolChangeResult>),
}

pub(crate) struct Worker {
    pub(crate) commands: Sender<Command>,
    pub(crate) events: Receiver<Event>,
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
    pub(crate) fn start(custom_roots: Vec<PathBuf>, ctx: eframe::egui::Context) -> Self {
        let (commands_tx, commands_rx) = unbounded();
        let (events_tx, events_rx) = unbounded();
        let events = EventSink {
            events: events_tx,
            ctx,
        };
        thread::Builder::new()
            .name("dlss-background-worker".into())
            .spawn(move || run(&commands_rx, &events, custom_roots))
            .expect("background worker thread can start");
        Self {
            commands: commands_tx,
            events: events_rx,
        }
    }
}

struct WorkerState {
    roots: Vec<PathBuf>,
    games: Vec<GameInstall>,
    catalog_path: Option<PathBuf>,
    catalog: CatalogCacheIndex,
    assets: Vec<OfficialAsset>,
    undo_plans: HashMap<GameId, dlss_core::OperationPlan>,
}

fn run(commands: &Receiver<Command>, events: &EventSink, mut roots: Vec<PathBuf>) {
    canonicalize_roots(&mut roots);
    let catalog_path = catalog_index_path();
    let catalog = match catalog_path
        .as_deref()
        .map(CatalogCacheIndex::load)
        .transpose()
    {
        Ok(catalog) => catalog.unwrap_or_default(),
        Err(error) => {
            events.send(Event::Warning(format!(
                "Could not load the saved release catalog: {error}"
            )));
            CatalogCacheIndex::default()
        }
    };
    let mut state = WorkerState {
        roots,
        games: Vec::new(),
        catalog_path,
        assets: catalog.assets.clone(),
        catalog,
        undo_plans: HashMap::new(),
    };
    while let Ok(command) = commands.recv() {
        let span = tracing::info_span!("worker_command", command = command_name(&command));
        let _entered = span.enter();
        tracing::info!("worker command started");
        if !dispatch(command, events, &mut state) {
            break;
        }
    }
}

fn command_name(command: &Command) -> &'static str {
    match command {
        Command::Scan => "scan",
        Command::RefreshCatalog => "refresh_catalog",
        Command::InspectRelease(_) => "inspect_release",
        Command::UpgradeLatest(_) => "upgrade_latest",
        Command::ApplyProfile(_, _) => "apply_profile",
        Command::UndoLast(_) => "undo_last",
        Command::AddRoot(_) => "add_root",
        #[cfg(windows)]
        Command::ChangeIndicator(_) => "change_indicator",
        Command::Shutdown => "shutdown",
    }
}

fn dispatch(command: Command, events: &EventSink, state: &mut WorkerState) -> bool {
    match command {
        Command::Scan => scan(events, &state.roots, &mut state.games),
        Command::RefreshCatalog => refresh_catalog(
            events,
            &mut state.assets,
            &mut state.catalog,
            state.catalog_path.as_deref(),
        ),
        Command::InspectRelease(id) => inspect_release_command(id, events, state),
        Command::UpgradeLatest(id) => upgrade_command(id, events, state),
        Command::ApplyProfile(id, profile) => profile_command(id, &profile, events, state),
        Command::UndoLast(id) => undo_command(id, events, state),
        Command::AddRoot(root) => add_root_command(&root, events, state),
        #[cfg(windows)]
        Command::ChangeIndicator(request) => {
            events.send(Event::IndicatorFinished(change_indicator(request)));
        }
        Command::Shutdown => return false,
    }
    true
}

fn inspect_release_command(id: ReleaseId, events: &EventSink, state: &mut WorkerState) {
    let progress_events = events.clone();
    let progress_id = id.clone();
    let result = state
        .assets
        .iter()
        .find(|asset| asset.release.id == id)
        .ok_or_else(|| WorkerError::State("release is no longer in the official index".into()))
        .and_then(|asset| {
            inspect_release(asset, |release_state, received, total| {
                progress_events.send(Event::ReleaseProgress {
                    id: progress_id.clone(),
                    state: release_state,
                    received,
                    total,
                });
            })
        });
    if result.is_err() {
        events.send(Event::ReleaseProgress {
            id,
            state: dlss_core::ReleaseState::Invalid,
            received: 0,
            total: None,
        });
    }
    if let Ok(release) = &result {
        state.catalog.upsert_release(release.clone());
        if let Some(path) = &state.catalog_path
            && let Err(error) = state.catalog.save(path)
        {
            events.send(Event::Warning(format!(
                "Release was validated, but the catalog could not be saved: {error}"
            )));
        }
    }
    events.send(Event::ReleaseFinished(result));
}

fn upgrade_command(id: GameId, events: &EventSink, state: &mut WorkerState) {
    let Some(game) = begin_game_operation(&id, events, state) else {
        return;
    };
    let progress_events = events.clone();
    let result = latest_asset(&state.assets)
        .ok_or_else(|| WorkerError::State("official release metadata is not available".into()))
        .and_then(|asset| {
            let release_id = asset.release.id.clone();
            upgrade_game(&game, asset, |release_state, received, total| {
                progress_events.send(Event::ReleaseProgress {
                    id: release_id.clone(),
                    state: release_state,
                    received,
                    total,
                });
            })
        });
    finish_game_operation(id, game, result, events, state);
}

fn profile_command(
    id: GameId,
    profile: &TargetProfile,
    events: &EventSink,
    state: &mut WorkerState,
) {
    let Some(game) = begin_game_operation(&id, events, state) else {
        return;
    };
    let progress_events = events.clone();
    let cached: Vec<_> = state
        .catalog
        .releases
        .iter()
        .flat_map(|release| release.dlls.iter().cloned())
        .collect();
    let result = latest_asset(&state.assets)
        .ok_or_else(|| WorkerError::State("official release metadata is not available".into()))
        .and_then(|asset| {
            let release_id = asset.release.id.clone();
            apply_profile(
                &game,
                asset,
                &cached,
                profile,
                |release_state, received, total| {
                    progress_events.send(Event::ReleaseProgress {
                        id: release_id.clone(),
                        state: release_state,
                        received,
                        total,
                    });
                },
            )
        });
    finish_game_operation(id, game, result, events, state);
}

fn undo_command(id: GameId, events: &EventSink, state: &mut WorkerState) {
    let Some(game) = begin_game_operation(&id, events, state) else {
        return;
    };
    let result = state
        .undo_plans
        .remove(&id)
        .ok_or_else(|| WorkerError::State("the immediate undo plan is no longer available".into()))
        .and_then(|plan| undo_game(&game, &plan));
    finish_game_operation(id, game, result, events, state);
}

fn begin_game_operation(
    id: &GameId,
    events: &EventSink,
    state: &WorkerState,
) -> Option<GameInstall> {
    events.send(Event::UpgradeStarted(id.clone()));
    let game = state.games.iter().find(|game| game.id == *id).cloned();
    if game.is_none() {
        events.send(Event::UpgradeFinished {
            game_id: id.clone(),
            game: None,
            result: Err(WorkerError::State(
                "game is no longer present in the scan".into(),
            )),
        });
    }
    game
}

fn finish_game_operation(
    id: GameId,
    game: GameInstall,
    result: WorkerResult<UpgradeReport>,
    events: &EventSink,
    state: &mut WorkerState,
) {
    if let Ok(report) = &result
        && let Some(plan) = &report.undo_plan
    {
        state.undo_plans.insert(id.clone(), plan.clone());
    }
    let fresh = rescan_game(game);
    if fresh.inspection_errors > 0 {
        events.send(Event::Warning(format!(
            "The operation finished, but {} DLL or traversal entries could not be inspected",
            fresh.inspection_errors
        )));
    }
    if let Some(existing) = state.games.iter_mut().find(|game| game.id == fresh.id) {
        existing.clone_from(&fresh);
    }
    events.send(Event::UpgradeFinished {
        game_id: id,
        game: Some(fresh),
        result,
    });
}

fn add_root_command(root: &std::path::Path, events: &EventSink, state: &mut WorkerState) {
    match root.canonicalize() {
        Ok(root) if !state.roots.contains(&root) => state.roots.push(root),
        Ok(_) => {}
        Err(error) => events.send(Event::Warning(format!(
            "Could not add game folder {}: {error}",
            root.display()
        ))),
    }
    scan(events, &state.roots, &mut state.games);
}

fn canonicalize_roots(roots: &mut Vec<PathBuf>) {
    let mut normalized = Vec::with_capacity(roots.len());
    for root in roots.drain(..) {
        let root = root.canonicalize().unwrap_or(root);
        if !normalized.contains(&root) {
            normalized.push(root);
        }
    }
    *roots = normalized;
}

fn scan(events: &EventSink, roots: &[PathBuf], games: &mut Vec<GameInstall>) {
    events.send(Event::ScanStarted);
    let (discovered, warnings) = scan_roots(roots);
    games.clone_from(&discovered);
    for warning in warnings {
        events.send(Event::Warning(warning));
    }
    events.send(Event::ScanFinished(Ok(discovered)));
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
    match result {
        Ok((snapshot, warning)) => {
            if let Some(warning) = warning {
                tracing::warn!(%warning, "using cached release catalog");
                events.send(Event::Warning(warning));
            }
            events.send(Event::CatalogFinished(Ok(snapshot)));
        }
        Err(error) => {
            tracing::warn!(%error, "catalog refresh failed");
            events.send(Event::CatalogFinished(Err(error)));
        }
    }
}

fn merge_catalog_refresh(
    assets: &mut Vec<OfficialAsset>,
    catalog: &mut CatalogCacheIndex,
    catalog_path: Option<&std::path::Path>,
    refresh: Result<ReleaseRefresh, dlss_catalog::GithubError>,
) -> WorkerResult<(CatalogSnapshot, Option<String>)> {
    let refreshed: WorkerResult<CatalogSnapshot> = (|| {
        let refresh = refresh?;
        match refresh {
            ReleaseRefresh::NotModified => {}
            ReleaseRefresh::Modified {
                assets: updated,
                etag,
            } => {
                catalog.etag = etag;
                catalog.assets.clone_from(&updated);
                *assets = updated;
            }
        }
        if let Some(path) = catalog_path {
            catalog.save(path)?;
        }
        Ok(catalog_snapshot(assets, catalog))
    })();
    match refreshed {
        Ok(snapshot) => Ok((snapshot, None)),
        Err(error) if !assets.is_empty() => Ok((
            catalog_snapshot(assets, catalog),
            Some(format!(
                "Catalog refresh failed; using saved metadata: {error}"
            )),
        )),
        Err(error) => Err(error),
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

fn scan_roots(roots: &[PathBuf]) -> (Vec<GameInstall>, Vec<String>) {
    #[cfg(windows)]
    let inspector: Box<dyn DllInspector> = Box::new(dlss_platform::windows::WindowsDllInspector);
    #[cfg(not(windows))]
    let inspector: Box<dyn DllInspector> = Box::new(dlss_platform::PortablePeInspector);

    #[cfg(windows)]
    let (mut games, mut warnings) = match dlss_platform::windows::WindowsGameLocator.discover() {
        Ok(games) => (games, Vec::new()),
        Err(error) => (
            Vec::new(),
            vec![format!("Automatic game discovery failed: {error}")],
        ),
    };
    #[cfg(not(windows))]
    let mut games: Vec<GameInstall> = Vec::new();
    #[cfg(not(windows))]
    let mut warnings = Vec::new();
    for root in roots {
        let mut game = match dlss_platform::manual_install(root) {
            Ok(game) => game,
            Err(error) => {
                warnings.push(format!("Could not scan {}: {error}", root.display()));
                continue;
            }
        };
        let inspected = dlss_platform::scan_game(&game.id, &game.root, inspector.as_ref());
        for error in inspected.iter().filter_map(|result| result.as_ref().err()) {
            warnings.push(format!("{}: {error}", game.name));
        }
        game.inspection_errors = inspected.iter().filter(|result| result.is_err()).count();
        game.dlls = inspected.into_iter().filter_map(Result::ok).collect();
        if let Some(existing) = games.iter_mut().find(|existing| existing.id == game.id) {
            *existing = game;
        } else {
            games.push(game);
        }
    }
    games.sort_by_key(|game| game.name.to_lowercase());
    (games, warnings)
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
) -> WorkerResult<dlss_core::CachedRelease> {
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
) -> WorkerResult<dlss_core::CachedRelease> {
    Err(WorkerError::Unavailable(
        "Authenticode release inspection is available only in Windows builds",
    ))
}

#[cfg(windows)]
fn upgrade_game(
    game: &GameInstall,
    asset: &OfficialAsset,
    progress: impl FnMut(dlss_core::ReleaseState, u64, Option<u64>),
) -> WorkerResult<UpgradeReport> {
    let (base, catalog_dlls) = prepare_release(asset, progress)?;
    let plan = dlss_core::plan_strict_upgrades(operation_nonce(), &game.dlls, &catalog_dlls);
    Ok(execute_game_plan(game, asset, &base, &plan))
}

#[cfg(windows)]
fn apply_profile(
    game: &GameInstall,
    asset: &OfficialAsset,
    cached: &[dlss_core::CatalogDll],
    profile: &TargetProfile,
    progress: impl FnMut(dlss_core::ReleaseState, u64, Option<u64>),
) -> WorkerResult<UpgradeReport> {
    let (base, catalog_dlls) = prepare_release(asset, progress)?;
    let backup_index = dlss_core::BackupStore::new(base.join("backups")).load_index()?;
    let plan = dlss_core::plan_target_profile(
        operation_nonce(),
        &game.dlls,
        &catalog_dlls,
        cached,
        &backup_index.records,
        profile,
    )?;
    Ok(execute_game_plan(game, asset, &base, &plan))
}

#[cfg(windows)]
fn prepare_release(
    asset: &OfficialAsset,
    mut progress: impl FnMut(dlss_core::ReleaseState, u64, Option<u64>),
) -> WorkerResult<(PathBuf, Vec<dlss_core::CatalogDll>)> {
    let component = safe_component(&asset.release.id.0)?;
    let directories = dlss_platform::windows::WindowsKnownDirectories;
    let base = directories.local_app_data()?.join("DLSS Updater");
    let archive = base.join("cache/archives").join(format!("{component}.zip"));
    let extracted = base.join("cache/releases").join(component);
    tracing::info!(release = %asset.release.tag, archive = %archive.display(), "preparing release");
    let client = GithubCatalogClient::new()?;
    let mut downloaded_fresh = false;
    if archive.exists()
        && let Err(error) = dlss_catalog::validate_cached_archive(asset, &archive)
    {
        tracing::warn!(release = %asset.release.tag, %error, "discarding invalid cached archive");
        std::fs::remove_file(&archive)?;
    }
    if !archive.exists() {
        progress(dlss_core::ReleaseState::Downloading, 0, Some(asset.size));
        client.download_with_progress(asset, &archive, |download| {
            progress(
                dlss_core::ReleaseState::Downloading,
                download.received,
                download.total,
            );
        })?;
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
            &asset.release.id,
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
            std::fs::remove_file(&archive)?;
            progress(dlss_core::ReleaseState::Downloading, 0, Some(asset.size));
            client.download_with_progress(asset, &archive, |download| {
                progress(
                    dlss_core::ReleaseState::Downloading,
                    download.received,
                    download.total,
                );
            })?;
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
            let catalog_dlls = extract(&archive)?;
            Ok((base, catalog_dlls))
        }
        Err(error) => Err(error.into()),
    }
}

#[cfg(windows)]
fn execute_game_plan(
    game: &GameInstall,
    asset: &OfficialAsset,
    base: &std::path::Path,
    plan: &dlss_core::OperationPlan,
) -> UpgradeReport {
    if plan.swaps.is_empty() {
        return UpgradeReport {
            changed: 0,
            failed: 0,
            release: asset.release.tag.clone(),
            can_undo: false,
            warning: None,
            undo_plan: None,
        };
    }
    let inspector = dlss_platform::windows::WindowsDllInspector;
    let backups = dlss_core::BackupStore::new(base.join("backups"));
    let mut result = dlss_core::execute_plan(
        plan,
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
        match elevate_failed(game, base, denied) {
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
            Err(error) => warning = Some(error.to_string()),
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
    UpgradeReport {
        changed,
        failed: result.swaps.len() - changed,
        release: asset.release.tag.clone(),
        can_undo,
        warning,
        undo_plan: can_undo.then(|| dlss_core::OperationPlan {
            nonce: operation_nonce(),
            swaps: undo_swaps,
        }),
    }
}

#[cfg(windows)]
fn elevate_failed(
    game: &GameInstall,
    base: &std::path::Path,
    swaps: Vec<dlss_core::PlannedSwap>,
) -> WorkerResult<dlss_core::BatchResult> {
    let nonce = operation_nonce();
    let plans = base.join("helper-plans");
    let results = base.join("helper-results");
    std::fs::create_dir_all(&plans)?;
    std::fs::create_dir_all(&results)?;
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
    let outcome = (|| {
        dlss_core::write_versioned_json(&plan_path, 1, &plan)?;
        dlss_platform::windows::WindowsPrivilegeBroker
            .run_elevated(&plan_path)
            .map_err(elevation_error)?;
        let outcome: Result<dlss_core::BatchResult, String> =
            dlss_core::read_versioned_json(&result_path, 1).map_err(|error| {
                WorkerError::Elevation(format!(
                    "the elevated helper did not report a result: {error}"
                ))
            })?;
        outcome.map_err(|message| {
            WorkerError::Elevation(format!("the elevated helper rejected the plan: {message}"))
        })
    })();
    cleanup_helper_files(&plan_path, &result_path);
    outcome
}

/// Maps a UAC prompt cancellation to a friendly message and keeps other errors verbatim.
#[cfg(windows)]
fn elevation_error(error: dlss_core::CoreError) -> WorkerError {
    match error {
        dlss_core::CoreError::Cancelled => WorkerError::Elevation("elevation was cancelled".into()),
        other => WorkerError::Elevation(other.to_string()),
    }
}

/// Runs the elevated DLSS-indicator change off the UI thread. The caller has
/// already resolved the stale-hash confirmation and target state.
#[cfg(windows)]
fn change_indicator(request: IndicatorRequest) -> WorkerResult<dlss_core::ToolChangeResult> {
    let base = dlss_platform::windows::WindowsKnownDirectories
        .local_app_data()?
        .join("DLSS Updater");
    let plans = base.join("helper-plans");
    let results = base.join("helper-results");
    std::fs::create_dir_all(&plans)?;
    std::fs::create_dir_all(&results)?;
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
    let outcome = (|| {
        dlss_core::write_versioned_json(
            &plan_path,
            1,
            &dlss_core::ElevatedHelperPlan::SystemTool(plan),
        )?;
        dlss_platform::windows::WindowsPrivilegeBroker
            .run_elevated(&plan_path)
            .map_err(elevation_error)?;
        let outcome: Result<dlss_core::ToolChangeResult, String> =
            dlss_core::read_versioned_json(&result_path, 1).map_err(|error| {
                WorkerError::Elevation(format!(
                    "the elevated helper did not report a result: {error}"
                ))
            })?;
        outcome.map_err(|message| {
            WorkerError::Elevation(format!("the elevated helper rejected the plan: {message}"))
        })
    })();
    cleanup_helper_files(&plan_path, &result_path);
    outcome
}

#[cfg(windows)]
fn undo_game(_game: &GameInstall, plan: &dlss_core::OperationPlan) -> WorkerResult<UpgradeReport> {
    let base = dlss_platform::windows::WindowsKnownDirectories
        .local_app_data()?
        .join("DLSS Updater");
    let result = dlss_core::execute_plan(
        plan,
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
) -> WorkerResult<UpgradeReport> {
    Err(WorkerError::Unavailable(
        "DLL replacement is available only in Windows builds",
    ))
}

#[cfg(not(windows))]
fn apply_profile(
    _game: &GameInstall,
    _asset: &OfficialAsset,
    _cached: &[dlss_core::CatalogDll],
    _profile: &TargetProfile,
    _progress: impl FnMut(dlss_core::ReleaseState, u64, Option<u64>),
) -> WorkerResult<UpgradeReport> {
    Err(WorkerError::Unavailable(
        "DLL replacement is available only in Windows builds",
    ))
}

#[cfg(not(windows))]
fn undo_game(_game: &GameInstall, _plan: &dlss_core::OperationPlan) -> WorkerResult<UpgradeReport> {
    Err(WorkerError::Unavailable(
        "DLL replacement is available only in Windows builds",
    ))
}

fn rescan_game(mut game: GameInstall) -> GameInstall {
    #[cfg(windows)]
    let inspector: Box<dyn DllInspector> = Box::new(dlss_platform::windows::WindowsDllInspector);
    #[cfg(not(windows))]
    let inspector: Box<dyn DllInspector> = Box::new(dlss_platform::PortablePeInspector);
    // Tolerate a DLL that momentarily cannot be inspected (e.g. locked by a
    // running game) rather than discarding the whole rescan, matching discovery.
    let inspected = dlss_platform::scan_game(&game.id, &game.root, inspector.as_ref());
    game.inspection_errors = inspected.iter().filter(|result| result.is_err()).count();
    game.dlls = inspected.into_iter().filter_map(Result::ok).collect();
    game
}

#[cfg(windows)]
fn safe_component(value: &str) -> WorkerResult<&str> {
    (!value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_')))
    .then_some(value)
    .ok_or_else(|| WorkerError::State("release tag is not a safe cache component".into()))
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

#[cfg(windows)]
fn cleanup_helper_files(plan: &std::path::Path, result: &std::path::Path) {
    for path in [plan, result] {
        if let Err(error) = std::fs::remove_file(path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(path = %path.display(), %error, "could not clean helper staging file");
        }
    }
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
        let (snapshot, warning) =
            merge_catalog_refresh(&mut assets, &mut catalog, None, offline()).unwrap();
        assert_eq!(snapshot.latest.as_deref(), Some("v1"));
        assert!(warning.is_some());

        assets.clear();
        assert!(merge_catalog_refresh(&mut assets, &mut catalog, None, offline()).is_err());
    }

    #[test]
    fn custom_roots_are_canonicalized_and_deduplicated() {
        let directory = tempfile::tempdir().unwrap();
        let child = directory.path().join("game");
        std::fs::create_dir(&child).unwrap();
        let alias = child.join("..").join("game");
        let mut roots = vec![child.clone(), alias];
        canonicalize_roots(&mut roots);
        assert_eq!(roots, vec![child.canonicalize().unwrap()]);
    }
}
