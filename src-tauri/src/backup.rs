//! Backing up and restoring what you made in Vista: subscriptions, watch
//! history (with where you stopped), Watch later, saved playlists, searches,
//! what you said you're not interested in, and the page's settings. A backup
//! is one file: a consistent SQLite copy of the database (made with `VACUUM
//! INTO`) plus a `backup_meta` table. The feed and For you are left to be
//! fetched again, and downloaded videos stay out because they can be
//! gigabytes. (Adapted from Cadence's backup.rs.)

use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use serde::Serialize;

/// Marks a file as a Vista backup.
const MAGIC: &str = "vista-backup-1";
/// Tables copied back on restore.
const TABLES: &[&str] = &["subscriptions", "history", "later", "saved_playlists", "searches", "hidden"];
/// Safety copies made before a restore that are kept.
const KEEP_SAFETY_COPIES: usize = 5;
/// How automatic backups' names start; only these are ever deleted. (Phones
/// prune in Kotlin, Backups.kt.)
#[cfg_attr(not(desktop), allow(dead_code))]
pub const AUTO_PREFIX: &str = "Vista auto backup ";

/// What a backup holds, shown before restoring it.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
    /// Seconds since 1970.
    pub created_at: i64,
    pub app_version: String,
    pub subscriptions: i64,
    pub history: i64,
    pub later: i64,
    pub playlists: i64,
}

type Res<T> = Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

fn table_exists(conn: &Connection, schema: &str, table: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        &format!("SELECT 1 FROM {schema}.sqlite_master WHERE type = 'table' AND name = ?1"),
        params![table],
        |_| Ok(()),
    )
    .optional()
    .map(|r| r.is_some())
}

fn count(conn: &Connection, table: &str) -> i64 {
    if !table_exists(conn, "main", table).unwrap_or(false) {
        return 0;
    }
    conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0)).unwrap_or(0)
}

fn meta(conn: &Connection, key: &str) -> rusqlite::Result<Option<String>> {
    conn.query_row("SELECT value FROM backup_meta WHERE key = ?1", params![key], |r| r.get(0))
        .optional()
}

/// Writes a backup of `conn`'s database, with the page's `settings` (JSON),
/// to `dest`. Goes through a temporary file, so a failure never leaves a
/// half-written backup behind.
pub fn create(conn: &Connection, dest: &Path, settings: &str) -> Res<Summary> {
    let tmp = dest.with_extension("part");
    let _ = std::fs::remove_file(&tmp);
    let result = (|| {
        conn.execute("VACUUM INTO ?1", params![tmp.to_string_lossy()]).map_err(err)?;
        let copy = Connection::open(&tmp).map_err(err)?;
        // Things fetched again anyway make the file smaller left out.
        copy.execute_batch(
            "DELETE FROM feed; DELETE FROM downloads; DELETE FROM meta;
             CREATE TABLE backup_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
        )
        .map_err(err)?;
        let created_at = crate::db::now();
        for (key, value) in [
            ("magic", MAGIC.to_string()),
            ("app_version", env!("CARGO_PKG_VERSION").to_string()),
            ("created_at", created_at.to_string()),
            ("settings", settings.to_string()),
        ] {
            copy.execute("INSERT INTO backup_meta(key, value) VALUES (?1, ?2)", params![key, value])
                .map_err(err)?;
        }
        copy.execute_batch("VACUUM").map_err(err)?;
        let summary = summarize(&copy)?;
        drop(copy);
        // Replaces an existing file of the same name.
        std::fs::rename(&tmp, dest).map_err(err)?;
        Ok(summary)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

fn summarize(conn: &Connection) -> Res<Summary> {
    let bad = || "That file isn't a Vista backup.".to_string();
    if !table_exists(conn, "main", "backup_meta").map_err(|_| bad())? || meta(conn, "magic").map_err(err)?.as_deref() != Some(MAGIC) {
        return Err(bad());
    }
    Ok(Summary {
        created_at: meta(conn, "created_at").map_err(err)?.and_then(|v| v.parse().ok()).unwrap_or(0),
        app_version: meta(conn, "app_version").map_err(err)?.unwrap_or_default(),
        subscriptions: count(conn, "subscriptions"),
        history: count(conn, "history"),
        later: count(conn, "later"),
        playlists: count(conn, "saved_playlists"),
    })
}

/// Reads a backup's summary and settings without changing anything.
pub fn peek(path: &Path) -> Res<(Summary, String)> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|_| "Couldn't open that file.".to_string())?;
    let summary = summarize(&conn).map_err(|_| "That file isn't a Vista backup.".to_string())?;
    let settings = meta(&conn, "settings").map_err(err)?.unwrap_or_else(|| "{}".into());
    Ok((summary, settings))
}

fn columns(conn: &Connection, schema: &str, table: &str) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare(&format!("PRAGMA {schema}.table_info({table})"))?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
    rows.collect()
}

/// Replaces the data in `conn` with the backup's. A safety copy of the
/// current data (with `current_settings`) goes into `safety_dir` first.
/// Returns the backup's summary and its settings for the page to apply.
pub fn restore(conn: &Connection, path: &Path, safety_dir: &Path, current_settings: &str) -> Res<(Summary, String, PathBuf)> {
    let (summary, settings) = peek(path)?;
    std::fs::create_dir_all(safety_dir).map_err(err)?;
    let safety = safety_dir.join(format!("before-restore-{}.vista-backup", crate::db::now()));
    create(conn, &safety, current_settings)?;
    prune(safety_dir, "before-restore-", KEEP_SAFETY_COPIES);

    conn.execute("ATTACH DATABASE ?1 AS b", params![path.to_string_lossy()]).map_err(err)?;
    let copied = (|| -> rusqlite::Result<()> {
        let tx = conn.unchecked_transaction()?;
        for table in TABLES {
            tx.execute(&format!("DELETE FROM main.{table}"), [])?;
            if !table_exists(&tx, "b", table)? {
                continue;
            }
            // Only columns both sides know, so older and newer backups work.
            let theirs = columns(&tx, "b", table)?;
            let shared: Vec<String> = columns(&tx, "main", table)?
                .into_iter()
                .filter(|c| theirs.contains(c))
                .map(|c| format!("\"{c}\""))
                .collect();
            let list = shared.join(", ");
            tx.execute(&format!("INSERT INTO main.{table} ({list}) SELECT {list} FROM b.{table}"), [])?;
        }
        // New subscriptions and history: the feed and For you start over.
        tx.execute("DELETE FROM main.meta WHERE key IN ('feed_checked', 'recs')", [])?;
        tx.commit()
    })();
    let _ = conn.execute("DETACH DATABASE b", []);
    copied.map_err(|e| format!("Restoring failed, nothing was changed: {e}"))?;
    Ok((summary, settings, safety))
}

/// Keeps only the newest `keep` files in `dir` whose names start with
/// `prefix`. Their names end in a date or time, so they sort oldest first.
pub fn prune(dir: &Path, prefix: &str, keep: usize) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with(prefix)))
        .collect();
    files.sort();
    let excess = files.len().saturating_sub(keep);
    for old in files.into_iter().take(excess) {
        let _ = std::fs::remove_file(old);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{db, Channel, Video};

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("vista-backup-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn channel(id: &str) -> Channel {
        Channel { id: id.into(), name: format!("Channel {id}"), ..Default::default() }
    }

    fn video(id: &str) -> Video {
        Video { id: id.into(), title: format!("Video {id}"), channel: "c".into(), ..Default::default() }
    }

    #[test]
    fn backup_and_restore_round_trip() {
        let dir = temp_dir("round");
        let conn = db::open(&dir.join("vista.db")).unwrap();
        db::subscribe(&conn, &channel("UCa")).unwrap();
        db::history_add(&conn, &video("v1")).unwrap();
        db::later_set(&conn, &video("v2"), true).unwrap();
        db::search_add(&conn, "cats").unwrap();
        db::hide(&conn, &db::Hidden { id: "UCx".into(), kind: "channel".into(), name: "Loud".into() }).unwrap();
        db::set_meta(&conn, "recs", "{}").unwrap();

        let file = dir.join("my.vista-backup");
        let made = create(&conn, &file, r#"{"quality":720}"#).unwrap();
        assert_eq!((made.subscriptions, made.history, made.later, made.playlists), (1, 1, 1, 0));
        assert!(!dir.join("my.part").exists());

        // Change everything after the backup.
        db::unsubscribe(&conn, "UCa").unwrap();
        db::subscribe(&conn, &channel("UCb")).unwrap();
        db::later_set(&conn, &video("v2"), false).unwrap();
        db::searches_clear(&conn).unwrap();
        db::unhide(&conn, "UCx").unwrap();

        let (summary, settings, safety) = restore(&conn, &file, &dir.join("safety"), "{}").unwrap();
        assert_eq!(summary, made);
        assert_eq!(settings, r#"{"quality":720}"#);
        let subs: Vec<String> = db::subscriptions(&conn).unwrap().into_iter().map(|c| c.id).collect();
        assert_eq!(subs, ["UCa"]);
        assert_eq!(db::later(&conn).unwrap().len(), 1);
        assert_eq!(db::searches(&conn, 10).unwrap(), ["cats"]);
        assert_eq!(db::hidden(&conn).unwrap().len(), 1);
        // For you is made again for the restored history.
        assert!(db::meta(&conn, "recs").unwrap().is_none());
        // The data replaced by the restore was kept.
        let (before, _) = peek(&safety).unwrap();
        assert_eq!((before.subscriptions, before.later), (1, 0));
        // Windows won't delete a database that is still open.
        drop(conn);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rejects_other_files() {
        let dir = temp_dir("reject");
        std::fs::write(dir.join("notes.txt"), "hello").unwrap();
        assert!(peek(&dir.join("notes.txt")).is_err());
        // A plain database that isn't a backup.
        drop(db::open(&dir.join("other.db")).unwrap());
        assert!(peek(&dir.join("other.db")).is_err());
        assert!(peek(&dir.join("missing.vista-backup")).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn keeps_only_the_newest() {
        let dir = temp_dir("prune");
        for day in ["2026-11-01", "2026-11-08", "2026-11-15", "2026-11-22", "2026-11-29"] {
            std::fs::write(dir.join(format!("{AUTO_PREFIX}{day}.vista-backup")), "x").unwrap();
        }
        std::fs::write(dir.join("Vista backup 2026-10-01.vista-backup"), "x").unwrap();
        prune(&dir, AUTO_PREFIX, 4);
        let mut left: Vec<String> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        left.sort();
        assert_eq!(left.len(), 5);
        // The oldest automatic one went; the one made by hand stayed.
        assert!(!left.iter().any(|n| n.contains("2026-11-01")));
        assert!(left.iter().any(|n| n == "Vista backup 2026-10-01.vista-backup"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
