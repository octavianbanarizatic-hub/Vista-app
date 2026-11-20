//! Everything the page can ask for.

use std::collections::HashMap;

use futures_util::StreamExt;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::player::{self, LoadArgs, PlayInfo};
use crate::youtube::{self, ChannelPage, Page, SearchResults, WatchInfo};
use crate::{backup, db, extras, feeds, media, recommend, updater, ytdlp, AppState, Channel, CmdResult, Video};

/// What `play` answers when another video was asked for while this one loaded.
const SUPERSEDED: &str = "superseded";

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

#[tauri::command]
pub async fn ytdlp_status(state: State<'_, AppState>) -> CmdResult<ytdlp::Status> {
    Ok(state.ytdlp.status().await)
}

#[derive(Clone, Serialize)]
struct InstallProgress {
    downloaded: u64,
    total: Option<u64>,
}

#[tauri::command]
pub async fn ytdlp_install(app: AppHandle, state: State<'_, AppState>) -> CmdResult<ytdlp::Status> {
    let mut last_emit = 0;
    state
        .ytdlp
        .install(&state.http, |downloaded, total| {
            // Report roughly every 256 KB.
            if downloaded - last_emit >= 256 * 1024 || Some(downloaded) == total {
                last_emit = downloaded;
                let _ = app.emit("ytdlp-progress", InstallProgress { downloaded, total });
            }
        })
        .await?;
    Ok(state.ytdlp.status().await)
}

#[tauri::command]
pub async fn search(state: State<'_, AppState>, query: String, next: Option<String>) -> CmdResult<SearchResults> {
    let results = youtube::search(&state.http, query.trim(), next.as_deref()).await?;
    // Remembered for recommendations (only the first part counts as a search).
    if next.is_none() {
        db::search_add(&state.db.lock().unwrap(), &query).map_err(err)?;
    }
    Ok(results)
}

#[tauri::command]
pub async fn search_suggestions(state: State<'_, AppState>, query: String) -> CmdResult<Vec<String>> {
    youtube::suggestions(&state.http, query.trim()).await
}

/// Recent searches, newest first, for the search screen.
#[tauri::command]
pub fn searches(state: State<'_, AppState>) -> CmdResult<Vec<String>> {
    db::searches(&state.db.lock().unwrap(), 12).map_err(err)
}

#[tauri::command]
pub fn search_remove(state: State<'_, AppState>, query: String) -> CmdResult<()> {
    db::search_remove(&state.db.lock().unwrap(), &query).map_err(err)
}

#[tauri::command]
pub fn searches_clear(state: State<'_, AppState>) -> CmdResult<()> {
    db::searches_clear(&state.db.lock().unwrap()).map_err(err)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WatchPage {
    #[serde(flatten)]
    info: WatchInfo,
    subscribed: bool,
    saved: bool,
}

#[tauri::command]
pub async fn watch_info(state: State<'_, AppState>, id: String) -> CmdResult<WatchPage> {
    let info = youtube::watch(&state.http, &id).await?;
    let conn = state.db.lock().unwrap();
    Ok(WatchPage {
        subscribed: db::is_subscribed(&conn, &info.channel.id).map_err(err)?,
        saved: db::is_later(&conn, &id).map_err(err)?,
        info,
    })
}

/// The next part of a list: `kind` is "related" or "channel".
#[tauri::command]
pub async fn more(state: State<'_, AppState>, kind: String, token: String, channel: Option<Channel>) -> CmdResult<Page<Video>> {
    let endpoint = if kind == "channel" { "browse" } else { "next" };
    let mut page = youtube::more(&state.http, endpoint, &token).await?;
    if let Some(c) = channel {
        for v in &mut page.items {
            youtube::fill_channel(v, &c);
        }
    }
    Ok(page)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelView {
    #[serde(flatten)]
    page: ChannelPage,
    subscribed: bool,
}

#[tauri::command]
pub async fn channel(state: State<'_, AppState>, id: String) -> CmdResult<ChannelView> {
    let page = youtube::channel(&state.http, &id).await?;
    let conn = state.db.lock().unwrap();
    // Keeps a subscribed channel's name and avatar current.
    let subscribed = db::is_subscribed(&conn, &id).map_err(err)?;
    if subscribed {
        db::subscribe(&conn, &page.channel).map_err(err)?;
    }
    Ok(ChannelView { page, subscribed })
}

/// Starts `video` at `quality` (a height like 1080), `start` seconds in.
/// `skip` lists the SponsorBlock categories to skip; `fresh` drops streams
/// found earlier (after they stopped working).
#[tauri::command]
pub async fn play(
    app: AppHandle,
    state: State<'_, AppState>,
    video: Video,
    quality: u32,
    start: f64,
    skip: Vec<String>,
    fresh: bool,
) -> CmdResult<PlayInfo> {
    if !media::valid_video_id(&video.id) {
        return Err("bad video id".into());
    }
    *state.playing.lock().unwrap() = video.id.clone();
    if fresh {
        state.streams.invalidate(&video.id);
    }
    // A downloaded video plays from its files, without asking yt-dlp.
    let saved = db::download(&state.db.lock().unwrap(), &video.id).map_err(err)?;
    if let Some(files) = saved.as_ref().and_then(|d| state.downloads.files(d)) {
        return play_saved(&app, &state, video, files, start, &skip).await;
    }
    let (resolved, segments) = tokio::join!(
        state.streams.get(&state.ytdlp, &video.id),
        extras::segments(&state.http, &video.id)
    );
    let resolved = resolved?;
    let mut segments = segments.unwrap_or_default();
    segments.retain(|s| skip.contains(&s.category));
    let choice = player::choose(&resolved, quality);
    // Desktop always plays the one file with sound and picture together.
    let (qualities, quality) = if cfg!(desktop) {
        (Vec::new(), choice.muxed.and_then(|f| f.height))
    } else {
        (choice.heights.clone(), choice.video.and_then(|f| f.height))
    };
    let info = PlayInfo {
        qualities,
        quality,
        duration: resolved.duration,
        chapters: resolved.chapters.clone(),
        segments: segments.clone(),
        live: resolved.live,
        src: cfg!(desktop).then(|| format!("yt/{}", video.id)),
        offline: false,
    };
    let mut args = LoadArgs::new(&video.id, &resolved, &choice);
    if !args.playable() {
        return Err("yt-dlp found no stream Vista can play for this video.".into());
    }
    if cfg!(desktop) && choice.muxed.is_none() {
        return Err(if resolved.live {
            "Live streams only play in the phone app.".into()
        } else {
            "This video has no file the desktop player can play. It works in the phone app.".into()
        });
    }
    args.start = start;
    args.title = video.title.clone();
    args.channel = video.channel.clone();
    args.artwork = video.thumbnail.clone();
    args.segments = segments;
    // Fresh stream URLs are refused for a moment.
    let age = resolved.resolved_at.elapsed();
    if age < media::URL_WARMUP {
        tokio::time::sleep(media::URL_WARMUP - age).await;
    }
    if *state.playing.lock().unwrap() != video.id {
        return Err(SUPERSEDED.into());
    }
    load_native(&app, args).await?;
    Ok(info)
}

async fn play_saved(
    app: &AppHandle,
    state: &AppState,
    video: Video,
    files: Vec<std::path::PathBuf>,
    start: f64,
    skip: &[String],
) -> CmdResult<PlayInfo> {
    // Offline, SponsorBlock can't answer; don't keep the video waiting for it.
    let mut segments = tokio::time::timeout(std::time::Duration::from_secs(3), extras::segments(&state.http, &video.id))
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or_default();
    segments.retain(|s| skip.contains(&s.category));
    let path = |p: &std::path::PathBuf| p.to_string_lossy().into_owned();
    let mut args = LoadArgs {
        id: video.id.clone(),
        start,
        title: video.title.clone(),
        channel: video.channel.clone(),
        artwork: video.thumbnail.clone(),
        segments: segments.clone(),
        ..Default::default()
    };
    // Video then audio, or one file with both (see downloads.rs).
    if let [v, a] = files.as_slice() {
        args.video_url = Some(path(v));
        args.audio_url = Some(path(a));
    } else {
        args.muxed_url = Some(path(&files[0]));
    }
    load_native(app, args).await?;
    Ok(PlayInfo {
        duration: video.duration,
        segments,
        src: cfg!(desktop).then(|| format!("yt/{}", video.id)),
        offline: true,
        ..Default::default()
    })
}

/// Finds streams for videos the user is likely to open next, so they start
/// sooner. One at a time, in the background; already found ones are skipped.
#[tauri::command]
pub fn prefetch(app: AppHandle, state: State<'_, AppState>, ids: Vec<String>) {
    for id in ids.into_iter().filter(|id| media::valid_video_id(id)).take(3) {
        if state.streams.contains(&id) {
            continue;
        }
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            let state = app.state::<AppState>();
            let Ok(_slot) = state.prefetch_slots.acquire().await else { return };
            if state.streams.contains(&id) || db::download(&state.db.lock().unwrap(), &id).ok().flatten().is_some_and(|d| d.done) {
                return;
            }
            let _ = state.streams.get(&state.ytdlp, &id).await;
        });
    }
}

#[cfg(target_os = "android")]
async fn load_native(app: &AppHandle, args: LoadArgs) -> CmdResult<()> {
    use tauri_plugin_vista_android::VistaAndroidExt;
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || app.vista_android().player_load(&args))
        .await
        .map_err(err)?
}

/// Desktop plays in the page's <video> element, from `PlayInfo::src`.
#[cfg(desktop)]
async fn load_native(_app: &AppHandle, _args: LoadArgs) -> CmdResult<()> {
    Ok(())
}

/// Controls the Android player: "play", "pause", "seek" (to `value`
/// seconds), "rate", "show", "hide", "stop", "background" and "pip" (1 or 0).
/// Desktop pages control their own <video> element instead.
#[tauri::command]
pub async fn player_command(app: AppHandle, command: String, value: Option<f64>) -> CmdResult<()> {
    #[cfg(target_os = "android")]
    {
        use tauri_plugin_vista_android::VistaAndroidExt;
        tauri::async_runtime::spawn_blocking(move || app.vista_android().player_command(&command, value))
            .await
            .map_err(err)?
    }
    #[cfg(desktop)]
    {
        let _ = (app, command, value);
        Ok(())
    }
}

/// Phones: your subscriptions (the page's JSON: `{ channels, shorts }`) to
/// check for new videos in the background, or with `on` false, stop checking.
#[tauri::command]
pub async fn uploads_watch(app: AppHandle, json: String, on: bool) -> CmdResult<()> {
    #[cfg(target_os = "android")]
    {
        use tauri_plugin_vista_android::VistaAndroidExt;
        tauri::async_runtime::spawn_blocking(move || app.vista_android().watch_uploads(&json, on))
            .await
            .map_err(err)?
    }
    #[cfg(desktop)]
    {
        let _ = (app, json, on);
        Ok(())
    }
}

/// Phones: whether Vista may show notifications; Android 13 and newer ask
/// the first time. The desktop has no notifications.
#[tauri::command]
pub async fn notifications_allow(app: AppHandle) -> CmdResult<bool> {
    #[cfg(target_os = "android")]
    {
        use tauri_plugin_vista_android::VistaAndroidExt;
        tauri::async_runtime::spawn_blocking(move || app.vista_android().allow_notifications())
            .await
            .map_err(err)?
    }
    #[cfg(desktop)]
    {
        let _ = app;
        Ok(false)
    }
}

/// Whether the connection is Wi-Fi or another that isn't metered. The
/// desktop counts as unmetered.
#[tauri::command]
pub async fn unmetered(app: AppHandle) -> CmdResult<bool> {
    #[cfg(target_os = "android")]
    {
        use tauri_plugin_vista_android::VistaAndroidExt;
        tauri::async_runtime::spawn_blocking(move || app.vista_android().unmetered())
            .await
            .map_err(err)?
    }
    #[cfg(desktop)]
    {
        let _ = app;
        Ok(true)
    }
}

/// A video's link, from `start` seconds in when given.
fn video_link(id: &str, start: Option<u32>) -> Option<String> {
    let valid = id.len() == 11 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    valid.then(|| match start {
        Some(t) if t > 0 => format!("https://youtu.be/{id}?t={t}"),
        _ => format!("https://youtu.be/{id}"),
    })
}

/// Phones: opens Android's share sheet with a video's link and says true.
/// The desktop has no share sheet: false, and the page copies the link.
#[tauri::command]
pub async fn share(app: AppHandle, id: String, start: Option<u32>) -> CmdResult<bool> {
    let link = video_link(&id, start).ok_or("That video can't be shared.")?;
    #[cfg(target_os = "android")]
    {
        use tauri_plugin_vista_android::VistaAndroidExt;
        tauri::async_runtime::spawn_blocking(move || app.vista_android().share(&link))
            .await
            .map_err(err)??;
        Ok(true)
    }
    #[cfg(desktop)]
    {
        let _ = (app, link);
        Ok(false)
    }
}

// ---- app updates -------------------------------------------------------------

/// Whether a newer Vista is out. Only phones update; the desktop build is for testing.
#[tauri::command]
pub async fn app_update_check(app: AppHandle, state: State<'_, AppState>) -> CmdResult<updater::Check> {
    let current = app.package_info().version.to_string();
    if cfg!(desktop) {
        return Ok(updater::Check { current, update: None });
    }
    updater::check(&state.http, &current).await
}

/// Downloads the newest version and opens Android's installer. Resolves to
/// false when Android first wants permission to install apps from Vista
/// (its settings screen is open then).
#[tauri::command]
pub async fn app_update_install(app: AppHandle, state: State<'_, AppState>) -> CmdResult<bool> {
    #[cfg(target_os = "android")]
    {
        use tauri_plugin_vista_android::VistaAndroidExt;
        let current = app.package_info().version.to_string();
        let release = updater::check(&state.http, &current)
            .await?
            .update
            .ok_or("Vista is up to date.")?;
        let dir = app.path().app_cache_dir().map_err(err)?.join("updates");
        let apk = updater::download(&app, &state.http, &release, &dir).await?;
        tauri::async_runtime::spawn_blocking(move || app.vista_android().install_apk(&apk.to_string_lossy()))
            .await
            .map_err(err)?
    }
    #[cfg(desktop)]
    {
        let _ = (app, state);
        Err("Updates are only for phones.".into())
    }
}

// ---- backups ---------------------------------------------------------------

/// Automatic backups kept; older ones are deleted.
const AUTO_BACKUPS_KEPT: u32 = 4;

/// Saves a backup named `name` where people find it: on phones in Downloads
/// → Vista backups, on the desktop in Documents → Vista backups. With
/// `auto`, only the newest few automatic backups stay. Returns the folder.
#[tauri::command]
pub async fn backup_save(app: AppHandle, state: State<'_, AppState>, settings: String, name: String, auto: bool) -> CmdResult<String> {
    let plain = name.ends_with(".vista-backup") && name.chars().all(|c| c.is_ascii_alphanumeric() || " ._-".contains(c));
    if !plain {
        return Err("bad backup name".into());
    }
    let keep = if auto { AUTO_BACKUPS_KEPT } else { 0 };
    #[cfg(target_os = "android")]
    {
        use tauri_plugin_vista_android::VistaAndroidExt;
        // Made in the cache first; Android copies it to Downloads.
        let dir = app.path().app_cache_dir().map_err(err)?.join("backup");
        std::fs::create_dir_all(&dir).map_err(err)?;
        let tmp = dir.join(&name);
        backup::create(&state.db.lock().unwrap(), &tmp, &settings)?;
        let path = tmp.to_string_lossy().into_owned();
        let handle = app.clone();
        let place = tauri::async_runtime::spawn_blocking(move || handle.vista_android().save_backup(&path, &name, keep))
            .await
            .map_err(err)?;
        let _ = std::fs::remove_file(&tmp);
        place
    }
    #[cfg(desktop)]
    {
        let dir = app.path().document_dir().map_err(err)?.join("Vista backups");
        std::fs::create_dir_all(&dir).map_err(err)?;
        backup::create(&state.db.lock().unwrap(), &dir.join(&name), &settings)?;
        if keep > 0 {
            backup::prune(&dir, backup::AUTO_PREFIX, keep as usize);
        }
        Ok(dir.to_string_lossy().into_owned())
    }
}

/// Takes a backup file the page read (from the file picker) and keeps it
/// where `backup_peek` and `backup_restore` read it. (A plain list of bytes:
/// Android's bridge doesn't carry raw request bodies.)
#[tauri::command]
pub fn backup_stage(app: AppHandle, bytes: Vec<u8>) -> CmdResult<String> {
    let dir = app.path().app_cache_dir().map_err(err)?.join("backup");
    std::fs::create_dir_all(&dir).map_err(err)?;
    let path = dir.join("picked.vista-backup");
    std::fs::write(&path, bytes).map_err(err)?;
    Ok(path.to_string_lossy().into_owned())
}

/// What a backup file holds, shown before restoring it.
#[tauri::command]
pub fn backup_peek(path: String) -> CmdResult<backup::Summary> {
    backup::peek(std::path::Path::new(&path)).map(|(summary, _)| summary)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Restored {
    summary: backup::Summary,
    /// The backup's page settings, for the page to apply.
    settings: String,
}

/// Replaces your data with the backup's. What it replaces is kept first, in
/// the app's own `backups` folder.
#[tauri::command]
pub fn backup_restore(app: AppHandle, state: State<'_, AppState>, path: String, current_settings: String) -> CmdResult<Restored> {
    let safety_dir = app.path().app_data_dir().map_err(err)?.join("backups");
    let (summary, settings, _) =
        backup::restore(&state.db.lock().unwrap(), std::path::Path::new(&path), &safety_dir, &current_settings)?;
    let _ = std::fs::remove_file(&path);
    Ok(Restored { summary, settings })
}

/// Comments, or one comment's replies, after a token from the video page.
#[tauri::command]
pub async fn comments(state: State<'_, AppState>, token: String) -> CmdResult<youtube::Comments> {
    youtube::comments(&state.http, &token).await
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaylistView {
    #[serde(flatten)]
    page: youtube::PlaylistPage,
    saved: bool,
}

#[tauri::command]
pub async fn playlist(state: State<'_, AppState>, id: String) -> CmdResult<PlaylistView> {
    let page = youtube::playlist(&state.http, &id).await?;
    let saved = db::is_playlist_saved(&state.db.lock().unwrap(), &id).map_err(err)?;
    Ok(PlaylistView { page, saved })
}

#[tauri::command]
pub fn playlist_save(state: State<'_, AppState>, playlist: youtube::Playlist, saved: bool) -> CmdResult<()> {
    db::playlist_save(&state.db.lock().unwrap(), &playlist, saved).map_err(err)
}

#[tauri::command]
pub fn saved_playlists(state: State<'_, AppState>) -> CmdResult<Vec<youtube::Playlist>> {
    db::saved_playlists(&state.db.lock().unwrap()).map_err(err)
}

/// What a YouTube link (or text with one in it) points at.
#[tauri::command]
pub async fn resolve_link(state: State<'_, AppState>, text: String) -> CmdResult<youtube::Link> {
    youtube::resolve_link(&state.http, &text).await
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadItem {
    #[serde(flatten)]
    download: db::Download,
    /// Being downloaded right now.
    active: bool,
    /// A saved thumbnail exists (`media://thumb/<id>`).
    thumb: bool,
    /// Finished, but its files are gone.
    missing: bool,
}

#[tauri::command]
pub fn downloads_list(state: State<'_, AppState>) -> CmdResult<Vec<DownloadItem>> {
    let current = state.downloads.current();
    let list = db::downloads(&state.db.lock().unwrap()).map_err(err)?;
    Ok(list
        .into_iter()
        .map(|d| DownloadItem {
            active: current.as_deref() == Some(&d.video.id),
            thumb: state.downloads.thumbnail(&d.video.id).exists(),
            missing: d.done && state.downloads.files(&d).is_none(),
            download: d,
        })
        .collect())
}

/// Queues `video` for offline watching at `quality` (a height like 720).
/// "downloads-changed" and "download-progress" keep the page informed.
#[tauri::command]
pub fn download_add(app: AppHandle, state: State<'_, AppState>, video: Video, quality: u32) -> CmdResult<()> {
    if !media::valid_video_id(&video.id) {
        return Err("bad video id".into());
    }
    if video.live {
        return Err("Live streams can't be downloaded.".into());
    }
    let conn = state.db.lock().unwrap();
    db::download_add(&conn, &video, quality).map_err(err)?;
    if db::download(&conn, &video.id).map_err(err)?.is_some_and(|d| d.done && state.downloads.files(&d).is_none()) {
        // Its files went missing: download it again.
        db::download_remove(&conn, &video.id).map_err(err)?;
        db::download_add(&conn, &video, quality).map_err(err)?;
    }
    state.downloads.enqueue(&video.id);
    let _ = app.emit("downloads-changed", ());
    Ok(())
}

#[tauri::command]
pub fn download_remove(app: AppHandle, state: State<'_, AppState>, id: String) -> CmdResult<()> {
    state.downloads.remove(&id);
    db::download_remove(&state.db.lock().unwrap(), &id).map_err(err)?;
    let _ = app.emit("downloads-changed", ());
    Ok(())
}

#[tauri::command]
pub async fn votes(state: State<'_, AppState>, id: String) -> CmdResult<extras::Votes> {
    extras::votes(&state.http, &id).await
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Feed {
    videos: Vec<Video>,
    /// When the feeds were last checked (seconds since 1970).
    checked: Option<i64>,
    /// Channels whose feed couldn't be read this time.
    failed: usize,
}

/// Feed entries kept for the page.
const FEED_LIMIT: u32 = 300;

/// The newest uploads of subscribed channels. With `refresh`, every
/// channel's feed is checked first.
#[tauri::command]
pub async fn feed(state: State<'_, AppState>, refresh: bool, shorts: bool) -> CmdResult<Feed> {
    let mut failed = 0;
    if refresh {
        let _guard = state.feed_lock.lock().await;
        let ids: Vec<String> = {
            let conn = state.db.lock().unwrap();
            db::subscriptions(&conn).map_err(err)?.into_iter().map(|c| c.id).collect()
        };
        let (entries, f) = feeds::fetch_all(&state.http, &ids).await;
        failed = f;
        let mut conn = state.db.lock().unwrap();
        db::save_feed(&mut conn, &entries).map_err(err)?;
        // Only a check that reached most channels counts as fresh.
        if failed * 2 <= ids.len() {
            db::set_meta(&conn, "feed_checked", &db::now().to_string()).map_err(err)?;
        }
    }
    let conn = state.db.lock().unwrap();
    Ok(Feed {
        videos: db::feed(&conn, shorts, FEED_LIMIT).map_err(err)?,
        checked: db::meta(&conn, "feed_checked").map_err(err)?.and_then(|s| s.parse().ok()),
        failed,
    })
}

#[derive(Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ForYou {
    items: Vec<recommend::Rec>,
    /// When the list was made (seconds since 1970).
    made: Option<i64>,
}

/// The "For you" list. Without `refresh`, the last one made (instantly);
/// with it, a new one from your history, searches and subscriptions.
#[tauri::command]
pub async fn recommendations(state: State<'_, AppState>, refresh: bool) -> CmdResult<ForYou> {
    let cached = || -> CmdResult<ForYou> {
        let conn = state.db.lock().unwrap();
        let mut recs: ForYou = db::meta(&conn, "recs")
            .map_err(err)?
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        // Hidden since the list was made.
        recommend::drop_hidden(&mut recs.items, &db::hidden(&conn).map_err(err)?);
        Ok(recs)
    };
    if !refresh {
        return cached();
    }
    let _guard = state.recs_lock.lock().await;
    let (seeds, watched, subscribed) = {
        let conn = state.db.lock().unwrap();
        let history = db::history(&conn, 300).map_err(err)?;
        let searches = db::searches(&conn, 15).map_err(err)?;
        let uploads = db::feed(&conn, false, 40).map_err(err)?;
        let subscribed: std::collections::HashSet<String> =
            db::subscriptions(&conn).map_err(err)?.into_iter().map(|c| c.id).collect();
        let watched_videos: Vec<Video> = history.iter().map(|w| w.video.clone()).collect();
        (
            recommend::seeds(&watched_videos, &searches, &uploads),
            history.into_iter().map(|w| w.video.id).collect::<std::collections::HashSet<_>>(),
            subscribed,
        )
    };
    if seeds.is_empty() {
        return Ok(ForYou::default());
    }
    let found = recommend::fetch(&state.http, seeds).await;
    if found.is_empty() {
        return Err("Can't reach YouTube. Check your internet connection.".into());
    }
    let mut items = recommend::mix(&found, &watched, &subscribed);
    recommend::drop_hidden(&mut items, &db::hidden(&state.db.lock().unwrap()).map_err(err)?);
    let made = ForYou { items, made: Some(db::now()) };
    db::set_meta(&state.db.lock().unwrap(), "recs", &serde_json::to_string(&made).map_err(err)?).map_err(err)?;
    Ok(made)
}

/// A topic chip on Home: a search for the topic, plus the videos related to
/// its top results, mixed like For you and without what you've watched.
#[tauri::command]
pub async fn topic(state: State<'_, AppState>, topic: String) -> CmdResult<Vec<recommend::Rec>> {
    let topic = topic.trim();
    let first = youtube::search(&state.http, topic, None).await?.videos;
    let watched: std::collections::HashSet<String> = {
        let conn = state.db.lock().unwrap();
        db::history(&conn, 300).map_err(err)?.into_iter().map(|w| w.video.id).collect()
    };
    let seeds = recommend::topic_seeds(&first, &watched);
    let mut found = recommend::fetch(&state.http, seeds).await;
    found.insert(0, (recommend::Seed::Topic(topic.to_string()), first));
    let mut items = recommend::mix(&found, &watched, &Default::default());
    recommend::drop_hidden(&mut items, &db::hidden(&state.db.lock().unwrap()).map_err(err)?);
    Ok(items)
}

#[tauri::command]
pub fn subscriptions(state: State<'_, AppState>) -> CmdResult<Vec<Channel>> {
    db::subscriptions(&state.db.lock().unwrap()).map_err(err)
}

/// Subscribes (or unsubscribes). A new channel's uploads are fetched right
/// away, and "feed-changed" tells the page when they're in.
#[tauri::command]
pub fn subscribe(app: AppHandle, state: State<'_, AppState>, channel: Channel, on: bool) -> CmdResult<()> {
    let conn = state.db.lock().unwrap();
    if !on {
        db::unsubscribe(&conn, &channel.id).map_err(err)?;
        let _ = app.emit("feed-changed", ());
        return Ok(());
    }
    db::subscribe(&conn, &channel).map_err(err)?;
    if channel.avatar.is_none() {
        spawn_complete_subscriptions(app);
        return Ok(());
    }
    tauri::async_runtime::spawn(async move {
        let state = app.state::<AppState>();
        let (entries, _) = feeds::fetch_all(&state.http, &[channel.id]).await;
        let saved = db::save_feed(&mut state.db.lock().unwrap(), &entries);
        if saved.is_ok() {
            let _ = app.emit("feed-changed", ());
        }
    });
    Ok(())
}

/// Reads Google Takeout's `subscriptions.csv` ("Channel Id,Channel Url,Channel Title").
fn parse_takeout(csv: &str) -> Vec<Channel> {
    csv.lines()
        .filter_map(|line| {
            let mut cols = line.trim_start_matches('\u{feff}').splitn(3, ',');
            let id = cols.next()?.trim();
            let _url = cols.next()?;
            let title = cols.next()?.trim();
            if !(id.starts_with("UC") && id.len() == 24) {
                return None;
            }
            // Titles with commas or quotes are quoted, with quotes doubled.
            let title = match title.strip_prefix('"').and_then(|t| t.strip_suffix('"')) {
                Some(inner) => inner.replace("\"\"", "\""),
                None => title.to_string(),
            };
            Some(Channel {
                id: id.to_string(),
                name: title,
                ..Default::default()
            })
        })
        .collect()
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Imported {
    found: usize,
    added: usize,
}

#[tauri::command]
pub fn import_subscriptions(app: AppHandle, state: State<'_, AppState>, csv: String) -> CmdResult<Imported> {
    let channels = parse_takeout(&csv);
    if channels.is_empty() {
        return Err("No channels found. Pick the subscriptions.csv file from your Google Takeout download.".into());
    }
    let added = db::import_subscriptions(&mut state.db.lock().unwrap(), &channels).map_err(err)?;
    spawn_complete_subscriptions(app);
    Ok(Imported { found: channels.len(), added })
}

/// Looks up avatars (and current names) of channels added without them,
/// then checks their feeds. Emits "subscriptions-changed" and "feed-changed".
pub fn spawn_complete_subscriptions(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let state = app.state::<AppState>();
        let Ok(ids) = db::incomplete_subscriptions(&state.db.lock().unwrap()) else { return };
        if ids.is_empty() {
            return;
        }
        let http = &state.http;
        let found: Vec<Channel> = futures_util::stream::iter(ids.iter().cloned())
            .map(|id| async move { youtube::channel(http, &id).await.ok().map(|p| p.channel) })
            .buffer_unordered(6)
            .filter_map(|c| async move { c })
            .collect()
            .await;
        {
            let conn = state.db.lock().unwrap();
            for c in &found {
                let _ = db::subscribe(&conn, c);
            }
        }
        let _ = app.emit("subscriptions-changed", ());
        let (entries, _) = feeds::fetch_all(http, &ids).await;
        if db::save_feed(&mut state.db.lock().unwrap(), &entries).is_ok() {
            let _ = app.emit("feed-changed", ());
        }
    });
}

#[tauri::command]
pub fn history(state: State<'_, AppState>, add: Option<Video>) -> CmdResult<Vec<db::Watched>> {
    let conn = state.db.lock().unwrap();
    if let Some(v) = add {
        db::history_add(&conn, &v).map_err(err)?;
    }
    db::history(&conn, 500).map_err(err)
}

#[tauri::command]
pub fn history_progress(state: State<'_, AppState>, id: String, position: f64, duration: Option<f64>) -> CmdResult<()> {
    db::history_progress(&state.db.lock().unwrap(), &id, position, duration).map_err(err)
}

/// Forgets one video, or all of them when `id` is missing.
#[tauri::command]
pub fn history_remove(state: State<'_, AppState>, id: Option<String>) -> CmdResult<()> {
    db::history_remove(&state.db.lock().unwrap(), id.as_deref()).map_err(err)
}

#[derive(Serialize)]
pub struct Position {
    position: f64,
    duration: Option<f64>,
}

/// Where each of `ids` was left off.
#[tauri::command]
pub fn positions(state: State<'_, AppState>, ids: Vec<String>) -> CmdResult<HashMap<String, Position>> {
    Ok(db::positions(&state.db.lock().unwrap(), &ids)
        .map_err(err)?
        .into_iter()
        .map(|(id, position, duration)| (id, Position { position, duration }))
        .collect())
}

/// Videos and channels kept out of For you, topics and autoplay.
#[tauri::command]
pub fn hidden(state: State<'_, AppState>) -> CmdResult<Vec<db::Hidden>> {
    db::hidden(&state.db.lock().unwrap()).map_err(err)
}

#[tauri::command]
pub fn hide(state: State<'_, AppState>, item: db::Hidden, on: bool) -> CmdResult<()> {
    let conn = state.db.lock().unwrap();
    if on { db::hide(&conn, &item) } else { db::unhide(&conn, &item.id) }.map_err(err)
}

#[tauri::command]
pub fn later(state: State<'_, AppState>) -> CmdResult<Vec<Video>> {
    db::later(&state.db.lock().unwrap()).map_err(err)
}

#[tauri::command]
pub fn later_set(state: State<'_, AppState>, video: Video, saved: bool) -> CmdResult<()> {
    db::later_set(&state.db.lock().unwrap(), &video, saved).map_err(err)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_takeout() {
        let csv = "\u{feff}Channel Id,Channel Url,Channel Title\n\
            UCHnyfMqiRRG1u-2MsSQLbXA,http://www.youtube.com/channel/UCHnyfMqiRRG1u-2MsSQLbXA,Veritasium\n\
            UCsXVk37bltHxD1rDPwtNM8Q,http://www.youtube.com/channel/UCsXVk37bltHxD1rDPwtNM8Q,\"Kurzgesagt, \"\"In a Nutshell\"\"\"\n\
            \n";
        let c = parse_takeout(csv);
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].name, "Veritasium");
        assert_eq!(c[1].name, "Kurzgesagt, \"In a Nutshell\"");
    }

    #[test]
    fn makes_video_links() {
        assert_eq!(video_link("dQw4w9WgXcQ", None).as_deref(), Some("https://youtu.be/dQw4w9WgXcQ"));
        assert_eq!(video_link("dQw4w9WgXcQ", Some(75)).as_deref(), Some("https://youtu.be/dQw4w9WgXcQ?t=75"));
        assert_eq!(video_link("dQw4w9WgXcQ", Some(0)).as_deref(), Some("https://youtu.be/dQw4w9WgXcQ"));
        assert!(video_link("not a video", None).is_none());
    }
}
