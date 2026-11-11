# Changelog

All notable changes to Aurora NG are documented here. The project follows [Semantic Versioning](https://semver.org).

## [0.7.0] - 2026-11-11

### Added

- An app icon: aurora curtains in the app's teal, violet and pink over a night sky, with a pair of music notes. It is built into the exe, so the Start Menu, taskbar, Explorer and the window's title bar show it. Small sizes use a simpler drawing so they stay sharp.
- The exe now carries its name and version ("Aurora NG", 0.7.0), shown in its Properties and in Task Manager instead of "aurora_ng.exe".

 - 2026-11-11

### Added

- Covers 300 pixels or wider show two more lines under the title and artist, without clicking: genre · label · track count · length (like "Action · Varese Sarabande · 15 tracks · 40:09"), then the average rating in stars and the Album Score (like "★★★½ 3.60 · Album Score: 3.60"). The details panel then leaves this info out.

### Changed

- On smaller covers, the details panel header is laid out like Aurora's album detail: a small cover next to the title, artist and year, the info line and the rating line, instead of labeled columns.
- Average rating and Album Score show two decimals, as in Aurora.

## [0.5.0] - 2026-11-11

### Added

- Album details on the cover wall: clicking a cover opens a panel right below its row, pushing the covers underneath down. It shows the album's title, artist and year, its track count, total length, label, average rating in stars and Album Score, and a table of tracks with number, title, artist (only when it differs from the album artist), time, star rating and Love. Click the cover again or press Esc to close it; double-click a track to play the album from there.
- When the details open near the bottom of the window, the wall scrolls just enough to show them, keeping the album's row in view.

### Changed

- The year under a cover is always the album's original year. It no longer falls back to the release year when the year is missing.

## [0.4.0] - 2026-11-11

### Added

- A **Size** slider in the header while the cover wall is shown sets the cover size from 50 to 500 pixels, updating the wall as you drag. The album at the top left stays in view. Below 100 pixels, titles are hidden so more covers fit.

### Changed

- The header says **Aurora NG** (Next Generation) instead of Aurora.
- Cover thumbnails come in four sizes (128, 256, 512 and 1024 pixels). The wall uses the smallest one that is sharp at the current cover size and screen scaling, and keeps showing the smaller one until a bigger one is ready.
- Thumbnail memory is limited to 192 MB instead of 300 thumbnails, since sizes now vary. Covers on screen are never dropped.

## [0.3.0] - 2026-11-11

### Added

- Cover wall: a **Cover wall** button in the header shows every album as a grid of covers. Click a cover to select the album, double-click to play it, and type an artist's name to jump to their albums. Switching back to the columns keeps the album selected.
- Album covers are matched to albums by folder name from `C:\_code\music_backup_v5\AlbumCovers`, or the folder in `AURORA_NG_COVERS`.
- Covers are shrunk to 320 × 320 thumbnails in the background and saved in `%LOCALAPPDATA%\aurora_ng\thumbnails`. Only covers on screen are loaded, and memory use stays bounded however far you scroll.
- The header shows how many albums have a cover.

## [0.2.1] - 2026-11-11

### Changed

- Faster release builds: tests run in release mode so they share compiled dependencies with the build, and a new CI workflow builds and tests every push to `master`, keeping a cache that the release workflow reuses.

## [0.2.0] - 2026-11-11

### Added

- In-app updates: Aurora NG checks GitHub for a newer release when it starts and every 30 minutes after that. The header has a **Check for updates** button, and when an update is found, an **Update to vX.Y.Z** button downloads it, replaces the app and restarts it.
- The header shows the app version.
- A GitHub Actions release workflow: pushing a `vX.Y.Z` tag tests and builds the Windows app and publishes `aurora_ng-windows-x64.exe` as a GitHub Release, with notes taken from this changelog.

## [0.1.0] - 2026-11-11

### Added

- A native Rust + GPUI app that reads Aurora's Music Library catalog read-only (`%APPDATA%\com.local.musiclibrary\music-library.sqlite3`, or the path in `AURORA_NG_CATALOG`).
- Library browser with Artists → Albums → Tracks columns, using virtualized lists for large libraries.
- Type an artist's name anywhere in the window to jump to it.
- Playback with rodio: double-click an album or track to play the album from there, with automatic advance to the next track.
- Now playing bar with previous, play/pause and next, elapsed and total time, and a clickable progress bar for seeking. Space plays or pauses.
