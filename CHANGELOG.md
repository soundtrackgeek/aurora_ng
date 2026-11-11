# Changelog

All notable changes to Aurora NG are documented here. The project follows [Semantic Versioning](https://semver.org).

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
