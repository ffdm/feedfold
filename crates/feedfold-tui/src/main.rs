use std::collections::{hash_map::DefaultHasher, HashMap, HashSet};
use std::fs;
use std::hash::{Hash, Hasher};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use crossterm::{
    event::{self, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
    Terminal,
};
use viuer::KittySupport;

use feedfold_adapters::{
    RssAdapter, YoutubeAdapter, YOUTUBE_DURATION_KEY, YOUTUBE_LIVE_BROADCAST_KEY,
    YOUTUBE_VIEW_COUNT_KEY,
};
use feedfold_core::adapter::SourceAdapter;
use feedfold_core::config::{AdapterType, Config, RankingMode};
use feedfold_core::storage::{Entry, EntryState, NewEntry, NewSource, Source as DbSource, Storage};
use feedfold_core::VERSION;

mod opml;

const THUMBNAIL_HEIGHT: u16 = 12;

#[derive(Debug, Parser)]
#[command(name = "feedfold", version = VERSION, about = "Terminal RSS reader")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Add a feed by URL: fetch it, parse it, and persist entries.
    Add {
        /// Feed URL (RSS, Atom, or JSON Feed)
        url: String,
        /// Override the display name (defaults to the feed's own title)
        #[arg(long)]
        name: Option<String>,
    },
    /// Bulk-import subscriptions from an OPML file.
    Import {
        /// Path to an OPML file exported from another reader.
        path: PathBuf,
    },
    /// List every source currently tracked in the database.
    List,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ThumbnailMode {
    Viuer,
    TextFallback,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActiveView {
    Home,
    Channels,
    Viewed,
    Overflow,
    Ignored,
}

impl ActiveView {
    fn next(self) -> Self {
        match self {
            Self::Home => Self::Channels,
            Self::Channels => Self::Viewed,
            Self::Viewed => Self::Overflow,
            Self::Overflow => Self::Home,
            Self::Ignored => Self::Home,
        }
    }

    fn previous(self) -> Self {
        match self {
            Self::Home => Self::Overflow,
            Self::Channels => Self::Home,
            Self::Viewed => Self::Channels,
            Self::Overflow => Self::Viewed,
            Self::Ignored => Self::Home,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Overlay {
    None,
    Settings(SettingsState),
    TopNInput(String),
    ChannelsManager(ChannelsManagerState),
    AddChannelInput(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ChannelsManagerState {
    sources: Vec<DbSource>,
    selected: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SettingsState {
    selected: usize,
    top_n: u32,
    ranking_mode: RankingMode,
    poll_interval: u32,
    show_shorts: bool,
    show_live: bool,
    show_premieres: bool,
}

impl SettingsState {
    fn from_config(config: &Config) -> Self {
        Self {
            selected: 0,
            top_n: config.general.default_top_n,
            ranking_mode: config.ranking.mode,
            poll_interval: config.general.poll_interval_mins,
            show_shorts: config.youtube.show_shorts,
            show_live: config.youtube.show_live,
            show_premieres: config.youtube.show_premieres,
        }
    }

    const FIELD_COUNT: usize = 7;
    const VIEW_IGNORED_FIELD: usize = 6;
}

#[derive(Debug, Clone)]
enum ChannelRow {
    Header {
        source_id: i64,
        name: String,
        count: usize,
        expanded: bool,
    },
    Entry(Entry),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ThumbnailStatus {
    Loading,
    Ready(PathBuf),
    Failed(String),
}

#[derive(Debug)]
struct ThumbnailDownload {
    url: String,
    result: Result<PathBuf, String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Some(Command::Add { url, name }) => add_feed(&url, name.as_deref()).await,
        Some(Command::Import { path }) => import_opml(&path).await,
        Some(Command::List) => list_sources(),
        None => run_tui().await,
    }
}

async fn run_tui() -> Result<()> {
    let db_path = Storage::default_path().context("resolving database path")?;
    let mut storage = Storage::open(&db_path)
        .with_context(|| format!("opening database at {}", db_path.display()))?;

    let entries = storage.list_top_n_entries()?;
    let sources = storage.list_sources()?;
    let config = Config::load().unwrap_or_default();
    let thumbnail_dir = db_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("thumbnails");

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new(entries, sources, config, detect_thumbnail_mode(), thumbnail_dir);
    let res = run_app(&mut terminal, &mut app, &mut storage);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    res
}

struct App {
    active_view: ActiveView,
    entries: Vec<Entry>,
    entry_enrichments: HashMap<i64, HashMap<String, String>>,
    channel_rows: Vec<ChannelRow>,
    channels_expanded: HashSet<i64>,
    source_names: HashMap<i64, String>,
    viewed_today_count: usize,
    state: ListState,
    list_viewport_height: u16,
    pending_g: bool,
    search_query: Option<String>,
    search_editing: bool,
    overlay: Overlay,
    config: Config,
    thumbnail_mode: ThumbnailMode,
    thumbnail_dir: PathBuf,
    thumbnail_cache: HashMap<String, ThumbnailStatus>,
    thumbnail_tx: Sender<ThumbnailDownload>,
    thumbnail_rx: Receiver<ThumbnailDownload>,
}

impl App {
    fn new(
        entries: Vec<Entry>,
        sources: Vec<DbSource>,
        config: Config,
        thumbnail_mode: ThumbnailMode,
        thumbnail_dir: PathBuf,
    ) -> App {
        let (thumbnail_tx, thumbnail_rx) = mpsc::channel();
        let mut state = ListState::default();
        if !entries.is_empty() {
            state.select(Some(0));
        }
        let source_names = sources
            .into_iter()
            .map(|s| (s.id, s.name))
            .collect();
        App {
            active_view: ActiveView::Home,
            entries,
            entry_enrichments: HashMap::new(),
            channel_rows: Vec::new(),
            channels_expanded: HashSet::new(),
            source_names,
            viewed_today_count: 0,
            state,
            list_viewport_height: 0,
            pending_g: false,
            search_query: None,
            search_editing: false,
            overlay: Overlay::None,
            config,
            thumbnail_mode,
            thumbnail_dir,
            thumbnail_cache: HashMap::new(),
            thumbnail_tx,
            thumbnail_rx,
        }
    }

    fn current_list_len(&self) -> usize {
        if self.active_view == ActiveView::Channels {
            self.channel_rows.len()
        } else {
            self.entries.len()
        }
    }

    fn next(&mut self) {
        let len = self.current_list_len();
        if len == 0 {
            return;
        }
        let i = match self.state.selected() {
            Some(i) => {
                if i >= len - 1 {
                    0
                } else {
                    i + 1
                }
            }
            None => 0,
        };
        self.state.select(Some(i));
    }

    fn previous(&mut self) {
        let len = self.current_list_len();
        if len == 0 {
            return;
        }
        let i = match self.state.selected() {
            Some(i) => {
                if i == 0 {
                    len - 1
                } else {
                    i - 1
                }
            }
            None => 0,
        };
        self.state.select(Some(i));
    }

    fn jump_first(&mut self) {
        if self.current_list_len() > 0 {
            self.state.select(Some(0));
        }
    }

    fn jump_last(&mut self) {
        let len = self.current_list_len();
        if len > 0 {
            self.state.select(Some(len - 1));
        }
    }

    fn scroll_by(&mut self, delta: isize) {
        let len = self.current_list_len();
        if len == 0 {
            return;
        }
        let current = self.state.selected().unwrap_or(0) as isize;
        let max = (len - 1) as isize;
        let target = (current + delta).clamp(0, max);
        self.state.select(Some(target as usize));
    }

    fn half_page(&self) -> usize {
        (self.list_viewport_height as usize / 2).max(1)
    }

    fn replace_entries(&mut self, entries: Vec<Entry>) {
        self.entries = entries;
        self.rebuild_channel_rows();
        self.sync_selection();
    }

    fn set_view(&mut self, active_view: ActiveView) {
        self.active_view = active_view;
    }

    fn rebuild_channel_rows(&mut self) {
        use std::collections::hash_map::Entry as HEntry;
        let mut order: Vec<i64> = Vec::new();
        let mut groups: HashMap<i64, (String, Vec<Entry>)> = HashMap::new();
        for entry in &self.entries {
            match groups.entry(entry.source_id) {
                HEntry::Vacant(slot) => {
                    order.push(entry.source_id);
                    let name = self
                        .source_names
                        .get(&entry.source_id)
                        .cloned()
                        .or_else(|| entry.author.clone())
                        .unwrap_or_else(|| "Unknown".to_string());
                    slot.insert((name, vec![entry.clone()]));
                }
                HEntry::Occupied(mut slot) => {
                    slot.get_mut().1.push(entry.clone());
                }
            }
        }

        let mut rows = Vec::new();
        for source_id in order {
            let Some((name, entries)) = groups.remove(&source_id) else {
                continue;
            };
            let expanded = self.channels_expanded.contains(&source_id);
            let count = entries.len();
            rows.push(ChannelRow::Header {
                source_id,
                name,
                count,
                expanded,
            });
            if expanded {
                for entry in entries {
                    rows.push(ChannelRow::Entry(entry));
                }
            }
        }
        self.channel_rows = rows;
    }

    fn selected_channel_header_source(&self) -> Option<i64> {
        if self.active_view != ActiveView::Channels {
            return None;
        }
        let i = self.state.selected()?;
        match self.channel_rows.get(i)? {
            ChannelRow::Header { source_id, .. } => Some(*source_id),
            ChannelRow::Entry(_) => None,
        }
    }

    fn toggle_channel(&mut self, source_id: i64) {
        if !self.channels_expanded.remove(&source_id) {
            self.channels_expanded.insert(source_id);
        }
        self.rebuild_channel_rows();
        let new_idx = self.channel_rows.iter().position(
            |row| matches!(row, ChannelRow::Header { source_id: sid, .. } if *sid == source_id),
        );
        if let Some(idx) = new_idx {
            self.state.select(Some(idx));
        } else {
            self.sync_selection();
        }
    }

    fn list_title(&self) -> String {
        if let Some(query) = self.search_query.as_deref() {
            if self.search_editing {
                return format!("Search: {query}_");
            }

            if !query.trim().is_empty() {
                return format!("Search: {query}");
            }
        }

        match self.active_view {
            ActiveView::Home => "Home".to_string(),
            ActiveView::Channels => "Channels".to_string(),
            ActiveView::Viewed => format!("Viewed (today: {})", self.viewed_today_count),
            ActiveView::Overflow => "Overflow".to_string(),
            ActiveView::Ignored => "Ignored".to_string(),
        }
    }

    fn sync_selection(&mut self) {
        let len = self.current_list_len();
        match self.state.selected() {
            Some(_) if len == 0 => self.state.select(None),
            Some(i) if i >= len => self.state.select(Some(len - 1)),
            None if len > 0 => self.state.select(Some(0)),
            _ => {}
        }
    }

    fn selected_entry(&self) -> Option<&Entry> {
        let i = self.state.selected()?;
        if self.active_view == ActiveView::Channels {
            match self.channel_rows.get(i)? {
                ChannelRow::Entry(e) => Some(e),
                ChannelRow::Header { .. } => None,
            }
        } else {
            self.entries.get(i)
        }
    }

    fn selected_entry_mut(&mut self) -> Option<&mut Entry> {
        let i = self.state.selected()?;
        if self.active_view == ActiveView::Channels {
            match self.channel_rows.get_mut(i)? {
                ChannelRow::Entry(e) => Some(e),
                ChannelRow::Header { .. } => None,
            }
        } else {
            self.entries.get_mut(i)
        }
    }

    fn selected_thumbnail_status(&self) -> Option<&ThumbnailStatus> {
        let url = self.selected_entry()?.thumbnail_url.as_ref()?;
        self.thumbnail_cache.get(url)
    }

    fn begin_search(&mut self) {
        self.search_query.get_or_insert_with(String::new);
        self.search_editing = true;
    }

    fn finish_search(&mut self) {
        if self
            .search_query
            .as_deref()
            .is_some_and(|query| query.trim().is_empty())
        {
            self.search_query = None;
        }
        self.search_editing = false;
    }

    fn clear_search(&mut self) {
        self.search_query = None;
        self.search_editing = false;
    }

    fn push_search_char(&mut self, c: char) {
        self.search_query.get_or_insert_with(String::new).push(c);
    }

    fn pop_search_char(&mut self) -> bool {
        self.search_query
            .as_mut()
            .is_some_and(|query| query.pop().is_some())
    }

    fn search_query(&self) -> Option<&str> {
        self.search_query.as_deref()
    }

    fn is_search_active(&self) -> bool {
        self.search_query
            .as_deref()
            .is_some_and(|query| !query.trim().is_empty())
    }

    fn is_search_editing(&self) -> bool {
        self.search_editing
    }

    fn process_thumbnail_updates(&mut self) -> bool {
        let mut updated = false;
        while let Ok(download) = self.thumbnail_rx.try_recv() {
            updated = true;
            match download.result {
                Ok(path) => {
                    self.thumbnail_cache
                        .insert(download.url, ThumbnailStatus::Ready(path));
                }
                Err(error) => {
                    self.thumbnail_cache
                        .insert(download.url, ThumbnailStatus::Failed(error));
                }
            }
        }
        updated
    }

    fn ensure_selected_thumbnail(&mut self) -> bool {
        if self.thumbnail_mode != ThumbnailMode::Viuer {
            return false;
        }

        let Some(url) = self
            .selected_entry()
            .and_then(|entry| entry.thumbnail_url.clone())
        else {
            return false;
        };

        if self.thumbnail_cache.contains_key(&url) {
            return false;
        }

        let path = thumbnail_cache_path(&self.thumbnail_dir, &url);
        self.thumbnail_cache
            .insert(url.clone(), ThumbnailStatus::Loading);
        let tx = self.thumbnail_tx.clone();
        thread::spawn(move || {
            let result = download_thumbnail(&url, &path)
                .map(|_| path)
                .map_err(|error| error.to_string());
            let _ = tx.send(ThumbnailDownload { url, result });
        });
        true
    }

    fn mark_selected_thumbnail_failed(&mut self, error: String) {
        let Some(url) = self
            .selected_entry()
            .and_then(|entry| entry.thumbnail_url.clone())
        else {
            return;
        };

        self.thumbnail_cache
            .insert(url, ThumbnailStatus::Failed(error));
    }
}

fn run_app(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    app: &mut App,
    storage: &mut Storage,
) -> Result<()> {
    let mut needs_redraw = true;

    loop {
        if app.process_thumbnail_updates() {
            needs_redraw = true;
        }

        if app.ensure_selected_thumbnail() {
            needs_redraw = true;
        }

        if needs_redraw {
            if app.thumbnail_mode == ThumbnailMode::Viuer {
                clear_kitty_graphics(terminal.backend_mut())?;
            }
            terminal.draw(|f| ui(f, app))?;
            if app.overlay == Overlay::None {
                needs_redraw = draw_selected_thumbnail(terminal, app)?;
            } else {
                needs_redraw = false;
            }
        }

        if event::poll(Duration::from_millis(50))? {
            if let Event::Key(key) = event::read()? {
                if app.is_search_editing() {
                    match key.code {
                        KeyCode::Esc | KeyCode::Enter => {
                            let had_query = app.search_query().is_some();
                            app.finish_search();
                            if had_query && !app.is_search_active() {
                                refresh_view(app, storage)?;
                            }
                            needs_redraw = true;
                        }
                        KeyCode::Backspace => {
                            if app.pop_search_char() {
                                refresh_view(app, storage)?;
                                needs_redraw = true;
                            }
                        }
                        KeyCode::Char(c) => {
                            app.push_search_char(c);
                            refresh_view(app, storage)?;
                            needs_redraw = true;
                        }
                        _ => {}
                    }
                    continue;
                }

                if let Overlay::Settings(ref mut settings) = app.overlay {
                    match key.code {
                        KeyCode::Esc => {
                            app.overlay = Overlay::None;
                            needs_redraw = true;
                        }
                        KeyCode::Char('j') | KeyCode::Down => {
                            settings.selected =
                                (settings.selected + 1) % SettingsState::FIELD_COUNT;
                            needs_redraw = true;
                        }
                        KeyCode::Char('k') | KeyCode::Up => {
                            settings.selected = if settings.selected == 0 {
                                SettingsState::FIELD_COUNT - 1
                            } else {
                                settings.selected - 1
                            };
                            needs_redraw = true;
                        }
                        KeyCode::Char('l') | KeyCode::Right | KeyCode::Char(' ') => {
                            match settings.selected {
                                0 => settings.top_n = settings.top_n.saturating_add(1).min(50),
                                1 => settings.ranking_mode = settings.ranking_mode.cycle_next(),
                                2 => settings.poll_interval = settings.poll_interval.saturating_add(5).min(1440),
                                3 => settings.show_shorts = !settings.show_shorts,
                                4 => settings.show_live = !settings.show_live,
                                5 => settings.show_premieres = !settings.show_premieres,
                                SettingsState::VIEW_IGNORED_FIELD => {
                                    app.overlay = Overlay::None;
                                    app.clear_search();
                                    app.set_view(ActiveView::Ignored);
                                    refresh_view(app, storage)?;
                                }
                                _ => {}
                            }
                            needs_redraw = true;
                        }
                        KeyCode::Char('h') | KeyCode::Left => {
                            match settings.selected {
                                0 => settings.top_n = settings.top_n.saturating_sub(1).max(1),
                                1 => {
                                    settings.ranking_mode = match settings.ranking_mode {
                                        RankingMode::Recency => RankingMode::Claude,
                                        RankingMode::Popularity => RankingMode::Recency,
                                        RankingMode::Claude => RankingMode::Popularity,
                                    };
                                }
                                2 => settings.poll_interval = settings.poll_interval.saturating_sub(5).max(5),
                                3 => settings.show_shorts = !settings.show_shorts,
                                4 => settings.show_live = !settings.show_live,
                                5 => settings.show_premieres = !settings.show_premieres,
                                _ => {}
                            }
                            needs_redraw = true;
                        }
                        KeyCode::Enter => {
                            if settings.selected == SettingsState::VIEW_IGNORED_FIELD {
                                app.overlay = Overlay::None;
                                app.clear_search();
                                app.set_view(ActiveView::Ignored);
                                refresh_view(app, storage)?;
                            } else {
                                let settings = settings.clone();
                                app.config.general.default_top_n = settings.top_n;
                                app.config.ranking.mode = settings.ranking_mode;
                                app.config.general.poll_interval_mins = settings.poll_interval;
                                app.config.youtube.show_shorts = settings.show_shorts;
                                app.config.youtube.show_live = settings.show_live;
                                app.config.youtube.show_premieres = settings.show_premieres;
                                let _ = app.config.save();
                                app.overlay = Overlay::None;
                                refresh_view(app, storage)?;
                            }
                            needs_redraw = true;
                        }
                        _ => {}
                    }
                    continue;
                }

                if let Overlay::TopNInput(ref mut buf) = app.overlay {
                    match key.code {
                        KeyCode::Esc => {
                            app.overlay = Overlay::None;
                            needs_redraw = true;
                        }
                        KeyCode::Char(c) if c.is_ascii_digit() && buf.len() < 3 => {
                            buf.push(c);
                            needs_redraw = true;
                        }
                        KeyCode::Backspace => {
                            buf.pop();
                            needs_redraw = true;
                        }
                        KeyCode::Enter => {
                            if let Ok(n) = buf.parse::<u32>() {
                                let n = n.clamp(1, 50);
                                app.config.general.default_top_n = n;
                                let _ = app.config.save();
                            }
                            app.overlay = Overlay::None;
                            refresh_view(app, storage)?;
                            needs_redraw = true;
                        }
                        _ => {}
                    }
                    continue;
                }

                if let Overlay::ChannelsManager(ref mut state) = app.overlay {
                    match key.code {
                        KeyCode::Esc | KeyCode::Char('q') => {
                            app.overlay = Overlay::None;
                            needs_redraw = true;
                        }
                        KeyCode::Char('j') | KeyCode::Down => {
                            if !state.sources.is_empty() {
                                state.selected = (state.selected + 1) % state.sources.len();
                                needs_redraw = true;
                            }
                        }
                        KeyCode::Char('k') | KeyCode::Up => {
                            if !state.sources.is_empty() {
                                state.selected = if state.selected == 0 {
                                    state.sources.len() - 1
                                } else {
                                    state.selected - 1
                                };
                                needs_redraw = true;
                            }
                        }
                        KeyCode::Char('d') => {
                            if !state.sources.is_empty() {
                                let id = state.sources[state.selected].id;
                                if storage.delete_source(id).is_ok() {
                                    if let Ok(sources) = storage.list_sources() {
                                        app.source_names = sources.iter().map(|s| (s.id, s.name.clone())).collect();
                                        state.sources = sources;
                                        if state.selected >= state.sources.len() {
                                            state.selected = state.sources.len().saturating_sub(1);
                                        }
                                    }
                                    let _ = refresh_view(app, storage);
                                    needs_redraw = true;
                                }
                            }
                        }
                        KeyCode::Char('a') => {
                            app.overlay = Overlay::AddChannelInput(String::new());
                            needs_redraw = true;
                        }
                        _ => {}
                    }
                    continue;
                }

                if let Overlay::AddChannelInput(ref mut buf) = app.overlay {
                    match key.code {
                        KeyCode::Esc => {
                            if let Ok(sources) = storage.list_sources() {
                                app.overlay = Overlay::ChannelsManager(ChannelsManagerState {
                                    sources,
                                    selected: 0,
                                });
                            } else {
                                app.overlay = Overlay::None;
                            }
                            needs_redraw = true;
                        }
                        KeyCode::Char(c) if c.is_ascii() && !c.is_control() => {
                            buf.push(c);
                            needs_redraw = true;
                        }
                        KeyCode::Backspace => {
                            buf.pop();
                            needs_redraw = true;
                        }
                        KeyCode::Enter => {
                            let url = buf.trim().to_string();
                            if !url.is_empty() {
                                let new_source = feedfold_core::storage::NewSource {
                                    name: url.clone(),
                                    url: url.clone(),
                                    adapter: adapter_for_url(&url),
                                    top_n_override: None,
                                };
                                let _ = storage.insert_source(&new_source);
                            }
                            if let Ok(sources) = storage.list_sources() {
                                app.source_names = sources.iter().map(|s| (s.id, s.name.clone())).collect();
                                app.overlay = Overlay::ChannelsManager(ChannelsManagerState {
                                    sources,
                                    selected: 0,
                                });
                            } else {
                                app.overlay = Overlay::None;
                            }
                            let _ = refresh_view(app, storage);
                            needs_redraw = true;
                        }
                        _ => {}
                    }
                    continue;
                }

                if key.modifiers.contains(KeyModifiers::CONTROL) {
                    match key.code {
                        KeyCode::Char('u') => {
                            let delta = app.half_page() as isize;
                            app.scroll_by(-delta);
                            app.pending_g = false;
                            needs_redraw = true;
                            continue;
                        }
                        KeyCode::Char('d') => {
                            let delta = app.half_page() as isize;
                            app.scroll_by(delta);
                            app.pending_g = false;
                            needs_redraw = true;
                            continue;
                        }
                        _ => {}
                    }
                }

                if let KeyCode::Char('g') = key.code {
                    if app.pending_g {
                        app.jump_first();
                        app.pending_g = false;
                    } else {
                        app.pending_g = true;
                    }
                    needs_redraw = true;
                    continue;
                }
                if let KeyCode::Char('G') = key.code {
                    app.jump_last();
                    app.pending_g = false;
                    needs_redraw = true;
                    continue;
                }
                app.pending_g = false;

                match key.code {
                    KeyCode::Char('q') => return Ok(()),
                    KeyCode::Char('/') => {
                        app.begin_search();
                        needs_redraw = true;
                    }
                    KeyCode::Esc => {
                        if app.is_search_active() {
                            app.clear_search();
                            refresh_view(app, storage)?;
                            needs_redraw = true;
                        }
                    }
                    KeyCode::Tab => {
                        let next = app.active_view.next();
                        app.clear_search();
                        app.set_view(next);
                        refresh_view(app, storage)?;
                        needs_redraw = true;
                    }
                    KeyCode::BackTab => {
                        let prev = app.active_view.previous();
                        app.clear_search();
                        app.set_view(prev);
                        refresh_view(app, storage)?;
                        needs_redraw = true;
                    }
                    KeyCode::Char('S') => {
                        app.overlay = Overlay::Settings(SettingsState::from_config(&app.config));
                        needs_redraw = true;
                    }
                    KeyCode::Char('C') => {
                        if let Ok(sources) = storage.list_sources() {
                            app.overlay = Overlay::ChannelsManager(ChannelsManagerState {
                                sources,
                                selected: 0,
                            });
                            needs_redraw = true;
                        }
                    }
                    KeyCode::Char('n') => {
                        app.overlay = Overlay::TopNInput(
                            app.config.general.default_top_n.to_string(),
                        );
                        needs_redraw = true;
                    }
                    KeyCode::Char('r') => {
                        let sources = storage.list_sources()?;

                        for source in &sources {
                            let top_n = source.top_n_override.unwrap_or(app.config.general.default_top_n) as usize;
                            let mode = app.config.sources.iter().find(|s| s.url == source.url).and_then(|s| s.ranking).unwrap_or(app.config.ranking.mode);
                            
                            if let Ok(entries) = storage.list_entries_for_source(source.id) {
                                if let Ok(enrichments) = storage.list_enrichments_for_source(source.id) {
                                    let ctx = feedfold_core::ranker::RankContext {
                                        top_n,
                                        enrichments,
                                    };
                                    let scores = match mode {
                                        RankingMode::Recency => feedfold_core::ranker::Ranker::rank(&feedfold_core::ranker::RecencyRanker, &entries, &ctx),
                                        RankingMode::Popularity => feedfold_core::ranker::Ranker::rank(&feedfold_core::ranker::PopularityRanker, &entries, &ctx),
                                        RankingMode::Claude => feedfold_core::ranker::Ranker::rank(&feedfold_core::ranker::RecencyRanker, &entries, &ctx),
                                    };
                                    let _ = storage.apply_ranking(source.id, &scores, top_n);
                                }
                            }
                        }

                        app.source_names = sources.into_iter().map(|s| (s.id, s.name)).collect();
                        refresh_view(app, storage)?;
                        needs_redraw = true;
                    }
                    KeyCode::Char('j') | KeyCode::Down => {
                        app.next();
                        needs_redraw = true;
                    }
                    KeyCode::Char('k') | KeyCode::Up => {
                        app.previous();
                        needs_redraw = true;
                    }
                    KeyCode::Char(c @ '1'..='5') => {
                        let rating = c.to_digit(10).expect("digit key") as u8;
                        if let Some(entry) = app.selected_entry_mut() {
                            storage.set_entry_rating(entry.id, rating)?;
                            entry.rating = Some(rating);
                            needs_redraw = true;
                        }
                    }
                    KeyCode::Char('s') => {
                        if toggle_star_for_selected(app, storage)? {
                            needs_redraw = true;
                        }
                    }
                    KeyCode::Char('i') => {
                        if let Some(entry) = app.selected_entry() {
                            let id = entry.id;
                            let was_new = entry.state == EntryState::New;
                            if was_new {
                                storage.set_entry_state(id, EntryState::Ignored)?;
                                if let Some(entry) = app.selected_entry_mut() {
                                    entry.state = EntryState::Ignored;
                                }
                                if !app.is_search_active()
                                    && matches!(
                                        app.active_view,
                                        ActiveView::Home | ActiveView::Channels | ActiveView::Overflow
                                    )
                                {
                                    refresh_view(app, storage)?;
                                }
                                needs_redraw = true;
                            }
                        }
                    }
                    KeyCode::Char('v') => {
                        if mark_selected_viewed(app, storage)? {
                            needs_redraw = true;
                        }
                    }
                    KeyCode::Enter => {
                        if let Some(source_id) = app.selected_channel_header_source() {
                            app.toggle_channel(source_id);
                            needs_redraw = true;
                        } else if let Some((entry_id, url, was_new)) =
                            app.selected_entry().map(|entry| {
                                (entry.id, entry.url.clone(), entry.state == EntryState::New)
                            })
                        {
                            let _ = open::that(&url);
                            storage.record_entry_view(entry_id)?;
                            app.viewed_today_count = storage.count_entries_viewed_today()?;

                            if let Some(entry) = app.selected_entry_mut() {
                                if was_new {
                                    entry.state = EntryState::Viewed;
                                }
                            }

                            if !app.is_search_active()
                                && matches!(
                                    app.active_view,
                                    ActiveView::Viewed | ActiveView::Overflow
                                )
                            {
                                refresh_view(app, storage)?;
                            }

                            needs_redraw = true;
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

fn load_entries_for_view(
    storage: &Storage,
    active_view: ActiveView,
    search_query: Option<&str>,
) -> Result<Vec<Entry>> {
    if let Some(query) = search_query.filter(|query| !query.trim().is_empty()) {
        return Ok(storage.search_entries(query)?);
    }

    match active_view {
        ActiveView::Home => Ok(storage.list_top_n_entries()?),
        ActiveView::Channels => {
            let mut entries = storage.list_top_n_entries()?;
            entries.sort_by(|a, b| {
                a.author
                    .as_deref()
                    .unwrap_or("")
                    .cmp(b.author.as_deref().unwrap_or(""))
                    .then(b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal))
            });
            Ok(entries)
        }
        ActiveView::Viewed => Ok(storage.list_viewed_entries()?),
        ActiveView::Overflow => Ok(storage.list_overflow_entries()?),
        ActiveView::Ignored => Ok(storage.list_ignored_entries()?),
    }
}

fn mark_selected_viewed(app: &mut App, storage: &mut Storage) -> Result<bool> {
    let Some((entry_id, was_transitionable)) = app.selected_entry().map(|entry| {
        (
            entry.id,
            matches!(entry.state, EntryState::New | EntryState::Ignored),
        )
    }) else {
        return Ok(false);
    };

    if !was_transitionable {
        return Ok(false);
    }

    storage.record_entry_view(entry_id)?;
    app.viewed_today_count = storage.count_entries_viewed_today()?;

    if let Some(entry) = app.selected_entry_mut() {
        entry.state = EntryState::Viewed;
    }

    if !app.is_search_active()
        && matches!(
            app.active_view,
            ActiveView::Home | ActiveView::Channels | ActiveView::Overflow | ActiveView::Ignored
        )
    {
        refresh_view(app, storage)?;
    }

    Ok(true)
}

fn toggle_star_for_selected(app: &mut App, storage: &mut Storage) -> Result<bool> {
    let Some((entry_id, next_state)) = app.selected_entry().map(|entry| {
        let next_state = if entry.state == EntryState::Starred {
            EntryState::Viewed
        } else {
            EntryState::Starred
        };
        (entry.id, next_state)
    }) else {
        return Ok(false);
    };

    storage.set_entry_state(entry_id, next_state)?;

    if app.active_view == ActiveView::Overflow && !app.is_search_active() {
        refresh_view(app, storage)?;
    } else if let Some(entry) = app.selected_entry_mut() {
        entry.state = next_state;
    }

    Ok(true)
}

fn refresh_view(app: &mut App, storage: &Storage) -> Result<()> {
    let mut entries = load_entries_for_view(storage, app.active_view, app.search_query())?;
    filter_youtube_content(storage, &mut entries, &app.config)?;
    let ids: Vec<i64> = entries.iter().map(|e| e.id).collect();
    app.entry_enrichments = storage.get_enrichments_for_entries(&ids)?;
    app.replace_entries(entries);
    if app.active_view == ActiveView::Viewed {
        app.viewed_today_count = storage.count_entries_viewed_today()?;
    }
    Ok(())
}

fn parse_iso8601_duration_seconds(duration: &str) -> Option<u64> {
    let s = duration.strip_prefix("PT")?;
    let mut total = 0u64;
    let mut num_buf = String::new();
    for ch in s.chars() {
        if ch.is_ascii_digit() {
            num_buf.push(ch);
        } else {
            let n: u64 = num_buf.parse().ok()?;
            num_buf.clear();
            match ch {
                'H' => total += n * 3600,
                'M' => total += n * 60,
                'S' => total += n,
                _ => return None,
            }
        }
    }
    Some(total)
}

fn filter_youtube_content(
    storage: &Storage,
    entries: &mut Vec<Entry>,
    config: &Config,
) -> Result<()> {
    if config.youtube.show_shorts && config.youtube.show_live && config.youtube.show_premieres {
        return Ok(());
    }

    let entry_ids: Vec<i64> = entries.iter().map(|e| e.id).collect();
    let enrichments = storage.get_enrichments_for_entries(&entry_ids)?;

    entries.retain(|entry| {
        let Some(entry_enrichments) = enrichments.get(&entry.id) else {
            return true;
        };

        if !config.youtube.show_shorts {
            if let Some(duration) = entry_enrichments.get(YOUTUBE_DURATION_KEY) {
                if let Some(seconds) = parse_iso8601_duration_seconds(duration) {
                    if seconds <= 60 {
                        return false;
                    }
                }
            }
        }

        if let Some(broadcast) = entry_enrichments.get(YOUTUBE_LIVE_BROADCAST_KEY) {
            let broadcast = broadcast.to_lowercase();
            if !config.youtube.show_live && broadcast == "live" {
                return false;
            }
            if !config.youtube.show_premieres && broadcast == "upcoming" {
                return false;
            }
        }

        true
    });

    Ok(())
}

fn safe_truncate(s: &str, max_width: usize) -> String {
    use unicode_width::UnicodeWidthChar;
    let mut current_width = 0;
    let mut result = String::new();
    for c in s.chars() {
        let u = c as u32;
        // Strip emojis and variation selectors that cause terminal rendering bugs
        if (u >= 0x2600 && u <= 0x27BF) || (u >= 0x1F000 && u <= 0x1FAFF) || u == 0xFE0F || u == 0xFE0E || u == 0x200D {
            continue;
        }

        let w = c.width().unwrap_or(0);
        if current_width + w > max_width {
            result.push('…');
            break;
        }
        result.push(c);
        current_width += w;
    }
    result
}

fn ui(f: &mut ratatui::Frame, app: &mut App) {
    let outer = f.area();
    let main_and_bar = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(outer);
    let main_area = main_and_bar[0];
    let bar_area = main_and_bar[1];

    let (list_area, detail_area) = main_sections(main_area);

    let items: Vec<ListItem> = if app.active_view == ActiveView::Channels {
        app.channel_rows
            .iter()
            .map(|row| match row {
                ChannelRow::Header {
                    name,
                    count,
                    expanded,
                    ..
                } => {
                    let caret = if *expanded { "\u{25be}" } else { "\u{25b8}" };
                    let line = Line::from(vec![
                        Span::styled(
                            format!("{caret} {name}"),
                            Style::default()
                                .fg(Color::Cyan)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            format!("  ({count})"),
                            Style::default().fg(Color::DarkGray),
                        ),
                    ]);
                    ListItem::new(line)
                }
                ChannelRow::Entry(entry) => {
                    let title_style = if entry.state == EntryState::New {
                        Style::default()
                            .fg(Color::White)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(Color::Gray)
                    };
                    let star = if entry.state == EntryState::Starred {
                        Span::styled("* ", Style::default().fg(Color::Yellow))
                    } else {
                        Span::raw("  ")
                    };
                    
                    let max_title_width = list_area.width.saturating_sub(8) as usize;
                    let truncated_title = safe_truncate(&entry.title, max_title_width);

                    let line = Line::from(vec![
                        Span::raw("    "),
                        star,
                        Span::styled(truncated_title, title_style),
                    ]);
                    ListItem::new(line)
                }
            })
            .collect()
    } else {
        app.entries
            .iter()
            .map(|entry| {
                let source = entry
                    .author
                    .clone()
                    .unwrap_or_else(|| "Unknown".to_string());
                let title_style = if entry.state == EntryState::New {
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::Gray)
                };
                let star = if entry.state == EntryState::Starred {
                    Span::styled("* ", Style::default().fg(Color::Yellow))
                } else {
                    Span::raw("  ")
                };
                
                use unicode_width::UnicodeWidthStr;
                let source_str = format!("{source}  ");
                let source_width = source_str.width();
                let max_title_width = list_area.width.saturating_sub(6 + source_width as u16) as usize;
                let truncated_title = safe_truncate(&entry.title, max_title_width);

                let line = Line::from(vec![
                    star,
                    Span::styled(source_str, Style::default().fg(Color::Cyan)),
                    Span::styled(truncated_title, title_style),
                ]);
                ListItem::new(line)
            })
            .collect()
    };

    let list_title = Line::from(vec![
        Span::raw(" "),
        Span::styled(
            app.list_title(),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
    ]);

    let items_list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(list_title))
        .highlight_style(Style::default().bg(Color::DarkGray))
        .highlight_symbol(" > ");

    app.list_viewport_height = list_area.height.saturating_sub(2);
    f.render_stateful_widget(items_list, list_area, &mut app.state);

    let detail_title = Line::from(vec![
        Span::raw(" "),
        Span::styled("Detail", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" "),
    ]);
    let detail_block = Block::default()
        .borders(Borders::ALL)
        .title(detail_title);
    let detail_inner = detail_block.inner(detail_area);
    f.render_widget(detail_block, detail_area);

    let show_thumbnail_area = app.thumbnail_mode == ThumbnailMode::Viuer
        && app
            .selected_entry()
            .and_then(|e| e.thumbnail_url.as_ref())
            .is_some();

    if show_thumbnail_area {
        let (thumbnail_area, summary_area) = detail_sections(detail_inner);
        if thumbnail_area.width > 0 && thumbnail_area.height > 0 {
            let thumbnail_text = build_thumbnail_status_text(
                app.selected_entry(),
                app.selected_thumbnail_status(),
            );
            if !thumbnail_text.is_empty() {
                f.render_widget(
                    Paragraph::new(thumbnail_text)
                        .style(Style::default().fg(Color::DarkGray))
                        .wrap(Wrap { trim: true }),
                    thumbnail_area,
                );
            }
        }
        if summary_area.width > 0 && summary_area.height > 0 {
            let selected = app.selected_entry();
            let enrichments = selected.and_then(|e| app.entry_enrichments.get(&e.id));
            let lines = build_detail_lines(selected, enrichments);
            f.render_widget(
                Paragraph::new(lines).wrap(Wrap { trim: true }),
                summary_area,
            );
        }
    } else if detail_inner.width > 0 && detail_inner.height > 0 {
        let selected = app.selected_entry();
        let enrichments = selected.and_then(|e| app.entry_enrichments.get(&e.id));
        let lines = build_detail_lines(selected, enrichments);
        f.render_widget(
            Paragraph::new(lines).wrap(Wrap { trim: true }),
            detail_inner,
        );
    }

    let key = Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD);
    let dim = Style::default().fg(Color::DarkGray);
    let sep = Span::styled("\u{2502} ", dim);
    let bar = Line::from(vec![
        Span::styled(" Tab", key),
        Span::styled(" views  ", dim),
        sep.clone(),
        Span::styled("j/k", key),
        Span::styled(" move ", dim),
        Span::styled("^d/^u", key),
        Span::styled(" half-page ", dim),
        Span::styled("gg/G", key),
        Span::styled(" top/end ", dim),
        sep.clone(),
        Span::styled("v", key),
        Span::styled("iew ", dim),
        Span::styled("i", key),
        Span::styled("gnore ", dim),
        Span::styled("s", key),
        Span::styled("tar ", dim),
        Span::styled("r", key),
        Span::styled("eload ", dim),
        sep,
        Span::styled("n", key),
        Span::styled("=top_n ", dim),
        Span::styled("S", key),
        Span::styled("ettings ", dim),
        Span::styled("C", key),
        Span::styled("hannels ", dim),
        Span::styled("/", key),
        Span::styled("search ", dim),
        Span::styled("q", key),
        Span::styled("uit", dim),
    ]);
    f.render_widget(Paragraph::new(bar), bar_area);

    match &app.overlay {
        Overlay::None => {}
        Overlay::Settings(settings) => draw_settings_overlay(f, settings, outer),
        Overlay::TopNInput(buf) => draw_top_n_overlay(f, buf, outer),
        Overlay::ChannelsManager(state) => draw_channels_manager_overlay(f, state, outer),
        Overlay::AddChannelInput(buf) => draw_add_channel_overlay(f, buf, outer),
    }
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height) / 2;
    Rect::new(x, y, width.min(area.width), height.min(area.height))
}

fn bool_display(v: bool) -> &'static str {
    if v { "yes" } else { "no" }
}

fn draw_settings_overlay(f: &mut ratatui::Frame, settings: &SettingsState, area: Rect) {
    let popup = centered_rect(54, 16, area);
    f.render_widget(ratatui::widgets::Clear, popup);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(Line::from(vec![
            Span::raw(" "),
            Span::styled("Settings", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(" "),
        ]))
        .style(Style::default().bg(Color::Black));
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    let fields: Vec<(&str, String)> = vec![
        ("Top N", format!("\u{25c2} {} \u{25b8}", settings.top_n)),
        ("Ranking", format!("\u{25c2} {} \u{25b8}", settings.ranking_mode)),
        ("Poll (min)", format!("\u{25c2} {} \u{25b8}", settings.poll_interval)),
        ("Show Shorts", format!("[{}]", bool_display(settings.show_shorts))),
        ("Show Live", format!("[{}]", bool_display(settings.show_live))),
        ("Show Premieres", format!("[{}]", bool_display(settings.show_premieres))),
        ("View Ignored", "\u{21b2} open".to_string()),
    ];

    let mut constraints: Vec<Constraint> = fields.iter().map(|_| Constraint::Length(1)).collect();
    constraints.push(Constraint::Length(1)); // separator
    constraints.push(Constraint::Length(1)); // hint line 1
    constraints.push(Constraint::Length(1)); // hint line 2
    constraints.push(Constraint::Min(0));
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(inner);

    for (i, (label, value)) in fields.iter().enumerate() {
        let selected = i == settings.selected;
        let label_style = if selected {
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };
        let value_style = if selected {
            Style::default().fg(Color::White).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let indicator = if selected { "\u{25b8} " } else { "  " };
        let line = Line::from(vec![
            Span::styled(indicator, label_style),
            Span::styled(format!("{label:<16}"), label_style),
            Span::styled(value.clone(), value_style),
        ]);
        f.render_widget(Paragraph::new(line), rows[i]);
    }

    let dim = Style::default().fg(Color::DarkGray);
    let hint1 = Line::from(Span::styled(
        "  j/k move   h/l change   Space toggle",
        dim,
    ));
    let hint2 = Line::from(Span::styled(
        "  Enter save/open   Esc cancel",
        dim,
    ));
    f.render_widget(Paragraph::new(hint1), rows[fields.len() + 1]);
    f.render_widget(Paragraph::new(hint2), rows[fields.len() + 2]);
}

fn draw_top_n_overlay(f: &mut ratatui::Frame, buf: &str, area: Rect) {
    let popup = centered_rect(30, 5, area);
    f.render_widget(ratatui::widgets::Clear, popup);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(Line::from(vec![
            Span::raw(" "),
            Span::styled("Set Top N", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(" "),
        ]))
        .style(Style::default().bg(Color::Black));
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Length(1), Constraint::Min(0)])
        .split(inner);

    let input = Line::from(vec![
        Span::styled("  N = ", Style::default().fg(Color::Gray)),
        Span::styled(
            format!("{buf}_"),
            Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
        ),
    ]);
    f.render_widget(Paragraph::new(input), rows[0]);

    let hint = Line::from(Span::styled(
        "  Enter: save  Esc: cancel",
        Style::default().fg(Color::DarkGray),
    ));
    f.render_widget(Paragraph::new(hint), rows[1]);
}

fn draw_channels_manager_overlay(f: &mut ratatui::Frame, state: &ChannelsManagerState, area: Rect) {
    let popup = centered_rect(60, 20, area);
    f.render_widget(ratatui::widgets::Clear, popup);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(Line::from(vec![
            Span::raw(" "),
            Span::styled("Channels", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(" "),
        ]))
        .style(Style::default().bg(Color::Black));
    
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    let items: Vec<ListItem> = state.sources.iter().map(|s| {
        ListItem::new(s.name.clone())
    }).collect();

    let mut list_state = ListState::default();
    list_state.select(Some(state.selected));

    let list = List::new(items)
        .highlight_style(Style::default().bg(Color::DarkGray))
        .highlight_symbol(" > ");

    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(inner);

    f.render_stateful_widget(list, sections[0], &mut list_state);

    let help = Span::styled("  a: add  d: delete  esc: close", Style::default().fg(Color::DarkGray));
    f.render_widget(Paragraph::new(help), sections[1]);
}

fn draw_add_channel_overlay(f: &mut ratatui::Frame, buf: &str, area: Rect) {
    let popup = centered_rect(60, 5, area);
    f.render_widget(ratatui::widgets::Clear, popup);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(Line::from(vec![
            Span::raw(" "),
            Span::styled("Add Channel URL", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(" "),
        ]))
        .style(Style::default().bg(Color::Black));
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Length(1)])
        .split(inner);

    let input = Line::from(vec![
        Span::styled(" URL: ", Style::default().fg(Color::Gray)),
        Span::styled(
            format!("{buf}_"),
            Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
        ),
    ]);

    f.render_widget(Paragraph::new(input), rows[0]);
    f.render_widget(
        Paragraph::new("  Enter: add  Esc: cancel")
            .style(Style::default().fg(Color::DarkGray)),
        rows[1],
    );
}

fn main_sections(area: Rect) -> (Rect, Rect) {
    let sections = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);
    (sections[0], sections[1])
}

fn detail_sections(inner: Rect) -> (Rect, Rect) {
    let thumbnail_height = inner.height.min(THUMBNAIL_HEIGHT);
    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(thumbnail_height), Constraint::Min(0)])
        .split(inner);
    (sections[0], sections[1])
}

fn build_thumbnail_status_text(
    entry: Option<&Entry>,
    status: Option<&ThumbnailStatus>,
) -> String {
    let Some(_entry) = entry else {
        return String::new();
    };

    match status {
        Some(ThumbnailStatus::Ready(_)) => String::new(),
        Some(ThumbnailStatus::Loading) | None => "Loading thumbnail...".to_string(),
        Some(ThumbnailStatus::Failed(_)) => String::new(),
    }
}

fn format_compact_count(n: u64) -> String {
    if n < 1_000 {
        n.to_string()
    } else if n < 1_000_000 {
        let tenths = n / 100;
        if tenths % 10 == 0 || tenths >= 100 {
            format!("{}K", tenths / 10)
        } else {
            format!("{}.{}K", tenths / 10, tenths % 10)
        }
    } else if n < 1_000_000_000 {
        let tenths = n / 100_000;
        if tenths % 10 == 0 || tenths >= 100 {
            format!("{}M", tenths / 10)
        } else {
            format!("{}.{}M", tenths / 10, tenths % 10)
        }
    } else {
        let tenths = n / 100_000_000;
        if tenths % 10 == 0 {
            format!("{}B", tenths / 10)
        } else {
            format!("{}.{}B", tenths / 10, tenths % 10)
        }
    }
}

fn build_detail_lines(
    entry: Option<&Entry>,
    enrichments: Option<&HashMap<String, String>>,
) -> Vec<Line<'static>> {
    let Some(entry) = entry else {
        return vec![];
    };

    let dim = Style::default().fg(Color::DarkGray);
    let label = Style::default().fg(Color::Gray);
    let accent = Style::default().fg(Color::Cyan);

    let mut lines: Vec<Line<'static>> = Vec::new();

    lines.push(Line::from(Span::styled(
        entry.title.clone(),
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    )));

    if let Some(author) = &entry.author {
        lines.push(Line::from(vec![
            Span::styled("by ", label),
            Span::styled(author.clone(), accent),
        ]));
    }

    let mut meta_spans: Vec<Span> = Vec::new();
    if let Some(date) = entry.published_at {
        meta_spans.push(Span::styled(
            date.format("%b %d, %Y").to_string(),
            label,
        ));
    }

    let views = enrichments
        .and_then(|e| e.get(YOUTUBE_VIEW_COUNT_KEY))
        .and_then(|v| v.parse::<u64>().ok());
    if let Some(views) = views {
        if !meta_spans.is_empty() {
            meta_spans.push(Span::styled("  \u{00b7}  ", dim));
        }
        meta_spans.push(Span::styled(
            format!("{} views", format_compact_count(views)),
            label,
        ));
    }

    let state_str = match entry.state {
        EntryState::New => "new",
        EntryState::Viewed => "viewed",
        EntryState::Ignored => "ignored",
        EntryState::Starred => "starred",
    };
    if !meta_spans.is_empty() {
        meta_spans.push(Span::styled("  \u{00b7}  ", dim));
    }
    meta_spans.push(Span::styled(state_str.to_string(), label));
    if let Some(rating) = entry.rating {
        meta_spans.push(Span::styled("  \u{00b7}  ", dim));
        let stars = "\u{2605}".repeat(rating as usize);
        meta_spans.push(Span::styled(
            stars,
            Style::default().fg(Color::Yellow),
        ));
    }
    lines.push(Line::from(meta_spans));

    lines.push(Line::from(Span::styled(
        entry.url.clone(),
        Style::default().fg(Color::Blue),
    )));

    lines.push(Line::from(""));

    let summary = entry
        .summary
        .as_deref()
        .map(strip_html)
        .unwrap_or_else(|| "No summary available.".to_string());
    for line in summary.lines() {
        lines.push(Line::from(Span::styled(
            line.to_string(),
            Style::default().fg(Color::Gray),
        )));
    }

    lines
}

fn strip_html(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    let mut tag_buf = String::new();

    for ch in html.chars() {
        if ch == '<' {
            in_tag = true;
            tag_buf.clear();
            continue;
        }
        if in_tag {
            if ch == '>' {
                in_tag = false;
                let tag = tag_buf.trim().to_lowercase();
                let tag_name = tag
                    .split(|c: char| c.is_whitespace() || c == '/')
                    .next()
                    .unwrap_or("");
                match tag_name {
                    "br" | "p" | "/p" | "div" | "/div" | "hr" | "/hr" | "tr" | "/tr"
                    | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "/h1" | "/h2" | "/h3"
                    | "/h4" | "/h5" | "/h6" => {
                        if !out.ends_with('\n') {
                            out.push('\n');
                        }
                    }
                    "li" => {
                        if !out.ends_with('\n') {
                            out.push('\n');
                        }
                        out.push_str("  - ");
                    }
                    _ => {}
                }
            } else {
                tag_buf.push(ch);
            }
            continue;
        }
        out.push(ch);
    }

    decode_html_entities(&out)
        .lines()
        .map(|line| line.trim_end())
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

fn decode_html_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '&' {
            out.push(ch);
            continue;
        }
        let mut entity = String::new();
        for ech in chars.by_ref() {
            if ech == ';' {
                break;
            }
            entity.push(ech);
            if entity.len() > 10 {
                break;
            }
        }
        match entity.as_str() {
            "amp" => out.push('&'),
            "lt" => out.push('<'),
            "gt" => out.push('>'),
            "quot" => out.push('"'),
            "apos" => out.push('\''),
            "nbsp" => out.push(' '),
            "mdash" => out.push('\u{2014}'),
            "ndash" => out.push('\u{2013}'),
            "hellip" => out.push_str("..."),
            "lsquo" | "rsquo" => out.push('\''),
            "ldquo" | "rdquo" => out.push('"'),
            s if s.starts_with('#') => {
                let code = if s.starts_with("#x") || s.starts_with("#X") {
                    u32::from_str_radix(&s[2..], 16).ok()
                } else {
                    s[1..].parse::<u32>().ok()
                };
                match code.and_then(char::from_u32) {
                    Some(c) => out.push(c),
                    None => {
                        out.push('&');
                        out.push_str(&entity);
                        out.push(';');
                    }
                }
            }
            _ => {
                out.push('&');
                out.push_str(&entity);
                out.push(';');
            }
        }
    }
    out
}

fn draw_selected_thumbnail(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    app: &mut App,
) -> Result<bool> {
    if app.thumbnail_mode != ThumbnailMode::Viuer {
        return Ok(false);
    }

    let Some(path) = app
        .selected_thumbnail_status()
        .and_then(|status| match status {
            ThumbnailStatus::Ready(path) => Some(path.clone()),
            ThumbnailStatus::Loading | ThumbnailStatus::Failed(_) => None,
        })
    else {
        return Ok(false);
    };

    let (_, detail_area) = main_sections(terminal.size()?.into());
    let detail_inner = Block::default()
        .borders(Borders::ALL)
        .title("Detail")
        .inner(detail_area);
    let (thumbnail_area, _) = detail_sections(detail_inner);
    if thumbnail_area.width == 0 || thumbnail_area.height == 0 {
        return Ok(false);
    }

    let config = viuer::Config {
        x: thumbnail_area.x,
        y: thumbnail_area.y as i16,
        width: Some(u32::from(thumbnail_area.width)),
        height: Some(u32::from(thumbnail_area.height)),
        restore_cursor: true,
        transparent: true,
        ..Default::default()
    };

    if let Err(error) = viuer::print_from_file(&path, &config) {
        app.mark_selected_thumbnail_failed(error.to_string());
        return Ok(true);
    }

    Ok(false)
}

fn clear_kitty_graphics(stdout: &mut CrosstermBackend<std::io::Stdout>) -> Result<()> {
    use std::io::Write;
    // Kitty graphics protocol: a=d deletes all image placements
    stdout.write_all(b"\x1b_Ga=d\x1b\\")?;
    stdout.flush()?;
    Ok(())
}

fn detect_thumbnail_mode() -> ThumbnailMode {
    if viuer::get_kitty_support() != KittySupport::None || viuer::is_iterm_supported() {
        ThumbnailMode::Viuer
    } else {
        ThumbnailMode::TextFallback
    }
}

fn thumbnail_cache_path(cache_dir: &Path, url: &str) -> PathBuf {
    let mut hasher = DefaultHasher::new();
    url.hash(&mut hasher);
    cache_dir.join(format!("{:016x}.img", hasher.finish()))
}

fn download_thumbnail(url: &str, path: &Path) -> Result<()> {
    let parent = path
        .parent()
        .context("thumbnail cache path is missing a parent directory")?;
    fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;

    if path.exists() {
        return Ok(());
    }

    let response =
        reqwest::blocking::get(url).with_context(|| format!("requesting thumbnail at {url}"))?;
    let response = response
        .error_for_status()
        .with_context(|| format!("thumbnail request failed for {url}"))?;
    let bytes = response
        .bytes()
        .with_context(|| format!("reading thumbnail bytes from {url}"))?;

    let temp_path = path.with_extension("part");
    fs::write(&temp_path, &bytes).with_context(|| format!("writing {}", temp_path.display()))?;
    fs::rename(&temp_path, path).with_context(|| format!("persisting {}", path.display()))?;
    Ok(())
}


async fn add_feed(url: &str, override_name: Option<&str>) -> Result<()> {
    let db_path = Storage::default_path().context("resolving database path")?;
    let mut storage = Storage::open(&db_path)
        .with_context(|| format!("opening database at {}", db_path.display()))?;
    let outcome = add_feed_with_storage(&mut storage, url, override_name).await?;
    print_import_outcome(&outcome);
    Ok(())
}

fn adapter_for_url(url: &str) -> AdapterType {
    if opml::looks_like_youtube_feed(url) {
        AdapterType::Youtube
    } else {
        AdapterType::Rss
    }
}

fn youtube_adapter() -> YoutubeAdapter {
    let key = Config::load().ok().and_then(|c| c.youtube_api_key());
    match key {
        Some(key) => YoutubeAdapter::with_api_key(key),
        None => YoutubeAdapter::new(),
    }
}

async fn fetch_feed_for(kind: AdapterType, url: &str) -> Result<feedfold_core::adapter::FetchedFeed> {
    match kind {
        AdapterType::Rss => RssAdapter::new()
            .fetch(url)
            .await
            .with_context(|| format!("fetching feed at {url}")),
        AdapterType::Youtube => youtube_adapter()
            .fetch(url)
            .await
            .with_context(|| format!("fetching YouTube feed at {url}")),
    }
}

struct ImportOutcome {
    source_name: String,
    already_tracked: bool,
    new_entries: usize,
    total_entries: usize,
}

async fn add_feed_with_storage(
    storage: &mut Storage,
    url: &str,
    override_name: Option<&str>,
) -> Result<ImportOutcome> {
    let kind = adapter_for_url(url);
    let fetched = fetch_feed_for(kind, url).await?;

    let name = override_name
        .map(str::to_owned)
        .or_else(|| fetched.name.clone())
        .unwrap_or_else(|| url.to_string());

    let (source_id, already_tracked, source_name) = match storage.source_by_url(url)? {
        Some(existing) => (existing.id, true, existing.name),
        None => {
            let new = NewSource {
                name: name.clone(),
                url: url.to_string(),
                adapter: kind,
                top_n_override: None,
            };
            let id = storage.insert_source(&new)?;
            (id, false, name)
        }
    };

    let new_entries: Vec<NewEntry> = fetched
        .entries
        .into_iter()
        .map(|fe| fe.into_new_entry(source_id))
        .collect();
    let total_entries = new_entries.len();
    let inserted = storage.upsert_entries(&new_entries)?;

    Ok(ImportOutcome {
        source_name,
        already_tracked,
        new_entries: inserted,
        total_entries,
    })
}

fn print_import_outcome(outcome: &ImportOutcome) {
    let ImportOutcome {
        source_name,
        already_tracked,
        new_entries,
        total_entries,
    } = outcome;
    let prefix = if *already_tracked {
        format!("Refreshed {source_name}")
    } else {
        format!("Added {source_name}")
    };
    println!("{prefix}: {new_entries} new ({total_entries} in feed)");
}

async fn import_opml(path: &Path) -> Result<()> {
    let raw = fs::read_to_string(path)
        .with_context(|| format!("reading OPML file at {}", path.display()))?;
    let feeds = opml::parse(&raw).with_context(|| format!("parsing OPML at {}", path.display()))?;

    if feeds.is_empty() {
        println!("No feed URLs found in {}", path.display());
        return Ok(());
    }

    let db_path = Storage::default_path().context("resolving database path")?;
    let mut storage = Storage::open(&db_path)
        .with_context(|| format!("opening database at {}", db_path.display()))?;

    println!("Importing {} feed(s) from {}", feeds.len(), path.display());
    let mut added = 0usize;
    let mut refreshed = 0usize;
    let mut failed = 0usize;

    for feed in &feeds {
        let override_name = feed.title.as_deref();
        match add_feed_with_storage(&mut storage, &feed.url, override_name).await {
            Ok(outcome) => {
                print_import_outcome(&outcome);
                if outcome.already_tracked {
                    refreshed += 1;
                } else {
                    added += 1;
                }
            }
            Err(err) => {
                failed += 1;
                eprintln!("  ! {} ({}): {err:#}", feed.display_name(), feed.url);
            }
        }
    }

    println!(
        "Import complete: {added} added, {refreshed} refreshed, {failed} failed out of {}.",
        feeds.len()
    );
    Ok(())
}

fn list_sources() -> Result<()> {
    let db_path = Storage::default_path().context("resolving database path")?;
    let storage = Storage::open(&db_path)
        .with_context(|| format!("opening database at {}", db_path.display()))?;
    let sources = storage.list_sources().context("listing sources")?;

    if sources.is_empty() {
        println!("No sources tracked yet. Try `feedfold add <url>` or `feedfold import <opml>`.");
        return Ok(());
    }

    println!("{} source(s) tracked:", sources.len());
    for source in sources {
        println!(
            "  [{:>3}] {:<8} {}  ({})",
            source.id,
            source.adapter.as_canonical_str(),
            source.name,
            source.url,
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};

    use super::*;
    use feedfold_core::config::AdapterType;

    fn test_app(entries: Vec<Entry>) -> App {
        App::new(
            entries,
            Vec::new(),
            Config::default(),
            ThumbnailMode::TextFallback,
            PathBuf::new(),
        )
    }

    fn sample_source() -> NewSource {
        NewSource {
            name: "Example Feed".to_string(),
            url: "https://example.com/feed.xml".to_string(),
            adapter: AdapterType::Rss,
            top_n_override: None,
        }
    }

    fn sample_entry() -> Entry {
        Entry {
            id: 1,
            source_id: 1,
            external_id: "entry-1".to_string(),
            title: "Feedfold ships thumbnails".to_string(),
            summary: Some("Selected entries can show inline thumbnails.".to_string()),
            url: "https://example.com/posts/1".to_string(),
            thumbnail_url: Some("https://img.example.com/thumb.jpg".to_string()),
            author: Some("Example Feed".to_string()),
            published_at: Some(Utc.with_ymd_and_hms(2024, 1, 2, 3, 4, 5).single().unwrap()),
            fetched_at: Utc.with_ymd_and_hms(2024, 1, 2, 3, 5, 0).single().unwrap(),
            state: EntryState::New,
            rating: None,
            score: Some(123.0),
            displayed_in_top_n: true,
        }
    }

    fn sample_new_entry(source_id: i64, external_id: &str, title: &str) -> NewEntry {
        NewEntry {
            source_id,
            external_id: external_id.to_string(),
            title: title.to_string(),
            summary: Some(format!("Summary for {title}.")),
            url: format!("https://example.com/posts/{external_id}"),
            thumbnail_url: None,
            author: Some("Example Feed".to_string()),
            published_at: Some(Utc.with_ymd_and_hms(2024, 1, 2, 3, 4, 5).single().unwrap()),
            enrichments: HashMap::new(),
        }
    }

    #[test]
    fn ready_thumbnail_clears_placeholder_text() {
        let entry = sample_entry();
        let status = ThumbnailStatus::Ready(PathBuf::from("/tmp/thumb.img"));

        let text = build_thumbnail_status_text(Some(&entry), Some(&status));

        assert!(text.is_empty());
    }

    #[test]
    fn detail_lines_include_title_and_summary() {
        let entry = sample_entry();

        let lines = build_detail_lines(Some(&entry), None);
        let text: String = lines.iter().map(|l| l.to_string() + "\n").collect();

        assert!(text.contains("Feedfold ships thumbnails"));
        assert!(text.contains("https://example.com/posts/1"));
        assert!(text.contains("Jan 02, 2024"));
        assert!(text.contains("new"));
        assert!(text.contains("Selected entries can show inline thumbnails."));
    }

    #[test]
    fn detail_lines_show_rating_as_stars() {
        let mut entry = sample_entry();
        entry.rating = Some(3);

        let lines = build_detail_lines(Some(&entry), None);
        let text: String = lines.iter().map(|l| l.to_string() + "\n").collect();

        assert!(text.contains("\u{2605}\u{2605}\u{2605}"));
    }

    #[test]
    fn detail_lines_show_youtube_view_count() {
        let entry = sample_entry();
        let mut enrichments = HashMap::new();
        enrichments.insert(YOUTUBE_VIEW_COUNT_KEY.to_string(), "1234567".to_string());

        let lines = build_detail_lines(Some(&entry), Some(&enrichments));
        let text: String = lines.iter().map(|l| l.to_string() + "\n").collect();

        assert!(text.contains("1.2M views"), "got: {text}");
    }

    #[test]
    fn compact_count_formats_at_thresholds() {
        assert_eq!(format_compact_count(0), "0");
        assert_eq!(format_compact_count(999), "999");
        assert_eq!(format_compact_count(1_000), "1K");
        assert_eq!(format_compact_count(1_500), "1.5K");
        assert_eq!(format_compact_count(12_300), "12K");
        assert_eq!(format_compact_count(999_999), "999K");
        assert_eq!(format_compact_count(1_000_000), "1M");
        assert_eq!(format_compact_count(1_234_567), "1.2M");
        assert_eq!(format_compact_count(1_000_000_000), "1B");
    }

    #[test]
    fn thumbnail_cache_path_is_stable_for_the_same_url() {
        let cache_dir = Path::new("/tmp/feedfold-test-cache");
        let first = thumbnail_cache_path(cache_dir, "https://img.example.com/thumb.jpg");
        let second = thumbnail_cache_path(cache_dir, "https://img.example.com/thumb.jpg");
        let other = thumbnail_cache_path(cache_dir, "https://img.example.com/other.jpg");

        assert_eq!(first, second);
        assert_ne!(first, other);
    }

    #[test]
    fn strip_html_removes_tags_and_decodes_entities() {
        assert_eq!(
            strip_html("<p>Hello <b>world</b> &amp; friends</p>"),
            "Hello world & friends"
        );
    }

    #[test]
    fn strip_html_converts_block_elements_to_newlines() {
        let html = "<p>First paragraph.</p><p>Second paragraph.</p>";
        let result = strip_html(html);
        assert!(result.contains("First paragraph.\n"));
        assert!(result.contains("Second paragraph."));
    }

    #[test]
    fn strip_html_handles_list_items() {
        let html = "<ul><li>One</li><li>Two</li></ul>";
        let result = strip_html(html);
        assert!(result.contains("- One"));
        assert!(result.contains("- Two"));
    }

    #[test]
    fn strip_html_decodes_numeric_entities() {
        assert_eq!(strip_html("&#60;tag&#62;"), "<tag>");
        assert_eq!(strip_html("&#x2014;"), "\u{2014}");
    }

    #[test]
    fn strip_html_passes_plain_text_through() {
        assert_eq!(strip_html("Just plain text"), "Just plain text");
    }

    #[test]
    fn viewed_title_includes_todays_count() {
        let mut app = test_app(vec![sample_entry()]);
        app.set_view(ActiveView::Viewed);
        app.viewed_today_count = 3;

        assert_eq!(app.list_title(), "Viewed (today: 3)");
    }

    #[test]
    fn search_title_tracks_editing_state() {
        let mut app = test_app(vec![sample_entry()]);

        app.begin_search();
        assert_eq!(app.list_title(), "Search: _");

        for c in "rust".chars() {
            app.push_search_char(c);
        }
        assert_eq!(app.list_title(), "Search: rust_");

        app.finish_search();
        assert_eq!(app.list_title(), "Search: rust");
    }

    #[test]
    fn refresh_view_uses_search_results_over_active_view() {
        let mut storage = Storage::open_in_memory().unwrap();
        let source_id = storage.insert_source(&sample_source()).unwrap();

        storage
            .upsert_entries(&[
                sample_new_entry(source_id, "rust", "Rust notes"),
                NewEntry {
                    source_id,
                    external_id: "sqlite".to_string(),
                    title: "Database internals".to_string(),
                    summary: Some("SQLite FTS5 query planner".to_string()),
                    url: "https://example.com/posts/sqlite".to_string(),
                    thumbnail_url: None,
                    author: Some("Example Feed".to_string()),
                    published_at: Some(Utc.with_ymd_and_hms(2024, 1, 2, 3, 4, 5).single().unwrap()),
                    enrichments: HashMap::new(),
                },
            ])
            .unwrap();

        let mut app = test_app(Vec::new());
        app.begin_search();
        for c in "FTS5".chars() {
            app.push_search_char(c);
        }

        refresh_view(&mut app, &storage).unwrap();

        assert_eq!(app.entries.len(), 1);
        assert_eq!(app.entries[0].external_id, "sqlite");

        app.clear_search();
        refresh_view(&mut app, &storage).unwrap();

        assert!(app.entries.is_empty());
    }

    #[test]
    fn refresh_view_loads_viewed_entries_and_today_count() {
        let mut storage = Storage::open_in_memory().unwrap();
        let source_id = storage.insert_source(&sample_source()).unwrap();

        storage
            .upsert_entries(&[
                sample_new_entry(source_id, "new", "New Entry"),
                sample_new_entry(source_id, "viewed", "Viewed Entry"),
            ])
            .unwrap();
        let viewed_entry = storage
            .list_entries_for_source(source_id)
            .unwrap()
            .into_iter()
            .find(|entry| entry.external_id == "viewed")
            .unwrap();
        storage.record_entry_view(viewed_entry.id).unwrap();

        let mut app = test_app(Vec::new());
        app.set_view(ActiveView::Viewed);

        refresh_view(&mut app, &storage).unwrap();

        assert_eq!(app.entries.len(), 1);
        assert_eq!(app.entries[0].external_id, "viewed");
        assert_eq!(app.viewed_today_count, 1);
    }

    #[test]
    fn refresh_view_includes_starred_entries_in_viewed_list() {
        let mut storage = Storage::open_in_memory().unwrap();
        let source_id = storage.insert_source(&sample_source()).unwrap();

        storage
            .upsert_entries(&[
                sample_new_entry(source_id, "viewed", "Viewed Entry"),
                sample_new_entry(source_id, "starred", "Starred Entry"),
            ])
            .unwrap();
        let entries = storage.list_entries_for_source(source_id).unwrap();
        let viewed = entries
            .iter()
            .find(|entry| entry.external_id == "viewed")
            .unwrap();
        let starred = entries
            .iter()
            .find(|entry| entry.external_id == "starred")
            .unwrap();

        storage.record_entry_view(viewed.id).unwrap();
        storage
            .set_entry_state(starred.id, EntryState::Starred)
            .unwrap();

        let mut app = test_app(Vec::new());
        app.set_view(ActiveView::Viewed);

        refresh_view(&mut app, &storage).unwrap();

        assert_eq!(app.entries.len(), 2);
        assert!(app
            .entries
            .iter()
            .any(|entry| entry.external_id == "viewed"));
        assert!(app
            .entries
            .iter()
            .any(|entry| entry.external_id == "starred" && entry.state == EntryState::Starred));
    }

    #[test]
    fn overflow_title_is_static() {
        let mut app = test_app(vec![sample_entry()]);
        app.set_view(ActiveView::Overflow);

        assert_eq!(app.list_title(), "Overflow");
    }

    #[test]
    fn refresh_view_loads_only_overflow_entries() {
        use feedfold_core::ranker::Score;

        let mut storage = Storage::open_in_memory().unwrap();
        let source_id = storage.insert_source(&sample_source()).unwrap();

        storage
            .upsert_entries(&[
                sample_new_entry(source_id, "top", "Top Entry"),
                sample_new_entry(source_id, "overflow", "Overflow Entry"),
                sample_new_entry(source_id, "viewed", "Viewed Entry"),
            ])
            .unwrap();
        let entries = storage.list_entries_for_source(source_id).unwrap();
        let top = entries
            .iter()
            .find(|entry| entry.external_id == "top")
            .unwrap();
        let overflow = entries
            .iter()
            .find(|entry| entry.external_id == "overflow")
            .unwrap();
        let viewed = entries
            .iter()
            .find(|entry| entry.external_id == "viewed")
            .unwrap();

        storage
            .apply_ranking(
                source_id,
                &[
                    Score {
                        entry_id: top.id,
                        value: 30.0,
                    },
                    Score {
                        entry_id: overflow.id,
                        value: 20.0,
                    },
                    Score {
                        entry_id: viewed.id,
                        value: 10.0,
                    },
                ],
                1,
            )
            .unwrap();
        storage.record_entry_view(viewed.id).unwrap();

        let mut app = test_app(Vec::new());
        app.set_view(ActiveView::Overflow);

        refresh_view(&mut app, &storage).unwrap();

        assert_eq!(app.entries.len(), 1);
        assert_eq!(app.entries[0].external_id, "overflow");
    }

    #[test]
    fn toggling_star_in_overflow_refreshes_the_list() {
        use feedfold_core::ranker::Score;

        let mut storage = Storage::open_in_memory().unwrap();
        let source_id = storage.insert_source(&sample_source()).unwrap();

        storage
            .upsert_entries(&[sample_new_entry(source_id, "overflow", "Overflow Entry")])
            .unwrap();
        let entry = storage
            .list_entries_for_source(source_id)
            .unwrap()
            .into_iter()
            .find(|entry| entry.external_id == "overflow")
            .unwrap();
        storage
            .apply_ranking(
                source_id,
                &[Score {
                    entry_id: entry.id,
                    value: 10.0,
                }],
                0,
            )
            .unwrap();

        let mut app = test_app(Vec::new());
        app.set_view(ActiveView::Overflow);
        refresh_view(&mut app, &storage).unwrap();

        assert_eq!(app.entries.len(), 1);
        assert!(toggle_star_for_selected(&mut app, &mut storage).unwrap());
        assert!(app.entries.is_empty());

        let updated = storage
            .list_entries_for_source(source_id)
            .unwrap()
            .into_iter()
            .find(|item| item.id == entry.id)
            .unwrap();
        assert_eq!(updated.state, EntryState::Starred);
    }
}
