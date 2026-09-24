//! Pure logic behind the library table: row status, sorting, and range
//! selection. Kept free of egui so it can be tested directly.

use crate::{GameRow, GameSort, SortKey};
use std::cmp::Ordering;

/// What a library row tells the user, and what it lets them do.
///
/// Declared in the order the Status column sorts: things to act on first,
/// then things to look at, then things that need nothing.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum RowStatus {
    /// A DLSS DLL has a newer official build.
    UpdateAvailable,
    /// The latest release is not downloaded yet, so nothing can be compared.
    NotChecked,
    /// Some files could not be read during the scan.
    Problem,
    /// DLSS is current, but Streamline or Reflex have newer builds. Those
    /// are opt-in, so the row points at the game page instead of a button.
    OptionalUpdates,
    /// A DLL has no readable version resource.
    VersionUnknown,
    UpToDate,
    NoDlls,
}

pub(crate) fn row_status(game: &GameRow, latest_ready: bool) -> RowStatus {
    if game.inspection_errors > 0 {
        RowStatus::Problem
    } else if game.dlls == 0 {
        RowStatus::NoDlls
    } else if !latest_ready {
        RowStatus::NotChecked
    } else if game.dlss_upgrades > 0 {
        RowStatus::UpdateAvailable
    } else if game.upgrades > 0 {
        RowStatus::OptionalUpdates
    } else if game.has_unknown {
        RowStatus::VersionUnknown
    } else {
        RowStatus::UpToDate
    }
}

pub(crate) fn sort_rows(rows: &mut [usize], games: &[GameRow], sort: GameSort, latest_ready: bool) {
    rows.sort_by(|left, right| {
        let left = &games[*left];
        let right = &games[*right];
        let order = match sort.key {
            SortKey::Name => Ordering::Equal,
            SortKey::Store => left.store.cmp(right.store),
            SortKey::Dlls => left.dlls.cmp(&right.dlls),
            SortKey::DlssVersion => {
                // Games without a DLSS version sort last in both directions,
                // so flipping the order never floods the top with blanks.
                return match (left.dlss_version, right.dlss_version) {
                    (Some(l), Some(r)) if sort.ascending => l.cmp(&r),
                    (Some(l), Some(r)) => r.cmp(&l),
                    (Some(_), None) => Ordering::Less,
                    (None, Some(_)) => Ordering::Greater,
                    (None, None) => Ordering::Equal,
                }
                .then_with(|| folded_name(left).cmp(&folded_name(right)));
            }
            SortKey::Status => row_status(left, latest_ready).cmp(&row_status(right, latest_ready)),
        };
        let order = order.then_with(|| folded_name(left).cmp(&folded_name(right)));
        if sort.ascending {
            order
        } else {
            order.reverse()
        }
    });
}

fn folded_name(game: &GameRow) -> String {
    game.name.to_lowercase()
}

/// The visible rows from the shift-click anchor to the clicked row,
/// inclusive, as positions in `rows`.
///
/// The anchor is a game id rather than a position, because sorting or
/// filtering between two clicks moves rows. When the anchor is no longer
/// visible there is no range, and the click selects one row.
pub(crate) fn selection_range(
    rows: &[usize],
    games: &[GameRow],
    anchor: Option<&dlss_core::GameId>,
    clicked: usize,
) -> Option<std::ops::RangeInclusive<usize>> {
    let anchor = anchor?;
    let from = rows.iter().position(|&index| &games[index].id == anchor)?;
    Some(from.min(clicked)..=from.max(clicked))
}

pub(crate) fn sort_header(
    ui: &mut eframe::egui::Ui,
    label: &str,
    key: SortKey,
    sort: GameSort,
) -> Option<GameSort> {
    use super::theme;
    use eframe::egui;
    super::widgets::table_header_background(ui);
    let active = sort.key == key;
    let color = if active {
        theme::TEXT
    } else {
        theme::TEXT_MUTED
    };
    let mut job = egui::text::LayoutJob::default();
    job.append(
        label,
        0.0,
        egui::TextFormat {
            font_id: egui::FontId::new(12.5, egui::FontFamily::Name("semibold".into())),
            color,
            ..Default::default()
        },
    );
    if let Some(marker) = sort_marker(key, sort) {
        job.append(
            marker,
            4.0,
            egui::TextFormat {
                font_id: theme::icon_font(11.0),
                color: theme::ACCENT,
                ..Default::default()
            },
        );
    }
    let direction = if active {
        if sort.ascending {
            "ascending; activate to sort descending"
        } else {
            "descending; activate to sort ascending"
        }
    } else {
        "not sorted; activate to sort ascending"
    };
    ui.add(egui::Button::new(job).frame(false))
        .on_hover_text(format!("Sort by {label}: {direction}"))
        .clicked()
        .then_some(GameSort {
            key,
            ascending: !active || !sort.ascending,
        })
}

/// Sort direction glyphs from the bundled Phosphor icon font.
fn sort_marker(key: SortKey, sort: GameSort) -> Option<&'static str> {
    use super::theme::icons;
    if sort.key == key {
        if sort.ascending {
            Some(icons::CARET_UP)
        } else {
            Some(icons::CARET_DOWN)
        }
    } else {
        None
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use dlss_core::{GameId, StoreKind};

    pub(crate) fn row(
        name: &str,
        store: &'static str,
        dlls: usize,
        version: Option<dlss_core::DllVersion>,
        upgrades: usize,
    ) -> GameRow {
        GameRow {
            id: GameId(name.into()),
            selected: false,
            name: name.into(),
            store,
            store_kind: StoreKind::Manual,
            root: name.into(),
            dlls,
            dlss_version: version,
            dlss_upgrades: 0,
            upgrades,
            has_unknown: false,
            last_operation: None,
            details: Vec::new(),
            inspection_errors: 0,
            known_risk: None,
        }
    }

    fn names_for(games: &[GameRow], key: SortKey, ascending: bool) -> Vec<String> {
        let mut rows: Vec<_> = (0..games.len()).collect();
        sort_rows(&mut rows, games, GameSort { key, ascending }, true);
        rows.into_iter()
            .map(|index| games[index].name.clone())
            .collect()
    }

    #[test]
    fn sorts_every_table_column() {
        let mut alpha = row(
            "alpha",
            "Epic",
            1,
            Some(dlss_core::DllVersion::new(2, 0, 0, 0)),
            4,
        );
        alpha.dlss_upgrades = 1;
        let games = [
            row(
                "Zulu",
                "Steam",
                3,
                Some(dlss_core::DllVersion::new(3, 0, 0, 0)),
                0,
            ),
            alpha,
        ];
        assert_eq!(names_for(&games, SortKey::Name, true), ["alpha", "Zulu"]);
        assert_eq!(names_for(&games, SortKey::Name, false), ["Zulu", "alpha"]);
        assert_eq!(names_for(&games, SortKey::Store, true), ["alpha", "Zulu"]);
        assert_eq!(names_for(&games, SortKey::Dlls, true), ["alpha", "Zulu"]);
        assert_eq!(
            names_for(&games, SortKey::DlssVersion, false),
            ["Zulu", "alpha"]
        );
        assert_eq!(names_for(&games, SortKey::Status, true), ["alpha", "Zulu"]);
        assert_eq!(names_for(&games, SortKey::Status, false), ["Zulu", "alpha"]);
    }

    #[test]
    fn missing_dlss_versions_sort_last_in_both_directions() {
        let games = [
            row("Missing", "Manual", 0, None, 0),
            row(
                "Old",
                "Manual",
                1,
                Some(dlss_core::DllVersion::new(2, 0, 0, 0)),
                0,
            ),
            row(
                "New",
                "Manual",
                1,
                Some(dlss_core::DllVersion::new(3, 0, 0, 0)),
                0,
            ),
        ];
        assert_eq!(
            names_for(&games, SortKey::DlssVersion, true),
            ["Old", "New", "Missing"]
        );
        assert_eq!(
            names_for(&games, SortKey::DlssVersion, false),
            ["New", "Old", "Missing"]
        );
    }

    #[test]
    fn status_puts_actionable_games_first_and_empty_games_last() {
        let mut update = row("Update", "Steam", 2, None, 3);
        update.dlss_upgrades = 1;
        let optional = row("Optional", "Steam", 2, None, 2);
        let mut problem = row("Problem", "Steam", 0, None, 0);
        problem.inspection_errors = 1;
        let mut unknown = row("Unknown", "Steam", 1, None, 0);
        unknown.has_unknown = true;
        let current = row("Current", "Steam", 1, None, 0);
        let empty = row("Empty", "Steam", 0, None, 0);

        assert_eq!(row_status(&update, true), RowStatus::UpdateAvailable);
        assert_eq!(row_status(&optional, true), RowStatus::OptionalUpdates);
        assert_eq!(row_status(&problem, true), RowStatus::Problem);
        assert_eq!(row_status(&unknown, true), RowStatus::VersionUnknown);
        assert_eq!(row_status(&current, true), RowStatus::UpToDate);
        assert_eq!(row_status(&empty, true), RowStatus::NoDlls);
        // Without a downloaded release, nothing can claim to be up to date.
        assert_eq!(row_status(&current, false), RowStatus::NotChecked);
        assert_eq!(row_status(&empty, false), RowStatus::NoDlls);

        let games = [empty, current, unknown, optional, problem, update];
        assert_eq!(
            names_for(&games, SortKey::Status, true),
            [
                "Update", "Problem", "Optional", "Unknown", "Current", "Empty"
            ]
        );
    }

    #[test]
    fn range_selection_follows_the_anchor_game_through_reordering() {
        let games = [
            row("a", "Steam", 1, None, 0),
            row("b", "Steam", 1, None, 0),
            row("c", "Steam", 1, None, 0),
            row("d", "Steam", 1, None, 0),
        ];
        let anchor = GameId("b".into());
        assert_eq!(
            selection_range(&[0, 1, 2, 3], &games, Some(&anchor), 3),
            Some(1..=3)
        );
        // Reversed order: "b" now sits at position 2, and the range runs upward.
        assert_eq!(
            selection_range(&[3, 2, 1, 0], &games, Some(&anchor), 0),
            Some(0..=2)
        );
        // Anchor filtered out, or never set: no range.
        assert_eq!(selection_range(&[0, 2, 3], &games, Some(&anchor), 1), None);
        assert_eq!(selection_range(&[0, 1, 2, 3], &games, None, 1), None);
    }

    #[test]
    fn sort_markers_show_direction_with_bundled_icons() {
        use crate::ui::theme::icons;
        let active = GameSort {
            key: SortKey::Name,
            ascending: true,
        };
        assert_eq!(sort_marker(SortKey::Name, active), Some(icons::CARET_UP));
        assert_eq!(
            sort_marker(
                SortKey::Name,
                GameSort {
                    ascending: false,
                    ..active
                }
            ),
            Some(icons::CARET_DOWN)
        );
        assert_eq!(sort_marker(SortKey::Store, active), None);
    }
}
