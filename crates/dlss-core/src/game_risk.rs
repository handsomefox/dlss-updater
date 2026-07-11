//! Advisory game-risk matching.
//!
//! This is the exact 36-row snapshot from
//! <https://raw.githubusercontent.com/Recol/DLSS-Updater-Whitelist/main/whitelist.csv>
//! retrieved on 2026-07-11. Despite that repository's historical name, the
//! upstream project presents these as blacklisted/unsupported games. Its
//! blacklist covers several safety and compatibility reasons (including DLLs
//! replaced on launch and old or game-specific implementations), not confirmed
//! ban outcomes. We therefore expose an advisory risk signal only.

use crate::GameInstall;

/// Advisory copy shared by the detail and mandatory-review warnings.
pub const KNOWN_GAME_RISK_WARNING: &str = "Known online/anti-cheat risk. Replacing game DLLs may be treated as tampering and could result in a ban.";

/// `(snapshot entry, canonical display name)`. Aliases remain separate rows so
/// the vendored snapshot stays auditable and exact.
const KNOWN_GAME_RISKS: [(&str, &str); 36] = [
    ("3DMark", "3DMark"),
    ("Fortnite", "Fortnite"),
    ("The First Descendant", "The First Descendant"),
    ("EVIL DEAD The Game", "EVIL DEAD The Game"),
    ("EvilDead", "EVIL DEAD The Game"),
    ("Escape From Tarkov", "Escape From Tarkov"),
    ("Escape from Tarkov Arena", "Escape from Tarkov Arena"),
    ("Planetside 2", "Planetside 2"),
    ("AFOP", "Avatar: Frontiers of Pandora"),
    ("Back 4 Blood", "Back 4 Blood"),
    ("Squad", "Squad"),
    ("Squad 44", "Squad 44"),
    ("Chivalry 2", "Chivalry 2"),
    ("Call of Duty", "Call of Duty"),
    ("Hunt Showdown", "Hunt Showdown"),
    ("Hunt", "Hunt Showdown"),
    ("Need For Speed Unbound", "Need For Speed Unbound"),
    (
        "StarshipTroopersExtermination",
        "Starship Troopers: Extermination",
    ),
    ("Space Marine 2", "Space Marine 2"),
    ("Dark and Darker", "Dark and Darker"),
    ("Throne and Liberty", "Throne and Liberty"),
    ("War Thunder", "War Thunder"),
    ("The Finals", "The Finals"),
    ("FINAL FANTASY XV", "FINAL FANTASY XV"),
    ("GZW", "Gray Zone Warfare"),
    ("Lords of the Fallen", "Lords of the Fallen"),
    ("Monster Hunter World", "Monster Hunter World"),
    ("For Honor", "For Honor"),
    (
        "Tom Clancy's Rainbow Six Siege",
        "Tom Clancy's Rainbow Six Siege",
    ),
    (
        "FINAL FANTASY TACTICS - The Ivalice Chronicles",
        "FINAL FANTASY TACTICS - The Ivalice Chronicles",
    ),
    ("Battlefield 6", "Battlefield 6"),
    ("Dead by Daylight", "Dead by Daylight"),
    ("7 Days To Die", "7 Days To Die"),
    ("NINJAGAIDEN4", "NINJA GAIDEN 4"),
    ("MonsterHunterWilds", "Monster Hunter Wilds"),
    ("FINAL FANTASY XVI", "FINAL FANTASY XVI"),
];

/// Returns the canonical upstream risk name when either the discovered display
/// name or the installation-directory basename is an exact normalized match.
#[must_use]
pub fn known_game_risk(game: &GameInstall) -> Option<&'static str> {
    let display_name = normalize(&game.name);
    let root_name = game
        .root
        .file_name()
        .map(|name| normalize(&name.to_string_lossy()));
    KNOWN_GAME_RISKS
        .iter()
        .find(|(candidate, _)| {
            let candidate = normalize(candidate);
            candidate == display_name || root_name.as_ref() == Some(&candidate)
        })
        .map(|(_, canonical)| *canonical)
}

fn normalize(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{GameId, StoreKind};
    use std::path::PathBuf;

    fn game(name: &str, root: &str) -> GameInstall {
        GameInstall {
            id: GameId::from(name),
            name: name.into(),
            store: StoreKind::Manual,
            root: PathBuf::from(root),
            dlls: Vec::new(),
            inspection_errors: 0,
        }
    }

    #[test]
    fn normalizes_case_punctuation_and_spacing() {
        assert_eq!(
            known_game_risk(&game("tom clancys rainbow-six siege", "/games/other")),
            Some("Tom Clancy's Rainbow Six Siege")
        );
    }

    #[test]
    fn resolves_folder_alias_to_canonical_name() {
        assert_eq!(
            known_game_risk(&game("Avatar", "/games/AFOP")),
            Some("Avatar: Frontiers of Pandora")
        );
        assert_eq!(
            known_game_risk(&game("Evil Dead", "/games/EvilDead")),
            Some("EVIL DEAD The Game")
        );
    }

    #[test]
    fn matches_display_name_or_root_basename() {
        assert_eq!(
            known_game_risk(&game("THE FINALS", "/games/unrelated")),
            Some("The Finals")
        );
        assert_eq!(
            known_game_risk(&game("Custom label", "/games/MonsterHunterWilds")),
            Some("Monster Hunter Wilds")
        );
    }

    #[test]
    fn rejects_partial_matches_and_parent_directories() {
        assert_eq!(known_game_risk(&game("Fortnite Demo", "/games/safe")), None);
        assert_eq!(known_game_risk(&game("Safe Game", "/Fortnite/safe")), None);
    }
}
