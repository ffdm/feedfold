use std::collections::HashSet;

use feedfold_core::config::Config;
use feedfold_core::ranker::Score;
use feedfold_core::storage::Entry;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use thiserror::Error;

const ANTHROPIC_MESSAGES_API_URL: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_VERSION: &str = "2023-06-01";
const DEFAULT_MODEL: &str = "claude-sonnet-4-20250514";
const DEFAULT_SYSTEM_PROMPT: &str = "You rank feed entries for a reader. Prefer entries that seem substantive, novel, and worth reading in full. Return only valid JSON.";

pub struct ClaudeRanker {
    client: Client,
    api_key: String,
    api_url: String,
    model: String,
    system_prompt: String,
    interests: String,
}

impl ClaudeRanker {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::with_client(Client::new(), api_key)
    }

    pub fn from_config(api_key: impl Into<String>, config: &Config) -> Self {
        Self::new(api_key).with_interests(&config.ai.interests)
    }

    pub fn with_client(client: Client, api_key: impl Into<String>) -> Self {
        Self {
            client,
            api_key: api_key.into(),
            api_url: ANTHROPIC_MESSAGES_API_URL.into(),
            model: DEFAULT_MODEL.into(),
            system_prompt: DEFAULT_SYSTEM_PROMPT.into(),
            interests: String::new(),
        }
    }

    pub fn with_api_url(mut self, api_url: impl Into<String>) -> Self {
        self.api_url = api_url.into();
        self
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    pub fn with_system_prompt(mut self, system_prompt: impl Into<String>) -> Self {
        self.system_prompt = system_prompt.into();
        self
    }

    pub fn with_interests(mut self, interests: impl Into<String>) -> Self {
        self.interests = interests.into();
        self
    }

    pub async fn rank(
        &self,
        entries: &[Entry],
        top_n: usize,
        rating_history: &[Entry],
    ) -> Result<Vec<Score>, ClaudeRankerError> {
        if entries.is_empty() {
            return Ok(Vec::new());
        }

        let response = self
            .client
            .post(&self.api_url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .json(&MessagesRequest {
                model: self.model.clone(),
                max_tokens: 256,
                temperature: 0.0,
                system: build_system_prompt(&self.system_prompt, &self.interests),
                messages: vec![Message {
                    role: "user",
                    content: build_user_prompt(entries, top_n, rating_history),
                }],
            })
            .send()
            .await?
            .error_for_status()?;

        let payload: MessagesResponse = response.json().await?;
        let text = payload
            .content
            .iter()
            .find(|block| block.kind == "text")
            .map(|block| block.text.as_str())
            .ok_or(ClaudeRankerError::MissingTextResponse)?;
        let ranking: RankingResponse = serde_json::from_str(extract_json_block(text)?)?;
        scores_from_ranked_ids(entries, ranking.ranked_entry_ids)
    }
}

#[derive(Debug, Error)]
pub enum ClaudeRankerError {
    #[error("requesting Anthropic ranking")]
    Request(#[from] reqwest::Error),

    #[error("parsing Anthropic ranking response")]
    Parse(#[from] serde_json::Error),

    #[error("Anthropic response did not include a text block")]
    MissingTextResponse,

    #[error("Anthropic response did not contain JSON")]
    MissingJsonBlock,

    #[error("Anthropic response ranked unknown entry id {0}")]
    UnknownEntryId(i64),

    #[error("Anthropic response ranked entry id {0} more than once")]
    DuplicateEntryId(i64),

    #[error("Anthropic response ranked {actual} entries, expected {expected}")]
    WrongEntryCount { expected: usize, actual: usize },
}

#[derive(Debug, Serialize)]
struct MessagesRequest {
    model: String,
    max_tokens: u32,
    temperature: f32,
    system: String,
    messages: Vec<Message>,
}

#[derive(Debug, Serialize)]
struct Message {
    role: &'static str,
    content: String,
}

#[derive(Debug, Deserialize)]
struct MessagesResponse {
    content: Vec<ContentBlock>,
}

#[derive(Debug, Deserialize)]
struct ContentBlock {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: String,
}

#[derive(Debug, Deserialize)]
struct RankingResponse {
    ranked_entry_ids: Vec<i64>,
}

fn build_system_prompt(system_prompt: &str, interests: &str) -> String {
    let interests = interests.trim();
    if interests.is_empty() {
        return system_prompt.to_string();
    }

    format!("{system_prompt}\n\nReader interests:\n{interests}")
}

fn build_user_prompt(entries: &[Entry], top_n: usize, rating_history: &[Entry]) -> String {
    let mut prompt = format!(
        "Rank these feed entries from best to worst for a thoughtful reader.\n\
         Return JSON only in this exact shape: {{\"ranked_entry_ids\":[... ]}}.\n\
         Include every entry id exactly once.\n\
         The daemon will keep the top {top_n} entries after ranking.\n"
    );

    let rated_entries: Vec<_> = rating_history
        .iter()
        .filter_map(|entry| entry.rating.map(|rating| (entry, rating)))
        .collect();
    if !rated_entries.is_empty() {
        prompt.push_str(
            "Recent reader ratings are included below. Treat 5 as strong interest and 1 as strong dislike.\n\n",
        );

        for (entry, rating) in rated_entries {
            let published_at = entry
                .published_at
                .map(|timestamp| timestamp.to_rfc3339())
                .unwrap_or_else(|| "unknown".into());
            let summary = entry.summary.as_deref().unwrap_or("");
            let summary = summary.trim();

            prompt.push_str(&format!(
                "Rated Entry\nRating: {rating}/5\nTitle: {}\nPublished: {}\nURL: {}\nSummary: {}\n\n",
                entry.title, published_at, entry.url, summary
            ));
        }
    }

    prompt.push_str("Entries to rank:\n\n");

    for entry in entries {
        let published_at = entry
            .published_at
            .map(|timestamp| timestamp.to_rfc3339())
            .unwrap_or_else(|| "unknown".into());
        let summary = entry.summary.as_deref().unwrap_or("");
        let summary = summary.trim();

        prompt.push_str(&format!(
            "Entry ID: {}\nTitle: {}\nPublished: {}\nURL: {}\nSummary: {}\n\n",
            entry.id, entry.title, published_at, entry.url, summary
        ));
    }

    prompt
}

fn extract_json_block(text: &str) -> Result<&str, ClaudeRankerError> {
    let trimmed = text.trim();
    if trimmed.starts_with('{') {
        return Ok(trimmed);
    }

    if let Some(stripped) = trimmed.strip_prefix("```json\n") {
        return stripped
            .strip_suffix("\n```")
            .map(str::trim)
            .ok_or(ClaudeRankerError::MissingJsonBlock);
    }

    if let Some(stripped) = trimmed.strip_prefix("```\n") {
        return stripped
            .strip_suffix("\n```")
            .map(str::trim)
            .ok_or(ClaudeRankerError::MissingJsonBlock);
    }

    Err(ClaudeRankerError::MissingJsonBlock)
}

fn scores_from_ranked_ids(
    entries: &[Entry],
    ranked_entry_ids: Vec<i64>,
) -> Result<Vec<Score>, ClaudeRankerError> {
    if ranked_entry_ids.len() != entries.len() {
        return Err(ClaudeRankerError::WrongEntryCount {
            expected: entries.len(),
            actual: ranked_entry_ids.len(),
        });
    }

    let known_ids: HashSet<i64> = entries.iter().map(|entry| entry.id).collect();
    let mut seen = HashSet::with_capacity(ranked_entry_ids.len());
    let total = ranked_entry_ids.len() as f64;
    let mut scores = Vec::with_capacity(ranked_entry_ids.len());

    for (index, entry_id) in ranked_entry_ids.into_iter().enumerate() {
        if !known_ids.contains(&entry_id) {
            return Err(ClaudeRankerError::UnknownEntryId(entry_id));
        }

        if !seen.insert(entry_id) {
            return Err(ClaudeRankerError::DuplicateEntryId(entry_id));
        }

        scores.push(Score {
            entry_id,
            value: total - index as f64,
        });
    }

    Ok(scores)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use serde_json::Value;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::sync::oneshot;

    use super::*;
    use feedfold_core::config::{AdapterType, Config};
    use feedfold_core::storage::{NewEntry, NewSource, Storage};

    #[tokio::test]
    async fn claude_ranker_posts_request_and_returns_scores() {
        let (entries, server_entries) = sample_entries();
        let response_body = serde_json::json!({
            "content": [
                {
                    "type": "text",
                    "text": "{\"ranked_entry_ids\":[2,1]}"
                }
            ]
        })
        .to_string();
        let (server_url, request_rx) = spawn_test_server(response_body).await;

        let scores = ClaudeRanker::new("test-key")
            .with_api_url(server_url)
            .with_model("claude-test")
            .rank(&entries, 1, &[])
            .await
            .unwrap();

        assert_eq!(scores.len(), 2);
        assert_eq!(scores[0].entry_id, entries[1].id);
        assert_eq!(scores[1].entry_id, entries[0].id);
        assert!(scores[0].value > scores[1].value);

        let request = request_rx.await.unwrap();
        assert_eq!(request.request_line, "POST / HTTP/1.1");
        assert_eq!(
            request.headers.get("x-api-key").map(String::as_str),
            Some("test-key")
        );
        assert_eq!(
            request.headers.get("anthropic-version").map(String::as_str),
            Some(ANTHROPIC_VERSION)
        );

        let body: Value = serde_json::from_str(&request.body).unwrap();
        assert_eq!(body["model"], "claude-test");
        assert_eq!(body["max_tokens"], 256);
        assert_eq!(body["temperature"], 0.0);
        assert_eq!(body["system"], DEFAULT_SYSTEM_PROMPT);
        assert!(body["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains(&format!("Entry ID: {}", server_entries[0].id)));
        assert!(body["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains(&format!("Entry ID: {}", server_entries[1].id)));
    }

    #[tokio::test]
    async fn claude_ranker_rejects_invalid_rankings() {
        let (entries, _) = sample_entries();
        let response_body = serde_json::json!({
            "content": [
                {
                    "type": "text",
                    "text": "{\"ranked_entry_ids\":[999,1]}"
                }
            ]
        })
        .to_string();
        let (server_url, _request_rx) = spawn_test_server(response_body).await;

        let err = match ClaudeRanker::new("test-key")
            .with_api_url(server_url)
            .rank(&entries, 1, &[])
            .await
        {
            Ok(_) => panic!("ranking should fail"),
            Err(err) => err,
        };

        assert!(matches!(err, ClaudeRankerError::UnknownEntryId(999)));
    }

    #[tokio::test]
    async fn claude_ranker_skips_network_for_empty_entries() {
        let scores = ClaudeRanker::new("test-key")
            .rank(&[], 3, &[])
            .await
            .unwrap();
        assert!(scores.is_empty());
    }

    #[tokio::test]
    async fn claude_ranker_loads_interests_from_config() {
        let (entries, _) = sample_entries();
        let response_body = serde_json::json!({
            "content": [
                {
                    "type": "text",
                    "text": "{\"ranked_entry_ids\":[2,1]}"
                }
            ]
        })
        .to_string();
        let (server_url, request_rx) = spawn_test_server(response_body).await;
        let config = Config::parse(
            r#"
[ai]
interests = """
Rust internals and thoughtful essays.
Skip product launches.
"""
"#,
        )
        .unwrap();

        ClaudeRanker::from_config("test-key", &config)
            .with_api_url(server_url)
            .rank(&entries, 1, &[])
            .await
            .unwrap();

        let request = request_rx.await.unwrap();
        let body: Value = serde_json::from_str(&request.body).unwrap();
        let system = body["system"].as_str().unwrap();

        assert!(system.contains(DEFAULT_SYSTEM_PROMPT));
        assert!(system.contains(
            "Reader interests:\nRust internals and thoughtful essays.\nSkip product launches."
        ));
    }

    #[tokio::test]
    async fn claude_ranker_includes_rating_history_in_user_prompt() {
        let (entries, _) = sample_entries();
        let rating_history = sample_rating_history();
        let response_body = serde_json::json!({
            "content": [
                {
                    "type": "text",
                    "text": "{\"ranked_entry_ids\":[2,1]}"
                }
            ]
        })
        .to_string();
        let (server_url, request_rx) = spawn_test_server(response_body).await;

        ClaudeRanker::new("test-key")
            .with_api_url(server_url)
            .rank(&entries, 1, &rating_history)
            .await
            .unwrap();

        let request = request_rx.await.unwrap();
        let body: Value = serde_json::from_str(&request.body).unwrap();
        let prompt = body["messages"][0]["content"].as_str().unwrap();

        assert!(prompt.contains(
            "Recent reader ratings are included below. Treat 5 as strong interest and 1 as strong dislike."
        ));
        assert!(prompt.contains("Rating: 5/5\nTitle: Beloved essay"));
        assert!(prompt.contains("Rating: 1/5\nTitle: Skipped launch post"));
    }

    async fn spawn_test_server(
        response_body: String,
    ) -> (String, oneshot::Receiver<CapturedRequest>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (request_tx, request_rx) = oneshot::channel();

        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let request = read_request(&mut stream).await;
            request_tx.send(request).unwrap();

            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                response_body.len(),
                response_body
            );
            stream.write_all(response.as_bytes()).await.unwrap();
        });

        (format!("http://{address}"), request_rx)
    }

    async fn read_request(stream: &mut tokio::net::TcpStream) -> CapturedRequest {
        let mut buffer = Vec::new();
        let mut chunk = [0_u8; 1024];

        loop {
            let bytes_read = stream.read(&mut chunk).await.unwrap();
            if bytes_read == 0 {
                break;
            }
            buffer.extend_from_slice(&chunk[..bytes_read]);
            if find_header_body_split(&buffer).is_some() {
                break;
            }
        }

        let split = find_header_body_split(&buffer).unwrap();
        let headers_raw = String::from_utf8(buffer[..split].to_vec()).unwrap();
        let mut lines = headers_raw.lines();
        let request_line = lines.next().unwrap().to_string();
        let headers = lines
            .filter_map(|line| line.split_once(": "))
            .map(|(name, value)| (name.to_ascii_lowercase(), value.to_string()))
            .collect::<HashMap<_, _>>();

        let content_length = headers
            .get("content-length")
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(0);
        let body_start = split + 4;
        let mut body = buffer[body_start..].to_vec();

        while body.len() < content_length {
            let bytes_read = stream.read(&mut chunk).await.unwrap();
            if bytes_read == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..bytes_read]);
        }

        CapturedRequest {
            request_line,
            headers,
            body: String::from_utf8(body).unwrap(),
        }
    }

    fn find_header_body_split(buffer: &[u8]) -> Option<usize> {
        buffer.windows(4).position(|window| window == b"\r\n\r\n")
    }

    fn sample_entries() -> (Vec<Entry>, Vec<Entry>) {
        let mut storage = Storage::open_in_memory().unwrap();
        let source_id = storage
            .insert_source(&NewSource {
                name: "Test".into(),
                url: "https://example.com/feed".into(),
                adapter: AdapterType::Rss,
                top_n_override: None,
            })
            .unwrap();

        storage
            .upsert_entries(&[
                NewEntry {
                    source_id,
                    external_id: "first".into(),
                    title: "First article".into(),
                    summary: Some("A short first summary.".into()),
                    url: "https://example.com/first".into(),
                    thumbnail_url: None,
                    author: None,
                    published_at: None,
                    enrichments: HashMap::new(),
                },
                NewEntry {
                    source_id,
                    external_id: "second".into(),
                    title: "Second article".into(),
                    summary: Some("A short second summary.".into()),
                    url: "https://example.com/second".into(),
                    thumbnail_url: None,
                    author: None,
                    published_at: None,
                    enrichments: HashMap::new(),
                },
            ])
            .unwrap();

        let entries = storage.list_entries_for_source(source_id).unwrap();
        (entries.clone(), entries)
    }

    fn sample_rating_history() -> Vec<Entry> {
        let mut storage = Storage::open_in_memory().unwrap();
        let source_id = storage
            .insert_source(&NewSource {
                name: "History".into(),
                url: "https://example.com/history".into(),
                adapter: AdapterType::Rss,
                top_n_override: None,
            })
            .unwrap();

        storage
            .upsert_entries(&[
                NewEntry {
                    source_id,
                    external_id: "liked".into(),
                    title: "Beloved essay".into(),
                    summary: Some("Deeply reported and reflective.".into()),
                    url: "https://example.com/liked".into(),
                    thumbnail_url: None,
                    author: None,
                    published_at: None,
                    enrichments: HashMap::new(),
                },
                NewEntry {
                    source_id,
                    external_id: "disliked".into(),
                    title: "Skipped launch post".into(),
                    summary: Some("Mostly marketing copy.".into()),
                    url: "https://example.com/disliked".into(),
                    thumbnail_url: None,
                    author: None,
                    published_at: None,
                    enrichments: HashMap::new(),
                },
            ])
            .unwrap();

        let entries = storage.list_entries_for_source(source_id).unwrap();
        let liked = entries
            .iter()
            .find(|entry| entry.external_id == "liked")
            .unwrap();
        let disliked = entries
            .iter()
            .find(|entry| entry.external_id == "disliked")
            .unwrap();

        storage.set_entry_rating(liked.id, 5).unwrap();
        storage.set_entry_rating(disliked.id, 1).unwrap();
        storage.list_rated_entries(10).unwrap()
    }

    #[derive(Debug)]
    struct CapturedRequest {
        request_line: String,
        headers: HashMap<String, String>,
        body: String,
    }
}
