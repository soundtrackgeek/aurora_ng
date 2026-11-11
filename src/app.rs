//! The main window: Artists → Albums → Tracks, or a wall of album covers,
//! with a now-playing bar.

use crate::{
    audio::Audio,
    covers::{self, CoverIndex},
    library::{Album, Artist, Catalog, Track, catalog_path},
    music_library,
    rating::{self, PendingRating, Stars},
    search,
    settings::{self, SavedWindow, Settings},
    text_input::{InputEvent, TextInput},
    update::{self, Release},
};
use gpui::{
    Bounds, ClickEvent, Context, Div, Entity, FocusHandle, Focusable, ImageSource, KeyDownEvent,
    ListAlignment, ListOffset, ListState, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ObjectFit, Pixels, RenderImage, ScrollStrategy, SharedString, Stateful,
    Subscription, Task, UniformListScrollHandle, Window, WindowBounds, actions, canvas, div, img,
    list, prelude::*, px, relative, rgb, svg, uniform_list,
};
use rusqlite::InterruptHandle;
use std::{
    cell::Cell,
    collections::{HashMap, HashSet, VecDeque},
    ops::Range,
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};

const ROW_HEIGHT: Pixels = px(30.);
const TICK: Duration = Duration::from_millis(250);
/// Keystrokes this close together build one search, like "pink f".
const TYPE_AHEAD_WINDOW: Duration = Duration::from_millis(1000);
/// A search starts once typing has paused this long, so it does not run for
/// every letter of a query. Enter searches right away.
const SEARCH_DELAY: Duration = Duration::from_secs(2);

/// Cover wall tiles: the square cover, then two lines of text when there is room.
const TILE_MIN: f32 = 50.;
const TILE_MAX: f32 = 500.;
/// Tiles narrower than this show only the cover.
const TILE_MIN_FOR_TEXT: f32 = 100.;
/// From this cover size, every cover shows its album's info under the title,
/// and the details panel leaves it out.
const TILE_MIN_FOR_INFO: f32 = 300.;
/// Room for the title and artist lines under a cover.
const TILE_TEXT_HEIGHT: Pixels = px(52.);
/// Room for the info and rating lines under the title on big covers.
const TILE_INFO_HEIGHT: Pixels = px(44.);
/// Memory for thumbnails; the ones off screen longest are dropped past this.
const COVER_MEMORY_BYTES: usize = 192 * 1024 * 1024;
/// How long to wait before giving Music Library a rating again, when it was
/// busy, and when it turned the file down.
const RATING_SYNC_RETRY: Duration = Duration::from_secs(30);
const RATING_SYNC_RETRY_AFTER_ERROR: Duration = Duration::from_secs(10 * 60);
/// How long "Rating saved" and the like stay in the now-playing bar.
const RATING_STATUS_TIME: Duration = Duration::from_secs(6);

actions!(aurora, [FocusSearch]);

pub(crate) mod color {
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
    pub const STAR: u32 = 0xfbbf24;
    pub const LOVE: u32 = 0xf472b6;
    /// Empty stars and hearts.
    pub const UNSET: u32 = 0x3a4052;
}

/// An item in the cover wall's list: a row of covers, or the details of the
/// open album, which sits right under its row.
#[derive(Clone, Copy, PartialEq, Debug)]
enum WallItem {
    Row(usize),
    Details,
}

enum UpdateState {
    Idle,
    Checking,
    UpToDate,
    Available(Release),
    Installing(Release),
    Failed(String),
}

enum SearchState {
    Idle,
    /// The query being run.
    Searching(String),
    Failed(String),
}

enum LoadState {
    Loading,
    Ready,
    Failed(String),
}

/// Work on a rating, done in the background one at a time, in order.
#[derive(Clone)]
enum RatingJob {
    /// Write the rating into the MP3.
    Save {
        path: PathBuf,
        album_id: String,
        title: String,
        stars: Option<Stars>,
    },
    /// Have Music Library read an MP3 whose rating was saved.
    Sync { path: PathBuf, album_id: String },
}

struct RatingStatus {
    text: String,
    error: bool,
    at: Instant,
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
    /// Every album artist in the catalog.
    library: Vec<Artist>,
    library_album_count: usize,
    /// The artists and albums shown: the library, or what the search found.
    artists: Vec<Artist>,
    album_count: usize,

    search_input: Entity<TextInput>,
    /// The query the shown albums match; `None` shows the whole library.
    search: Option<String>,
    search_state: SearchState,
    /// The delay before searching, then the search itself. Replacing it
    /// cancels both.
    search_task: Option<Task<()>>,
    /// Stops a search the catalog is still running when a newer one starts.
    search_interrupt: Option<InterruptHandle>,
    /// Whether the search filters the albums, rather than only sorting them.
    filtered: bool,
    /// The cover wall's order from `sort:`; the wall lists artist by artist
    /// without one.
    sort: Option<search::Sort>,
    /// The draw `sort:random` shows; a new seed shuffles again.
    shuffle_seed: u64,
    /// Wall position → album, counting albums artist by artist. Empty
    /// without a sort.
    wall_order: Vec<usize>,
    /// The other way round: album → wall position.
    wall_position: Vec<usize>,
    _search_events: Subscription,

    /// Where each artist's albums start in the cover wall, which lists every
    /// album in artist order.
    album_offsets: Vec<usize>,
    /// How many albums have a cover, or why none do.
    covers_found: Result<usize, String>,

    cover_wall: bool,
    /// Rows of covers, plus the open album's details when there is one.
    wall_list: ListState,
    /// The wall position of the album whose details are open.
    open_album: Option<usize>,
    /// Frames left to scroll newly opened details into view; their height is
    /// only known once they have been drawn.
    reveal_details: u8,
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

    /// The settings the app started with: the album to go back to once the
    /// library has loaded, and what to save if it closes before then.
    restore: Settings,
    /// Where the window is, kept so the settings can be saved without it.
    window_bounds: Option<WindowBounds>,

    /// Ratings waiting their turn, oldest first.
    rating_jobs: VecDeque<RatingJob>,
    /// The rating job running now; dropping the task does not stop it.
    rating_running: Option<(RatingJob, Task<()>)>,
    /// Ratings saved in MP3s that Music Library has not read yet; they are
    /// shown instead of the catalog's until it has.
    pending_ratings: Vec<PendingRating>,
    /// When to hand the pending ratings to Music Library again.
    next_rating_sync: Instant,
    rating_status: Option<RatingStatus>,
    /// The star under the mouse, to preview the rating it gives: where the
    /// stars are, the track id, and the star.
    star_hover: Option<(&'static str, i64, u8)>,
}

impl AuroraApp {
    pub fn new(settings: Settings, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle);

        let search_input = cx.new(|cx| {
            TextInput::new(
                "Search albums — aartist:\"bon jovi\", year:1980..1989 AND cr:..99   (Ctrl+K)",
                cx,
            )
        });
        let search_events = cx.subscribe_in(&search_input, window, Self::on_search_event);

        // Remember how everything looks for next time.
        let this = cx.entity().downgrade();
        window.on_window_should_close(cx, move |window, cx| {
            this.update(cx, |this, _| {
                this.window_bounds = Some(window.window_bounds());
                this.current_settings().save();
            })
            .ok();
            true
        });

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
            library: Vec::new(),
            library_album_count: 0,
            artists: Vec::new(),
            album_count: 0,
            search_input,
            search: None,
            search_state: SearchState::Idle,
            search_task: None,
            search_interrupt: None,
            filtered: false,
            sort: None,
            shuffle_seed: settings.shuffle_seed.unwrap_or_else(search::new_seed),
            wall_order: Vec::new(),
            wall_position: Vec::new(),
            _search_events: search_events,
            album_offsets: Vec::new(),
            covers_found: Ok(0),
            cover_wall: settings.cover_wall,
            wall_list: ListState::new(0, ListAlignment::Top, px(600.)),
            open_album: None,
            reveal_details: 0,
            wall_columns: 1,
            wall_row_height: px(0.),
            tile_size: if settings.cover_size.is_finite() {
                settings.cover_size.clamp(TILE_MIN, TILE_MAX)
            } else {
                settings::DEFAULT_COVER_SIZE
            },
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
            restore: settings,
            window_bounds: None,
            rating_jobs: VecDeque::new(),
            rating_running: None,
            pending_ratings: rating::load_pending(),
            // Ratings left from last time go to Music Library once the app is up.
            next_rating_sync: Instant::now() + Duration::from_secs(5),
            rating_status: None,
            star_hover: None,
        }
    }

    fn finish_loading(&mut self, result: Result<Loaded, String>, cx: &mut Context<Self>) {
        match result {
            Ok(Loaded {
                catalog,
                artists,
                covers,
            }) => {
                self.covers_found = covers;
                self.catalog = Some(catalog);
                self.library_album_count = artists.iter().map(|artist| artist.albums.len()).sum();
                self.library = artists;
                self.set_artists(self.library.clone());
                self.load_state = LoadState::Ready;
                self.reset_wall_items();
                if !self.restore_album(cx) && !self.artists.is_empty() {
                    self.select_artist(0, cx);
                }
                // Search again for what was shown last time; the album
                // stays selected if it is among the results.
                if !self.restore.search.is_empty() {
                    let query = self.restore.search.clone();
                    self.search_input
                        .update(cx, |input, cx| input.set_text(query, cx));
                    self.run_search(cx);
                }
            }
            Err(error) => self.load_state = LoadState::Failed(error),
        }
        cx.notify();
    }

    // ── Settings ────────────────────────────────────────────────────────

    /// Goes back to the album that was selected when the app last closed,
    /// with its details open again if they were. False if it is gone.
    fn restore_album(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(id) = self.restore.album_id.as_deref() else {
            return false;
        };
        let Some((artist, album)) = self.find_album(id) else {
            return false;
        };
        self.selected_artist = Some(artist);
        self.select_album(album, cx);

        if let Some(track_id) = self.restore.track_id {
            self.selected_track = self.tracks.iter().position(|track| track.id == track_id);
            if let Some(track) = self.selected_track {
                self.track_scroll
                    .scroll_to_item(track, ScrollStrategy::Center);
            }
        }

        let ix = self.wall_index(artist, album);
        if self.cover_wall && self.restore.details_open {
            self.open_details(ix);
        }
        self.scroll_wall_to_album(ix);
        self.artist_scroll
            .scroll_to_item(artist, ScrollStrategy::Center);
        self.album_scroll
            .scroll_to_item(album, ScrollStrategy::Center);
        true
    }

    /// How the app looks right now, to open the same way next time.
    fn current_settings(&self) -> Settings {
        // Until the library has loaded, keep the album from last time.
        let (album_id, details_open, track_id) = if matches!(self.load_state, LoadState::Ready) {
            (
                self.selected_artist
                    .zip(self.selected_album)
                    .map(|(artist, album)| self.artists[artist].albums[album].id.clone()),
                self.open_album.is_some(),
                self.selected_track
                    .and_then(|ix| self.tracks.get(ix))
                    .map(|track| track.id),
            )
        } else {
            (
                self.restore.album_id.clone(),
                self.restore.details_open,
                self.restore.track_id,
            )
        };
        let window = match self.window_bounds {
            Some(bounds) => {
                let maximized = !matches!(bounds, WindowBounds::Windowed(_));
                let bounds = bounds.get_bounds();
                Some(SavedWindow {
                    x: f32::from(bounds.origin.x),
                    y: f32::from(bounds.origin.y),
                    width: f32::from(bounds.size.width),
                    height: f32::from(bounds.size.height),
                    maximized,
                })
            }
            None => self.restore.window,
        };
        Settings {
            cover_wall: self.cover_wall,
            cover_size: self.tile_size,
            album_id,
            details_open,
            track_id,
            search: match self.load_state {
                LoadState::Ready => self.search.clone().unwrap_or_default(),
                _ => self.restore.search.clone(),
            },
            shuffle_seed: Some(self.shuffle_seed),
            window,
        }
    }

    // ── Search ──────────────────────────────────────────────────────────

    fn on_search_event(
        &mut self,
        _: &Entity<TextInput>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Changed => {
                // A query still running is for text that is gone now.
                if let Some(interrupt) = self.search_interrupt.take() {
                    interrupt.interrupt();
                }
                self.search_state = SearchState::Idle;
                if self.search_text(cx).trim().is_empty() {
                    // Clearing the box shows everything at once.
                    self.run_search(cx);
                } else {
                    self.search_task = Some(cx.spawn(async move |this, cx| {
                        cx.background_executor().timer(SEARCH_DELAY).await;
                        this.update(cx, |this, cx| this.run_search(cx)).ok();
                    }));
                }
            }
            InputEvent::Submit => {
                // Enter on a random sort that is already shown draws again.
                if !self.run_search(cx) && self.sort == Some(search::Sort::Random) {
                    self.reshuffle(cx);
                }
            }
            // Esc clears the search; a second Esc leaves the box.
            InputEvent::Cancel => {
                if self.search_text(cx).is_empty() {
                    window.focus(&self.focus_handle);
                } else {
                    self.search_input
                        .update(cx, |input, cx| input.set_text("", cx));
                    self.run_search(cx);
                }
            }
        }
        cx.notify();
    }

    fn search_text(&self, cx: &Context<Self>) -> String {
        self.search_input.read(cx).text().to_owned()
    }

    /// Searches for what is in the box now, unless those albums are already
    /// shown. False when there was nothing new to search for.
    fn run_search(&mut self, cx: &mut Context<Self>) -> bool {
        if !matches!(self.load_state, LoadState::Ready) {
            return false;
        }
        let query = self.search_text(cx).trim().to_owned();
        match &self.search_state {
            SearchState::Searching(running) if *running == query => return false,
            SearchState::Idle if self.search.as_deref().unwrap_or("") == query => {
                self.search_task = None;
                return false;
            }
            _ => {}
        }
        self.search_task = None;
        if let Some(interrupt) = self.search_interrupt.take() {
            interrupt.interrupt();
        }
        cx.notify();

        let parsed = search::split_sort(&query)
            .and_then(|(filter, sort)| search::parse(&filter).map(|parsed| (parsed, sort)));
        let (parsed, sort) = match parsed {
            Ok(parsed) => parsed,
            Err(error) => {
                self.search_state = SearchState::Failed(error);
                return true;
            }
        };
        if parsed.is_empty() {
            // Nothing to filter, though there may be a sort.
            self.search_state = SearchState::Idle;
            self.search = (!query.is_empty()).then_some(query);
            self.sort = sort;
            self.show_matches(None, cx);
            return true;
        }
        // A connection of its own, so the search runs off the UI thread.
        let catalog = match catalog_path().and_then(|path| Catalog::open(&path)) {
            Ok(catalog) => catalog,
            Err(error) => {
                self.search_state = SearchState::Failed(error);
                return true;
            }
        };
        self.search_interrupt = Some(catalog.interrupt_handle());
        self.search_state = SearchState::Searching(query.clone());
        let job = cx
            .background_executor()
            .spawn(async move { catalog.search_album_ids(&parsed) });
        self.search_task = Some(cx.spawn(async move |this, cx| {
            let result = job.await;
            this.update(cx, |this, cx| this.finish_search(query, sort, result, cx))
                .ok();
        }));
        true
    }

    fn finish_search(
        &mut self,
        query: String,
        sort: Option<search::Sort>,
        result: Result<HashSet<String>, String>,
        cx: &mut Context<Self>,
    ) {
        // Ignore a search that was replaced while it ran.
        if !matches!(&self.search_state, SearchState::Searching(running) if *running == query) {
            return;
        }
        self.search_interrupt = None;
        self.search_task = None;
        match result {
            Ok(ids) => {
                self.search_state = SearchState::Idle;
                self.search = Some(query);
                self.sort = sort;
                self.show_matches(Some(&ids), cx);
            }
            Err(error) => self.search_state = SearchState::Failed(error),
        }
        cx.notify();
    }

    fn clear_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search_input
            .update(cx, |input, cx| input.set_text("", cx));
        self.run_search(cx);
        window.focus(&self.focus_handle);
    }

    fn focus_search(&mut self, _: &FocusSearch, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.search_input.focus_handle(cx));
        self.search_input
            .update(cx, |input, cx| input.select_all_text(cx));
    }

    /// Shows the albums with these ids, or every album. The selected album
    /// stays selected, with its details open, when it is still shown.
    fn show_matches(&mut self, ids: Option<&HashSet<String>>, cx: &mut Context<Self>) {
        let selected_id = self.selected_album_id();
        let details_were_open = self.open_album.is_some();
        let artists = match ids {
            None => self.library.clone(),
            Some(ids) => self
                .library
                .iter()
                .filter_map(|artist| {
                    let albums: Vec<Album> = artist
                        .albums
                        .iter()
                        .filter(|album| ids.contains(&album.id))
                        .cloned()
                        .collect();
                    (!albums.is_empty()).then(|| Artist {
                        name: artist.name.clone(),
                        sort_key: artist.sort_key.clone(),
                        albums,
                    })
                })
                .collect(),
        };
        self.filtered = ids.is_some();
        self.set_artists(artists);
        self.update_wall_order();
        self.forget_wall();

        match selected_id.and_then(|id| self.find_album(&id)) {
            Some((artist, album)) => {
                // Its tracks are already loaded.
                self.selected_artist = Some(artist);
                self.selected_album = Some(album);
                let ix = self.wall_index(artist, album);
                if details_were_open && self.cover_wall {
                    self.open_details(ix);
                }
                self.scroll_wall_to_album(ix);
                self.artist_scroll
                    .scroll_to_item(artist, ScrollStrategy::Center);
                self.album_scroll
                    .scroll_to_item(album, ScrollStrategy::Center);
            }
            None if !self.artists.is_empty() => {
                self.select_artist(0, cx);
                self.scroll_wall_to_album(0);
                self.artist_scroll.scroll_to_item(0, ScrollStrategy::Top);
            }
            None => {
                self.selected_artist = None;
                self.select_album(0, cx);
            }
        }
        cx.notify();
    }

    /// A new draw for `sort:random`, seen from the top of the wall.
    fn reshuffle(&mut self, cx: &mut Context<Self>) {
        self.shuffle_seed = search::new_seed();
        self.update_wall_order();
        self.forget_wall();
        self.scroll_wall_to_album(0);
        cx.notify();
    }

    /// Works out the wall order for the sort, if there is one.
    fn update_wall_order(&mut self) {
        self.wall_order = match self.sort {
            Some(sort) => search::wall_order(&self.artists, sort, self.shuffle_seed),
            None => Vec::new(),
        };
        self.wall_position = vec![0; self.wall_order.len()];
        for (position, &album) in self.wall_order.iter().enumerate() {
            self.wall_position[album] = position;
        }
    }

    /// After the wall's albums moved: drops the covers, which are kept by
    /// wall position, and closes the details.
    fn forget_wall(&mut self) {
        for (_, cover) in self.covers.drain() {
            match cover {
                Cover::Ready { image, .. }
                | Cover::Loading {
                    previous: Some(image),
                    ..
                } => self.stale_images.push(image),
                _ => {}
            }
        }
        self.open_album = None;
        self.reveal_details = 0;
        self.reset_wall_items();
    }

    /// Shows these artists, and works out where their albums are on the wall.
    fn set_artists(&mut self, artists: Vec<Artist>) {
        self.album_offsets = artists
            .iter()
            .scan(0, |offset, artist| {
                let start = *offset;
                *offset += artist.albums.len();
                Some(start)
            })
            .collect();
        self.album_count = artists.iter().map(|artist| artist.albums.len()).sum();
        self.artists = artists;
    }

    /// The shown artist and album with this catalog id.
    fn find_album(&self, id: &str) -> Option<(usize, usize)> {
        self.artists.iter().enumerate().find_map(|(ix, artist)| {
            let album = artist.albums.iter().position(|album| album.id == id)?;
            Some((ix, album))
        })
    }

    fn selected_album_id(&self) -> Option<String> {
        let (artist, album) = self.selected_artist.zip(self.selected_album)?;
        Some(self.artists.get(artist)?.albums.get(album)?.id.clone())
    }

    // ── Browsing ────────────────────────────────────────────────────────

    fn select_artist(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.selected_artist = Some(ix);
        self.album_scroll.scroll_to_item(0, ScrollStrategy::Top);
        self.select_album(0, cx);
    }

    fn select_album(&mut self, ix: usize, cx: &mut Context<Self>) {
        // The details always belong to the selected album.
        self.close_details();
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
            Ok(mut tracks) => {
                self.apply_known_ratings(&mut tracks);
                self.tracks = tracks;
            }
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
                self.scroll_wall_to_album(self.wall_index(ix, 0));
            }
        }
    }

    // ── Cover wall ──────────────────────────────────────────────────────

    fn toggle_cover_wall(&mut self, cx: &mut Context<Self>) {
        self.cover_wall = !self.cover_wall;
        // Keep the selected album in view when switching.
        if let (Some(artist), Some(album)) = (self.selected_artist, self.selected_album) {
            if self.cover_wall {
                self.scroll_wall_to_album(self.wall_index(artist, album));
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
        let album = self.wall_order.get(ix).copied().unwrap_or(ix);
        let artist = self.album_offsets.partition_point(|&start| start <= album) - 1;
        (artist, album - self.album_offsets[artist])
    }

    /// Where an artist's album is on the wall.
    fn wall_index(&self, artist: usize, album: usize) -> usize {
        let album = self.album_offsets[artist] + album;
        self.wall_position.get(album).copied().unwrap_or(album)
    }

    /// The number of rows of covers.
    fn wall_rows(&self) -> usize {
        self.album_count.div_ceil(self.wall_columns)
    }

    /// The row the open album's details follow.
    fn details_row(&self) -> Option<usize> {
        self.open_album.map(|ix| ix / self.wall_columns)
    }

    fn wall_item_count(&self) -> usize {
        self.wall_rows() + usize::from(self.open_album.is_some())
    }

    fn wall_item(&self, item: usize) -> WallItem {
        match self.details_row() {
            Some(row) if item == row + 1 => WallItem::Details,
            Some(row) if item > row + 1 => WallItem::Row(item - 1),
            _ => WallItem::Row(item),
        }
    }

    /// The list item that shows row `row`.
    fn row_item(&self, row: usize) -> usize {
        match self.details_row() {
            Some(details) if row > details => row + 1,
            _ => row,
        }
    }

    /// The first album at the top of the wall's view.
    fn top_wall_album(&self) -> usize {
        let top = self.wall_list.logical_scroll_top();
        if top.item_ix >= self.wall_list.item_count() {
            return 0;
        }
        match self.wall_item(top.item_ix) {
            WallItem::Row(row) => row * self.wall_columns,
            WallItem::Details => self.open_album.unwrap_or(0),
        }
    }

    /// Makes the list match the rows, forgetting every measured height.
    fn reset_wall_items(&mut self) {
        let old = self.wall_list.item_count();
        self.wall_list.splice(0..old, self.wall_item_count());
    }

    /// Scrolls so the row with wall position `ix` is at the top.
    fn scroll_wall_to_album(&mut self, ix: usize) {
        let ix = ix.min(self.album_count.saturating_sub(1));
        let item = self.row_item(ix / self.wall_columns);
        self.wall_list.scroll_to(ListOffset {
            item_ix: item,
            offset_in_item: px(0.),
        });
    }

    /// Opens the details for wall position `ix` under its row, pushing the
    /// rows below down.
    fn open_details(&mut self, ix: usize) {
        self.close_details();
        self.open_album = Some(ix);
        let item = ix / self.wall_columns + 1;
        self.wall_list.splice(item..item, 1);
        self.reveal_details = 4;
    }

    fn close_details(&mut self) {
        if let Some(row) = self.details_row() {
            self.wall_list.splice(row + 1..row + 2, 0);
            self.open_album = None;
            self.reveal_details = 0;
        }
    }

    /// Once the new details have been measured, scrolls just enough to show
    /// them, without pushing their row of covers off the top.
    fn reveal_open_details(&mut self, window: &mut Window) {
        if self.reveal_details == 0 {
            return;
        }
        let Some(row) = self.details_row() else {
            self.reveal_details = 0;
            return;
        };
        let row_bounds = self.wall_list.bounds_for_item(row);
        let details_bounds = self.wall_list.bounds_for_item(row + 1);
        match (row_bounds, details_bounds) {
            (Some(row_bounds), Some(details_bounds)) => {
                let view = self.wall_list.viewport_bounds();
                let hidden = details_bounds.bottom() - view.bottom();
                let room = row_bounds.top() - view.top();
                if hidden > px(0.) && room > px(0.) {
                    self.wall_list.scroll_by(hidden.min(room));
                }
                self.reveal_details = 0;
            }
            // The row is above the view, so there is nothing to keep in sight.
            (None, _) if self.wall_list.logical_scroll_top().item_ix > row => {
                self.reveal_details = 0;
            }
            _ => {
                self.reveal_details -= 1;
                window.request_animation_frame();
            }
        }
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
        // The new version starts before this one quits, so save now.
        self.current_settings().save();
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
        self.tick_ratings(cx);
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

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        // Keys typed in the search box are for the search.
        if self.search_input.focus_handle(cx).is_focused(window) {
            return;
        }
        let keystroke = &event.keystroke;
        let modifiers = &keystroke.modifiers;
        // Ctrl+1 to Ctrl+5 rate the selected track, Ctrl+0 clears its rating.
        if modifiers.control
            && !modifiers.alt
            && !modifiers.shift
            && !modifiers.platform
            && let Some(digit) = keystroke.key.parse::<u8>().ok().filter(|digit| *digit <= 5)
        {
            self.rate_with_key(Stars::new(digit), cx);
            cx.stop_propagation();
            return;
        }
        if modifiers.control || modifiers.alt || modifiers.platform || modifiers.function {
            return;
        }
        let typing =
            !self.type_ahead.is_empty() && self.type_ahead_at.elapsed() < TYPE_AHEAD_WINDOW;
        if keystroke.key == "escape" && self.open_album.is_some() {
            self.close_details();
            cx.notify();
            cx.stop_propagation();
            return;
        }
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

    // ── Ratings ─────────────────────────────────────────────────────────
    //
    // As in Aurora: the rating goes into the MP3 first, then Music Library
    // is asked to read the file again, which updates its catalog. The new
    // rating shows at once and stays shown until the catalog has it.

    /// Rates a track with whole stars, or clears its rating with `None`.
    fn rate(&mut self, track: &Track, stars: Option<Stars>, cx: &mut Context<Self>) {
        self.show_rating(&track.path, stars.map(Stars::points));
        let job = RatingJob::Save {
            path: track.path.clone(),
            album_id: track.album_id.clone(),
            title: track.title.clone(),
            stars,
        };
        // A newer rating replaces one for the same track still waiting.
        let waiting = self
            .rating_jobs
            .iter_mut()
            .find(|waiting| matches!(waiting, RatingJob::Save { path, .. } if *path == track.path));
        match waiting {
            Some(waiting) => *waiting = job,
            None => self.rating_jobs.push_back(job),
        }
        self.rating_status = None;
        self.run_rating_job(cx);
        cx.notify();
    }

    /// Ctrl+0–5: rates the selected track, or the one playing when no track
    /// is selected.
    fn rate_with_key(&mut self, stars: Option<Stars>, cx: &mut Context<Self>) {
        let track = self
            .selected_track
            .and_then(|ix| self.tracks.get(ix))
            .or_else(|| self.current_track())
            .cloned();
        if let Some(track) = track {
            self.rate(&track, stars, cx);
        }
    }

    /// Shows a rating on the track in the album and in what is playing.
    fn show_rating(&mut self, path: &Path, rating: Option<i64>) {
        for track in self.tracks.iter_mut().chain(self.queue.iter_mut()) {
            if track.path == path {
                track.rating = rating;
            }
        }
    }

    /// The rating a track has that the catalog may not show yet: one being
    /// saved, or saved and waiting for Music Library.
    fn known_rating(&self, path: &Path) -> Option<Option<i64>> {
        let saving = self
            .rating_jobs
            .iter()
            .rev()
            .chain(self.rating_running.as_ref().map(|(job, _)| job))
            .find_map(|job| match job {
                RatingJob::Save {
                    path: saving,
                    stars,
                    ..
                } if saving == path => Some(stars.map(Stars::points)),
                _ => None,
            });
        saving.or_else(|| {
            self.pending_ratings
                .iter()
                .find(|pending| pending.path == path)
                .map(|pending| pending.rating)
        })
    }

    fn apply_known_ratings(&self, tracks: &mut [Track]) {
        for track in tracks {
            if let Some(rating) = self.known_rating(&track.path) {
                track.rating = rating;
            }
        }
    }

    /// Starts the next rating job, unless one is running.
    fn run_rating_job(&mut self, cx: &mut Context<Self>) {
        if self.rating_running.is_some() {
            return;
        }
        let Some(job) = self.rating_jobs.pop_front() else {
            return;
        };
        let work = {
            let job = job.clone();
            cx.background_executor().spawn(async move {
                match job {
                    RatingJob::Save { path, stars, .. } => rating::write_rating(&path, stars),
                    RatingJob::Sync { path, .. } => music_library::sync_file(&path),
                }
            })
        };
        let task = cx.spawn(async move |this, cx| {
            let result = work.await;
            this.update(cx, |this, cx| this.finish_rating_job(result, cx))
                .ok();
        });
        self.rating_running = Some((job, task));
    }

    fn finish_rating_job(&mut self, result: Result<(), String>, cx: &mut Context<Self>) {
        let Some((job, _)) = self.rating_running.take() else {
            return;
        };
        match (job, result) {
            (
                RatingJob::Save {
                    path,
                    album_id,
                    stars,
                    ..
                },
                Ok(()),
            ) => {
                self.pending_ratings.retain(|pending| pending.path != path);
                self.pending_ratings.push(PendingRating {
                    path: path.clone(),
                    album_id: album_id.clone(),
                    rating: stars.map(Stars::points),
                });
                rating::save_pending(&self.pending_ratings);
                // Music Library reads it next, before any other rating.
                self.rating_jobs
                    .push_front(RatingJob::Sync { path, album_id });
            }
            (
                RatingJob::Save {
                    album_id, title, ..
                },
                Err(error),
            ) => {
                self.set_rating_status(format!("Could not rate “{title}”: {error}"), true);
                // Back to the rating the MP3 has.
                self.refresh_album(&album_id);
            }
            (RatingJob::Sync { path, album_id }, Ok(())) => {
                self.pending_ratings.retain(|pending| pending.path != path);
                rating::save_pending(&self.pending_ratings);
                self.refresh_album(&album_id);
                if self.rating_jobs.is_empty() {
                    self.set_rating_status("Rating saved".to_owned(), false);
                }
            }
            (RatingJob::Sync { .. }, Err(error)) => {
                // The others would most likely fail the same way; they stay
                // pending and are tried again later.
                self.rating_jobs
                    .retain(|job| !matches!(job, RatingJob::Sync { .. }));
                let temporary = music_library::is_temporary(&error);
                self.next_rating_sync = Instant::now()
                    + if temporary {
                        RATING_SYNC_RETRY
                    } else {
                        RATING_SYNC_RETRY_AFTER_ERROR
                    };
                self.set_rating_status(
                    format!(
                        "The rating is saved in the MP3; Music Library will be given it again soon. {error}"
                    ),
                    !temporary,
                );
            }
        }
        self.run_rating_job(cx);
        cx.notify();
    }

    fn set_rating_status(&mut self, text: String, error: bool) {
        self.rating_status = Some(RatingStatus {
            text,
            error,
            at: Instant::now(),
        });
    }

    /// Hands pending ratings to Music Library again when it is time, and
    /// clears a status message that has been shown long enough.
    fn tick_ratings(&mut self, cx: &mut Context<Self>) {
        if self
            .rating_status
            .as_ref()
            .is_some_and(|status| !status.error && status.at.elapsed() > RATING_STATUS_TIME)
        {
            self.rating_status = None;
            cx.notify();
        }
        let idle = self.rating_running.is_none() && self.rating_jobs.is_empty();
        if !idle || self.pending_ratings.is_empty() || Instant::now() < self.next_rating_sync {
            return;
        }
        self.next_rating_sync = Instant::now() + RATING_SYNC_RETRY;
        self.rating_jobs
            .extend(self.pending_ratings.iter().map(|pending| RatingJob::Sync {
                path: pending.path.clone(),
                album_id: pending.album_id.clone(),
            }));
        self.run_rating_job(cx);
        cx.notify();
    }

    /// Reads an album's track ratings, its rating and its Album Score from the
    /// catalog again, where they are shown.
    fn refresh_album(&mut self, album_id: &str) {
        let Some(catalog) = &self.catalog else {
            return;
        };
        let Ok(mut fresh) = catalog.tracks(album_id) else {
            return;
        };
        let album_rating = catalog.album_rating(album_id);
        self.apply_known_ratings(&mut fresh);
        for track in self.tracks.iter_mut().chain(self.queue.iter_mut()) {
            if let Some(fresh) = fresh.iter().find(|fresh| fresh.path == track.path) {
                track.rating = fresh.rating;
            }
        }
        if let Ok((rating, score)) = album_rating {
            for album in self
                .library
                .iter_mut()
                .chain(self.artists.iter_mut())
                .flat_map(|artist| artist.albums.iter_mut())
                .filter(|album| album.id == album_id)
            {
                album.rating = rating;
                album.score = score;
            }
        }
    }

    /// Five stars to click. A star gives the track that many stars; clicking
    /// the stars it already has clears its rating. Never half stars.
    fn rating_control(
        &self,
        place: &'static str,
        track: &Track,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let size = px(13.);
        let id = track.id;
        let hovered = self
            .star_hover
            .filter(|(hover_place, track, _)| *hover_place == place && *track == id)
            .map(|(_, _, star)| star);
        let halves = match hovered {
            Some(star) => i64::from(star) * 2,
            None => star_halves(track.rating),
        };
        div()
            .id(place)
            .flex()
            .items_center()
            .children((1..=5u8).map(|star| {
                let filled = (halves - i64::from(star - 1) * 2).clamp(0, 2);
                let target = track.clone();
                div()
                    .id(star as usize)
                    .px(px(1.))
                    .py_1()
                    .cursor_pointer()
                    .child(star_slot(filled, size, color::STAR))
                    .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                        if *hovered {
                            this.star_hover = Some((place, id, star));
                        } else if this.star_hover == Some((place, id, star)) {
                            this.star_hover = None;
                        } else {
                            return;
                        }
                        cx.notify();
                    }))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        // The click is for the stars, not for the row under them.
                        cx.stop_propagation();
                        let stars = if target.rating == Some(i64::from(star) * 20) {
                            None
                        } else {
                            Stars::new(star)
                        };
                        this.rate(&target, stars, cx);
                    }))
            }))
    }

    /// What ratings are doing, for the now-playing bar.
    fn rating_status_line(&self) -> Option<(String, u32)> {
        if self.rating_running.is_some() || !self.rating_jobs.is_empty() {
            let saving = self
                .rating_jobs
                .iter()
                .chain(self.rating_running.as_ref().map(|(job, _)| job))
                .any(|job| matches!(job, RatingJob::Save { .. }));
            return Some((
                if saving {
                    "Saving rating…"
                } else {
                    "Updating Music Library…"
                }
                .to_owned(),
                color::MUTED,
            ));
        }
        if let Some(status) = &self.rating_status {
            let color = if status.error {
                color::ERROR
            } else {
                color::MUTED
            };
            return Some((status.text.clone(), color));
        }
        match self.pending_ratings.len() {
            0 => None,
            1 => Some((
                "1 rating waiting for Music Library".to_owned(),
                color::MUTED,
            )),
            count => Some((
                format!("{count} ratings waiting for Music Library"),
                color::MUTED,
            )),
        }
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
            LoadState::Ready if self.filtered => format!(
                "{} of {} albums · {} artists",
                thousands(self.album_count),
                thousands(self.library_album_count),
                thousands(self.artists.len())
            )
            .into(),
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
            .child(self.render_search(cx))
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

    /// The search box, a button to clear it, and what the search is doing.
    fn render_search(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let has_text = !self.search_input.read(cx).text().is_empty();
        let state = match &self.search_state {
            SearchState::Idle if self.filtered && self.album_count == 0 => {
                Some(("No albums match".to_owned(), color::MUTED))
            }
            SearchState::Idle => None,
            SearchState::Searching(_) => Some(("Searching…".to_owned(), color::MUTED)),
            SearchState::Failed(error) => Some((error.clone(), color::ERROR)),
        };
        div()
            .flex_1()
            .min_w_0()
            .mx_6()
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .flex_1()
                    .min_w(px(160.))
                    .max_w(px(560.))
                    .child(self.search_input.clone()),
            )
            .when(
                self.sort == Some(search::Sort::Random)
                    && matches!(self.search_state, SearchState::Idle),
                |row| {
                    row.child(
                        text_button("shuffle", "Shuffle", false).on_click(
                            cx.listener(|this, _: &ClickEvent, _, cx| this.reshuffle(cx)),
                        ),
                    )
                },
            )
            .when(has_text, |row| {
                row.child(text_button("clear-search", "Clear", false).on_click(
                    cx.listener(|this, _: &ClickEvent, window, cx| this.clear_search(window, cx)),
                ))
            })
            .when_some(state, |row, (text, text_color)| {
                row.child(
                    div()
                        .flex_shrink()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .text_color(rgb(text_color))
                        .child(text),
                )
            })
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
                            .child(this.rating_control("list", track, cx))
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
        let text_height = match (show_text, self.info_under_cover()) {
            (false, _) => px(0.),
            (true, false) => TILE_TEXT_HEIGHT,
            (true, true) => TILE_TEXT_HEIGHT + TILE_INFO_HEIGHT,
        };
        let row_height = gap + tile + text_height;
        self.thumbnail_size = covers::thumbnail_size(self.tile_size * window.scale_factor());

        let width = window.viewport_size().width;
        let columns = (((width - gap) / (tile + gap)).floor() as usize).max(1);
        let changed = columns != self.wall_columns || row_height != self.wall_row_height;
        if changed || self.wall_list.item_count() != self.wall_item_count() {
            // Rows change height or contents: keep the album at the top left
            // in view.
            let top = self.top_wall_album();
            self.wall_columns = columns;
            self.wall_row_height = row_height;
            self.reset_wall_items();
            self.scroll_wall_to_album(top);
        }
        self.reveal_open_details(window);
        // Centre the grid; the last row stays left-aligned.
        let used = tile * columns as f32 + gap * (columns as f32 - 1.);
        let inset = ((width - used) / 2.).max(gap / 2.);

        let wall = list(
            self.wall_list.clone(),
            cx.processor(move |this, item: usize, _, cx| match this.wall_item(item) {
                WallItem::Row(row) => {
                    let start = row * this.wall_columns;
                    let end = (start + this.wall_columns).min(this.album_count);
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
                        .into_any_element()
                }
                WallItem::Details => div()
                    .pt(gap)
                    .pl(inset)
                    .child(this.render_details(used, cx))
                    .into_any_element(),
            }),
        )
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
        let show_info = show_text && self.info_under_cover();

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
                .when(show_info, |tile| {
                    tile.child(
                        div()
                            .flex_none()
                            .truncate()
                            .text_xs()
                            .text_color(rgb(color::MUTED))
                            .child(self.album_info(album)),
                    )
                    .child(self.album_rating(album).text_xs())
                })
            })
            // A click opens or closes the album's details; a double-click plays it.
            .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                let double = event.click_count() >= 2;
                if this.open_album == Some(ix) && !double {
                    this.close_details();
                } else {
                    this.select_wall_album(ix, cx);
                    if this.open_album != Some(ix) {
                        this.open_details(ix);
                    }
                    if double {
                        this.play_from(0, cx);
                    }
                }
                cx.notify();
            }))
    }

    /// Big covers leave room for the open album's info under the cover.
    fn info_under_cover(&self) -> bool {
        self.tile_size >= TILE_MIN_FOR_INFO
    }

    /// "Action · Varese Sarabande · 15 tracks · 40:09", leaving out what is unknown.
    fn album_info(&self, album: &Album) -> String {
        // From the catalog's album totals: covers show this before their
        // tracks are loaded.
        let seconds = album.total_seconds.filter(|&total| total > 0);
        [
            album.genre.clone(),
            album.publisher.clone(),
            album
                .total_tracks
                .map(|tracks| format!("{tracks} {}", if tracks == 1 { "track" } else { "tracks" })),
            seconds.map(clock),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ")
    }

    /// "★★★½ 3.60 · Album Score: 3.60".
    fn album_rating(&self, album: &Album) -> Div {
        div()
            .flex()
            .items_center()
            .gap_2()
            .text_color(rgb(color::MUTED))
            .child(stars(album.rating, px(13.)))
            .child(match album.rating {
                Some(rating) => format!("{:.2}", rating as f32 / 20.),
                None => "Not rated".to_owned(),
            })
            .child("·")
            .child(format!(
                "Album Score: {}",
                album
                    .score
                    .map_or_else(|| "—".to_owned(), |score| format!("{score:.2}"))
            ))
    }

    /// The open album's details: what it is, then its tracks.
    fn render_details(&self, width: Pixels, cx: &mut Context<Self>) -> Div {
        let panel = div()
            .w(width)
            .flex()
            .flex_col()
            .gap_4()
            .p_5()
            .rounded_md()
            .bg(rgb(color::PANEL))
            .border_1()
            .border_color(rgb(color::BORDER));
        let Some(ix) = self.open_album else {
            return panel;
        };
        let (artist_ix, album_ix) = self.wall_album(ix);
        let artist = &self.artists[artist_ix];
        let album = &artist.albums[album_ix];

        let subtitle = match album.year {
            Some(year) => format!("{} · {year}", artist.name),
            None => artist.name.clone(),
        };
        let thumbnail = match self.covers.get(&ix) {
            Some(Cover::Ready { image, .. }) => Some(image.clone()),
            Some(Cover::Loading { previous, .. }) => previous.clone(),
            _ => None,
        };
        let artwork = div()
            .size(px(112.))
            .flex_none()
            .rounded_md()
            .overflow_hidden()
            .bg(rgb(color::BACKGROUND))
            .when_some(thumbnail, |artwork, image| {
                artwork.child(
                    img(ImageSource::Render(image))
                        .size_full()
                        .object_fit(ObjectFit::Cover),
                )
            });
        let header = div().flex().items_center().gap_5().child(artwork).child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_xl()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .truncate()
                        .child(album.title.clone()),
                )
                .child(
                    div()
                        .truncate()
                        .text_color(rgb(color::MUTED))
                        .child(subtitle),
                )
                .child(
                    div()
                        .truncate()
                        .text_color(rgb(color::MUTED))
                        .child(self.album_info(album)),
                )
                .child(self.album_rating(album)),
        );

        // Big covers already show this under the cover.
        let panel = panel.when(!self.info_under_cover(), |panel| panel.child(header));
        if let Some(error) = &self.tracks_error {
            return panel.child(div().text_color(rgb(color::ERROR)).child(error.clone()));
        }

        // Show the artist column only when some track is by someone else.
        let other_artists = self.tracks.iter().any(|track| track.artist != artist.name);
        let multi_disc = self.tracks.iter().any(|track| track.disc.unwrap_or(1) > 1);
        let playing_id = self.current_track().map(|track| track.id);
        let columns = |row: Stateful<Div>, cells: [Div; 6]| {
            let [number, title, track_artist, time, rating, love] = cells;
            row.child(number.w(px(36.)).flex().justify_end().flex_none())
                .child(title.flex_1().min_w_0().truncate())
                .when(other_artists, |row| {
                    row.child(track_artist.w(relative(0.25)).flex_none().truncate())
                })
                .child(time.w(px(52.)).flex().justify_end().flex_none())
                .child(rating.w(px(96.)).flex().justify_center().flex_none())
                .child(love.w(px(28.)).flex().justify_center().flex_none())
        };
        let heading = |text: &'static str| div().child(text);
        let table_header = columns(
            div()
                .id("details-header")
                .h(px(26.))
                .px_3()
                .flex()
                .items_center()
                .gap_3()
                .text_xs()
                .text_color(rgb(color::MUTED))
                .border_b_1()
                .border_color(rgb(color::BORDER)),
            [
                heading("#"),
                heading("TITLE"),
                heading("ARTIST"),
                heading("TIME"),
                heading("RATING"),
                heading(""),
            ],
        );
        let rows: Vec<_> = self
            .tracks
            .iter()
            .enumerate()
            .map(|(track_ix, track)| {
                let playing = playing_id == Some(track.id);
                let number = if playing {
                    div().child(
                        svg()
                            .path("icons/play.svg")
                            .size_3()
                            .text_color(rgb(color::ACCENT)),
                    )
                } else {
                    div()
                        .text_xs()
                        .text_color(rgb(color::MUTED))
                        .child(track.position_label(multi_disc))
                };
                let track_artist = div().text_xs().text_color(rgb(color::MUTED));
                let track_artist = if track.artist != artist.name {
                    track_artist.child(track.artist.clone())
                } else {
                    track_artist
                };
                columns(
                    row(track_ix, self.selected_track == Some(track_ix))
                        .rounded_sm()
                        .when(playing, |row| row.text_color(rgb(color::ACCENT)))
                        .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                            this.selected_track = Some(track_ix);
                            if event.click_count() >= 2 {
                                this.play_from(track_ix, cx);
                            }
                            cx.notify();
                        })),
                    [
                        number,
                        div().child(track.title.clone()),
                        track_artist,
                        div()
                            .text_xs()
                            .text_color(rgb(color::MUTED))
                            .child(track.duration_seconds.map(clock).unwrap_or_default()),
                        div().child(self.rating_control("details", track, cx)),
                        div().child(heart(track.loved, px(14.))),
                    ],
                )
            })
            .collect();
        panel.child(div().flex().flex_col().child(table_header).children(rows))
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
                .items_center()
                .gap_2()
                .min_w_0()
                .child(self.rating_control("playing", track, cx))
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
            .when_some(self.rating_status_line(), |bar, (text, text_color)| {
                bar.child(
                    div()
                        .flex_none()
                        .max_w(px(360.))
                        .text_xs()
                        .text_color(rgb(text_color))
                        .line_clamp(2)
                        .child(text),
                )
            })
    }
}

impl Render for AuroraApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.window_bounds = Some(window.window_bounds());
        self.tidy_covers(window);
        let body = match &self.load_state {
            LoadState::Loading => message("Loading your library…".to_owned(), color::MUTED),
            LoadState::Failed(error) => message(error.clone(), color::ERROR),
            LoadState::Ready if self.artists.is_empty() && self.search.is_some() => message(
                "No albums match this search. Press Esc in the search box to show everything."
                    .to_owned(),
                color::MUTED,
            ),
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
            .on_action(cx.listener(Self::focus_search))
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

/// Whole and half stars filled in for a 0–100 rating (20 per star); all
/// five are empty when there is no rating.
fn stars(rating: Option<i64>, size: Pixels) -> Div {
    let halves = star_halves(rating);
    div()
        .flex()
        .items_center()
        .gap(px(2.))
        .children((0..5).map(|star| star_slot((halves - star * 2).clamp(0, 2), size, color::STAR)))
}

/// One star with 0, 1 or 2 of its halves filled in. Ratings set in other
/// programs can have half stars, so they are shown; they are never set here.
fn star_slot(filled: i64, size: Pixels, fill: u32) -> Div {
    let icon = move |color: u32| {
        svg()
            .path("icons/star.svg")
            .size(size)
            .text_color(rgb(color))
    };
    div()
        .relative()
        .size(size)
        .flex_none()
        .child(icon(color::UNSET))
        .when(filled > 0, |slot| {
            // A half star is a full one, clipped to its left half.
            slot.child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .h(size)
                    .w(if filled == 2 { size } else { size / 2. })
                    .overflow_hidden()
                    .child(icon(fill)),
            )
        })
}

/// 0–100 → half stars, 0 to 10.
fn star_halves(rating: Option<i64>) -> i64 {
    rating.map_or(0, |rating| (rating.clamp(0, 100) + 5) / 10)
}

fn heart(loved: bool, size: Pixels) -> impl IntoElement {
    let (path, color) = if loved {
        ("icons/heart.svg", color::LOVE)
    } else {
        ("icons/heart-outline.svg", color::UNSET)
    };
    svg().path(path).size(size).text_color(rgb(color))
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
    fn rounds_ratings_to_half_stars() {
        assert_eq!(star_halves(None), 0);
        assert_eq!(star_halves(Some(20)), 2);
        assert_eq!(star_halves(Some(70)), 7);
        assert_eq!(star_halves(Some(86)), 9);
        assert_eq!(star_halves(Some(100)), 10);
    }

    #[test]
    fn formats_thousands() {
        assert_eq!(thousands(7), "7");
        assert_eq!(thousands(20802), "20,802");
        assert_eq!(thousands(1122668), "1,122,668");
    }
}
