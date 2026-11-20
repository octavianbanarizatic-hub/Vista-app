//! Resolved streams, and the `media://` protocol the desktop <video> element
//! reads from.
//!
//! - `yt/<videoId>` streams the video's 360p file (picture and sound in one),
//!   fetched in ranges from YouTube's CDN, or from disk when it's downloaded
//! - `thumb/<videoId>` returns a downloaded video's thumbnail
//! - `img/<url>` returns a channel avatar through an on-disk cache
//!
//! On Android the native player fetches streams itself.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tauri::http::{header, Request, Response, StatusCode};
use tauri::{AppHandle, Manager, UriSchemeResponder};
use tokio::sync::OnceCell;

use crate::ytdlp::{Resolved, YtDlp};
use crate::AppState;

/// YouTube throttles large single requests, so stream in modest ranges.
const YT_CHUNK: u64 = 2 * 1024 * 1024;
/// Stream URLs expire after about six hours; refresh well before that.
const STREAM_TTL: Duration = Duration::from_secs(3 * 60 * 60);
/// YouTube's CDN rejects a stream URL with 403 for a moment after it is
/// issued, so wait this long before first using one, and retry 403s.
pub const URL_WARMUP: Duration = Duration::from_millis(1500);
const MAX_ATTEMPTS: u32 = 6;

type Fail = (StatusCode, String);

struct Entry {
    created: Instant,
    cell: Arc<OnceCell<Resolved>>,
}

/// Resolved streams by video id. Concurrent requests for the same id share
/// one yt-dlp run.
#[derive(Default)]
pub struct StreamCache {
    map: Mutex<HashMap<String, Entry>>,
}

impl StreamCache {
    pub async fn get(&self, ytdlp: &YtDlp, id: &str) -> Result<Resolved, String> {
        let cell = {
            let mut map = self.map.lock().unwrap();
            map.retain(|_, e| e.created.elapsed() < STREAM_TTL);
            map.entry(id.to_string())
                .or_insert_with(|| Entry {
                    created: Instant::now(),
                    cell: Arc::new(OnceCell::new()),
                })
                .cell
                .clone()
        };
        cell.get_or_try_init(|| ytdlp.resolve(id)).await.cloned()
    }

    /// Whether streams for `id` are found (or being found) already.
    pub fn contains(&self, id: &str) -> bool {
        self.map.lock().unwrap().get(id).is_some_and(|e| e.created.elapsed() < STREAM_TTL)
    }

    pub fn invalidate(&self, id: &str) {
        self.map.lock().unwrap().remove(id);
    }
}

pub fn handle(app: AppHandle, request: Request<Vec<u8>>, responder: UriSchemeResponder) {
    tauri::async_runtime::spawn(async move {
        let response = if request.method() == "OPTIONS" {
            base(StatusCode::NO_CONTENT).body(Vec::new()).unwrap()
        } else {
            serve(&app, &request).await.unwrap_or_else(|(status, msg)| {
                base(status)
                    .header(header::CONTENT_TYPE, "text/plain")
                    .body(msg.into_bytes())
                    .unwrap()
            })
        };
        responder.respond(response);
    });
}

fn base(status: StatusCode) -> tauri::http::response::Builder {
    Response::builder()
        .status(status)
        .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
        .header(header::ACCESS_CONTROL_ALLOW_HEADERS, "*")
        .header(header::ACCESS_CONTROL_EXPOSE_HEADERS, "Content-Range, Content-Length")
}

/// Reads `Range: bytes=start-[end]`. Suffix ranges are treated as a full read.
fn parse_range(request: &Request<Vec<u8>>) -> (u64, Option<u64>) {
    let Some(spec) = request
        .headers()
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("bytes="))
    else {
        return (0, None);
    };
    let (a, b) = spec.split_once('-').unwrap_or((spec, ""));
    let start = a.trim().parse().unwrap_or(0);
    let end = b.trim().parse().ok().filter(|&e| e >= start);
    (start, end)
}

async fn serve(app: &AppHandle, request: &Request<Vec<u8>>) -> Result<Response<Vec<u8>>, Fail> {
    let path = request.uri().path().trim_start_matches('/');
    let path = urlencoding::decode(path).map_err(|_| (StatusCode::BAD_REQUEST, "bad path".to_string()))?;
    let (kind, arg) = path.split_once('/').ok_or((StatusCode::NOT_FOUND, "not found".to_string()))?;
    let state = app.state::<AppState>();
    match kind {
        "yt" => serve_youtube(&state, arg, parse_range(request)).await,
        "img" => serve_remote_image(&state, arg).await,
        "thumb" if valid_video_id(arg) => {
            let bytes = tokio::fs::read(state.downloads.thumbnail(arg))
                .await
                .map_err(|_| (StatusCode::NOT_FOUND, "no thumbnail".to_string()))?;
            Ok(base(StatusCode::OK).header(header::CONTENT_TYPE, "image/jpeg").body(bytes).unwrap())
        }
        _ => Err((StatusCode::NOT_FOUND, "not found".into())),
    }
}

pub fn valid_video_id(id: &str) -> bool {
    id.len() == 11 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

async fn serve_youtube(state: &AppState, id: &str, (start, req_end): (u64, Option<u64>)) -> Result<Response<Vec<u8>>, Fail> {
    if !valid_video_id(id) {
        return Err((StatusCode::BAD_REQUEST, "bad video id".into()));
    }
    // A downloaded video plays from disk, so it works offline.
    let saved = crate::db::download(&state.db.lock().unwrap(), id).ok().flatten();
    if let Some(files) = saved.as_ref().and_then(|d| state.downloads.files(d)) {
        return serve_file(&files[0], "video/mp4", (start, req_end)).await;
    }
    let bad_gateway = |e: String| (StatusCode::BAD_GATEWAY, e);
    for attempt in 1..=MAX_ATTEMPTS {
        let resolved = state.streams.get(&state.ytdlp, id).await.map_err(bad_gateway)?;
        let stream = crate::player::choose(&resolved, 360)
            .muxed
            .cloned()
            .ok_or_else(|| bad_gateway("This video has no file the desktop player can play.".into()))?;
        let age = resolved.resolved_at.elapsed();
        if age < URL_WARMUP {
            tokio::time::sleep(URL_WARMUP - age).await;
        }
        let mut end = start + YT_CHUNK - 1;
        if let Some(e) = req_end {
            end = end.min(e);
        }
        if let Some(size) = stream.size {
            if start >= size {
                return Ok(base(StatusCode::RANGE_NOT_SATISFIABLE)
                    .header(header::CONTENT_RANGE, format!("bytes */{size}"))
                    .body(Vec::new())
                    .unwrap());
            }
            end = end.min(size - 1);
        }

        let mut req = state.http.get(&stream.url);
        for (k, v) in &resolved.headers {
            req = req.header(k.as_str(), v.as_str());
        }
        let resp = req
            .header(reqwest::header::RANGE, format!("bytes={start}-{end}"))
            .send()
            .await
            .map_err(|e| bad_gateway(e.to_string()))?;
        let status = resp.status();
        if (status == 403 || status == 410) && attempt < MAX_ATTEMPTS {
            // Halfway through, assume the URL itself went bad and resolve a new one.
            if attempt == MAX_ATTEMPTS / 2 {
                state.streams.invalidate(id);
            }
            tokio::time::sleep(Duration::from_millis(500 * u64::from(attempt.min(3)))).await;
            continue;
        }
        if !status.is_success() {
            return Err(bad_gateway(format!("YouTube returned {status}")));
        }
        let total = resp
            .headers()
            .get(reqwest::header::CONTENT_RANGE)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.rsplit('/').next())
            .and_then(|v| v.parse::<u64>().ok());
        let body = resp.bytes().await.map_err(|e| bad_gateway(e.to_string()))?.to_vec();
        let builder = match total {
            Some(total) => base(StatusCode::PARTIAL_CONTENT)
                .header(header::ACCEPT_RANGES, "bytes")
                .header(header::CONTENT_RANGE, format!("bytes {start}-{}/{total}", start + body.len().max(1) as u64 - 1)),
            None => base(StatusCode::OK),
        };
        return Ok(builder
            .header(header::CONTENT_TYPE, "video/mp4")
            .header(header::CONTENT_LENGTH, body.len())
            .body(body)
            .unwrap());
    }
    Err(bad_gateway("stream URL keeps expiring".into()))
}

/// Serves a byte range of a file on disk.
async fn serve_file(path: &std::path::Path, mime: &str, (start, req_end): (u64, Option<u64>)) -> Result<Response<Vec<u8>>, Fail> {
    use tokio::io::{AsyncReadExt, AsyncSeekExt};
    let io = |_: std::io::Error| (StatusCode::NOT_FOUND, "The file is missing or can't be read.".to_string());
    let mut file = tokio::fs::File::open(path).await.map_err(io)?;
    let len = file.metadata().await.map_err(io)?.len();
    if start >= len {
        return Ok(base(StatusCode::RANGE_NOT_SATISFIABLE)
            .header(header::CONTENT_RANGE, format!("bytes */{len}"))
            .body(Vec::new())
            .unwrap());
    }
    let end = req_end.unwrap_or(u64::MAX).min(start + YT_CHUNK * 2 - 1).min(len - 1);
    file.seek(std::io::SeekFrom::Start(start)).await.map_err(io)?;
    let mut body = vec![0; (end - start + 1) as usize];
    file.read_exact(&mut body).await.map_err(io)?;
    Ok(base(StatusCode::PARTIAL_CONTENT)
        .header(header::CONTENT_TYPE, mime)
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_RANGE, format!("bytes {start}-{end}/{len}"))
        .header(header::CONTENT_LENGTH, body.len())
        .body(body)
        .unwrap())
}

/// Avatars come from Google's image CDN, which answers bursts of requests
/// from the web view with 429. Fetching them here, once, and keeping them on
/// disk avoids that and makes repeat views free.
async fn serve_remote_image(state: &AppState, url: &str) -> Result<Response<Vec<u8>>, Fail> {
    let parsed = reqwest::Url::parse(url).map_err(|_| (StatusCode::BAD_REQUEST, "bad url".to_string()))?;
    let host = parsed.host_str().unwrap_or("");
    let allowed = parsed.scheme() == "https"
        && ["googleusercontent.com", "ytimg.com", "ggpht.com"]
            .iter()
            .any(|d| host == *d || host.ends_with(&format!(".{d}")));
    if !allowed {
        return Err((StatusCode::FORBIDDEN, "host not allowed".into()));
    }
    let path = state.image_cache.path_for(url);
    let bytes = match tokio::fs::read(&path).await {
        Ok(bytes) => bytes,
        Err(_) => {
            let _slot = state.image_cache.fetch_slots.acquire().await;
            let bytes = fetch_image(&state.http, url).await?;
            let _ = tokio::fs::write(&path, &bytes).await;
            bytes
        }
    };
    Ok(base(StatusCode::OK)
        .header(header::CONTENT_TYPE, sniff_image(&bytes))
        .header(header::CACHE_CONTROL, "max-age=604800")
        .body(bytes)
        .unwrap())
}

async fn fetch_image(http: &reqwest::Client, url: &str) -> Result<Vec<u8>, Fail> {
    let fail = |e: String| (StatusCode::BAD_GATEWAY, e);
    for attempt in 0..3u64 {
        let resp = http.get(url).send().await.map_err(|e| fail(e.to_string()))?;
        if resp.status() == StatusCode::TOO_MANY_REQUESTS && attempt < 2 {
            tokio::time::sleep(Duration::from_millis(800 * (attempt + 1))).await;
            continue;
        }
        let resp = resp.error_for_status().map_err(|e| fail(e.to_string()))?;
        return Ok(resp.bytes().await.map_err(|e| fail(e.to_string()))?.to_vec());
    }
    Err(fail("image server is rate limiting".into()))
}

fn sniff_image(bytes: &[u8]) -> &'static str {
    match bytes {
        [0x89, b'P', b'N', b'G', ..] => "image/png",
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => "image/webp",
        [b'G', b'I', b'F', ..] => "image/gif",
        _ => "image/jpeg",
    }
}

/// Avatars saved under the app's cache folder, named by a hash of their URL.
pub struct ImageCache {
    dir: std::path::PathBuf,
    fetch_slots: tokio::sync::Semaphore,
}

impl ImageCache {
    /// Cache size above which the oldest images are deleted at startup.
    const MAX_BYTES: u64 = 60 * 1024 * 1024;

    pub fn new(dir: std::path::PathBuf) -> Self {
        let _ = std::fs::create_dir_all(&dir);
        Self {
            dir,
            fetch_slots: tokio::sync::Semaphore::new(6),
        }
    }

    pub fn path_for(&self, url: &str) -> std::path::PathBuf {
        use sha2::{Digest, Sha256};
        self.dir.join(format!("{:x}", Sha256::digest(url.as_bytes())))
    }

    /// Deletes the least recently written images once the cache is too big.
    pub fn trim(&self) {
        let Ok(entries) = std::fs::read_dir(&self.dir) else { return };
        let mut files: Vec<_> = entries
            .filter_map(Result::ok)
            .filter_map(|e| {
                let meta = e.metadata().ok()?;
                Some((meta.modified().ok()?, meta.len(), e.path()))
            })
            .collect();
        let mut total: u64 = files.iter().map(|f| f.1).sum();
        if total <= Self::MAX_BYTES {
            return;
        }
        files.sort_by_key(|f| f.0);
        for (_, len, path) in files {
            if total <= Self::MAX_BYTES * 3 / 4 {
                break;
            }
            if std::fs::remove_file(&path).is_ok() {
                total -= len;
            }
        }
    }
}
