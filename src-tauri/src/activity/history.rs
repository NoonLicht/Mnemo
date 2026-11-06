//! Browser history import without an extension (SPEC §5.4 alternative): reads the browsers'
//! own history databases, URL and title only. The live database is locked by the browser, so a
//! copy (with its journal/WAL) is read instead. Page text needs the extension and is out of scope.

use std::path::{Path, PathBuf};

use crate::mnemo_err::{Error, Result};
use rusqlite::{Connection, OpenFlags};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Visit {
    /// Unix seconds.
    pub ts: i64,
    pub url: String,
    pub title: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Chromium,
    Firefox,
}

#[derive(Debug, Clone)]
pub struct Browser {
    pub name: String,
    pub db: PathBuf,
    pub kind: Kind,
}

/// Seconds between 1601-01-01 (Chromium epoch) and 1970-01-01.
const CHROMIUM_EPOCH_OFFSET: i64 = 11_644_473_600;

fn err(e: impl std::fmt::Display) -> Error {
    Error::Index(format!("history: {e}"))
}

/// Browsers whose history database exists on this machine.
pub fn installed_browsers() -> Vec<Browser> {
    let mut out = Vec::new();
    let mut add = |name: &str, db: PathBuf, kind: Kind| {
        if db.is_file() {
            out.push(Browser {
                name: name.to_owned(),
                db,
                kind,
            });
        }
    };
    if let Some(local) = dirs::data_local_dir() {
        add(
            "chrome",
            local.join("Google/Chrome/User Data/Default/History"),
            Kind::Chromium,
        );
        add(
            "edge",
            local.join("Microsoft/Edge/User Data/Default/History"),
            Kind::Chromium,
        );
        add(
            "brave",
            local.join("BraveSoftware/Brave-Browser/User Data/Default/History"),
            Kind::Chromium,
        );
        add(
            "chromium",
            local.join("Chromium/User Data/Default/History"),
            Kind::Chromium,
        );
    }
    if let Some(cfg) = dirs::config_dir() {
        add(
            "chrome",
            cfg.join("google-chrome/Default/History"),
            Kind::Chromium,
        );
        add(
            "chromium",
            cfg.join("chromium/Default/History"),
            Kind::Chromium,
        );
        add(
            "brave",
            cfg.join("BraveSoftware/Brave-Browser/Default/History"),
            Kind::Chromium,
        );
    }
    // Firefox: <profiles>/<random>.<name>/places.sqlite
    let mut ff_roots = Vec::new();
    if let Some(roaming) = dirs::config_dir() {
        ff_roots.push(roaming.join("Mozilla/Firefox/Profiles"));
    }
    if let Some(home) = dirs::home_dir() {
        ff_roots.push(home.join(".mozilla/firefox"));
    }
    for root in ff_roots {
        if let Ok(rd) = std::fs::read_dir(root) {
            for e in rd.flatten() {
                add("firefox", e.path().join("places.sqlite"), Kind::Firefox);
            }
        }
    }
    out
}

/// Copies the database and its sidecar files next to each other in a temp dir.
fn snapshot(db: &Path) -> Result<(tempfile_dir::TempDir, PathBuf)> {
    let dir = tempfile_dir::TempDir::new()?;
    let name = db.file_name().ok_or_else(|| err("no file name"))?;
    let target = dir.path().join(name);
    std::fs::copy(db, &target)?;
    for suffix in ["-wal", "-journal", "-shm"] {
        let mut side = db.as_os_str().to_owned();
        side.push(suffix);
        let side = PathBuf::from(side);
        if side.is_file() {
            let mut t = target.as_os_str().to_owned();
            t.push(suffix);
            let _ = std::fs::copy(&side, PathBuf::from(t));
        }
    }
    Ok((dir, target))
}

/// Visits newer than `since` (Unix seconds), oldest first, at most `limit`.
pub fn read(browser: &Browser, since: i64, limit: usize) -> Result<Vec<Visit>> {
    let (_guard, copy) = snapshot(&browser.db)?;
    let conn = Connection::open_with_flags(&copy, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(err)?;
    let (sql, since_db) = match browser.kind {
        Kind::Chromium => (
            "SELECT urls.url, urls.title, visits.visit_time FROM visits JOIN urls ON urls.id = visits.url
             WHERE visits.visit_time > ?1 ORDER BY visits.visit_time LIMIT ?2",
            (since + CHROMIUM_EPOCH_OFFSET) * 1_000_000,
        ),
        Kind::Firefox => (
            "SELECT p.url, p.title, v.visit_date FROM moz_historyvisits v JOIN moz_places p ON p.id = v.place_id
             WHERE v.visit_date > ?1 ORDER BY v.visit_date LIMIT ?2",
            since * 1_000_000,
        ),
    };
    let mut st = conn.prepare(sql).map_err(err)?;
    let rows = st
        .query_map(rusqlite::params![since_db, limit as i64], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })
        .map_err(err)?;
    let mut out = Vec::new();
    for row in rows {
        let (url, title, t) = row.map_err(err)?;
        let ts = match browser.kind {
            Kind::Chromium => t / 1_000_000 - CHROMIUM_EPOCH_OFFSET,
            Kind::Firefox => t / 1_000_000,
        };
        out.push(Visit {
            ts,
            url,
            title: title.unwrap_or_default(),
        });
    }
    Ok(out)
}

/// A tiny self-deleting temp directory (avoids a runtime dependency on `tempfile`).
mod tempfile_dir {
    use std::path::{Path, PathBuf};

    pub struct TempDir(PathBuf);

    impl TempDir {
        pub fn new() -> std::io::Result<Self> {
            let base = std::env::temp_dir();
            for i in 0..100u32 {
                let p = base.join(format!("mnemo-hist-{}-{i}", std::process::id()));
                match std::fs::create_dir(&p) {
                    Ok(()) => return Ok(Self(p)),
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(e) => return Err(e),
                }
            }
            Err(std::io::Error::other("no free temp dir name"))
        }
        pub fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chromium_fixture(dir: &Path) -> PathBuf {
        let p = dir.join("History");
        let c = Connection::open(&p).unwrap();
        c.execute_batch(
            "CREATE TABLE urls(id INTEGER PRIMARY KEY, url TEXT, title TEXT);
             CREATE TABLE visits(id INTEGER PRIMARY KEY, url INTEGER, visit_time INTEGER);
             INSERT INTO urls VALUES (1, 'https://a.example/', 'A'), (2, 'https://b.example/', NULL);",
        )
        .unwrap();
        for (url, unix) in [
            (1, 1_700_000_000i64),
            (2, 1_700_000_500),
            (1, 1_700_001_000),
        ] {
            c.execute(
                "INSERT INTO visits(url, visit_time) VALUES (?1, ?2)",
                rusqlite::params![url, (unix + CHROMIUM_EPOCH_OFFSET) * 1_000_000],
            )
            .unwrap();
        }
        p
    }

    #[test]
    fn reads_chromium_visits_incrementally() {
        let d = tempfile::tempdir().unwrap();
        let b = Browser {
            name: "chrome".into(),
            db: chromium_fixture(d.path()),
            kind: Kind::Chromium,
        };
        let all = read(&b, 0, 100).unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(
            all[0],
            Visit {
                ts: 1_700_000_000,
                url: "https://a.example/".into(),
                title: "A".into()
            }
        );
        assert_eq!(all[1].title, "", "missing title becomes empty");
        let newer = read(&b, 1_700_000_500, 100).unwrap();
        assert_eq!(newer.len(), 1);
        assert_eq!(newer[0].ts, 1_700_001_000);
        assert_eq!(read(&b, 0, 2).unwrap().len(), 2, "limit applies");
    }

    #[test]
    fn reads_firefox_visits() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("places.sqlite");
        let c = Connection::open(&p).unwrap();
        c.execute_batch(
            "CREATE TABLE moz_places(id INTEGER PRIMARY KEY, url TEXT, title TEXT);
             CREATE TABLE moz_historyvisits(id INTEGER PRIMARY KEY, place_id INTEGER, visit_date INTEGER);
             INSERT INTO moz_places VALUES (1, 'https://f.example/', 'F');
             INSERT INTO moz_historyvisits VALUES (1, 1, 1700000000000000);",
        )
        .unwrap();
        drop(c);
        let b = Browser {
            name: "firefox".into(),
            db: p,
            kind: Kind::Firefox,
        };
        let v = read(&b, 0, 10).unwrap();
        assert_eq!(
            v,
            [Visit {
                ts: 1_700_000_000,
                url: "https://f.example/".into(),
                title: "F".into()
            }]
        );
    }

    #[test]
    fn missing_database_is_an_error_not_a_panic() {
        let b = Browser {
            name: "x".into(),
            db: PathBuf::from("/definitely/not/here"),
            kind: Kind::Chromium,
        };
        assert!(read(&b, 0, 1).is_err());
    }
}
