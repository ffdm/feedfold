use crate::storage::Entry;

pub struct RankContext {
    pub top_n: usize,
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
            .map(|e| {
                let value = e
                    .published_at
                    .map(|dt| dt.timestamp() as f64)
                    .unwrap_or(0.0);
                Score {
                    entry_id: e.id,
                    value,
                }
            })
            .collect();
        scored.sort_by(|a, b| b.value.partial_cmp(&a.value).unwrap_or(std::cmp::Ordering::Equal));
        scored
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

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

    #[test]
    fn recency_ranker_sorts_newest_first() {
        let (storage, source_id) = setup_storage_with_entries();
        let entries = storage.list_entries_for_source(source_id).unwrap();

        let ranker = RecencyRanker;
        let ctx = RankContext { top_n: 2 };
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
        let scores = ranker.rank(&db_entries, &RankContext { top_n: 10 });

        assert_eq!(scores.len(), 2);
        let dated = db_entries.iter().find(|e| e.external_id == "dated").unwrap();
        assert_eq!(scores[0].entry_id, dated.id, "dated entry should rank first");
    }

    #[test]
    fn recency_ranker_empty_entries() {
        let ranker = RecencyRanker;
        let scores = ranker.rank(&[], &RankContext { top_n: 5 });
        assert!(scores.is_empty());
    }
}
