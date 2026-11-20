//! Everything Vista remembers, in one SQLite file: subscriptions, the feed of
//! their uploads, watch history (with where you stopped) and Watch later.
//! Videos are stored as JSON next to the columns they are looked up by.

use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::feeds::Entry;
use crate::youtube::Playlist;
use crate::{Channel, Video};

pub fn open(path: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open(path)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS subscriptions (
            channel_id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            avatar TEXT,
            added INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS feed (
            video_id TEXT PRIMARY KEY,
            channel_id TEXT NOT NULL,
            published INTEGER NOT NULL,
            short INTEGER NOT NULL,
            data TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS feed_published ON feed (published DESC);
        CREATE TABLE IF NOT EXISTS history (
            video_id TEXT PRIMARY KEY,
            data TEXT NOT NULL,
            position REAL NOT NULL DEFAULT 0,
            duration REAL,
            watched INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS history_watched ON history (watched DESC);
        CREATE TABLE IF NOT EXISTS later (
            video_id TEXT PRIMARY KEY,
            data TEXT NOT NULL,
            added INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS downloads (
            video_id TEXT PRIMARY KEY,
            data TEXT NOT NULL,
            quality INTEGER NOT NULL,
            files TEXT NOT NULL DEFAULT '[]',
            size INTEGER NOT NULL DEFAULT 0,
            done INTEGER NOT NULL DEFAULT 0,
            error TEXT,
            added INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS saved_playlists (
            playlist_id TEXT PRIMARY KEY,
            data TEXT NOT NULL,
            added INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS searches (
            query TEXT PRIMARY KEY COLLATE NOCASE,
            at INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS hidden (
            id TEXT PRIMARY KEY,
            kind TEXT NOT NULL,
            name TEXT NOT NULL,
            added INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS meta (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );",
    )?;
    Ok(conn)
}

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn to_json(v: &Video) -> String {
    serde_json::to_string(v).unwrap_or_default()
}

fn from_json(s: String) -> Option<Video> {
    serde_json::from_str(&s).ok()
}

// ---- subscriptions -------------------------------------------------------

pub fn subscriptions(conn: &Connection) -> rusqlite::Result<Vec<Channel>> {
    let mut stmt = conn.prepare("SELECT channel_id, name, avatar FROM subscriptions ORDER BY name COLLATE NOCASE")?;
    let rows = stmt.query_map([], |r| {
        Ok(Channel {
            id: r.get(0)?,
            name: r.get(1)?,
            avatar: r.get(2)?,
            ..Default::default()
        })
    })?;
    rows.collect()
}

pub fn subscribe(conn: &Connection, c: &Channel) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO subscriptions (channel_id, name, avatar, added) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (channel_id) DO UPDATE SET name = excluded.name, avatar = COALESCE(excluded.avatar, avatar)",
        params![c.id, c.name, c.avatar, now()],
    )?;
    Ok(())
}

pub fn unsubscribe(conn: &Connection, channel_id: &str) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM subscriptions WHERE channel_id = ?1", [channel_id])?;
    conn.execute("DELETE FROM feed WHERE channel_id = ?1", [channel_id])?;
    Ok(())
}

pub fn is_subscribed(conn: &Connection, channel_id: &str) -> rusqlite::Result<bool> {
    conn.query_row("SELECT 1 FROM subscriptions WHERE channel_id = ?1", [channel_id], |_| Ok(()))
        .optional()
        .map(|r| r.is_some())
}

/// Channels whose name or avatar still need looking up (imported ones).
pub fn incomplete_subscriptions(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT channel_id FROM subscriptions WHERE avatar IS NULL")?;
    let rows = stmt.query_map([], |r| r.get(0))?;
    rows.collect()
}

/// Adds the channels from a Google Takeout `subscriptions.csv`. Returns how
/// many were new.
pub fn import_subscriptions(conn: &mut Connection, channels: &[Channel]) -> rusqlite::Result<usize> {
    let tx = conn.transaction()?;
    let mut added = 0;
    for c in channels {
        added += tx.execute(
            "INSERT OR IGNORE INTO subscriptions (channel_id, name, avatar, added) VALUES (?1, ?2, NULL, ?3)",
            params![c.id, c.name, now()],
        )?;
    }
    tx.commit()?;
    Ok(added)
}

// ---- feed ----------------------------------------------------------------

/// How long feed entries are kept.
const FEED_KEEP_SECS: i64 = 120 * 24 * 60 * 60;

pub fn save_feed(conn: &mut Connection, entries: &[Entry]) -> rusqlite::Result<()> {
    let tx = conn.transaction()?;
    {
        let mut stmt = tx.prepare(
            "INSERT INTO feed (video_id, channel_id, published, short, data) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (video_id) DO UPDATE SET data = excluded.data, short = excluded.short",
        )?;
        for e in entries {
            let (Some(channel_id), Some(published)) = (&e.video.channel_id, e.video.published_at) else {
                continue;
            };
            stmt.execute(params![e.video.id, channel_id, published, e.short, to_json(&e.video)])?;
        }
    }
    tx.execute("DELETE FROM feed WHERE published < ?1", [now() - FEED_KEEP_SECS])?;
    tx.execute("DELETE FROM feed WHERE channel_id NOT IN (SELECT channel_id FROM subscriptions)", [])?;
    tx.commit()
}

/// The newest uploads of subscribed channels, with each channel's avatar.
pub fn feed(conn: &Connection, shorts: bool, limit: u32) -> rusqlite::Result<Vec<Video>> {
    let mut stmt = conn.prepare(
        "SELECT f.data, s.avatar FROM feed f JOIN subscriptions s ON s.channel_id = f.channel_id
         WHERE ?1 OR f.short = 0 ORDER BY f.published DESC LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![shorts, limit], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)))?;
    Ok(rows
        .filter_map(Result::ok)
        .filter_map(|(data, avatar)| {
            let mut v = from_json(data)?;
            v.avatar = avatar;
            Some(v)
        })
        .collect())
}

pub fn meta(conn: &Connection, key: &str) -> rusqlite::Result<Option<String>> {
    conn.query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0)).optional()
}

pub fn set_meta(conn: &Connection, key: &str, value: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO meta (key, value) VALUES (?1, ?2) ON CONFLICT (key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

// ---- history and Watch later --------------------------------------------

/// A watched video and where it was left off.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Watched {
    pub video: Video,
    pub position: f64,
    pub duration: Option<f64>,
    pub watched: i64,
}

pub fn history_add(conn: &Connection, v: &Video) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO history (video_id, data, duration, watched) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (video_id) DO UPDATE SET data = excluded.data, watched = excluded.watched,
           duration = COALESCE(excluded.duration, duration)",
        params![v.id, to_json(v), v.duration, now()],
    )?;
    Ok(())
}

pub fn history_progress(conn: &Connection, id: &str, position: f64, duration: Option<f64>) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE history SET position = ?2, duration = COALESCE(?3, duration) WHERE video_id = ?1",
        params![id, position, duration],
    )?;
    Ok(())
}

pub fn history(conn: &Connection, limit: u32) -> rusqlite::Result<Vec<Watched>> {
    let mut stmt = conn.prepare("SELECT data, position, duration, watched FROM history ORDER BY watched DESC LIMIT ?1")?;
    let rows = stmt.query_map([limit], |r| {
        Ok((r.get::<_, String>(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
    })?;
    Ok(rows
        .filter_map(Result::ok)
        .filter_map(|(data, position, duration, watched)| {
            Some(Watched {
                video: from_json(data)?,
                position,
                duration,
                watched,
            })
        })
        .collect())
}

/// Where each of `ids` was left off, for progress bars on thumbnails.
pub fn positions(conn: &Connection, ids: &[String]) -> rusqlite::Result<Vec<(String, f64, Option<f64>)>> {
    let mut stmt = conn.prepare("SELECT position, duration FROM history WHERE video_id = ?1")?;
    let mut out = Vec::new();
    for id in ids {
        if let Some((p, d)) = stmt.query_row([id], |r| Ok((r.get(0)?, r.get(1)?))).optional()? {
            out.push((id.clone(), p, d));
        }
    }
    Ok(out)
}

pub fn history_remove(conn: &Connection, id: Option<&str>) -> rusqlite::Result<()> {
    match id {
        Some(id) => conn.execute("DELETE FROM history WHERE video_id = ?1", [id])?,
        None => conn.execute("DELETE FROM history", [])?,
    };
    Ok(())
}

pub fn later(conn: &Connection) -> rusqlite::Result<Vec<Video>> {
    let mut stmt = conn.prepare("SELECT data FROM later ORDER BY added DESC")?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    Ok(rows.filter_map(Result::ok).filter_map(from_json).collect())
}

pub fn later_set(conn: &Connection, v: &Video, saved: bool) -> rusqlite::Result<()> {
    if saved {
        conn.execute(
            "INSERT OR REPLACE INTO later (video_id, data, added) VALUES (?1, ?2, ?3)",
            params![v.id, to_json(v), now()],
        )?;
    } else {
        conn.execute("DELETE FROM later WHERE video_id = ?1", [&v.id])?;
    }
    Ok(())
}

pub fn is_later(conn: &Connection, id: &str) -> rusqlite::Result<bool> {
    conn.query_row("SELECT 1 FROM later WHERE video_id = ?1", [id], |_| Ok(()))
        .optional()
        .map(|r| r.is_some())
}

// ---- downloads -----------------------------------------------------------

/// A video saved for offline watching, or waiting to be.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Download {
    pub video: Video,
    pub quality: u32,
    /// File names in the downloads folder: video and audio, or one file with both.
    pub files: Vec<String>,
    pub size: u64,
    pub done: bool,
    pub error: Option<String>,
}

pub fn download_add(conn: &Connection, v: &Video, quality: u32) -> rusqlite::Result<bool> {
    let added = conn.execute(
        "INSERT OR IGNORE INTO downloads (video_id, data, quality, added) VALUES (?1, ?2, ?3, ?4)",
        params![v.id, to_json(v), quality, now()],
    )?;
    // Trying again after an error.
    conn.execute("UPDATE downloads SET error = NULL WHERE video_id = ?1 AND done = 0", [&v.id])?;
    Ok(added > 0)
}

pub fn download_done(conn: &Connection, id: &str, files: &[String], size: u64) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE downloads SET files = ?2, size = ?3, done = 1, error = NULL WHERE video_id = ?1",
        params![id, serde_json::to_string(files).unwrap_or_default(), size as i64],
    )?;
    Ok(())
}

pub fn download_failed(conn: &Connection, id: &str, error: &str) -> rusqlite::Result<()> {
    conn.execute("UPDATE downloads SET error = ?2 WHERE video_id = ?1", params![id, error])?;
    Ok(())
}

pub fn download_remove(conn: &Connection, id: &str) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM downloads WHERE video_id = ?1", [id])?;
    Ok(())
}

pub fn downloads(conn: &Connection) -> rusqlite::Result<Vec<Download>> {
    let mut stmt = conn.prepare("SELECT data, quality, files, size, done, error FROM downloads ORDER BY added DESC")?;
    let rows = stmt.query_map([], |r| {
        Ok((r.get::<_, String>(0)?, r.get(1)?, r.get::<_, String>(2)?, r.get::<_, i64>(3)?, r.get(4)?, r.get(5)?))
    })?;
    Ok(rows
        .filter_map(Result::ok)
        .filter_map(|(data, quality, files, size, done, error)| {
            Some(Download {
                video: from_json(data)?,
                quality,
                files: serde_json::from_str(&files).unwrap_or_default(),
                size: size as u64,
                done,
                error,
            })
        })
        .collect())
}

pub fn download(conn: &Connection, id: &str) -> rusqlite::Result<Option<Download>> {
    Ok(downloads(conn)?.into_iter().find(|d| d.video.id == id))
}

// ---- searches --------------------------------------------------------------

/// Searches kept for recommendations.
const SEARCHES_KEPT: u32 = 50;

pub fn search_add(conn: &Connection, query: &str) -> rusqlite::Result<()> {
    let query = query.trim();
    if query.is_empty() {
        return Ok(());
    }
    conn.execute(
        "INSERT INTO searches (query, at) VALUES (?1, ?2) ON CONFLICT (query) DO UPDATE SET at = excluded.at",
        params![query, now()],
    )?;
    conn.execute(
        "DELETE FROM searches WHERE query NOT IN (SELECT query FROM searches ORDER BY at DESC LIMIT ?1)",
        [SEARCHES_KEPT],
    )?;
    Ok(())
}

/// The latest searches, newest first.
pub fn searches(conn: &Connection, limit: u32) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT query FROM searches ORDER BY at DESC LIMIT ?1")?;
    let rows = stmt.query_map([limit], |r| r.get(0))?;
    rows.collect()
}

pub fn search_remove(conn: &Connection, query: &str) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM searches WHERE query = ?1", [query])?;
    Ok(())
}

pub fn searches_clear(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM searches", [])?;
    Ok(())
}

// ---- not interested ------------------------------------------------------

/// A video or channel kept out of For you, topics and autoplay.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Hidden {
    /// A video id, or a channel id ("UC…").
    pub id: String,
    /// "video" or "channel".
    pub kind: String,
    /// The video's title or the channel's name, for Settings to list.
    pub name: String,
}

pub fn hide(conn: &Connection, h: &Hidden) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO hidden (id, kind, name, added) VALUES (?1, ?2, ?3, ?4)",
        params![h.id, h.kind, h.name, now()],
    )?;
    Ok(())
}

pub fn unhide(conn: &Connection, id: &str) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM hidden WHERE id = ?1", [id])?;
    Ok(())
}

/// Newest first.
pub fn hidden(conn: &Connection) -> rusqlite::Result<Vec<Hidden>> {
    let mut stmt = conn.prepare("SELECT id, kind, name FROM hidden ORDER BY added DESC")?;
    let rows = stmt.query_map([], |r| Ok(Hidden { id: r.get(0)?, kind: r.get(1)?, name: r.get(2)? }))?;
    rows.collect()
}

// ---- saved playlists -----------------------------------------------------

pub fn saved_playlists(conn: &Connection) -> rusqlite::Result<Vec<Playlist>> {
    let mut stmt = conn.prepare("SELECT data FROM saved_playlists ORDER BY added DESC")?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    Ok(rows.filter_map(Result::ok).filter_map(|s| serde_json::from_str(&s).ok()).collect())
}

pub fn playlist_save(conn: &Connection, p: &Playlist, saved: bool) -> rusqlite::Result<()> {
    if saved {
        conn.execute(
            "INSERT OR REPLACE INTO saved_playlists (playlist_id, data, added) VALUES (?1, ?2, ?3)",
            params![p.id, serde_json::to_string(p).unwrap_or_default(), now()],
        )?;
    } else {
        conn.execute("DELETE FROM saved_playlists WHERE playlist_id = ?1", [&p.id])?;
    }
    Ok(())
}

pub fn is_playlist_saved(conn: &Connection, id: &str) -> rusqlite::Result<bool> {
    conn.query_row("SELECT 1 FROM saved_playlists WHERE playlist_id = ?1", [id], |_| Ok(()))
        .optional()
        .map(|r| r.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn video(id: &str, channel_id: &str, published: i64) -> Video {
        Video {
            id: id.into(),
            title: format!("Video {id}"),
            channel_id: Some(channel_id.into()),
            published_at: Some(published),
            ..Default::default()
        }
    }

    fn channel(id: &str, name: &str) -> Channel {
        Channel { id: id.into(), name: name.into(), avatar: Some(format!("https://yt3.ggpht.com/{id}")), ..Default::default() }
    }

    #[test]
    fn feed_follows_subscriptions() {
        let mut conn = open(Path::new(":memory:")).unwrap();
        subscribe(&conn, &channel("UCa", "Alpha")).unwrap();
        subscribe(&conn, &channel("UCb", "beta")).unwrap();
        let t = now();
        let entries = vec![
            Entry { video: video("v1", "UCa", t - 100), short: false },
            Entry { video: video("v2", "UCb", t - 50), short: true },
            Entry { video: video("v3", "UCb", t - 10), short: false },
            Entry { video: video("old", "UCa", t - FEED_KEEP_SECS - 1), short: false },
        ];
        save_feed(&mut conn, &entries).unwrap();
        let ids = |vs: Vec<Video>| vs.into_iter().map(|v| v.id).collect::<Vec<_>>();
        assert_eq!(ids(feed(&conn, true, 10).unwrap()), ["v3", "v2", "v1"]);
        assert_eq!(ids(feed(&conn, false, 10).unwrap()), ["v3", "v1"]);
        assert_eq!(feed(&conn, false, 10).unwrap()[0].avatar.as_deref(), Some("https://yt3.ggpht.com/UCb"));
        unsubscribe(&conn, "UCb").unwrap();
        assert_eq!(ids(feed(&conn, true, 10).unwrap()), ["v1"]);
        let names: Vec<_> = subscriptions(&conn).unwrap().into_iter().map(|c| c.name).collect();
        assert_eq!(names, ["Alpha"]);
    }

    #[test]
    fn imports_keep_existing_channels() {
        let mut conn = open(Path::new(":memory:")).unwrap();
        subscribe(&conn, &channel("UCa", "Alpha")).unwrap();
        let imported = [Channel { id: "UCa".into(), name: "Old name".into(), ..Default::default() }, Channel { id: "UCz".into(), name: "Zed".into(), ..Default::default() }];
        assert_eq!(import_subscriptions(&mut conn, &imported).unwrap(), 1);
        assert_eq!(incomplete_subscriptions(&conn).unwrap(), ["UCz"]);
        assert_eq!(subscriptions(&conn).unwrap()[0].name, "Alpha");
        // Filling in the avatar keeps the channel and completes it.
        subscribe(&conn, &channel("UCz", "Zed")).unwrap();
        assert!(incomplete_subscriptions(&conn).unwrap().is_empty());
    }

    #[test]
    fn remembers_progress_and_later() {
        let conn = open(Path::new(":memory:")).unwrap();
        let v = video("abc", "UCa", 0);
        history_add(&conn, &v).unwrap();
        history_progress(&conn, "abc", 42.5, Some(600.0)).unwrap();
        let h = history(&conn, 10).unwrap();
        assert_eq!((h[0].position, h[0].duration), (42.5, Some(600.0)));
        // Watching again keeps the place.
        history_add(&conn, &v).unwrap();
        assert_eq!(positions(&conn, &["abc".into(), "nope".into()]).unwrap(), [("abc".to_string(), 42.5, Some(600.0))]);
        history_remove(&conn, None).unwrap();
        assert!(history(&conn, 10).unwrap().is_empty());

        later_set(&conn, &v, true).unwrap();
        assert!(is_later(&conn, "abc").unwrap());
        assert_eq!(later(&conn).unwrap().len(), 1);
        later_set(&conn, &v, false).unwrap();
        assert!(!is_later(&conn, "abc").unwrap());
    }

    #[test]
    fn keeps_downloads_and_playlists() {
        let conn = open(Path::new(":memory:")).unwrap();
        let v = video("abc", "UCa", 0);
        assert!(download_add(&conn, &v, 720).unwrap());
        assert!(!download_add(&conn, &v, 720).unwrap());
        download_failed(&conn, "abc", "no network").unwrap();
        assert_eq!(download(&conn, "abc").unwrap().unwrap().error.as_deref(), Some("no network"));
        // Asking again clears the error.
        download_add(&conn, &v, 720).unwrap();
        assert!(download(&conn, "abc").unwrap().unwrap().error.is_none());
        download_done(&conn, "abc", &["abc.video.mp4".into(), "abc.audio.m4a".into()], 1234).unwrap();
        let d = download(&conn, "abc").unwrap().unwrap();
        assert!(d.done && d.size == 1234 && d.files.len() == 2);
        download_remove(&conn, "abc").unwrap();
        assert!(downloads(&conn).unwrap().is_empty());

        let p = Playlist { id: "PLx".into(), title: "Rain".into(), ..Default::default() };
        playlist_save(&conn, &p, true).unwrap();
        assert!(is_playlist_saved(&conn, "PLx").unwrap());
        assert_eq!(saved_playlists(&conn).unwrap(), std::slice::from_ref(&p));
        playlist_save(&conn, &p, false).unwrap();
        assert!(saved_playlists(&conn).unwrap().is_empty());
    }

    #[test]
    fn remembers_searches() {
        let conn = open(Path::new(":memory:")).unwrap();
        search_add(&conn, "cats").unwrap();
        search_add(&conn, "  ").unwrap();
        search_add(&conn, "dogs").unwrap();
        // Searching again (any case) moves it to the front instead of repeating it.
        conn.execute("UPDATE searches SET at = at - 10", []).unwrap();
        search_add(&conn, "Cats").unwrap();
        assert_eq!(searches(&conn, 10).unwrap(), ["cats", "dogs"]);
        search_remove(&conn, "dogs").unwrap();
        assert_eq!(searches(&conn, 10).unwrap(), ["cats"]);
        for i in 0..SEARCHES_KEPT + 5 {
            search_add(&conn, &format!("q{i}")).unwrap();
        }
        assert_eq!(searches(&conn, 1000).unwrap().len(), SEARCHES_KEPT as usize);
        searches_clear(&conn).unwrap();
        assert!(searches(&conn, 10).unwrap().is_empty());
    }
}
