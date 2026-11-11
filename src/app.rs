//! The main window: Artists → Albums → Tracks, with a now-playing bar.

use crate::{
    audio::Audio,
    library::{Artist, Catalog, Track, catalog_path},
    update::{self, Release},
};
use gpui::{
    Bounds, ClickEvent, Context, Div, FocusHandle, KeyDownEvent, MouseButton, MouseDownEvent,
    Pixels, ScrollStrategy, SharedString, Stateful, Task, UniformListScrollHandle, Window, canvas,
    div, prelude::*, px, relative, rgb, svg, uniform_list,
};
use std::{
    cell::Cell,
    ops::Range,
    rc::Rc,
    time::{Duration, Instant},
};

const ROW_HEIGHT: Pixels = px(30.);
const TICK: Duration = Duration::from_millis(250);
/// Keystrokes this close together build one search, like "pink f".
const TYPE_AHEAD_WINDOW: Duration = Duration::from_millis(1000);

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

pub struct AuroraApp {
    focus_handle: FocusHandle,
    load_state: LoadState,
    catalog: Option<Catalog>,
    artists: Vec<Artist>,
    album_count: usize,

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
            let artists = catalog.artists()?;
            Ok::<_, String>((catalog, artists))
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

    fn finish_loading(
        &mut self,
        result: Result<(Catalog, Vec<Artist>), String>,
        cx: &mut Context<Self>,
    ) {
        match result {
            Ok((catalog, artists)) => {
                self.album_count = artists.iter().map(|artist| artist.albums.len()).sum();
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
            LoadState::Ready => format!(
                "{} artists · {} albums",
                thousands(self.artists.len()),
                thousands(self.album_count)
            )
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
                            .child("Aurora"),
                    )
                    .child(muted(concat!("v", env!("CARGO_PKG_VERSION")))),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_4()
                    .child(div().text_sm().text_color(rgb(color::MUTED)).child(status))
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
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match &self.load_state {
            LoadState::Loading => message("Loading your library…".to_owned(), color::MUTED),
            LoadState::Failed(error) => message(error.clone(), color::ERROR),
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
