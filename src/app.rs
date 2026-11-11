//! The main window: Artists → Albums → Tracks, or a wall of album covers,
//! with a now-playing bar.

use crate::{
    audio::Audio,
    covers::{self, CoverIndex},
    library::{Artist, Catalog, Track, catalog_path},
    update::{self, Release},
};
use gpui::{
    Bounds, ClickEvent, Context, Div, FocusHandle, ImageSource, KeyDownEvent, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, ObjectFit, Pixels, RenderImage, ScrollStrategy,
    SharedString, Stateful, Task, UniformListScrollHandle, Window, canvas, div, img, prelude::*,
    px, relative, rgb, svg, uniform_list,
};
use std::{
    cell::Cell,
    collections::HashMap,
    ops::Range,
    path::PathBuf,
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};

const ROW_HEIGHT: Pixels = px(30.);
const TICK: Duration = Duration::from_millis(250);
/// Keystrokes this close together build one search, like "pink f".
const TYPE_AHEAD_WINDOW: Duration = Duration::from_millis(1000);

/// Cover wall tiles: the square cover, then two lines of text when there is room.
const TILE_MIN: f32 = 50.;
const TILE_MAX: f32 = 500.;
const TILE_DEFAULT: f32 = 180.;
/// Tiles narrower than this show only the cover.
const TILE_MIN_FOR_TEXT: f32 = 100.;
/// Room for the title and artist lines under a cover.
const TILE_TEXT_HEIGHT: Pixels = px(52.);
/// Memory for thumbnails; the ones off screen longest are dropped past this.
const COVER_MEMORY_BYTES: usize = 192 * 1024 * 1024;

mod color {
    pub const BACKGROUND: u32 = 0x0e1016;
    pub const PANEL: u32 = 0x141720;
    pub const BORDER: u32 = 0x222634;
    pub const TEXT: u32 = 0xe6e8ef;
    pub const MUTED: u32 = 0x868ca0;
    pub const HOVER: u32 = 0x1b1f2b;
    pub const SELECTED: u32 = 0x23304a;
    pub const ACCENT: u32 = 0x5eead4;
    pub const ACCENT_HOVER: u32 = 0x99f6e4;
    pub const BAR: u32 = 0x2a2f3f;
    pub const ERROR: u32 = 0xf87171;
}

enum UpdateState {
    Idle,
    Checking,
    UpToDate,
    Available(Release),
    Installing(Release),
    Failed(String),
}

enum LoadState {
    Loading,
    Ready,
    Failed(String),
}

struct Loaded {
    catalog: Catalog,
    artists: Vec<Artist>,
    covers: Result<usize, String>,
}

/// A cover on the wall. The numbers are the wall frame it was last on screen,
/// so jobs and thumbnails that scrolled away can be cancelled or dropped.
enum Cover {
    /// Dropping the task cancels the job if it has not started.
    Loading {
        _task: Task<()>,
        wanted: u64,
        /// A smaller thumbnail to show until the new one is ready.
        previous: Option<Arc<RenderImage>>,
    },
    Ready {
        image: Arc<RenderImage>,
        size: u32,
        used: u64,
    },
    Missing,
}

pub struct AuroraApp {
    focus_handle: FocusHandle,
    load_state: LoadState,
    catalog: Option<Catalog>,
    artists: Vec<Artist>,
    album_count: usize,

    /// Where each artist's albums start in the cover wall, which lists every
    /// album in artist order.
    album_offsets: Vec<usize>,
    /// How many albums have a cover, or why none do.
    covers_found: Result<usize, String>,

    cover_wall: bool,
    wall_scroll: UniformListScrollHandle,
    wall_columns: usize,
    wall_row_height: Pixels,
    /// Cover width in pixels, set with the slider.
    tile_size: f32,
    /// The thumbnail size the wall is drawn with, from the tile size.
    thumbnail_size: u32,
    dragging_size: bool,
    size_slider_bounds: Rc<Cell<Bounds<Pixels>>>,
    /// Counts renders, to tell which covers are still on screen.
    wall_frame: u64,
    covers: HashMap<usize, Cover>,
    thumbnail_dir: Option<PathBuf>,
    /// Thumbnails dropped from memory, to be removed from the GPU on the next render.
    stale_images: Vec<Arc<RenderImage>>,

    selected_artist: Option<usize>,
    selected_album: Option<usize>,
    selected_track: Option<usize>,
    tracks: Vec<Track>,
    tracks_error: Option<String>,
    artist_scroll: UniformListScrollHandle,
    album_scroll: UniformListScrollHandle,
    track_scroll: UniformListScrollHandle,

    audio: Result<Audio, String>,
    /// The album that is playing, which stays put while you browse elsewhere.
    queue: Vec<Track>,
    queue_album: SharedString,
    current: Option<usize>,
    playback_error: Option<String>,
    seek_bar_bounds: Rc<Cell<Bounds<Pixels>>>,

    type_ahead: String,
    type_ahead_at: Instant,
    _ticker: Task<()>,

    update: UpdateState,
    /// Checks for updates at startup and then every 30 minutes.
    _update_checker: Task<()>,
}

impl AuroraApp {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle);

        // Reading 74k albums takes a moment, so it happens off the UI thread.
        let loading = cx.background_executor().spawn(async move {
            let catalog = Catalog::open(&catalog_path()?)?;
            let mut artists = catalog.artists()?;
            let covers = CoverIndex::scan(&covers::covers_dir()).map(|index| {
                let mut found = 0;
                for album in artists.iter_mut().flat_map(|artist| &mut artist.albums) {
                    album.cover = album
                        .folder
                        .as_deref()
                        .and_then(|folder| index.find(folder));
                    found += usize::from(album.cover.is_some());
                }
                found
            });
            Ok::<_, String>(Loaded {
                catalog,
                artists,
                covers,
            })
        });
        cx.spawn(async move |this, cx| {
            let result = loading.await;
            this.update(cx, |this, cx| this.finish_loading(result, cx))
                .ok();
        })
        .detach();

        let ticker = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(TICK).await;
                if this.update(cx, |this, cx| this.tick(cx)).is_err() {
                    break;
                }
            }
        });

        let update_checker = cx.spawn(async move |this, cx| {
            loop {
                if this
                    .update(cx, |this, cx| this.check_for_updates(cx))
                    .is_err()
                {
                    break;
                }
                cx.background_executor().timer(update::CHECK_INTERVAL).await;
            }
        });

        Self {
            focus_handle,
            load_state: LoadState::Loading,
            catalog: None,
            artists: Vec::new(),
            album_count: 0,
            album_offsets: Vec::new(),
            covers_found: Ok(0),
            cover_wall: false,
            wall_scroll: UniformListScrollHandle::new(),
            wall_columns: 1,
            wall_row_height: px(0.),
            tile_size: TILE_DEFAULT,
            thumbnail_size: covers::THUMBNAIL_SIZES[0],
            dragging_size: false,
            size_slider_bounds: Rc::new(Cell::new(Bounds::default())),
            wall_frame: 0,
            covers: HashMap::new(),
            thumbnail_dir: covers::thumbnail_dir(),
            stale_images: Vec::new(),
            selected_artist: None,
            selected_album: None,
            selected_track: None,
            tracks: Vec::new(),
            tracks_error: None,
            artist_scroll: UniformListScrollHandle::new(),
            album_scroll: UniformListScrollHandle::new(),
            track_scroll: UniformListScrollHandle::new(),
            audio: Audio::new(),
            queue: Vec::new(),
            queue_album: SharedString::default(),
            current: None,
            playback_error: None,
            seek_bar_bounds: Rc::new(Cell::new(Bounds::default())),
            type_ahead: String::new(),
            type_ahead_at: Instant::now(),
            _ticker: ticker,
            update: UpdateState::Idle,
            _update_checker: update_checker,
        }
    }

    fn finish_loading(&mut self, result: Result<Loaded, String>, cx: &mut Context<Self>) {
        match result {
            Ok(Loaded {
                catalog,
                artists,
                covers,
            }) => {
                self.album_offsets = artists
                    .iter()
                    .scan(0, |offset, artist| {
                        let start = *offset;
                        *offset += artist.albums.len();
                        Some(start)
                    })
                    .collect();
                self.album_count = artists.iter().map(|artist| artist.albums.len()).sum();
                self.covers_found = covers;
                self.catalog = Some(catalog);
                self.artists = artists;
                self.load_state = LoadState::Ready;
                if !self.artists.is_empty() {
                    self.select_artist(0, cx);
                }
            }
            Err(error) => self.load_state = LoadState::Failed(error),
        }
        cx.notify();
    }

    // ── Browsing ────────────────────────────────────────────────────────

    fn select_artist(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.selected_artist = Some(ix);
        self.album_scroll.scroll_to_item(0, ScrollStrategy::Top);
        self.select_album(0, cx);
    }

    fn select_album(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.selected_album = None;
        self.selected_track = None;
        self.tracks.clear();
        self.tracks_error = None;
        self.track_scroll.scroll_to_item(0, ScrollStrategy::Top);

        let (Some(artist), Some(catalog)) = (self.selected_artist, &self.catalog) else {
            return;
        };
        let Some(album) = self.artists[artist].albums.get(ix) else {
            return;
        };
        self.selected_album = Some(ix);
        match catalog.tracks(&album.id) {
            Ok(tracks) => self.tracks = tracks,
            Err(error) => self.tracks_error = Some(error),
        }
        cx.notify();
    }

    fn selected_album_title(&self) -> SharedString {
        match (self.selected_artist, self.selected_album) {
            (Some(artist), Some(album)) => self.artists[artist].albums[album].title.clone().into(),
            _ => SharedString::default(),
        }
    }

    /// Jumps to the first artist whose name starts with what was typed.
    fn jump_to_artist(&mut self, cx: &mut Context<Self>) {
        let prefix = self.type_ahead.as_str();
        let ix = self
            .artists
            .partition_point(|artist| artist.sort_key.as_str() < prefix);
        if self
            .artists
            .get(ix)
            .is_some_and(|artist| artist.sort_key.starts_with(prefix))
        {
            self.select_artist(ix, cx);
            self.artist_scroll.scroll_to_item(ix, ScrollStrategy::Top);
            if self.cover_wall {
                let row = self.album_offsets[ix] / self.wall_columns;
                self.wall_scroll.scroll_to_item(row, ScrollStrategy::Top);
            }
        }
    }

    // ── Cover wall ──────────────────────────────────────────────────────

    fn toggle_cover_wall(&mut self, cx: &mut Context<Self>) {
        self.cover_wall = !self.cover_wall;
        // Keep the selected album in view when switching.
        if let (Some(artist), Some(album)) = (self.selected_artist, self.selected_album) {
            if self.cover_wall {
                let row = (self.album_offsets[artist] + album) / self.wall_columns;
                self.wall_scroll.scroll_to_item(row, ScrollStrategy::Center);
            } else {
                self.artist_scroll
                    .scroll_to_item(artist, ScrollStrategy::Center);
                self.album_scroll
                    .scroll_to_item(album, ScrollStrategy::Center);
            }
        }
        cx.notify();
    }

    /// The artist and album at position `ix` on the wall.
    fn wall_album(&self, ix: usize) -> (usize, usize) {
        let artist = self.album_offsets.partition_point(|&start| start <= ix) - 1;
        (artist, ix - self.album_offsets[artist])
    }

    fn select_wall_album(&mut self, ix: usize, cx: &mut Context<Self>) {
        let (artist, album) = self.wall_album(ix);
        if self.selected_artist != Some(artist) || self.selected_album != Some(album) {
            self.selected_artist = Some(artist);
            self.select_album(album, cx);
        }
    }

    /// The thumbnail for wall position `ix`: `None` while it loads,
    /// `Some(None)` when the album has no cover. Starts a job if needed.
    fn cover(&mut self, ix: usize, cx: &mut Context<Self>) -> Option<Option<Arc<RenderImage>>> {
        let frame = self.wall_frame;
        let size = self.thumbnail_size;
        let previous = match self.covers.get_mut(&ix) {
            Some(Cover::Ready {
                image,
                size: ready_size,
                used,
            }) => {
                *used = frame;
                if *ready_size >= size {
                    return Some(Some(image.clone()));
                }
                // Too small for the tile now: load a bigger one, showing this meanwhile.
                Some(image.clone())
            }
            Some(Cover::Loading {
                wanted, previous, ..
            }) => {
                *wanted = frame;
                return previous.clone().map(Some);
            }
            Some(Cover::Missing) => return Some(None),
            None => None,
        };
        let (artist, album) = self.wall_album(ix);
        let Some(path) = self.artists[artist].albums[album].cover.clone() else {
            self.covers.insert(ix, Cover::Missing);
            return Some(None);
        };
        let cache = self.thumbnail_dir.clone();
        let job = cx
            .background_executor()
            .spawn(async move { covers::thumbnail(&path, size, cache.as_deref()) });
        let task = cx.spawn(async move |this, cx| {
            let result = job.await;
            this.update(cx, |this, cx| this.finish_cover(ix, size, result, cx))
                .ok();
        });
        let shown = previous.clone().map(Some);
        self.covers.insert(
            ix,
            Cover::Loading {
                _task: task,
                wanted: frame,
                previous,
            },
        );
        shown
    }

    fn finish_cover(
        &mut self,
        ix: usize,
        size: u32,
        result: Result<Arc<RenderImage>, String>,
        cx: &mut Context<Self>,
    ) {
        let cover = match result {
            Ok(image) => Cover::Ready {
                image,
                size,
                used: self.wall_frame,
            },
            Err(_) => Cover::Missing,
        };
        if let Some(Cover::Loading {
            previous: Some(image),
            ..
        }) = self.covers.insert(ix, cover)
        {
            self.stale_images.push(image);
        }

        // Over budget: drop the covers that have been off screen longest, down
        // to three quarters of the budget so this does not run for every cover.
        // Covers on screen are always kept.
        let bytes = |size: u32| size as usize * size as usize * 4;
        let mut ready: Vec<(u64, usize, usize)> = self
            .covers
            .iter()
            .filter_map(|(&ix, cover)| match cover {
                Cover::Ready { used, size, .. } => Some((*used, ix, bytes(*size))),
                _ => None,
            })
            .collect();
        let mut total: usize = ready.iter().map(|(_, _, bytes)| bytes).sum();
        if total > COVER_MEMORY_BYTES {
            ready.sort_unstable();
            for (used, ix, bytes) in ready {
                if total <= COVER_MEMORY_BYTES / 4 * 3 || used >= self.wall_frame {
                    break;
                }
                if let Some(Cover::Ready { image, .. }) = self.covers.remove(&ix) {
                    self.stale_images.push(image);
                    total -= bytes;
                }
            }
        }
        cx.notify();
    }

    /// Cancels jobs for covers that were not on screen in the last frame, and
    /// frees the GPU copies of dropped thumbnails.
    fn tidy_covers(&mut self, window: &mut Window) {
        let frame = self.wall_frame;
        let stale = &mut self.stale_images;
        self.covers.retain(|_, cover| match cover {
            Cover::Loading {
                wanted, previous, ..
            } if *wanted < frame => {
                stale.extend(previous.take());
                false
            }
            _ => true,
        });
        for image in self.stale_images.drain(..) {
            window.drop_image(image).ok();
        }
        self.wall_frame += 1;
    }

    /// Sets the cover size from a point on the size slider.
    fn drag_size_to(&mut self, x: Pixels, cx: &mut Context<Self>) {
        let bounds = self.size_slider_bounds.get();
        if bounds.size.width <= px(0.) {
            return;
        }
        let fraction = ((x - bounds.origin.x) / bounds.size.width).clamp(0., 1.);
        let size = (TILE_MIN + fraction * (TILE_MAX - TILE_MIN)).round();
        if size != self.tile_size {
            self.tile_size = size;
            cx.notify();
        }
    }

    // ── Updates ─────────────────────────────────────────────────────────

    fn check_for_updates(&mut self, cx: &mut Context<Self>) {
        if matches!(
            self.update,
            UpdateState::Checking | UpdateState::Installing(_)
        ) {
            return;
        }
        self.update = UpdateState::Checking;
        cx.notify();
        let checking = cx.background_executor().spawn(async { update::check() });
        cx.spawn(async move |this, cx| {
            let result = checking.await;
            this.update(cx, |this, cx| {
                // An install may have started while the check was running.
                if matches!(this.update, UpdateState::Checking) {
                    this.update = match result {
                        Ok(Some(release)) => UpdateState::Available(release),
                        Ok(None) => UpdateState::UpToDate,
                        Err(error) => UpdateState::Failed(error),
                    };
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn install_update(&mut self, cx: &mut Context<Self>) {
        let UpdateState::Available(release) = &self.update else {
            return;
        };
        let release = release.clone();
        self.update = UpdateState::Installing(release.clone());
        cx.notify();
        let installing = cx
            .background_executor()
            .spawn(async move { update::install(&release).and_then(|()| update::relaunch()) });
        cx.spawn(async move |this, cx| {
            let result = installing.await;
            this.update(cx, |this, cx| match result {
                Ok(()) => cx.quit(),
                Err(error) => {
                    this.update = UpdateState::Failed(error);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    // ── Playback ────────────────────────────────────────────────────────

    /// Plays the selected album, starting at track `ix`.
    fn play_from(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.queue = self.tracks.clone();
        self.queue_album = self.selected_album_title();
        self.start(ix, cx);
    }

    /// Starts queue entry `ix`, skipping forward past files that cannot be played.
    fn start(&mut self, ix: usize, cx: &mut Context<Self>) {
        cx.notify();
        let audio = match &self.audio {
            Ok(audio) => audio,
            Err(error) => {
                self.playback_error = Some(error.clone());
                return;
            }
        };
        for (offset, track) in self.queue.iter().enumerate().skip(ix) {
            match audio.play_file(&track.path) {
                Ok(()) => {
                    self.current = Some(offset);
                    self.playback_error = None;
                    return;
                }
                Err(error) => self.playback_error = Some(error),
            }
        }
        audio.stop();
        self.current = None;
    }

    fn toggle_play(&mut self, cx: &mut Context<Self>) {
        match (&self.audio, self.current) {
            (Ok(audio), Some(_)) if audio.is_paused() => audio.resume(),
            (Ok(audio), Some(_)) => audio.pause(),
            _ if !self.tracks.is_empty() => self.play_from(self.selected_track.unwrap_or(0), cx),
            _ => {}
        }
        cx.notify();
    }

    fn next(&mut self, cx: &mut Context<Self>) {
        if let Some(ix) = self.current
            && ix + 1 < self.queue.len()
        {
            self.start(ix + 1, cx);
        }
    }

    /// Restarts the track, or goes to the previous one in its first 3 seconds.
    fn previous(&mut self, cx: &mut Context<Self>) {
        let (Ok(audio), Some(ix)) = (&self.audio, self.current) else {
            return;
        };
        if ix == 0 || audio.position() > Duration::from_secs(3) {
            audio.seek(Duration::ZERO);
            cx.notify();
        } else {
            self.start(ix - 1, cx);
        }
    }

    fn seek_to(&mut self, x: Pixels, cx: &mut Context<Self>) {
        let bounds = self.seek_bar_bounds.get();
        let (Ok(audio), Some(track)) = (&self.audio, self.current_track()) else {
            return;
        };
        let Some(seconds) = track.duration_seconds else {
            return;
        };
        if bounds.size.width <= px(0.) {
            return;
        }
        let fraction = ((x - bounds.origin.x) / bounds.size.width).clamp(0., 1.);
        audio.seek(Duration::from_secs_f32(fraction * seconds as f32));
        cx.notify();
    }

    fn current_track(&self) -> Option<&Track> {
        self.current.and_then(|ix| self.queue.get(ix))
    }

    fn tick(&mut self, cx: &mut Context<Self>) {
        let (Ok(audio), Some(ix)) = (&self.audio, self.current) else {
            return;
        };
        if audio.is_paused() {
            return;
        }
        if audio.is_finished() {
            if ix + 1 < self.queue.len() {
                self.start(ix + 1, cx);
            } else {
                self.current = None;
            }
        }
        cx.notify();
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        let modifiers = &keystroke.modifiers;
        if modifiers.control || modifiers.alt || modifiers.platform || modifiers.function {
            return;
        }
        let typing =
            !self.type_ahead.is_empty() && self.type_ahead_at.elapsed() < TYPE_AHEAD_WINDOW;
        let text = match (keystroke.key.as_str(), keystroke.key_char.as_deref()) {
            // Space plays or pauses, unless it is part of a name like "pink floyd".
            ("space", _) if !typing => {
                self.toggle_play(cx);
                cx.stop_propagation();
                return;
            }
            // Windows reports Space without a typed character.
            ("space", _) => " ",
            (_, Some(text)) => text,
            (_, None) => return,
        };
        if text.is_empty() || text.chars().any(char::is_control) {
            return;
        }
        if !typing {
            self.type_ahead.clear();
        }
        self.type_ahead.push_str(&text.to_lowercase());
        self.type_ahead_at = Instant::now();
        self.jump_to_artist(cx);
        cx.stop_propagation();
    }

    // ── Rendering ───────────────────────────────────────────────────────

    fn render_update(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let check_button = || {
            text_button("check-updates", "Check for updates", false)
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.check_for_updates(cx)))
        };
        let row = div().flex().items_center().gap_2().text_xs();
        match &self.update {
            UpdateState::Idle => row.child(check_button()),
            UpdateState::Checking => row.child(muted("Checking for updates…")),
            UpdateState::UpToDate => row.child(muted("Up to date")).child(check_button()),
            UpdateState::Available(release) => row.child(
                text_button(
                    "install-update",
                    format!("Update to v{}", release.version),
                    true,
                )
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.install_update(cx))),
            ),
            UpdateState::Installing(release) => {
                row.child(muted(format!("Installing v{}…", release.version)))
            }
            UpdateState::Failed(error) => row
                .child(
                    div()
                        .max_w(px(360.))
                        .truncate()
                        .text_color(rgb(color::ERROR))
                        .child(error.clone()),
                )
                .child(check_button()),
        }
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let status: SharedString = match &self.load_state {
            LoadState::Loading => "Loading library…".into(),
            LoadState::Ready => match &self.covers_found {
                Ok(covers) => format!(
                    "{} artists · {} albums · {} covers",
                    thousands(self.artists.len()),
                    thousands(self.album_count),
                    thousands(*covers)
                ),
                Err(_) => format!(
                    "{} artists · {} albums",
                    thousands(self.artists.len()),
                    thousands(self.album_count)
                ),
            }
            .into(),
            LoadState::Failed(_) => "Library unavailable".into(),
        };
        div()
            .flex()
            .items_center()
            .justify_between()
            .h(px(44.))
            .px_4()
            .border_b_1()
            .border_color(rgb(color::BORDER))
            .child(
                div()
                    .flex()
                    .items_baseline()
                    .gap_2()
                    .child(
                        div()
                            .text_color(rgb(color::ACCENT))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child("Aurora NG"),
                    )
                    .child(muted(concat!("v", env!("CARGO_PKG_VERSION")))),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_4()
                    .child(div().text_sm().text_color(rgb(color::MUTED)).child(status))
                    .when(self.cover_wall, |row| {
                        row.child(self.render_size_slider(cx))
                    })
                    .when(matches!(self.load_state, LoadState::Ready), |row| {
                        row.child(
                            text_button("cover-wall", "Cover wall", self.cover_wall).on_click(
                                cx.listener(|this, _: &ClickEvent, _, cx| {
                                    this.toggle_cover_wall(cx)
                                }),
                            ),
                        )
                    })
                    .child(self.render_update(cx)),
            )
    }

    fn render_artists(&self, cx: &mut Context<Self>) -> impl IntoElement {
        panel("Artists", self.artists.len()).child(
            uniform_list(
                "artists",
                self.artists.len(),
                cx.processor(|this, range: Range<usize>, _, cx| {
                    range
                        .map(|ix| {
                            let artist = &this.artists[ix];
                            row(ix, this.selected_artist == Some(ix))
                                .child(truncated(artist.name.clone()))
                                .child(muted(artist.albums.len().to_string()))
                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    this.select_artist(ix, cx);
                                }))
                        })
                        .collect()
                }),
            )
            .track_scroll(self.artist_scroll.clone())
            .flex_1(),
        )
    }

    fn render_albums(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let count = self
            .selected_artist
            .map_or(0, |ix| self.artists[ix].albums.len());
        panel("Albums", count).child(
            uniform_list(
                "albums",
                count,
                cx.processor(|this, range: Range<usize>, _, cx| {
                    let Some(artist) = this.selected_artist else {
                        return Vec::new();
                    };
                    range
                        .map(|ix| {
                            let album = &this.artists[artist].albums[ix];
                            row(ix, this.selected_album == Some(ix))
                                .child(truncated(album.title.clone()))
                                .child(muted(
                                    album.year.map(|year| year.to_string()).unwrap_or_default(),
                                ))
                                .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                                    if this.selected_album != Some(ix) {
                                        this.select_album(ix, cx);
                                    }
                                    if event.click_count() >= 2 {
                                        this.play_from(0, cx);
                                    }
                                }))
                        })
                        .collect()
                }),
            )
            .track_scroll(self.album_scroll.clone())
            .flex_1(),
        )
    }

    fn render_tracks(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let list = uniform_list(
            "tracks",
            self.tracks.len(),
            cx.processor(|this, range: Range<usize>, _, cx| {
                let multi_disc = this.tracks.iter().any(|track| track.disc.unwrap_or(1) > 1);
                let playing_id = this.current_track().map(|track| track.id);
                let album_artist = this
                    .selected_artist
                    .map(|ix| this.artists[ix].name.as_str());
                range
                    .map(|ix| {
                        let track = &this.tracks[ix];
                        let playing = playing_id == Some(track.id);
                        let number = div().w(px(36.)).flex().justify_end().flex_none();
                        let number = if playing {
                            number.child(
                                svg()
                                    .path("icons/play.svg")
                                    .size_3()
                                    .text_color(rgb(color::ACCENT)),
                            )
                        } else {
                            number.child(muted(track.position_label(multi_disc)))
                        };
                        row(ix, this.selected_track == Some(ix))
                            .when(playing, |row| row.text_color(rgb(color::ACCENT)))
                            .child(number)
                            .child(truncated(track.title.clone()))
                            .when(album_artist != Some(track.artist.as_str()), |row| {
                                row.child(
                                    div()
                                        .max_w(px(220.))
                                        .truncate()
                                        .text_xs()
                                        .text_color(rgb(color::MUTED))
                                        .child(track.artist.clone()),
                                )
                            })
                            .child(muted(track.duration_seconds.map(clock).unwrap_or_default()))
                            .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                                this.selected_track = Some(ix);
                                if event.click_count() >= 2 {
                                    this.play_from(ix, cx);
                                }
                                cx.notify();
                            }))
                    })
                    .collect()
            }),
        )
        .track_scroll(self.track_scroll.clone())
        .flex_1();

        let panel = panel("Tracks", self.tracks.len());
        match &self.tracks_error {
            Some(error) => panel.child(message(error.clone(), color::ERROR)),
            None => panel.child(list),
        }
    }

    fn render_wall(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tile = px(self.tile_size);
        let gap = px((self.tile_size * 0.1).clamp(6., 18.));
        let show_text = self.tile_size >= TILE_MIN_FOR_TEXT;
        let row_height = gap + tile + if show_text { TILE_TEXT_HEIGHT } else { px(0.) };
        self.thumbnail_size = covers::thumbnail_size(self.tile_size * window.scale_factor());

        let width = window.viewport_size().width;
        let columns = (((width - gap) / (tile + gap)).floor() as usize).max(1);
        if columns != self.wall_columns || row_height != self.wall_row_height {
            // Keep the album at the top left in view when the window or the
            // cover size changes.
            if self.wall_row_height > px(0.) {
                let scrolled = -self.wall_scroll.0.borrow().base_handle.offset().y;
                let top_row = (scrolled / self.wall_row_height).max(0.) as usize;
                let first = top_row * self.wall_columns;
                self.wall_scroll
                    .scroll_to_item(first / columns, ScrollStrategy::Top);
            }
            self.wall_columns = columns;
            self.wall_row_height = row_height;
        }
        // Centre the grid; the last row stays left-aligned.
        let used = tile * columns as f32 + gap * (columns as f32 - 1.);
        let inset = ((width - used) / 2.).max(gap / 2.);
        let rows = self.album_count.div_ceil(columns);

        let wall = uniform_list(
            "cover-wall",
            rows,
            cx.processor(move |this, range: Range<usize>, _, cx| {
                let columns = this.wall_columns;
                range
                    .map(|row| {
                        let start = row * columns;
                        let end = (start + columns).min(this.album_count);
                        let tiles: Vec<_> = (start..end)
                            .map(|ix| this.render_tile(ix, tile, show_text, cx))
                            .collect();
                        div()
                            .h(row_height)
                            .pt(gap)
                            .pl(inset)
                            .flex()
                            .gap(gap)
                            .children(tiles)
                    })
                    .collect()
            }),
        )
        .track_scroll(self.wall_scroll.clone())
        .size_full();

        div()
            .size_full()
            .flex()
            .flex_col()
            .when_some(self.covers_found.as_ref().err(), |wall, error| {
                wall.child(
                    div()
                        .px_4()
                        .py_2()
                        .text_xs()
                        .text_color(rgb(color::ERROR))
                        .border_b_1()
                        .border_color(rgb(color::BORDER))
                        .child(format!(
                            "{error} Set AURORA_NG_COVERS to the folder with your album covers."
                        )),
                )
            })
            .child(div().flex_1().min_h_0().child(wall))
    }

    fn render_tile(
        &mut self,
        ix: usize,
        tile: Pixels,
        show_text: bool,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let cover = self.cover(ix, cx);
        let (artist_ix, album_ix) = self.wall_album(ix);
        let selected =
            self.selected_artist == Some(artist_ix) && self.selected_album == Some(album_ix);
        let artist = &self.artists[artist_ix];
        let album = &artist.albums[album_ix];
        let subtitle = match album.year {
            Some(year) => format!("{} · {year}", artist.name),
            None => artist.name.clone(),
        };

        let artwork = div()
            .size(tile)
            .flex_none()
            .rounded_md()
            .overflow_hidden()
            .bg(rgb(color::PANEL))
            .border_2()
            .border_color(rgb(if selected {
                color::ACCENT
            } else {
                color::BACKGROUND
            }))
            .when(!selected, |artwork| {
                artwork.group_hover("tile", |style| style.border_color(rgb(color::MUTED)))
            });
        let artwork = match cover {
            Some(Some(image)) => artwork.child(
                img(ImageSource::Render(image))
                    .size_full()
                    .object_fit(ObjectFit::Cover),
            ),
            // No cover: show the title instead, when it fits.
            Some(None) if show_text => artwork.child(
                div()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .p_4()
                    .text_center()
                    .text_color(rgb(color::MUTED))
                    .child(album.title.clone()),
            ),
            Some(None) | None => artwork,
        };

        div()
            .id(("cover", ix))
            .group("tile")
            .w(tile)
            .flex_none()
            .flex()
            .flex_col()
            .gap_1()
            .cursor_pointer()
            .child(artwork)
            .when(show_text, |tile| {
                tile.child(
                    div()
                        .flex_none()
                        .mt_1()
                        .truncate()
                        .when(selected, |title| title.text_color(rgb(color::ACCENT)))
                        .child(album.title.clone()),
                )
                .child(
                    div()
                        .flex_none()
                        .truncate()
                        .text_xs()
                        .text_color(rgb(color::MUTED))
                        .child(subtitle),
                )
            })
            .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                this.select_wall_album(ix, cx);
                if event.click_count() >= 2 {
                    this.play_from(0, cx);
                }
            }))
    }

    fn render_size_slider(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let fraction = (self.tile_size - TILE_MIN) / (TILE_MAX - TILE_MIN);
        let bounds_cell = self.size_slider_bounds.clone();
        let knob = px(12.);
        let slider = div()
            .id("cover-size")
            .w(px(140.))
            .h(px(20.))
            .flex()
            .items_center()
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                    this.dragging_size = true;
                    this.drag_size_to(event.position.x, cx);
                }),
            )
            .child(
                div()
                    .relative()
                    .w_full()
                    .h(px(4.))
                    .rounded_full()
                    .bg(rgb(color::BAR))
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .left_0()
                            .h_full()
                            .w(relative(fraction))
                            .rounded_full()
                            .bg(rgb(color::ACCENT)),
                    )
                    .child(
                        div()
                            .absolute()
                            .top(px(-4.))
                            .left(relative(fraction))
                            .ml(-knob / 2.)
                            .size(knob)
                            .rounded_full()
                            .bg(rgb(if self.dragging_size {
                                color::ACCENT_HOVER
                            } else {
                                color::ACCENT
                            })),
                    )
                    // Remembers where the slider is on screen, to turn a mouse
                    // position into a size.
                    .child(
                        canvas(move |bounds, _, _| bounds_cell.set(bounds), |_, _, _, _| {})
                            .absolute()
                            .size_full(),
                    ),
            );
        div()
            .flex()
            .items_center()
            .gap_3()
            .text_xs()
            .text_color(rgb(color::MUTED))
            .child("Size")
            .child(slider)
            .child(div().w(px(32.)).child(format!("{}", self.tile_size as u32)))
    }

    fn render_now_playing(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let track = self.current_track();
        let paused = self.audio.as_ref().map_or(true, Audio::is_paused);
        let playing = track.is_some() && !paused;
        let elapsed = match (&self.audio, track) {
            (Ok(audio), Some(_)) => audio.position().as_secs() as i64,
            _ => 0,
        };
        let total = track.and_then(|track| track.duration_seconds).unwrap_or(0);
        let fraction = if total > 0 {
            (elapsed as f32 / total as f32).clamp(0., 1.)
        } else {
            0.
        };

        let title = match (track, &self.playback_error, &self.audio) {
            (_, _, Err(error)) | (None, Some(error), _) => div()
                .text_color(rgb(color::ERROR))
                .truncate()
                .child(error.clone()),
            (Some(track), _, _) => div()
                .flex()
                .gap_2()
                .min_w_0()
                .child(
                    div()
                        .flex_none()
                        .max_w(relative(0.6))
                        .truncate()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(track.title.clone()),
                )
                .child(
                    truncated(format!("{} · {}", track.artist, self.queue_album))
                        .text_color(rgb(color::MUTED)),
                ),
            (None, None, _) => div()
                .text_color(rgb(color::MUTED))
                .child("Nothing playing. Double-click an album or track."),
        };

        let bounds_cell = self.seek_bar_bounds.clone();
        let seek_bar = div()
            .id("seek-bar")
            .flex_1()
            .h(px(16.))
            .flex()
            .items_center()
            .when(track.is_some(), |bar| bar.cursor_pointer())
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                    this.seek_to(event.position.x, cx);
                }),
            )
            .child(
                div()
                    .relative()
                    .w_full()
                    .h(px(4.))
                    .rounded_full()
                    .bg(rgb(color::BAR))
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .left_0()
                            .h_full()
                            .w(relative(fraction))
                            .rounded_full()
                            .bg(rgb(color::ACCENT)),
                    )
                    // Remembers where the bar is on screen, so a click can be
                    // turned into a position in the track.
                    .child(
                        canvas(move |bounds, _, _| bounds_cell.set(bounds), |_, _, _, _| {})
                            .absolute()
                            .size_full(),
                    ),
            );

        div()
            .flex()
            .items_center()
            .gap_5()
            .h(px(72.))
            .px_4()
            .bg(rgb(color::PANEL))
            .border_t_1()
            .border_color(rgb(color::BORDER))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        icon_button("previous", "icons/previous.svg", false)
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.previous(cx))),
                    )
                    .child(
                        icon_button(
                            "play-pause",
                            if playing {
                                "icons/pause.svg"
                            } else {
                                "icons/play.svg"
                            },
                            true,
                        )
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.toggle_play(cx))),
                    )
                    .child(
                        icon_button("next", "icons/next.svg", false)
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.next(cx))),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(title)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .text_xs()
                            .text_color(rgb(color::MUTED))
                            .child(div().w(px(44.)).flex().justify_end().child(clock(elapsed)))
                            .child(seek_bar)
                            .child(div().w(px(44.)).child(clock(total))),
                    ),
            )
    }
}

impl Render for AuroraApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.tidy_covers(window);
        let body = match &self.load_state {
            LoadState::Loading => message("Loading your library…".to_owned(), color::MUTED),
            LoadState::Failed(error) => message(error.clone(), color::ERROR),
            LoadState::Ready if self.cover_wall => {
                div().size_full().child(self.render_wall(window, cx))
            }
            LoadState::Ready => div()
                .flex()
                .size_full()
                .child(self.render_artists(cx).into_any_element())
                .child(self.render_albums(cx).into_any_element())
                .child(div().flex_1().h_full().flex().child(self.render_tracks(cx))),
        };

        div()
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key_down))
            // The size slider keeps following the mouse while the button is held.
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                if this.dragging_size {
                    if event.pressed_button == Some(MouseButton::Left) {
                        this.drag_size_to(event.position.x, cx);
                    } else {
                        this.dragging_size = false;
                        cx.notify();
                    }
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _, cx| {
                    if this.dragging_size {
                        this.dragging_size = false;
                        cx.notify();
                    }
                }),
            )
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(color::BACKGROUND))
            .text_color(rgb(color::TEXT))
            .font_family("Segoe UI")
            .text_sm()
            .child(self.render_header(cx))
            .child(div().flex_1().min_h_0().child(body))
            .child(self.render_now_playing(cx))
    }
}

// ── Small building blocks ───────────────────────────────────────────────

/// A titled column. The track column grows; the others have a fixed width.
fn panel(title: &'static str, count: usize) -> Div {
    div()
        .flex()
        .flex_col()
        .h_full()
        .min_h_0()
        .when(title != "Tracks", |panel| {
            panel
                .w(px(300.))
                .flex_none()
                .border_r_1()
                .border_color(rgb(color::BORDER))
        })
        .when(title == "Tracks", |panel| panel.flex_1().min_w_0())
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .h(px(34.))
                .px_3()
                .text_xs()
                .text_color(rgb(color::MUTED))
                .child(title.to_uppercase())
                .child(thousands(count)),
        )
}

fn row(ix: usize, selected: bool) -> Stateful<Div> {
    div()
        .id(ix)
        .w_full()
        .h(ROW_HEIGHT)
        .px_3()
        .flex()
        .items_center()
        .gap_3()
        .cursor_pointer()
        .when(selected, |row| row.bg(rgb(color::SELECTED)))
        .when(!selected, |row| {
            row.hover(|style| style.bg(rgb(color::HOVER)))
        })
}

fn truncated(text: impl Into<SharedString>) -> Div {
    div().flex_1().min_w_0().truncate().child(text.into())
}

fn muted(text: impl Into<SharedString>) -> Div {
    div()
        .flex_none()
        .text_xs()
        .text_color(rgb(color::MUTED))
        .child(text.into())
}

fn message(text: String, text_color: u32) -> Div {
    div()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .p_8()
        .text_color(rgb(text_color))
        .child(text)
}

fn icon_button(id: &'static str, icon: &'static str, primary: bool) -> Stateful<Div> {
    let size = if primary { px(40.) } else { px(32.) };
    div()
        .id(id)
        .size(size)
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .cursor_pointer()
        .when(primary, |button| {
            button
                .bg(rgb(color::ACCENT))
                .hover(|style| style.bg(rgb(color::ACCENT_HOVER)))
        })
        .when(!primary, |button| {
            button.hover(|style| style.bg(rgb(color::HOVER)))
        })
        .child(
            svg()
                .path(icon)
                .size(if primary { px(18.) } else { px(16.) })
                .text_color(rgb(if primary {
                    color::BACKGROUND
                } else {
                    color::TEXT
                })),
        )
}

fn text_button(id: &'static str, label: impl Into<SharedString>, primary: bool) -> Stateful<Div> {
    div()
        .id(id)
        .px_3()
        .py_1()
        .rounded_md()
        .cursor_pointer()
        .text_xs()
        .when(primary, |button| {
            button
                .bg(rgb(color::ACCENT))
                .text_color(rgb(color::BACKGROUND))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .hover(|style| style.bg(rgb(color::ACCENT_HOVER)))
        })
        .when(!primary, |button| {
            button
                .border_1()
                .border_color(rgb(color::BORDER))
                .text_color(rgb(color::TEXT))
                .hover(|style| style.bg(rgb(color::HOVER)))
        })
        .child(label.into())
}

/// 245 → "4:05", 3725 → "1:02:05".
fn clock(seconds: i64) -> String {
    let seconds = seconds.max(0);
    let (hours, minutes, seconds) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

/// 20802 → "20,802".
fn thousands(value: usize) -> String {
    let digits = value.to_string();
    let mut out = String::new();
    for (ix, digit) in digits.chars().enumerate() {
        if ix > 0 && (digits.len() - ix).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_clock_times() {
        assert_eq!(clock(0), "0:00");
        assert_eq!(clock(245), "4:05");
        assert_eq!(clock(3725), "1:02:05");
    }

    #[test]
    fn formats_thousands() {
        assert_eq!(thousands(7), "7");
        assert_eq!(thousands(20802), "20,802");
        assert_eq!(thousands(1122668), "1,122,668");
    }
}
