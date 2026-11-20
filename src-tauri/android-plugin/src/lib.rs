//! Vista's Android-only native parts, written in Kotlin (see `android/`).
//!
//! - yt-dlp: a Python program that Android can't run as-is. The Kotlin side
//!   uses youtubedl-android, which ships Python, yt-dlp and QuickJS (the
//!   JavaScript runtime YouTube now needs) inside the app.
//! - The video player: ExoPlayer in a native view laid over the top of the
//!   page, with fullscreen, picture-in-picture, background play and lock
//!   screen controls. The page hears about it through `window.__vistaPlayer`.
//! - The back button, which goes to the page first.
//! - New video notifications: WorkManager checks your subscriptions' feeds
//!   in the background, with Vista closed too.

use serde::de::IgnoredAny;
use serde::{Deserialize, Serialize};
use tauri::plugin::{Builder, PluginHandle, TauriPlugin};
use tauri::{Manager, Runtime};

pub struct VistaAndroid<R: Runtime>(PluginHandle<R>);

/// What one yt-dlp run printed.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Output {
    pub exit_code: i32,
    pub out: String,
    pub err: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Version {
    pub version: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Update {
    /// False when the bundled copy was already the latest release.
    pub updated: bool,
    pub version: Option<String>,
}

#[derive(Serialize)]
struct Args<'a> {
    args: &'a [String],
    /// The worker script (`YtdlpWorker.kt`), when it should be used.
    worker: Option<&'a str>,
}

#[derive(Serialize)]
struct Watch<'a> {
    json: &'a str,
    on: bool,
}

#[derive(Deserialize)]
struct Allowed {
    allowed: bool,
}

#[derive(Serialize)]
struct PathArgs<'a> {
    path: &'a str,
}

#[derive(Deserialize)]
struct Opened {
    opened: bool,
}

#[derive(Deserialize)]
struct Network {
    unmetered: bool,
}

#[derive(Serialize)]
struct ShareArgs<'a> {
    text: &'a str,
}

#[derive(Serialize)]
struct BackupArgs<'a> {
    path: &'a str,
    name: &'a str,
    keep: u32,
}

#[derive(Deserialize)]
struct Place {
    place: String,
}

#[derive(Serialize)]
struct Command<'a> {
    command: &'a str,
    value: Option<f64>,
}

/// These calls block until the Kotlin side answers; yt-dlp runs take
/// seconds, so run them off the async runtime.
impl<R: Runtime> VistaAndroid<R> {
    pub fn ytdlp(&self, args: &[String], worker: Option<&str>) -> Result<Output, String> {
        self.0.run_mobile_plugin("ytdlp", Args { args, worker }).map_err(|e| e.to_string())
    }

    pub fn ytdlp_version(&self) -> Result<Option<String>, String> {
        self.0
            .run_mobile_plugin::<Version>("ytdlpVersion", ())
            .map(|v| v.version)
            .map_err(|e| e.to_string())
    }

    pub fn ytdlp_update(&self) -> Result<Update, String> {
        self.0.run_mobile_plugin("ytdlpUpdate", ()).map_err(|e| e.to_string())
    }

    /// Starts a video; `args` is the app's `player::LoadArgs`.
    pub fn player_load(&self, args: &impl Serialize) -> Result<(), String> {
        self.0
            .run_mobile_plugin::<IgnoredAny>("playerLoad", args)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    pub fn player_command(&self, command: &str, value: Option<f64>) -> Result<(), String> {
        self.0
            .run_mobile_plugin::<IgnoredAny>("playerCommand", Command { command, value })
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// Your subscriptions (`{ channels, shorts }` as JSON) to check for new
    /// videos in the background, or with `on` false, stop checking.
    pub fn watch_uploads(&self, json: &str, on: bool) -> Result<(), String> {
        self.0
            .run_mobile_plugin::<IgnoredAny>("watchUploads", Watch { json, on })
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// Whether the connection is Wi-Fi (or another that isn't metered).
    pub fn unmetered(&self) -> Result<bool, String> {
        self.0
            .run_mobile_plugin::<Network>("network", ())
            .map(|n| n.unmetered)
            .map_err(|e| e.to_string())
    }

    /// Opens Android's installer for a downloaded update. False when Android
    /// first wants Vista allowed to install apps (its settings screen opens).
    pub fn install_apk(&self, path: &str) -> Result<bool, String> {
        self.0
            .run_mobile_plugin::<Opened>("installApk", PathArgs { path })
            .map(|o| o.opened)
            .map_err(|e| e.to_string())
    }

    /// Opens Android's share sheet with `text`.
    pub fn share(&self, text: &str) -> Result<(), String> {
        self.0
            .run_mobile_plugin::<IgnoredAny>("share", ShareArgs { text })
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// Saves the backup file at `path` in Downloads → Vista backups as
    /// `name`; with `keep` above 0, only the newest `keep` automatic ones stay.
    /// Returns where it went, for the page to say.
    pub fn save_backup(&self, path: &str, name: &str, keep: u32) -> Result<String, String> {
        self.0
            .run_mobile_plugin::<Place>("saveBackup", BackupArgs { path, name, keep })
            .map(|p| p.place)
            .map_err(|e| e.to_string())
    }

    /// Whether Vista may notify. Android 13 and newer ask the first time, so
    /// this waits for the answer.
    pub fn allow_notifications(&self) -> Result<bool, String> {
        self.0
            .run_mobile_plugin::<Allowed>("allowNotifications", ())
            .map(|a| a.allowed)
            .map_err(|e| e.to_string())
    }
}

pub trait VistaAndroidExt<R: Runtime> {
    fn vista_android(&self) -> &VistaAndroid<R>;
}

impl<R: Runtime, T: Manager<R>> VistaAndroidExt<R> for T {
    fn vista_android(&self) -> &VistaAndroid<R> {
        self.state::<VistaAndroid<R>>().inner()
    }
}

pub fn init<R: Runtime>() -> TauriPlugin<R> {
    Builder::new("vista-android")
        .setup(|app, api| {
            let handle = api.register_android_plugin("dev.vista.android", "VistaPlugin")?;
            app.manage(VistaAndroid(handle));
            Ok(())
        })
        .build()
}
