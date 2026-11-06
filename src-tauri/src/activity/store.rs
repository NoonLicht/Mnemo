//! SQLite store for activity events (SPEC §4.4 `events`).

use std::path::Path;
use std::sync::Mutex;

use crate::mnemo_err::{Error, Result};
use rusqlite::{Connection, params};

fn db_err(e: rusqlite::Error) -> Error {
    Error::Index(format!("activity db: {e}"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub id: i64,
    pub ts_start: i64,
    pub ts_end: i64,
    /// "focus" (a window was in front) or "web" (a page was visited).
    pub kind: String,
    pub app: String,
    pub title: String,
    pub url: Option<String>,
}

pub struct ActivityStore {
    conn: Mutex<Connection>,
    /// When set, window titles and URLs are stored as ChaCha20-Poly1305 sealed values.
    key: Option<[u8; 32]>,
}

const SEAL_PREFIX: &str = "enc1:";

fn seal(key: &[u8; 32], text: &str) -> String {
    use base64::Engine;
    use chacha20poly1305::aead::{Aead, KeyInit};
    use chacha20poly1305::{ChaCha20Poly1305, Nonce};
    let nonce = crate::crypt::random_bytes::<12>().unwrap_or([0u8; 12]);
    let ct = ChaCha20Poly1305::new(key.into())
        .encrypt(Nonce::from_slice(&nonce), text.as_bytes())
        .unwrap_or_default();
    let mut blob = nonce.to_vec();
    blob.extend_from_slice(&ct);
    format!(
        "{SEAL_PREFIX}{}",
        base64::engine::general_purpose::STANDARD.encode(blob)
    )
}

/// Decrypts a sealed value; plaintext (legacy) values pass through unchanged.
fn open_value(key: &Option<[u8; 32]>, stored: String) -> String {
    use base64::Engine;
    use chacha20poly1305::aead::{Aead, KeyInit};
    use chacha20poly1305::{ChaCha20Poly1305, Nonce};
    let Some(body) = stored.strip_prefix(SEAL_PREFIX) else {
        return stored;
    };
    let Some(key) = key else {
        return "(encrypted)".to_string();
    };
    let Ok(blob) = base64::engine::general_purpose::STANDARD.decode(body) else {
        return "(unreadable)".to_string();
    };
    if blob.len() < 12 {
        return "(unreadable)".to_string();
    }
    let (nonce, ct) = blob.split_at(12);
    ChaCha20Poly1305::new(key.into())
        .decrypt(Nonce::from_slice(nonce), ct)
        .ok()
        .and_then(|b| String::from_utf8(b).ok())
        .unwrap_or_else(|| "(unreadable)".to_string())
}

fn url_hash(key: &[u8; 32], url: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(b"url-hash");
    h.update(key);
    h.update(url.as_bytes());
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

impl ActivityStore {
    pub fn open(path: &Path) -> Result<Self> {
        Self::open_with_key(path, crate::crypt::activity_key())
    }

    pub fn open_with_key(path: &Path, key: Option<[u8; 32]>) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        Self::init(Connection::open(path).map_err(db_err)?, key)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory().map_err(db_err)?, None)
    }

    pub fn open_in_memory_with_key(key: [u8; 32]) -> Result<Self> {
        Self::init(Connection::open_in_memory().map_err(db_err)?, Some(key))
    }

    fn init(conn: Connection, key: Option<[u8; 32]>) -> Result<Self> {
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA secure_delete = ON;
             CREATE TABLE IF NOT EXISTS events (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 ts_start INTEGER NOT NULL,
                 ts_end INTEGER,                 -- NULL while the event is still open
                 kind TEXT NOT NULL,
                 app TEXT NOT NULL,
                 title TEXT NOT NULL,
                 url TEXT
             );
             CREATE INDEX IF NOT EXISTS events_time ON events(ts_start);
             CREATE UNIQUE INDEX IF NOT EXISTS events_web_unique ON events(kind, url, ts_start) WHERE kind = 'web';",
        )
        .map_err(db_err)?;
        // Older databases predate the url_hash column used to de-duplicate sealed visits.
        let _ = conn.execute("ALTER TABLE events ADD COLUMN url_hash TEXT", []);
        conn.execute_batch(
            "CREATE UNIQUE INDEX IF NOT EXISTS events_web_unique_hash ON events(kind, url_hash, ts_start)
             WHERE kind = 'web' AND url_hash IS NOT NULL;",
        )
        .map_err(db_err)?;
        let store = Self {
            conn: Mutex::new(conn),
            key,
        };
        store.seal_legacy_rows()?;
        Ok(store)
    }

    /// Encrypts rows written before encryption was switched on. `secure_delete` makes SQLite
    /// overwrite the old plaintext cells; the WAL is truncated afterwards.
    fn seal_legacy_rows(&self) -> Result<()> {
        let Some(key) = self.key else { return Ok(()) };
        let conn = self.conn.lock().unwrap();
        let rows: Vec<(i64, String, Option<String>)> = {
            let mut st = conn
                .prepare("SELECT id, title, url FROM events WHERE title NOT LIKE 'enc1:%'")
                .map_err(db_err)?;
            let it = st
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                .map_err(db_err)?;
            it.collect::<std::result::Result<_, _>>().map_err(db_err)?
        };
        if rows.is_empty() {
            return Ok(());
        }
        for (id, title, url) in rows {
            let hash = url.as_deref().map(|u| url_hash(&key, u));
            let sealed_url = url.as_deref().map(|u| seal(&key, u));
            conn.execute(
                "UPDATE events SET title = ?2, url = ?3, url_hash = ?4 WHERE id = ?1",
                params![id, seal(&key, &title), sealed_url, hash],
            )
            .map_err(db_err)?;
        }
        let _ = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()));
        Ok(())
    }

    /// Starts a focus event and returns its id.
    pub fn begin_focus(&self, ts: i64, app: &str, title: &str) -> Result<i64> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO events(ts_start, kind, app, title) VALUES (?1, 'focus', ?2, ?3)",
            params![ts, app, self.sealed(title)],
        )
        .map_err(db_err)?;
        Ok(conn.last_insert_rowid())
    }

    fn sealed(&self, text: &str) -> String {
        match &self.key {
            Some(k) => seal(k, text),
            None => text.to_string(),
        }
    }

    pub fn end_event(&self, id: i64, ts_end: i64) -> Result<()> {
        self.conn
            .lock()
            .unwrap()
            .execute(
                "UPDATE events SET ts_end = MAX(?2, ts_start) WHERE id = ?1 AND ts_end IS NULL",
                params![id, ts_end],
            )
            .map_err(db_err)?;
        Ok(())
    }

    /// Closes events left open by a crash or kill: they end where they started.
    pub fn close_dangling(&self) -> Result<usize> {
        self.conn
            .lock()
            .unwrap()
            .execute(
                "UPDATE events SET ts_end = ts_start WHERE ts_end IS NULL",
                [],
            )
            .map_err(db_err)
    }

    /// Records a visited page. Returns false if the same visit was already stored.
    pub fn add_web(&self, ts: i64, app: &str, url: &str, title: &str) -> Result<bool> {
        let n = self
            .conn
            .lock()
            .unwrap()
            .execute(
                "INSERT OR IGNORE INTO events(ts_start, ts_end, kind, app, title, url, url_hash) VALUES (?1, ?1, 'web', ?2, ?3, ?4, ?5)",
                params![
                    ts,
                    app,
                    self.sealed(title),
                    self.sealed(url),
                    self.key.as_ref().map(|k| url_hash(k, url))
                ],
            )
            .map_err(db_err)?;
        Ok(n > 0)
    }

    /// Events starting in `[from, to)`, oldest first. Open events are reported as ending `now`.
    pub fn between(&self, from: i64, to: i64, now: i64) -> Result<Vec<Event>> {
        let conn = self.conn.lock().unwrap();
        let mut st = conn
            .prepare(
                "SELECT id, ts_start, COALESCE(ts_end, ?3), kind, app, title, url FROM events
                 WHERE ts_start >= ?1 AND ts_start < ?2 ORDER BY ts_start, id",
            )
            .map_err(db_err)?;
        let rows = st
            .query_map(params![from, to, now], |r| {
                Ok(Event {
                    id: r.get(0)?,
                    ts_start: r.get(1)?,
                    ts_end: r.get(2)?,
                    kind: r.get(3)?,
                    app: r.get(4)?,
                    title: open_value(&self.key, r.get(5)?),
                    url: r
                        .get::<_, Option<String>>(6)?
                        .map(|u| open_value(&self.key, u)),
                })
            })
            .map_err(db_err)?;
        rows.collect::<std::result::Result<_, _>>().map_err(db_err)
    }

    /// Deletes everything recorded in `[from, to)` ("delete the last hour", SPEC §9.2).
    pub fn delete_between(&self, from: i64, to: i64) -> Result<usize> {
        self.conn
            .lock()
            .unwrap()
            .execute(
                "DELETE FROM events WHERE ts_start >= ?1 AND ts_start < ?2",
                params![from, to],
            )
            .map_err(db_err)
    }

    /// Latest web visit time already stored for `app` (0 if none): where an import resumes.
    pub fn last_web_ts(&self, app: &str) -> Result<i64> {
        self.conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT COALESCE(MAX(ts_start), 0) FROM events WHERE kind = 'web' AND app = ?1",
                [app],
                |r| r.get(0),
            )
            .map_err(db_err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn focus_lifecycle_and_dangling_recovery() {
        let s = ActivityStore::open_in_memory().unwrap();
        let a = s.begin_focus(100, "code.exe", "main.rs").unwrap();
        s.end_event(a, 160).unwrap();
        let b = s.begin_focus(160, "chrome.exe", "Docs").unwrap();
        let open = s.between(0, 1000, 500).unwrap();
        assert_eq!(
            (open[0].ts_end, open[1].ts_end),
            (160, 500),
            "open events end at `now`"
        );
        assert_eq!(s.close_dangling().unwrap(), 1);
        let closed = s.between(0, 1000, 500).unwrap();
        assert_eq!(closed[1].ts_end, 160);
        s.end_event(b, 999).unwrap();
        assert_eq!(
            s.between(0, 1000, 500).unwrap()[1].ts_end,
            160,
            "already closed stays closed"
        );
    }

    #[test]
    fn web_visits_are_deduplicated_and_deletable() {
        let s = ActivityStore::open_in_memory().unwrap();
        assert!(s.add_web(10, "chrome", "https://a.example", "A").unwrap());
        assert!(!s.add_web(10, "chrome", "https://a.example", "A").unwrap());
        assert!(s.add_web(20, "chrome", "https://a.example", "A").unwrap());
        assert_eq!(s.last_web_ts("chrome").unwrap(), 20);
        assert_eq!(s.last_web_ts("firefox").unwrap(), 0);
        assert_eq!(s.delete_between(0, 15).unwrap(), 1);
        assert_eq!(s.between(0, 100, 100).unwrap().len(), 1);
    }
}

#[cfg(test)]
mod encryption_tests {
    use super::*;

    #[test]
    fn titles_and_urls_are_sealed_and_deduplicated() {
        let s = ActivityStore::open_in_memory_with_key([9u8; 32]).unwrap();
        s.begin_focus(10, "code.exe", "secret-project main.rs")
            .unwrap();
        assert!(
            s.add_web(20, "Chrome", "https://example.com/private", "Private page")
                .unwrap()
        );
        assert!(
            !s.add_web(20, "Chrome", "https://example.com/private", "Private page")
                .unwrap()
        );
        let events = s.between(0, 100, 100).unwrap();
        assert!(events.iter().any(|e| e.title == "secret-project main.rs"));
        assert!(
            events
                .iter()
                .any(|e| e.url.as_deref() == Some("https://example.com/private"))
        );
        let conn = s.conn.lock().unwrap();
        let raw: String = conn
            .query_row(
                "SELECT group_concat(title || ifnull(url,''), '|') FROM events",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(!raw.contains("secret-project") && !raw.contains("example.com"));
        assert!(raw.contains(SEAL_PREFIX));
    }

    #[test]
    fn legacy_plaintext_rows_are_sealed_on_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.sqlite");
        {
            let s = ActivityStore::open_with_key(&path, None).unwrap();
            s.begin_focus(1, "x.exe", "old plaintext title").unwrap();
        }
        let s = ActivityStore::open_with_key(&path, Some([3u8; 32])).unwrap();
        let ev = s.between(0, 10, 10).unwrap();
        assert_eq!(ev[0].title, "old plaintext title");
        let conn = Connection::open(&path).unwrap();
        let raw: String = conn
            .query_row("SELECT title FROM events", [], |r| r.get(0))
            .unwrap();
        assert!(raw.starts_with(SEAL_PREFIX));
    }
}
