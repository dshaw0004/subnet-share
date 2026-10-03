use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};

/// Persisted subset of config (what survives app restarts).
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
struct Persisted {
    #[serde(default)]
    device_name: Option<String>,
    #[serde(default)]
    muted: bool,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub device_name: String,
    pub default_port: u16,
    pub fallback_range: std::ops::RangeInclusive<u16>,
    pub save_dir: PathBuf,
    /// Path where Persisted is read/written.
    pub config_path: PathBuf,
    /// Muted state as loaded from disk at startup.
    pub muted: bool,
}

impl Default for Config {
    fn default() -> Self {
        let config_path = config_file_path();
        let persisted = load_persisted(&config_path);
        Self {
            device_name: persisted
                .device_name
                .unwrap_or_else(|| default_device_name()),
            default_port: 48080,
            fallback_range: 48081..=48099,
            save_dir: default_save_dir(),
            config_path,
            muted: persisted.muted,
        }
    }
}

impl Config {
    /// Persist device_name and muted to disk (best-effort; errors are silently ignored).
    pub fn save(&self, muted: bool) {
        let p = Persisted { device_name: Some(self.device_name.clone()), muted };
        if let Ok(json) = serde_json::to_string_pretty(&p) {
            if let Some(parent) = self.config_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(&self.config_path, json);
        }
    }
}

// ---------- helpers ----------

fn load_persisted(path: &Path) -> Persisted {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn config_file_path() -> PathBuf {
    // $HOME/.config/subnet-share/config.json on Linux
    // %APPDATA%\subnet-share\config.json on Windows
    #[cfg(target_os = "windows")]
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| ".".into());
    #[cfg(not(target_os = "windows"))]
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map(|h| PathBuf::from(h).join(".config"))
                .unwrap_or_else(|| ".".into())
        });
    base.join("subnet-share").join("config.json")
}

fn default_device_name() -> String {
    hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_else(|| "lan-device".into())
}

fn default_save_dir() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(|h| PathBuf::from(h).join("Downloads"))
        .unwrap_or_else(|| ".".into())
}
