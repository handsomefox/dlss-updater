use crate::{GameRow, GameSort};

pub(crate) fn sort_rows(rows: &mut [usize], games: &[GameRow], sort: GameSort) {
    rows.sort_by(|left, right| match sort {
        GameSort::Name => games[*left]
            .name
            .to_ascii_lowercase()
            .cmp(&games[*right].name.to_ascii_lowercase()),
        GameSort::DllsAscending => games[*left]
            .dlls
            .cmp(&games[*right].dlls)
            .then_with(|| games[*left].name.cmp(&games[*right].name)),
        GameSort::DllsDescending => games[*right]
            .dlls
            .cmp(&games[*left].dlls)
            .then_with(|| games[*left].name.cmp(&games[*right].name)),
    });
}
