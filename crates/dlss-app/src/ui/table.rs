use crate::{GameRow, GameSort, SortKey};
use std::cmp::Ordering;

pub(crate) fn sort_rows(rows: &mut [usize], games: &[GameRow], sort: GameSort) {
    rows.sort_by(|left, right| {
        let left = &games[*left];
        let right = &games[*right];
        let order = match sort.key {
            SortKey::Name => folded_name(left).cmp(&folded_name(right)),
            SortKey::Store => left.store.cmp(right.store),
            SortKey::Dlls => left.dlls.cmp(&right.dlls),
            SortKey::DlssVersion => compare_optional_version(left.dlss_version, right.dlss_version),
            SortKey::Upgrades => left.upgrades.cmp(&right.upgrades),
            SortKey::State => left.state.cmp(&right.state),
        };
        let order = if sort.ascending && sort.key != SortKey::DlssVersion {
            order
        } else if sort.key == SortKey::DlssVersion {
            match (left.dlss_version, right.dlss_version) {
                (Some(_), Some(_)) if !sort.ascending => order.reverse(),
                _ => order,
            }
        } else {
            order.reverse()
        };
        order.then_with(|| folded_name(left).cmp(&folded_name(right)))
    });
}

fn folded_name(game: &GameRow) -> String {
    game.name.to_ascii_lowercase()
}

fn compare_optional_version(
    left: Option<dlss_core::DllVersion>,
    right: Option<dlss_core::DllVersion>,
) -> Ordering {
    match (left, right) {
        (Some(left), Some(right)) => left.cmp(&right),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

pub(crate) fn sort_header(
    ui: &mut eframe::egui::Ui,
    label: &str,
    key: SortKey,
    sort: GameSort,
) -> Option<GameSort> {
    let marker = sort_marker(key, sort);
    let text = marker.map_or_else(
        || eframe::egui::WidgetText::from(eframe::egui::RichText::new(label).strong()),
        |marker| super::widgets::text_icon(label, marker),
    );
    let direction = if sort.key == key {
        if sort.ascending {
            "ascending; activate to sort descending"
        } else {
            "descending; activate to sort ascending"
        }
    } else {
        "not sorted; activate to sort ascending"
    };
    ui.add(eframe::egui::Button::new(text).frame(false))
        .on_hover_text(format!("Sort by {label}: {direction}"))
        .clicked()
        .then(|| GameSort {
            key,
            ascending: if sort.key == key {
                !sort.ascending
            } else {
                true
            },
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
mod tests {
    use super::*;
    use dlss_core::{GameId, StoreKind};

    fn row(
        name: &str,
        store: &'static str,
        dlls: usize,
        version: Option<dlss_core::DllVersion>,
        upgrades: usize,
        state: &str,
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
            state: state.into(),
            last_operation: "Never".into(),
            details: Vec::new(),
            inspection_errors: 0,
            known_risk: None,
        }
    }

    fn names_for(games: &[GameRow], key: SortKey, ascending: bool) -> Vec<String> {
        let mut rows: Vec<_> = (0..games.len()).collect();
        sort_rows(&mut rows, games, GameSort { key, ascending });
        rows.into_iter()
            .map(|index| games[index].name.clone())
            .collect()
    }

    #[test]
    fn sorts_every_table_column() {
        let games = [
            row(
                "Zulu",
                "Steam",
                3,
                Some(dlss_core::DllVersion::new(3, 0, 0, 0)),
                2,
                "Current",
            ),
            row(
                "alpha",
                "Epic",
                1,
                Some(dlss_core::DllVersion::new(2, 0, 0, 0)),
                4,
                "Error",
            ),
        ];
        assert_eq!(names_for(&games, SortKey::Name, true), ["alpha", "Zulu"]);
        assert_eq!(names_for(&games, SortKey::Store, true), ["alpha", "Zulu"]);
        assert_eq!(names_for(&games, SortKey::Dlls, true), ["alpha", "Zulu"]);
        assert_eq!(
            names_for(&games, SortKey::DlssVersion, false),
            ["Zulu", "alpha"]
        );
        assert_eq!(
            names_for(&games, SortKey::Upgrades, false),
            ["alpha", "Zulu"]
        );
        assert_eq!(names_for(&games, SortKey::State, true), ["Zulu", "alpha"]);
    }

    #[test]
    fn missing_dlss_versions_sort_last_in_both_directions() {
        let games = [
            row("Missing", "Manual", 0, None, 0, "No DLLs"),
            row(
                "Old",
                "Manual",
                1,
                Some(dlss_core::DllVersion::new(2, 0, 0, 0)),
                0,
                "Current",
            ),
            row(
                "New",
                "Manual",
                1,
                Some(dlss_core::DllVersion::new(3, 0, 0, 0)),
                0,
                "Current",
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
