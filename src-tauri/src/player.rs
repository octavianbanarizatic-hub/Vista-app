//! Picking streams for a video and handing them to the player: the native
//! video player on Android, or the page's <video> element on desktop (which
//! is only meant for testing the app, so it plays the 360p file that has
//! sound and picture together).

use serde::Serialize;

use crate::extras::Segment;
use crate::ytdlp::{Format, Resolved};

/// The streams picked for one video at one quality.
#[derive(Debug, Default)]
pub struct Choice<'a> {
    pub video: Option<&'a Format>,
    pub audio: Option<&'a Format>,
    /// Picture and sound in one file (desktop, and videos without separate streams).
    pub muxed: Option<&'a Format>,
    /// A live stream's HLS playlist.
    pub hls: Option<&'a str>,
    /// Every quality on offer, best first.
    pub heights: Vec<u32>,
}

/// Codecs the phone can play, best first for each size. Up to 1080p, H.264
/// is decoded in hardware by every phone; above it only VP9 and AV1 exist.
fn codec_rank(f: &Format, height: u32) -> u8 {
    let codec = f.vcodec.as_deref().unwrap_or("");
    let order: [&str; 3] = if height <= 1080 { ["avc1", "vp", "av01"] } else { ["vp", "av01", "avc1"] };
    order.iter().position(|c| codec.starts_with(c)).map_or(0, |i| 3 - i as u8)
}

fn plain(f: &Format) -> bool {
    f.protocol == "https"
}

pub fn choose(r: &Resolved, quality: u32) -> Choice<'_> {
    let hls = if r.live {
        r.formats
            .iter()
            .filter(|f| f.protocol.starts_with("m3u8"))
            .max_by(|a, b| a.height.cmp(&b.height))
            .and_then(|f| f.manifest_url.as_deref().or(Some(f.url.as_str())))
    } else {
        None
    };
    let videos: Vec<&Format> = r
        .formats
        .iter()
        .filter(|f| plain(f) && f.has_video() && !f.has_audio() && f.height.is_some())
        .filter(|f| codec_rank(f, f.height.unwrap_or(0)) > 0)
        .collect();
    let mut heights: Vec<u32> = videos.iter().filter_map(|f| f.height).collect();
    heights.sort_unstable_by(|a, b| b.cmp(a));
    heights.dedup();
    let target = heights.iter().copied().find(|&h| h <= quality).or(heights.last().copied());
    let video = target.and_then(|h| {
        videos
            .iter()
            .copied()
            .filter(|f| f.height == Some(h))
            .max_by(|a, b| codec_rank(a, h).cmp(&codec_rank(b, h)).then(a.tbr.total_cmp(&b.tbr)))
    });
    // The original-language track, full dynamic range, AAC before Opus.
    let audio = r
        .formats
        .iter()
        .filter(|f| plain(f) && f.has_audio() && !f.has_video() && !f.is_drc())
        .max_by(|a, b| {
            a.language_preference
                .cmp(&b.language_preference)
                .then((a.ext == "m4a").cmp(&(b.ext == "m4a")))
                .then(a.tbr.total_cmp(&b.tbr))
        });
    let muxed = r
        .formats
        .iter()
        .filter(|f| plain(f) && f.has_video() && f.has_audio())
        .max_by(|a, b| a.height.cmp(&b.height));
    Choice { video, audio, muxed, hls, heights }
}

/// What the page needs to know about the video that is now playing.
#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayInfo {
    pub qualities: Vec<u32>,
    pub quality: Option<u32>,
    pub duration: Option<f64>,
    pub chapters: Vec<crate::ytdlp::Chapter>,
    pub segments: Vec<Segment>,
    pub live: bool,
    /// Desktop: the `media://` path for the page's <video> element.
    pub src: Option<String>,
    /// Playing a download.
    pub offline: bool,
}

/// Everything the Android player needs to play a video.
#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadArgs {
    pub id: String,
    pub video_url: Option<String>,
    pub audio_url: Option<String>,
    /// Picture and sound in one file, when there are no separate streams.
    pub muxed_url: Option<String>,
    pub hls_url: Option<String>,
    pub headers: std::collections::HashMap<String, String>,
    /// Where to start, in seconds.
    pub start: f64,
    pub title: String,
    pub channel: String,
    pub artwork: String,
    pub segments: Vec<Segment>,
}

impl LoadArgs {
    pub fn new(id: &str, r: &Resolved, c: &Choice) -> Self {
        let (video, audio) = match (c.video, c.audio) {
            (Some(v), Some(a)) => (Some(v), Some(a)),
            _ => (None, None),
        };
        Self {
            id: id.to_string(),
            video_url: video.map(|f| f.url.clone()),
            audio_url: audio.map(|f| f.url.clone()),
            muxed_url: c.muxed.map(|f| f.url.clone()),
            hls_url: c.hls.map(str::to_string),
            headers: r.headers.iter().cloned().collect(),
            ..Default::default()
        }
    }

    pub fn playable(&self) -> bool {
        self.hls_url.is_some() || self.video_url.is_some() || self.muxed_url.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn f(id: &str, height: Option<u32>, vcodec: Option<&str>, acodec: Option<&str>, ext: &str, tbr: f64) -> Format {
        Format {
            id: id.into(),
            url: format!("https://cdn/{id}"),
            height,
            vcodec: vcodec.map(str::to_string),
            acodec: acodec.map(str::to_string),
            ext: ext.into(),
            tbr,
            protocol: "https".into(),
            ..Default::default()
        }
    }

    fn resolved(formats: Vec<Format>) -> Resolved {
        Resolved { formats, headers: vec![], duration: None, chapters: vec![], live: false, resolved_at: Instant::now() }
    }

    #[test]
    fn picks_streams() {
        let mut hls = f("270", Some(1080), Some("avc1"), None, "mp4", 5000.0);
        hls.protocol = "m3u8_native".into();
        let r = resolved(vec![
            f("140-drc", None, None, Some("mp4a.40.2"), "m4a", 130.0),
            f("140", None, None, Some("mp4a.40.2"), "m4a", 129.0),
            f("251", None, None, Some("opus"), "webm", 137.0),
            f("18", Some(360), Some("avc1.42001E"), Some("mp4a.40.2"), "mp4", 484.0),
            f("136", Some(720), Some("avc1.4D401F"), None, "mp4", 592.0),
            f("247", Some(720), Some("vp9"), None, "webm", 733.0),
            f("137", Some(1080), Some("avc1.640028"), None, "mp4", 1769.0),
            f("399", Some(1080), Some("av01.0.08M.08"), None, "mp4", 897.0),
            f("271", Some(1440), Some("vp9"), None, "webm", 3692.0),
            f("400", Some(1440), Some("av01.0.12M.08"), None, "mp4", 2508.0),
            hls,
        ]);
        let c = choose(&r, 1080);
        assert_eq!(c.heights, [1440, 1080, 720]);
        assert_eq!(c.video.unwrap().id, "137");
        assert_eq!(c.audio.unwrap().id, "140");
        assert_eq!(c.muxed.unwrap().id, "18");
        assert!(c.hls.is_none());
        assert_eq!(choose(&r, 4320).video.unwrap().id, "271");
        assert_eq!(choose(&r, 720).video.unwrap().id, "136");
        // Below the smallest size on offer: the smallest.
        assert_eq!(choose(&r, 144).video.unwrap().id, "136");
        let args = LoadArgs::new("abc", &r, &c);
        assert_eq!(args.video_url.as_deref(), Some("https://cdn/137"));
        assert!(args.playable());
    }

    #[test]
    fn live_streams_use_hls() {
        let mut hls = f("95", Some(720), Some("avc1"), Some("mp4a"), "mp4", 2000.0);
        hls.protocol = "m3u8_native".into();
        hls.manifest_url = Some("https://manifest/master.m3u8".into());
        let mut r = resolved(vec![hls]);
        r.live = true;
        let c = choose(&r, 1080);
        assert_eq!(c.hls, Some("https://manifest/master.m3u8"));
        assert!(c.video.is_none() && c.heights.is_empty());
        assert!(LoadArgs::new("abc", &r, &c).playable());
    }
}
