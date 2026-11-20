//! Search, video pages and channel pages through YouTube's web API (the same
//! one youtube.com's pages use). Only page data comes from here; streams
//! always come from yt-dlp.
//!
//! YouTube describes videos in two shapes: the older `videoRenderer` (search)
//! and the newer `lockupViewModel` (related videos, channel pages). Both
//! become a [`Video`].

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{Channel, Video};

pub const BROWSER_UA: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36";
const CLIENT_VERSION: &str = "2.20251020.00.00";
/// The channel page's "Videos" tab.
const VIDEOS_TAB: &str = "EgZ2aWRlb3PyBgQKAjoA";

/// A list with a token for the next part, if there is more.
#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Page<T> {
    pub items: Vec<T>,
    pub next: Option<String>,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResults {
    pub channels: Vec<Channel>,
    pub playlists: Vec<Playlist>,
    pub videos: Vec<Video>,
    pub next: Option<String>,
}

/// A YouTube playlist, as lists and the Library show it.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Playlist {
    /// "PL…"
    pub id: String,
    pub title: String,
    pub channel: String,
    /// "29 videos"
    pub count: Option<String>,
    pub thumbnail: Option<String>,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaylistPage {
    pub playlist: Playlist,
    pub videos: Page<Video>,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Comment {
    pub id: String,
    pub author: String,
    pub author_id: Option<String>,
    pub avatar: Option<String>,
    pub text: String,
    /// "2 days ago"
    pub published: String,
    /// "569K", or missing when nobody liked it.
    pub likes: Option<String>,
    pub reply_count: Option<String>,
    /// The token for this comment's replies.
    pub replies: Option<String>,
    /// Written by the video's own channel.
    pub creator: bool,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Comments {
    /// "2,460,823 Comments", on the first part only.
    pub count: Option<String>,
    pub items: Vec<Comment>,
    pub next: Option<String>,
}

/// What a YouTube link points at.
#[derive(Debug, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Link {
    /// "video", "channel" or "playlist"
    pub kind: &'static str,
    pub id: String,
    /// Where a video link starts, in seconds (`t=1m30s`).
    pub start: Option<f64>,
}

/// What a video's page shows besides the video itself.
#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WatchInfo {
    pub id: String,
    pub title: String,
    /// "1,831,012,849 views"
    pub views: Option<String>,
    /// "Oct 25, 2009", "Premiered Mar 3, 2024", "Started streaming 2 hours ago"
    pub date: Option<String>,
    pub likes: Option<String>,
    pub description: String,
    pub channel: Channel,
    pub related: Page<Video>,
    /// The token for the first part of the comments; missing when they're off.
    pub comments: Option<String>,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelPage {
    pub channel: Channel,
    pub description: String,
    pub videos: Page<Video>,
}

async fn call(http: &reqwest::Client, endpoint: &str, body: Value) -> Result<Value, String> {
    let mut body = body;
    body["context"] = json!({ "client": {
        "clientName": "WEB",
        "clientVersion": CLIENT_VERSION,
        "hl": "en",
        "gl": "US",
    }});
    http.post(format!("https://www.youtube.com/youtubei/v1/{endpoint}?prettyPrint=false"))
        .header("Origin", "https://www.youtube.com")
        .header("Referer", "https://www.youtube.com/")
        .header("X-YouTube-Client-Name", "1")
        .header("X-YouTube-Client-Version", CLIENT_VERSION)
        .json(&body)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| {
            if e.is_connect() || e.is_timeout() || e.is_request() {
                "Can't reach YouTube. Check your internet connection.".to_string()
            } else {
                format!("YouTube didn't answer: {e}")
            }
        })?
        .json()
        .await
        .map_err(|e| e.to_string())
}

pub async fn search(http: &reqwest::Client, query: &str, next: Option<&str>) -> Result<SearchResults, String> {
    let body = match next {
        Some(token) => json!({ "continuation": token }),
        None => json!({ "query": query }),
    };
    let resp = call(http, "search", body).await?;
    let mut channels = Vec::new();
    let mut playlists = Vec::new();
    let mut found = Vec::new();
    collect(&resp, &["channelRenderer", "lockupViewModel"], &mut found);
    for (key, r) in found {
        if key == "channelRenderer" {
            channels.extend(parse_channel_renderer(r));
        } else {
            playlists.extend(parse_playlist_lockup(r));
        }
    }
    Ok(SearchResults {
        channels,
        playlists,
        videos: videos(&resp),
        next: continuation(&resp),
    })
}

/// What YouTube's search box suggests for what's typed so far.
pub async fn suggestions(http: &reqwest::Client, query: &str) -> Result<Vec<String>, String> {
    let body = http
        .get("https://suggestqueries-clients6.youtube.com/complete/search")
        .query(&[("client", "youtube"), ("ds", "yt"), ("hl", "en"), ("gl", "US"), ("q", query)])
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| e.to_string())?
        .text()
        .await
        .map_err(|e| e.to_string())?;
    Ok(parse_suggestions(&body))
}

/// The answer is a script, `window.google.ac.h(["query", [["suggestion", …], …], …])`.
fn parse_suggestions(body: &str) -> Vec<String> {
    let json = body
        .find('(')
        .zip(body.rfind(')'))
        .and_then(|(start, end)| body.get(start + 1..end))
        .and_then(|s| serde_json::from_str::<Value>(s).ok());
    json.as_ref()
        .and_then(|v| v[1].as_array())
        .map(|list| list.iter().filter_map(|s| s[0].as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}

/// A playlist in search results. Mixes (endless lists made for each viewer)
/// have no page of their own, so they're left out.
fn parse_playlist_lockup(l: &Value) -> Option<Playlist> {
    if l["contentType"].as_str() != Some("LOCKUP_CONTENT_TYPE_PLAYLIST") {
        return None;
    }
    let id = l["contentId"].as_str().filter(|id| !id.starts_with("RD"))?;
    let meta = &l["metadata"]["lockupMetadataViewModel"];
    let mut badges = Vec::new();
    collect(&l["contentImage"], &["thumbnailBadgeViewModel"], &mut badges);
    let first_row = &meta["metadata"]["contentMetadataViewModel"]["metadataRows"][0]["metadataParts"][0];
    Some(Playlist {
        id: id.to_string(),
        title: meta["title"]["content"].as_str()?.to_string(),
        channel: text(&first_row["text"]),
        count: badges.iter().find_map(|(_, b)| b["text"].as_str()).map(str::to_string),
        thumbnail: find(&l["contentImage"], "sources")
            .and_then(Value::as_array)
            .and_then(|s| s.last())
            .and_then(|s| s["url"].as_str())
            .map(str::to_string),
    })
}

pub async fn playlist(http: &reqwest::Client, list_id: &str) -> Result<PlaylistPage, String> {
    let resp = call(http, "browse", json!({ "browseId": format!("VL{list_id}") })).await?;
    let header = find(&resp["header"], "pageHeaderViewModel").unwrap_or(&Value::Null);
    let mut title = header["title"]["dynamicTextViewModel"]["text"]["content"]
        .as_str()
        .unwrap_or("")
        .to_string();
    if title.is_empty() {
        title = resp["metadata"]["playlistMetadataRenderer"]["title"].as_str().unwrap_or("").to_string();
    }
    if title.is_empty() {
        return Err("This playlist is private or doesn't exist.".into());
    }
    let mut parts = Vec::new();
    collect_texts(&header["metadata"], &mut parts);
    let items = videos(&resp);
    Ok(PlaylistPage {
        playlist: Playlist {
            id: list_id.to_string(),
            title,
            channel: parts
                .iter()
                .find_map(|p| p.strip_prefix("by "))
                .unwrap_or_default()
                .to_string(),
            count: parts.iter().find(|p| p.ends_with("videos") || p.ends_with("video")).cloned(),
            thumbnail: items.first().map(|v| v.thumbnail.clone()),
        },
        videos: Page {
            items,
            next: continuation(&resp),
        },
    })
}

/// Comments (or one comment's replies) after a token from [`watch`] or an
/// earlier part. The comment text lives apart from the list, in
/// `frameworkUpdates`, keyed by each comment's `commentKey`.
pub async fn comments(http: &reqwest::Client, token: &str) -> Result<Comments, String> {
    let resp = call(http, "next", json!({ "continuation": token })).await?;
    Ok(parse_comments(&resp))
}

fn parse_comments(resp: &Value) -> Comments {
    let payloads: std::collections::HashMap<&str, &Value> = resp["frameworkUpdates"]["entityBatchUpdate"]["mutations"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| Some((m["entityKey"].as_str()?, m["payload"].get("commentEntityPayload")?)))
        .collect();
    let mut found = Vec::new();
    collect(resp, &["commentThreadRenderer", "commentViewModel"], &mut found);
    let items = found
        .into_iter()
        .filter_map(|(key, r)| {
            let (vm, replies) = if key == "commentThreadRenderer" {
                (&r["commentViewModel"]["commentViewModel"], token(&r["replies"]))
            } else {
                // Replies come without a thread around them.
                (if r["commentKey"].is_string() { r } else { &r["commentViewModel"] }, None)
            };
            let p = payloads.get(vm["commentKey"].as_str()?)?;
            let props = &p["properties"];
            let toolbar = &p["toolbar"];
            let count = |key: &str| toolbar[key].as_str().filter(|s| !s.is_empty() && *s != "0").map(str::to_string);
            Some(Comment {
                id: props["commentId"].as_str().unwrap_or("").to_string(),
                author: p["author"]["displayName"].as_str().unwrap_or("").to_string(),
                author_id: p["author"]["channelId"].as_str().map(str::to_string),
                avatar: p["author"]["avatarThumbnailUrl"].as_str().map(bigger_avatar),
                text: props["content"]["content"].as_str().unwrap_or("").to_string(),
                published: props["publishedTime"].as_str().unwrap_or("").to_string(),
                likes: count("likeCountNotliked"),
                reply_count: count("replyCount"),
                replies: replies.map(str::to_string),
                creator: p["author"]["isCreator"].as_bool().unwrap_or(false),
            })
        })
        .collect();
    let mut header = Vec::new();
    collect(resp, &["commentsHeaderRenderer"], &mut header);
    // Only the list's own "more" item; each thread has one for its replies too.
    let next = resp["onResponseReceivedEndpoints"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|e| e.as_object().into_iter().flat_map(|o| o.values()))
        .flat_map(|action| action["continuationItems"].as_array().cloned().unwrap_or_default())
        .filter_map(|item| {
            token(item.get("continuationItemRenderer")?).map(str::to_string)
        })
        .next_back();
    Comments {
        count: header.first().map(|(_, h)| text(&h["countText"])).filter(|s| !s.is_empty()),
        items,
        next,
    }
}

/// Reads a YouTube link: a video (youtu.be/…, watch?v=…, shorts/…), a
/// playlist (list=…) or a channel (/channel/UC…, or /@handle, which YouTube
/// is asked to look up).
pub async fn resolve_link(http: &reqwest::Client, text: &str) -> Result<Link, String> {
    let url = find_url(text).ok_or("That doesn't look like a YouTube link.")?;
    if let Some(link) = parse_link(&url) {
        return Ok(link);
    }
    let resp = call(http, "navigation/resolve_url", json!({ "url": url.as_str() })).await?;
    match resp["endpoint"]["browseEndpoint"]["browseId"].as_str() {
        Some(id) if id.starts_with("UC") => Ok(Link { kind: "channel", id: id.to_string(), start: None }),
        _ => Err("Vista can't open this kind of YouTube link.".into()),
    }
}

/// The first YouTube URL in `text` (shared text often has words around it).
fn find_url(text: &str) -> Option<reqwest::Url> {
    text.split_whitespace().find_map(|word| {
        let word = word.trim_matches(|c: char| matches!(c, '"' | '\'' | '<' | '>' | '(' | ')'));
        let word = if word.starts_with("http") { word.to_string() } else { format!("https://{word}") };
        let url = reqwest::Url::parse(&word).ok()?;
        let host = url.host_str()?.trim_start_matches("www.").trim_start_matches("m.").trim_start_matches("music.");
        matches!(host, "youtube.com" | "youtu.be" | "youtube-nocookie.com").then_some(url)
    })
}

/// "90", "1m30s" or "1h2m3s" into seconds.
fn parse_start(t: &str) -> Option<f64> {
    let t = t.trim_end_matches('s');
    if let Ok(n) = t.parse::<f64>() {
        return Some(n);
    }
    let mut total = 0.0;
    let mut num = String::new();
    for c in t.chars().chain(std::iter::once('s')) {
        match c {
            '0'..='9' => num.push(c),
            'h' | 'm' | 's' => {
                let n: f64 = num.parse().ok()?;
                total += n * match c { 'h' => 3600.0, 'm' => 60.0, _ => 1.0 };
                num.clear();
            }
            _ => return None,
        }
    }
    Some(total)
}

/// Links that can be read without asking YouTube.
fn parse_link(url: &reqwest::Url) -> Option<Link> {
    let query = |key: &str| url.query_pairs().find(|(k, _)| k == key).map(|(_, v)| v.into_owned());
    let start = query("t").or_else(|| query("start")).and_then(|t| parse_start(&t));
    let segments: Vec<&str> = url.path_segments().map(|s| s.filter(|p| !p.is_empty()).collect()).unwrap_or_default();
    let video = |id: &str| crate::media::valid_video_id(id).then(|| Link { kind: "video", id: id.to_string(), start });
    if url.host_str() == Some("youtu.be") {
        return segments.first().and_then(|id| video(id));
    }
    match segments.as_slice() {
        ["watch"] => query("v").and_then(|id| video(&id)),
        ["shorts" | "live" | "embed" | "v", id, ..] => video(id),
        ["playlist"] => query("list").map(|id| Link { kind: "playlist", id, start: None }),
        ["channel", id, ..] if id.starts_with("UC") => Some(Link { kind: "channel", id: id.to_string(), start: None }),
        _ => None,
    }
}

pub async fn watch(http: &reqwest::Client, video_id: &str) -> Result<WatchInfo, String> {
    let resp = call(http, "next", json!({ "videoId": video_id })).await?;
    let results = &resp["contents"]["twoColumnWatchNextResults"];
    let primary = find(results, "videoPrimaryInfoRenderer").unwrap_or(&Value::Null);
    let secondary = find(results, "videoSecondaryInfoRenderer").unwrap_or(&Value::Null);
    let owner = &secondary["owner"]["videoOwnerRenderer"];
    let title = text(&primary["title"]);
    if title.is_empty() {
        return Err("This video is unavailable.".into());
    }
    let channel = Channel {
        id: owner["navigationEndpoint"]["browseEndpoint"]["browseId"]
            .as_str()
            .unwrap_or("")
            .to_string(),
        name: text(&owner["title"]),
        avatar: last_thumbnail(&owner["thumbnail"]).map(|u| bigger_avatar(&u)),
        subscribers: non_empty(text(&owner["subscriberCountText"])),
        handle: None,
    };
    // The like count sits deep in the like button: its label is the count.
    let likes = find(primary, "likeButtonViewModel")
        .and_then(|b| find(b, "buttonViewModel"))
        .and_then(|b| b["title"].as_str())
        .map(str::to_string);
    let secondary_results = &results["secondaryResults"];
    let mut related = videos(secondary_results);
    related.retain(|v| v.id != video_id);
    Ok(WatchInfo {
        id: video_id.to_string(),
        title,
        views: non_empty(text(&primary["viewCount"]["videoViewCountRenderer"]["viewCount"])),
        date: non_empty(text(&primary["dateText"])),
        likes,
        description: secondary["attributedDescription"]["content"]
            .as_str()
            .unwrap_or("")
            .to_string(),
        channel,
        related: Page {
            items: related,
            next: continuation(secondary_results),
        },
        comments: continuation(&results["results"]),
    })
}

/// More related videos (or channel videos) after a [`Page`]'s token.
pub async fn more(http: &reqwest::Client, endpoint: &str, token: &str) -> Result<Page<Video>, String> {
    let resp = call(http, endpoint, json!({ "continuation": token })).await?;
    Ok(Page {
        items: videos(&resp),
        next: continuation(&resp),
    })
}

pub async fn channel(http: &reqwest::Client, channel_id: &str) -> Result<ChannelPage, String> {
    let resp = call(http, "browse", json!({ "browseId": channel_id, "params": VIDEOS_TAB })).await?;
    let header = find(&resp["header"], "pageHeaderViewModel").unwrap_or(&Value::Null);
    let meta = &resp["metadata"]["channelMetadataRenderer"];
    let mut name = header["title"]["dynamicTextViewModel"]["text"]["content"]
        .as_str()
        .unwrap_or("")
        .to_string();
    if name.is_empty() {
        name = meta["title"].as_str().unwrap_or("").to_string();
    }
    if name.is_empty() {
        return Err("This channel doesn't exist or was removed.".into());
    }
    let mut parts = Vec::new();
    collect_texts(&header["metadata"], &mut parts);
    let avatar = find(&header["image"], "sources")
        .and_then(Value::as_array)
        .and_then(|s| s.last())
        .and_then(|s| s["url"].as_str())
        .or_else(|| meta["avatar"]["thumbnails"][0]["url"].as_str())
        .map(|u| bigger_avatar(&absolute(u)));
    let channel = Channel {
        id: channel_id.to_string(),
        name,
        avatar,
        handle: parts.iter().find(|p| p.starts_with('@')).cloned(),
        subscribers: parts.iter().find(|p| p.contains("subscriber")).cloned(),
    };
    let mut items = videos(&resp);
    for v in &mut items {
        fill_channel(v, &channel);
    }
    Ok(ChannelPage {
        description: meta["description"].as_str().unwrap_or("").to_string(),
        videos: Page {
            items,
            next: continuation(&resp),
        },
        channel,
    })
}

/// Channel pages leave the channel out of each video; it's the page's own.
pub fn fill_channel(v: &mut Video, channel: &Channel) {
    if v.channel_id.is_none() {
        v.channel_id = Some(channel.id.clone());
        v.channel = channel.name.clone();
        v.avatar = channel.avatar.clone();
    }
}

/// Gathers every object stored under one of `keys`, anywhere in `v`, without
/// looking inside the ones it found.
fn collect<'a>(v: &'a Value, keys: &[&'a str], out: &mut Vec<(&'a str, &'a Value)>) {
    match v {
        Value::Object(map) => {
            for (k, child) in map {
                if let Some(key) = keys.iter().find(|key| **key == k) {
                    out.push((key, child));
                } else {
                    collect(child, keys, out);
                }
            }
        }
        Value::Array(arr) => arr.iter().for_each(|c| collect(c, keys, out)),
        _ => {}
    }
}

/// The first object stored under `key`, anywhere in `v`.
fn find<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    match v {
        Value::Object(map) => map.get(key).or_else(|| map.values().find_map(|c| find(c, key))),
        Value::Array(arr) => arr.iter().find_map(|c| find(c, key)),
        _ => None,
    }
}

/// Every `content` string under `v`, in order.
fn collect_texts(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::Object(map) => {
            if let Some(s) = map.get("content").and_then(Value::as_str) {
                out.push(s.to_string());
            }
            map.iter().filter(|(k, _)| *k != "content").for_each(|(_, c)| collect_texts(c, out));
        }
        Value::Array(arr) => arr.iter().for_each(|c| collect_texts(c, out)),
        _ => {}
    }
}

/// Every video in a response, in order, without repeats. End screens and
/// Shorts shelves are left out.
fn videos(v: &Value) -> Vec<Video> {
    let mut found = Vec::new();
    collect(
        v,
        &["videoRenderer", "compactVideoRenderer", "gridVideoRenderer", "lockupViewModel", "endScreenVideoRenderer", "reelShelfRenderer"],
        &mut found,
    );
    let mut seen = std::collections::HashSet::new();
    found
        .into_iter()
        .filter_map(|(key, r)| match key {
            "lockupViewModel" => parse_lockup(r),
            "endScreenVideoRenderer" | "reelShelfRenderer" => None,
            _ => parse_video_renderer(r),
        })
        .filter(|v| seen.insert(v.id.clone()))
        .collect()
}

/// The token for the next part of a list. Comments have their own token on
/// video pages, but they live outside the part callers pass in.
fn continuation(v: &Value) -> Option<String> {
    let mut found = Vec::new();
    collect(v, &["continuationItemRenderer", "continuationItemViewModel"], &mut found);
    found
        .into_iter()
        .find_map(|(_, r)| token(r))
        .map(str::to_string)
}

/// A continuation token. Newer lists wrap the command in another one.
fn token(v: &Value) -> Option<&str> {
    let c = find(v, "continuationCommand")?;
    c["token"].as_str().or_else(|| token(c))
}

/// `simpleText`, `runs` or `content`: the three ways YouTube writes text.
fn text(v: &Value) -> String {
    if let Some(s) = v["simpleText"].as_str().or(v["content"].as_str()) {
        return s.to_string();
    }
    v["runs"]
        .as_array()
        .map(|runs| runs.iter().filter_map(|r| r["text"].as_str()).collect())
        .unwrap_or_default()
}

fn non_empty(s: String) -> Option<String> {
    (!s.trim().is_empty()).then_some(s)
}

/// Some image URLs leave out the scheme ("//yt3.ggpht.com/…").
fn absolute(url: &str) -> String {
    match url.strip_prefix("//") {
        Some(rest) => format!("https://{rest}"),
        None => url.to_string(),
    }
}

fn last_thumbnail(v: &Value) -> Option<String> {
    v["thumbnails"].as_array()?.last()?["url"].as_str().map(absolute)
}

/// Avatars come at 48–88 px; ask for 176 px so they stay sharp on phones.
pub fn bigger_avatar(url: &str) -> String {
    if let Some(i) = url.find("=s") {
        let rest = &url[i + 2..];
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        if digits > 0 {
            return format!("{}=s176{}", &url[..i], &rest[digits..]);
        }
    }
    url.to_string()
}

/// A 16:9 thumbnail that exists for every video (black bars are cropped by the page).
pub fn thumbnail(id: &str) -> String {
    format!("https://i.ytimg.com/vi/{id}/hqdefault.jpg")
}

/// Parses "3:45" or "1:02:03" into seconds.
pub fn parse_duration(s: &str) -> Option<f64> {
    let parts: Vec<&str> = s.trim().split(':').collect();
    if !(2..=3).contains(&parts.len()) {
        return None;
    }
    parts.iter().try_fold(0.0, |acc, p| {
        let n: u32 = p.parse().ok()?;
        Some(acc * 60.0 + n as f64)
    })
}

fn browse_id(v: &Value) -> Option<String> {
    find(v, "browseEndpoint")?["browseId"]
        .as_str()
        .filter(|id| id.starts_with("UC"))
        .map(str::to_string)
}

fn parse_video_renderer(r: &Value) -> Option<Video> {
    let id = r["videoId"].as_str()?;
    let owner = if r["ownerText"].is_object() { &r["ownerText"] } else { &r["longBylineText"] };
    let views = text(&r["shortViewCountText"]);
    let views = if views.is_empty() { text(&r["viewCountText"]) } else { views };
    let live = views.contains("watching")
        || r["badges"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|b| b["metadataBadgeRenderer"]["label"].as_str().is_some_and(|l| l.starts_with("LIVE")));
    let avatar = last_thumbnail(&r["channelThumbnailSupportedRenderers"]["channelThumbnailWithLinkRenderer"]["thumbnail"])
        .or_else(|| last_thumbnail(&r["channelThumbnail"]));
    Some(Video {
        id: id.to_string(),
        title: text(&r["title"]),
        channel: text(owner),
        channel_id: browse_id(owner),
        duration: parse_duration(&text(&r["lengthText"])),
        views: non_empty(views),
        published: non_empty(text(&r["publishedTimeText"])),
        published_at: None,
        thumbnail: thumbnail(id),
        avatar: avatar.map(|u| bigger_avatar(&u)),
        live,
    })
}

fn parse_lockup(l: &Value) -> Option<Video> {
    if l["contentType"].as_str() != Some("LOCKUP_CONTENT_TYPE_VIDEO") {
        return None;
    }
    let id = l["contentId"].as_str()?;
    let meta = &l["metadata"]["lockupMetadataViewModel"];
    let rows: Vec<Vec<String>> = meta["metadata"]["contentMetadataViewModel"]["metadataRows"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|row| {
            row["metadataParts"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|p| p["text"]["content"].as_str().map(str::to_string))
                .collect()
        })
        .filter(|parts: &Vec<String>| !parts.is_empty())
        .collect();
    let avatar_box = &meta["image"]["decoratedAvatarViewModel"];
    // With an avatar (related videos) the first row is the channel; on a
    // channel's own page there is no avatar and only the stats row.
    let (channel, stats) = if avatar_box.is_object() {
        (rows.first().and_then(|r| r.first()).cloned().unwrap_or_default(), rows.get(1))
    } else {
        (String::new(), rows.first())
    };
    let mut badges = Vec::new();
    collect(&l["contentImage"], &["thumbnailBadgeViewModel"], &mut badges);
    let badges: Vec<&str> = badges.iter().filter_map(|(_, b)| b["text"].as_str()).collect();
    let live = badges.iter().any(|b| b.eq_ignore_ascii_case("live"));
    let mut views = stats.and_then(|s| s.first()).cloned();
    // Related videos say "6.6K" with a play icon instead of "6.6K views".
    if let Some(v) = &mut views {
        if v.bytes().next().is_some_and(|b| b.is_ascii_digit()) && !v.contains(' ') {
            v.push_str(" views");
        }
    }
    Some(Video {
        id: id.to_string(),
        title: meta["title"]["content"].as_str()?.to_string(),
        channel,
        channel_id: browse_id(avatar_box),
        duration: badges.iter().find_map(|b| parse_duration(b)),
        views,
        published: stats.and_then(|s| s.get(1)).cloned(),
        published_at: None,
        thumbnail: thumbnail(id),
        avatar: find(avatar_box, "sources")
            .and_then(Value::as_array)
            .and_then(|s| s.last())
            .and_then(|s| s["url"].as_str())
            .map(bigger_avatar),
        live,
    })
}

fn parse_channel_renderer(r: &Value) -> Option<Channel> {
    let id = r["channelId"].as_str()?;
    // Newer search results put the @handle where the subscriber count used to be.
    let a = non_empty(text(&r["subscriberCountText"]));
    let b = non_empty(text(&r["videoCountText"]));
    let (handle, subscribers) = match a {
        Some(h) if h.starts_with('@') => (Some(h), b),
        other => (None, other),
    };
    Some(Channel {
        id: id.to_string(),
        name: text(&r["title"]),
        avatar: last_thumbnail(&r["thumbnail"]).map(|u| bigger_avatar(&u)),
        handle,
        subscribers,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_durations_and_avatars() {
        assert_eq!(parse_duration("3:45"), Some(225.0));
        assert_eq!(parse_duration("1:02:03"), Some(3723.0));
        assert_eq!(parse_duration("LIVE"), None);
        assert_eq!(
            bigger_avatar("https://yt3.ggpht.com/abc=s68-c-k-c0x00ffffff-no-rj"),
            "https://yt3.ggpht.com/abc=s176-c-k-c0x00ffffff-no-rj"
        );
        assert_eq!(absolute("//yt3.ggpht.com/a"), "https://yt3.ggpht.com/a");
    }

    #[test]
    fn reads_search_results() {
        let resp = json!({ "contents": [
            { "channelRenderer": {
                "channelId": "UCabc", "title": { "simpleText": "Science" },
                "thumbnail": { "thumbnails": [{ "url": "//yt3.ggpht.com/x=s88-c" }] },
                "subscriberCountText": { "simpleText": "@science" },
                "videoCountText": { "simpleText": "21.4M subscribers" },
            }},
            { "videoRenderer": {
                "videoId": "eyZEqSPeMwg", "title": { "runs": [{ "text": "This Clock Measures Gravity" }] },
                "ownerText": { "runs": [{ "text": "Veritasium", "navigationEndpoint": { "browseEndpoint": { "browseId": "UCHnyfMqiRRG1u-2MsSQLbXA" } } }] },
                "lengthText": { "simpleText": "34:28" },
                "shortViewCountText": { "simpleText": "5.8M views" },
                "publishedTimeText": { "simpleText": "3 days ago" },
            }},
            { "reelShelfRenderer": { "items": [{ "videoRenderer": { "videoId": "shortshort1" } }] } },
            { "continuationItemRenderer": { "continuationEndpoint": { "continuationCommand": { "token": "T1" } } } },
        ]});
        let videos = videos(&resp);
        assert_eq!(videos.len(), 1);
        let v = &videos[0];
        assert_eq!((v.title.as_str(), v.channel.as_str()), ("This Clock Measures Gravity", "Veritasium"));
        assert_eq!(v.channel_id.as_deref(), Some("UCHnyfMqiRRG1u-2MsSQLbXA"));
        assert_eq!(v.duration, Some(2068.0));
        assert_eq!(v.views.as_deref(), Some("5.8M views"));
        assert_eq!(continuation(&resp).as_deref(), Some("T1"));
        let c = parse_channel_renderer(&resp["contents"][0]["channelRenderer"]).unwrap();
        assert_eq!(c.handle.as_deref(), Some("@science"));
        assert_eq!(c.subscribers.as_deref(), Some("21.4M subscribers"));
        assert_eq!(c.avatar.as_deref(), Some("https://yt3.ggpht.com/x=s176-c"));
    }

    #[test]
    fn reads_lockups() {
        let related = json!({
            "contentId": "soi6dKw8-oI", "contentType": "LOCKUP_CONTENT_TYPE_VIDEO",
            "contentImage": { "thumbnailViewModel": { "overlays": [{ "thumbnailBottomOverlayViewModel": {
                "badges": [{ "thumbnailBadgeViewModel": { "text": "1:02:03" } }] } }] } },
            "metadata": { "lockupMetadataViewModel": {
                "title": { "content": "80s Grooves" },
                "image": { "decoratedAvatarViewModel": {
                    "avatar": { "avatarViewModel": { "image": { "sources": [{ "url": "https://yt3.ggpht.com/a=s68-c" }] } } },
                    "rendererContext": { "commandContext": { "onTap": { "innertubeCommand": { "browseEndpoint": { "browseId": "UCEy1g" } } } } },
                }},
                "metadata": { "contentMetadataViewModel": { "metadataRows": [
                    { "metadataParts": [{ "text": { "content": "THE KING OF DISCO" } }] },
                    { "metadataParts": [{ "text": { "content": "6.6K" } }, { "text": { "content": "4d ago" } }] },
                ]}},
            }},
        });
        let v = parse_lockup(&related).unwrap();
        assert_eq!(v.channel, "THE KING OF DISCO");
        assert_eq!(v.channel_id.as_deref(), Some("UCEy1g"));
        assert_eq!(v.views.as_deref(), Some("6.6K views"));
        assert_eq!(v.published.as_deref(), Some("4d ago"));
        assert_eq!(v.duration, Some(3723.0));
        assert_eq!(v.avatar.as_deref(), Some("https://yt3.ggpht.com/a=s176-c"));

        let own = json!({
            "contentId": "eyZEqSPeMwg", "contentType": "LOCKUP_CONTENT_TYPE_VIDEO",
            "metadata": { "lockupMetadataViewModel": {
                "title": { "content": "No device could prove Einstein wrong" },
                "metadata": { "contentMetadataViewModel": { "metadataRows": [
                    { "metadataParts": [{ "text": { "content": "5.8M views" } }, { "text": { "content": "3 days ago" } }] },
                ]}},
            }},
        });
        let mut v = parse_lockup(&own).unwrap();
        assert_eq!((v.channel.as_str(), v.views.as_deref()), ("", Some("5.8M views")));
        let channel = Channel { id: "UCx".into(), name: "Veritasium".into(), ..Default::default() };
        fill_channel(&mut v, &channel);
        assert_eq!(v.channel, "Veritasium");
        assert!(parse_lockup(&json!({ "contentId": "x", "contentType": "LOCKUP_CONTENT_TYPE_PLAYLIST" })).is_none());
    }

    #[test]
    fn reads_links() {
        let link = |s: &str| find_url(s).and_then(|u| parse_link(&u));
        let video = |id: &str, start: Option<f64>| Some(Link { kind: "video", id: id.into(), start });
        assert_eq!(link("https://youtu.be/dQw4w9WgXcQ?t=42"), video("dQw4w9WgXcQ", Some(42.0)));
        assert_eq!(link("Watch this! https://www.youtube.com/watch?v=dQw4w9WgXcQ&t=1m30s"), video("dQw4w9WgXcQ", Some(90.0)));
        assert_eq!(link("https://m.youtube.com/shorts/dQw4w9WgXcQ"), video("dQw4w9WgXcQ", None));
        assert_eq!(link("youtube.com/live/dQw4w9WgXcQ"), video("dQw4w9WgXcQ", None));
        assert_eq!(link("https://music.youtube.com/watch?v=dQw4w9WgXcQ&list=RDAMVM"), video("dQw4w9WgXcQ", None));
        assert_eq!(
            link("https://www.youtube.com/playlist?list=PLFgquLnL59alCl_2TQvOiD5Vgm1hCaGSI"),
            Some(Link { kind: "playlist", id: "PLFgquLnL59alCl_2TQvOiD5Vgm1hCaGSI".into(), start: None })
        );
        assert_eq!(link("https://www.youtube.com/channel/UCHnyfMqiRRG1u-2MsSQLbXA/videos").map(|l| l.kind), Some("channel"));
        // Handles need YouTube to look them up; other sites are no link at all.
        assert!(find_url("https://www.youtube.com/@veritasium").is_some() && link("https://www.youtube.com/@veritasium").is_none());
        assert!(find_url("https://example.com/watch?v=dQw4w9WgXcQ").is_none());
        assert_eq!(parse_start("1h2m3s"), Some(3723.0));
        assert_eq!(parse_start("bogus"), None);
    }

    #[test]
    fn reads_suggestions() {
        let body = r#"window.google.ac.h(["anime exp",[["anime expeditions",0,[3]],["anime expo",0,[512]]],{"k":1}])"#;
        assert_eq!(parse_suggestions(body), ["anime expeditions", "anime expo"]);
        assert!(parse_suggestions("<html>Error 400</html>").is_empty());
    }

    #[test]
    fn reads_comments_and_playlists() {
        let resp = json!({
            "onResponseReceivedEndpoints": [{ "reloadContinuationItemsCommand": { "continuationItems": [
                { "commentsHeaderRenderer": { "countText": { "runs": [{ "text": "1,234" }, { "text": " Comments" }] } } },
                { "commentThreadRenderer": {
                    "commentViewModel": { "commentViewModel": { "commentKey": "K1" } },
                    "replies": { "commentRepliesRenderer": { "contents": [{ "continuationItemRenderer": {
                        "continuationEndpoint": { "continuationCommand": { "token": "R1" } } } }] } },
                }},
                { "continuationItemRenderer": { "continuationEndpoint": { "continuationCommand": { "token": "NEXT" } } } },
            ]}}],
            "frameworkUpdates": { "entityBatchUpdate": { "mutations": [{ "entityKey": "K1", "payload": { "commentEntityPayload": {
                "properties": { "commentId": "c1", "content": { "content": "Nice video" }, "publishedTime": "2 days ago" },
                "author": { "displayName": "@someone", "channelId": "UCx", "isCreator": true },
                "toolbar": { "likeCountNotliked": "12", "replyCount": "" },
            }}}]}},
        });
        let c = parse_comments(&resp);
        assert_eq!(c.count.as_deref(), Some("1,234 Comments"));
        assert_eq!(c.next.as_deref(), Some("NEXT"));
        let first = &c.items[0];
        assert_eq!((first.text.as_str(), first.author.as_str(), first.published.as_str()), ("Nice video", "@someone", "2 days ago"));
        assert_eq!((first.likes.as_deref(), first.reply_count.as_deref(), first.replies.as_deref()), (Some("12"), None, Some("R1")));
        assert!(first.creator);

        // Newer lists wrap the token one level deeper.
        let nested = json!([{ "continuationItemViewModel": { "continuationCommand": { "innertubeCommand": {
            "continuationCommand": { "token": "DEEP" } } } } }]);
        assert_eq!(continuation(&nested).as_deref(), Some("DEEP"));

        let mix = json!({ "contentType": "LOCKUP_CONTENT_TYPE_PLAYLIST", "contentId": "RDabc" });
        assert!(parse_playlist_lockup(&mix).is_none());
        let list = json!({
            "contentType": "LOCKUP_CONTENT_TYPE_PLAYLIST", "contentId": "PLabc",
            "contentImage": { "collectionThumbnailViewModel": { "primaryThumbnail": { "thumbnailViewModel": {
                "image": { "sources": [{ "url": "https://i.ytimg.com/vi/x/hq720.jpg" }] },
                "overlays": [{ "thumbnailOverlayBadgeViewModel": { "thumbnailBadges": [{ "thumbnailBadgeViewModel": { "text": "29 videos" } }] } }],
            }}}},
            "metadata": { "lockupMetadataViewModel": { "title": { "content": "Rain" },
                "metadata": { "contentMetadataViewModel": { "metadataRows": [{ "metadataParts": [{ "text": { "content": "the bootleg boy" } }] }] } } } },
        });
        let p = parse_playlist_lockup(&list).unwrap();
        assert_eq!((p.title.as_str(), p.channel.as_str(), p.count.as_deref()), ("Rain", "the bootleg boy", Some("29 videos")));
        assert_eq!(p.thumbnail.as_deref(), Some("https://i.ytimg.com/vi/x/hq720.jpg"));
    }
}
