use std::collections::{hash_map::DefaultHasher, HashMap};
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
    event::{self, Event, KeyCode},
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

use feedfold_adapters::RssAdapter;
use feedfold_core::adapter::SourceAdapter;
use feedfold_core::storage::{Entry, EntryState, NewEntry, NewSource, Storage};
use feedfold_core::VERSION;

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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ThumbnailMode {
    Viuer,
    TextFallback,
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
        None => run_tui().await,
    }
}

async fn run_tui() -> Result<()> {
    let db_path = Storage::default_path().context("resolving database path")?;
    let mut storage = Storage::open(&db_path)
        .with_context(|| format!("opening database at {}", db_path.display()))?;

    let entries = storage.list_top_n_entries()?;
    let thumbnail_dir = db_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("thumbnails");

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new(entries, detect_thumbnail_mode(), thumbnail_dir);
    let res = run_app(&mut terminal, &mut app, &mut storage);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    res
}

struct App {
    entries: Vec<Entry>,
    state: ListState,
    thumbnail_mode: ThumbnailMode,
    thumbnail_dir: PathBuf,
    thumbnail_cache: HashMap<String, ThumbnailStatus>,
    thumbnail_tx: Sender<ThumbnailDownload>,
    thumbnail_rx: Receiver<ThumbnailDownload>,
}

impl App {
    fn new(entries: Vec<Entry>, thumbnail_mode: ThumbnailMode, thumbnail_dir: PathBuf) -> App {
        let (thumbnail_tx, thumbnail_rx) = mpsc::channel();
        let mut state = ListState::default();
        if !entries.is_empty() {
            state.select(Some(0));
        }
        App {
            entries,
            state,
            thumbnail_mode,
            thumbnail_dir,
            thumbnail_cache: HashMap::new(),
            thumbnail_tx,
            thumbnail_rx,
        }
    }

    fn next(&mut self) {
        if self.entries.is_empty() {
            return;
        }
        let i = match self.state.selected() {
            Some(i) => {
                if i >= self.entries.len() - 1 {
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
        if self.entries.is_empty() {
            return;
        }
        let i = match self.state.selected() {
            Some(i) => {
                if i == 0 {
                    self.entries.len() - 1
                } else {
                    i - 1
                }
            }
            None => 0,
        };
        self.state.select(Some(i));
    }

    fn replace_entries(&mut self, entries: Vec<Entry>) {
        self.entries = entries;
        self.sync_selection();
    }

    fn sync_selection(&mut self) {
        match self.state.selected() {
            Some(_) if self.entries.is_empty() => self.state.select(None),
            Some(i) if i >= self.entries.len() => self.state.select(Some(self.entries.len() - 1)),
            None if !self.entries.is_empty() => self.state.select(Some(0)),
            _ => {}
        }
    }

    fn selected_entry(&self) -> Option<&Entry> {
        self.state.selected().and_then(|i| self.entries.get(i))
    }

    fn selected_thumbnail_status(&self) -> Option<&ThumbnailStatus> {
        let url = self.selected_entry()?.thumbnail_url.as_ref()?;
        self.thumbnail_cache.get(url)
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
        self.thumbnail_cache.insert(url.clone(), ThumbnailStatus::Loading);
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
            terminal.draw(|f| ui(f, app))?;
            needs_redraw = draw_selected_thumbnail(terminal, app)?;
        }

        if event::poll(Duration::from_millis(50))? {
            if let Event::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Char('q') => return Ok(()),
                    KeyCode::Char('r') => {
                        app.replace_entries(storage.list_top_n_entries()?);
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
                    KeyCode::Enter => {
                        if let Some(i) = app.state.selected() {
                            if let Some(entry) = app.entries.get_mut(i) {
                                let _ = open::that(&entry.url);
                                if entry.state == EntryState::New {
                                    entry.state = EntryState::Viewed;
                                    let _ = storage.set_entry_state(entry.id, EntryState::Viewed);
                                    needs_redraw = true;
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

fn ui(f: &mut ratatui::Frame, app: &mut App) {
    let (list_area, detail_area) = main_sections(f.area());

    let items: Vec<ListItem> = app
        .entries
        .iter()
        .map(|entry| {
            let source = entry.author.clone().unwrap_or_else(|| "Unknown".to_string());
            let title_style = if entry.state == EntryState::New {
                Style::default().add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            let line = Line::from(vec![
                Span::styled(format!("[{source}] "), Style::default().fg(Color::DarkGray)),
                Span::styled(entry.title.clone(), title_style),
            ]);
            ListItem::new(line)
        })
        .collect();

    let items_list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title("Home"))
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol(">> ");

    f.render_stateful_widget(items_list, list_area, &mut app.state);

    let detail_block = Block::default().borders(Borders::ALL).title("Detail");
    let detail_inner = detail_block.inner(detail_area);
    f.render_widget(detail_block, detail_area);

    let (thumbnail_area, summary_area) = detail_sections(detail_inner);
    if thumbnail_area.width > 0 && thumbnail_area.height > 0 {
        let thumbnail_text = build_thumbnail_status_text(
            app.selected_entry(),
            app.selected_thumbnail_status(),
            app.thumbnail_mode,
        );
        f.render_widget(
            Paragraph::new(thumbnail_text).wrap(Wrap { trim: true }),
            thumbnail_area,
        );
    }

    if summary_area.width > 0 && summary_area.height > 0 {
        let summary_text = build_summary_text(app.selected_entry());
        f.render_widget(
            Paragraph::new(summary_text).wrap(Wrap { trim: true }),
            summary_area,
        );
    }
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
    mode: ThumbnailMode,
) -> String {
    let Some(entry) = entry else {
        return String::new();
    };

    let Some(_) = entry.thumbnail_url.as_ref() else {
        return "No thumbnail available.".to_string();
    };

    match mode {
        ThumbnailMode::TextFallback => {
            "Thumbnail available on image-capable terminals. Showing text-only detail."
                .to_string()
        }
        ThumbnailMode::Viuer => match status {
            Some(ThumbnailStatus::Ready(_)) => String::new(),
            Some(ThumbnailStatus::Loading) | None => "Loading thumbnail...".to_string(),
            Some(ThumbnailStatus::Failed(_)) => {
                "Thumbnail unavailable. Showing text-only detail.".to_string()
            }
        },
    }
}

fn build_summary_text(entry: Option<&Entry>) -> String {
    let Some(entry) = entry else {
        return String::new();
    };

    let mut text = format!("Title: {}\nURL: {}\n", entry.title, entry.url);
    if let Some(date) = entry.published_at {
        text.push_str(&format!("Published: {}\n", date.to_rfc2822()));
    }
    text.push('\n');
    text.push_str(entry.summary.as_deref().unwrap_or("No summary available."));
    text
}

fn draw_selected_thumbnail(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    app: &mut App,
) -> Result<bool> {
    if app.thumbnail_mode != ThumbnailMode::Viuer {
        return Ok(false);
    }

    let Some(path) = app.selected_thumbnail_status().and_then(|status| match status {
        ThumbnailStatus::Ready(path) => Some(path.clone()),
        ThumbnailStatus::Loading | ThumbnailStatus::Failed(_) => None,
    }) else {
        return Ok(false);
    };

    let (_, detail_area) = main_sections(terminal.size()?.into());
    let detail_inner = Block::default().borders(Borders::ALL).title("Detail").inner(detail_area);
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
    let adapter = RssAdapter::new();
    let fetched = adapter
        .fetch(url)
        .await
        .with_context(|| format!("fetching feed at {url}"))?;

    let name = override_name
        .map(str::to_owned)
        .or_else(|| fetched.name.clone())
        .unwrap_or_else(|| url.to_string());

    let db_path = Storage::default_path().context("resolving database path")?;
    let mut storage = Storage::open(&db_path)
        .with_context(|| format!("opening database at {}", db_path.display()))?;

    let source_id = match storage.source_by_url(url)? {
        Some(existing) => {
            println!(
                "Source already tracked: {} (id {}). Refreshing entries.",
                existing.name, existing.id
            );
            existing.id
        }
        None => {
            let new = NewSource {
                name: name.clone(),
                url: url.to_string(),
                adapter: adapter.kind(),
                top_n_override: None,
            };
            let id = storage.insert_source(&new)?;
            println!("Added source: {name} (id {id})");
            id
        }
    };

    let new_entries: Vec<NewEntry> = fetched
        .entries
        .into_iter()
        .map(|fe| fe.into_new_entry(source_id))
        .collect();

    let inserted = storage.upsert_entries(&new_entries)?;
    println!(
        "Imported {inserted} new entries ({} total in feed).",
        new_entries.len()
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};

    use super::*;

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

    #[test]
    fn text_fallback_mentions_text_only_detail() {
        let entry = sample_entry();

        let text = build_thumbnail_status_text(Some(&entry), None, ThumbnailMode::TextFallback);

        assert!(text.contains("text-only detail"));
    }

    #[test]
    fn ready_thumbnail_clears_placeholder_text() {
        let entry = sample_entry();
        let status = ThumbnailStatus::Ready(PathBuf::from("/tmp/thumb.img"));

        let text = build_thumbnail_status_text(Some(&entry), Some(&status), ThumbnailMode::Viuer);

        assert!(text.is_empty());
    }

    #[test]
    fn summary_text_includes_metadata_and_summary() {
        let entry = sample_entry();

        let text = build_summary_text(Some(&entry));

        assert!(text.contains("Title: Feedfold ships thumbnails"));
        assert!(text.contains("URL: https://example.com/posts/1"));
        assert!(text.contains("Published: Tue, 2 Jan 2024 03:04:05 +0000"));
        assert!(text.contains("Selected entries can show inline thumbnails."));
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
}
