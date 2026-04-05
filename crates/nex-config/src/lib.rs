use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Top-level Nexterm configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct NextermConfig {
    pub general: GeneralConfig,
    pub rendering: RenderingConfig,
    pub mux: MuxConfig,
    pub mcp: McpConfig,
    pub autocomplete: AutocompleteConfig,
}

impl Default for NextermConfig {
    fn default() -> Self {
        Self {
            general: GeneralConfig::default(),
            rendering: RenderingConfig::default(),
            mux: MuxConfig::default(),
            mcp: McpConfig::default(),
            autocomplete: AutocompleteConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GeneralConfig {
    pub shell: Option<String>,
    pub font_family: String,
    pub font_size: f32,
    pub theme: String,
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            shell: None, // Use $SHELL
            font_family: "JetBrains Mono".to_string(),
            font_size: 14.0,
            theme: "dark".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RenderingConfig {
    pub target_fps: u32,
    pub gpu_preference: GpuPreference,
}

impl Default for RenderingConfig {
    fn default() -> Self {
        Self {
            target_fps: 60,
            gpu_preference: GpuPreference::HighPerformance,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GpuPreference {
    HighPerformance,
    LowPower,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MuxConfig {
    pub auto_start_daemon: bool,
    pub socket_path: Option<PathBuf>,
}

impl Default for MuxConfig {
    fn default() -> Self {
        Self {
            auto_start_daemon: true,
            socket_path: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct McpConfig {
    pub enabled: bool,
    pub http_port: u16,
    pub stdio_enabled: bool,
}

impl Default for McpConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            http_port: 19840,
            stdio_enabled: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AutocompleteConfig {
    pub enabled: bool,
    pub model_path: Option<PathBuf>,
    pub debounce_ms: u64,
}

impl Default for AutocompleteConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            model_path: None,
            debounce_ms: 150,
        }
    }
}

/// Get the platform-specific config directory.
pub fn config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("nexterm")
}

/// Load configuration from the default path, falling back to defaults.
pub fn load_config() -> NextermConfig {
    let config_path = config_dir().join("config.toml");
    if config_path.exists() {
        match std::fs::read_to_string(&config_path) {
            Ok(content) => match toml::from_str(&content) {
                Ok(config) => return config,
                Err(e) => {
                    tracing::warn!("Failed to parse config: {e}, using defaults");
                }
            },
            Err(e) => {
                tracing::warn!("Failed to read config: {e}, using defaults");
            }
        }
    }
    NextermConfig::default()
}
