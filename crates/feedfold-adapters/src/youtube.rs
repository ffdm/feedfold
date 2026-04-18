use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use chrono::DateTime;
use feedfold_core::adapter::{AdapterError, FetchedEntry, FetchedFeed, SourceAdapter};
use feedfold_core::config::AdapterType;
use reqwest::Client;
use serde::Deserialize;

use crate::rss::RssAdapter;

const YOUTUBE_CHANNELS_API_URL: &str = "https://www.googleapis.com/youtube/v3/channels";
const YOUTUBE_PLAYLIST_ITEMS_API_URL: &str =
    "https://www.googleapis.com/youtube/v3/playlistItems";
const YOUTUBE_VIDEOS_API_URL: &str = "https://www.googleapis.com/youtube/v3/videos";
const YOUTUBE_BATCH_SIZE: usize = 50;
pub const YOUTUBE_VIEW_COUNT_KEY: &str = "youtube_view_count";
const YOUTUBE_LIKE_COUNT_KEY: &str = "youtube_like_count";
const YOUTUBE_COMMENT_COUNT_KEY: &str = "youtube_comment_count";
pub const YOUTUBE_DURATION_KEY: &str = "youtube_duration";
const YOUTUBE_CHANNEL_ID_KEY: &str = "youtube_channel_id";
const YOUTUBE_CHANNEL_TITLE_KEY: &str = "youtube_channel_title";
pub const YOUTUBE_LIVE_BROADCAST_KEY: &str = "youtube_live_broadcast";

pub struct YoutubeAdapter {
    rss: RssAdapter,
    client: Client,
    api_key: Option<String>,
    channels_api_url: String,
    playlist_items_api_url: String,
    videos_api_url: String,
    uploads_cache: Mutex<HashMap<String, String>>,
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
            channels_api_url: YOUTUBE_CHANNELS_API_URL.into(),
            playlist_items_api_url: YOUTUBE_PLAYLIST_ITEMS_API_URL.into(),
            videos_api_url: YOUTUBE_VIDEOS_API_URL.into(),
            uploads_cache: Mutex::new(HashMap::new()),
        }
    }

    #[cfg(test)]
    fn with_api_urls(
        mut self,
        channels_url: String,
        playlist_items_url: String,
        videos_url: String,
    ) -> Self {
        self.channels_api_url = channels_url;
        self.playlist_items_api_url = playlist_items_url;
        self.videos_api_url = videos_url;
        self
    }

    async fn fetch_via_api(&self, url: &str) -> Result<FetchedFeed, AdapterError> {
        let api_key = self.api_key.as_deref().ok_or_else(|| {
            AdapterError::InvalidResponse(
                "YouTube RSS feed is unavailable and YOUTUBE_API_KEY is not set. \
                 Set the YOUTUBE_API_KEY environment variable to fetch YouTube channels \
                 via the Data API."
                    .into(),
            )
        })?;

        let channel_id = extract_channel_id_from_feed_url(url).ok_or_else(|| {
            AdapterError::Fetch(Box::from(format!(
                "could not extract a channel ID from {url}"
            )))
        })?;

        let uploads_playlist_id = self
            .resolve_uploads_playlist_id(api_key, &channel_id)
            .await?;

        let mut entries = self
            .fetch_playlist_entries(api_key, &uploads_playlist_id)
            .await?;

        let video_ids = collect_video_ids(&entries);
        if !video_ids.is_empty() {
            let enrichments = self.fetch_video_enrichments(&video_ids).await?;
            self.apply_enrichments(&mut entries, &enrichments);
        }

        Ok(FetchedFeed {
            name: entries.first().and_then(|e| e.author.clone()),
            entries,
        })
    }

    async fn resolve_uploads_playlist_id(
        &self,
        api_key: &str,
        channel_id: &str,
    ) -> Result<String, AdapterError> {
        if let Some(cached) = self.uploads_cache.lock().unwrap().get(channel_id) {
            return Ok(cached.clone());
        }

        let response = self
            .client
            .get(&self.channels_api_url)
            .query(&[
                ("part", "contentDetails"),
                ("id", channel_id),
                ("key", api_key),
            ])
            .send()
            .await
            .map_err(|err| AdapterError::Fetch(Box::new(err)))?
            .error_for_status()
            .map_err(|err| AdapterError::Fetch(Box::new(err)))?;

        let payload: ChannelsListResponse = response
            .json()
            .await
            .map_err(|err| AdapterError::Parse(Box::new(err)))?;

        let uploads_id = payload
            .items
            .into_iter()
            .next()
            .and_then(|ch| ch.content_details)
            .and_then(|cd| cd.related_playlists)
            .and_then(|rp| rp.uploads)
            .ok_or_else(|| {
                AdapterError::InvalidResponse(format!(
                    "no uploads playlist found for channel {channel_id}"
                ))
            })?;

        self.uploads_cache
            .lock()
            .unwrap()
            .insert(channel_id.to_string(), uploads_id.clone());
        Ok(uploads_id)
    }

    async fn fetch_playlist_entries(
        &self,
        api_key: &str,
        playlist_id: &str,
    ) -> Result<Vec<FetchedEntry>, AdapterError> {
        let response = self
            .client
            .get(&self.playlist_items_api_url)
            .query(&[
                ("part", "snippet,contentDetails"),
                ("playlistId", playlist_id),
                ("maxResults", "50"),
                ("key", api_key),
            ])
            .send()
            .await
            .map_err(|err| AdapterError::Fetch(Box::new(err)))?
            .error_for_status()
            .map_err(|err| AdapterError::Fetch(Box::new(err)))?;

        let payload: PlaylistItemsResponse = response
            .json()
            .await
            .map_err(|err| AdapterError::Parse(Box::new(err)))?;

        Ok(payload
            .items
            .into_iter()
            .filter_map(playlist_item_to_entry)
            .collect())
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
        match self.rss.fetch(url).await {
            Ok(mut feed) => {
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
            Err(AdapterError::Fetch(ref source))
                if source.to_string().contains("404") =>
            {
                self.fetch_via_api(url).await
            }
            Err(e) => Err(e),
        }
    }
}

fn extract_channel_id_from_feed_url(url: &str) -> Option<String> {
    let query = url.split_once('?').map(|(_, q)| q)?;
    for pair in query.split('&') {
        let (key, value) = pair.split_once('=')?;
        if key == "channel_id" && !value.is_empty() {
            return Some(value.to_string());
        }
    }
    None
}

fn playlist_item_to_entry(item: PlaylistItem) -> Option<FetchedEntry> {
    let snippet = item.snippet?;
    let video_id = snippet
        .resource_id
        .as_ref()
        .filter(|r| r.kind == "youtube#video")
        .map(|r| r.video_id.clone())?;

    let published_at = snippet
        .published_at
        .as_deref()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.to_utc());

    let thumbnail_url = snippet.thumbnails.and_then(Thumbnails::best_url);

    Some(FetchedEntry {
        external_id: format!("yt:video:{video_id}"),
        title: snippet.title.unwrap_or_default(),
        summary: snippet.description.filter(|d| !d.is_empty()),
        url: format!("https://www.youtube.com/watch?v={video_id}"),
        thumbnail_url,
        author: snippet.channel_title,
        published_at,
        enrichments: HashMap::new(),
    })
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

// --- YouTube Data API response types ---

#[derive(Debug, Deserialize)]
struct ChannelsListResponse {
    #[serde(default)]
    items: Vec<ChannelItem>,
}

#[derive(Debug, Deserialize)]
struct ChannelItem {
    #[serde(default, rename = "contentDetails")]
    content_details: Option<ChannelContentDetails>,
}

#[derive(Debug, Deserialize)]
struct ChannelContentDetails {
    #[serde(default, rename = "relatedPlaylists")]
    related_playlists: Option<RelatedPlaylists>,
}

#[derive(Debug, Deserialize)]
struct RelatedPlaylists {
    uploads: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PlaylistItemsResponse {
    #[serde(default)]
    items: Vec<PlaylistItem>,
}

#[derive(Debug, Deserialize)]
struct PlaylistItem {
    #[serde(default)]
    snippet: Option<PlaylistItemSnippet>,
}

#[derive(Debug, Deserialize)]
struct PlaylistItemSnippet {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default, rename = "publishedAt")]
    published_at: Option<String>,
    #[serde(default, rename = "channelTitle")]
    channel_title: Option<String>,
    #[serde(default)]
    thumbnails: Option<Thumbnails>,
    #[serde(default, rename = "resourceId")]
    resource_id: Option<ResourceId>,
}

#[derive(Debug, Deserialize)]
struct ResourceId {
    kind: String,
    #[serde(rename = "videoId")]
    video_id: String,
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
    #[serde(default, rename = "liveBroadcastContent")]
    live_broadcast_content: Option<String>,
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
    live_broadcast_content: Option<String>,
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
        if let Some(value) = &self.live_broadcast_content {
            pairs.push((YOUTUBE_LIVE_BROADCAST_KEY.into(), value.clone()));
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
            live_broadcast_content: item
                .snippet
                .as_ref()
                .and_then(|snippet| snippet.live_broadcast_content.clone()),
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

    #[test]
    fn extracts_channel_id_from_feed_url() {
        assert_eq!(
            extract_channel_id_from_feed_url(
                "https://www.youtube.com/feeds/videos.xml?channel_id=UC123abc"
            )
            .as_deref(),
            Some("UC123abc")
        );
        assert_eq!(
            extract_channel_id_from_feed_url("https://example.com/rss"),
            None
        );
    }

    #[test]
    fn converts_playlist_item_to_entry() {
        let item = PlaylistItem {
            snippet: Some(PlaylistItemSnippet {
                title: Some("Test Video".into()),
                description: Some("A description.".into()),
                published_at: Some("2026-04-01T12:00:00Z".into()),
                channel_title: Some("Test Channel".into()),
                thumbnails: Some(Thumbnails {
                    high: Some(Thumbnail {
                        url: "https://img.example/high.jpg".into(),
                    }),
                    medium: None,
                    default: None,
                }),
                resource_id: Some(ResourceId {
                    kind: "youtube#video".into(),
                    video_id: "abc123".into(),
                }),
            }),
        };

        let entry = playlist_item_to_entry(item).expect("should convert");
        assert_eq!(entry.external_id, "yt:video:abc123");
        assert_eq!(entry.title, "Test Video");
        assert_eq!(entry.summary.as_deref(), Some("A description."));
        assert_eq!(entry.url, "https://www.youtube.com/watch?v=abc123");
        assert_eq!(entry.author.as_deref(), Some("Test Channel"));
        assert_eq!(
            entry.thumbnail_url.as_deref(),
            Some("https://img.example/high.jpg")
        );
        assert!(entry.published_at.is_some());
    }

    #[test]
    fn skips_non_video_playlist_items() {
        let item = PlaylistItem {
            snippet: Some(PlaylistItemSnippet {
                title: Some("A Playlist".into()),
                description: None,
                published_at: None,
                channel_title: None,
                thumbnails: None,
                resource_id: Some(ResourceId {
                    kind: "youtube#playlist".into(),
                    video_id: "PLabc".into(),
                }),
            }),
        };

        assert!(playlist_item_to_entry(item).is_none());
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
        let adapter = YoutubeAdapter::with_client_and_api_key(client, Some("test-key".into()))
            .with_api_urls(String::new(), String::new(), api_url);

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
        let adapter = YoutubeAdapter::with_client_and_api_key(client, Some("test-key".into()))
            .with_api_urls(String::new(), String::new(), api_url);

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

    #[tokio::test]
    async fn falls_back_to_api_on_rss_404() {
        let rss_body = "<!DOCTYPE html><title>Error 404</title>";
        let channels_body =
            r#"{"items":[{"contentDetails":{"relatedPlaylists":{"uploads":"UU123"}}}]}"#;
        let playlist_body = r#"{"items":[{"snippet":{"title":"Fallback Video","description":"via API","publishedAt":"2026-04-01T12:00:00Z","channelTitle":"Test Channel","thumbnails":{"high":{"url":"https://img.example/thumb.jpg"}},"resourceId":{"kind":"youtube#video","videoId":"vid-1"}}}]}"#;
        let videos_body = r#"{"items":[{"id":"vid-1","contentDetails":{"duration":"PT3M"},"statistics":{"viewCount":"99"},"snippet":{"channelId":"UC123","channelTitle":"Test Channel","thumbnails":{"high":{"url":"https://img.example/thumb.jpg"}}}}]}"#;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let base = format!("http://{address}");

        let bodies = vec![
            rss_body.to_string(),
            channels_body.to_string(),
            playlist_body.to_string(),
            videos_body.to_string(),
        ];
        tokio::spawn(async move {
            for body in bodies {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut buf = [0u8; 8192];
                let _ = socket.read(&mut buf).await.unwrap();
                let status = if body.contains("404") {
                    "404 Not Found"
                } else {
                    "200 OK"
                };
                let ct = if body.contains("<!DOCTYPE") {
                    "text/html"
                } else {
                    "application/json"
                };
                let resp = format!(
                    "HTTP/1.1 {status}\r\ncontent-type: {ct}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                socket.write_all(resp.as_bytes()).await.unwrap();
            }
        });

        let client = Client::new();
        let adapter = YoutubeAdapter::with_client_and_api_key(client, Some("test-key".into()))
            .with_api_urls(
                format!("{base}/channels"),
                format!("{base}/playlistItems"),
                format!("{base}/videos"),
            );

        let feed_url = format!(
            "{base}/feeds/videos.xml?channel_id=UC123"
        );
        let feed = adapter.fetch(&feed_url).await.unwrap();

        assert_eq!(feed.entries.len(), 1);
        assert_eq!(feed.entries[0].external_id, "yt:video:vid-1");
        assert_eq!(feed.entries[0].title, "Fallback Video");
        assert_eq!(
            feed.entries[0].enrichments.get(YOUTUBE_VIEW_COUNT_KEY).map(|s| s.as_str()),
            Some("99")
        );
    }

    #[tokio::test]
    async fn api_fallback_fails_clearly_without_key() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let base = format!("http://{address}");

        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 4096];
            let _ = socket.read(&mut buf).await.unwrap();
            let body = "<!DOCTYPE html><title>Error 404</title>";
            let resp = format!(
                "HTTP/1.1 404 Not Found\r\ncontent-type: text/html\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            socket.write_all(resp.as_bytes()).await.unwrap();
        });

        let adapter = YoutubeAdapter::with_client(Client::new());
        let feed_url = format!("{base}/feeds/videos.xml?channel_id=UC123");
        let err = adapter.fetch(&feed_url).await.unwrap_err();

        let msg = err.to_string();
        assert!(
            msg.contains("YOUTUBE_API_KEY"),
            "error should mention the env var, got: {msg}"
        );
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
