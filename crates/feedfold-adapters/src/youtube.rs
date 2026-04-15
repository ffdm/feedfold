use std::collections::{HashMap, HashSet};

use feedfold_core::adapter::{AdapterError, FetchedEntry, FetchedFeed, SourceAdapter};
use feedfold_core::config::AdapterType;
use reqwest::Client;
use serde::Deserialize;

use crate::rss::RssAdapter;

const YOUTUBE_VIDEOS_API_URL: &str = "https://www.googleapis.com/youtube/v3/videos";
const YOUTUBE_BATCH_SIZE: usize = 50;
const YOUTUBE_VIEW_COUNT_KEY: &str = "youtube_view_count";
const YOUTUBE_LIKE_COUNT_KEY: &str = "youtube_like_count";
const YOUTUBE_COMMENT_COUNT_KEY: &str = "youtube_comment_count";
const YOUTUBE_DURATION_KEY: &str = "youtube_duration";
const YOUTUBE_CHANNEL_ID_KEY: &str = "youtube_channel_id";
const YOUTUBE_CHANNEL_TITLE_KEY: &str = "youtube_channel_title";

pub struct YoutubeAdapter {
    rss: RssAdapter,
    client: Client,
    api_key: Option<String>,
    videos_api_url: String,
}

impl YoutubeAdapter {
    pub fn new() -> Self {
        Self::with_client(Client::new())
    }

    pub fn with_api_key(api_key: impl Into<String>) -> Self {
        Self::with_client_and_api_key(Client::new(), Some(api_key.into()))
    }

    pub fn with_client(client: Client) -> Self {
        Self::with_client_and_api_key(client, None)
    }

    pub fn with_client_and_api_key(client: Client, api_key: Option<String>) -> Self {
        Self {
            rss: RssAdapter::with_client(client.clone()),
            client,
            api_key,
            videos_api_url: YOUTUBE_VIDEOS_API_URL.into(),
        }
    }

    async fn fetch_video_enrichments(
        &self,
        video_ids: &[String],
    ) -> Result<HashMap<String, VideoEnrichment>, AdapterError> {
        let Some(api_key) = self.api_key.as_deref() else {
            return Ok(HashMap::new());
        };

        let mut enrichments = HashMap::new();
        for batch in video_ids.chunks(YOUTUBE_BATCH_SIZE) {
            let response = self
                .client
                .get(&self.videos_api_url)
                .query(&[
                    ("part", "contentDetails,statistics,snippet"),
                    ("id", &batch.join(",")),
                    ("key", api_key),
                ])
                .send()
                .await
                .map_err(|err| AdapterError::Fetch(Box::new(err)))?
                .error_for_status()
                .map_err(|err| AdapterError::Fetch(Box::new(err)))?;

            let payload: VideosListResponse = response
                .json()
                .await
                .map_err(|err| AdapterError::Parse(Box::new(err)))?;

            for item in payload.items {
                enrichments.insert(item.id.clone(), VideoEnrichment::from(item));
            }
        }

        Ok(enrichments)
    }

    fn apply_enrichments(
        &self,
        entries: &mut [FetchedEntry],
        enrichments_by_video_id: &HashMap<String, VideoEnrichment>,
    ) {
        for entry in entries {
            let Some(video_id) = extract_video_id(entry) else {
                continue;
            };
            let Some(enrichment) = enrichments_by_video_id.get(&video_id) else {
                continue;
            };

            for (key, value) in enrichment.as_pairs() {
                entry.enrichments.insert(key, value);
            }

            if entry.thumbnail_url.is_none() {
                entry.thumbnail_url = enrichment.thumbnail_url.clone();
            }
        }
    }
}
impl Default for YoutubeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl SourceAdapter for YoutubeAdapter {
    fn kind(&self) -> AdapterType {
        AdapterType::Youtube
    }

    async fn fetch(&self, url: &str) -> Result<FetchedFeed, AdapterError> {
        let mut feed = self.rss.fetch(url).await?;
        if self.api_key.is_none() {
            return Ok(feed);
        }

        let video_ids = collect_video_ids(&feed.entries);
        if video_ids.is_empty() {
            return Ok(feed);
        }

        let enrichments = self.fetch_video_enrichments(&video_ids).await?;
        self.apply_enrichments(&mut feed.entries, &enrichments);
        Ok(feed)
    }
}

fn collect_video_ids(entries: &[FetchedEntry]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut video_ids = Vec::new();

    for entry in entries {
        let Some(video_id) = extract_video_id(entry) else {
            continue;
        };

        if seen.insert(video_id.clone()) {
            video_ids.push(video_id);
        }
    }

    video_ids
}

fn extract_video_id(entry: &FetchedEntry) -> Option<String> {
    extract_video_id_from_external_id(&entry.external_id)
        .or_else(|| extract_video_id_from_url(&entry.url))
}

fn extract_video_id_from_external_id(external_id: &str) -> Option<String> {
    external_id
        .strip_prefix("yt:video:")
        .filter(|video_id| !video_id.is_empty())
        .map(ToOwned::to_owned)
}

fn extract_video_id_from_url(url: &str) -> Option<String> {
    let query = url.split_once('?').map(|(_, query)| query);
    if let Some(query) = query {
        for pair in query.split('&') {
            let Some((key, value)) = pair.split_once('=') else {
                continue;
            };
            if key == "v" && !value.is_empty() {
                return Some(value.to_owned());
            }
        }
    }

    let short_url_id = url
        .split("youtu.be/")
        .nth(1)
        .and_then(|suffix| suffix.split(['?', '&', '/', '#']).next())
        .filter(|video_id| !video_id.is_empty());

    short_url_id.map(ToOwned::to_owned)
}

#[derive(Debug, Deserialize)]
struct VideosListResponse {
    #[serde(default)]
    items: Vec<VideoItem>,
}

#[derive(Debug, Deserialize)]
struct VideoItem {
    id: String,
    #[serde(default)]
    snippet: Option<Snippet>,
    #[serde(default, rename = "contentDetails")]
    content_details: Option<ContentDetails>,
    #[serde(default)]
    statistics: Option<Statistics>,
}

#[derive(Debug, Deserialize)]
struct Snippet {
    #[serde(default, rename = "channelId")]
    channel_id: Option<String>,
    #[serde(default, rename = "channelTitle")]
    channel_title: Option<String>,
    #[serde(default)]
    thumbnails: Option<Thumbnails>,
}

#[derive(Debug, Clone, Deserialize)]
struct Thumbnails {
    #[serde(default)]
    high: Option<Thumbnail>,
    #[serde(default)]
    medium: Option<Thumbnail>,
    #[serde(default)]
    default: Option<Thumbnail>,
}

impl Thumbnails {
    fn best_url(self) -> Option<String> {
        self.high
            .or(self.medium)
            .or(self.default)
            .map(|thumbnail| thumbnail.url)
    }
}

#[derive(Debug, Clone, Deserialize)]
struct Thumbnail {
    url: String,
}

#[derive(Debug, Deserialize)]
struct ContentDetails {
    duration: String,
}

#[derive(Debug, Deserialize)]
struct Statistics {
    #[serde(default, rename = "viewCount")]
    view_count: Option<String>,
    #[serde(default, rename = "likeCount")]
    like_count: Option<String>,
    #[serde(default, rename = "commentCount")]
    comment_count: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct VideoEnrichment {
    view_count: Option<String>,
    like_count: Option<String>,
    comment_count: Option<String>,
    duration: Option<String>,
    channel_id: Option<String>,
    channel_title: Option<String>,
    thumbnail_url: Option<String>,
}

impl VideoEnrichment {
    fn as_pairs(&self) -> Vec<(String, String)> {
        let mut pairs = Vec::new();

        if let Some(value) = &self.view_count {
            pairs.push((YOUTUBE_VIEW_COUNT_KEY.into(), value.clone()));
        }
        if let Some(value) = &self.like_count {
            pairs.push((YOUTUBE_LIKE_COUNT_KEY.into(), value.clone()));
        }
        if let Some(value) = &self.comment_count {
            pairs.push((YOUTUBE_COMMENT_COUNT_KEY.into(), value.clone()));
        }
        if let Some(value) = &self.duration {
            pairs.push((YOUTUBE_DURATION_KEY.into(), value.clone()));
        }
        if let Some(value) = &self.channel_id {
            pairs.push((YOUTUBE_CHANNEL_ID_KEY.into(), value.clone()));
        }
        if let Some(value) = &self.channel_title {
            pairs.push((YOUTUBE_CHANNEL_TITLE_KEY.into(), value.clone()));
        }

        pairs
    }
}

impl From<VideoItem> for VideoEnrichment {
    fn from(item: VideoItem) -> Self {
        let thumbnail_url = item
            .snippet
            .as_ref()
            .and_then(|snippet| snippet.thumbnails.as_ref())
            .cloned()
            .and_then(Thumbnails::best_url);

        Self {
            view_count: item
                .statistics
                .as_ref()
                .and_then(|statistics| statistics.view_count.clone()),
            like_count: item
                .statistics
                .as_ref()
                .and_then(|statistics| statistics.like_count.clone()),
            comment_count: item
                .statistics
                .as_ref()
                .and_then(|statistics| statistics.comment_count.clone()),
            duration: item
                .content_details
                .as_ref()
                .map(|content_details| content_details.duration.clone()),
            channel_id: item
                .snippet
                .as_ref()
                .and_then(|snippet| snippet.channel_id.clone()),
            channel_title: item
                .snippet
                .as_ref()
                .and_then(|snippet| snippet.channel_title.clone()),
            thumbnail_url,
        }
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::*;

    #[test]
    fn adapter_reports_kind_as_youtube() {
        let adapter = YoutubeAdapter::new();
        assert_eq!(adapter.kind(), AdapterType::Youtube);
    }

    #[test]
    fn extract_video_id_prefers_external_id() {
        let entry = FetchedEntry {
            external_id: "yt:video:abc123".into(),
            title: "Video".into(),
            summary: None,
            url: "https://www.youtube.com/watch?v=ignored".into(),
            thumbnail_url: None,
            author: None,
            published_at: None,
            enrichments: HashMap::new(),
        };

        assert_eq!(extract_video_id(&entry).as_deref(), Some("abc123"));
    }

    #[tokio::test]
    async fn fetch_applies_youtube_api_enrichments() {
        let rss_body = r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Feedfold Videos</title>
  <entry>
    <id>yt:video:video-one</id>
    <title>Video One</title>
    <link href="https://www.youtube.com/watch?v=video-one"/>
    <updated>2026-04-01T12:00:00Z</updated>
  </entry>
</feed>"#;
        let api_body = r#"{"items":[{"id":"video-one","contentDetails":{"duration":"PT5M"},"statistics":{"viewCount":"123","likeCount":"9","commentCount":"2"},"snippet":{"channelId":"chan-1","channelTitle":"Feedfold Channel","thumbnails":{"high":{"url":"https://img.example/high.jpg"}}}}]}"#;

        let (rss_url, api_url, requests_handle) =
            spawn_test_server(vec![rss_body.to_owned(), api_body.to_owned()]).await;

        let client = Client::new();
        let adapter = YoutubeAdapter {
            rss: RssAdapter::with_client(client.clone()),
            client,
            api_key: Some("test-key".into()),
            videos_api_url: api_url,
        };

        let feed = adapter.fetch(&rss_url).await.unwrap();
        let entry = &feed.entries[0];

        assert_eq!(
            entry.enrichments.get(YOUTUBE_VIEW_COUNT_KEY).map(|s| s.as_str()),
            Some("123")
        );
        assert_eq!(
            entry.enrichments.get(YOUTUBE_DURATION_KEY).map(|s| s.as_str()),
            Some("PT5M")
        );
        assert_eq!(
            entry
                .enrichments
                .get(YOUTUBE_CHANNEL_TITLE_KEY)
                .map(|s| s.as_str()),
            Some("Feedfold Channel")
        );
        assert_eq!(
            entry.thumbnail_url.as_deref(),
            Some("https://img.example/high.jpg")
        );

        let requests = requests_handle.await.unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests[1].contains("part=contentDetails%2Cstatistics%2Csnippet"));
        assert!(requests[1].contains("id=video-one"));
        assert!(requests[1].contains("key=test-key"));
    }

    #[tokio::test]
    async fn fetch_video_enrichments_batches_requests() {
        let batch_one = r#"{"items":[{"id":"video-000","statistics":{"viewCount":"1"}}]}"#;
        let batch_two = r#"{"items":[{"id":"video-050","statistics":{"viewCount":"2"}}]}"#;
        let (_, api_url, requests_handle) =
            spawn_test_server(vec![batch_one.to_owned(), batch_two.to_owned()]).await;

        let client = Client::new();
        let adapter = YoutubeAdapter {
            rss: RssAdapter::with_client(client.clone()),
            client,
            api_key: Some("test-key".into()),
            videos_api_url: api_url,
        };

        let video_ids = (0..51).map(|index| format!("video-{index:03}")).collect::<Vec<_>>();
        let enrichments = adapter.fetch_video_enrichments(&video_ids).await.unwrap();

        assert_eq!(
            enrichments.get("video-000").and_then(|item| item.view_count.as_deref()),
            Some("1")
        );
        assert_eq!(
            enrichments.get("video-050").and_then(|item| item.view_count.as_deref()),
            Some("2")
        );

        let requests = requests_handle.await.unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests[0].contains("id=video-000%2Cvideo-001"));
        assert!(requests[0].contains("video-049"));
        assert!(!requests[0].contains("video-050"));
        assert!(requests[1].contains("id=video-050"));
    }

    async fn spawn_test_server(
        response_bodies: Vec<String>,
    ) -> (String, String, tokio::task::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();

        let requests_handle = tokio::spawn(async move {
            let mut requests = Vec::new();

            for body in response_bodies {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut buffer = [0u8; 8192];
                let read = socket.read(&mut buffer).await.unwrap();
                requests.push(String::from_utf8_lossy(&buffer[..read]).into_owned());

                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                socket.write_all(response.as_bytes()).await.unwrap();
            }

            requests
        });

        (
            format!("http://{address}/feed.atom"),
            format!("http://{address}/youtube/v3/videos"),
            requests_handle,
        )
    }
}
