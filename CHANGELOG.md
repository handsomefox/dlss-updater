# Changelog

Releases before 1.1.3 are listed on the [releases page](https://github.com/handsomefox/dlss-updater/releases).

## 1.4.10

- Apply an imported DLL or restore a backup to a game in a protected folder, such as Program Files,
  before any official release has been downloaded. The administrator helper failed with "The system
  cannot find the file specified" when the release cache did not exist.
- One damaged imported DLL no longer hides every import. The damaged one is left out with a warning,
  the others stay usable, and new imports work. Importing the same DLL again replaces the damaged
  one.
- **Restore previous** for the DLSS indicator asks for confirmation when the value was changed
  outside DLSS Updater, then restores it. Clicking it asked for administrator rights and failed
  every time.
- Reviewing staged changes pins **Latest official** to the release shown. A newer release found
  while the review was open could be installed instead.
- When a DLL changes again right after it was replaced, for example because a launcher repaired its
  files, DLSS Updater leaves it alone instead of putting the backup over it. The original is still
  in the restore history.
- Updates installed from this version on are safer with two copies of the app open. A copy that
  starts while another installs an update no longer deletes that download, and a copy that finds
  another copy already installed an update asks you to restart instead of installing its own, which
  could be older. Files you keep beside the app, and logs you renamed to keep, are no longer deleted
  at startup.

## 1.4.9

- Apply an imported DLL. Selecting one always failed with "selected official release is
  unavailable".
- Restore a backup, an imported DLL, or an older cached release without downloading the newest
  official release first. A newly published release blocked those while offline.
- **Undo** asks for administrator rights when the update it reverses needed them, and keeps the
  DLLs it could not put back so you can try again. It failed with access denied.
- List the backups that a change made with administrator rights created. They were missing from
  **Restore**.
- Fail a DLL swap when a launcher rewrites the DLL while the swap runs, instead of overwriting the
  launcher's file.
- Remove the copies staged for administrator rights when a later DLL in the same change fails its
  checks.
- Opening a second copy of the app while the first has an update waiting no longer deletes the
  first copy's download.

## 1.4.8

- Stop waiting forever on a download, update, or undo started after the background worker had
  stopped. The app now says the worker stopped.

## 1.4.7

- Find the data folder the same way in **About** and in the background work. **Open data folder**
  read `%LOCALAPPDATA%` from the environment, which can differ from the folder the backups are in.
- Log a crash with its backtrace before the app closes.
- Get the self-updater, the update window, logging, and the folder lookups from [handsomefox/app-internals](https://github.com/handsomefox/app-internals),
  which all five apps share, instead of a copy inside this app.

## 1.4.6

- Line up icons, tab counts, and the search box with the text and controls beside them, and even
  out the spacing in **About**, **App updates**, **DLL sources**, and **Tools**. The close button
  on every dialog is larger. Thanks to @groozchique for these in #27.
- Rename **App updates…** in **About** to **Check for updates**. It now starts a check as it opens
  **App updates**, unless one is already running. Also from @groozchique in #27.
- Make the **Older**, **Streamline**, **Known risk**, and **Indicator on** chips as tall as the
  badges beside them. They made their rows taller, and grew the toolbar while the DLSS indicator
  was on.
- Put the count after **DLSS up to date** on the same line as the status, and tighten the space
  around the release headings in **App updates**.
- Get the self-updater from its own repository,
  [handsomefox/app-update](https://github.com/handsomefox/app-update), instead of a copy inside
  this app. Checking for, downloading, and installing an update work as before.

## 1.4.5

- Keep GitHub's error page out of the banner when a catalog refresh or a download fails. During
  a GitHub outage the banner filled with the page's HTML. It now names only the status, such as
  "GitHub returned 504 Gateway Timeout".

## 1.4.4

- Show only the requested release's download progress. While an update ran ahead of a download,
  the setup card and **Review DLSS updates** showed the progress of other releases.
- Clear the spinners and enable the buttons again when the background worker stops unexpectedly.
- Leave a Streamline release out of **DLL sources** when GitHub reports a checksum for it that the
  app cannot read. Such a release could stop the background worker.
- Stop leaving a copy of the DLL in the app's data folder when an import fails.
- Refuse an app update whose checksum on GitHub cannot be read, instead of skipping that check.
- Retry putting the current version back when installing an app update fails.

## 1.4.3

- Fix the DLL names in **Review DLSS updates**, which turned almost black on ticked rows in 1.4.1
  and 1.4.2.

## 1.4.2

- New installs check for a new version each time the app starts. Existing installs keep their
  setting. To change it, open **About** > **App updates**.
- Keep a found update visible after a restart. With a check period of a day or longer, a restart
  hid the new version until the next check was due.
- Remove the line in **App updates** that said no check had run, which appeared beside the time
  of the last check.

## 1.4.1

- Give DLSS Updater a new icon, with transparent corners in Explorer, shortcuts, and the
  taskbar. Thanks to groozchique for the corner fix.
- Show the app's icon in the title bar and the taskbar, where egui's default icon appeared
  before. Thanks to groozchique for this fix.
- Update eframe and egui_extras to 0.37. The first launch after this update opens at the default
  window size and position. Later launches remember the window again.
- Update flate2 to 1.1.10, serde_json to 1.0.154, sha2 to 0.11.1, windows-registry to 0.100.0,
  and zip to 9.2.0.

## 1.4.0

- DLSS Updater can now update itself from its GitHub releases. It checks once a day, and when a
  new version is out, a chip in the toolbar names it. Click the chip to read what changed since
  your version, then download the update, skip that version, or leave it for later. The download
  must match its published SHA-256 checksums before it replaces the app, and a restart finishes
  the install. To check more or less often, or never, open **About** > **App updates**.
- Getting 1.4.0 itself is still a manual download. Later versions install from inside the app.

## 1.3.6

- Replace the yanked clipboard-win 5.4.1 with 5.4.3.
- Update tokio to 1.54.1 and webpki-root-certs to 1.0.11.

## 1.3.5

- Stop offering DLSS updates for games on DLSS 1, such as Battlefield V. A DLSS 2 or later DLL
  turns DLSS off in those games. The library shows them as "DLSS 1, can't update", and the
  game's page no longer offers a later DLL for DLSS Super Resolution.
- If an earlier version already updated a DLSS 1 game, open the game's page, choose the
  "Restore backup 1.x" entry for DLSS Super Resolution, and apply the change.
- Update rustls-webpki to 0.103.16 and webpki-root-certs to 1.0.10, which fix RustSec security
  advisories.

## 1.3.4

- Update zip to 9.0.5, rustls to 0.23.46, and ruzstd to 0.9.1, which fix RustSec security advisories.
- Update jiff to 0.2.38 and embed-resource to 3.0.12.

## 1.3.3

- Update thiserror to 2.0.21.

## 1.3.2

- Warn about anti-cheat risk for Assassin's Creed Shadows and Assassin's Creed Black Flag
  Resynced, which the upstream known-risk list added in August.
- Show game folders as `D:\Games\...` rather than `\\?\D:\Games\...` in Game folders, on a
  game's page, and in scan errors.

## 1.3.1

- Fix the release links in DLL sources and the repository link in About, which did not open.
- Stop listing `NvLowLatencyVk.dll` in a game's DLLs. The file has no version, so the app could
  not compare or replace it, and importing it failed.

## 1.3.0

- Update egui from 0.35 to 0.36, along with the library that checks each downloaded DLL is a
  64-bit Windows DLL, the HTTP client, and smaller dependencies.
- Build with Rust 1.98.1.

## 1.2.1

- Fix the Tools dialog growing to the full window height, with its buttons in the middle.
- Make the DLSS indicator options visible, and show disabled buttons as plain instead of
  faded green.

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
- Update rustls to 0.23.45, which fixes a TLS 1.3 handshake flaw (RUSTSEC-2026-0285).

## 1.1.3

- Ship `dlss-updater-<version>-windows-x86_64.zip`, which holds a folder of the same name with
  `dlss-updater.exe`, `README.md`, and `LICENSE` in it, beside a `SHA256SUMS` file. The
  executable used to be `DLSS Updater.exe`, and was also attached on its own. Windows still
  shows it as DLSS Updater.
- Show the release version in the executable's properties. Every release after 1.0.3 still
  said 1.0.3 there.
