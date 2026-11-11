# Changelog

All notable changes to Aurora NG are documented here. The project follows [Semantic Versioning](https://semver.org).

## [0.1.0] - 2026-11-11

### Added

- A native Rust + GPUI app that reads Aurora's Music Library catalog read-only (`%APPDATA%\com.local.musiclibrary\music-library.sqlite3`, or the path in `AURORA_NG_CATALOG`).
- Library browser with Artists → Albums → Tracks columns, using virtualized lists for large libraries.
- Type an artist's name anywhere in the window to jump to it.
- Playback with rodio: double-click an album or track to play the album from there, with automatic advance to the next track.
- Now playing bar with previous, play/pause and next, elapsed and total time, and a clickable progress bar for seeking. Space plays or pauses.
