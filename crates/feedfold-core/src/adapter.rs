use std::future::Future;

use chrono::{DateTime, Utc};
use thiserror::Error;

use crate::config::AdapterType;
use crate::storage::NewEntry;

#[derive(Debug, Clone)]
pub struct FetchedFeed {
    pub name: Option<String>,
    pub entries: Vec<FetchedEntry>,
}

#[derive(Debug, Clone)]
pub struct FetchedEntry {
    pub external_id: String,
    pub title: String,
    pub summary: Option<String>,
    pub url: String,
    pub thumbnail_url: Option<String>,
    pub author: Option<String>,
    pub published_at: Option<DateTime<Utc>>,
}

impl FetchedEntry {
    pub fn into_new_entry(self, source_id: i64) -> NewEntry {
        NewEntry {
            source_id,
            external_id: self.external_id,
            title: self.title,
            summary: self.summary,
            url: self.url,
            thumbnail_url: self.thumbnail_url,
            author: self.author,
            published_at: self.published_at,
        }
    }
}

#[derive(Debug, Error)]
pub enum AdapterError {
    #[error("fetching feed")]
    Fetch(#[source] Box<dyn std::error::Error + Send + Sync>),

    #[error("parsing feed")]
    Parse(#[source] Box<dyn std::error::Error + Send + Sync>),

    #[error("entry is missing a link")]
    MissingEntryUrl,

    #[error("invalid response: {0}")]
    InvalidResponse(String),
}

pub trait SourceAdapter: Send + Sync {
    fn kind(&self) -> AdapterType;

    fn fetch(
        &self,
        url: &str,
    ) -> impl Future<Output = Result<FetchedFeed, AdapterError>> + Send;
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeAdapter;

    impl SourceAdapter for FakeAdapter {
        fn kind(&self) -> AdapterType {
            AdapterType::Rss
        }

        async fn fetch(&self, url: &str) -> Result<FetchedFeed, AdapterError> {
            if url.is_empty() {
                return Err(AdapterError::InvalidResponse("empty url".into()));
            }
            Ok(FetchedFeed {
                name: Some("Fake".into()),
                entries: vec![FetchedEntry {
                    external_id: "fake-1".into(),
                    title: "Fake Entry".into(),
                    summary: None,
                    url: format!("{url}#1"),
                    thumbnail_url: None,
                    author: None,
                    published_at: None,
                }],
            })
        }
    }

    #[tokio::test]
    async fn fake_adapter_returns_fetched_feed() {
        let adapter = FakeAdapter;
        let feed = adapter.fetch("https://example.com/rss").await.unwrap();
        assert_eq!(feed.name.as_deref(), Some("Fake"));
        assert_eq!(feed.entries.len(), 1);
        assert_eq!(feed.entries[0].external_id, "fake-1");
        assert_eq!(adapter.kind(), AdapterType::Rss);
    }

    #[tokio::test]
    async fn fake_adapter_reports_invalid_response() {
        let adapter = FakeAdapter;
        let err = adapter.fetch("").await.unwrap_err();
        assert!(matches!(err, AdapterError::InvalidResponse(_)));
    }

    #[test]
    fn fetched_entry_into_new_entry_keeps_fields() {
        let fe = FetchedEntry {
            external_id: "abc".into(),
            title: "T".into(),
            summary: Some("S".into()),
            url: "https://example.com/x".into(),
            thumbnail_url: None,
            author: Some("A".into()),
            published_at: None,
        };
        let ne = fe.into_new_entry(42);
        assert_eq!(ne.source_id, 42);
        assert_eq!(ne.external_id, "abc");
        assert_eq!(ne.title, "T");
        assert_eq!(ne.summary.as_deref(), Some("S"));
        assert_eq!(ne.url, "https://example.com/x");
        assert_eq!(ne.author.as_deref(), Some("A"));
    }
}
