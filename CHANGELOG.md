# Changelog

Releases before 1.1.3 are listed on the [releases page](https://github.com/handsomefox/dlss-updater/releases).

## 1.2.0

- Show one status per game in the library: an Update button, "Up to date", or the reason
  neither applies. The DLSS column shows the installed version next to the newer one.
- Add select-all boxes to the library and the review dialog. Shift-click selects a range,
  Ctrl+A selects every game shown, Esc clears the selection, and a rescan keeps it.
- Split the library into tabs that count their games. The default tab hides games without
  NVIDIA DLLs.
- Let you remove a game folder you added, even one deleted from disk, and open it in File
  Explorer. Store problems show in the same dialog.
- Show a game's DLLs as one table, with the installed and latest version of each.
- Stop the window flickering during bulk updates and at startup. A bulk update ends in one
  summary instead of a notice per game.
- Keep selection actions, staged changes, and Undo in a bar at the bottom of the window.
- Fix dialogs that stopped at half the window height, the DLL sources list, which gave every
  release the same widget ID, and a download button that showed a broken icon.

## 1.1.3

- Ship `dlss-updater-<version>-windows-x86_64.zip`, which holds a folder of the same name with
  `dlss-updater.exe`, `README.md`, and `LICENSE` in it, beside a `SHA256SUMS` file. The
  executable used to be `DLSS Updater.exe`, and was also attached on its own. Windows still
  shows it as DLSS Updater.
- Show the release version in the executable's properties. Every release after 1.0.3 still
  said 1.0.3 there.
