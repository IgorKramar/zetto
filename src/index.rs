//! Удаляемый SQLite-кэш: файлы и recovery всегда остаются источником истины.
use crate::app::{Catalog, Record, Stage, Warning, derive_catalog};
use crate::config::VaultConfig;
use crate::error::{Error, Result};
use crate::note::{Diagnostic, Link, Note, Severity};
use crate::resolver::Resolver;
use crate::vault::{Vault, hash};
use rusqlite::{Connection, OpenFlags, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant, UNIX_EPOCH};

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FreshnessMode {
    Full,
    #[default]
    Stat,
    Cached,
    NotApplicable,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Freshness {
    pub mode: FreshnessMode,
    pub generation: Option<i64>,
}
impl Freshness {
    pub fn full() -> Self {
        Self {
            mode: FreshnessMode::Full,
            generation: None,
        }
    }
}
impl Default for Freshness {
    fn default() -> Self {
        Self {
            mode: FreshnessMode::NotApplicable,
            generation: None,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ScanMetrics {
    pub walk_micros: u64,
    pub read_micros: u64,
    pub files_seen: usize,
    pub files_read: usize,
}

#[derive(Clone, Debug, Default)]
pub struct Cancellation(Arc<AtomicBool>);
impl Cancellation {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
    pub fn check(&self) -> Result<()> {
        if self.is_cancelled() {
            Err(Error::new("interrupted", "Запрос отменён"))
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct SearchHit {
    pub path: String,
    pub score: f64,
}
#[derive(Clone, Debug, Serialize)]
pub struct IndexedLink {
    pub source_path: String,
    pub target_path: Option<String>,
    pub link: Link,
    pub reason: String,
}

pub struct Index {
    connection: Connection,
    path: PathBuf,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct Stamp {
    size: u64,
    modified: u128,
}
struct Entry {
    stamp: Option<Stamp>,
    hash: Option<String>,
    record: Record,
}
struct Meta {
    generation: i64,
    revision: i64,
    ready: bool,
    dirty: bool,
    warnings: Vec<Warning>,
    derived_digest: String,
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS meta (
 singleton INTEGER PRIMARY KEY CHECK(singleton=1), schema_version INTEGER NOT NULL,
 generation INTEGER NOT NULL, revision INTEGER NOT NULL, ready INTEGER NOT NULL,
 dirty INTEGER NOT NULL, complete INTEGER NOT NULL, warnings TEXT NOT NULL,
 config TEXT NOT NULL, derived_digest TEXT NOT NULL, checked_at TEXT
);
INSERT OR IGNORE INTO meta VALUES(1,1,0,0,0,0,1,'[]','','',NULL);
CREATE TABLE IF NOT EXISTS files (
 file_key INTEGER PRIMARY KEY AUTOINCREMENT, path TEXT NOT NULL UNIQUE, id TEXT,
 stamp TEXT, hash TEXT, readable INTEGER NOT NULL, parsed TEXT NOT NULL, snapshot TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS files_id ON files(id);
CREATE TABLE IF NOT EXISTS links (
 source_key INTEGER NOT NULL REFERENCES files(file_key) ON DELETE CASCADE,
 target_key INTEGER REFERENCES files(file_key) ON DELETE SET NULL,
 source_offset INTEGER NOT NULL, link TEXT NOT NULL, reason TEXT NOT NULL,
 PRIMARY KEY(source_key, source_offset)
);
CREATE INDEX IF NOT EXISTS links_target ON links(target_key);
CREATE TABLE IF NOT EXISTS tags (file_key INTEGER NOT NULL REFERENCES files(file_key) ON DELETE CASCADE,
 value TEXT NOT NULL, PRIMARY KEY(file_key,value));
CREATE INDEX IF NOT EXISTS tags_value ON tags(value);
CREATE TABLE IF NOT EXISTS aliases (file_key INTEGER NOT NULL REFERENCES files(file_key) ON DELETE CASCADE,
 value TEXT NOT NULL, normalized TEXT NOT NULL, PRIMARY KEY(file_key,value));
CREATE INDEX IF NOT EXISTS aliases_normalized ON aliases(normalized);
CREATE TABLE IF NOT EXISTS diagnostics (file_key INTEGER NOT NULL REFERENCES files(file_key) ON DELETE CASCADE,
 ordinal INTEGER NOT NULL, diagnostic TEXT NOT NULL, PRIMARY KEY(file_key,ordinal));
CREATE VIRTUAL TABLE IF NOT EXISTS notes_fts USING fts5(title, body, tokenize='unicode61');
";

fn unavailable(error: impl std::fmt::Display) -> Error {
    Error::new("index_unavailable", format!("Индекс недоступен: {error}"))
}
fn encode(value: &impl Serialize) -> Result<String> {
    serde_json::to_string(value).map_err(unavailable)
}
fn decode<T: for<'de> Deserialize<'de>>(value: &str) -> Result<T> {
    serde_json::from_str(value).map_err(unavailable)
}

impl Index {
    pub fn cache_path(vault: &Vault) -> Result<PathBuf> {
        let base = if let Some(path) = std::env::var_os("XDG_CACHE_HOME") {
            PathBuf::from(path)
        } else {
            PathBuf::from(
                std::env::var_os("HOME")
                    .ok_or_else(|| unavailable("Укажите HOME или XDG_CACHE_HOME"))?,
            )
            .join(".cache")
        };
        if !base.is_absolute() {
            return Err(unavailable("XDG_CACHE_HOME должен быть абсолютным"));
        }
        // HOME/XDG — явная локальная настройка; разрешаем её symlink-prefix.
        // Собственные zetto/hash/файлы ниже проверяются без следования symlink.
        let base = canonical_base(&base)?;
        let key = hash(vault.root().as_os_str().as_encoded_bytes());
        let path = base.join("zetto").join(key).join("index.sqlite3");
        if path.starts_with(vault.root()) {
            return Err(unavailable("Кэш не должен находиться внутри vault"));
        }
        Ok(path)
    }
    pub fn open(vault: &Vault) -> Result<Self> {
        Self::open_at(vault, &Self::cache_path(vault)?)
    }
    pub fn open_cached(vault: &Vault) -> Result<Self> {
        Self::connect(&Self::cache_path(vault)?, false)
    }
    /// Явный локальный путь также позволяет изолировать интеграционные проверки.
    pub fn open_at(vault: &Vault, path: &Path) -> Result<Self> {
        if path.starts_with(vault.root()) {
            return Err(unavailable("Кэш не должен находиться внутри vault"));
        }
        Self::connect(path, true)
    }
    fn connect(path: &Path, create: bool) -> Result<Self> {
        check_cache_path(path, create)?;
        let existed = path.try_exists().map_err(unavailable)?;
        if !existed && !create {
            return Err(unavailable(
                "Индекс ещё не построен; выполните index rebuild",
            ));
        }
        if !existed {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            match options.open(path) {
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(unavailable(e)),
            }
        }
        let flags = if create {
            OpenFlags::SQLITE_OPEN_READ_WRITE
        } else {
            OpenFlags::SQLITE_OPEN_READ_ONLY
        } | OpenFlags::SQLITE_OPEN_NOFOLLOW;
        let connection = Connection::open_with_flags(path, flags).map_err(unavailable)?;
        connection
            .busy_timeout(Duration::from_millis(250))
            .map_err(unavailable)?;
        connection
            .pragma_update(None, "foreign_keys", true)
            .map_err(unavailable)?;
        let mut index = Self {
            connection,
            path: path.to_owned(),
        };
        if create {
            let version = index
                .connection
                .pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
                .map_err(unavailable)?;
            if version == 0 {
                index
                    .connection
                    .pragma_update(None, "journal_mode", "WAL")
                    .map_err(unavailable)?;
                let transaction = index
                    .connection
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .map_err(unavailable)?;
                transaction.execute_batch(SCHEMA).map_err(unavailable)?;
                transaction
                    .pragma_update(None, "user_version", 1)
                    .map_err(unavailable)?;
                transaction.commit().map_err(unavailable)?;
            } else if version != 1 {
                return Err(unavailable(
                    "Неизвестная схема; нужен поддерживающий её бинарник",
                ));
            }
        }
        index.meta()?;
        if index.sqlite_version()? < (3, 51, 3) {
            return Err(unavailable("SQLite старше исправления WAL-reset (3.51.3)"));
        }
        Ok(index)
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn sqlite_version(&self) -> Result<(u32, u32, u32)> {
        let version: String = self
            .connection
            .query_row("SELECT sqlite_version()", [], |r| r.get(0))
            .map_err(unavailable)?;
        let values: Vec<_> = version
            .split('.')
            .map(str::parse::<u32>)
            .collect::<std::result::Result<_, _>>()
            .map_err(unavailable)?;
        match values.as_slice() {
            [a, b, c] => Ok((*a, *b, *c)),
            _ => Err(unavailable("Неверный sqlite_version")),
        }
    }
    fn meta(&self) -> Result<Meta> {
        load_meta(&self.connection)
    }
    pub fn mark_dirty(&mut self) -> Result<()> {
        self.connection
            .execute(
                "UPDATE meta SET dirty=1, revision=revision+1 WHERE singleton=1",
                [],
            )
            .map_err(unavailable)?;
        Ok(())
    }
    pub fn snapshot(&mut self) -> Result<Catalog> {
        let transaction = self.connection.transaction().map_err(unavailable)?;
        let result = load_catalog(&transaction);
        transaction.commit().map_err(unavailable)?;
        result
    }
    pub fn links(&self) -> Result<Vec<IndexedLink>> {
        let mut statement = self.connection.prepare("SELECT s.path,t.path,l.link,l.reason FROM links l JOIN files s ON s.file_key=l.source_key LEFT JOIN files t ON t.file_key=l.target_key ORDER BY s.id,s.path,l.source_offset").map_err(unavailable)?;
        let rows = statement
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })
            .map_err(unavailable)?;
        rows.map(|row| {
            let (source_path, target_path, link, reason) = row.map_err(unavailable)?;
            Ok(IndexedLink {
                source_path,
                target_path,
                link: decode(&link)?,
                reason,
            })
        })
        .collect()
    }
    pub fn rebuild(
        &mut self,
        vault: &Vault,
        config: &VaultConfig,
        token: &Cancellation,
    ) -> Result<Catalog> {
        self.reconcile_inner(vault, config, FreshnessMode::Full, token, true)
    }
    pub fn reconcile(
        &mut self,
        vault: &Vault,
        config: &VaultConfig,
        mode: FreshnessMode,
        token: &Cancellation,
    ) -> Result<Catalog> {
        if mode == FreshnessMode::Cached {
            token.check()?;
            return self.snapshot();
        }
        self.reconcile_inner(vault, config, mode, token, false)
    }
    fn reconcile_inner(
        &mut self,
        vault: &Vault,
        config: &VaultConfig,
        mode: FreshnessMode,
        token: &Cancellation,
        rebuild: bool,
    ) -> Result<Catalog> {
        for _ in 0..3 {
            token.check()?;
            let transaction = self.connection.transaction().map_err(unavailable)?;
            let base = load_meta(&transaction)?;
            let previous_config: String = transaction
                .query_row("SELECT config FROM meta WHERE singleton=1", [], |r| {
                    r.get(0)
                })
                .map_err(unavailable)?;
            let mut previous = BTreeMap::new();
            {
                let mut statement = transaction
                    .prepare("SELECT path,stamp,hash,parsed FROM files")
                    .map_err(unavailable)?;
                let rows = statement
                    .query_map([], |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, Option<String>>(1)?,
                            r.get::<_, Option<String>>(2)?,
                            r.get::<_, String>(3)?,
                        ))
                    })
                    .map_err(unavailable)?;
                for row in rows {
                    token.check()?;
                    let (path, stamp, hash, parsed) = row.map_err(unavailable)?;
                    previous.insert(
                        path,
                        Entry {
                            stamp: stamp.map(|s| decode(&s)).transpose()?,
                            hash,
                            record: decode(&parsed)?,
                        },
                    );
                }
            }
            transaction.commit().map_err(unavailable)?;
            let walk = Instant::now();
            let paths = vault.note_paths()?;
            let mut metrics = ScanMetrics {
                files_seen: paths.len(),
                walk_micros: walk.elapsed().as_micros() as u64,
                ..Default::default()
            };
            let mut spool = tempfile::tempfile().map_err(unavailable)?;
            let mut changed_bodies = BTreeMap::new();
            let mut entries = BTreeMap::new();
            let mut changed = rebuild || !base.ready || previous_config != encode(config)?;
            for path in paths {
                token.check()?;
                let stamp_start = Instant::now();
                let stamp = file_stamp(vault, &path).ok();
                metrics.walk_micros += stamp_start.elapsed().as_micros() as u64;
                let old = previous.remove(&path);
                if !rebuild
                    && mode != FreshnessMode::Full
                    && stamp.is_some()
                    && old
                        .as_ref()
                        .is_some_and(|old| old.stamp == stamp && old.record.readable)
                {
                    entries.insert(path, old.unwrap());
                    continue;
                }
                let start = Instant::now();
                let read = stable_read(vault, &path, token);
                metrics.read_micros += start.elapsed().as_micros() as u64;
                metrics.files_read += 1;
                let entry = match read {
                    Ok((raw, stamp)) => {
                        let digest = hash(raw.as_bytes());
                        if !rebuild
                            && old
                                .as_ref()
                                .is_some_and(|old| old.hash.as_ref() == Some(&digest))
                        {
                            let mut old = old.unwrap();
                            // Метаданные могут измениться при неизменном содержимом.
                            changed |= old.stamp.as_ref() != Some(&stamp);
                            old.stamp = Some(stamp);
                            entries.insert(path, old);
                            continue;
                        }
                        let mut record = Record::from_text(path.clone(), raw);
                        let offset = spool.stream_position().map_err(unavailable)?;
                        spool
                            .write_all(record.note.body.as_bytes())
                            .map_err(unavailable)?;
                        changed_bodies.insert(path.clone(), (offset, record.note.body.len()));
                        record.raw = None;
                        record.note.body.clear();
                        Entry {
                            stamp: Some(stamp),
                            hash: Some(digest),
                            record,
                        }
                    }
                    Err(e) if e.code == "interrupted" => return Err(e),
                    Err(error) => {
                        let mut diagnostic = Diagnostic::new(
                            "zetto/file-read-error",
                            Severity::Error,
                            error.message,
                        );
                        diagnostic.path = Some(path.clone());
                        changed_bodies.insert(path.clone(), (0, 0));
                        Entry {
                            stamp,
                            hash: None,
                            record: Record {
                                path: path.clone(),
                                raw: None,
                                readable: false,
                                tags: Vec::new(),
                                note: Note {
                                    diagnostics: vec![diagnostic],
                                    ..Default::default()
                                },
                                stage: if path.starts_with("inbox/") {
                                    Stage::Inbox
                                } else {
                                    Stage::Published
                                },
                            },
                        }
                    }
                };
                changed = true;
                entries.insert(path, entry);
            }
            changed |= !previous.is_empty();
            let mut catalog = derive_catalog(
                vault,
                config,
                entries.values().map(|e| e.record.clone()).collect(),
                true,
                token,
            )?;
            catalog.metrics = metrics;
            let derived_digest = hash(encode(&catalog.records)?.as_bytes());
            changed |= base.derived_digest != derived_digest;
            token.check()?;
            let transaction = self
                .connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(unavailable)?;
            let latest = load_meta(&transaction)?;
            if latest.generation != base.generation || latest.revision != base.revision {
                transaction.rollback().map_err(unavailable)?;
                continue;
            }
            if changed {
                for path in previous.keys() {
                    transaction.execute("DELETE FROM notes_fts WHERE rowid=(SELECT file_key FROM files WHERE path=?1)", [path]).map_err(unavailable)?;
                    transaction
                        .execute("DELETE FROM files WHERE path=?1", [path])
                        .map_err(unavailable)?;
                }
                // ponytail: все рёбра пересчитываются после изменения; reverse dependencies нужны при измеренном узком месте.
                transaction.execute_batch("DELETE FROM links; DELETE FROM tags; DELETE FROM aliases; DELETE FROM diagnostics;").map_err(unavailable)?;
                for (path, entry) in &entries {
                    token.check()?;
                    let parsed = encode(&entry.record)?;
                    let stamp = entry.stamp.as_ref().map(encode).transpose()?;
                    transaction.execute("INSERT INTO files(path,id,stamp,hash,readable,parsed,snapshot) VALUES(?1,?2,?3,?4,?5,?6,?6) ON CONFLICT(path) DO UPDATE SET id=excluded.id,stamp=excluded.stamp,hash=excluded.hash,readable=excluded.readable,parsed=excluded.parsed", params![path,entry.record.note.id(),stamp,entry.hash,entry.record.readable,parsed]).map_err(unavailable)?;
                }
                let mut keys = BTreeMap::new();
                {
                    let mut statement = transaction
                        .prepare("SELECT path,file_key FROM files")
                        .map_err(unavailable)?;
                    for row in statement
                        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
                        .map_err(unavailable)?
                    {
                        let (path, key) = row.map_err(unavailable)?;
                        keys.insert(path, key);
                    }
                }
                let resolver = Resolver::new(&catalog.records);
                for record in &catalog.records {
                    token.check()?;
                    let key = keys[&record.path];
                    transaction
                        .execute(
                            "UPDATE files SET snapshot=?1 WHERE file_key=?2",
                            params![encode(record)?, key],
                        )
                        .map_err(unavailable)?;
                    if let Some(&(offset, length)) = changed_bodies.get(&record.path) {
                        spool.seek(SeekFrom::Start(offset)).map_err(unavailable)?;
                        let mut body = String::new();
                        Read::by_ref(&mut spool)
                            .take(length as u64)
                            .read_to_string(&mut body)
                            .map_err(unavailable)?;
                        transaction
                            .execute("DELETE FROM notes_fts WHERE rowid=?1", [key])
                            .map_err(unavailable)?;
                        transaction
                            .execute(
                                "INSERT INTO notes_fts(rowid,title,body) VALUES(?1,?2,?3)",
                                params![key, record.note.title().unwrap_or(""), body],
                            )
                            .map_err(unavailable)?;
                    }
                    for tag in &record.tags {
                        transaction
                            .execute("INSERT INTO tags VALUES(?1,?2)", params![key, tag])
                            .map_err(unavailable)?;
                    }
                    for alias in record.note.aliases().into_iter().collect::<BTreeSet<_>>() {
                        transaction
                            .execute(
                                "INSERT INTO aliases VALUES(?1,?2,?3)",
                                params![key, alias, alias.trim().to_lowercase()],
                            )
                            .map_err(unavailable)?;
                    }
                    for (ordinal, diagnostic) in record.note.diagnostics.iter().enumerate() {
                        transaction
                            .execute(
                                "INSERT INTO diagnostics VALUES(?1,?2,?3)",
                                params![key, ordinal as i64, encode(diagnostic)?],
                            )
                            .map_err(unavailable)?;
                    }
                    for link in &record.note.links {
                        let resolution = resolver.resolve(vault, &record.path, link);
                        let target = resolution.target.map(|i| keys[&catalog.records[i].path]);
                        transaction
                            .execute(
                                "INSERT INTO links VALUES(?1,?2,?3,?4,?5)",
                                params![
                                    key,
                                    target,
                                    link.span.start as i64,
                                    encode(link)?,
                                    resolution.kind.as_str()
                                ],
                            )
                            .map_err(unavailable)?;
                    }
                }
            }
            let generation = base.generation + i64::from(changed);
            transaction.execute("UPDATE meta SET generation=?1, revision=revision+1, ready=1, dirty=0, complete=?2, warnings=?3, config=?4, derived_digest=?5, checked_at=?6 WHERE singleton=1",params![generation,catalog.complete,encode(&catalog.warnings)?,encode(config)?,derived_digest,chrono::Utc::now().to_rfc3339()]).map_err(unavailable)?;
            token.check()?;
            transaction.commit().map_err(unavailable)?;
            catalog.freshness = Freshness {
                mode,
                generation: Some(generation),
            };
            return Ok(catalog);
        }
        Err(unavailable(
            "Индекс изменяется другим процессом; повторите запрос",
        ))
    }
    pub fn search_fts(
        &mut self,
        query: &str,
        advanced: bool,
        limit: usize,
        offset: usize,
        token: &Cancellation,
    ) -> Result<Vec<SearchHit>> {
        self.search_fts_snapshot(query, advanced, limit, offset, token)
            .map(|(_, hits)| hits)
    }
    pub fn search_fts_snapshot(
        &mut self,
        query: &str,
        advanced: bool,
        limit: usize,
        offset: usize,
        token: &Cancellation,
    ) -> Result<(Catalog, Vec<SearchHit>)> {
        token.check()?;
        if !(1..=10_001).contains(&limit) || i64::try_from(offset).is_err() {
            return Err(Error::new("usage", "Неверный limit/offset поиска"));
        }
        let query = if advanced {
            query.to_owned()
        } else {
            escaped_query(query)
        };
        let cancellation = token.clone();
        self.connection
            .progress_handler(1000, Some(move || cancellation.is_cancelled()))
            .map_err(unavailable)?;
        let result = (|| {
            let transaction = self.connection.transaction().map_err(unavailable)?;
            let catalog = load_catalog(&transaction)?;
            let mut hits = Vec::new();
            if !query.trim().is_empty() {
                let mut statement = transaction.prepare("SELECT f.path,-bm25(notes_fts,5.0,1.0) FROM notes_fts JOIN files f ON f.file_key=notes_fts.rowid WHERE notes_fts MATCH ?1 ORDER BY bm25(notes_fts,5.0,1.0),f.id,f.path LIMIT ?2 OFFSET ?3").map_err(unavailable)?;
                let rows = statement
                    .query_map(params![query, limit as i64, offset as i64], |r| {
                        Ok(SearchHit {
                            path: r.get(0)?,
                            score: r.get(1)?,
                        })
                    })
                    .map_err(|e| fts_error(e, token))?;
                for row in rows {
                    token.check()?;
                    hits.push(row.map_err(|e| fts_error(e, token))?);
                }
            }
            transaction.commit().map_err(unavailable)?;
            Ok((catalog, hits))
        })();
        self.connection
            .progress_handler(0, None::<fn() -> bool>)
            .map_err(unavailable)?;
        result
    }
}

fn fts_error(error: rusqlite::Error, token: &Cancellation) -> Error {
    if token.is_cancelled() {
        Error::new("interrupted", "Запрос отменён")
    } else if let rusqlite::Error::SqliteFailure(code, _) = &error {
        if code.code == rusqlite::ErrorCode::Unknown {
            Error::new("usage", format!("Неверный FTS-запрос: {error}"))
        } else {
            unavailable(error)
        }
    } else {
        unavailable(error)
    }
}
fn escaped_query(query: &str) -> String {
    query
        .split_whitespace()
        .map(|term| format!("\"{}\"", term.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" AND ")
}
fn load_meta(connection: &Connection) -> Result<Meta> {
    let (version,generation,revision,ready,dirty,warnings,derived_digest): (i64,i64,i64,bool,bool,String,String) = connection.query_row("SELECT schema_version,generation,revision,ready,dirty,warnings,derived_digest FROM meta WHERE singleton=1",[],|r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?))).map_err(unavailable)?;
    if version != 1 {
        return Err(unavailable("Неподдерживаемая схема индекса"));
    }
    Ok(Meta {
        generation,
        revision,
        ready,
        dirty,
        warnings: decode(&warnings)?,
        derived_digest,
    })
}
fn load_catalog(connection: &Connection) -> Result<Catalog> {
    let meta = load_meta(connection)?;
    if !meta.ready {
        return Err(unavailable(
            "Индекс ещё не построен; выполните index rebuild",
        ));
    }
    let mut statement = connection
        .prepare("SELECT snapshot FROM files ORDER BY id,path")
        .map_err(unavailable)?;
    let mut records: Vec<Record> = Vec::new();
    for row in statement
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(unavailable)?
    {
        records.push(decode(&row.map_err(unavailable)?)?);
    }
    let mut warnings = meta.warnings;
    if meta.dirty {
        warnings.push(Warning {
            code: "index_stale".into(),
            message: "Есть сохранённые изменения; cached показывает предыдущее поколение".into(),
            path: None,
        });
    }
    let complete = records.iter().all(|r| r.readable);
    Ok(Catalog {
        records,
        warnings,
        complete,
        freshness: Freshness {
            mode: FreshnessMode::Cached,
            generation: Some(meta.generation),
        },
        metrics: ScanMetrics::default(),
    })
}
fn file_stamp(vault: &Vault, relative: &str) -> Result<Stamp> {
    let metadata = fs::symlink_metadata(vault.path(relative)?)?;
    if !metadata.is_file() {
        return Err(Error::new(
            "validation_failed",
            format!("Нужен обычный файл: {relative}"),
        ));
    }
    Ok(Stamp {
        size: metadata.len(),
        modified: metadata
            .modified()?
            .duration_since(UNIX_EPOCH)
            .map_err(|e| Error::new("io", e.to_string()))?
            .as_nanos(),
    })
}
fn stable_read(vault: &Vault, path: &str, token: &Cancellation) -> Result<(String, Stamp)> {
    for _ in 0..3 {
        token.check()?;
        let before = file_stamp(vault, path)?;
        let bytes = vault.read(path)?;
        let after = file_stamp(vault, path)?;
        if before == after && bytes.len() as u64 == after.size {
            return String::from_utf8(bytes)
                .map(|text| (text, after))
                .map_err(|_| Error::new("validation_failed", "Файл не UTF-8"));
        }
    }
    Err(Error::new(
        "io",
        "Файл меняется во время чтения; повторите сверку",
    ))
}
fn canonical_base(base: &Path) -> Result<PathBuf> {
    if base.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(unavailable(".. запрещён в XDG cache"));
    }
    let mut existing = base;
    let mut missing = Vec::new();
    while !existing.try_exists().map_err(unavailable)? {
        missing.push(
            existing
                .file_name()
                .ok_or_else(|| unavailable("Нет существующего корня cache"))?
                .to_owned(),
        );
        existing = existing
            .parent()
            .ok_or_else(|| unavailable("Нет каталога cache"))?;
    }
    let mut canonical = fs::canonicalize(existing).map_err(unavailable)?;
    if !canonical.is_dir() {
        return Err(unavailable("XDG cache должен быть каталогом"));
    }
    for part in missing.into_iter().rev() {
        canonical.push(part);
    }
    Ok(canonical)
}

fn check_cache_path(path: &Path, create: bool) -> Result<()> {
    if !path.is_absolute() {
        return Err(unavailable("Путь индекса должен быть абсолютным"));
    }
    let parent = path
        .parent()
        .ok_or_else(|| unavailable("Нет каталога индекса"))?;
    let mut current = PathBuf::new();
    for component in parent.components() {
        if matches!(component, Component::ParentDir) {
            return Err(unavailable(".. запрещён в пути индекса"));
        }
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if !metadata.is_dir() || metadata.file_type().is_symlink() => {
                return Err(unavailable(
                    "Каталог индекса содержит symlink или не является каталогом",
                ));
            }
            Ok(_) => {}
            Err(error) if create && error.kind() == std::io::ErrorKind::NotFound => {
                let mut builder = fs::DirBuilder::new();
                #[cfg(unix)]
                {
                    use std::os::unix::fs::DirBuilderExt;
                    builder.mode(0o700);
                }
                match builder.create(&current) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                        if fs::symlink_metadata(&current)
                            .map_err(unavailable)?
                            .file_type()
                            .is_symlink()
                        {
                            return Err(unavailable("Symlink индекса"));
                        }
                    }
                    Err(e) => return Err(unavailable(e)),
                }
            }
            Err(error) => return Err(unavailable(error)),
        }
    }
    #[cfg(unix)]
    if create {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700)).map_err(unavailable)?;
    }
    for candidate in [
        path.to_owned(),
        path.with_file_name(format!(
            "{}-wal",
            path.file_name().unwrap().to_string_lossy()
        )),
        path.with_file_name(format!(
            "{}-shm",
            path.file_name().unwrap().to_string_lossy()
        )),
    ] {
        match fs::symlink_metadata(candidate) {
            Ok(metadata) if !metadata.is_file() || metadata.file_type().is_symlink() => {
                return Err(unavailable("Нужен обычный файл индекса без symlink"));
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(unavailable(e)),
        }
    }
    Ok(())
}
