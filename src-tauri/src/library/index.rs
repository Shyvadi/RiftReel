//! The library's index (doc 03 §7): a SQLite cache of the match folders' `match.json`
//! sidecars and the fixture recordings, so the window pages, filters and sorts hundreds
//! of matches without reading every file. The sidecars stay the truth: the index is
//! rebuilt from them when it is missing, unreadable or of another schema, and a sync
//! re-reads only the sidecars whose size or modification time changed (one stat each).
//! When the file can't be used at all, an index in memory stands in.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use rusqlite::{params, params_from_iter, Connection, OptionalExtension, ToSql};
use serde::{Deserialize, Serialize};

/// Bumped when the tables change: an index of another schema is rebuilt.
const SCHEMA: i64 = 1;

const TABLES: &str = "
CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE matches (
    id TEXT PRIMARY KEY,
    source TEXT NOT NULL,
    when_ms INTEGER NOT NULL,
    champion TEXT,
    queue TEXT,
    game_mode TEXT,
    result TEXT,
    kills INTEGER,
    deaths INTEGER,
    assists INTEGER,
    duration_s REAL,
    moments INTEGER NOT NULL,
    multikills INTEGER NOT NULL,
    clips INTEGER NOT NULL,
    favorite_clips INTEGER NOT NULL,
    video INTEGER NOT NULL,
    thumbnail TEXT,
    sidecar_ns INTEGER NOT NULL,
    sidecar_len INTEGER NOT NULL,
    video_len INTEGER NOT NULL
);
CREATE INDEX matches_by_time ON matches (when_ms DESC);
CREATE TABLE clips (
    match_id TEXT NOT NULL REFERENCES matches (id) ON DELETE CASCADE,
    file TEXT NOT NULL,
    label TEXT NOT NULL,
    from_s REAL NOT NULL,
    to_s REAL NOT NULL,
    favorite INTEGER NOT NULL,
    PRIMARY KEY (match_id, file)
);
";

/// Matches per page when a query doesn't say, and the most it may ask for.
const DEFAULT_LIMIT: u32 = 100;
const MAX_LIMIT: u32 = 500;

/// Where an entry comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// Tracked live by Downplayed.
    Game,
    /// A fixture recorded by `downplayed-dev record`.
    Recording,
    /// A video imported from Outplayed (by reference).
    Outplayed,
}

impl Source {
    fn as_str(self) -> &'static str {
        match self {
            Self::Game => "game",
            Self::Recording => "recording",
            Self::Outplayed => "outplayed",
        }
    }

    fn parse(text: &str) -> Self {
        match text {
            "recording" => Self::Recording,
            "outplayed" => Self::Outplayed,
            _ => Self::Game,
        }
    }
}

/// What a sidecar looked like when it was read: if any of it differs now, it is read
/// again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stamp {
    /// The sidecar's modification time (Unix ns) and size.
    pub modified_ns: i64,
    pub len: i64,
    /// The video's size, or -1 when there is none.
    pub video_len: i64,
}

/// One match (or recording) as the index keeps it.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub id: String,
    pub source: Source,
    pub when_ms: i64,
    pub champion: Option<String>,
    pub queue: Option<String>,
    pub game_mode: Option<String>,
    pub result: Option<String>,
    pub kills: Option<i64>,
    pub deaths: Option<i64>,
    pub assists: Option<i64>,
    pub duration_s: Option<f64>,
    pub moments: i64,
    /// Double kills or better.
    pub multikills: i64,
    pub clips: Vec<ClipEntry>,
    pub video: bool,
    /// The match's thumbnail, inside its folder, once made.
    pub thumbnail: Option<String>,
    pub stamp: Stamp,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ClipEntry {
    pub file: String,
    pub label: String,
    pub from_s: f64,
    pub to_s: f64,
    pub favorite: bool,
}

/// A row of a query: an entry without its clips.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub id: String,
    pub source: Source,
    pub when_ms: i64,
    pub champion: Option<String>,
    pub queue: Option<String>,
    pub game_mode: Option<String>,
    pub result: Option<String>,
    pub kills: Option<i64>,
    pub deaths: Option<i64>,
    pub assists: Option<i64>,
    pub duration_s: Option<f64>,
    pub moments: i64,
    pub multikills: i64,
    pub clips: i64,
    pub favorite_clips: i64,
    pub video: bool,
    pub thumbnail: Option<String>,
}

/// What the matches list asks for: a page, filtered and sorted.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MatchQuery {
    pub offset: u32,
    /// Matches per page; 0 for the default.
    pub limit: u32,
    pub champion: Option<String>,
    /// The queue, or the game mode when the queue isn't known (what the card shows).
    pub queue: Option<String>,
    /// `win`, `lose` or `unknown`.
    pub result: Option<String>,
    pub has_clips: bool,
    pub has_multikill: bool,
    /// Started at or after `from_ms`, before `to_ms` (Unix ms).
    pub from_ms: Option<i64>,
    pub to_ms: Option<i64>,
    pub sort: Sort,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sort {
    #[default]
    Newest,
    Oldest,
    Longest,
    MostKills,
}

/// The values the filters offer.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Facets {
    pub champions: Vec<String>,
    pub queues: Vec<String>,
}

/// How the index came up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Opened {
    /// An existing index of this schema.
    Existing,
    /// Created, or rebuilt because the old one was unusable or of another schema.
    New,
    /// The file couldn't be used: an index in memory, rebuilt each start.
    InMemory,
}

pub struct Index {
    conn: Mutex<Connection>,
    path: Option<PathBuf>,
    opened: Opened,
}

#[derive(Debug, thiserror::Error)]
enum OpenError {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error("schema {0}, not {SCHEMA}")]
    OtherSchema(i64),
    #[error("integrity check failed: {0}")]
    Damaged(String),
}

impl Index {
    /// Opens the index at `path`, creating it if needed. A file that isn't a usable index
    /// of this schema is deleted and made anew; if even that fails, the index lives in
    /// memory.
    pub fn open(path: &Path) -> Self {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        match Self::open_file(path) {
            Ok((conn, opened)) => return Self::with(conn, Some(path), opened),
            Err(error) if path.exists() => {
                tracing::warn!(%error, path = %path.display(), "library index unusable; rebuilding it");
                remove_files(path);
            }
            Err(error) => tracing::warn!(%error, "could not create the library index"),
        }
        match Self::open_file(path) {
            Ok((conn, _)) => Self::with(conn, Some(path), Opened::New),
            Err(error) => {
                tracing::warn!(%error, "library index kept in memory for this run");
                Self::in_memory()
            }
        }
    }

    pub fn in_memory() -> Self {
        let conn = Connection::open_in_memory().expect("SQLite in memory");
        create_tables(&conn).expect("tables in memory");
        Self::with(conn, None, Opened::InMemory)
    }

    fn with(conn: Connection, path: Option<&Path>, opened: Opened) -> Self {
        Self {
            conn: Mutex::new(conn),
            path: path.map(Path::to_owned),
            opened,
        }
    }

    fn open_file(path: &Path) -> Result<(Connection, Opened), OpenError> {
        let conn = Connection::open(path)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        // A file that isn't a database fails here.
        let tables: i64 =
            conn.query_row("SELECT count(*) FROM sqlite_master", [], |row| row.get(0))?;
        if tables == 0 {
            create_tables(&conn)?;
            return Ok((conn, Opened::New));
        }
        let schema: Option<String> = conn
            .query_row("SELECT value FROM meta WHERE key = 'schema'", [], |row| {
                row.get(0)
            })
            .optional()?;
        let schema = schema.and_then(|s| s.parse().ok()).unwrap_or(0);
        if schema != SCHEMA {
            return Err(OpenError::OtherSchema(schema));
        }
        let check: String = conn.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
        if check != "ok" {
            return Err(OpenError::Damaged(check));
        }
        conn.pragma_update(None, "foreign_keys", "ON")?;
        Ok((conn, Opened::Existing))
    }

    pub fn opened(&self) -> Opened {
        self.opened
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The stamps of everything indexed from `source`, by id.
    pub fn stamps(&self, source: Source) -> rusqlite::Result<HashMap<String, Stamp>> {
        let conn = self.conn();
        let mut statement = conn.prepare_cached(
            "SELECT id, sidecar_ns, sidecar_len, video_len FROM matches WHERE source = ?1",
        )?;
        let rows = statement.query_map([source.as_str()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                Stamp {
                    modified_ns: row.get(1)?,
                    len: row.get(2)?,
                    video_len: row.get(3)?,
                },
            ))
        })?;
        rows.collect()
    }

    /// Writes `entries` (replacing what the index had for them) and drops `removed`, all
    /// at once.
    pub fn apply(&self, entries: &[Entry], removed: &[String]) -> rusqlite::Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        {
            let mut drop_clips = tx.prepare_cached("DELETE FROM clips WHERE match_id = ?1")?;
            let mut drop_match = tx.prepare_cached("DELETE FROM matches WHERE id = ?1")?;
            for id in removed {
                drop_clips.execute([id])?;
                drop_match.execute([id])?;
            }
            let mut put_match = tx.prepare_cached(
                "INSERT OR REPLACE INTO matches (id, source, when_ms, champion, queue, game_mode,
                    result, kills, deaths, assists, duration_s, moments, multikills, clips,
                    favorite_clips, video, thumbnail, sidecar_ns, sidecar_len, video_len)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
                    ?17, ?18, ?19, ?20)",
            )?;
            let mut put_clip = tx.prepare_cached(
                "INSERT OR REPLACE INTO clips (match_id, file, label, from_s, to_s, favorite)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?;
            for entry in entries {
                drop_clips.execute([&entry.id])?;
                let favorites = entry.clips.iter().filter(|c| c.favorite).count() as i64;
                put_match.execute(params![
                    entry.id,
                    entry.source.as_str(),
                    entry.when_ms,
                    entry.champion,
                    entry.queue,
                    entry.game_mode,
                    entry.result,
                    entry.kills,
                    entry.deaths,
                    entry.assists,
                    entry.duration_s,
                    entry.moments,
                    entry.multikills,
                    entry.clips.len() as i64,
                    favorites,
                    entry.video,
                    entry.thumbnail,
                    entry.stamp.modified_ns,
                    entry.stamp.len,
                    entry.stamp.video_len,
                ])?;
                for clip in &entry.clips {
                    put_clip.execute(params![
                        entry.id,
                        clip.file,
                        clip.label,
                        clip.from_s,
                        clip.to_s,
                        clip.favorite
                    ])?;
                }
            }
        }
        tx.commit()
    }

    /// A value kept beside the entries (what they were made with).
    pub fn meta(&self, key: &str) -> rusqlite::Result<Option<String>> {
        self.conn()
            .query_row("SELECT value FROM meta WHERE key = ?1", [key], |row| {
                row.get(0)
            })
            .optional()
    }

    pub fn set_meta(&self, key: &str, value: &str) -> rusqlite::Result<()> {
        self.conn()
            .execute(
                "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)",
                [key, value],
            )
            .map(|_| ())
    }

    pub fn set_thumbnail(&self, id: &str, thumbnail: Option<&str>) -> rusqlite::Result<bool> {
        self.conn()
            .execute(
                "UPDATE matches SET thumbnail = ?2 WHERE id = ?1",
                params![id, thumbnail],
            )
            .map(|changed| changed > 0)
    }

    /// A page of matches: how many match the filters in all, and those from `offset`.
    pub fn query(&self, query: &MatchQuery) -> rusqlite::Result<(u64, Vec<Row>)> {
        let (filter, values) = filter(query);
        let conn = self.conn();
        let total: i64 = conn.query_row(
            &format!("SELECT count(*) FROM matches{filter}"),
            params_from_iter(values.iter().map(|v| v.as_ref())),
            |row| row.get(0),
        )?;
        let limit = match query.limit {
            0 => DEFAULT_LIMIT,
            limit => limit.min(MAX_LIMIT),
        };
        let sql = format!(
            "SELECT id, source, when_ms, champion, queue, game_mode, result, kills, deaths,
                assists, duration_s, moments, multikills, clips, favorite_clips, video, thumbnail
             FROM matches{filter} ORDER BY {} LIMIT {limit} OFFSET {}",
            order(query.sort),
            query.offset
        );
        let mut statement = conn.prepare(&sql)?;
        let rows =
            statement.query_map(params_from_iter(values.iter().map(|v| v.as_ref())), |row| {
                Ok(Row {
                    id: row.get(0)?,
                    source: Source::parse(&row.get::<_, String>(1)?),
                    when_ms: row.get(2)?,
                    champion: row.get(3)?,
                    queue: row.get(4)?,
                    game_mode: row.get(5)?,
                    result: row.get(6)?,
                    kills: row.get(7)?,
                    deaths: row.get(8)?,
                    assists: row.get(9)?,
                    duration_s: row.get(10)?,
                    moments: row.get(11)?,
                    multikills: row.get(12)?,
                    clips: row.get(13)?,
                    favorite_clips: row.get(14)?,
                    video: row.get(15)?,
                    thumbnail: row.get(16)?,
                })
            })?;
        let rows = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok((u64::try_from(total).unwrap_or_default(), rows))
    }

    /// The champions and queues there are, for the filters.
    pub fn facets(&self) -> rusqlite::Result<Facets> {
        let conn = self.conn();
        let distinct = |sql: &str| -> rusqlite::Result<Vec<String>> {
            let mut statement = conn.prepare(sql)?;
            let values = statement.query_map([], |row| row.get(0))?;
            values.collect()
        };
        Ok(Facets {
            champions: distinct(
                "SELECT DISTINCT champion FROM matches WHERE champion IS NOT NULL
                 ORDER BY champion COLLATE NOCASE",
            )?,
            queues: distinct(
                "SELECT DISTINCT coalesce(queue, game_mode) AS q FROM matches WHERE q IS NOT NULL
                 ORDER BY q COLLATE NOCASE",
            )?,
        })
    }

    pub fn len(&self) -> rusqlite::Result<u64> {
        let total: i64 = self
            .conn()
            .query_row("SELECT count(*) FROM matches", [], |row| row.get(0))?;
        Ok(u64::try_from(total).unwrap_or_default())
    }
}

fn create_tables(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(TABLES)?;
    conn.execute(
        "INSERT INTO meta (key, value) VALUES ('schema', ?1)",
        [SCHEMA.to_string()],
    )?;
    conn.pragma_update(None, "foreign_keys", "ON")
}

/// The index file and SQLite's companions.
fn remove_files(path: &Path) {
    for suffix in ["", "-journal", "-wal", "-shm"] {
        let mut name = path.as_os_str().to_owned();
        name.push(suffix);
        let _ = std::fs::remove_file(PathBuf::from(name));
    }
}

/// The `WHERE` clause of a query and its values (the user's text only ever as a value).
fn filter(query: &MatchQuery) -> (String, Vec<Box<dyn ToSql>>) {
    let mut clauses: Vec<&str> = Vec::new();
    let mut values: Vec<Box<dyn ToSql>> = Vec::new();
    if let Some(champion) = query.champion.as_ref().filter(|c| !c.is_empty()) {
        clauses.push("champion = ?");
        values.push(Box::new(champion.clone()));
    }
    if let Some(queue) = query.queue.as_ref().filter(|q| !q.is_empty()) {
        clauses.push("coalesce(queue, game_mode) = ?");
        values.push(Box::new(queue.clone()));
    }
    match query.result.as_deref() {
        Some("win") | Some("lose") => {
            clauses.push("result = ?");
            values.push(Box::new(query.result.clone()));
        }
        Some("unknown") => clauses.push("result IS NULL"),
        _ => {}
    }
    if query.has_clips {
        clauses.push("clips > 0");
    }
    if query.has_multikill {
        clauses.push("multikills > 0");
    }
    if let Some(from) = query.from_ms {
        clauses.push("when_ms >= ?");
        values.push(Box::new(from));
    }
    if let Some(to) = query.to_ms {
        clauses.push("when_ms < ?");
        values.push(Box::new(to));
    }
    let sql = if clauses.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", clauses.join(" AND "))
    };
    (sql, values)
}

fn order(sort: Sort) -> &'static str {
    match sort {
        Sort::Newest => "when_ms DESC, id DESC",
        Sort::Oldest => "when_ms ASC, id ASC",
        Sort::Longest => "duration_s IS NULL, duration_s DESC, when_ms DESC",
        Sort::MostKills => "kills IS NULL, kills DESC, when_ms DESC",
    }
}
