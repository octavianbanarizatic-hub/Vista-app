//! yt-dlp on desktop: the official executable, which Vista downloads into
//! its data folder and keeps up to date, or one already on PATH.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use futures_util::StreamExt;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::process::Command;
use tokio::sync::{Mutex, OnceCell};

use super::{Status, UPDATE_INTERVAL};

const RELEASE_BASE: &str = "https://github.com/yt-dlp/yt-dlp/releases/latest/download";
const LATEST_RELEASE: &str = "https://github.com/yt-dlp/yt-dlp/releases/latest";

pub struct YtDlp {
    bin_dir: PathBuf,
    // Extra arguments picked once per run (which JavaScript runtime to use).
    runtime_args: OnceCell<Vec<String>>,
    install_lock: Mutex<()>,
}

/// Deno is what yt-dlp looks for by default; Node works when told to use it.
fn js_runtime() -> Option<&'static str> {
    if which("deno").is_some() {
        Some("deno")
    } else if which("node").is_some() {
        Some("node")
    } else {
        None
    }
}

fn exe_name() -> &'static str {
    if cfg!(windows) {
        "yt-dlp.exe"
    } else {
        "yt-dlp"
    }
}

fn release_asset() -> &'static str {
    if cfg!(windows) {
        "yt-dlp.exe"
    } else if cfg!(target_os = "macos") {
        "yt-dlp_macos"
    } else if cfg!(target_arch = "aarch64") {
        "yt-dlp_linux_aarch64"
    } else {
        "yt-dlp_linux"
    }
}

/// Finds an executable on PATH.
fn which(name: &str) -> Option<PathBuf> {
    let exe = if cfg!(windows) && !name.ends_with(".exe") {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(&exe))
        .find(|p| p.is_file())
}

fn command(bin: &Path) -> Command {
    let mut cmd = Command::new(bin);
    cmd.kill_on_drop(true);
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

impl YtDlp {
    pub fn new(bin_dir: PathBuf) -> Self {
        Self {
            bin_dir,
            runtime_args: OnceCell::new(),
            install_lock: Mutex::new(()),
        }
    }

    fn managed_path(&self) -> PathBuf {
        self.bin_dir.join(exe_name())
    }

    /// Prefers the copy Vista installed (which it keeps up to date), then PATH.
    fn find(&self) -> Option<(PathBuf, bool)> {
        let managed = self.managed_path();
        if managed.is_file() {
            return Some((managed, true));
        }
        which("yt-dlp").map(|p| (p, false))
    }

    async fn version_of(bin: &Path) -> Option<String> {
        let out = command(bin).arg("--version").output().await.ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    pub async fn status(&self) -> Status {
        match self.find() {
            Some((path, managed)) => {
                let version = Self::version_of(&path).await;
                Status {
                    installed: version.is_some(),
                    managed,
                    version,
                    path: Some(path.to_string_lossy().into_owned()),
                    js_runtime: js_runtime(),
                }
            }
            None => Status {
                installed: false,
                managed: false,
                version: None,
                path: None,
                js_runtime: js_runtime(),
            },
        }
    }

    /// Whether the managed copy has gone a week without an update check.
    pub fn update_due(&self) -> bool {
        std::fs::metadata(self.managed_path())
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| SystemTime::now().duration_since(t).ok())
            .is_some_and(|age| age > UPDATE_INTERVAL)
    }

    /// Updates the managed copy if a newer release exists. Returns the new
    /// version, or `None` when it was already current.
    pub async fn update(&self, http: &reqwest::Client) -> Result<Option<String>, String> {
        let current = Self::version_of(&self.managed_path()).await;
        // The "latest" page redirects to /releases/tag/<version>.
        let resp = http
            .head(LATEST_RELEASE)
            .send()
            .await
            .map_err(|e| format!("could not check for updates: {e}"))?;
        let latest = resp.url().path().rsplit('/').next().unwrap_or("").to_string();
        if !latest.is_empty() && current.as_deref() == Some(latest.as_str()) {
            // Up to date: restart the weekly clock without downloading.
            if let Ok(f) = std::fs::File::options().write(true).open(self.managed_path()) {
                let _ = f.set_modified(SystemTime::now());
            }
            return Ok(None);
        }
        self.install(http, |_, _| {}).await.map(Some)
    }

    /// YouTube now needs a JavaScript runtime to decode stream URLs. yt-dlp
    /// looks for Deno by default; if only Node is installed, point it there.
    async fn runtime_args(&self, bin: &Path) -> Vec<String> {
        self.runtime_args
            .get_or_init(|| async {
                if js_runtime() != Some("node") {
                    return Vec::new();
                }
                // Releases before 2025.11 do not know the flag.
                match Self::version_of(bin).await {
                    Some(v) if v.as_str() >= "2025.11" => {
                        vec!["--js-runtimes".into(), "node".into()]
                    }
                    _ => Vec::new(),
                }
            })
            .await
            .clone()
    }

    pub(super) async fn run_json(&self, args: &[&str]) -> Result<Value, String> {
        let (bin, _) = self.find().ok_or("yt-dlp is not installed")?;
        let extra = self.runtime_args(&bin).await;
        let out = command(&bin)
            .args(&extra)
            .args(super::BASE_ARGS)
            .args(args)
            .output()
            .await
            .map_err(|e| format!("could not run yt-dlp: {e}"))?;
        super::parse_output(out.status.success(), &out.stdout, &String::from_utf8_lossy(&out.stderr))
    }

    /// Downloads the latest official release and checks it against the
    /// published SHA-256 sums before putting it in place.
    pub async fn install(
        &self,
        http: &reqwest::Client,
        mut progress: impl FnMut(u64, Option<u64>),
    ) -> Result<String, String> {
        // A second click or the weekly update must not race the first download.
        let _guard = self.install_lock.lock().await;
        let asset = release_asset();
        let sums = http
            .get(format!("{RELEASE_BASE}/SHA2-256SUMS"))
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .map_err(|e| format!("could not fetch checksums: {e}"))?
            .text()
            .await
            .map_err(|e| e.to_string())?;
        let expected = sums
            .lines()
            .find_map(|l| {
                let (hash, name) = l.split_once(char::is_whitespace)?;
                (name.trim() == asset).then(|| hash.to_ascii_lowercase())
            })
            .ok_or("no checksum published for this platform")?;

        let resp = http
            .get(format!("{RELEASE_BASE}/{asset}"))
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .map_err(|e| format!("download failed: {e}"))?;
        let total = resp.content_length();
        let mut hasher = Sha256::new();
        let mut bytes = Vec::with_capacity(total.unwrap_or(0) as usize);
        let mut body = resp.bytes_stream();
        while let Some(chunk) = body.next().await {
            let chunk = chunk.map_err(|e| format!("download failed: {e}"))?;
            hasher.update(&chunk);
            bytes.extend_from_slice(&chunk);
            progress(bytes.len() as u64, total);
        }
        let actual = format!("{:x}", hasher.finalize());
        if actual != expected {
            return Err("checksum mismatch, download discarded".into());
        }

        tokio::fs::create_dir_all(&self.bin_dir)
            .await
            .map_err(|e| e.to_string())?;
        let target = self.managed_path();
        let tmp = target.with_extension("download");
        tokio::fs::write(&tmp, &bytes)
            .await
            .map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755));
        }
        tokio::fs::rename(&tmp, &target)
            .await
            .map_err(|e| e.to_string())?;
        Self::version_of(&target)
            .await
            .ok_or_else(|| "installed yt-dlp does not run".to_string())
    }
}
