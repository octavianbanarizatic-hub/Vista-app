//! Running yt-dlp, which turns a YouTube video id into direct video and
//! audio stream URLs. How it runs depends on the platform (see `desktop` and
//! `android`); reading its output is shared.

use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::Value;

#[cfg(target_os = "android")]
mod android;
#[cfg(desktop)]
mod desktop;

#[cfg(target_os = "android")]
pub use android::YtDlp;
#[cfg(desktop)]
pub use desktop::YtDlp;

/// YouTube changes often and old yt-dlp releases stop working, so the copy
/// Vista manages is checked for updates this often.
const UPDATE_INTERVAL: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// Passed on every run, before the run's own arguments.
const BASE_ARGS: &[&str] = &["--no-warnings", "--ignore-config"];

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub installed: bool,
    pub managed: bool,
    pub version: Option<String>,
    pub path: Option<String>,
    /// The JavaScript runtime yt-dlp will use for YouTube, if any.
    pub js_runtime: Option<&'static str>,
}

/// One stream yt-dlp found for a video: video only, audio only, or both.
#[derive(Debug, Clone, Default)]
pub struct Format {
    pub id: String,
    pub url: String,
    pub height: Option<u32>,
    pub vcodec: Option<String>,
    pub acodec: Option<String>,
    pub ext: String,
    /// Average bitrate in kbit/s.
    pub tbr: f64,
    pub size: Option<u64>,
    /// "https" for plain files, "m3u8_native" for HLS (live streams).
    pub protocol: String,
    pub manifest_url: Option<String>,
    /// Dubbed and dynamic-range-compressed audio tracks rank below the original.
    pub language_preference: i64,
}

impl Format {
    pub fn has_video(&self) -> bool {
        self.vcodec.is_some()
    }

    pub fn has_audio(&self) -> bool {
        self.acodec.is_some()
    }

    /// Volume-compressed copies of the normal audio ("-drc" ids).
    pub fn is_drc(&self) -> bool {
        self.id.ends_with("-drc")
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Chapter {
    pub start: f64,
    pub title: String,
}

/// Everything one yt-dlp run says about a video.
#[derive(Debug, Clone)]
pub struct Resolved {
    pub formats: Vec<Format>,
    /// Request headers the stream URLs need (yt-dlp's user agent and friends).
    pub headers: Vec<(String, String)>,
    pub duration: Option<f64>,
    pub chapters: Vec<Chapter>,
    pub live: bool,
    pub resolved_at: Instant,
}

/// The JSON a run printed, or the most useful line of its error output.
fn parse_output(success: bool, stdout: &[u8], stderr: &str) -> Result<Value, String> {
    if !success {
        let line = stderr
            .lines()
            .rev()
            .find(|l| l.contains("ERROR"))
            .unwrap_or(stderr.trim());
        return Err(friendly_error(line.trim()));
    }
    serde_json::from_slice(stdout).map_err(|e| format!("bad yt-dlp output: {e}"))
}

/// yt-dlp's errors are written for developers; the common ones get plain words.
fn friendly_error(line: &str) -> String {
    let lower = line.to_lowercase();
    let plain = if lower.contains("sign in to confirm your age") || lower.contains("age-restricted") {
        "This video is age-restricted, and Vista doesn't sign in to YouTube."
    } else if lower.contains("private video") {
        "This video is private."
    } else if lower.contains("members-only") || lower.contains("join this channel") {
        "This video is for channel members only."
    } else if lower.contains("not available in your country") || lower.contains("blocked it in your country") {
        "This video isn't available in your country."
    } else if lower.contains("premieres in") || lower.contains("live event will begin") {
        "This video hasn't started yet."
    } else if lower.contains("video unavailable") || lower.contains("has been removed") {
        "This video is unavailable."
    } else {
        return line.to_string();
    };
    plain.to_string()
}

fn parse_format(f: &Value) -> Option<Format> {
    let codec = |key: &str| f[key].as_str().filter(|c| *c != "none").map(str::to_string);
    let protocol = f["protocol"].as_str().unwrap_or("https").to_string();
    // Storyboards (preview images) and anything else that isn't audio or video.
    if f["format_note"].as_str() == Some("storyboard") || f["ext"].as_str() == Some("mhtml") {
        return None;
    }
    Some(Format {
        id: f["format_id"].as_str()?.to_string(),
        url: f["url"].as_str()?.to_string(),
        height: f["height"].as_u64().map(|h| h as u32),
        vcodec: codec("vcodec"),
        acodec: codec("acodec"),
        ext: f["ext"].as_str().unwrap_or("").to_string(),
        tbr: f["tbr"].as_f64().unwrap_or(0.0),
        size: f["filesize"].as_u64().or(f["filesize_approx"].as_u64()),
        protocol,
        manifest_url: f["manifest_url"].as_str().map(str::to_string),
        language_preference: f["language_preference"].as_i64().unwrap_or(-1),
    })
}

fn parse_resolved(info: &Value) -> Result<Resolved, String> {
    let formats: Vec<Format> = info["formats"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(parse_format)
        .collect();
    if formats.is_empty() {
        return Err("yt-dlp found no streams for this video".into());
    }
    // Every format carries the same headers; the top-level copy is the chosen one's.
    let headers_of = |v: &Value| -> Vec<(String, String)> {
        v["http_headers"]
            .as_object()
            .map(|h| {
                h.iter()
                    .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string())))
                    .collect()
            })
            .unwrap_or_default()
    };
    let mut headers = headers_of(info);
    if headers.is_empty() {
        headers = info["formats"].get(0).map(headers_of).unwrap_or_default();
    }
    let chapters = info["chapters"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| {
            Some(Chapter {
                start: c["start_time"].as_f64()?,
                title: c["title"].as_str()?.to_string(),
            })
        })
        .collect();
    Ok(Resolved {
        formats,
        headers,
        duration: info["duration"].as_f64(),
        chapters,
        live: info["is_live"].as_bool().unwrap_or(false),
        resolved_at: Instant::now(),
    })
}

impl YtDlp {
    pub async fn resolve(&self, video_id: &str) -> Result<Resolved, String> {
        let url = format!("https://www.youtube.com/watch?v={video_id}");
        let info = self.run_json(&["-j", "--no-playlist", &url]).await?;
        parse_resolved(&info)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_output() {
        assert_eq!(parse_output(true, br#"{"a":1}"#, "").unwrap()["a"], 1);
        let err = "WARNING: slow\nERROR: [youtube] abc: Private video. Sign in if you've been granted access\n";
        assert_eq!(parse_output(false, b"", err).unwrap_err(), "This video is private.");
        assert_eq!(parse_output(false, b"", "  boom \n").unwrap_err(), "boom");
        assert!(parse_output(true, b"not json", "").is_err());
    }

    #[test]
    fn reads_formats() {
        let info = json!({
            "duration": 120.0,
            "is_live": false,
            "http_headers": { "User-Agent": "UA" },
            "chapters": [{ "start_time": 0.0, "end_time": 60.0, "title": "Intro" }],
            "formats": [
                { "format_id": "sb0", "format_note": "storyboard", "ext": "mhtml", "url": "x", "vcodec": "none", "acodec": "none" },
                { "format_id": "140", "url": "a", "ext": "m4a", "vcodec": "none", "acodec": "mp4a.40.2", "tbr": 129.0, "filesize": 10 },
                { "format_id": "137", "url": "v", "ext": "mp4", "vcodec": "avc1.640028", "acodec": "none", "height": 1080, "tbr": 1700.0 },
            ],
        });
        let r = parse_resolved(&info).unwrap();
        assert_eq!(r.formats.len(), 2);
        assert!(r.formats[0].has_audio() && !r.formats[0].has_video());
        assert_eq!(r.formats[1].height, Some(1080));
        assert_eq!(r.headers, vec![("User-Agent".to_string(), "UA".to_string())]);
        assert_eq!(r.chapters[0].title, "Intro");
        assert!(parse_resolved(&json!({ "formats": [] })).is_err());
    }
}
