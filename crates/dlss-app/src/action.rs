//! What a click asks the app to do.
//!
//! Views draw from a borrow of [`DlssApp`], so they cannot change it while
//! they draw. They collect [`Action`]s instead, and the code that called the
//! view hands them to [`DlssApp::apply`] as soon as the view is drawn, in the
//! same frame. That keeps each action where it always ran: a dialog's actions
//! apply right after that dialog, before the dialogs below it draw.

use crate::ui::review::ReviewIntent;
use crate::{AppWindow, Command, DlssApp, GameFilter, GameSort, StoreFilter, View};
use eframe::egui;

pub(crate) enum Action {
    ShowLibrary,
    ShowGame(dlss_core::GameId),
    OpenWindow(AppWindow),
    Sort(GameSort),
    /// Selects or clears these library rows, given as indices into
    /// `DlssApp::games`. A click on one row also moves the shift-click
    /// anchor to that row's game.
    Select {
        rows: Vec<usize>,
        selected: bool,
        anchor: Option<dlss_core::GameId>,
    },
    /// Clears whichever filter left the library empty.
    WidenFilter,
    Review(ReviewIntent),
    /// Applies the checked rows of the open review, then closes it.
    ApplyReview,
    CloseReview,
    DownloadRelease(dlss_core::ReleaseId),
    RemoveRelease(dlss_core::ReleaseId),
    RefreshCatalog,
    /// Picks a DLL file and imports it.
    ImportDll,
    RemoveImport([u8; 32]),
    /// Picks a game folder and adds it.
    AddFolder,
    RemoveFolder(std::path::PathBuf),
    ChangeIndicator {
        restore: bool,
    },
    DismissToast,
    UndoLastChange,
    /// A choice in the app updates dialog, or the About window's check.
    AppUpdate(app_update_ui::Action),
}

impl DlssApp {
    pub(crate) fn apply_all(&mut self, ctx: &egui::Context, actions: Vec<Action>) {
        for action in actions {
            self.apply(ctx, action);
        }
    }

    pub(crate) fn apply(&mut self, ctx: &egui::Context, action: Action) {
        match action {
            Action::ShowLibrary => self.view = View::Library,
            Action::ShowGame(id) => self.view = View::Game(id),
            Action::OpenWindow(window) => {
                self.open_windows.insert(window);
            }
            Action::Sort(sort) => self.game_sort = sort,
            Action::Select {
                rows,
                selected,
                anchor,
            } => {
                self.set_selection(selected, Some(&rows));
                if anchor.is_some() {
                    self.selection_anchor = anchor;
                }
            }
            Action::WidenFilter => {
                if !self.filter.is_empty() {
                    self.filter.clear();
                } else if self.store_filter != StoreFilter::All {
                    self.store_filter = StoreFilter::All;
                } else {
                    self.filter_mode = GameFilter::All;
                }
            }
            Action::Review(intent) => self.open_review(intent),
            Action::ApplyReview => {
                if let Some(review) = self.review.take() {
                    self.apply_review(&review);
                }
            }
            Action::CloseReview => self.review = None,
            Action::DownloadRelease(id) => self.start_release_download(id),
            Action::RemoveRelease(id) => {
                let _ = self.worker.commands.send(Command::RemoveRelease(id));
            }
            Action::RefreshCatalog => {
                let _ = self.worker.commands.send(Command::RefreshCatalog);
            }
            Action::ImportDll => {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("Windows DLL", &["dll"])
                    .pick_file()
                {
                    let _ = self.worker.commands.send(Command::ImportDll(path));
                }
            }
            Action::RemoveImport(hash) => {
                let _ = self.worker.commands.send(Command::RemoveImport(hash));
            }
            Action::AddFolder => self.pick_and_add_custom_root(),
            Action::RemoveFolder(root) => self.remove_custom_root(&root),
            Action::ChangeIndicator { restore } => self.change_indicator(restore),
            Action::DismissToast => self.toast = None,
            Action::UndoLastChange => self.undo_last_change(),
            Action::AppUpdate(action) => {
                if app_update_ui::apply(
                    action,
                    ctx,
                    &mut self.updater,
                    &mut self.persisted.app_updates,
                ) {
                    self.open_windows.remove(&AppWindow::Update);
                }
            }
        }
    }
}
