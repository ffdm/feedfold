use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use feedfold_adapters::{ClaudeRanker, RssAdapter, YoutubeAdapter};
use feedfold_core::adapter::SourceAdapter;
use feedfold_core::config::{AdapterType, Config, RankingMode};
use feedfold_core::ranker::{
    EntryEnrichments, PopularityRanker, RankContext, Ranker, RecencyRanker, Score,
};
use feedfold_core::storage::{Entry, Storage};
use tracing::{error, info, warn};
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::prelude::*;

const CLAUDE_RATING_HISTORY_LIMIT: usize = 20;
pub const DAEMON_PID_FILE: &str = "feedfoldd.pid";
const DAEMON_LOG_FILE: &str = "feedfoldd.log";
const MAX_LOG_BYTES: u64 = 1_048_576;

struct RuntimeRankers {
    claude: Option<ClaudeRanker>,
}

impl RuntimeRankers {
    fn from_env(config: &Config) -> Self {
        let claude = std::env::var("ANTHROPIC_API_KEY")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .map(|api_key| ClaudeRanker::from_config(api_key, config));

        Self { claude }
    }
}

#[derive(Clone)]
struct RotatingLogWriter {
    inner: Arc<Mutex<RotatingLogState>>,
}

struct RotatingLogState {
    path: PathBuf,
    max_bytes: u64,
    file: File,
}

struct RotatingLogHandle {
    inner: Arc<Mutex<RotatingLogState>>,
}

struct PidFileGuard {
    path: PathBuf,
}

impl RotatingLogWriter {
    fn new(path: PathBuf, max_bytes: u64) -> io::Result<Self> {
        let file = open_log_file(&path)?;
        let mut state = RotatingLogState {
            path,
            max_bytes,
            file,
        };
        state.rotate_if_needed(0)?;
        Ok(Self {
            inner: Arc::new(Mutex::new(state)),
        })
    }
}

impl RotatingLogState {
    fn rotate_if_needed(&mut self, next_write_len: usize) -> io::Result<()> {
        let current_len = self.file.metadata()?.len();
        if current_len.saturating_add(next_write_len as u64) <= self.max_bytes {
            return Ok(());
        }

        self.file.flush()?;
        let rotated_path = rotated_log_path(&self.path);
        match fs::remove_file(&rotated_path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        match fs::rename(&self.path, &rotated_path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        self.file = open_log_file(&self.path)?;
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for RotatingLogWriter {
    type Writer = RotatingLogHandle;

    fn make_writer(&'a self) -> Self::Writer {
        RotatingLogHandle {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl Write for RotatingLogHandle {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut state = self
            .inner
            .lock()
            .map_err(|_| io::Error::other("log writer lock poisoned"))?;
        state.rotate_if_needed(buf.len())?;
        state.file.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        let mut state = self
            .inner
            .lock()
            .map_err(|_| io::Error::other("log writer lock poisoned"))?;
        state.file.flush()
    }
}

impl PidFileGuard {
    fn create(path: PathBuf) -> io::Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&path, format!("{}\n", std::process::id()))?;
        Ok(Self { path })
    }
}

impl Drop for PidFileGuard {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_file(&self.path) {
            if error.kind() != io::ErrorKind::NotFound {
                eprintln!(
                    "warning: failed to remove pid file at {}: {error}",
                    self.path.display()
                );
            }
        }
    }
}

pub async fn run(process_name: &str) -> Result<()> {
    let state_dir = Storage::data_dir().context("resolving daemon state directory")?;
    let log_path = state_dir.join(DAEMON_LOG_FILE);
    init_logging(&log_path).with_context(|| format!("opening log file {}", log_path.display()))?;
    let _pid_file =
        PidFileGuard::create(state_dir.join(DAEMON_PID_FILE)).context("writing daemon pid file")?;

    if let Some(path) = Config::bootstrap_if_missing().context("bootstrapping config")? {
        info!(
            "Created config at {}. Edit it to set feeds, polling, and API keys.",
            path.display()
        );
    }

    let config = match Config::load() {
        Ok(c) => c,
        Err(feedfold_core::config::ConfigError::NotFound(path)) => {
            info!("No config file at {}, using defaults", path.display());
            Config::default()
        }
        Err(e) => {
            return Err(anyhow::anyhow!(e).context("loading config"));
        }
    };

    let poll_mins = config.general.poll_interval_mins;
    let interval = Duration::from_secs(u64::from(poll_mins) * 60);

    info!(
        "{process_name} {} starting (poll every {poll_mins}m)",
        feedfold_core::VERSION
    );
    info!("Log file: {}", log_path.display());

    let db_path = Storage::default_path().context("resolving database path")?;
    let mut storage = Storage::open(&db_path)
        .with_context(|| format!("opening database at {}", db_path.display()))?;
    info!("Database: {}", db_path.display());

    let rss_adapter = RssAdapter::new();
    let youtube_adapter = config
        .youtube_api_key()
        .map(YoutubeAdapter::with_api_key)
        .unwrap_or_default();
    let rankers = RuntimeRankers::from_env(&config);
    poll_all(
        &mut storage,
        &rss_adapter,
        &youtube_adapter,
        &rankers,
        &config,
    )
    .await;

    let mut ticker = tokio::time::interval(interval);
    ticker.tick().await;
    let shutdown_signal = wait_for_shutdown_signal();
    tokio::pin!(shutdown_signal);

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                poll_all(&mut storage, &rss_adapter, &youtube_adapter, &rankers, &config).await;
            }
            result = &mut shutdown_signal => {
                result?;
                info!("Shutting down");
                break;
            }
        }
    }

    Ok(())
}

async fn poll_all(
    storage: &mut Storage,
    rss_adapter: &RssAdapter,
    youtube_adapter: &YoutubeAdapter,
    rankers: &RuntimeRankers,
    config: &Config,
) {
    let sources = match storage.list_sources() {
        Ok(s) => s,
        Err(e) => {
            error!("Failed to list sources: {e}");
            return;
        }
    };

    if sources.is_empty() {
        info!("No sources to poll");
        return;
    }

    info!("Polling {} source(s)", sources.len());

    if let Err(e) = storage.set_last_poll_at(chrono::Utc::now()) {
        warn!("Failed to record last poll time: {e}");
    }

    for source in &sources {
        let fetch_result = match source.adapter {
            AdapterType::Rss => rss_adapter.fetch(&source.url).await,
            AdapterType::Youtube => youtube_adapter.fetch(&source.url).await,
        };

        match fetch_result {
            Ok(fetched) => {
                let new_entries: Vec<_> = fetched
                    .entries
                    .into_iter()
                    .map(|fe| fe.into_new_entry(source.id))
                    .collect();
                let total = new_entries.len();
                match storage.upsert_entries(&new_entries) {
                    Ok(inserted) => {
                        info!("{}: {inserted} new ({total} in feed)", source.name);
                    }
                    Err(e) => {
                        error!("{}: upsert failed: {e}", source.name);
                        continue;
                    }
                }
            }
            Err(e) => {
                error!("{}: fetch failed: {e}", source.name);
                continue;
            }
        }

        let top_n = source
            .top_n_override
            .unwrap_or(config.general.default_top_n) as usize;
        let ranking_mode = configured_ranking_mode(config, &source.url);
        match storage.list_entries_for_source(source.id) {
            Ok(entries) => {
                let enrichments = match storage.list_enrichments_for_source(source.id) {
                    Ok(enrichments) => enrichments,
                    Err(e) => {
                        error!(
                            "{}: failed to load enrichments for ranking: {e}",
                            source.name
                        );
                        continue;
                    }
                };
                let candidates: Vec<Entry> = entries
                    .into_iter()
                    .filter(|e| {
                        matches!(
                            e.state,
                            feedfold_core::storage::EntryState::New
                                | feedfold_core::storage::EntryState::Starred
                        )
                    })
                    .collect();
                let scores = rank_entries(
                    storage,
                    &candidates,
                    top_n,
                    enrichments,
                    ranking_mode,
                    rankers.claude.as_ref(),
                    &source.name,
                )
                .await;
                if let Err(e) = storage.apply_ranking(source.id, &scores, top_n) {
                    error!("{}: ranking update failed: {e}", source.name);
                }
            }
            Err(e) => {
                error!("{}: failed to load entries for ranking: {e}", source.name);
            }
        }
    }
}

fn configured_ranking_mode(config: &Config, source_url: &str) -> RankingMode {
    config
        .sources
        .iter()
        .find(|source| source.url == source_url)
        .and_then(|source| source.ranking)
        .unwrap_or(config.ranking.mode)
}

async fn rank_entries(
    storage: &Storage,
    entries: &[Entry],
    top_n: usize,
    enrichments: EntryEnrichments,
    mode: RankingMode,
    claude_ranker: Option<&ClaudeRanker>,
    source_name: &str,
) -> Vec<Score> {
    let ctx = RankContext { top_n, enrichments };
    match mode {
        RankingMode::Recency => RecencyRanker.rank(entries, &ctx),
        RankingMode::Popularity => PopularityRanker.rank(entries, &ctx),
        RankingMode::Claude => {
            match rank_entries_with_claude(storage, entries, top_n, claude_ranker).await {
                Ok(scores) => scores,
                Err(e) => {
                    warn!("{source_name}: Claude ranking unavailable, using recency: {e}");
                    RecencyRanker.rank(entries, &ctx)
                }
            }
        }
    }
}

async fn rank_entries_with_claude(
    storage: &Storage,
    entries: &[Entry],
    top_n: usize,
    claude_ranker: Option<&ClaudeRanker>,
) -> Result<Vec<Score>> {
    let claude_ranker = claude_ranker.context("ANTHROPIC_API_KEY is not set")?;
    let rating_history = storage
        .list_rated_entries(CLAUDE_RATING_HISTORY_LIMIT)
        .context("loading rated entries for Claude ranking")?;

    claude_ranker
        .rank(entries, top_n, &rating_history)
        .await
        .context("requesting Claude ranking")
}

fn init_logging(log_path: &Path) -> Result<()> {
    let writer = RotatingLogWriter::new(log_path.to_path_buf(), MAX_LOG_BYTES)
        .with_context(|| format!("opening rotating log file {}", log_path.display()))?;
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "info".parse().unwrap());

    tracing_subscriber::registry()
        .with(env_filter)
        .with(tracing_subscriber::fmt::layer().with_writer(io::stderr))
        .with(
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_writer(writer),
        )
        .init();

    Ok(())
}

fn open_log_file(path: &Path) -> io::Result<File> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    OpenOptions::new().create(true).append(true).open(path)
}

fn rotated_log_path(path: &Path) -> PathBuf {
    path.with_extension("log.1")
}

async fn wait_for_shutdown_signal() -> Result<()> {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .context("installing SIGTERM handler")?;

        tokio::select! {
            _ = tokio::signal::ctrl_c() => Ok(()),
            _ = terminate.recv() => Ok(()),
        }
    }

    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c()
            .await
            .context("waiting for shutdown signal")
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::time::{SystemTime, UNIX_EPOCH};

    use chrono::{TimeZone, Utc};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::*;
    use feedfold_core::config::AdapterType;
    use feedfold_core::storage::{NewEntry, NewSource};

    fn parse_config(raw: &str) -> Config {
        Config::parse(raw).expect("config parses")
    }

    #[test]
    fn configured_ranking_mode_uses_global_default() {
        let config = parse_config(
            r#"
[ranking]
mode = "popularity"
"#,
        );

        assert_eq!(
            configured_ranking_mode(&config, "https://example.com/feed.xml"),
            RankingMode::Popularity
        );
    }

    #[test]
    fn configured_ranking_mode_prefers_source_override() {
        let config = parse_config(
            r#"
[ranking]
mode = "recency"

[[sources]]
name = "Videos"
url = "https://example.com/feed.xml"
adapter = "youtube"
ranking = "popularity"
"#,
        );

        assert_eq!(
            configured_ranking_mode(&config, "https://example.com/feed.xml"),
            RankingMode::Popularity
        );
    }

    #[tokio::test]
    async fn rank_entries_uses_claude_when_configured() {
        let (storage, entries) = sample_storage_with_entries();
        let older_entry = entries
            .iter()
            .find(|entry| entry.external_id == "old")
            .unwrap();
        let newer_entry = entries
            .iter()
            .find(|entry| entry.external_id == "new")
            .unwrap();
        let response_body = format!(
            "{{\"content\":[{{\"type\":\"text\",\"text\":\"{{\\\"ranked_entry_ids\\\":[{},{}]}}\"}}]}}",
            older_entry.id, newer_entry.id
        );
        let server_url = spawn_test_server(response_body).await;
        let claude_ranker = ClaudeRanker::new("test-key").with_api_url(server_url);

        let scores = rank_entries(
            &storage,
            &entries,
            1,
            HashMap::new(),
            RankingMode::Claude,
            Some(&claude_ranker),
            "Example",
        )
        .await;

        assert_eq!(scores[0].entry_id, older_entry.id);
    }

    #[tokio::test]
    async fn rank_entries_falls_back_to_recency_when_claude_is_unavailable() {
        let (storage, entries) = sample_storage_with_entries();
        let newer_entry = entries
            .iter()
            .find(|entry| entry.external_id == "new")
            .unwrap();

        let scores = rank_entries(
            &storage,
            &entries,
            1,
            HashMap::new(),
            RankingMode::Claude,
            None,
            "Example",
        )
        .await;

        assert_eq!(scores[0].entry_id, newer_entry.id);
    }

    fn sample_storage_with_entries() -> (Storage, Vec<Entry>) {
        let mut storage = Storage::open_in_memory().unwrap();
        let source_id = storage
            .insert_source(&NewSource {
                name: "Example".into(),
                url: "https://example.com/feed.xml".into(),
                adapter: AdapterType::Rss,
                top_n_override: None,
            })
            .unwrap();

        storage
            .upsert_entries(&[
                NewEntry {
                    source_id,
                    external_id: "old".into(),
                    title: "Old".into(),
                    summary: None,
                    url: "https://example.com/old".into(),
                    thumbnail_url: None,
                    author: None,
                    published_at: Some(Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap()),
                    enrichments: HashMap::new(),
                },
                NewEntry {
                    source_id,
                    external_id: "new".into(),
                    title: "New".into(),
                    summary: None,
                    url: "https://example.com/new".into(),
                    thumbnail_url: None,
                    author: None,
                    published_at: Some(Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap()),
                    enrichments: HashMap::new(),
                },
            ])
            .unwrap();

        let entries = storage.list_entries_for_source(source_id).unwrap();
        (storage, entries)
    }

    async fn spawn_test_server(response_body: String) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buffer = [0_u8; 4096];
            let _ = stream.read(&mut buffer).await.unwrap();

            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                response_body.len(),
                response_body
            );
            stream.write_all(response.as_bytes()).await.unwrap();
        });

        format!("http://{address}")
    }

    #[test]
    fn rotated_log_path_adds_numeric_suffix() {
        let path = Path::new("/tmp/feedfoldd.log");

        assert_eq!(
            rotated_log_path(path),
            PathBuf::from("/tmp/feedfoldd.log.1")
        );
    }

    #[test]
    fn rotating_log_writer_moves_large_file_to_backup() {
        let temp_dir = unique_test_dir("rotating-log-writer");
        fs::create_dir_all(&temp_dir).unwrap();
        let log_path = temp_dir.join("feedfoldd.log");
        fs::write(&log_path, vec![b'x'; MAX_LOG_BYTES as usize]).unwrap();

        let writer = RotatingLogWriter::new(log_path.clone(), MAX_LOG_BYTES).unwrap();
        let mut handle = writer.make_writer();
        handle.write_all(b"hello\n").unwrap();
        handle.flush().unwrap();

        let rotated = rotated_log_path(&log_path);
        assert!(rotated.exists());
        assert_eq!(fs::metadata(&rotated).unwrap().len(), MAX_LOG_BYTES);
        assert_eq!(fs::read_to_string(&log_path).unwrap(), "hello\n");

        fs::remove_dir_all(temp_dir).unwrap();
    }

    #[test]
    fn pid_file_guard_removes_file_on_drop() {
        let temp_dir = unique_test_dir("pid-file-guard");
        let pid_path = temp_dir.join(DAEMON_PID_FILE);

        {
            let _guard = PidFileGuard::create(pid_path.clone()).unwrap();
            assert_eq!(
                fs::read_to_string(&pid_path).unwrap(),
                format!("{}\n", std::process::id())
            );
        }

        assert!(!pid_path.exists());
    }

    fn unique_test_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("feedfold-{label}-{}-{nanos}", std::process::id()))
    }
}
