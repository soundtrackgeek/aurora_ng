# Changelog

All notable changes to Aurora NG are documented here. The project follows [Semantic Versioning](https://semver.org).

## [0.14.0] - 2026-11-11

### Added

- **Your registered plays** and **Your listening time** for the playing track in the now-playing bar, as in Aurora's inspector. They count every play: Aurora NG's own, Aurora's on this computer, Music Library's player (Tonehavn), and other computers' histories in `OneDrive\_musicbackup`, and they update live while the track plays.
- Aurora NG keeps a listening history the way Aurora does: a play registers after 30 seconds of listening (or the whole track, when it is shorter), and only forward playback counts, so seeking ahead adds nothing. It uses Aurora's history format in `%LOCALAPPDATA%\aurora_ng\aurora-history.sqlite3`, under its own device (`aurora-ng-<computer>`).
- The history is published as `aurora-history-aurora-ng-<computer>.sqlite3` in `OneDrive\_musicbackup` (at most once a minute while playing, and on close), so Aurora shows Aurora NG's plays too. A new install on the same computer starts from that copy.
- Registered plays are sent to Music Library's listening history through its `recordPlays` bridge operation, as Aurora sends them; plays Music Library already has from Aurora are ignored, so nothing is counted twice.

## [0.13.0] - 2026-11-11

### Added

- Windows-wide shortcuts with the same keys as Aurora, working while another program has focus: Ctrl+Alt+P plays or pauses, Ctrl+Alt+N skips to the next track, Ctrl+Alt+Num 1 to Num 5 rate the playing track, Ctrl+Alt+Num 0 clears its rating, and Ctrl+Alt+L toggles its Love. If another program (such as Aurora) already has some of the keys, the now-playing bar says which.
- Toggle Love works as in Aurora: it writes MusicBee's `LOVE RATING` tag (`L`) into the MP3 with the same verified, atomic save as ratings, or removes it; a banned track becomes loved. Music Library then re-reads the file. The now-playing bar shows a heart for the playing track.

### Changed

- Rating and Love from the keyboard always go to the track that is playing, never to the one selected in the window. Ctrl+0 to Ctrl+5 no longer rate the selected track; clicking stars still rates any track.

## [0.12.0] - 2026-11-11

### Added

- Rate tracks with one to five whole stars by clicking the stars in the album details, the Tracks column or the now-playing bar; hovering previews the rating, and clicking a track's current rating clears it. Ctrl+1 to Ctrl+5 rate the selected (or playing) track and Ctrl+0 clears it. Half stars cannot be set.
- As in Aurora, the rating is written into the MP3 as MusicBee's `POPM` frame, through a verified working copy and an atomic replace that keep every other tag and the audio unchanged. Music Library is then asked through its bridge to re-read the file, which updates the track's rating and the album's rating and Album Score in its catalog.
- Ratings Music Library has not picked up yet keep showing, are saved in `%LOCALAPPDATA%\aurora_ng\pending-ratings.json`, and are handed to it again every 30 seconds and after a restart. The now-playing bar shows what ratings are doing.

## [0.11.0] - 2026-11-11

### Added

- `sort:` in the search box orders the cover wall: `sort:random`, `sort:year..` (oldest first), `sort:..year` (newest first), `sort:album az` / `za` (album title), `sort:aartist az` / `za` (album artist), and `sort:az` / `sort:za` as short forms for album title. It can go anywhere in a query, alone or with a search, like `genre:scores sort:random`. Aurora does not have it.
- With `sort:random`, Enter in the search box or the new **Shuffle** button gives a new draw. The draw is remembered between runs.

## [0.10.0] - 2026-11-11

### Added

- A search box in the header that filters the albums on the cover wall and in the columns, using Aurora's search language: fields such as `aartist:`, `genre:`, `year:`, `cr:` and `love:`, exact values in quotes, ranges with `..`, and `AND`, `OR`, `NOT`. `genre:scores` covers every film, TV and game score genre. For example, `year:2000..2009 AND genre:scores AND cr:..99` finds score albums from the 2000s that are not fully rated, and `aartist:"bon jovi"` finds Bon Jovi but not Jon Bon Jovi.
- The search starts 2 seconds after you stop typing, so it does not run for every letter; Enter searches right away. Esc or **Clear** shows everything again. Ctrl+K or Ctrl+F jumps to the box.
- The header shows how many albums and artists matched, and explains a query it cannot read. The selected album stays selected when it is among the results.
- The search is remembered and runs again at the next start.

## [0.9.0] - 2026-11-11

### Added

- The app remembers how it looked when it was closed and opens the same way: cover wall or columns, the cover size, the selected album and track, whether the album's details were open, and the window's position, size and maximized state. The album is scrolled into view, and if it is no longer in the library the app starts at the first artist. Settings are saved in `%LOCALAPPDATA%\aurora_ng\settings.json`, also just before an update restarts the app.

### Changed

- The first time it starts, the app opens on the cover wall with 500-pixel covers, instead of the columns with 180-pixel covers.

## [0.8.0] - 2026-11-11

### Added

- The app installs itself: the first time the downloaded `aurora_ng-windows-x64.exe` runs, it copies itself to `%LOCALAPPDATA%\Programs\Aurora NG\Aurora NG.exe`, adds an **Aurora NG** shortcut to the Start Menu, and starts from there. It now shows as "Aurora NG" in Start Menu search and can be pinned to Start and the taskbar. If the shortcut is removed, the installed app puts it back the next time it starts.

## [0.7.0] - 2026-11-11

### Added

- An app icon: aurora curtains in the app's teal, violet and pink over a night sky, with a pair of music notes. It is built into the exe, so the Start Menu, taskbar, Explorer and the window's title bar show it. Small sizes use a simpler drawing so they stay sharp.
- The exe now carries its name and version ("Aurora NG", 0.7.0), shown in its Properties and in Task Manager instead of "aurora_ng.exe".

## [0.6.0] - 2026-11-11

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
