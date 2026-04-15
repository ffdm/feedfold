use std::collections::HashMap;

use feed_rs::parser;
use feedfold_core::adapter::{AdapterError, FetchedEntry, FetchedFeed, SourceAdapter};
use feedfold_core::config::AdapterType;
use reqwest::Client;

pub struct RssAdapter {
    client: Client,
}

impl RssAdapter {
    pub fn new() -> Self {
        Self {
            client: Client::new(),
        }
    }

    pub fn with_client(client: Client) -> Self {
        Self { client }
    }
}

impl Default for RssAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl SourceAdapter for RssAdapter {
    fn kind(&self) -> AdapterType {
        AdapterType::Rss
    }

    async fn fetch(&self, url: &str) -> Result<FetchedFeed, AdapterError> {
        let response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|e| AdapterError::Fetch(Box::new(e)))?;

        let response = response
            .error_for_status()
            .map_err(|e| AdapterError::Fetch(Box::new(e)))?;

        let bytes = response
            .bytes()
            .await
            .map_err(|e| AdapterError::Fetch(Box::new(e)))?;

        let feed =
            parser::parse(bytes.as_ref()).map_err(|e| AdapterError::Parse(Box::new(e)))?;

        let name = feed.title.as_ref().map(|t| t.content.clone());

        let entries = feed
            .entries
            .into_iter()
            .map(convert_entry)
            .collect::<Result<Vec<_>, _>>()?;

        Ok(FetchedFeed { name, entries })
    }
}

fn convert_entry(entry: feed_rs::model::Entry) -> Result<FetchedEntry, AdapterError> {
    let url = entry
        .links
        .first()
        .map(|l| l.href.clone())
        .ok_or(AdapterError::MissingEntryUrl)?;

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

    Ok(FetchedEntry {
        external_id: entry.id,
        title,
        summary,
        url,
        thumbnail_url,
        author,
        published_at: entry.published.or(entry.updated),
        enrichments: HashMap::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_ATOM: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Example Feed</title>
  <id>urn:example:feed</id>
  <updated>2026-04-01T12:00:00Z</updated>
  <entry>
    <title>First Post</title>
    <id>urn:example:entry:1</id>
    <link href="https://example.com/first"/>
    <updated>2026-04-01T12:00:00Z</updated>
    <published>2026-04-01T12:00:00Z</published>
    <author><name>Jane Doe</name></author>
    <summary>A short summary.</summary>
  </entry>
  <entry>
    <title>Second Post</title>
    <id>urn:example:entry:2</id>
    <link href="https://example.com/second"/>
    <updated>2026-04-02T09:30:00Z</updated>
  </entry>
</feed>"#;

    #[test]
    fn convert_entry_pulls_expected_fields() {
        let feed = parser::parse(SAMPLE_ATOM.as_bytes()).unwrap();
        let mut entries = feed.entries.into_iter();

        let first = convert_entry(entries.next().unwrap()).unwrap();
        assert_eq!(first.external_id, "urn:example:entry:1");
        assert_eq!(first.title, "First Post");
        assert_eq!(first.url, "https://example.com/first");
        assert_eq!(first.author.as_deref(), Some("Jane Doe"));
        assert_eq!(first.summary.as_deref(), Some("A short summary."));
        assert!(first.published_at.is_some());

        let second = convert_entry(entries.next().unwrap()).unwrap();
        assert_eq!(second.external_id, "urn:example:entry:2");
        assert_eq!(second.title, "Second Post");
        assert!(second.author.is_none());
        assert!(second.summary.is_none());
        assert!(second.published_at.is_some());
    }

    #[test]
    fn adapter_reports_kind_as_rss() {
        let adapter = RssAdapter::new();
        assert_eq!(adapter.kind(), AdapterType::Rss);
    }
}
