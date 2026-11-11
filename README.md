# Aurora NG

<img src="assets/app-icon/aurora_ng.png" alt="Aurora NG icon" width="128">

Aurora NG is [Aurora](https://github.com/soundtrackgeek/aurora) rebuilt as a native Rust app with [GPUI](https://gpui.rs), the GPU-accelerated UI framework behind the Zed editor.

Version **0.9.0** browses your library, shows a wall of album covers, plays music, and keeps itself up to date.

## What it does

- **Browse**: three columns, **Artists → Albums → Tracks**. Click an artist to see their albums (oldest first), and click an album to see its tracks. Long lists scroll smoothly, even with tens of thousands of artists.
- **Cover wall**: click **Cover wall** in the header to swap the columns for a grid of every album's cover, in artist order. From 300 pixels up, every cover also shows two more lines under its title: genre · label · track count · length, then the average rating in stars and the Album Score. Click a cover to open the album's details right below its row, pushing the covers underneath down: a header with the same info (only on smaller covers, where it isn't already under the cover), then every track with its number, title, artist (when it differs from the album artist), time, star rating and a heart for loved tracks. Click the cover again or press Esc to close the details. Double-click a cover to play the album, or double-click a track in the details to play from there. Typing an artist's name jumps to their albums. Drag the **Size** slider in the header to make the covers anywhere from 50 to 500 pixels wide; the wall updates as you drag. Below 100 pixels, the titles are hidden so more covers fit. Click **Cover wall** again to go back to the columns, with the same album selected.
- **Picks up where you left off**: closing the app remembers the view (cover wall or columns), the cover size, the selected album and track, whether its details were open, and the window's position and size. The next start opens the same way, with that album in view. The first start opens on the cover wall with 500-pixel covers. The settings are kept in `%LOCALAPPDATA%\aurora_ng\settings.json`; delete it to start fresh.
- **Find an artist**: start typing a name anywhere in the window ("pink f") to jump to the first matching artist.
- **Play**: double-click an album to play it from the start, or double-click a track to play the album from that track. The album plays through to the end.
- **Now playing bar**: previous, play/pause and next buttons, plus elapsed and total time. Click the progress bar to jump to that point in the track. **Previous** restarts the track, or goes back one track during its first 3 seconds.
- **Space** plays or pauses.
- **Updates**: the app checks for a new release when it starts and every 30 minutes after that. Click **Check for updates** in the header to check right away. When a new version is out, click **Update to vX.Y.Z** to download it, replace the app and restart.

## Download

Get `aurora_ng-windows-x64.exe` from the [latest release](https://github.com/soundtrackgeek/aurora_ng/releases/latest) and run it. The download is its own installer: the first time it runs, it

- copies itself to `%LOCALAPPDATA%\Programs\Aurora NG\Aurora NG.exe` (no admin rights needed),
- adds **Aurora NG** to the Start Menu,
- starts the installed copy and closes.

Open it from the Start Menu from then on; right-click it there to **Pin to Start** or **Pin to taskbar**. The downloaded file can be deleted. Running the download again installs it over the old copy (close the app first). The app updates itself in place, so the Start Menu entry and pins keep working after updates.

To uninstall, delete the `%LOCALAPPDATA%\Programs\Aurora NG` folder and the Start Menu shortcut (`%APPDATA%\Microsoft\Windows\Start Menu\Programs\Aurora NG.lnk`).

## Your library

Aurora NG reads the same catalog as Aurora: Music Library's database at

```
%APPDATA%\com.local.musiclibrary\music-library.sqlite3
```

It opens the catalog read-only and never changes it. Music Library must have imported your music first. To use a catalog somewhere else, set the `AURORA_NG_CATALOG` environment variable to its full path before starting the app.

## Album covers

The cover wall uses one image per album, named after the album's folder: the album in `D:\MUSIC\Kittie - Oracle (2001)` uses `Kittie - Oracle (2001).jpg` (JPG, PNG, GIF or BMP; case does not matter). Covers are read from

```
C:\_code\music_backup_v5\AlbumCovers
```

To use another folder, set the `AURORA_NG_COVERS` environment variable to it. Albums without a cover show their title instead. The header shows how many albums have a cover.

The original images are never changed. Square copies are made as you scroll, in four sizes (128, 256, 512 and 1024 pixels) so covers stay sharp at every size on the slider. They are saved in `%LOCALAPPDATA%\aurora_ng\thumbnails`, so covers load instantly the next time. That folder can be deleted at any time; the copies are made again when needed.

Audio files are played from the folders the catalog records (for example `D:\MUSIC\...`). MP3, FLAC, M4A/AAC, Ogg Vorbis and WAV are supported.

## Building and running

You need Rust **1.95.0** (pinned in `rust-toolchain.toml`; rustup installs it automatically) and, on Windows, the MSVC build tools.

```powershell
cargo run --release
```

The first build compiles GPUI and takes several minutes. Run the tests with `cargo test`.

## App icon

The icon (aurora curtains over a night sky, with a pair of music notes) is built into the exe, so Explorer, the Start Menu, the taskbar and the window all show it, as does the Start Menu shortcut the app makes when it installs itself. `build.rs` embeds `assets/app-icon/aurora_ng.ico` together with the app's name and version, which Windows shows in the file's Properties and in Task Manager.

The icon is drawn by a script. To change it, edit `assets/app-icon/generate.py` and run it from the repository root (needs Python with Pillow and numpy):

```powershell
python assets/app-icon/generate.py
```

It writes the `.ico` (10 sizes, 16 to 256 pixels; the smallest ones are simplified so they stay sharp), a 1024-pixel `aurora_ng.png`, and `preview.png` with every size side by side for checking by eye.

## Releasing

Every push to `master` is built and tested by GitHub Actions (`.github/workflows/ci.yml`), which also keeps the build cache warm. Releases are built by `.github/workflows/release.yml`:

1. Bump `version` in `Cargo.toml` and add the matching section to `CHANGELOG.md`.
2. Commit, then tag and push: `git tag v0.4.0 && git push origin v0.4.0`. Wait for the CI run on `master` to finish first, so the release can reuse its cache.

The workflow checks that the tag matches `Cargo.toml`, runs the tests, builds the release exe and publishes it with that version's changelog notes. Running apps pick it up within 30 minutes.
