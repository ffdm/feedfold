use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Local, NaiveDate, Utc};
use directories::ProjectDirs;
use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSql, ToSqlOutput, ValueRef};
use rusqlite::{params, Connection, OptionalExtension};
use thiserror::Error;

use crate::config::AdapterType;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub id: i64,
    pub name: String,
    pub url: String,
    pub adapter: AdapterType,
    pub top_n_override: Option<u32>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct NewSource {
    pub name: String,
    pub url: String,
    pub adapter: AdapterType,
    pub top_n_override: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryState {
    New,
    Viewed,
    Starred,
}

impl EntryState {
    pub fn as_canonical_str(self) -> &'static str {
        match self {
            EntryState::New => "new",
            EntryState::Viewed => "viewed",
            EntryState::Starred => "starred",
        }
    }

    pub fn from_canonical_str(s: &str) -> Option<Self> {
        match s {
            "new" => Some(EntryState::New),
            "viewed" => Some(EntryState::Viewed),
            "starred" => Some(EntryState::Starred),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub id: i64,
    pub source_id: i64,
    pub external_id: String,
    pub title: String,
    pub summary: Option<String>,
    pub url: String,
    pub thumbnail_url: Option<String>,
    pub author: Option<String>,
    pub published_at: Option<DateTime<Utc>>,
    pub fetched_at: DateTime<Utc>,
    pub state: EntryState,
    pub rating: Option<u8>,
    pub score: Option<f64>,
    pub displayed_in_top_n: bool,
}

#[derive(Debug, Clone)]
pub struct NewEntry {
    pub source_id: i64,
    pub external_id: String,
    pub title: String,
    pub summary: Option<String>,
    pub url: String,
    pub thumbnail_url: Option<String>,
    pub author: Option<String>,
    pub published_at: Option<DateTime<Utc>>,
    pub enrichments: HashMap<String, String>,
}

impl ToSql for AdapterType {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(ToSqlOutput::from(self.as_canonical_str()))
    }
}

impl FromSql for AdapterType {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        let s = value.as_str()?;
        AdapterType::from_canonical_str(s)
            .ok_or_else(|| FromSqlError::Other(format!("unknown adapter_type {s:?}").into()))
    }
}

impl ToSql for EntryState {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(ToSqlOutput::from(self.as_canonical_str()))
    }
}

impl FromSql for EntryState {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        let s = value.as_str()?;
        EntryState::from_canonical_str(s)
            .ok_or_else(|| FromSqlError::Other(format!("unknown entry state {s:?}").into()))
    }
}

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("no user data directory could be determined for this platform")]
    NoDataDir,

    #[error("rating must be between 1 and 5, got {0}")]
    InvalidRating(u8),

    #[error("opening database at {path}")]
    Open {
        path: PathBuf,
        #[source]
        source: rusqlite::Error,
    },

    #[error("running migration step {step}")]
    Migration {
        step: &'static str,
        #[source]
        source: rusqlite::Error,
    },

    #[error("database query")]
    Query(#[from] rusqlite::Error),
}

const SCHEMA_V1: &str = r#"
CREATE TABLE IF NOT EXISTS sources (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    name            TEXT    NOT NULL,
    url             TEXT    NOT NULL UNIQUE,
    adapter_type    TEXT    NOT NULL,
    top_n_override  INTEGER,
    created_at      TEXT    NOT NULL
);

CREATE TABLE IF NOT EXISTS entries (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    source_id           INTEGER NOT NULL REFERENCES sources(id) ON DELETE CASCADE,
    external_id         TEXT    NOT NULL,
    title               TEXT    NOT NULL,
    summary             TEXT,
    url                 TEXT    NOT NULL,
    thumbnail_url       TEXT,
    author              TEXT,
    published_at        TEXT,
    fetched_at          TEXT    NOT NULL,
    state               TEXT    NOT NULL DEFAULT 'new',
    rating              INTEGER,
    score               REAL,
    displayed_in_top_n  INTEGER NOT NULL DEFAULT 0,
    UNIQUE(source_id, external_id)
);

CREATE INDEX IF NOT EXISTS entries_source_id_idx ON entries(source_id);
CREATE INDEX IF NOT EXISTS entries_published_idx ON entries(published_at);

CREATE TABLE IF NOT EXISTS enrichments (
    entry_id INTEGER NOT NULL REFERENCES entries(id) ON DELETE CASCADE,
    key      TEXT    NOT NULL,
    value    TEXT    NOT NULL,
    PRIMARY KEY (entry_id, key)
);

CREATE TABLE IF NOT EXISTS daily_views (
    date      TEXT    NOT NULL,
    entry_id  INTEGER NOT NULL REFERENCES entries(id) ON DELETE CASCADE,
    viewed_at TEXT    NOT NULL,
    PRIMARY KEY (date, entry_id)
);
"#;

pub struct Storage {
    conn: Connection,
}

impl Storage {
    pub fn default_path() -> Result<PathBuf, StorageError> {
        let dirs = ProjectDirs::from("", "", "feedfold").ok_or(StorageError::NoDataDir)?;
        Ok(dirs.data_dir().join("feedfold.db"))
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self, StorageError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|_| StorageError::Open {
                path: path.to_path_buf(),
                source: rusqlite::Error::InvalidPath(path.to_path_buf()),
            })?;
        }
        let conn = Connection::open(path).map_err(|source| StorageError::Open {
            path: path.to_path_buf(),
            source,
        })?;
        Self::from_connection(conn)
    }

    pub fn open_in_memory() -> Result<Self, StorageError> {
        let conn = Connection::open_in_memory().map_err(|source| StorageError::Open {
            path: PathBuf::from(":memory:"),
            source,
        })?;
        Self::from_connection(conn)
    }

    fn from_connection(conn: Connection) -> Result<Self, StorageError> {
        conn.pragma_update(None, "foreign_keys", "ON")
            .map_err(|source| StorageError::Migration {
                step: "enable foreign keys",
                source,
            })?;
        conn.execute_batch(SCHEMA_V1)
            .map_err(|source| StorageError::Migration {
                step: "schema v1",
                source,
            })?;
        Ok(Self { conn })
    }

    pub fn insert_source(&self, new: &NewSource) -> Result<i64, StorageError> {
        let now = Utc::now();
        self.conn.execute(
            "INSERT INTO sources (name, url, adapter_type, top_n_override, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![new.name, new.url, new.adapter, new.top_n_override, now],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn list_sources(&self) -> Result<Vec<Source>, StorageError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, url, adapter_type, top_n_override, created_at \
             FROM sources ORDER BY name",
        )?;
        let rows = stmt.query_map([], row_to_source)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn source_by_url(&self, url: &str) -> Result<Option<Source>, StorageError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, url, adapter_type, top_n_override, created_at \
             FROM sources WHERE url = ?1",
        )?;
        stmt.query_row([url], row_to_source)
            .optional()
            .map_err(Into::into)
    }

    pub fn upsert_entries(&mut self, entries: &[NewEntry]) -> Result<usize, StorageError> {
        let tx = self.conn.transaction()?;
        let mut inserted = 0usize;
        {
            let mut insert_entry_stmt = tx.prepare(
                "INSERT INTO entries \
                    (source_id, external_id, title, summary, url, thumbnail_url, \
                     author, published_at, fetched_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9) \
                 ON CONFLICT(source_id, external_id) DO NOTHING \
                 RETURNING id",
            )?;
            let mut check_exists_stmt =
                tx.prepare("SELECT id FROM entries WHERE source_id = ?1 AND external_id = ?2")?;
            let mut insert_enrichment_stmt = tx.prepare(
                "INSERT INTO enrichments (entry_id, key, value) \
                 VALUES (?1, ?2, ?3) \
                 ON CONFLICT(entry_id, key) DO UPDATE SET value = excluded.value",
            )?;

            let now = Utc::now();
            for entry in entries {
                let id_opt: Option<i64> = insert_entry_stmt
                    .query_row(
                        params![
                            entry.source_id,
                            entry.external_id,
                            entry.title,
                            entry.summary,
                            entry.url,
                            entry.thumbnail_url,
                            entry.author,
                            entry.published_at,
                            now,
                        ],
                        |row| row.get(0),
                    )
                    .optional()?;

                let (entry_id, is_new) = match id_opt {
                    Some(id) => (id, true),
                    None => {
                        let id: i64 = check_exists_stmt
                            .query_row(params![entry.source_id, entry.external_id], |row| {
                                row.get(0)
                            })?;
                        (id, false)
                    }
                };

                if is_new {
                    inserted += 1;
                }

                for (key, value) in &entry.enrichments {
                    insert_enrichment_stmt.execute(params![entry_id, key, value])?;
                }
            }
        }
        tx.commit()?;
        Ok(inserted)
    }

    pub fn apply_ranking(
        &mut self,
        source_id: i64,
        scores: &[crate::ranker::Score],
        top_n: usize,
    ) -> Result<(), StorageError> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "UPDATE entries SET displayed_in_top_n = 0, score = NULL WHERE source_id = ?1",
            [source_id],
        )?;
        for (rank, score) in scores.iter().enumerate() {
            let in_top_n = rank < top_n;
            tx.execute(
                "UPDATE entries SET score = ?1, displayed_in_top_n = ?2 WHERE id = ?3",
                params![score.value, in_top_n, score.entry_id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn list_enrichments_for_source(
        &self,
        source_id: i64,
    ) -> Result<HashMap<i64, HashMap<String, String>>, StorageError> {
        let mut stmt = self.conn.prepare(
            "SELECT enrichments.entry_id, enrichments.key, enrichments.value \
             FROM enrichments \
             INNER JOIN entries ON entries.id = enrichments.entry_id \
             WHERE entries.source_id = ?1 \
             ORDER BY enrichments.entry_id, enrichments.key",
        )?;
        let mut rows = stmt.query([source_id])?;
        let mut enrichments = HashMap::new();

        while let Some(row) = rows.next()? {
            let entry_id: i64 = row.get(0)?;
            let key: String = row.get(1)?;
            let value: String = row.get(2)?;
            enrichments
                .entry(entry_id)
                .or_insert_with(HashMap::new)
                .insert(key, value);
        }

        Ok(enrichments)
    }

    pub fn set_entry_state(&mut self, id: i64, state: EntryState) -> Result<(), StorageError> {
        self.conn.execute(
            "UPDATE entries SET state = ?1 WHERE id = ?2",
            params![state, id],
        )?;
        Ok(())
    }

    pub fn set_entry_rating(&mut self, id: i64, rating: u8) -> Result<(), StorageError> {
        if !(1..=5).contains(&rating) {
            return Err(StorageError::InvalidRating(rating));
        }

        self.conn.execute(
            "UPDATE entries SET rating = ?1 WHERE id = ?2",
            params![rating, id],
        )?;
        Ok(())
    }

    pub fn record_entry_view(&mut self, id: i64) -> Result<(), StorageError> {
        let now = Local::now();
        let viewed_at = now.with_timezone(&Utc);
        let viewed_on = now.date_naive();
        self.record_entry_view_at(id, viewed_at, viewed_on)
    }

    pub fn count_entries_viewed_today(&self) -> Result<usize, StorageError> {
        self.count_entries_viewed_on(Local::now().date_naive())
    }

    pub fn list_top_n_entries(&self) -> Result<Vec<Entry>, StorageError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, source_id, external_id, title, summary, url, thumbnail_url, \
                    author, published_at, fetched_at, state, rating, score, \
                    displayed_in_top_n \
             FROM entries WHERE displayed_in_top_n = 1 \
             ORDER BY score DESC, published_at DESC",
        )?;
        let rows = stmt.query_map([], row_to_entry)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn list_viewed_entries(&self) -> Result<Vec<Entry>, StorageError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, source_id, external_id, title, summary, url, thumbnail_url, \
                    author, published_at, fetched_at, state, rating, score, \
                    displayed_in_top_n \
             FROM entries WHERE state IN (?1, ?2) \
             ORDER BY published_at DESC, fetched_at DESC",
        )?;
        let rows = stmt.query_map([EntryState::Viewed, EntryState::Starred], row_to_entry)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn list_overflow_entries(&self) -> Result<Vec<Entry>, StorageError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, source_id, external_id, title, summary, url, thumbnail_url, \
                    author, published_at, fetched_at, state, rating, score, \
                    displayed_in_top_n \
             FROM entries WHERE state = ?1 AND displayed_in_top_n = 0 \
             ORDER BY score DESC, published_at DESC, fetched_at DESC",
        )?;
        let rows = stmt.query_map([EntryState::New], row_to_entry)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn list_entries_for_source(&self, source_id: i64) -> Result<Vec<Entry>, StorageError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, source_id, external_id, title, summary, url, thumbnail_url, \
                    author, published_at, fetched_at, state, rating, score, \
                    displayed_in_top_n \
             FROM entries WHERE source_id = ?1 \
             ORDER BY published_at DESC",
        )?;
        let rows = stmt.query_map([source_id], row_to_entry)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    fn record_entry_view_at(
        &mut self,
        id: i64,
        viewed_at: DateTime<Utc>,
        viewed_on: NaiveDate,
    ) -> Result<(), StorageError> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "UPDATE entries SET state = ?1 WHERE id = ?2 AND state = ?3",
            params![EntryState::Viewed, id, EntryState::New],
        )?;
        tx.execute(
            "INSERT INTO daily_views (date, entry_id, viewed_at) VALUES (?1, ?2, ?3) \
             ON CONFLICT(date, entry_id) DO UPDATE SET viewed_at = excluded.viewed_at",
            params![viewed_on.to_string(), id, viewed_at],
        )?;
        tx.commit()?;
        Ok(())
    }

    fn count_entries_viewed_on(&self, viewed_on: NaiveDate) -> Result<usize, StorageError> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM daily_views WHERE date = ?1",
            [viewed_on.to_string()],
            |row| row.get(0),
        )?;
        Ok(count as usize)
    }
}

fn row_to_source(row: &rusqlite::Row<'_>) -> rusqlite::Result<Source> {
    Ok(Source {
        id: row.get(0)?,
        name: row.get(1)?,
        url: row.get(2)?,
        adapter: row.get(3)?,
        top_n_override: row.get(4)?,
        created_at: row.get(5)?,
    })
}

fn row_to_entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<Entry> {
    Ok(Entry {
        id: row.get(0)?,
        source_id: row.get(1)?,
        external_id: row.get(2)?,
        title: row.get(3)?,
        summary: row.get(4)?,
        url: row.get(5)?,
        thumbnail_url: row.get(6)?,
        author: row.get(7)?,
        published_at: row.get(8)?,
        fetched_at: row.get(9)?,
        state: row.get(10)?,
        rating: row.get(11)?,
        score: row.get(12)?,
        displayed_in_top_n: row.get(13)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn new_source(name: &str, url: &str) -> NewSource {
        NewSource {
            name: name.into(),
            url: url.into(),
            adapter: AdapterType::Rss,
            top_n_override: None,
        }
    }

    fn new_entry(source_id: i64, external_id: &str, title: &str) -> NewEntry {
        NewEntry {
            source_id,
            external_id: external_id.into(),
            title: title.into(),
            summary: None,
            url: format!("https://example.com/{external_id}"),
            thumbnail_url: None,
            author: None,
            published_at: None,
            enrichments: HashMap::new(),
        }
    }

    #[test]
    fn open_in_memory_creates_schema() {
        let storage = Storage::open_in_memory().expect("open");
        assert!(storage.list_sources().unwrap().is_empty());
    }

    #[test]
    fn insert_and_list_sources() {
        let storage = Storage::open_in_memory().unwrap();
        let id = storage
            .insert_source(&new_source("Blog", "https://a.example/feed.xml"))
            .unwrap();
        assert!(id > 0);

        let listed = storage.list_sources().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, id);
        assert_eq!(listed[0].name, "Blog");
        assert_eq!(listed[0].url, "https://a.example/feed.xml");
        assert_eq!(listed[0].adapter, AdapterType::Rss);
        assert_eq!(listed[0].top_n_override, None);
    }

    #[test]
    fn source_by_url_roundtrip() {
        let storage = Storage::open_in_memory().unwrap();
        let url = "https://a.example/feed.xml";

        assert!(storage.source_by_url(url).unwrap().is_none());

        let id = storage.insert_source(&new_source("Blog", url)).unwrap();
        let found = storage.source_by_url(url).unwrap().expect("found");
        assert_eq!(found.id, id);
        assert_eq!(found.url, url);
    }

    #[test]
    fn unique_url_constraint() {
        let storage = Storage::open_in_memory().unwrap();
        storage
            .insert_source(&new_source("Blog", "https://a.example/feed.xml"))
            .unwrap();
        let err = storage.insert_source(&new_source("Dup", "https://a.example/feed.xml"));
        assert!(err.is_err(), "duplicate url should be rejected");
    }

    #[test]
    fn upsert_entries_deduplicates_and_lists() {
        let mut storage = Storage::open_in_memory().unwrap();
        let source_id = storage
            .insert_source(&new_source("Blog", "https://a.example/feed.xml"))
            .unwrap();

        let entries = vec![
            new_entry(source_id, "a", "Entry A"),
            new_entry(source_id, "b", "Entry B"),
        ];
        assert_eq!(storage.upsert_entries(&entries).unwrap(), 2);
        assert_eq!(storage.upsert_entries(&entries).unwrap(), 0);

        let mut more = entries.clone();
        more.push(new_entry(source_id, "c", "Entry C"));
        assert_eq!(storage.upsert_entries(&more).unwrap(), 1);

        let listed = storage.list_entries_for_source(source_id).unwrap();
        assert_eq!(listed.len(), 3);
        assert!(listed.iter().all(|e| e.state == EntryState::New));
        assert!(listed.iter().all(|e| !e.displayed_in_top_n));
    }

    #[test]
    fn upsert_entries_persists_and_updates_enrichments() {
        let mut storage = Storage::open_in_memory().unwrap();
        let source_id = storage
            .insert_source(&new_source("Blog", "https://a.example/feed.xml"))
            .unwrap();

        let mut first = new_entry(source_id, "a", "Entry A");
        first
            .enrichments
            .insert("youtube_view_count".into(), "42".into());
        first
            .enrichments
            .insert("youtube_duration".into(), "PT3M14S".into());

        assert_eq!(storage.upsert_entries(&[first]).unwrap(), 1);

        let entry_id: i64 = storage
            .conn
            .query_row(
                "SELECT id FROM entries WHERE source_id = ?1 AND external_id = ?2",
                params![source_id, "a"],
                |row| row.get(0),
            )
            .unwrap();

        let mut updated = new_entry(source_id, "a", "Entry A");
        updated
            .enrichments
            .insert("youtube_view_count".into(), "100".into());
        updated
            .enrichments
            .insert("youtube_like_count".into(), "7".into());

        assert_eq!(storage.upsert_entries(&[updated]).unwrap(), 0);

        let mut stmt = storage
            .conn
            .prepare("SELECT key, value FROM enrichments WHERE entry_id = ?1 ORDER BY key")
            .unwrap();
        let enrichments = stmt
            .query_map([entry_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .unwrap()
            .collect::<Result<HashMap<_, _>, _>>()
            .unwrap();

        assert_eq!(
            enrichments.get("youtube_duration").map(|s| s.as_str()),
            Some("PT3M14S")
        );
        assert_eq!(
            enrichments.get("youtube_view_count").map(|s| s.as_str()),
            Some("100")
        );
        assert_eq!(
            enrichments.get("youtube_like_count").map(|s| s.as_str()),
            Some("7")
        );
    }

    #[test]
    fn list_enrichments_for_source_groups_by_entry() {
        let mut storage = Storage::open_in_memory().unwrap();
        let source_id = storage
            .insert_source(&new_source("Blog", "https://a.example/feed.xml"))
            .unwrap();

        let mut first = new_entry(source_id, "a", "Entry A");
        first
            .enrichments
            .insert("youtube_view_count".into(), "42".into());
        first
            .enrichments
            .insert("youtube_duration".into(), "PT3M14S".into());

        let mut second = new_entry(source_id, "b", "Entry B");
        second
            .enrichments
            .insert("youtube_view_count".into(), "100".into());

        storage.upsert_entries(&[first, second]).unwrap();
        let entries = storage.list_entries_for_source(source_id).unwrap();
        let first_id = entries
            .iter()
            .find(|entry| entry.external_id == "a")
            .map(|entry| entry.id)
            .unwrap();
        let second_id = entries
            .iter()
            .find(|entry| entry.external_id == "b")
            .map(|entry| entry.id)
            .unwrap();
        let enrichments = storage.list_enrichments_for_source(source_id).unwrap();

        assert_eq!(enrichments.len(), 2);
        assert_eq!(enrichments.get(&first_id).map(HashMap::len), Some(2));
        assert_eq!(enrichments.get(&second_id).map(HashMap::len), Some(1));
    }

    #[test]
    fn apply_ranking_sets_scores_and_top_n() {
        use crate::ranker::Score;

        let mut storage = Storage::open_in_memory().unwrap();
        let source_id = storage
            .insert_source(&new_source("Blog", "https://a.example/feed.xml"))
            .unwrap();

        let entries = vec![
            new_entry(source_id, "a", "Entry A"),
            new_entry(source_id, "b", "Entry B"),
            new_entry(source_id, "c", "Entry C"),
        ];
        storage.upsert_entries(&entries).unwrap();
        let db_entries = storage.list_entries_for_source(source_id).unwrap();

        let scores = vec![
            Score {
                entry_id: db_entries[0].id,
                value: 30.0,
            },
            Score {
                entry_id: db_entries[1].id,
                value: 20.0,
            },
            Score {
                entry_id: db_entries[2].id,
                value: 10.0,
            },
        ];
        storage.apply_ranking(source_id, &scores, 2).unwrap();

        let after = storage.list_entries_for_source(source_id).unwrap();
        let a = after.iter().find(|e| e.external_id == "a").unwrap();
        let b = after.iter().find(|e| e.external_id == "b").unwrap();
        let c = after.iter().find(|e| e.external_id == "c").unwrap();

        assert_eq!(a.score, Some(30.0));
        assert_eq!(b.score, Some(20.0));
        assert_eq!(c.score, Some(10.0));
        assert!(a.displayed_in_top_n);
        assert!(b.displayed_in_top_n);
        assert!(!c.displayed_in_top_n);
    }

    #[test]
    fn list_top_n_entries_orders_by_score_then_date() {
        use crate::ranker::Score;

        let mut storage = Storage::open_in_memory().unwrap();
        let source_id = storage
            .insert_source(&new_source("Blog", "https://a.example/feed.xml"))
            .unwrap();

        let mut low = new_entry(source_id, "low", "Low");
        low.published_at = Some(Utc::now());

        let mut high = new_entry(source_id, "high", "High");
        high.published_at = Some(Utc::now());

        storage.upsert_entries(&[low, high]).unwrap();
        let entries = storage.list_entries_for_source(source_id).unwrap();
        let low = entries
            .iter()
            .find(|entry| entry.external_id == "low")
            .unwrap();
        let high = entries
            .iter()
            .find(|entry| entry.external_id == "high")
            .unwrap();

        storage
            .apply_ranking(
                source_id,
                &[
                    Score {
                        entry_id: low.id,
                        value: 10.0,
                    },
                    Score {
                        entry_id: high.id,
                        value: 20.0,
                    },
                ],
                2,
            )
            .unwrap();

        let top = storage.list_top_n_entries().unwrap();
        assert_eq!(top[0].external_id, "high");
        assert_eq!(top[1].external_id, "low");
    }

    #[test]
    fn set_entry_rating_persists_rating() {
        let mut storage = Storage::open_in_memory().unwrap();
        let source_id = storage
            .insert_source(&new_source("Blog", "https://a.example/feed.xml"))
            .unwrap();

        storage
            .upsert_entries(&[new_entry(source_id, "a", "Entry A")])
            .unwrap();
        let entry = storage
            .list_entries_for_source(source_id)
            .unwrap()
            .pop()
            .unwrap();

        storage.set_entry_rating(entry.id, 4).unwrap();

        let updated = storage
            .list_entries_for_source(source_id)
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(updated.rating, Some(4));
    }

    #[test]
    fn set_entry_rating_rejects_out_of_range_values() {
        let mut storage = Storage::open_in_memory().unwrap();
        let source_id = storage
            .insert_source(&new_source("Blog", "https://a.example/feed.xml"))
            .unwrap();

        storage
            .upsert_entries(&[new_entry(source_id, "a", "Entry A")])
            .unwrap();
        let entry = storage
            .list_entries_for_source(source_id)
            .unwrap()
            .pop()
            .unwrap();

        let err = storage.set_entry_rating(entry.id, 0).unwrap_err();
        assert!(matches!(err, StorageError::InvalidRating(0)));
    }

    #[test]
    fn record_entry_view_marks_entry_viewed_and_counts_unique_daily_views() {
        let mut storage = Storage::open_in_memory().unwrap();
        let source_id = storage
            .insert_source(&new_source("Blog", "https://a.example/feed.xml"))
            .unwrap();

        storage
            .upsert_entries(&[
                new_entry(source_id, "a", "Entry A"),
                new_entry(source_id, "b", "Entry B"),
            ])
            .unwrap();
        let entries = storage.list_entries_for_source(source_id).unwrap();
        let first = entries
            .iter()
            .find(|entry| entry.external_id == "a")
            .unwrap();
        let second = entries
            .iter()
            .find(|entry| entry.external_id == "b")
            .unwrap();
        let viewed_on = NaiveDate::from_ymd_opt(2026, 4, 15).unwrap();
        let first_seen_at = Utc::now();

        storage
            .record_entry_view_at(first.id, first_seen_at, viewed_on)
            .unwrap();
        storage
            .record_entry_view_at(first.id, Utc::now(), viewed_on)
            .unwrap();
        storage
            .record_entry_view_at(second.id, Utc::now(), viewed_on)
            .unwrap();

        let viewed_entries = storage.list_viewed_entries().unwrap();
        assert_eq!(storage.count_entries_viewed_on(viewed_on).unwrap(), 2);
        assert_eq!(viewed_entries.len(), 2);
        assert!(viewed_entries
            .iter()
            .all(|entry| entry.state == EntryState::Viewed));
    }

    #[test]
    fn list_viewed_entries_excludes_new_entries() {
        let mut storage = Storage::open_in_memory().unwrap();
        let source_id = storage
            .insert_source(&new_source("Blog", "https://a.example/feed.xml"))
            .unwrap();

        storage
            .upsert_entries(&[
                new_entry(source_id, "new", "New Entry"),
                new_entry(source_id, "viewed", "Viewed Entry"),
            ])
            .unwrap();
        let viewed_entry = storage
            .list_entries_for_source(source_id)
            .unwrap()
            .into_iter()
            .find(|entry| entry.external_id == "viewed")
            .unwrap();

        storage
            .record_entry_view_at(
                viewed_entry.id,
                Utc::now(),
                NaiveDate::from_ymd_opt(2026, 4, 15).unwrap(),
            )
            .unwrap();

        let viewed_entries = storage.list_viewed_entries().unwrap();
        assert_eq!(viewed_entries.len(), 1);
        assert_eq!(viewed_entries[0].external_id, "viewed");
    }

    #[test]
    fn list_viewed_entries_includes_starred_entries() {
        let mut storage = Storage::open_in_memory().unwrap();
        let source_id = storage
            .insert_source(&new_source("Blog", "https://a.example/feed.xml"))
            .unwrap();

        storage
            .upsert_entries(&[
                new_entry(source_id, "new", "New Entry"),
                new_entry(source_id, "viewed", "Viewed Entry"),
                new_entry(source_id, "starred", "Starred Entry"),
            ])
            .unwrap();
        let entries = storage.list_entries_for_source(source_id).unwrap();
        let viewed = entries
            .iter()
            .find(|entry| entry.external_id == "viewed")
            .unwrap();
        let starred = entries
            .iter()
            .find(|entry| entry.external_id == "starred")
            .unwrap();

        storage.record_entry_view(viewed.id).unwrap();
        storage
            .set_entry_state(starred.id, EntryState::Starred)
            .unwrap();

        let viewed_entries = storage.list_viewed_entries().unwrap();
        assert_eq!(viewed_entries.len(), 2);
        assert!(viewed_entries
            .iter()
            .any(|entry| entry.external_id == "viewed"));
        assert!(viewed_entries
            .iter()
            .any(|entry| entry.external_id == "starred" && entry.state == EntryState::Starred));
    }

    #[test]
    fn list_overflow_entries_returns_only_new_entries_outside_top_n() {
        use crate::ranker::Score;

        let mut storage = Storage::open_in_memory().unwrap();
        let source_id = storage
            .insert_source(&new_source("Blog", "https://a.example/feed.xml"))
            .unwrap();

        storage
            .upsert_entries(&[
                new_entry(source_id, "top", "Top Entry"),
                new_entry(source_id, "overflow", "Overflow Entry"),
                new_entry(source_id, "viewed", "Viewed Entry"),
            ])
            .unwrap();
        let entries = storage.list_entries_for_source(source_id).unwrap();
        let top = entries
            .iter()
            .find(|entry| entry.external_id == "top")
            .unwrap();
        let overflow = entries
            .iter()
            .find(|entry| entry.external_id == "overflow")
            .unwrap();
        let viewed = entries
            .iter()
            .find(|entry| entry.external_id == "viewed")
            .unwrap();

        storage
            .apply_ranking(
                source_id,
                &[
                    Score {
                        entry_id: top.id,
                        value: 30.0,
                    },
                    Score {
                        entry_id: overflow.id,
                        value: 20.0,
                    },
                    Score {
                        entry_id: viewed.id,
                        value: 10.0,
                    },
                ],
                1,
            )
            .unwrap();
        storage.record_entry_view(viewed.id).unwrap();

        let overflow_entries = storage.list_overflow_entries().unwrap();
        assert_eq!(overflow_entries.len(), 1);
        assert_eq!(overflow_entries[0].external_id, "overflow");
        assert_eq!(overflow_entries[0].state, EntryState::New);
        assert!(!overflow_entries[0].displayed_in_top_n);
    }

    #[test]
    fn list_overflow_entries_orders_by_score_then_date() {
        use crate::ranker::Score;

        let mut storage = Storage::open_in_memory().unwrap();
        let source_id = storage
            .insert_source(&new_source("Blog", "https://a.example/feed.xml"))
            .unwrap();

        let mut first = new_entry(source_id, "older-higher", "Older Higher");
        first.published_at = Some(Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).single().unwrap());
        let mut second = new_entry(source_id, "newer-lower", "Newer Lower");
        second.published_at = Some(Utc.with_ymd_and_hms(2024, 1, 2, 0, 0, 0).single().unwrap());

        storage.upsert_entries(&[first, second]).unwrap();
        let entries = storage.list_entries_for_source(source_id).unwrap();
        let older_higher = entries
            .iter()
            .find(|entry| entry.external_id == "older-higher")
            .unwrap();
        let newer_lower = entries
            .iter()
            .find(|entry| entry.external_id == "newer-lower")
            .unwrap();

        storage
            .apply_ranking(
                source_id,
                &[
                    Score {
                        entry_id: older_higher.id,
                        value: 50.0,
                    },
                    Score {
                        entry_id: newer_lower.id,
                        value: 10.0,
                    },
                ],
                0,
            )
            .unwrap();

        let overflow_entries = storage.list_overflow_entries().unwrap();
        assert_eq!(overflow_entries.len(), 2);
        assert_eq!(overflow_entries[0].external_id, "older-higher");
        assert_eq!(overflow_entries[1].external_id, "newer-lower");
    }
}
