use std::io;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use crossterm::{
    event::{self, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
    Terminal,
};

use feedfold_adapters::RssAdapter;
use feedfold_core::adapter::SourceAdapter;
use feedfold_core::storage::{Entry, NewEntry, NewSource, Storage};
use feedfold_core::VERSION;

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
    let storage = Storage::open(&db_path)
        .with_context(|| format!("opening database at {}", db_path.display()))?;

    let entries = storage.list_top_n_entries()?;

    // setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new(entries);
    let res = run_app(&mut terminal, &mut app);

    // restore terminal
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    res
}

struct App {
    entries: Vec<Entry>,
    state: ListState,
}

impl App {
    fn new(entries: Vec<Entry>) -> App {
        let mut state = ListState::default();
        if !entries.is_empty() {
            state.select(Some(0));
        }
        App { entries, state }
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
}

fn run_app(terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>, app: &mut App) -> Result<()> {
    loop {
        terminal.draw(|f| ui(f, app))?;

        if event::poll(std::time::Duration::from_millis(50))? {
            if let Event::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Char('q') => return Ok(()),
                    KeyCode::Char('j') | KeyCode::Down => app.next(),
                    KeyCode::Char('k') | KeyCode::Up => app.previous(),
                    KeyCode::Enter => {
                        if let Some(i) = app.state.selected() {
                            if let Some(entry) = app.entries.get(i) {
                                let _ = open::that(&entry.url);
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
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)].as_ref())
        .split(f.area());

    let items: Vec<ListItem> = app
        .entries
        .iter()
        .map(|entry| {
            let title = entry.title.clone();
            let source = entry.author.clone().unwrap_or_else(|| "Unknown".to_string());
            let line = Line::from(vec![
                Span::styled(format!("[{}] ", source), Style::default().fg(Color::DarkGray)),
                Span::raw(title),
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

    f.render_stateful_widget(items_list, chunks[0], &mut app.state);

    let summary_text = if let Some(i) = app.state.selected() {
        if let Some(entry) = app.entries.get(i) {
            let mut text = format!("Title: {}\nURL: {}\n", entry.title, entry.url);
            if let Some(date) = entry.published_at {
                text.push_str(&format!("Published: {}\n", date.to_rfc2822()));
            }
            text.push('\n');
            text.push_str(entry.summary.as_deref().unwrap_or("No summary available."));
            text
        } else {
            String::new()
        }
    } else {
        String::new()
    };

    let detail_paragraph = Paragraph::new(summary_text)
        .block(Block::default().borders(Borders::ALL).title("Detail"))
        .wrap(Wrap { trim: true });

    f.render_widget(detail_paragraph, chunks[1]);
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
