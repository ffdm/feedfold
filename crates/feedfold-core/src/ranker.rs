use std::cmp::Ordering;
use std::collections::HashMap;

use crate::storage::Entry;

pub type EntryEnrichments = HashMap<i64, HashMap<String, String>>;

const YOUTUBE_VIEW_COUNT_KEY: &str = "youtube_view_count";
const POPULARITY_SCORE_BASE: f64 = 1_000_000_000_000.0;

pub struct RankContext {
    pub top_n: usize,
    pub enrichments: EntryEnrichments,
}

pub struct Score {
    pub entry_id: i64,
    pub value: f64,
}

pub trait Ranker {
    fn rank(&self, entries: &[Entry], ctx: &RankContext) -> Vec<Score>;
}

pub struct RecencyRanker;

impl Ranker for RecencyRanker {
    fn rank(&self, entries: &[Entry], _ctx: &RankContext) -> Vec<Score> {
        let mut scored: Vec<Score> = entries
            .iter()
            .map(|entry| Score {
                entry_id: entry.id,
                value: recency_score(entry),
            })
            .collect();
        sort_scores(&mut scored);
        scored
    }
}

pub struct PopularityRanker;

impl Ranker for PopularityRanker {
    fn rank(&self, entries: &[Entry], ctx: &RankContext) -> Vec<Score> {
        let mut scored: Vec<Score> = entries
            .iter()
            .map(|entry| Score {
                entry_id: entry.id,
                value: popularity_score(entry, &ctx.enrichments),
            })
            .collect();
        sort_scores(&mut scored);
        scored
    }
}

fn recency_score(entry: &Entry) -> f64 {
    entry
        .published_at
        .map(|dt| dt.timestamp() as f64)
        .unwrap_or(0.0)
}

fn popularity_score(entry: &Entry, enrichments: &EntryEnrichments) -> f64 {
    match youtube_view_count(entry.id, enrichments) {
        Some(view_count) => POPULARITY_SCORE_BASE + view_count + recency_tiebreak(entry),
        None => recency_score(entry),
    }
}

fn youtube_view_count(entry_id: i64, enrichments: &EntryEnrichments) -> Option<f64> {
    enrichments
        .get(&entry_id)?
        .get(YOUTUBE_VIEW_COUNT_KEY)?
        .parse::<f64>()
        .ok()
}

fn recency_tiebreak(entry: &Entry) -> f64 {
    recency_score(entry) / POPULARITY_SCORE_BASE
}

fn sort_scores(scored: &mut [Score]) {
    scored.sort_by(|a, b| b.value.partial_cmp(&a.value).unwrap_or(Ordering::Equal));
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};

    use super::*;
    use crate::config::AdapterType;
    use crate::storage::{NewEntry, NewSource, Storage};

    fn setup_storage_with_entries() -> (Storage, i64) {
        let mut storage = Storage::open_in_memory().unwrap();
        let source_id = storage
            .insert_source(&NewSource {
                name: "Test".into(),
                url: "https://example.com/feed".into(),
                adapter: AdapterType::Rss,
                top_n_override: None,
            })
            .unwrap();

        let entries = vec![
            NewEntry {
                source_id,
                external_id: "old".into(),
                title: "Old Post".into(),
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
                title: "New Post".into(),
                summary: None,
                url: "https://example.com/new".into(),
                thumbnail_url: None,
                author: None,
                published_at: Some(Utc.with_ymd_and_hms(2025, 6, 15, 12, 0, 0).unwrap()),
                enrichments: HashMap::new(),
            },
            NewEntry {
                source_id,
                external_id: "mid".into(),
                title: "Mid Post".into(),
                summary: None,
                url: "https://example.com/mid".into(),
                thumbnail_url: None,
                author: None,
                published_at: Some(Utc.with_ymd_and_hms(2024, 6, 1, 0, 0, 0).unwrap()),
                enrichments: HashMap::new(),
            },
        ];
        storage.upsert_entries(&entries).unwrap();
        (storage, source_id)
    }

    fn empty_ctx(top_n: usize) -> RankContext {
        RankContext {
            top_n,
            enrichments: HashMap::new(),
        }
    }

    #[test]
    fn recency_ranker_sorts_newest_first() {
        let (storage, source_id) = setup_storage_with_entries();
        let entries = storage.list_entries_for_source(source_id).unwrap();

        let ranker = RecencyRanker;
        let ctx = empty_ctx(2);
        let scores = ranker.rank(&entries, &ctx);

        assert_eq!(scores.len(), 3);
        assert!(
            scores[0].value > scores[1].value,
            "first score should be highest"
        );
        assert!(
            scores[1].value > scores[2].value,
            "second score should beat third"
        );

        let new_entry = entries.iter().find(|e| e.external_id == "new").unwrap();
        assert_eq!(scores[0].entry_id, new_entry.id);
    }

    #[test]
    fn recency_ranker_handles_no_published_at() {
        let mut storage = Storage::open_in_memory().unwrap();
        let source_id = storage
            .insert_source(&NewSource {
                name: "Test".into(),
                url: "https://example.com/feed".into(),
                adapter: AdapterType::Rss,
                top_n_override: None,
            })
            .unwrap();

        let entries = vec![
            NewEntry {
                source_id,
                external_id: "dated".into(),
                title: "Dated".into(),
                summary: None,
                url: "https://example.com/dated".into(),
                thumbnail_url: None,
                author: None,
                published_at: Some(Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap()),
                enrichments: HashMap::new(),
            },
            NewEntry {
                source_id,
                external_id: "undated".into(),
                title: "Undated".into(),
                summary: None,
                url: "https://example.com/undated".into(),
                thumbnail_url: None,
                author: None,
                published_at: None,
                enrichments: HashMap::new(),
            },
        ];
        storage.upsert_entries(&entries).unwrap();

        let db_entries = storage.list_entries_for_source(source_id).unwrap();
        let ranker = RecencyRanker;
        let scores = ranker.rank(&db_entries, &empty_ctx(10));

        assert_eq!(scores.len(), 2);
        let dated = db_entries
            .iter()
            .find(|e| e.external_id == "dated")
            .unwrap();
        assert_eq!(
            scores[0].entry_id, dated.id,
            "dated entry should rank first"
        );
    }

    #[test]
    fn recency_ranker_empty_entries() {
        let ranker = RecencyRanker;
        let scores = ranker.rank(&[], &empty_ctx(5));
        assert!(scores.is_empty());
    }

    #[test]
    fn popularity_ranker_prefers_higher_view_counts() {
        let mut storage = Storage::open_in_memory().unwrap();
        let source_id = storage
            .insert_source(&NewSource {
                name: "Videos".into(),
                url: "https://example.com/videos".into(),
                adapter: AdapterType::Youtube,
                top_n_override: None,
            })
            .unwrap();

        let mut lower_views = NewEntry {
            source_id,
            external_id: "lower".into(),
            title: "Lower views".into(),
            summary: None,
            url: "https://example.com/lower".into(),
            thumbnail_url: None,
            author: None,
            published_at: Some(Utc.with_ymd_and_hms(2025, 6, 15, 12, 0, 0).unwrap()),
            enrichments: HashMap::from([(YOUTUBE_VIEW_COUNT_KEY.into(), "100".into())]),
        };
        let higher_views = NewEntry {
            source_id,
            external_id: "higher".into(),
            title: "Higher views".into(),
            summary: None,
            url: "https://example.com/higher".into(),
            thumbnail_url: None,
            author: None,
            published_at: Some(Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap()),
            enrichments: HashMap::from([(YOUTUBE_VIEW_COUNT_KEY.into(), "500".into())]),
        };
        lower_views
            .enrichments
            .insert("youtube_duration".into(), "PT3M14S".into());

        storage
            .upsert_entries(&[lower_views, higher_views])
            .unwrap();
        let entries = storage.list_entries_for_source(source_id).unwrap();
        let enrichments = storage.list_enrichments_for_source(source_id).unwrap();

        let ranker = PopularityRanker;
        let scores = ranker.rank(
            &entries,
            &RankContext {
                top_n: 2,
                enrichments,
            },
        );

        let higher = entries
            .iter()
            .find(|entry| entry.external_id == "higher")
            .unwrap();
        assert_eq!(scores[0].entry_id, higher.id);
    }

    #[test]
    fn popularity_ranker_falls_back_to_recency_without_valid_view_count() {
        let mut storage = Storage::open_in_memory().unwrap();
        let source_id = storage
            .insert_source(&NewSource {
                name: "Videos".into(),
                url: "https://example.com/videos".into(),
                adapter: AdapterType::Youtube,
                top_n_override: None,
            })
            .unwrap();

        let mut malformed = NewEntry {
            source_id,
            external_id: "malformed".into(),
            title: "Malformed".into(),
            summary: None,
            url: "https://example.com/malformed".into(),
            thumbnail_url: None,
            author: None,
            published_at: Some(Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap()),
            enrichments: HashMap::new(),
        };
        malformed
            .enrichments
            .insert(YOUTUBE_VIEW_COUNT_KEY.into(), "not-a-number".into());
        let newer = NewEntry {
            source_id,
            external_id: "newer".into(),
            title: "Newer".into(),
            summary: None,
            url: "https://example.com/newer".into(),
            thumbnail_url: None,
            author: None,
            published_at: Some(Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap()),
            enrichments: HashMap::new(),
        };

        storage.upsert_entries(&[malformed, newer]).unwrap();
        let entries = storage.list_entries_for_source(source_id).unwrap();
        let enrichments = storage.list_enrichments_for_source(source_id).unwrap();

        let ranker = PopularityRanker;
        let scores = ranker.rank(
            &entries,
            &RankContext {
                top_n: 2,
                enrichments,
            },
        );

        let newer = entries
            .iter()
            .find(|entry| entry.external_id == "newer")
            .unwrap();
        assert_eq!(scores[0].entry_id, newer.id);
    }

    #[test]
    fn popularity_ranker_keeps_viewed_entries_above_plain_recency_fallbacks() {
        let mut storage = Storage::open_in_memory().unwrap();
        let source_id = storage
            .insert_source(&NewSource {
                name: "Videos".into(),
                url: "https://example.com/videos".into(),
                adapter: AdapterType::Youtube,
                top_n_override: None,
            })
            .unwrap();

        let popular = NewEntry {
            source_id,
            external_id: "popular".into(),
            title: "Popular".into(),
            summary: None,
            url: "https://example.com/popular".into(),
            thumbnail_url: None,
            author: None,
            published_at: Some(Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap()),
            enrichments: HashMap::from([(YOUTUBE_VIEW_COUNT_KEY.into(), "10".into())]),
        };
        let very_recent = NewEntry {
            source_id,
            external_id: "recent".into(),
            title: "Recent".into(),
            summary: None,
            url: "https://example.com/recent".into(),
            thumbnail_url: None,
            author: None,
            published_at: Some(Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap()),
            enrichments: HashMap::new(),
        };

        storage.upsert_entries(&[popular, very_recent]).unwrap();
        let entries = storage.list_entries_for_source(source_id).unwrap();
        let enrichments = storage.list_enrichments_for_source(source_id).unwrap();

        let ranker = PopularityRanker;
        let scores = ranker.rank(
            &entries,
            &RankContext {
                top_n: 2,
                enrichments,
            },
        );

        let popular = entries
            .iter()
            .find(|entry| entry.external_id == "popular")
            .unwrap();
        assert_eq!(scores[0].entry_id, popular.id);
    }
}
