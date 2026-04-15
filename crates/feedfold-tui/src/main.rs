use anyhow::{anyhow, Context, Result};
use clap::{Parser, Subcommand};
use feed_rs::parser;
use feedfold_core::config::AdapterType;
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
    let bytes = reqwest::get(url)
        .await
        .with_context(|| format!("fetching {url}"))?
        .error_for_status()
        .with_context(|| format!("bad response from {url}"))?
        .bytes()
        .await
        .with_context(|| format!("reading body from {url}"))?;

    let feed = parser::parse(bytes.as_ref())
        .with_context(|| format!("parsing feed body from {url}"))?;

    let name = override_name
        .map(str::to_owned)
        .or_else(|| feed.title.as_ref().map(|t| t.content.clone()))
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
                adapter: AdapterType::Rss,
                top_n_override: None,
            };
            let id = storage.insert_source(&new)?;
            println!("Added source: {name} (id {id})");
            id
        }
    };

    let new_entries: Vec<NewEntry> = feed
        .entries
        .iter()
        .map(|e| entry_from_feed(source_id, e))
        .collect::<Result<Vec<_>>>()?;

    let inserted = storage.upsert_entries(&new_entries)?;
    println!(
        "Imported {inserted} new entries ({} total in feed).",
        new_entries.len()
    );

    Ok(())
}

fn entry_from_feed(source_id: i64, entry: &feed_rs::model::Entry) -> Result<NewEntry> {
    let url = entry
        .links
        .first()
        .map(|l| l.href.clone())
        .ok_or_else(|| anyhow!("entry {} has no link", entry.id))?;

    let title = entry
        .title
        .as_ref()
        .map(|t| t.content.clone())
        .unwrap_or_else(|| "(untitled)".to_string());

    let summary = entry.summary.as_ref().map(|t| t.content.clone());
    let author = entry.authors.first().map(|p| p.name.clone());
    let thumbnail_url = entry
        .media
        .iter()
        .flat_map(|m| m.thumbnails.iter())
        .next()
        .map(|t| t.image.uri.clone());

    Ok(NewEntry {
        source_id,
        external_id: entry.id.clone(),
        title,
        summary,
        url,
        thumbnail_url,
        author,
        published_at: entry.published.or(entry.updated),
    })
}
