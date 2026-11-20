//! "For you": recommendations made on the phone, without an account. Vista
//! asks YouTube for the videos related to a few things you did (videos you
//! watched, your searches, new uploads from channels you follow) and mixes
//! them: videos that come up from several of them rank higher, videos you
//! already watched are left out, and no channel takes over the list.
//!
//! Nothing about you is sent anywhere; YouTube only sees ordinary requests
//! for the related videos of a video, or a search.

use std::collections::{HashMap, HashSet};

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};

use crate::{youtube, Video};

/// Seeds asked about on each refresh, by kind.
const FROM_WATCHED: usize = 5;
const FROM_SEARCHES: usize = 3;
const FROM_SUBSCRIPTIONS: usize = 3;
/// Top results of a topic search whose related videos are asked for.
const FROM_TOPIC: usize = 3;
/// The most videos kept from one channel.
const PER_CHANNEL: usize = 2;
const LIMIT: usize = 80;

/// A recommended video and why it's there.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Rec {
    pub video: Video,
    /// "Because you watched …", "From your search “…”", "Like …"
    pub reason: String,
}

/// Something Vista knows you like, and what it found for it.
#[derive(Debug, Clone)]
pub enum Seed {
    Watched(Video),
    Search(String),
    /// A new upload from a subscribed channel.
    Subscription(Video),
    /// A topic chip on Home, searched for.
    Topic(String),
    /// A top search result for a topic, whose related videos fit it too.
    Related(Video),
}

impl Seed {
    fn reason(&self) -> String {
        match self {
            Seed::Watched(v) => format!("Because you watched “{}”", v.title),
            Seed::Search(q) => format!("From your search “{q}”"),
            Seed::Subscription(v) => format!("Because you follow {}", v.channel),
            // The chip says it already.
            Seed::Topic(_) | Seed::Related(_) => String::new(),
        }
    }

    async fn fetch(&self, http: &reqwest::Client) -> Result<Vec<Video>, String> {
        match self {
            Seed::Watched(v) | Seed::Subscription(v) | Seed::Related(v) => youtube::watch(http, &v.id).await.map(|w| w.related.items),
            Seed::Search(q) | Seed::Topic(q) => youtube::search(http, q, None).await.map(|r| r.videos),
        }
    }
}

/// A small random number generator; good enough for picking seeds.
struct Shuffle(u64);

impl Shuffle {
    fn new() -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(1);
        Self(nanos | 1)
    }

    fn next(&mut self, below: usize) -> usize {
        // xorshift64
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % below.max(1) as u64) as usize
    }

    /// `n` items: the first `keep` as they are, the rest picked at random
    /// from the remainder, so each refresh brings something new.
    fn pick<T: Clone>(&mut self, items: &[T], n: usize, keep: usize) -> Vec<T> {
        let mut out: Vec<T> = items.iter().take(keep.min(n)).cloned().collect();
        let mut rest: Vec<T> = items.iter().skip(out.len()).cloned().collect();
        while out.len() < n && !rest.is_empty() {
            out.push(rest.remove(self.next(rest.len())));
        }
        out
    }
}

/// Chooses what to ask about: recent videos and searches always, plus a few
/// older ones at random.
pub fn seeds(watched: &[Video], searches: &[String], uploads: &[Video]) -> Vec<Seed> {
    let mut rng = Shuffle::new();
    let mut seeds: Vec<Seed> = rng.pick(&watched[..watched.len().min(30)], FROM_WATCHED, 2).into_iter().map(Seed::Watched).collect();
    seeds.extend(rng.pick(&searches[..searches.len().min(15)], FROM_SEARCHES, 1).into_iter().map(Seed::Search));
    // One upload per channel, so a busy channel doesn't use up every spot.
    let mut channels = HashSet::new();
    let uploads: Vec<Video> = uploads
        .iter()
        .filter(|v| channels.insert(v.channel_id.clone()))
        .take(12)
        .cloned()
        .collect();
    seeds.extend(rng.pick(&uploads, FROM_SUBSCRIPTIONS, 1).into_iter().map(Seed::Subscription));
    seeds
}

/// For a topic: the related videos of its first few search results that you
/// haven't watched (live streams make poor seeds).
pub fn topic_seeds(results: &[Video], watched: &HashSet<String>) -> Vec<Seed> {
    results
        .iter()
        .filter(|v| !v.live && !watched.contains(&v.id))
        .take(FROM_TOPIC)
        .cloned()
        .map(Seed::Related)
        .collect()
}

/// Leaves out videos and channels you said you're not interested in.
pub fn drop_hidden(recs: &mut Vec<Rec>, hidden: &[crate::db::Hidden]) {
    let ids: HashSet<&str> = hidden.iter().map(|h| h.id.as_str()).collect();
    recs.retain(|r| !ids.contains(r.video.id.as_str()) && !r.video.channel_id.as_deref().is_some_and(|c| ids.contains(c)));
}

/// Asks YouTube about every seed at once. Seeds that fail are skipped.
pub async fn fetch(http: &reqwest::Client, seeds: Vec<Seed>) -> Vec<(Seed, Vec<Video>)> {
    futures_util::stream::iter(seeds)
        .map(|seed| async move {
            let videos = seed.fetch(http).await.unwrap_or_default();
            (seed, videos)
        })
        .buffer_unordered(6)
        .filter(|(_, videos)| std::future::ready(!videos.is_empty()))
        .collect()
        .await
}

/// Mixes what the seeds found into one list. `watched` are video ids to
/// leave out; `subscribed` are channel ids, whose videos rank a little lower
/// (the Subscriptions feed has them already).
pub fn mix(found: &[(Seed, Vec<Video>)], watched: &HashSet<String>, subscribed: &HashSet<String>) -> Vec<Rec> {
    struct Candidate<'a> {
        video: &'a Video,
        score: f64,
        reason: String,
        first_seen: usize,
    }
    let seed_ids: HashSet<&str> = found
        .iter()
        .filter_map(|(s, _)| match s {
            Seed::Watched(v) | Seed::Subscription(v) => Some(v.id.as_str()),
            // A topic's top results belong in its list.
            Seed::Search(_) | Seed::Topic(_) | Seed::Related(_) => None,
        })
        .collect();
    let mut candidates: HashMap<&str, Candidate> = HashMap::new();
    let mut order = 0;
    for (seed, videos) in found {
        let n = videos.len().max(1) as f64;
        for (i, v) in videos.iter().enumerate() {
            if watched.contains(&v.id) || seed_ids.contains(v.id.as_str()) {
                continue;
            }
            // Earlier in a related list means more closely related.
            let weight = 1.0 + (n - i as f64) / n;
            let entry = candidates.entry(&v.id).or_insert_with(|| {
                order += 1;
                Candidate {
                    video: v,
                    score: 0.0,
                    reason: seed.reason(),
                    first_seen: order,
                }
            });
            entry.score += weight;
        }
    }
    let mut ranked: Vec<Candidate> = candidates.into_values().collect();
    for c in &mut ranked {
        if c.video.channel_id.as_ref().is_some_and(|id| subscribed.contains(id)) {
            c.score *= 0.7;
        }
        // Live streams rarely make good suggestions later on.
        if c.video.live {
            c.score *= 0.5;
        }
    }
    ranked.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.first_seen.cmp(&b.first_seen)));
    let mut per_channel: HashMap<String, usize> = HashMap::new();
    ranked
        .into_iter()
        .filter(|c| {
            let key = c.video.channel_id.clone().unwrap_or_else(|| c.video.channel.clone());
            let n = per_channel.entry(key).or_default();
            *n += 1;
            *n <= PER_CHANNEL
        })
        .take(LIMIT)
        .map(|c| Rec { video: c.video.clone(), reason: c.reason })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn video(id: &str, channel: &str) -> Video {
        Video {
            id: id.into(),
            title: format!("Video {id}"),
            channel: channel.into(),
            channel_id: Some(format!("UC{channel}")),
            ..Default::default()
        }
    }

    #[test]
    fn mixes_and_filters() {
        let seen = video("seen", "a");
        let found = vec![
            (Seed::Watched(seen.clone()), vec![video("x", "b"), video("y", "c"), video("seen", "a"), video("z", "sub")]),
            (Seed::Search("cats".into()), vec![video("y", "c"), video("w", "d")]),
        ];
        let watched = HashSet::from(["seen".to_string()]);
        let subscribed = HashSet::from(["UCsub".to_string()]);
        let recs = mix(&found, &watched, &subscribed);
        let ids: Vec<&str> = recs.iter().map(|r| r.video.id.as_str()).collect();
        // "y" came up twice, so it leads; the watched video is gone.
        assert_eq!(ids[0], "y");
        assert!(!ids.contains(&"seen"));
        assert_eq!(ids.len(), 4);
        assert_eq!(recs[0].reason, "Because you watched “Video seen”");
        // A subscribed channel's video ranks below an equal one from elsewhere.
        assert!(ids.iter().position(|i| *i == "z") > ids.iter().position(|i| *i == "x"));
    }

    #[test]
    fn mixes_topics() {
        let mut live = video("live", "l");
        live.live = true;
        let results = vec![live, video("seen", "a"), video("t1", "b"), video("t2", "c"), video("t3", "d"), video("t4", "e")];
        let watched = HashSet::from(["seen".to_string()]);
        let seeds = topic_seeds(&results, &watched);
        let ids: Vec<&str> = seeds.iter().filter_map(|s| if let Seed::Related(v) = s { Some(v.id.as_str()) } else { None }).collect();
        assert_eq!(ids, ["t1", "t2", "t3"]);
        // The topic's own results stay in its list, without a reason line.
        let found = vec![(Seed::Topic("tech".into()), results), (seeds[0].clone(), vec![video("r", "f"), video("t2", "c")])];
        let recs = mix(&found, &watched, &HashSet::new());
        assert_eq!(recs[0].video.id, "t2");
        assert!(recs.iter().any(|r| r.video.id == "t1"));
        assert!(!recs.iter().any(|r| r.video.id == "seen"));
        assert!(recs.iter().all(|r| r.reason.is_empty()));
    }

    #[test]
    fn leaves_out_hidden() {
        let hide = |id: &str, kind: &str| crate::db::Hidden { id: id.into(), kind: kind.into(), name: String::new() };
        let mut recs: Vec<Rec> = [video("a", "x"), video("b", "y"), video("c", "z")]
            .into_iter()
            .map(|video| Rec { video, reason: String::new() })
            .collect();
        drop_hidden(&mut recs, &[hide("a", "video"), hide("UCz", "channel")]);
        assert_eq!(recs.iter().map(|r| r.video.id.as_str()).collect::<Vec<_>>(), ["b"]);
    }

    #[test]
    fn keeps_channels_from_taking_over() {
        let many: Vec<Video> = (0..6).map(|i| video(&format!("v{i}"), "same")).collect();
        let found = vec![(Seed::Search("q".into()), many)];
        let recs = mix(&found, &HashSet::new(), &HashSet::new());
        assert_eq!(recs.len(), PER_CHANNEL);
    }

    #[test]
    fn picks_seeds() {
        let watched: Vec<Video> = (0..20).map(|i| video(&format!("w{i}"), &format!("c{i}"))).collect();
        let searches: Vec<String> = (0..5).map(|i| format!("q{i}")).collect();
        let uploads = vec![video("u1", "busy"), video("u2", "busy"), video("u3", "other")];
        let seeds = seeds(&watched, &searches, &uploads);
        let watched_seeds: Vec<&str> = seeds
            .iter()
            .filter_map(|s| if let Seed::Watched(v) = s { Some(v.id.as_str()) } else { None })
            .collect();
        // The two latest videos always, three more at random, no repeats.
        assert_eq!(watched_seeds.len(), FROM_WATCHED);
        assert_eq!(&watched_seeds[..2], ["w0", "w1"]);
        assert_eq!(watched_seeds.iter().collect::<HashSet<_>>().len(), FROM_WATCHED);
        assert!(seeds.iter().any(|s| matches!(s, Seed::Search(q) if q == "q0")));
        // One upload per channel: only two channels to pick from.
        assert_eq!(seeds.iter().filter(|s| matches!(s, Seed::Subscription(_))).count(), 2);
        assert!(super::seeds(&[], &[], &[]).is_empty());
    }
}
