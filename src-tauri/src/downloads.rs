//! Downloads for offline watching. One video at a time, in the app's own
//! folder. On Android the video and audio streams are saved as two files,
//! since the player plays them together anyway (no merging needed); the
//! desktop test build saves the 360p file that has both.
//!
//! A download that stops halfway (no network, the app closed) carries on
//! from where it stopped the next time.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tokio::io::AsyncWriteExt;
use tokio::sync::Notify;

use crate::{db, player, AppState};

/// Bytes per request; YouTube slows down single requests for a whole file.
const CHUNK: u64 = 10 * 1024 * 1024;
const MAX_ATTEMPTS: u32 = 6;

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Progress {
    id: String,
    done: u64,
    total: u64,
}

pub struct Downloads {
    dir: PathBuf,
    queue: Mutex<VecDeque<String>>,
    wake: Notify,
    /// The video being downloaded right now.
    current: Mutex<Option<String>>,
    /// Set to the current video's id to stop it.
    cancel: Mutex<Option<String>>,
}

impl Downloads {
    /// `pending` are unfinished downloads from earlier runs.
    pub fn new(dir: PathBuf, pending: Vec<String>) -> Self {
        let _ = std::fs::create_dir_all(&dir);
        Self {
            dir,
            queue: Mutex::new(pending.into()),
            wake: Notify::new(),
            current: Mutex::new(None),
            cancel: Mutex::new(None),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn enqueue(&self, id: &str) {
        let mut queue = self.queue.lock().unwrap();
        if !queue.iter().any(|q| q == id) && self.current.lock().unwrap().as_deref() != Some(id) {
            queue.push_back(id.to_string());
        }
        drop(queue);
        self.wake.notify_one();
    }

    /// Stops a download (if it's running), forgets it and deletes its files.
    pub fn remove(&self, id: &str) {
        self.queue.lock().unwrap().retain(|q| q != id);
        if self.current.lock().unwrap().as_deref() == Some(id) {
            *self.cancel.lock().unwrap() = Some(id.to_string());
        }
        self.delete_files(id);
    }

    fn delete_files(&self, id: &str) {
        let Ok(entries) = std::fs::read_dir(&self.dir) else { return };
        for e in entries.filter_map(Result::ok) {
            if e.file_name().to_string_lossy().starts_with(&format!("{id}.")) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }

    pub fn current(&self) -> Option<String> {
        self.current.lock().unwrap().clone()
    }

    fn cancelled(&self, id: &str) -> bool {
        self.cancel.lock().unwrap().as_deref() == Some(id)
    }

    /// The saved files of a finished download, if they're all still there.
    pub fn files(&self, d: &db::Download) -> Option<Vec<PathBuf>> {
        if !d.done || d.files.is_empty() {
            return None;
        }
        let paths: Vec<PathBuf> = d.files.iter().map(|f| self.dir.join(f)).collect();
        paths.iter().all(|p| p.exists()).then_some(paths)
    }

    pub fn thumbnail(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.jpg"))
    }

    /// Works through the queue for as long as the app runs.
    pub async fn run(app: AppHandle) {
        loop {
            let state = app.state::<AppState>();
            let next = state.downloads.queue.lock().unwrap().pop_front();
            let Some(id) = next else {
                state.downloads.wake.notified().await;
                continue;
            };
            *state.downloads.current.lock().unwrap() = Some(id.clone());
            let _ = app.emit("downloads-changed", ());
            let result = download(&app, &id).await;
            *state.downloads.current.lock().unwrap() = None;
            let cancelled = state.downloads.cancel.lock().unwrap().take().is_some_and(|c| c == id);
            if cancelled {
                state.downloads.delete_files(&id);
            } else if let Err(e) = result {
                let _ = db::download_failed(&state.db.lock().unwrap(), &id, &e);
            }
            let _ = app.emit("downloads-changed", ());
        }
    }
}

async fn download(app: &AppHandle, id: &str) -> Result<(), String> {
    let state = app.state::<AppState>();
    let Some(item) = db::download(&state.db.lock().unwrap(), id).map_err(|e| e.to_string())? else {
        return Ok(());
    };
    // Up to two tries: stream URLs can expire during a long download.
    let mut last = String::new();
    for fresh in [false, true] {
        if fresh {
            state.streams.invalidate(id);
        }
        match download_streams(app, &item).await {
            Ok(()) => return Ok(()),
            Err(e) if state.downloads.cancelled(id) => return Err(e),
            Err(e) => last = e,
        }
    }
    Err(last)
}

async fn download_streams(app: &AppHandle, item: &db::Download) -> Result<(), String> {
    let state = app.state::<AppState>();
    let id = &item.video.id;
    let resolved = state.streams.get(&state.ytdlp, id).await?;
    if resolved.live {
        return Err("Live streams can't be downloaded.".into());
    }
    let choice = player::choose(&resolved, item.quality);
    let parts = match (choice.video, choice.audio, choice.muxed) {
        (Some(v), Some(a), _) if cfg!(not(desktop)) => vec![(v, "video"), (a, "audio")],
        (_, _, Some(m)) => vec![(m, "both")],
        _ => return Err("No stream of this video can be downloaded.".into()),
    };
    let total: u64 = parts.iter().filter_map(|(f, _)| f.size).sum();
    let mut done_before = 0;
    let mut files = Vec::new();
    for (format, kind) in parts {
        let name = format!("{id}.{kind}.{}", format.ext);
        let path = state.downloads.dir.join(&name);
        if !path.exists() {
            let part = path.with_extension(format!("{}.part", format.ext));
            let mut last_emit = Instant::now() - Duration::from_secs(1);
            fetch_to_file(&state, id, &format.url, &resolved.headers, &part, format.size, |n| {
                if last_emit.elapsed() >= Duration::from_millis(500) {
                    last_emit = Instant::now();
                    let _ = app.emit("download-progress", Progress { id: id.clone(), done: done_before + n, total });
                }
            })
            .await?;
            tokio::fs::rename(&part, &path).await.map_err(|e| e.to_string())?;
        }
        done_before += tokio::fs::metadata(&path).await.map(|m| m.len()).unwrap_or(0);
        files.push(name);
    }
    // The thumbnail too, so the Downloads list looks right offline.
    if let Ok(resp) = state.http.get(&item.video.thumbnail).send().await.and_then(|r| r.error_for_status()) {
        if let Ok(bytes) = resp.bytes().await {
            let _ = tokio::fs::write(state.downloads.thumbnail(id), &bytes).await;
        }
    }
    let conn = state.db.lock().unwrap();
    db::download_done(&conn, id, &files, done_before).map_err(|e| e.to_string())
}

/// Appends `url` to `path` in [`CHUNK`]-sized requests, starting from what's
/// already in the file.
async fn fetch_to_file(
    state: &AppState,
    id: &str,
    url: &str,
    headers: &[(String, String)],
    path: &Path,
    size: Option<u64>,
    mut progress: impl FnMut(u64),
) -> Result<(), String> {
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await
        .map_err(|e| e.to_string())?;
    let mut pos = file.metadata().await.map_err(|e| e.to_string())?.len();
    let mut total = size;
    let mut attempt = 0;
    while total.is_none_or(|t| pos < t) {
        if state.downloads.cancelled(id) {
            return Err("cancelled".into());
        }
        let end = total.map_or(pos + CHUNK - 1, |t| (pos + CHUNK).min(t) - 1);
        let mut req = state.http.get(url).timeout(Duration::from_secs(120));
        for (k, v) in headers {
            req = req.header(k.as_str(), v.as_str());
        }
        let resp = req.header(reqwest::header::RANGE, format!("bytes={pos}-{end}")).send().await;
        let resp = match resp {
            Ok(r) if r.status() == 416 => break,
            Ok(r) if r.status().is_success() => r,
            // Fresh URLs answer 403 for a moment; the network may drop too.
            other => {
                attempt += 1;
                if attempt >= MAX_ATTEMPTS {
                    return Err(match other {
                        Ok(r) => format!("YouTube returned {}", r.status()),
                        Err(e) => format!("Download stopped: {e}"),
                    });
                }
                tokio::time::sleep(Duration::from_millis(800 * u64::from(attempt))).await;
                continue;
            }
        };
        if total.is_none() {
            total = resp
                .headers()
                .get(reqwest::header::CONTENT_RANGE)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.rsplit('/').next())
                .and_then(|v| v.parse().ok());
        }
        let bytes = resp.bytes().await.map_err(|e| format!("Download stopped: {e}"))?;
        if bytes.is_empty() {
            break;
        }
        file.write_all(&bytes).await.map_err(|e| e.to_string())?;
        pos += bytes.len() as u64;
        attempt = 0;
        progress(pos);
        // Without a known size, a short answer means the end.
        if total.is_none() && (bytes.len() as u64) < CHUNK {
            break;
        }
    }
    file.flush().await.map_err(|e| e.to_string())
}
