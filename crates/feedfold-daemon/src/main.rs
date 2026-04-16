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

const CLAUDE_RATING_HISTORY_LIMIT: usize = 20;

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

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".parse().unwrap()),
        )
        .init();

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
        "feedfoldd {} starting (poll every {poll_mins}m)",
        feedfold_core::VERSION
    );

    let db_path = Storage::default_path().context("resolving database path")?;
    let mut storage = Storage::open(&db_path)
        .with_context(|| format!("opening database at {}", db_path.display()))?;
    info!("Database: {}", db_path.display());

    let rss_adapter = RssAdapter::new();
    let youtube_adapter = std::env::var("YOUTUBE_API_KEY")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(YoutubeAdapter::with_api_key)
        .unwrap_or_default();
    let rankers = RuntimeRankers::from_env(&config);
    poll_all(&mut storage, &rss_adapter, &youtube_adapter, &rankers, &config).await;

    let mut ticker = tokio::time::interval(interval);
    ticker.tick().await;

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                poll_all(&mut storage, &rss_adapter, &youtube_adapter, &rankers, &config).await;
            }
            _ = tokio::signal::ctrl_c() => {
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
                let scores = rank_entries(
                    storage,
                    &entries,
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
        RankingMode::Claude => match rank_entries_with_claude(storage, entries, top_n, claude_ranker).await {
            Ok(scores) => scores,
            Err(e) => {
                warn!("{source_name}: Claude ranking unavailable, using recency: {e}");
                RecencyRanker.rank(entries, &ctx)
            }
        },
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

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
        let older_entry = entries.iter().find(|entry| entry.external_id == "old").unwrap();
        let newer_entry = entries.iter().find(|entry| entry.external_id == "new").unwrap();
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
        let newer_entry = entries.iter().find(|entry| entry.external_id == "new").unwrap();

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
}
