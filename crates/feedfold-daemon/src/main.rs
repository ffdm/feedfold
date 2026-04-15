use std::time::Duration;

use anyhow::{Context, Result};
use feedfold_adapters::{RssAdapter, YoutubeAdapter};
use feedfold_core::adapter::SourceAdapter;
use feedfold_core::config::{AdapterType, Config, RankingMode};
use feedfold_core::ranker::{
    EntryEnrichments, PopularityRanker, RankContext, Ranker, RecencyRanker, Score,
};
use feedfold_core::storage::{Entry, Storage};
use tracing::{error, info, warn};

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
        Err(_) => {
            info!("No config file found, using defaults");
            Config::default()
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
    poll_all(&mut storage, &rss_adapter, &youtube_adapter, &config).await;

    let mut ticker = tokio::time::interval(interval);
    ticker.tick().await;

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                poll_all(&mut storage, &rss_adapter, &youtube_adapter, &config).await;
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
        let ranking_mode = effective_ranking_mode(config, &source.name, &source.url);
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
                let scores = rank_entries(&entries, top_n, enrichments, ranking_mode);
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

fn effective_ranking_mode(config: &Config, source_name: &str, source_url: &str) -> RankingMode {
    match configured_ranking_mode(config, source_url) {
        RankingMode::Claude => {
            warn!("{source_name}: ranking mode \"claude\" is not implemented yet, using recency");
            RankingMode::Recency
        }
        mode => mode,
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

fn rank_entries(
    entries: &[Entry],
    top_n: usize,
    enrichments: EntryEnrichments,
    mode: RankingMode,
) -> Vec<Score> {
    let ctx = RankContext { top_n, enrichments };
    match mode {
        RankingMode::Recency => RecencyRanker.rank(entries, &ctx),
        RankingMode::Popularity => PopularityRanker.rank(entries, &ctx),
        RankingMode::Claude => unreachable!("claude mode should be resolved before ranking"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn effective_ranking_mode_falls_back_from_claude() {
        let config = parse_config(
            r#"
[ranking]
mode = "claude"
"#,
        );

        assert_eq!(
            effective_ranking_mode(&config, "Example", "https://example.com/feed.xml"),
            RankingMode::Recency
        );
    }
}
