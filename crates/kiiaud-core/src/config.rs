// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::error::{KiiaudError, Result};
use crate::format::DsdMode;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Template configuration file with rich explanatory comments.
pub const DEFAULT_CONFIG_TEMPLATE: &str = r#"# ==============================================================================
# kiiaud — Bit-Perfect Network Audio Playback Daemon Configuration
# ==============================================================================
# Documentation: docs/CONFIGURATION.md
# Hierarchy: /etc/kiiaud/config.toml (system) < ~/.config/kiiaud/config.toml (user)

[daemon]
# Friendly network endpoint identifier displayed in logs and service discovery.
name = "kiiaud-endpoint"

# Unix domain socket path used for local CLI IPC control (`kiiaudctl`).
# Recommendation: For per-user daemon, use an isolated path like "/tmp/kiiaud.sock"
# or "$XDG_RUNTIME_DIR/kiiaud.sock". For system daemon, use "/run/kiiaud/kiiaud.sock".
socket_path = "/tmp/kiiaud.sock"

# Log verbosity level: "trace", "debug", "info", "warn", "error".
log_level = "info"

[output]
# Audio output backend:
#   - "alsa": Linux direct ALSA hardware transport (recommended for bit-perfect Hi-Fi)
#   - "wasapi": Windows Exclusive WASAPI transport
#   - "android" / "oboe": Android direct AudioTrack / AAudio
#   - "mock" / "null": Headless dummy output for testing and CI
backend = "alsa"

# Audio output device identifier.
# On Linux ALSA:
#   - Use direct hardware card syntax to bypass PulseAudio / PipeWire software mixing:
#     e.g., "hw:CARD=Audio,DEV=0" (card by name) or "hw:0,0" (card by index).
#   - Run `aplay -l` to discover your audio card and subdevice names.
# On Windows WASAPI:
#   - Device name substring (e.g. "DAC") or "default" for system primary device.
device = "hw:CARD=Audio,DEV=0"

# Enforce bit-perfect transport:
#   - When true: Disables software resampling, format conversions, and software volume DSP.
#   - The DAC is always configured at the exact source sample rate and bit depth.
bit_perfect = true

# DSD (Direct Stream Digital) playback transport mode:
#   - "auto": Probes Native DSD (DSD_U32_BE / DSD_U32_LE). If unsupported by ALSA DAC,
#             automatically falls back to DoP (DSD over PCM). Never converts to PCM silently.
#   - "native": Strict Native DSD only. Fails with error if DAC lacks native DSD support.
#   - "dop": Strict DoP framing inside 24/32-bit PCM frames.
dsd_mode = "auto"

# Hardware ring buffer size in milliseconds (default: 200ms).
buffer_time_ms = 200

# ALSA period time (hardware interrupt interval) in milliseconds (default: 40ms).
period_time_ms = 40

# Optional hardware mixer control name for analog volume adjustment (e.g., "PCM" or "Master").
# Leave commented out or unset to keep hardware mixer untouched for purest 100% bit-perfect signal.
# hardware_mixer = "PCM"

[security]
# Allowed filesystem directory roots for local playback via CLI/WebUI.
# Example: allowed_roots = ["/media/music", "/home/music/DSD"]
# Leave empty (`[]`) to allow any non-system regular file on the system.
allowed_roots = []

# ------------------------------------------------------------------------------
# Protocol Adapters
# ------------------------------------------------------------------------------

[adapter.web]
# Embedded lightweight WebUI & HTTP REST/SSE API for browser control.
enabled = true
listen = "0.0.0.0:7110"

[adapter.mpd]
# Music Player Daemon (MPD) protocol emulation server.
# Allows controlling kiiaud from any standard MPD client (ncmpcpp, Cantata, MAFA, etc.).
enabled = true
listen = "0.0.0.0:6600"

[adapter.jellyfin]
# Jellyfin "Play To" / Remote Control Endpoint (Cast target).
# When enabled, kiiaud authenticates with your Jellyfin server via WebSocket and appears
# directly in the "Cast / Play On" device menu in Jellyfin Web & Mobile apps.
enabled = false
server_url = "http://jellyfin.local:8096"

# Authentication: You can use an API Key OR a Username + Password.
# api_key = "your_jellyfin_api_key_here"
# username = "your_username"
# password = "your_password"

# Identifier and display name shown in Jellyfin client UI:
device_id = "kiiaud-endpoint-01"
device_name = "kiiaud Hi-Fi Endpoint"

[adapter.lms]
# Logitech Media Server (LMS / Lyrion) SlimProto client adapter.
# Emulates a hardware Squeezebox player, supporting gapless bit-perfect streaming.
enabled = false
server_host = "192.168.1.100"
server_port = 3483
http_port = 9000
player_name = "kiiaud Squeezebox"

# Locally-administered virtual MAC address for LMS device tracking (6 bytes):
mac_address = [2, 75, 73, 73, 65, 85]
"#;

/// Information about where configuration was loaded from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigSource {
    Explicit(PathBuf),
    SystemOnly(PathBuf),
    Merged {
        system: Option<PathBuf>,
        user: PathBuf,
    },
    CurrentDir(PathBuf),
    GeneratedDefault(PathBuf),
    DefaultInMemory,
}

impl ConfigSource {
    /// Returns the target path where user edits should be written if modifying configuration at runtime.
    pub fn effective_save_path(&self) -> Option<PathBuf> {
        match self {
            ConfigSource::Explicit(p) => Some(p.clone()),
            ConfigSource::SystemOnly(p) => Some(p.clone()),
            ConfigSource::Merged { user, .. } => Some(user.clone()),
            ConfigSource::CurrentDir(p) => Some(p.clone()),
            ConfigSource::GeneratedDefault(p) => Some(p.clone()),
            ConfigSource::DefaultInMemory => KiiaudConfig::user_config_path(),
        }
    }
}

impl std::fmt::Display for ConfigSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigSource::Explicit(p) => write!(f, "explicit path: {}", p.display()),
            ConfigSource::SystemOnly(p) => write!(f, "system-wide config: {}", p.display()),
            ConfigSource::Merged { system, user } => match system {
                Some(sys) => write!(
                    f,
                    "merged [system: {}, overridden by user: {}]",
                    sys.display(),
                    user.display()
                ),
                None => write!(f, "user config: {}", user.display()),
            },
            ConfigSource::CurrentDir(p) => write!(f, "local working directory: {}", p.display()),
            ConfigSource::GeneratedDefault(p) => {
                write!(f, "newly generated default config at: {}", p.display())
            }
            ConfigSource::DefaultInMemory => write!(f, "builtin in-memory defaults"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct KiiaudConfig {
    pub daemon: DaemonConfig,
    pub output: OutputConfig,
    pub security: SecurityConfig,
    pub adapter: AdaptersConfig,
}

impl Default for KiiaudConfig {
    fn default() -> Self {
        Self {
            daemon: DaemonConfig::default(),
            output: OutputConfig::default(),
            security: SecurityConfig::default(),
            adapter: AdaptersConfig::default(),
        }
    }
}

impl KiiaudConfig {
    /// Return the canonical system-wide configuration path.
    pub fn system_config_path() -> PathBuf {
        #[cfg(target_os = "windows")]
        {
            let program_data = std::env::var("ProgramData").unwrap_or_else(|_| r"C:\ProgramData".to_owned());
            PathBuf::from(program_data).join("kiiaud").join("config.toml")
        }
        #[cfg(not(target_os = "windows"))]
        {
            PathBuf::from("/etc/kiiaud/config.toml")
        }
    }

    /// Return the canonical per-user configuration path respecting `$XDG_CONFIG_HOME`.
    pub fn user_config_path() -> Option<PathBuf> {
        #[cfg(target_os = "windows")]
        {
            std::env::var("APPDATA")
                .ok()
                .map(|p| PathBuf::from(p).join("kiiaud").join("config.toml"))
        }
        #[cfg(not(target_os = "windows"))]
        {
            if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
                if !xdg.is_empty() {
                    return Some(PathBuf::from(xdg).join("kiiaud").join("config.toml"));
                }
            }
            std::env::var("HOME")
                .ok()
                .map(|home| PathBuf::from(home).join(".config").join("kiiaud").join("config.toml"))
        }
    }

    /// Recursively merge two TOML values.
    ///
    /// If both values are tables, keys in `over` override or extend `base`.
    /// Otherwise, `over` completely replaces `base`.
    pub fn merge_toml(base: &mut toml::Value, over: toml::Value) {
        match (base, over) {
            (toml::Value::Table(base_map), toml::Value::Table(over_map)) => {
                for (key, val) in over_map {
                    match base_map.get_mut(&key) {
                        Some(base_child) => Self::merge_toml(base_child, val),
                        None => {
                            base_map.insert(key, val);
                        }
                    }
                }
            }
            (base_slot, over_val) => {
                *base_slot = over_val;
            }
        }
    }

    /// Load configuration by merging system and user files according to hierarchy:
    ///
    /// 1. If `explicit_path` is provided, load solely that file.
    /// 2. If `system_only` is true, load `/etc/kiiaud/config.toml` only.
    /// 3. Otherwise (per-user mode):
    ///    - Load `/etc/kiiaud/config.toml` as base (if present).
    ///    - Load `~/.config/kiiaud/config.toml` (if present) and merge over base.
    ///    - If neither exists, check `./kiiaud.toml` in the working directory.
    ///    - If still no config exists and `auto_generate` is true, generate a default user config
    ///      at `~/.config/kiiaud/config.toml` and return it.
    ///    - Otherwise, return default in-memory configuration.
    pub fn resolve_config(
        explicit_path: Option<&Path>,
        system_only: bool,
        auto_generate: bool,
    ) -> Result<(Self, ConfigSource)> {
        // Case 1: Explicit path requested on CLI
        if let Some(path) = explicit_path {
            let cfg = Self::from_file(path)?;
            return Ok((cfg, ConfigSource::Explicit(path.to_path_buf())));
        }

        let sys_path = Self::system_config_path();

        // Case 2: System-only requested
        if system_only {
            if sys_path.exists() {
                let cfg = Self::from_file(&sys_path)?;
                return Ok((cfg, ConfigSource::SystemOnly(sys_path)));
            } else if auto_generate {
                Self::generate_default_config_file(&sys_path, false)?;
                let cfg = Self::from_file(&sys_path)?;
                return Ok((cfg, ConfigSource::GeneratedDefault(sys_path)));
            } else {
                return Err(KiiaudError::Config(format!(
                    "System configuration file '{}' not found and auto-generate is disabled",
                    sys_path.display()
                )));
            }
        }

        // Case 3: Per-user resolution with system fallback / merge
        let user_path_opt = Self::user_config_path();
        let sys_exists = sys_path.exists();
        let user_exists = user_path_opt.as_ref().map(|p| p.exists()).unwrap_or(false);

        if sys_exists || user_exists {
            let mut base_val = if sys_exists {
                let sys_content = std::fs::read_to_string(&sys_path).map_err(|e| {
                    KiiaudError::Config(format!("Failed to read system config '{}': {e}", sys_path.display()))
                })?;
                toml::from_str::<toml::Value>(&sys_content)
                    .map_err(|e| KiiaudError::Config(format!("Error parsing system config: {e}")))?
            } else {
                toml::Value::Table(toml::map::Map::new())
            };

            let source = if let Some(ref u_path) = user_path_opt {
                if u_path.exists() {
                    let user_content = std::fs::read_to_string(u_path).map_err(|e| {
                        KiiaudError::Config(format!("Failed to read user config '{}': {e}", u_path.display()))
                    })?;
                    let user_val: toml::Value = toml::from_str(&user_content)
                        .map_err(|e| KiiaudError::Config(format!("Error parsing user config: {e}")))?;
                    Self::merge_toml(&mut base_val, user_val);
                    ConfigSource::Merged {
                        system: if sys_exists { Some(sys_path) } else { None },
                        user: u_path.clone(),
                    }
                } else {
                    ConfigSource::SystemOnly(sys_path)
                }
            } else {
                ConfigSource::SystemOnly(sys_path)
            };

            let cfg: KiiaudConfig = base_val
                .try_into()
                .map_err(|e| KiiaudError::Config(format!("Failed to deserialize merged config: {e}")))?;
            return Ok((cfg, source));
        }

        // Case 4: Check local ./kiiaud.toml (handy for dev or portable mode)
        let local_path = PathBuf::from("kiiaud.toml");
        if local_path.exists() {
            let cfg = Self::from_file(&local_path)?;
            return Ok((cfg, ConfigSource::CurrentDir(local_path)));
        }

        // Case 5: Brand new environment
        if auto_generate {
            if let Some(target) = user_path_opt {
                Self::generate_default_config_file(&target, false)?;
                let cfg = Self::from_file(&target)?;
                return Ok((cfg, ConfigSource::GeneratedDefault(target)));
            }
        }

        // Fallback to in-memory default
        Ok((Self::default(), ConfigSource::DefaultInMemory))
    }

    /// Generate a well-commented default configuration file at `path`.
    ///
    /// Creates parent directories if missing and enforces strict file permissions (0600 on Unix)
    /// to secure credentials.
    pub fn generate_default_config_file(path: &Path, force: bool) -> Result<PathBuf> {
        if path.exists() && !force {
            return Err(KiiaudError::Config(format!(
                "Configuration file '{}' already exists. Use --force to overwrite.",
                path.display()
            )));
        }

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                KiiaudError::Config(format!(
                    "Failed to create config directory '{}': {e}",
                    parent.display()
                ))
            })?;
        }

        std::fs::write(path, DEFAULT_CONFIG_TEMPLATE).map_err(|e| {
            KiiaudError::Config(format!(
                "Failed to write default config to '{}': {e}",
                path.display()
            ))
        })?;

        // Secure file permissions on Unix: only owner read/write (0600)
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o600);
            let _ = std::fs::set_permissions(path, perms);
        }

        Ok(path.to_path_buf())
    }

    pub fn from_file(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path).map_err(|e| {
            KiiaudError::Config(format!("Failed to read config '{}': {e}", path.display()))
        })?;
        Self::from_toml_str(&content)
    }

    pub fn from_toml_str(s: &str) -> Result<Self> {
        toml::from_str(s).map_err(|e| KiiaudError::Config(format!("TOML parse error: {e}")))
    }

    pub fn to_toml_string(&self) -> Result<String> {
        toml::to_string_pretty(self)
            .map_err(|e| KiiaudError::Config(format!("TOML serialize error: {e}")))
    }

    pub fn save_to_file(&self, path: &Path) -> Result<()> {
        let body = self.to_toml_string()?;
        let content =
            format!("# kiiaud — Bit-Perfect Network Audio Playback Daemon Configuration\n\n{body}");
        std::fs::write(path, content).map_err(|e| {
            KiiaudError::Config(format!("Failed to write config '{}': {e}", path.display()))
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DaemonConfig {
    pub name: String,
    pub socket_path: PathBuf,
    pub log_level: String,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            name: "kiiaud-rpi5".to_owned(),
            socket_path: PathBuf::from("/tmp/kiiaud.sock"),
            log_level: "info".to_owned(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct OutputConfig {
    /// Output backend: `"alsa"` or `"mock"` / `"null"`.
    pub backend: String,
    /// ALSA hardware device identifier (e.g. `"hw:CARD=Audio,DEV=0"` or `"hw:7,0"`).
    pub device: String,
    /// Enforce bit-perfect hardware transport (no software volume, no resampling).
    pub bit_perfect: bool,
    /// DSD transport mode (`"auto"`, `"native"`, or `"dop"`).
    pub dsd_mode: DsdMode,
    /// Buffer time in milliseconds (default 200ms).
    pub buffer_time_ms: u32,
    /// Period time in milliseconds (default 40ms).
    pub period_time_ms: u32,
    /// Optional hardware mixer control name (e.g. `"PCM"` or `"Master"`). `None` disables volume changes.
    pub hardware_mixer: Option<String>,
}

impl Default for OutputConfig {
    fn default() -> Self {
        #[cfg(target_os = "windows")]
        let (default_backend, default_dev) = ("wasapi", "default");
        #[cfg(target_os = "android")]
        let (default_backend, default_dev) = ("oboe", "default");
        #[cfg(all(not(target_os = "windows"), not(target_os = "android")))]
        let (default_backend, default_dev) = ("alsa", "hw:CARD=Audio,DEV=0");

        Self {
            backend: default_backend.to_owned(),
            device: default_dev.to_owned(),
            bit_perfect: true,
            dsd_mode: DsdMode::Auto,
            buffer_time_ms: 200,
            period_time_ms: 40,
            hardware_mixer: None,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SecurityConfig {
    pub allowed_roots: Vec<PathBuf>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AdaptersConfig {
    pub web: WebAdapterConfig,
    pub mpd: MpdAdapterConfig,
    pub jellyfin: JellyfinAdapterConfig,
    pub lms: LmsAdapterConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WebAdapterConfig {
    pub enabled: bool,
    pub listen: String,
}

impl Default for WebAdapterConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            listen: "0.0.0.0:7110".to_owned(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MpdAdapterConfig {
    pub enabled: bool,
    pub listen: String,
    pub music_directory: Option<PathBuf>,
}

impl Default for MpdAdapterConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            listen: "0.0.0.0:6600".to_owned(),
            music_directory: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct JellyfinAdapterConfig {
    pub enabled: bool,
    pub server_url: String,
    pub api_key: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
    pub device_id: String,
    pub device_name: String,
}

impl Default for JellyfinAdapterConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            server_url: "http://jellyfin.local:8096".to_owned(),
            api_key: None,
            username: None,
            password: None,
            device_id: "kiiaud-rpi5-endpoint".to_owned(),
            device_name: "kiiaud Endpoint".to_owned(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LmsAdapterConfig {
    pub enabled: bool,
    pub server_host: String,
    pub server_port: u16,
    pub http_port: u16,
    pub player_name: String,
    pub mac_address: [u8; 6],
    /// Maximum sample rate in Hz to report to LMS HELO (e.g. 192000, 384000, 768000).
    /// If None, auto-detected from output capabilities or defaults to 192000.
    pub max_sample_rate: Option<u32>,
    /// Whether to advertise DSD (`dsf,dff`) format support to LMS.
    /// If None, auto-detected from output capabilities (true if DAC supports DSD, false otherwise).
    /// When false, LMS automatically transcodes DSD to PCM at <= max_sample_rate.
    pub dsd_support: Option<bool>,
}

impl Default for LmsAdapterConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            server_host: "192.168.1.100".to_owned(),
            server_port: 3483,
            http_port: 9000,
            player_name: "kiiaud Squeezebox".to_owned(),
            mac_address: [0x02, 0x4b, 0x49, 0x49, 0x41, 0x55], // locally administered "KIIAU"
            max_sample_rate: None,
            dsd_support: None,
        }
    }
}
