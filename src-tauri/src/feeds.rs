//! New uploads from subscribed channels, read from each channel's public RSS
//! feed. The feed needs no account, has exact upload times and marks Shorts
//! (their links go to /shorts/), but only lists a channel's latest 15 uploads.

use std::time::Duration;

use futures_util::StreamExt;

use crate::Video;

/// Channels checked at once.
const PARALLEL: usize = 8;

/// One feed entry: the video, plus whether it is a Short.
#[derive(Debug, Clone)]
pub struct Entry {
    pub video: Video,
    pub short: bool,
}

/// Fetches the feeds of `channels`. Returns the entries found and how many
/// channels couldn't be read.
pub async fn fetch_all(http: &reqwest::Client, channels: &[String]) -> (Vec<Entry>, usize) {
    let results: Vec<Result<Vec<Entry>, String>> = futures_util::stream::iter(channels.iter().cloned())
        .map(|id| async move { fetch(http, &id).await })
        .buffer_unordered(PARALLEL)
        .collect()
        .await;
    let mut entries = Vec::new();
    let mut failed = 0;
    for r in results {
        match r {
            Ok(e) => entries.extend(e),
            Err(_) => failed += 1,
        }
    }
    (entries, failed)
}

async fn fetch(http: &reqwest::Client, channel_id: &str) -> Result<Vec<Entry>, String> {
    let url = format!("https://www.youtube.com/feeds/videos.xml?channel_id={channel_id}");
    // The feed server sometimes answers 404 or 500 for a moment; one retry helps.
    let mut last = String::new();
    for attempt in 0..2 {
        if attempt > 0 {
            tokio::time::sleep(Duration::from_millis(800)).await;
        }
        match http.get(&url).send().await.and_then(|r| r.error_for_status()) {
            Ok(resp) => return Ok(parse(&resp.text().await.map_err(|e| e.to_string())?)),
            Err(e) => last = e.to_string(),
        }
    }
    Err(last)
}

/// The text inside the first `<tag>…</tag>` in `s`.
fn tag<'a>(s: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let start = s.find(&open)? + open.len();
    let end = s[start..].find(&format!("</{tag}>"))? + start;
    Some(&s[start..end])
}

/// The value of `attr="…"` in the first `<tag …>` in `s`.
fn attr<'a>(s: &'a str, tag: &str, attr: &str) -> Option<&'a str> {
    let start = s.find(&format!("<{tag} "))?;
    let element = &s[start..start + s[start..].find('>')?];
    let key = format!("{attr}=\"");
    let from = element.find(&key)? + key.len();
    Some(&element[from..from + element[from..].find('"')?])
}

fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let Some(end) = rest.find(';').filter(|&e| e <= 10) else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let entity = &rest[1..end];
        let ch = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => entity
                .strip_prefix("#x")
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| entity.strip_prefix('#').and_then(|d| d.parse().ok()))
                .and_then(char::from_u32),
        };
        match ch {
            Some(c) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Seconds since 1970 for an RFC 3339 time like "2026-10-27T14:00:11+00:00".
pub fn parse_time(s: &str) -> Option<i64> {
    let s = s.trim();
    let num = |a: usize, b: usize| s.get(a..b)?.parse::<i64>().ok();
    let (y, m, d) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (hh, mm, ss) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    // The offset is the last six characters ("+02:00"), or "Z".
    let offset = match s.as_bytes().last()? {
        b'Z' => 0,
        _ => {
            let o = &s[s.len().checked_sub(6)?..];
            let sign = if o.starts_with('-') { -1 } else { 1 };
            sign * (o.get(1..3)?.parse::<i64>().ok()? * 3600 + o.get(4..6)?.parse::<i64>().ok()? * 60)
        }
    };
    // Days from the civil calendar (Howard Hinnant's algorithm).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * ((m + 9) % 12) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + hh * 3600 + mm * 60 + ss - offset)
}

/// "90344" → "90K views", the way YouTube shortens counts.
pub fn short_views(n: u64) -> String {
    let (value, unit) = match n {
        0..=999 => return format!("{n} view{}", if n == 1 { "" } else { "s" }),
        1_000..=999_999 => (n as f64 / 1e3, "K"),
        1_000_000..=999_999_999 => (n as f64 / 1e6, "M"),
        _ => (n as f64 / 1e9, "B"),
    };
    let text = if value < 10.0 { format!("{:.1}", (value * 10.0).floor() / 10.0) } else { format!("{}", value.floor()) };
    format!("{}{unit} views", text.trim_end_matches(".0"))
}

pub fn parse(xml: &str) -> Vec<Entry> {
    let channel = tag(xml, "title").map(unescape).unwrap_or_default();
    xml.split("<entry>")
        .skip(1)
        .filter_map(|entry| {
            let id = tag(entry, "yt:videoId")?;
            let link = attr(entry, "link", "href").unwrap_or("");
            Some(Entry {
                short: link.contains("/shorts/"),
                video: Video {
                    id: id.to_string(),
                    title: unescape(tag(entry, "title")?),
                    channel: tag(entry, "name").map(unescape).unwrap_or_else(|| channel.clone()),
                    channel_id: tag(entry, "yt:channelId").map(str::to_string),
                    duration: None,
                    views: attr(entry, "media:statistics", "views")
                        .and_then(|v| v.parse().ok())
                        // Upcoming premieres and fresh uploads show 0.
                        .filter(|&n| n > 0)
                        .map(short_views),
                    published: None,
                    published_at: tag(entry, "published").and_then(parse_time),
                    thumbnail: crate::youtube::thumbnail(id),
                    avatar: None,
                    live: false,
                },
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const FEED: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns:yt="http://www.youtube.com/xml/schemas/2015">
 <title>Veritasium</title>
 <entry>
  <yt:videoId>Kap7mRzr13w</yt:videoId>
  <yt:channelId>UCHnyfMqiRRG1u-2MsSQLbXA</yt:channelId>
  <title>Why doesn&#39;t your heart get tired?</title>
  <link rel="alternate" href="https://www.youtube.com/shorts/Kap7mRzr13w"/>
  <author><name>Veritasium</name></author>
  <published>2026-10-27T14:00:11+00:00</published>
  <media:group><media:community><media:statistics views="90344"/></media:community></media:group>
 </entry>
 <entry>
  <yt:videoId>eyZEqSPeMwg</yt:videoId>
  <yt:channelId>UCHnyfMqiRRG1u-2MsSQLbXA</yt:channelId>
  <title>Einstein &amp; clocks</title>
  <link rel="alternate" href="https://www.youtube.com/watch?v=eyZEqSPeMwg"/>
  <author><name>Veritasium</name></author>
  <published>2026-10-23T18:46:44+02:00</published>
  <media:group><media:community><media:statistics views="0"/></media:community></media:group>
 </entry>
</feed>"#;

    #[test]
    fn reads_feeds() {
        let entries = parse(FEED);
        assert_eq!(entries.len(), 2);
        assert!(entries[0].short && !entries[1].short);
        let v = &entries[0].video;
        assert_eq!(v.title, "Why doesn't your heart get tired?");
        assert_eq!(v.channel_id.as_deref(), Some("UCHnyfMqiRRG1u-2MsSQLbXA"));
        assert_eq!(v.views.as_deref(), Some("90K views"));
        assert_eq!(v.published_at, Some(1_793_109_611));
        assert_eq!(entries[1].video.title, "Einstein & clocks");
        assert_eq!(entries[1].video.views, None);
        assert_eq!(entries[1].video.published_at, parse_time("2026-10-23T16:46:44Z"));
    }

    #[test]
    fn formats_counts_and_times() {
        assert_eq!(short_views(1), "1 view");
        assert_eq!(short_views(999), "999 views");
        assert_eq!(short_views(1_250), "1.2K views");
        assert_eq!(short_views(90_344), "90K views");
        assert_eq!(short_views(5_841_897), "5.8M views");
        assert_eq!(short_views(1_000_000), "1M views");
        assert_eq!(parse_time("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_time("2000-03-01T00:00:00+00:00"), Some(951_868_800));
        assert_eq!(unescape("a &amp; b &#x41; &bogus c"), "a & b A &bogus c");
    }
}
