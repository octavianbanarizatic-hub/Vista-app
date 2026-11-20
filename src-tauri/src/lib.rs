mod backup;
mod commands;
mod db;
mod downloads;
mod extras;
mod feeds;
mod media;
mod player;
mod recommend;
mod updater;
mod youtube;
mod ytdlp;

use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{Emitter, Manager};

/// A video as lists show it: search results, the feed, related videos.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Video {
    pub id: String,
    pub title: String,
    pub channel: String,
    #[serde(default)]
    pub channel_id: Option<String>,
    #[serde(default)]
    pub duration: Option<f64>,
    /// As YouTube writes it: "5.8M views".
    #[serde(default)]
    pub views: Option<String>,
    /// As YouTube writes it: "3 days ago". The feed has `published_at` instead.
    #[serde(default)]
    pub published: Option<String>,
    /// Seconds since 1970.
    #[serde(default)]
    pub published_at: Option<i64>,
    pub thumbnail: String,
    #[serde(default)]
    pub avatar: Option<String>,
    #[serde(default)]
    pub live: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Channel {
    /// "UC…"
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub avatar: Option<String>,
    #[serde(default)]
    pub handle: Option<String>,
    /// "21.4M subscribers"
    #[serde(default)]
    pub subscribers: Option<String>,
}

pub struct AppState {
    pub db: Mutex<rusqlite::Connection>,
    pub http: reqwest::Client,
    pub ytdlp: ytdlp::YtDlp,
    pub streams: media::StreamCache,
    pub image_cache: media::ImageCache,
    /// One feed check at a time.
    pub feed_lock: tokio::sync::Mutex<()>,
    /// One recommendations refresh at a time.
    pub recs_lock: tokio::sync::Mutex<()>,
    /// The video most recently asked to play; a slower, earlier request must not replace it.
    pub playing: Mutex<String>,
    /// Caps background yt-dlp runs that prepare videos before they're tapped.
    pub prefetch_slots: tokio::sync::Semaphore,
    pub downloads: downloads::Downloads,
}

pub type CmdResult<T> = Result<T, String>;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default();
    #[cfg(desktop)]
    let builder = builder
        // Must come first: a second launch focuses the running window instead.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.unminimize();
                let _ = w.set_focus();
            }
        }))
        .plugin(tauri_plugin_window_state::Builder::default().build());
    #[cfg(target_os = "android")]
    let builder = builder.plugin(tauri_plugin_vista_android::init());
    builder
        .register_asynchronous_uri_scheme_protocol("media", |ctx, request, responder| {
            media::handle(ctx.app_handle().clone(), request, responder);
        })
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir)?;
            let conn = db::open(&data_dir.join("vista.db"))?;
            let pending = db::downloads(&conn)?
                .into_iter()
                .filter(|d| !d.done && d.error.is_none())
                .map(|d| d.video.id)
                .collect();
            let http = reqwest::Client::builder()
                .user_agent(youtube::BROWSER_UA)
                .connect_timeout(Duration::from_secs(10))
                .read_timeout(Duration::from_secs(20))
                .build()?;
            app.manage(AppState {
                db: Mutex::new(conn),
                http,
                #[cfg(desktop)]
                ytdlp: ytdlp::YtDlp::new(data_dir.join("bin")),
                #[cfg(target_os = "android")]
                ytdlp: ytdlp::YtDlp::new(app.handle().clone(), data_dir.join("bin")),
                streams: media::StreamCache::default(),
                image_cache: media::ImageCache::new(app.path().app_cache_dir()?.join("avatars")),
                feed_lock: tokio::sync::Mutex::new(()),
                recs_lock: tokio::sync::Mutex::new(()),
                playing: Mutex::new(String::new()),
                prefetch_slots: tokio::sync::Semaphore::new(1),
                downloads: downloads::Downloads::new(data_dir.join("downloads"), pending),
            });

            // Keep the managed yt-dlp current; YouTube breaks old releases.
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let state = handle.state::<AppState>();
                if !state.ytdlp.update_due() {
                    return;
                }
                match state.ytdlp.update(&state.http).await {
                    Ok(Some(version)) => {
                        let _ = handle.emit("ytdlp-updated", version);
                    }
                    Ok(None) => {}
                    Err(e) => eprintln!("yt-dlp update failed: {e}"),
                }
            });
            tauri::async_runtime::spawn(downloads::Downloads::run(app.handle().clone()));
            let handle = app.handle().clone();
            tauri::async_runtime::spawn_blocking(move || handle.state::<AppState>().image_cache.trim());
            // Imported subscriptions arrive without avatars; fetch them.
            commands::spawn_complete_subscriptions(app.handle().clone());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::ytdlp_status,
            commands::ytdlp_install,
            commands::search,
            commands::watch_info,
            commands::more,
            commands::channel,
            commands::play,
            commands::prefetch,
            commands::player_command,
            commands::uploads_watch,
            commands::share,
            commands::app_update_check,
            commands::app_update_install,
            commands::unmetered,
            commands::backup_save,
            commands::backup_stage,
            commands::backup_peek,
            commands::backup_restore,
            commands::notifications_allow,
            commands::votes,
            commands::comments,
            commands::playlist,
            commands::playlist_save,
            commands::saved_playlists,
            commands::resolve_link,
            commands::downloads_list,
            commands::download_add,
            commands::download_remove,
            commands::feed,
            commands::recommendations,
            commands::topic,
            commands::search_suggestions,
            commands::searches,
            commands::search_remove,
            commands::searches_clear,
            commands::subscriptions,
            commands::subscribe,
            commands::import_subscriptions,
            commands::history,
            commands::history_progress,
            commands::history_remove,
            commands::positions,
            commands::hidden,
            commands::hide,
            commands::later,
            commands::later_set,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Vista");
}
