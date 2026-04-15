use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
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
                 ON CONFLICT(source_id, external_id) DO UPDATE SET external_id = external_id \
                 RETURNING id",
            )?;
            let mut check_exists_stmt = tx.prepare(
                "SELECT id FROM entries WHERE source_id = ?1 AND external_id = ?2",
            )?;
            let mut insert_enrichment_stmt = tx.prepare(
                "INSERT INTO enrichments (entry_id, key, value) \
                 VALUES (?1, ?2, ?3) \
                 ON CONFLICT(entry_id, key) DO UPDATE SET value = excluded.value",
            )?;
            
            let now = Utc::now();
            for entry in entries {
                let id_opt: Option<i64> = insert_entry_stmt.query_row(params![
                    entry.source_id,
                    entry.external_id,
                    entry.title,
                    entry.summary,
                    entry.url,
                    entry.thumbnail_url,
                    entry.author,
                    entry.published_at,
                    now,
                ], |row| row.get(0)).optional()?;
                
                let (entry_id, is_new) = match id_opt {
                    Some(id) => (id, true),
                    None => {
                        let id: i64 = check_exists_stmt.query_row(params![entry.source_id, entry.external_id], |row| row.get(0))?;
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

    pub fn set_entry_state(&mut self, id: i64, state: EntryState) -> Result<(), StorageError> {
        self.conn.execute(
            "UPDATE entries SET state = ?1 WHERE id = ?2",
            params![state, id],
        )?;
        Ok(())
    }

    pub fn list_top_n_entries(&self) -> Result<Vec<Entry>, StorageError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, source_id, external_id, title, summary, url, thumbnail_url, \
                    author, published_at, fetched_at, state, rating, score, \
                    displayed_in_top_n \
             FROM entries WHERE displayed_in_top_n = 1 \
             ORDER BY published_at DESC",
        )?;
        let rows = stmt.query_map([], row_to_entry)?;
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
            Score { entry_id: db_entries[0].id, value: 30.0 },
            Score { entry_id: db_entries[1].id, value: 20.0 },
            Score { entry_id: db_entries[2].id, value: 10.0 },
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
}
