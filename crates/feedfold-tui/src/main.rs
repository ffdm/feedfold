use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use feedfold_adapters::RssAdapter;
use feedfold_core::adapter::SourceAdapter;
use feedfold_core::storage::{NewEntry, NewSource, Storage};
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
        None => {
            println!("feedfold {VERSION}: TUI not yet implemented. Try `feedfold add <url>`.");
            Ok(())
        }
    }
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
