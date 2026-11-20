//! Community extras: SponsorBlock (skips sponsor segments) and Return YouTube
//! Dislike (dislike counts, which YouTube hides).

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

/// Every SponsorBlock category Vista asks for; the page picks which to skip.
const CATEGORIES: &[&str] = &["sponsor", "selfpromo", "interaction", "intro", "outro", "preview", "music_offtopic", "filler"];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Segment {
    pub start: f64,
    pub end: f64,
    pub category: String,
}

/// The segments to skip in a video. Asks by the first characters of the id's
/// hash, so SponsorBlock never learns which video is being watched.
pub async fn segments(http: &reqwest::Client, video_id: &str) -> Result<Vec<Segment>, String> {
    let prefix = &format!("{:x}", Sha256::digest(video_id.as_bytes()))[..4];
    let categories = serde_json::to_string(CATEGORIES).unwrap_or_default();
    let resp = http
        .get(format!("https://sponsor.ajay.app/api/skipSegments/{prefix}"))
        .query(&[("categories", categories.as_str()), ("actionType", "skip")])
        .send()
        .await
        .map_err(|e| e.to_string())?;
    // 404 means no video with this prefix has segments.
    if resp.status() == 404 {
        return Ok(Vec::new());
    }
    let body: Value = resp.error_for_status().map_err(|e| e.to_string())?.json().await.map_err(|e| e.to_string())?;
    Ok(parse_segments(&body, video_id))
}

fn parse_segments(body: &Value, video_id: &str) -> Vec<Segment> {
    let mut out: Vec<Segment> = body
        .as_array()
        .into_iter()
        .flatten()
        .filter(|v| v["videoID"].as_str() == Some(video_id))
        .flat_map(|v| v["segments"].as_array().cloned().unwrap_or_default())
        .filter(|s| s["actionType"].as_str().unwrap_or("skip") == "skip")
        .filter_map(|s| {
            let range = s["segment"].as_array()?;
            let (start, end) = (range.first()?.as_f64()?, range.get(1)?.as_f64()?);
            (end > start).then(|| Segment {
                start,
                end,
                category: s["category"].as_str().unwrap_or("sponsor").to_string(),
            })
        })
        .collect();
    out.sort_by(|a, b| a.start.total_cmp(&b.start));
    out
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Votes {
    pub likes: u64,
    pub dislikes: u64,
}

pub async fn votes(http: &reqwest::Client, video_id: &str) -> Result<Votes, String> {
    let body: Value = http
        .get("https://returnyoutubedislikeapi.com/votes")
        .query(&[("videoId", video_id)])
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())?;
    Ok(Votes {
        likes: body["likes"].as_u64().unwrap_or(0),
        dislikes: body["dislikes"].as_u64().unwrap_or(0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_segments_for_this_video_only() {
        let body = json!([
            { "videoID": "other", "segments": [{ "segment": [1.0, 2.0], "category": "sponsor", "actionType": "skip" }] },
            { "videoID": "abc", "segments": [
                { "segment": [90.5, 120.0], "category": "selfpromo", "actionType": "skip" },
                { "segment": [10.0, 30.0], "category": "sponsor", "actionType": "skip" },
                { "segment": [40.0, 40.0], "category": "sponsor", "actionType": "skip" },
                { "segment": [50.0, 60.0], "category": "sponsor", "actionType": "mute" },
            ]},
        ]);
        let s = parse_segments(&body, "abc");
        assert_eq!(s.len(), 2);
        assert_eq!(s[0], Segment { start: 10.0, end: 30.0, category: "sponsor".into() });
        assert_eq!(s[1].category, "selfpromo");
    }
}
