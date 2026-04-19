use std::collections::{hash_map::DefaultHasher, HashMap, HashSet};
use std::fs;
use std::future::Future;
use std::hash::{Hash, Hasher};
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::OnceLock;
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, Local, Utc};
use clap::{Parser, Subcommand};
use crossterm::{
    event::{self, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use directories::BaseDirs;
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
    RssAdapter, YoutubeAdapter, YOUTUBE_DURATION_KEY, YOUTUBE_EMBED_HEIGHT_KEY,
    YOUTUBE_EMBED_WIDTH_KEY, YOUTUBE_LIVE_BROADCAST_KEY, YOUTUBE_VIEW_COUNT_KEY,
};
use feedfold_core::adapter::SourceAdapter;
use feedfold_core::config::{AdapterType, ChannelSort, Config, RankingMode};
use feedfold_core::storage::{
    ChannelStats, Entry, EntryState, NewEntry, NewSource, Source as DbSource, Storage,
};
use feedfold_core::VERSION;

mod opml;

const THUMBNAIL_HEIGHT: u16 = 12;
const YOUTUBE_SHORTS_2024_EXPANSION_AT: &str = "2024-10-15T00:00:00Z";
const LAUNCHD_LABEL: &str = "com.feedfold.feedfoldd";

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
    /// Export tracked sources as OPML to stdout.
    Export,
    /// List every source currently tracked in the database.
    List,
    /// Remove a source by its numeric id or its URL.
    Remove {
        /// Source id (from `feedfold list`) or feed URL.
        id_or_url: String,
        /// Skip the confirmation prompt.
        #[arg(short = 'y', long)]
        yes: bool,
    },
    /// Manage the background daemon on macOS.
    Daemon {
        #[command(subcommand)]
        command: DaemonCommand,
    },
}

#[derive(Debug, Subcommand)]
enum DaemonCommand {
    /// Install a launchd agent plist for feedfoldd.
    Install,
    /// Show whether the launchd service is installed or running.
    Status,
    /// Load and start the launchd service.
    Start,
    /// Stop and unload the launchd service.
    Stop,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum LaunchctlServiceStatus {
    Unloaded,
    Loaded {
        state: Option<String>,
        pid: Option<u32>,
    },
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
    channel_sort: ChannelSort,
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
            channel_sort: config.general.channel_sort,
            poll_interval: config.general.poll_interval_mins,
            show_shorts: config.youtube.show_shorts,
            show_live: config.youtube.show_live,
            show_premieres: config.youtube.show_premieres,
        }
    }

    const FIELD_COUNT: usize = 8;
    const VIEW_IGNORED_FIELD: usize = 7;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StatusKind {
    Info,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StatusMessage {
    text: String,
    kind: StatusKind,
}

#[derive(Debug)]
struct ThumbnailDownload {
    url: String,
    result: Result<PathBuf, String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    if let Some(path) = Config::bootstrap_if_missing().context("bootstrapping config")? {
        eprintln!(
            "Created config at {}. Edit it to set feeds, polling, and API keys.",
            path.display()
        );
    }
    match cli.command {
        Some(Command::Add { url, name }) => add_feed(&url, name.as_deref()).await,
        Some(Command::Import { path }) => import_opml(&path).await,
        Some(Command::Export) => export_opml(),
        Some(Command::List) => list_sources(),
        Some(Command::Remove { id_or_url, yes }) => remove_source(&id_or_url, yes),
        Some(Command::Daemon { command }) => match command {
            DaemonCommand::Install => install_daemon(),
            DaemonCommand::Status => daemon_status(),
            DaemonCommand::Start => start_daemon(),
            DaemonCommand::Stop => stop_daemon(),
        },
        None => run_tui().await,
    }
}

fn install_daemon() -> Result<()> {
    require_macos_daemon_command("install")?;

    let home_dir = home_dir()?;
    let plist_path = launch_agent_path(&home_dir);
    let daemon_path =
        daemon_binary_path(&std::env::current_exe().context("resolving current executable")?)?;
    if !daemon_path.exists() {
        anyhow::bail!(
            "feedfoldd was not found next to feedfold at {}",
            daemon_path.display()
        );
    }

    let working_dir = daemon_path
        .parent()
        .context("daemon executable has no parent directory")?;
    let plist = render_launchd_plist(&daemon_path, working_dir);
    let status = match fs::read_to_string(&plist_path) {
        Ok(existing) if existing == plist => "Launchd agent already up to date at",
        _ => {
            if let Some(parent) = plist_path.parent() {
                fs::create_dir_all(parent).with_context(|| {
                    format!("creating launch agent directory {}", parent.display())
                })?;
            }
            fs::write(&plist_path, plist).with_context(|| {
                format!("writing launch agent plist at {}", plist_path.display())
            })?;
            "Installed launchd agent at"
        }
    };

    println!("{status} {}", plist_path.display());
    Ok(())
}

fn daemon_status() -> Result<()> {
    require_macos_daemon_command("status")?;

    let plist_path = launch_agent_path(&home_dir()?);
    if !plist_path.exists() {
        println!("Daemon not installed. Run `feedfold daemon install` first.");
        return Ok(());
    }

    let (_, service_target) = launchctl_targets()?;
    match launchctl_service_status(&service_target)? {
        LaunchctlServiceStatus::Unloaded => {
            println!(
                "Daemon stopped. Launchd agent is installed at {}.",
                plist_path.display()
            );
        }
        LaunchctlServiceStatus::Loaded {
            state,
            pid: Some(pid),
        } => {
            println!("Daemon running with pid {pid}.");
            if let Some(state) = state.filter(|value| value != "running") {
                println!("Launchd state: {state}");
            }
        }
        LaunchctlServiceStatus::Loaded { state, pid: None } => {
            if let Some(state) = state {
                println!("Daemon loaded with launchd state `{state}`.");
            } else {
                println!("Daemon loaded.");
            }
        }
    }

    Ok(())
}

fn start_daemon() -> Result<()> {
    require_macos_daemon_command("start")?;

    let plist_path = launch_agent_path(&home_dir()?);
    if !plist_path.exists() {
        anyhow::bail!(
            "launchd agent is not installed at {}. Run `feedfold daemon install` first",
            plist_path.display()
        );
    }

    let (domain_target, service_target) = launchctl_targets()?;
    match launchctl_service_status(&service_target)? {
        LaunchctlServiceStatus::Loaded {
            pid: Some(pid),
            state,
        } if state.as_deref() == Some("running") => {
            println!("Daemon already running with pid {pid}.");
            return Ok(());
        }
        LaunchctlServiceStatus::Loaded { .. } => {
            let output = ProcessCommand::new("launchctl")
                .arg("kickstart")
                .arg("-p")
                .arg(&service_target)
                .output()
                .context("running `launchctl kickstart`")?;
            ensure_launchctl_success("kickstart", &output)?;

            if let Some(pid) = parse_launchctl_pid(&String::from_utf8_lossy(&output.stdout)) {
                println!("Started daemon with pid {pid}.");
            } else {
                println!("Started daemon.");
            }
        }
        LaunchctlServiceStatus::Unloaded => {
            let output = ProcessCommand::new("launchctl")
                .arg("bootstrap")
                .arg(&domain_target)
                .arg(&plist_path)
                .output()
                .context("running `launchctl bootstrap`")?;
            ensure_launchctl_success("bootstrap", &output)?;

            print_started_daemon_status(&launchctl_service_status(&service_target)?);
        }
    }

    Ok(())
}

fn stop_daemon() -> Result<()> {
    require_macos_daemon_command("stop")?;

    let plist_path = launch_agent_path(&home_dir()?);
    if !plist_path.exists() {
        println!("Daemon not installed.");
        return Ok(());
    }

    let (_, service_target) = launchctl_targets()?;
    match launchctl_service_status(&service_target)? {
        LaunchctlServiceStatus::Unloaded => {
            println!("Daemon already stopped.");
        }
        LaunchctlServiceStatus::Loaded { .. } => {
            let output = ProcessCommand::new("launchctl")
                .arg("bootout")
                .arg(&service_target)
                .output()
                .context("running `launchctl bootout`")?;
            ensure_launchctl_success("bootout", &output)?;
            println!("Stopped daemon.");
        }
    }

    Ok(())
}

fn require_macos_daemon_command(command: &str) -> Result<()> {
    if !cfg!(target_os = "macos") {
        anyhow::bail!("`feedfold daemon {command}` is only supported on macOS");
    }

    Ok(())
}

fn home_dir() -> Result<PathBuf> {
    BaseDirs::new()
        .map(|dirs| dirs.home_dir().to_path_buf())
        .context("resolving home directory")
}

fn launch_agent_path(home_dir: &Path) -> PathBuf {
    home_dir
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{LAUNCHD_LABEL}.plist"))
}

fn daemon_binary_path(current_exe: &Path) -> Result<PathBuf> {
    let exe_dir = current_exe
        .parent()
        .context("current executable has no parent directory")?;
    Ok(exe_dir.join(format!("feedfoldd{}", std::env::consts::EXE_SUFFIX)))
}

fn launchctl_targets() -> Result<(String, String)> {
    let domain_target = launchctl_domain_target()?;
    let service_target = launchctl_service_target(&domain_target);
    Ok((domain_target, service_target))
}

fn launchctl_domain_target() -> Result<String> {
    let output = ProcessCommand::new("id")
        .arg("-u")
        .output()
        .context("running `id -u`")?;
    if !output.status.success() {
        anyhow::bail!("`id -u` failed: {}", command_output_text(&output));
    }

    let uid = String::from_utf8(output.stdout)
        .context("`id -u` returned non-UTF-8 output")?
        .trim()
        .to_string();
    if uid.is_empty() {
        anyhow::bail!("`id -u` returned an empty uid");
    }

    Ok(format!("gui/{uid}"))
}

fn launchctl_service_target(domain_target: &str) -> String {
    format!("{domain_target}/{LAUNCHD_LABEL}")
}

fn launchctl_service_status(service_target: &str) -> Result<LaunchctlServiceStatus> {
    let output = ProcessCommand::new("launchctl")
        .arg("print")
        .arg(service_target)
        .output()
        .with_context(|| format!("running `launchctl print {service_target}`"))?;

    parse_launchctl_service_status(output.status.code(), &command_output_text(&output))
}

fn parse_launchctl_service_status(
    exit_code: Option<i32>,
    output: &str,
) -> Result<LaunchctlServiceStatus> {
    if exit_code == Some(0) {
        let state = output
            .lines()
            .find_map(|line| line.trim().strip_prefix("state = ").map(ToOwned::to_owned));
        let pid = parse_launchctl_pid(output);
        return Ok(LaunchctlServiceStatus::Loaded { state, pid });
    }

    if output.contains("Could not find service") {
        return Ok(LaunchctlServiceStatus::Unloaded);
    }

    anyhow::bail!(
        "`launchctl print` failed{}: {}",
        exit_code
            .map(|value| format!(" with exit code {value}"))
            .unwrap_or_default(),
        output.trim()
    );
}

fn parse_launchctl_pid(output: &str) -> Option<u32> {
    output.lines().find_map(|line| {
        let trimmed = line.trim();
        trimmed
            .strip_prefix("pid = ")
            .unwrap_or(trimmed)
            .parse()
            .ok()
    })
}

fn ensure_launchctl_success(command: &str, output: &std::process::Output) -> Result<()> {
    if output.status.success() {
        return Ok(());
    }

    anyhow::bail!(
        "`launchctl {command}` failed: {}",
        command_output_text(output)
    );
}

fn command_output_text(output: &std::process::Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();

    match (stdout.is_empty(), stderr.is_empty()) {
        (false, false) => format!("{stdout}\n{stderr}"),
        (false, true) => stdout,
        (true, false) => stderr,
        (true, true) => "(no output)".to_string(),
    }
}

fn print_started_daemon_status(status: &LaunchctlServiceStatus) {
    match status {
        LaunchctlServiceStatus::Loaded {
            pid: Some(pid),
            state,
        } if state.as_deref() == Some("running") => {
            println!("Started daemon with pid {pid}.");
        }
        LaunchctlServiceStatus::Loaded {
            state,
            pid: Some(pid),
        } => {
            if let Some(state) = state {
                println!("Started daemon with pid {pid} (launchd state `{state}`).");
            } else {
                println!("Started daemon with pid {pid}.");
            }
        }
        LaunchctlServiceStatus::Loaded { state, pid: None } => {
            if let Some(state) = state {
                println!("Started daemon (launchd state `{state}`).");
            } else {
                println!("Started daemon.");
            }
        }
        LaunchctlServiceStatus::Unloaded => {
            println!("Daemon loaded, but launchctl did not report a running service.");
        }
    }
}

fn render_launchd_plist(daemon_path: &Path, working_dir: &Path) -> String {
    let daemon = escape_xml_path(daemon_path);
    let workdir = escape_xml_path(working_dir);
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{LAUNCHD_LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{daemon}</string>
    </array>
    <key>WorkingDirectory</key>
    <string>{workdir}</string>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
</dict>
</plist>
"#
    )
}

fn escape_xml_path(path: &Path) -> String {
    path.display()
        .to_string()
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
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

    let last_poll_at = storage.last_poll_at().ok().flatten();
    let mut app = App::new(
        entries,
        sources,
        config,
        detect_thumbnail_mode(),
        thumbnail_dir,
    );
    app.last_poll_at = last_poll_at;
    if let Err(error) = trigger_refresh(&mut app, &mut storage) {
        app.set_error(format!("Refresh failed: {error:#}"));
    }
    let res = run_app(&mut terminal, &mut app, &mut storage);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    res
}

fn trigger_refresh(app: &mut App, storage: &mut Storage) -> Result<()> {
    let sources = storage.list_sources()?;

    for source in &sources {
        let top_n = source
            .top_n_override
            .unwrap_or(app.config.general.default_top_n) as usize;
        let mode = app
            .config
            .sources
            .iter()
            .find(|s| s.url == source.url)
            .and_then(|s| s.ranking)
            .unwrap_or(app.config.ranking.mode);

        let entries = storage.list_entries_for_source(source.id)?;
        let enrichments = storage.list_enrichments_for_source(source.id)?;
        let candidates: Vec<Entry> = entries
            .into_iter()
            .filter(|e| matches!(e.state, EntryState::New | EntryState::Starred))
            .collect();
        let ctx = feedfold_core::ranker::RankContext { top_n, enrichments };
        let scores = match mode {
            RankingMode::Recency => feedfold_core::ranker::Ranker::rank(
                &feedfold_core::ranker::RecencyRanker,
                &candidates,
                &ctx,
            ),
            RankingMode::Popularity => feedfold_core::ranker::Ranker::rank(
                &feedfold_core::ranker::PopularityRanker,
                &candidates,
                &ctx,
            ),
            RankingMode::Claude => feedfold_core::ranker::Ranker::rank(
                &feedfold_core::ranker::RecencyRanker,
                &candidates,
                &ctx,
            ),
        };
        storage.apply_ranking(source.id, &scores, top_n)?;
    }

    app.sources = sources.into_iter().map(|s| (s.id, s)).collect();
    storage.set_last_poll_at(Utc::now())?;
    refresh_view(app, storage)?;
    Ok(())
}

#[derive(Debug, Clone)]
enum UndoAction {
    SetState { id: i64, prev_state: EntryState },
    SetRating { id: i64, prev_rating: Option<u8> },
    MarkedView { id: i64, prev_state: EntryState },
    UnmarkedView { id: i64 },
}

const UNDO_STACK_MAX: usize = 50;

struct App {
    active_view: ActiveView,
    entries: Vec<Entry>,
    entry_enrichments: HashMap<i64, HashMap<String, String>>,
    channel_rows: Vec<ChannelRow>,
    channels_expanded: HashSet<i64>,
    sources: HashMap<i64, DbSource>,
    channel_stats: HashMap<i64, ChannelStats>,
    channel_sort: ChannelSort,
    viewed_today_count: usize,
    last_poll_at: Option<DateTime<Utc>>,
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
    undo_stack: Vec<UndoAction>,
    status_message: Option<StatusMessage>,
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
        let sources = sources.into_iter().map(|s| (s.id, s)).collect();
        let channel_sort = config.general.channel_sort;
        App {
            active_view: ActiveView::Home,
            entries,
            entry_enrichments: HashMap::new(),
            channel_rows: Vec::new(),
            channels_expanded: HashSet::new(),
            sources,
            channel_stats: HashMap::new(),
            channel_sort,
            viewed_today_count: 0,
            last_poll_at: None,
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
            undo_stack: Vec::new(),
            status_message: None,
        }
    }

    fn push_undo(&mut self, action: UndoAction) {
        if self.undo_stack.len() >= UNDO_STACK_MAX {
            self.undo_stack.remove(0);
        }
        self.undo_stack.push(action);
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
        let mut groups: HashMap<i64, Vec<Entry>> = HashMap::new();
        for entry in &self.entries {
            match groups.entry(entry.source_id) {
                HEntry::Vacant(slot) => {
                    slot.insert(vec![entry.clone()]);
                }
                HEntry::Occupied(mut slot) => {
                    slot.get_mut().push(entry.clone());
                }
            }
        }

        let sort = self.channel_sort;
        let stats = &self.channel_stats;
        let mut order: Vec<i64> = self.sources.keys().copied().collect();
        order.sort_by(|a, b| {
            let name_a = self.sources.get(a).map(|s| s.name.as_str()).unwrap_or("");
            let name_b = self.sources.get(b).map(|s| s.name.as_str()).unwrap_or("");
            let name_cmp = name_a.to_lowercase().cmp(&name_b.to_lowercase());
            match sort {
                ChannelSort::Alphabetical => name_cmp,
                ChannelSort::MostRecent => {
                    let at_a = stats.get(a).and_then(|s| s.latest_published);
                    let at_b = stats.get(b).and_then(|s| s.latest_published);
                    at_b.cmp(&at_a).then(name_cmp)
                }
                ChannelSort::TopRated => {
                    let avg = |id: &i64| -> f64 {
                        stats
                            .get(id)
                            .filter(|s| s.rating_n > 0)
                            .map(|s| s.rating_total as f64 / s.rating_n as f64)
                            .unwrap_or(-1.0)
                    };
                    avg(b)
                        .partial_cmp(&avg(a))
                        .unwrap_or(std::cmp::Ordering::Equal)
                        .then(name_cmp)
                }
                ChannelSort::MostNew => {
                    let new_count = |id: &i64| stats.get(id).map(|s| s.new_count).unwrap_or(0);
                    new_count(b).cmp(&new_count(a)).then(name_cmp)
                }
            }
        });

        let mut rows = Vec::new();
        for source_id in order {
            let Some(source) = self.sources.get(&source_id) else {
                continue;
            };
            let entries = groups.remove(&source_id).unwrap_or_default();
            let expanded = self.channels_expanded.contains(&source_id);
            let count = self
                .channel_stats
                .get(&source_id)
                .map(|stats| stats.total)
                .unwrap_or(entries.len());
            rows.push(ChannelRow::Header {
                source_id,
                name: source.name.clone(),
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
            ActiveView::Channels => format!("Channels \u{2014} {}", self.channel_sort.label()),
            ActiveView::Viewed => format!("Viewed (today: {})", self.viewed_today_count),
            ActiveView::Overflow => "Overflow".to_string(),
            ActiveView::Ignored => "Ignored".to_string(),
        }
    }

    fn next_refresh_at(&self) -> Option<DateTime<Utc>> {
        let interval_mins = self.config.general.poll_interval_mins as i64;
        self.last_poll_at
            .map(|t| t + chrono::Duration::minutes(interval_mins))
    }

    fn next_refresh_label(&self) -> String {
        let Some(next) = self.next_refresh_at() else {
            return "next refresh pending".to_string();
        };
        let now = Utc::now();
        let delta = next.signed_duration_since(now);
        let local = next.with_timezone(&Local);
        if delta.num_seconds() <= 0 {
            format!("next refresh due ({})", local.format("%H:%M"))
        } else if delta.num_minutes() < 60 {
            let mins = delta.num_minutes().max(1);
            format!("next refresh in {mins}m ({})", local.format("%H:%M"))
        } else {
            format!("next refresh at {}", local.format("%H:%M"))
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

    fn set_status(&mut self, text: impl Into<String>, kind: StatusKind) {
        self.status_message = Some(StatusMessage {
            text: text.into(),
            kind,
        });
    }

    fn set_info(&mut self, text: impl Into<String>) {
        self.set_status(text, StatusKind::Info);
    }

    fn set_error(&mut self, text: impl Into<String>) {
        self.set_status(text, StatusKind::Error);
    }
}

fn open_channels_manager(app: &mut App, storage: &Storage) -> Result<()> {
    let sources = storage.list_sources()?;
    app.overlay = Overlay::ChannelsManager(ChannelsManagerState {
        sources,
        selected: 0,
    });
    Ok(())
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
                                if let Err(error) = refresh_view(app, storage) {
                                    app.set_error(format!("Search refresh failed: {error:#}"));
                                }
                            }
                            needs_redraw = true;
                        }
                        KeyCode::Backspace => {
                            if app.pop_search_char() {
                                if let Err(error) = refresh_view(app, storage) {
                                    app.set_error(format!("Search refresh failed: {error:#}"));
                                }
                                needs_redraw = true;
                            }
                        }
                        KeyCode::Char(c) => {
                            app.push_search_char(c);
                            if let Err(error) = refresh_view(app, storage) {
                                app.set_error(format!("Search refresh failed: {error:#}"));
                            }
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
                                2 => settings.channel_sort = settings.channel_sort.cycle_next(),
                                3 => {
                                    settings.poll_interval =
                                        settings.poll_interval.saturating_add(5).min(1440)
                                }
                                4 => settings.show_shorts = !settings.show_shorts,
                                5 => settings.show_live = !settings.show_live,
                                6 => settings.show_premieres = !settings.show_premieres,
                                SettingsState::VIEW_IGNORED_FIELD => {
                                    app.overlay = Overlay::None;
                                    app.clear_search();
                                    app.set_view(ActiveView::Ignored);
                                    if let Err(error) = refresh_view(app, storage) {
                                        app.set_error(format!("Switching views failed: {error:#}"));
                                    }
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
                                2 => settings.channel_sort = settings.channel_sort.cycle_prev(),
                                3 => {
                                    settings.poll_interval =
                                        settings.poll_interval.saturating_sub(5).max(5)
                                }
                                4 => settings.show_shorts = !settings.show_shorts,
                                5 => settings.show_live = !settings.show_live,
                                6 => settings.show_premieres = !settings.show_premieres,
                                _ => {}
                            }
                            needs_redraw = true;
                        }
                        KeyCode::Enter => {
                            if settings.selected == SettingsState::VIEW_IGNORED_FIELD {
                                app.overlay = Overlay::None;
                                app.clear_search();
                                app.set_view(ActiveView::Ignored);
                                if let Err(error) = refresh_view(app, storage) {
                                    app.set_error(format!("Switching views failed: {error:#}"));
                                }
                            } else {
                                let settings = settings.clone();
                                app.config.general.default_top_n = settings.top_n;
                                app.config.ranking.mode = settings.ranking_mode;
                                app.config.general.channel_sort = settings.channel_sort;
                                app.channel_sort = settings.channel_sort;
                                app.config.general.poll_interval_mins = settings.poll_interval;
                                app.config.youtube.show_shorts = settings.show_shorts;
                                app.config.youtube.show_live = settings.show_live;
                                app.config.youtube.show_premieres = settings.show_premieres;
                                if let Err(error) = app.config.save() {
                                    app.set_error(format!("Saving config failed: {error:#}"));
                                }
                                app.overlay = Overlay::None;
                                if let Err(error) = refresh_view(app, storage) {
                                    app.set_error(format!("Refreshing view failed: {error:#}"));
                                }
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
                                if let Err(error) = app.config.save() {
                                    app.set_error(format!("Saving config failed: {error:#}"));
                                }
                            } else {
                                app.set_error("Top N must be a number between 1 and 50");
                            }
                            app.overlay = Overlay::None;
                            if let Err(error) = refresh_view(app, storage) {
                                app.set_error(format!("Refreshing view failed: {error:#}"));
                            }
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
                                match storage.delete_source(id) {
                                    Ok(()) => {
                                        match storage.list_sources() {
                                            Ok(sources) => {
                                                app.sources = sources
                                                    .iter()
                                                    .cloned()
                                                    .map(|s| (s.id, s))
                                                    .collect();
                                                state.sources = sources;
                                                if state.selected >= state.sources.len() {
                                                    state.selected =
                                                        state.sources.len().saturating_sub(1);
                                                }
                                            }
                                            Err(error) => app.set_error(format!(
                                                "Reloading channels failed: {error:#}"
                                            )),
                                        }
                                        if let Err(error) = refresh_view(app, storage) {
                                            app.set_error(format!(
                                                "Refreshing view failed: {error:#}"
                                            ));
                                        } else {
                                            app.set_info("Channel deleted");
                                        }
                                        needs_redraw = true;
                                    }
                                    Err(error) => {
                                        app.set_error(format!(
                                            "Deleting channel failed: {error:#}"
                                        ));
                                        needs_redraw = true;
                                    }
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
                            if let Err(error) = open_channels_manager(app, storage) {
                                app.overlay = Overlay::None;
                                app.set_error(format!("Loading channels failed: {error:#}"));
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
                            if url.is_empty() {
                                app.set_error("Enter a URL to add a channel");
                                needs_redraw = true;
                                continue;
                            }

                            match block_on_async(add_feed_with_storage(storage, &url, None)) {
                                Ok(outcome) => {
                                    if let Err(error) = trigger_refresh(app, storage) {
                                        app.set_error(format!("Refresh failed: {error:#}"));
                                    } else {
                                        let action = if outcome.already_tracked {
                                            "Refreshed"
                                        } else {
                                            "Added"
                                        };
                                        app.set_info(format!(
                                            "{action} {}: {} new ({} in feed)",
                                            outcome.source_name,
                                            outcome.new_entries,
                                            outcome.total_entries
                                        ));
                                    }
                                    if let Err(error) = open_channels_manager(app, storage) {
                                        app.overlay = Overlay::None;
                                        app.set_error(format!(
                                            "Loading channels failed: {error:#}"
                                        ));
                                    }
                                }
                                Err(error) => {
                                    app.set_error(format!("Adding channel failed: {error:#}"));
                                }
                            }
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
                            if let Err(error) = refresh_view(app, storage) {
                                app.set_error(format!("Refreshing view failed: {error:#}"));
                            }
                            needs_redraw = true;
                        }
                    }
                    KeyCode::Tab => {
                        let next = app.active_view.next();
                        app.clear_search();
                        app.set_view(next);
                        if let Err(error) = refresh_view(app, storage) {
                            app.set_error(format!("Switching views failed: {error:#}"));
                        }
                        needs_redraw = true;
                    }
                    KeyCode::BackTab => {
                        let prev = app.active_view.previous();
                        app.clear_search();
                        app.set_view(prev);
                        if let Err(error) = refresh_view(app, storage) {
                            app.set_error(format!("Switching views failed: {error:#}"));
                        }
                        needs_redraw = true;
                    }
                    KeyCode::Char('S') => {
                        app.overlay = Overlay::Settings(SettingsState::from_config(&app.config));
                        needs_redraw = true;
                    }
                    KeyCode::Char('C') => {
                        if let Err(error) = open_channels_manager(app, storage) {
                            app.set_error(format!("Loading channels failed: {error:#}"));
                        }
                        needs_redraw = true;
                    }
                    KeyCode::Char('n') => {
                        app.overlay =
                            Overlay::TopNInput(app.config.general.default_top_n.to_string());
                        needs_redraw = true;
                    }
                    KeyCode::Char('r') => {
                        if let Err(error) = trigger_refresh(app, storage) {
                            app.set_error(format!("Refresh failed: {error:#}"));
                        } else {
                            app.set_info("Refresh complete");
                        }
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
                            let id = entry.id;
                            let prev_rating = entry.rating;
                            if prev_rating != Some(rating) {
                                match storage.set_entry_rating(id, rating) {
                                    Ok(()) => {
                                        entry.rating = Some(rating);
                                        app.push_undo(UndoAction::SetRating { id, prev_rating });
                                    }
                                    Err(error) => {
                                        app.set_error(format!("Saving rating failed: {error:#}"));
                                    }
                                }
                                needs_redraw = true;
                            }
                        }
                    }
                    KeyCode::Char('s') => match toggle_star_for_selected(app, storage) {
                        Ok(Some((id, prev_state))) => {
                            app.push_undo(UndoAction::SetState { id, prev_state });
                            needs_redraw = true;
                        }
                        Ok(None) => {}
                        Err(error) => {
                            app.set_error(format!("Updating star failed: {error:#}"));
                            needs_redraw = true;
                        }
                    },
                    KeyCode::Char('i') => match toggle_ignore_for_selected(app, storage) {
                        Ok(Some((id, prev_state))) => {
                            app.push_undo(UndoAction::SetState { id, prev_state });
                            needs_redraw = true;
                        }
                        Ok(None) => {}
                        Err(error) => {
                            app.set_error(format!("Ignoring entry failed: {error:#}"));
                            needs_redraw = true;
                        }
                    },
                    KeyCode::Char('v') => match toggle_viewed_for_selected(app, storage) {
                        Ok(Some(action)) => {
                            app.push_undo(action);
                            needs_redraw = true;
                        }
                        Ok(None) => {}
                        Err(error) => {
                            app.set_error(format!("Updating viewed state failed: {error:#}"));
                            needs_redraw = true;
                        }
                    },
                    KeyCode::Char('u') => match apply_undo(app, storage) {
                        Ok(true) => needs_redraw = true,
                        Ok(false) => {}
                        Err(error) => {
                            app.set_error(format!("Undo failed: {error:#}"));
                            needs_redraw = true;
                        }
                    },
                    KeyCode::Enter => {
                        if let Some(source_id) = app.selected_channel_header_source() {
                            app.toggle_channel(source_id);
                            needs_redraw = true;
                        } else if let Some((entry_id, url, was_new)) =
                            app.selected_entry().map(|entry| {
                                (entry.id, entry.url.clone(), entry.state == EntryState::New)
                            })
                        {
                            if let Err(error) = open::that(&url) {
                                app.set_error(format!("Opening URL failed: {error}"));
                            } else if let Err(error) = storage.record_entry_view(entry_id) {
                                app.set_error(format!("Recording view failed: {error:#}"));
                            } else if let Ok(count) = storage.count_entries_viewed_today() {
                                app.viewed_today_count = count;
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
                                    if let Err(error) = refresh_view(app, storage) {
                                        app.set_error(format!("Refreshing view failed: {error:#}"));
                                    }
                                }
                            } else {
                                app.set_error("Counting viewed entries failed");
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
                    .then(
                        b.score
                            .partial_cmp(&a.score)
                            .unwrap_or(std::cmp::Ordering::Equal),
                    )
            });
            Ok(entries)
        }
        ActiveView::Viewed => Ok(storage.list_viewed_entries()?),
        ActiveView::Overflow => Ok(storage.list_overflow_entries()?),
        ActiveView::Ignored => Ok(storage.list_ignored_entries()?),
    }
}

fn toggle_ignore_for_selected(
    app: &mut App,
    storage: &mut Storage,
) -> Result<Option<(i64, EntryState)>> {
    let Some((entry_id, prev_state, next_state)) = app.selected_entry().and_then(|entry| {
        let next = match entry.state {
            EntryState::New => Some(EntryState::Ignored),
            EntryState::Ignored => Some(EntryState::New),
            EntryState::Viewed | EntryState::Starred => None,
        }?;
        Some((entry.id, entry.state, next))
    }) else {
        return Ok(None);
    };

    storage.set_entry_state(entry_id, next_state)?;
    if let Some(entry) = app.selected_entry_mut() {
        entry.state = next_state;
    }

    if !app.is_search_active()
        && matches!(
            app.active_view,
            ActiveView::Home | ActiveView::Channels | ActiveView::Overflow | ActiveView::Ignored
        )
    {
        refresh_view(app, storage)?;
    }

    Ok(Some((entry_id, prev_state)))
}

fn toggle_viewed_for_selected(app: &mut App, storage: &mut Storage) -> Result<Option<UndoAction>> {
    let Some((entry_id, prev_state)) = app.selected_entry().map(|entry| (entry.id, entry.state))
    else {
        return Ok(None);
    };

    let action = match prev_state {
        EntryState::New | EntryState::Ignored => {
            storage.record_entry_view(entry_id)?;
            app.viewed_today_count = storage.count_entries_viewed_today()?;
            if let Some(entry) = app.selected_entry_mut() {
                entry.state = EntryState::Viewed;
            }
            UndoAction::MarkedView {
                id: entry_id,
                prev_state,
            }
        }
        EntryState::Viewed => {
            storage.set_entry_state(entry_id, EntryState::New)?;
            storage.delete_daily_view_today(entry_id)?;
            app.viewed_today_count = storage.count_entries_viewed_today()?;
            if let Some(entry) = app.selected_entry_mut() {
                entry.state = EntryState::New;
            }
            UndoAction::UnmarkedView { id: entry_id }
        }
        EntryState::Starred => return Ok(None),
    };

    if !app.is_search_active()
        && matches!(
            app.active_view,
            ActiveView::Home
                | ActiveView::Channels
                | ActiveView::Overflow
                | ActiveView::Ignored
                | ActiveView::Viewed
        )
    {
        refresh_view(app, storage)?;
    }

    Ok(Some(action))
}

fn toggle_star_for_selected(
    app: &mut App,
    storage: &mut Storage,
) -> Result<Option<(i64, EntryState)>> {
    let Some((entry_id, prev_state, next_state)) = app.selected_entry().map(|entry| {
        let next_state = if entry.state == EntryState::Starred {
            EntryState::Viewed
        } else {
            EntryState::Starred
        };
        (entry.id, entry.state, next_state)
    }) else {
        return Ok(None);
    };

    storage.set_entry_state(entry_id, next_state)?;

    if app.active_view == ActiveView::Overflow && !app.is_search_active() {
        refresh_view(app, storage)?;
    } else if let Some(entry) = app.selected_entry_mut() {
        entry.state = next_state;
    }

    Ok(Some((entry_id, prev_state)))
}

fn apply_undo(app: &mut App, storage: &mut Storage) -> Result<bool> {
    let Some(action) = app.undo_stack.pop() else {
        return Ok(false);
    };

    match action {
        UndoAction::SetState { id, prev_state } => {
            storage.set_entry_state(id, prev_state)?;
        }
        UndoAction::SetRating { id, prev_rating } => match prev_rating {
            Some(r) => storage.set_entry_rating(id, r)?,
            None => storage.clear_entry_rating(id)?,
        },
        UndoAction::MarkedView { id, prev_state } => {
            storage.set_entry_state(id, prev_state)?;
            storage.delete_daily_view_today(id)?;
            app.viewed_today_count = storage.count_entries_viewed_today()?;
        }
        UndoAction::UnmarkedView { id } => {
            storage.record_entry_view(id)?;
            app.viewed_today_count = storage.count_entries_viewed_today()?;
        }
    }

    refresh_view(app, storage)?;
    Ok(true)
}

fn refresh_view(app: &mut App, storage: &Storage) -> Result<()> {
    let mut entries = load_entries_for_view(storage, app.active_view, app.search_query())?;
    filter_youtube_content(storage, &mut entries, &app.config)?;
    if app.active_view == ActiveView::Channels {
        app.channel_stats = storage.channel_stats()?;
    }
    let ids: Vec<i64> = entries.iter().map(|e| e.id).collect();
    app.entry_enrichments = storage.get_enrichments_for_entries(&ids)?;
    app.replace_entries(entries);
    app.last_poll_at = storage.last_poll_at().ok().flatten();
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

fn parse_youtube_shorts_expansion_at() -> DateTime<Utc> {
    static SHORTS_EXPANSION_AT: OnceLock<DateTime<Utc>> = OnceLock::new();

    *SHORTS_EXPANSION_AT.get_or_init(|| {
        DateTime::parse_from_rfc3339(YOUTUBE_SHORTS_2024_EXPANSION_AT)
            .expect("valid YouTube Shorts cutoff timestamp")
            .to_utc()
    })
}

fn parse_u64_enrichment(enrichments: &HashMap<String, String>, key: &str) -> Option<u64> {
    enrichments.get(key)?.parse().ok()
}

fn has_shorts_text_hint(entry: &Entry) -> bool {
    let title_lower = entry.title.to_lowercase();
    let summary_lower = entry.summary.as_deref().unwrap_or("").to_lowercase();
    let url_lower = entry.url.to_lowercase();

    title_lower.contains("#shorts")
        || summary_lower.contains("#shorts")
        || url_lower.contains("/shorts/")
}

fn has_square_or_vertical_player(enrichments: &HashMap<String, String>) -> bool {
    let Some(width) = parse_u64_enrichment(enrichments, YOUTUBE_EMBED_WIDTH_KEY) else {
        return false;
    };
    let Some(height) = parse_u64_enrichment(enrichments, YOUTUBE_EMBED_HEIGHT_KEY) else {
        return false;
    };

    height >= width
}

fn has_player_orientation(enrichments: &HashMap<String, String>) -> bool {
    parse_u64_enrichment(enrichments, YOUTUBE_EMBED_WIDTH_KEY).is_some()
        && parse_u64_enrichment(enrichments, YOUTUBE_EMBED_HEIGHT_KEY).is_some()
}

fn youtube_short_duration_limit(entry: &Entry) -> u64 {
    let Some(published_at) = entry.published_at else {
        return 60;
    };

    if published_at >= parse_youtube_shorts_expansion_at() {
        180
    } else {
        60
    }
}

fn is_probable_youtube_short(entry: &Entry, enrichments: &HashMap<String, String>) -> bool {
    if has_shorts_text_hint(entry) {
        return true;
    }

    let Some(duration) = enrichments.get(YOUTUBE_DURATION_KEY) else {
        return false;
    };
    let Some(seconds) = parse_iso8601_duration_seconds(duration) else {
        return false;
    };

    if !has_player_orientation(enrichments) {
        return seconds <= 60;
    }

    if !has_square_or_vertical_player(enrichments) {
        return false;
    }

    seconds <= youtube_short_duration_limit(entry)
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

        if !config.youtube.show_shorts && is_probable_youtube_short(entry, entry_enrichments) {
            return false;
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
        if (0x2600..=0x27BF).contains(&u)
            || (0x1F000..=0x1FAFF).contains(&u)
            || u == 0xFE0F
            || u == 0xFE0E
            || u == 0x200D
        {
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
        .constraints([
            Constraint::Min(0),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(outer);
    let main_area = main_and_bar[0];
    let status_area = main_and_bar[1];
    let bar_area = main_and_bar[2];

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
                        Span::styled(format!("  ({count})"), Style::default().fg(Color::DarkGray)),
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

                    let duration_label = entry_duration_label(entry, &app.entry_enrichments);
                    let title_span = if let Some(d) = duration_label {
                        Span::styled(format!("{} [{}]", truncated_title, d), title_style)
                    } else {
                        Span::styled(truncated_title, title_style)
                    };

                    let line = Line::from(vec![Span::raw("    "), star, title_span]);
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
                let max_title_width =
                    list_area.width.saturating_sub(6 + source_width as u16) as usize;
                let truncated_title = safe_truncate(&entry.title, max_title_width);

                let duration_label = entry_duration_label(entry, &app.entry_enrichments);
                let title_span = if let Some(d) = duration_label {
                    Span::styled(format!("{} [{}]", truncated_title, d), title_style)
                } else {
                    Span::styled(truncated_title, title_style)
                };

                let line = Line::from(vec![
                    star,
                    Span::styled(source_str, Style::default().fg(Color::Cyan)),
                    title_span,
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
    let refresh_title = Line::from(vec![
        Span::raw(" "),
        Span::styled(
            app.next_refresh_label(),
            Style::default().fg(Color::DarkGray),
        ),
        Span::raw(" "),
    ])
    .right_aligned();

    let items_list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title_top(list_title)
                .title_top(refresh_title),
        )
        .highlight_style(Style::default().bg(Color::DarkGray))
        .highlight_symbol(" > ");

    app.list_viewport_height = list_area.height.saturating_sub(2);
    f.render_stateful_widget(items_list, list_area, &mut app.state);

    let detail_title = Line::from(vec![
        Span::raw(" "),
        Span::styled("Detail", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" "),
    ]);
    let detail_block = Block::default().borders(Borders::ALL).title(detail_title);
    let detail_inner = detail_block.inner(detail_area);
    f.render_widget(detail_block, detail_area);

    let channel_header_source = app.selected_channel_header_source();
    let show_thumbnail_area = channel_header_source.is_none()
        && app.thumbnail_mode == ThumbnailMode::Viuer
        && app
            .selected_entry()
            .and_then(|e| e.thumbnail_url.as_ref())
            .is_some();

    if let Some(source_id) = channel_header_source {
        if detail_inner.width > 0 && detail_inner.height > 0 {
            let lines = build_channel_detail_lines(app, source_id);
            f.render_widget(
                Paragraph::new(lines).wrap(Wrap { trim: true }),
                detail_inner,
            );
        }
    } else if show_thumbnail_area {
        let (thumbnail_area, summary_area) = detail_sections(detail_inner);
        if thumbnail_area.width > 0 && thumbnail_area.height > 0 {
            let thumbnail_text =
                build_thumbnail_status_text(app.selected_entry(), app.selected_thumbnail_status());
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

    let key = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
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
        Span::styled("1-5", key),
        Span::styled(" rate ", dim),
        Span::styled("u", key),
        Span::styled("ndo ", dim),
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
    let status = match &app.status_message {
        Some(StatusMessage {
            text,
            kind: StatusKind::Error,
        }) => Paragraph::new(text.clone()).style(Style::default().fg(Color::LightRed)),
        Some(StatusMessage {
            text,
            kind: StatusKind::Info,
        }) => Paragraph::new(text.clone()).style(Style::default().fg(Color::DarkGray)),
        None => Paragraph::new(String::new()),
    };
    f.render_widget(status, status_area);
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
    if v {
        "yes"
    } else {
        "no"
    }
}

fn draw_settings_overlay(f: &mut ratatui::Frame, settings: &SettingsState, area: Rect) {
    let popup = centered_rect(54, 17, area);
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
        (
            "Ranking",
            format!("\u{25c2} {} \u{25b8}", settings.ranking_mode),
        ),
        (
            "Channel Sort",
            format!("\u{25c2} {} \u{25b8}", settings.channel_sort),
        ),
        (
            "Poll (min)",
            format!("\u{25c2} {} \u{25b8}", settings.poll_interval),
        ),
        (
            "Show Shorts",
            format!("[{}]", bool_display(settings.show_shorts)),
        ),
        (
            "Show Live",
            format!("[{}]", bool_display(settings.show_live)),
        ),
        (
            "Show Premieres",
            format!("[{}]", bool_display(settings.show_premieres)),
        ),
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
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };
        let value_style = if selected {
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD)
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
    let hint1 = Line::from(Span::styled("  j/k move   h/l change   Space toggle", dim));
    let hint2 = Line::from(Span::styled("  Enter save/open   Esc cancel", dim));
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
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(0),
        ])
        .split(inner);

    let input = Line::from(vec![
        Span::styled("  N = ", Style::default().fg(Color::Gray)),
        Span::styled(
            format!("{buf}_"),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
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

    let items: Vec<ListItem> = state
        .sources
        .iter()
        .map(|s| ListItem::new(s.name.clone()))
        .collect();

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

    let help = Span::styled(
        "  a: add  d: delete  esc: close",
        Style::default().fg(Color::DarkGray),
    );
    f.render_widget(Paragraph::new(help), sections[1]);
}

fn draw_add_channel_overlay(f: &mut ratatui::Frame, buf: &str, area: Rect) {
    let popup = centered_rect(60, 5, area);
    f.render_widget(ratatui::widgets::Clear, popup);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(Line::from(vec![
            Span::raw(" "),
            Span::styled(
                "Add Channel URL",
                Style::default().add_modifier(Modifier::BOLD),
            ),
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
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
    ]);

    f.render_widget(Paragraph::new(input), rows[0]);
    f.render_widget(
        Paragraph::new("  Enter: add  Esc: cancel  accepts channel/video/feed URLs")
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

fn build_thumbnail_status_text(entry: Option<&Entry>, status: Option<&ThumbnailStatus>) -> String {
    let Some(_entry) = entry else {
        return String::new();
    };

    match status {
        Some(ThumbnailStatus::Ready(_)) => String::new(),
        Some(ThumbnailStatus::Loading) | None => "Loading thumbnail...".to_string(),
        Some(ThumbnailStatus::Failed(_)) => String::new(),
    }
}

fn format_duration(seconds: u64) -> String {
    let h = seconds / 3600;
    let m = (seconds % 3600) / 60;
    let s = seconds % 60;
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

fn entry_duration_label(
    entry: &Entry,
    enrichments: &HashMap<i64, HashMap<String, String>>,
) -> Option<String> {
    enrichments
        .get(&entry.id)?
        .get(YOUTUBE_DURATION_KEY)
        .and_then(|raw| parse_iso8601_duration_seconds(raw))
        .map(format_duration)
}

fn format_compact_count(n: u64) -> String {
    if n < 1_000 {
        n.to_string()
    } else if n < 1_000_000 {
        let tenths = n / 100;
        if tenths.is_multiple_of(10) || tenths >= 100 {
            format!("{}K", tenths / 10)
        } else {
            format!("{}.{}K", tenths / 10, tenths % 10)
        }
    } else if n < 1_000_000_000 {
        let tenths = n / 100_000;
        if tenths.is_multiple_of(10) || tenths >= 100 {
            format!("{}M", tenths / 10)
        } else {
            format!("{}.{}M", tenths / 10, tenths % 10)
        }
    } else {
        let tenths = n / 100_000_000;
        if tenths.is_multiple_of(10) {
            format!("{}B", tenths / 10)
        } else {
            format!("{}.{}B", tenths / 10, tenths % 10)
        }
    }
}

fn build_channel_detail_lines(app: &App, source_id: i64) -> Vec<Line<'static>> {
    let dim = Style::default().fg(Color::DarkGray);
    let label = Style::default().fg(Color::Gray);
    let accent = Style::default().fg(Color::Cyan);
    let bold_white = Style::default()
        .fg(Color::White)
        .add_modifier(Modifier::BOLD);
    let default_stats = ChannelStats::default();
    let stats = app.channel_stats.get(&source_id).unwrap_or(&default_stats);

    let source = app.sources.get(&source_id);
    let name = source
        .map(|s| s.name.clone())
        .unwrap_or_else(|| "Unknown channel".to_string());
    let top_n = source
        .and_then(|s| s.top_n_override)
        .unwrap_or(app.config.general.default_top_n);
    let adapter = source
        .map(|s| match s.adapter {
            AdapterType::Rss => "RSS",
            AdapterType::Youtube => "YouTube",
        })
        .unwrap_or("?");
    let url = source.map(|s| s.url.clone()).unwrap_or_default();

    let mut lines: Vec<Line<'static>> = Vec::new();

    lines.push(Line::from(Span::styled(name, bold_white)));
    if !url.is_empty() {
        lines.push(Line::from(Span::styled(
            url,
            Style::default().fg(Color::Blue),
        )));
    }
    lines.push(Line::from(vec![
        Span::styled(adapter.to_string(), accent),
        Span::styled("  \u{00b7}  ", dim),
        Span::styled(format!("top {top_n}"), label),
    ]));

    lines.push(Line::from(""));

    lines.push(Line::from(Span::styled("Entries", label)));
    lines.push(Line::from(vec![
        Span::styled(format!("{}", stats.total), bold_white),
        Span::styled(" total", dim),
    ]));
    lines.push(Line::from(vec![
        Span::styled(
            format!("{}", stats.new_count),
            Style::default().fg(Color::White),
        ),
        Span::styled(" new  ", dim),
        Span::styled(format!("{}", stats.viewed_count), label),
        Span::styled(" viewed  ", dim),
        Span::styled(format!("{}", stats.ignored_count), label),
        Span::styled(" ignored  ", dim),
        Span::styled(
            format!("{}", stats.starred_count),
            Style::default().fg(Color::Yellow),
        ),
        Span::styled(" starred", dim),
    ]));

    if stats.rating_n > 0 {
        let avg = stats.rating_total as f64 / stats.rating_n as f64;
        let filled = avg.round().clamp(0.0, 5.0) as usize;
        let stars_filled = "\u{2605}".repeat(filled);
        let stars_empty = "\u{2606}".repeat(5 - filled);
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled("Avg rating  ", label),
            Span::styled(stars_filled, Style::default().fg(Color::Yellow)),
            Span::styled(stars_empty, dim),
            Span::styled(format!("  {avg:.1}  ({} rated)", stats.rating_n), dim),
        ]));
    }

    if let (Some(title), Some(at)) = (stats.latest_title.clone(), stats.latest_published) {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled("Latest entry", label)));
        lines.push(Line::from(Span::styled(title, bold_white)));
        lines.push(Line::from(Span::styled(
            at.with_timezone(&Local)
                .format("%b %d, %Y  %H:%M")
                .to_string(),
            dim,
        )));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        app.next_refresh_label(),
        Style::default().fg(Color::Cyan),
    )));
    lines.push(Line::from(Span::styled(
        "Ignored/viewed entries return after the next refresh.",
        dim,
    )));

    lines
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
        meta_spans.push(Span::styled(date.format("%b %d, %Y").to_string(), label));
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

    let duration = enrichments
        .and_then(|e| e.get(YOUTUBE_DURATION_KEY))
        .and_then(|raw| parse_iso8601_duration_seconds(raw))
        .map(format_duration);
    if let Some(d) = duration {
        if !meta_spans.is_empty() {
            meta_spans.push(Span::styled("  \u{00b7}  ", dim));
        }
        meta_spans.push(Span::styled(d, label));
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
        meta_spans.push(Span::styled(stars, Style::default().fg(Color::Yellow)));
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
                    "br" | "p" | "/p" | "div" | "/div" | "hr" | "/hr" | "tr" | "/tr" | "h1"
                    | "h2" | "h3" | "h4" | "h5" | "h6" | "/h1" | "/h2" | "/h3" | "/h4" | "/h5"
                    | "/h6" => {
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

fn block_on_async<F>(future: F) -> F::Output
where
    F: Future,
{
    tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(future))
}

fn youtube_feed_url(channel_id: &str) -> String {
    format!("https://www.youtube.com/feeds/videos.xml?channel_id={channel_id}")
}

fn extract_channel_id_after(haystack: &str, needle: &str) -> Option<String> {
    let idx = haystack.find(needle)?;
    let mut out = String::new();
    for ch in haystack[idx + needle.len()..].chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
            out.push(ch);
        } else {
            break;
        }
    }

    if out.starts_with("UC") {
        Some(out)
    } else {
        None
    }
}

fn extract_channel_id_from_youtube_html(html: &str) -> Option<String> {
    extract_channel_id_after(html, "https://www.youtube.com/feeds/videos.xml?channel_id=")
        .or_else(|| extract_channel_id_after(html, "\"channelId\":\""))
}

fn normalize_direct_youtube_url(url: &str) -> Option<String> {
    if opml::looks_like_youtube_feed(url) {
        return Some(url.to_string());
    }

    let parsed = reqwest::Url::parse(url).ok()?;
    let host = parsed.host_str()?.to_ascii_lowercase();
    if !host.contains("youtube.com") && host != "youtu.be" {
        return None;
    }

    if let Some(channel_id) = parsed
        .query_pairs()
        .find_map(|(key, value)| (key == "channel_id").then(|| value.into_owned()))
    {
        return Some(youtube_feed_url(&channel_id));
    }

    let mut segments = parsed.path_segments()?;
    let first = segments.next()?;
    let second = segments.next()?;
    if first == "channel" && second.starts_with("UC") {
        return Some(youtube_feed_url(second));
    }

    None
}

async fn normalize_source_url(url: &str) -> Result<String> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        anyhow::bail!("URL is empty");
    }

    if let Some(normalized) = normalize_direct_youtube_url(trimmed) {
        return Ok(normalized);
    }

    let parsed = reqwest::Url::parse(trimmed).context("URL is invalid")?;
    let host = parsed
        .host_str()
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    if !host.contains("youtube.com") && host != "youtu.be" {
        return Ok(trimmed.to_string());
    }

    let response = reqwest::Client::new()
        .get(parsed)
        .send()
        .await
        .context("resolving YouTube URL")?
        .error_for_status()
        .context("loading YouTube page")?;
    let html = response.text().await.context("reading YouTube page")?;
    let channel_id = extract_channel_id_from_youtube_html(&html).ok_or_else(|| {
        anyhow::anyhow!("could not find a YouTube channel for this URL; use a channel or feed URL")
    })?;

    Ok(youtube_feed_url(&channel_id))
}

async fn fetch_feed_for(
    kind: AdapterType,
    url: &str,
) -> Result<feedfold_core::adapter::FetchedFeed> {
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
    let url = normalize_source_url(url).await?;
    let kind = adapter_for_url(&url);
    let fetched = fetch_feed_for(kind, &url).await?;

    let name = override_name
        .map(str::to_owned)
        .or_else(|| fetched.name.clone())
        .unwrap_or_else(|| url.to_string());

    let (source_id, already_tracked, source_name) = match storage.source_by_url(&url)? {
        Some(existing) => (existing.id, true, existing.name),
        None => {
            let new = NewSource {
                name: name.clone(),
                url: url.clone(),
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

fn export_opml() -> Result<()> {
    let db_path = Storage::default_path().context("resolving database path")?;
    let storage = Storage::open(&db_path)
        .with_context(|| format!("opening database at {}", db_path.display()))?;
    let sources = storage.list_sources().context("listing sources")?;
    let feeds = sources
        .into_iter()
        .map(|source| opml::OpmlFeed {
            url: source.url,
            title: Some(source.name),
        })
        .collect::<Vec<_>>();

    print!("{}", opml::render(&feeds));
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

fn remove_source(id_or_url: &str, skip_confirm: bool) -> Result<()> {
    let db_path = Storage::default_path().context("resolving database path")?;
    let storage = Storage::open(&db_path)
        .with_context(|| format!("opening database at {}", db_path.display()))?;

    let trimmed = id_or_url.trim();
    let target = if let Ok(id) = trimmed.parse::<i64>() {
        storage
            .list_sources()
            .context("listing sources")?
            .into_iter()
            .find(|s| s.id == id)
    } else {
        storage
            .source_by_url(trimmed)
            .context("looking up source by url")?
    };

    let Some(source) = target else {
        anyhow::bail!(
            "No source matches {id_or_url:?}. Run `feedfold list` to see tracked sources."
        );
    };

    if !skip_confirm {
        print!(
            "Remove [{}] {} ({})? This deletes all stored entries. [y/N]: ",
            source.id, source.name, source.url
        );
        io::Write::flush(&mut io::stdout()).context("flushing stdout")?;
        let mut answer = String::new();
        io::stdin()
            .read_line(&mut answer)
            .context("reading confirmation")?;
        let trimmed = answer.trim().to_ascii_lowercase();
        if trimmed != "y" && trimmed != "yes" {
            println!("Aborted.");
            return Ok(());
        }
    }

    storage
        .delete_source(source.id)
        .context("deleting source")?;
    println!("Removed [{}] {} ({}).", source.id, source.name, source.url);
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

    fn sample_youtube_source() -> NewSource {
        NewSource {
            name: "Example Channel".to_string(),
            url: "https://www.youtube.com/feeds/videos.xml?channel_id=UC123".to_string(),
            adapter: AdapterType::Youtube,
            top_n_override: None,
        }
    }

    fn youtube_new_entry(
        source_id: i64,
        external_id: &str,
        title: &str,
        published_at: &str,
        duration: &str,
        embed_width: u64,
        embed_height: u64,
    ) -> NewEntry {
        let mut entry = sample_new_entry(source_id, external_id, title);
        entry.url = format!("https://www.youtube.com/watch?v={external_id}");
        entry.published_at = Some(DateTime::parse_from_rfc3339(published_at).unwrap().to_utc());
        entry
            .enrichments
            .insert(YOUTUBE_DURATION_KEY.to_string(), duration.to_string());
        entry
            .enrichments
            .insert(YOUTUBE_EMBED_WIDTH_KEY.to_string(), embed_width.to_string());
        entry.enrichments.insert(
            YOUTUBE_EMBED_HEIGHT_KEY.to_string(),
            embed_height.to_string(),
        );
        entry
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
    fn probable_youtube_short_uses_legacy_sixty_second_cutoff_before_october_2024() {
        let mut entry = sample_entry();
        entry.published_at = Some(Utc.with_ymd_and_hms(2024, 9, 1, 0, 0, 0).single().unwrap());

        let enrichments = HashMap::from([
            (YOUTUBE_DURATION_KEY.to_string(), "PT59S".to_string()),
            (YOUTUBE_EMBED_WIDTH_KEY.to_string(), "4608".to_string()),
            (YOUTUBE_EMBED_HEIGHT_KEY.to_string(), "8192".to_string()),
        ]);
        assert!(is_probable_youtube_short(&entry, &enrichments));

        let enrichments = HashMap::from([
            (YOUTUBE_DURATION_KEY.to_string(), "PT61S".to_string()),
            (YOUTUBE_EMBED_WIDTH_KEY.to_string(), "4608".to_string()),
            (YOUTUBE_EMBED_HEIGHT_KEY.to_string(), "8192".to_string()),
        ]);
        assert!(!is_probable_youtube_short(&entry, &enrichments));
    }

    #[test]
    fn probable_youtube_short_uses_three_minute_cutoff_after_october_2024() {
        let mut entry = sample_entry();
        entry.published_at = Some(
            Utc.with_ymd_and_hms(2024, 10, 16, 0, 0, 0)
                .single()
                .unwrap(),
        );

        let enrichments = HashMap::from([
            (YOUTUBE_DURATION_KEY.to_string(), "PT3M".to_string()),
            (YOUTUBE_EMBED_WIDTH_KEY.to_string(), "4608".to_string()),
            (YOUTUBE_EMBED_HEIGHT_KEY.to_string(), "8192".to_string()),
        ]);
        assert!(is_probable_youtube_short(&entry, &enrichments));

        let enrichments = HashMap::from([
            (YOUTUBE_DURATION_KEY.to_string(), "PT3M1S".to_string()),
            (YOUTUBE_EMBED_WIDTH_KEY.to_string(), "4608".to_string()),
            (YOUTUBE_EMBED_HEIGHT_KEY.to_string(), "8192".to_string()),
        ]);
        assert!(!is_probable_youtube_short(&entry, &enrichments));
    }

    #[test]
    fn probable_youtube_short_falls_back_to_sixty_seconds_when_orientation_is_missing() {
        let mut entry = sample_entry();
        entry.published_at = Some(Utc.with_ymd_and_hms(2025, 4, 1, 0, 0, 0).single().unwrap());

        let enrichments = HashMap::from([(YOUTUBE_DURATION_KEY.to_string(), "PT47S".to_string())]);
        assert!(is_probable_youtube_short(&entry, &enrichments));

        let enrichments =
            HashMap::from([(YOUTUBE_DURATION_KEY.to_string(), "PT1M15S".to_string())]);
        assert!(!is_probable_youtube_short(&entry, &enrichments));
    }

    #[test]
    fn probable_youtube_short_requires_square_or_vertical_player_without_text_hints() {
        let mut entry = sample_entry();
        entry.published_at = Some(Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).single().unwrap());
        entry.title = "Not labeled as a short".to_string();
        entry.summary = Some("A regular upload".to_string());
        entry.url = "https://www.youtube.com/watch?v=abc123".to_string();

        let enrichments = HashMap::from([
            (YOUTUBE_DURATION_KEY.to_string(), "PT45S".to_string()),
            (YOUTUBE_EMBED_WIDTH_KEY.to_string(), "8192".to_string()),
            (YOUTUBE_EMBED_HEIGHT_KEY.to_string(), "4608".to_string()),
        ]);
        assert!(!is_probable_youtube_short(&entry, &enrichments));
    }

    #[test]
    fn filter_youtube_content_hides_probable_shorts_without_touching_landscape_videos() {
        let mut storage = Storage::open_in_memory().unwrap();
        let source_id = storage.insert_source(&sample_youtube_source()).unwrap();

        storage
            .upsert_entries(&[
                youtube_new_entry(
                    source_id,
                    "short-vertical",
                    "Vertical short",
                    "2025-01-01T00:00:00Z",
                    "PT2M30S",
                    4608,
                    8192,
                ),
                youtube_new_entry(
                    source_id,
                    "landscape-short",
                    "Landscape clip",
                    "2025-01-01T00:00:00Z",
                    "PT45S",
                    8192,
                    4608,
                ),
            ])
            .unwrap();

        let mut entries = storage.list_entries_for_source(source_id).unwrap();
        let mut config = Config::default();
        config.youtube.show_shorts = false;
        config.youtube.show_live = true;
        config.youtube.show_premieres = true;

        filter_youtube_content(&storage, &mut entries, &config).unwrap();

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].external_id, "landscape-short");
    }

    #[test]
    fn rebuild_channel_rows_includes_all_sources() {
        let mut app = App::new(
            vec![sample_entry()],
            vec![
                DbSource {
                    id: 1,
                    name: "Example Feed".to_string(),
                    url: "https://example.com/feed.xml".to_string(),
                    adapter: AdapterType::Rss,
                    top_n_override: None,
                    created_at: Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).single().unwrap(),
                },
                DbSource {
                    id: 2,
                    name: "Branch Education".to_string(),
                    url: "https://www.youtube.com/feeds/videos.xml?channel_id=UC123".to_string(),
                    adapter: AdapterType::Youtube,
                    top_n_override: None,
                    created_at: Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).single().unwrap(),
                },
            ],
            Config::default(),
            ThumbnailMode::TextFallback,
            PathBuf::new(),
        );
        app.channel_stats.insert(
            2,
            ChannelStats {
                total: 50,
                new_count: 3,
                ..Default::default()
            },
        );

        app.rebuild_channel_rows();

        assert_eq!(app.channel_rows.len(), 2);
        assert!(app.channel_rows.iter().any(|row| matches!(
            row,
            ChannelRow::Header { name, count, .. }
                if name == "Branch Education" && *count == 50
        )));
    }

    #[test]
    fn normalize_direct_youtube_url_handles_channel_paths() {
        assert_eq!(
            normalize_direct_youtube_url("https://www.youtube.com/channel/UC123abc").as_deref(),
            Some("https://www.youtube.com/feeds/videos.xml?channel_id=UC123abc")
        );
    }

    #[test]
    fn extract_channel_id_from_youtube_html_finds_feed_links() {
        let html = r#"<link rel="alternate" type="application/rss+xml" href="https://www.youtube.com/feeds/videos.xml?channel_id=UCabc123_xyz">"#;
        assert_eq!(
            extract_channel_id_from_youtube_html(html).as_deref(),
            Some("UCabc123_xyz")
        );
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
        assert!(toggle_star_for_selected(&mut app, &mut storage)
            .unwrap()
            .is_some());
        assert!(app.entries.is_empty());

        let updated = storage
            .list_entries_for_source(source_id)
            .unwrap()
            .into_iter()
            .find(|item| item.id == entry.id)
            .unwrap();
        assert_eq!(updated.state, EntryState::Starred);
    }

    #[test]
    fn launch_agent_path_uses_home_library_launchagents() {
        let home = Path::new("/Users/alice");

        let path = launch_agent_path(home);

        assert_eq!(
            path,
            PathBuf::from("/Users/alice/Library/LaunchAgents/com.feedfold.feedfoldd.plist")
        );
    }

    #[test]
    fn daemon_binary_path_uses_feedfold_sibling_directory() {
        let current_exe = Path::new("/opt/feedfold/bin/feedfold");

        let path = daemon_binary_path(current_exe).unwrap();

        assert_eq!(path, PathBuf::from("/opt/feedfold/bin/feedfoldd"));
    }

    #[test]
    fn launchctl_service_target_uses_label_suffix() {
        let target = launchctl_service_target("gui/501");

        assert_eq!(target, "gui/501/com.feedfold.feedfoldd");
    }

    #[test]
    fn render_launchd_plist_embeds_escaped_paths() {
        let daemon = Path::new("/tmp/feed & fold/bin/feedfoldd");
        let workdir = Path::new("/tmp/feed <fold>/bin");

        let plist = render_launchd_plist(daemon, workdir);

        assert!(plist.contains("<string>com.feedfold.feedfoldd</string>"));
        assert!(plist.contains("/tmp/feed &amp; fold/bin/feedfoldd"));
        assert!(plist.contains("/tmp/feed &lt;fold&gt;/bin"));
        assert!(plist.contains("<key>RunAtLoad</key>"));
        assert!(plist.contains("<key>KeepAlive</key>"));
    }

    #[test]
    fn parse_launchctl_service_status_reads_state_and_pid() {
        let output = r#"gui/501/com.feedfold.feedfoldd = {
	state = running
	pid = 4242
}"#;

        let status = parse_launchctl_service_status(Some(0), output).unwrap();

        assert_eq!(
            status,
            LaunchctlServiceStatus::Loaded {
                state: Some("running".to_string()),
                pid: Some(4242),
            }
        );
    }

    #[test]
    fn parse_launchctl_service_status_marks_missing_service_as_unloaded() {
        let output = "Bad request.\nCould not find service \"com.feedfold.feedfoldd\" in domain for user gui: 501";

        let status = parse_launchctl_service_status(Some(113), output).unwrap();

        assert_eq!(status, LaunchctlServiceStatus::Unloaded);
    }

    #[test]
    fn parse_launchctl_service_status_rejects_other_launchctl_failures() {
        let error = parse_launchctl_service_status(Some(5), "permission denied").unwrap_err();

        assert!(error
            .to_string()
            .contains("`launchctl print` failed with exit code 5: permission denied"));
    }

    #[test]
    fn parse_launchctl_pid_accepts_plain_pid_output() {
        assert_eq!(parse_launchctl_pid("4242\n"), Some(4242));
    }
}
