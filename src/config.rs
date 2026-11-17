//! Application configuration: XDG paths, atomic save, recent projects.

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

/// Maximum number of remembered projects.
pub const MAX_RECENT: usize = 8;
/// Default web remote port.
pub const DEFAULT_REMOTE_PORT: u16 = 8720;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    #[default]
    System,
    En,
    Tr,
}

impl Language {
    pub const ALL: [Language; 3] = [Language::System, Language::En, Language::Tr];

    pub fn id(self) -> &'static str {
        match self {
            Language::System => "system",
            Language::En => "en",
            Language::Tr => "tr",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "system" => Some(Language::System),
            "en" => Some(Language::En),
            "tr" => Some(Language::Tr),
            _ => None,
        }
    }

    /// Catalogue key for the dropdown label.
    pub fn label_key(self) -> &'static str {
        match self {
            Language::System => "language.system",
            Language::En => "language.english",
            Language::Tr => "language.turkish",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    #[default]
    Dark,
    Light,
    System,
}

impl Theme {
    pub fn id(self) -> &'static str {
        match self {
            Theme::Dark => "dark",
            Theme::Light => "light",
            Theme::System => "system",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "dark" => Some(Theme::Dark),
            "light" => Some(Theme::Light),
            "system" => Some(Theme::System),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CaptureBackend {
    #[default]
    Auto,
    X11,
    Portal,
}

impl CaptureBackend {
    pub fn id(self) -> &'static str {
        match self {
            CaptureBackend::Auto => "auto",
            CaptureBackend::X11 => "x11",
            CaptureBackend::Portal => "portal",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "auto" => Some(CaptureBackend::Auto),
            "x11" => Some(CaptureBackend::X11),
            "portal" => Some(CaptureBackend::Portal),
            _ => None,
        }
    }
}

/// Stable monitor reference: connector first, then unique model, then index.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct MonitorRef {
    pub connector: Option<String>,
    pub model: String,
    pub index: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct OutputRole {
    pub monitor: Option<MonitorRef>,
    pub fullscreen: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Outputs {
    pub program: OutputRole,
    pub preview: OutputRole,
    pub prompter: OutputRole,
}

/// Sound played when a name is called.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CallSound {
    pub enabled: bool,
    /// `None` plays the bundled ding-dong; a path plays that file instead.
    pub path: Option<PathBuf>,
}

impl Default for CallSound {
    fn default() -> Self {
        Self {
            enabled: true,
            path: None,
        }
    }
}

/// Features that are still being validated with real shows.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Experimental {
    /// "Random pick" button on the queue page.
    pub random_pick: bool,
    /// Write the called status back into the linked `.ods` file.
    pub write_back: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RemoteConfig {
    pub enabled: bool,
    pub port: u16,
    pub token: String,
}

impl Default for RemoteConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            port: DEFAULT_REMOTE_PORT,
            token: String::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub language: Language,
    pub theme: Theme,
    pub last_project: Option<PathBuf>,
    pub recent_projects: Vec<PathBuf>,
    pub operator_monitor: Option<MonitorRef>,
    pub outputs: Outputs,
    /// Force every output role into a 960x540 window.
    pub windowed_outputs: bool,
    pub audio_sink: Option<String>,
    pub capture_backend: CaptureBackend,
    pub remote: RemoteConfig,
    /// Chime played when a queue name is called.
    pub call_sound: CallSound,
    /// Seconds the called name keeps the breathing glow (0 disables it).
    pub call_glow_secs: f64,
    /// Off by default; toggled in Preferences → Experimental.
    pub experimental: Experimental,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            language: Language::System,
            theme: Theme::Dark,
            last_project: None,
            recent_projects: Vec::new(),
            operator_monitor: None,
            outputs: Outputs::default(),
            windowed_outputs: false,
            audio_sink: None,
            capture_backend: CaptureBackend::Auto,
            remote: RemoteConfig::default(),
            call_sound: CallSound::default(),
            call_glow_secs: 5.0,
            experimental: Experimental::default(),
        }
    }
}

fn xdg_dir(var: &str, fallback: &str) -> PathBuf {
    match std::env::var_os(var) {
        Some(value) if !value.is_empty() => PathBuf::from(value),
        _ => match dirs::home_dir() {
            Some(home) => home.join(fallback),
            None => PathBuf::from(fallback),
        },
    }
}

/// `${XDG_CONFIG_HOME:-~/.config}/signifiage`.
pub fn config_dir() -> PathBuf {
    xdg_dir("XDG_CONFIG_HOME", ".config").join("signifiage")
}

/// `${XDG_STATE_HOME:-~/.local/state}/signifiage`.
pub fn state_dir() -> PathBuf {
    xdg_dir("XDG_STATE_HOME", ".local/state").join("signifiage")
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.json")
}

pub fn log_path() -> PathBuf {
    state_dir().join("log.txt")
}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Write `contents` to `path` atomically with mode 0600.
pub fn write_private_atomic(path: &Path, contents: &str) -> anyhow::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("path has no parent: {}", path.display()))?;
    fs::create_dir_all(dir)?;
    let tmp = path.with_extension("json.tmp");
    {
        let mut file = fs::File::create(&tmp)?;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
        file.write_all(contents.as_bytes())?;
        file.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    if let Ok(dir_handle) = fs::File::open(dir) {
        let _ = dir_handle.sync_all();
    }
    Ok(())
}

impl Config {
    /// Load configuration, falling back to defaults and quarantining bad files.
    pub fn load() -> Config {
        let path = config_path();
        let raw = match fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(err) => {
                if err.kind() != std::io::ErrorKind::NotFound {
                    warn!("cannot read {}: {err}", path.display());
                }
                return Config::default();
            }
        };
        match serde_json::from_str::<Config>(&raw) {
            Ok(mut config) => {
                config.prune_recent();
                config
            }
            Err(err) => {
                let stamp = unix_timestamp();
                let quarantined = config_dir().join(format!("config.json.invalid-{stamp}"));
                warn!(
                    "invalid config {}: {err}; moving to {}",
                    path.display(),
                    quarantined.display()
                );
                if let Err(move_err) = fs::rename(&path, &quarantined) {
                    warn!("cannot quarantine config: {move_err}");
                }
                Config::default()
            }
        }
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let path = config_path();
        let body = serde_json::to_string_pretty(self)?;
        write_private_atomic(&path, &body)?;
        info!("saved config {}", path.display());
        Ok(())
    }

    /// Drop recent entries whose directory disappeared.
    pub fn prune_recent(&mut self) {
        self.recent_projects.retain(|p| p.is_dir());
        self.recent_projects.truncate(MAX_RECENT);
        if let Some(last) = &self.last_project
            && !last.is_dir()
        {
            self.last_project = None;
        }
    }

    /// Record a project as most recent, deduplicated and capped.
    pub fn push_recent(&mut self, project: &Path) {
        let project = project
            .canonicalize()
            .unwrap_or_else(|_| project.to_path_buf());
        self.recent_projects.retain(|p| p != &project);
        self.recent_projects.insert(0, project.clone());
        self.recent_projects.truncate(MAX_RECENT);
        self.last_project = Some(project);
    }

    /// Ensure the remote has a token; generate and persist one when missing.
    pub fn ensure_remote_token(&mut self) -> String {
        if self.remote.token.trim().is_empty() {
            self.remote.token = generate_token();
            if let Err(err) = self.save() {
                warn!("cannot persist generated remote token: {err}");
            }
        }
        self.remote.token.clone()
    }
}

/// 24 random alphanumeric characters, used as the web remote token.
pub fn generate_token() -> String {
    use rand::Rng;
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut rng = rand::rng();
    (0..24)
        .map(|_| ALPHABET[rng.random_range(0..ALPHABET.len())] as char)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sane() {
        let config = Config::default();
        assert_eq!(config.language, Language::System);
        assert_eq!(config.theme, Theme::Dark);
        assert_eq!(config.remote.port, 8720);
        assert!(!config.remote.enabled);
        assert!(!config.windowed_outputs);
    }

    #[test]
    fn language_round_trips_and_keeps_english() {
        for language in Language::ALL {
            assert_eq!(Language::from_id(language.id()), Some(language));
        }
        assert!(Language::ALL.contains(&Language::En));
        assert_eq!(Language::from_id("de"), None);
    }

    #[test]
    fn token_is_24_alphanumeric() {
        let token = generate_token();
        assert_eq!(token.len(), 24);
        assert!(token.chars().all(|c| c.is_ascii_alphanumeric()));
        assert_ne!(token, generate_token());
    }

    #[test]
    fn partial_json_uses_field_defaults() {
        let config: Config = serde_json::from_str(r#"{"theme":"light"}"#).unwrap();
        assert_eq!(config.theme, Theme::Light);
        assert_eq!(config.remote.port, 8720);
        let remote: RemoteConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(remote.port, DEFAULT_REMOTE_PORT);
    }

    #[test]
    fn recent_projects_dedupe_and_cap() {
        let mut config = Config::default();
        let dir = std::env::temp_dir();
        config.push_recent(&dir);
        config.push_recent(&dir);
        assert_eq!(config.recent_projects.len(), 1);
        assert_eq!(config.last_project, Some(dir.canonicalize().unwrap()));
    }
}
