use std::time::Duration;

use anyhow::{Context, Result};
use feedfold_adapters::RssAdapter;
use feedfold_core::adapter::SourceAdapter;
use feedfold_core::config::{AdapterType, Config};
use feedfold_core::ranker::{RankContext, Ranker, RecencyRanker};
use feedfold_core::storage::Storage;
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

    let adapter = RssAdapter::new();
    let ranker = RecencyRanker;
    let default_top_n = config.general.default_top_n;

    poll_all(&mut storage, &adapter, &ranker, default_top_n).await;

    let mut ticker = tokio::time::interval(interval);
    ticker.tick().await;

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                poll_all(&mut storage, &adapter, &ranker, default_top_n).await;
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
    adapter: &RssAdapter,
    ranker: &impl Ranker,
    default_top_n: u32,
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
        if source.adapter != AdapterType::Rss {
            warn!(
                "Skipping {:?} adapter for {}, not yet supported",
                source.adapter, source.name
            );
            continue;
        }

        match adapter.fetch(&source.url).await {
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

        let top_n = source.top_n_override.unwrap_or(default_top_n) as usize;
        match storage.list_entries_for_source(source.id) {
            Ok(entries) => {
                let scores = ranker.rank(&entries, &RankContext { top_n });
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
