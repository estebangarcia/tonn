use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Top-level Tonn configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TonnConfig {
    pub general: GeneralConfig,
    pub rendering: RenderingConfig,
    pub mux: MuxConfig,
    pub mcp: McpConfig,
    pub autocomplete: AutocompleteConfig,
}

/// All available built-in theme names.
pub const AVAILABLE_THEMES: &[&str] = &[
    "dark", "light", "dracula", "solarized-dark", "solarized-light",
    "one-dark", "nord", "catppuccin-mocha", "catppuccin-latte",
    "gruvbox-dark", "gruvbox-light", "tokyo-night",
];

impl TonnConfig {
    /// Get the resolved theme based on the `general.theme` name.
    pub fn theme(&self) -> Theme {
        match self.general.theme.as_str() {
            "light" => light_theme(),
            "dracula" => dracula_theme(),
            "solarized-dark" => solarized_dark_theme(),
            "solarized-light" => solarized_light_theme(),
            "one-dark" => one_dark_theme(),
            "nord" => nord_theme(),
            "catppuccin-mocha" => catppuccin_mocha_theme(),
            "catppuccin-latte" => catppuccin_latte_theme(),
            "gruvbox-dark" => gruvbox_dark_theme(),
            "gruvbox-light" => gruvbox_light_theme(),
            "tokyo-night" => tokyo_night_theme(),
            _ => dark_theme(),
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
    pub scrollback_history: usize,
    pub auto_update: bool,
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            shell: None, // Use $SHELL
            font_family: String::new(), // empty = system monospace default
            font_size: 14.0,
            theme: "dark".to_string(),
            scrollback_history: 10_000,
            auto_update: true,
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

// ---------------------------------------------------------------------------
// Theme
// ---------------------------------------------------------------------------

/// Complete color theme for the terminal and UI chrome.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Theme {
    pub name: String,
    pub bg: [u8; 3],
    pub fg: [u8; 3],
    pub cursor: [u8; 4],
    pub selection: [u8; 3],
    pub tab_bar_bg: [u8; 3],
    pub tab_active_bg: [u8; 3],
    pub tab_inactive_bg: [u8; 3],
    pub tab_active_text: [u8; 3],
    pub tab_inactive_text: [u8; 3],
    pub focus_border: [u8; 3],
    pub divider: [u8; 3],
    pub overlay_panel_bg: [u8; 3],
    pub overlay_selected_bg: [u8; 3],
    pub overlay_normal_bg: [u8; 3],
    pub overlay_selected_text: [u8; 3],
    pub overlay_normal_text: [u8; 3],
    pub secondary_text: [u8; 3],
    pub session_search_bar_bg: [u8; 3],
    pub session_search_text: [u8; 3],
    pub session_search_placeholder: [u8; 3],
    pub session_active_indicator: [u8; 3],
    pub session_secondary_text: [u8; 3],
    pub session_header_selected_bg: [u8; 3],
    pub ansi_black: [u8; 3],
    pub ansi_red: [u8; 3],
    pub ansi_green: [u8; 3],
    pub ansi_yellow: [u8; 3],
    pub ansi_blue: [u8; 3],
    pub ansi_magenta: [u8; 3],
    pub ansi_cyan: [u8; 3],
    pub ansi_white: [u8; 3],
    pub ansi_bright_black: [u8; 3],
    pub ansi_bright_red: [u8; 3],
    pub ansi_bright_green: [u8; 3],
    pub ansi_bright_yellow: [u8; 3],
    pub ansi_bright_blue: [u8; 3],
    pub ansi_bright_magenta: [u8; 3],
    pub ansi_bright_cyan: [u8; 3],
    pub ansi_bright_white: [u8; 3],
}

impl Theme {
    /// Return the 16 ANSI colors as a `[[u8; 3]; 16]` palette array.
    /// Order: black, red, green, yellow, blue, magenta, cyan, white,
    ///        bright_black, bright_red, …, bright_white.
    pub fn ansi_palette(&self) -> [[u8; 3]; 16] {
        [
            self.ansi_black,
            self.ansi_red,
            self.ansi_green,
            self.ansi_yellow,
            self.ansi_blue,
            self.ansi_magenta,
            self.ansi_cyan,
            self.ansi_white,
            self.ansi_bright_black,
            self.ansi_bright_red,
            self.ansi_bright_green,
            self.ansi_bright_yellow,
            self.ansi_bright_blue,
            self.ansi_bright_magenta,
            self.ansi_bright_cyan,
            self.ansi_bright_white,
        ]
    }
}

/// Built-in dark theme — matches the original hardcoded colors.
pub fn dark_theme() -> Theme {
    Theme {
        name: "dark".to_string(),
        bg: [13, 13, 18],
        fg: [204, 204, 204],
        cursor: [200, 200, 200, 180],
        selection: [80, 120, 200],
        tab_bar_bg: [25, 25, 30],
        tab_active_bg: [50, 50, 58],
        tab_inactive_bg: [35, 35, 40],
        tab_active_text: [220, 220, 225],
        tab_inactive_text: [140, 140, 150],
        focus_border: [80, 140, 220],
        divider: [90, 90, 105],
        overlay_panel_bg: [50, 50, 62],
        overlay_selected_bg: [55, 95, 175],
        overlay_normal_bg: [58, 58, 68],
        overlay_selected_text: [255, 255, 255],
        overlay_normal_text: [170, 170, 180],
        secondary_text: [180, 180, 190],
        session_search_bar_bg: [40, 40, 50],
        session_search_text: [200, 200, 210],
        session_search_placeholder: [100, 100, 115],
        session_active_indicator: [80, 200, 120],
        session_secondary_text: [120, 120, 135],
        session_header_selected_bg: [65, 65, 80],
        ansi_black: [40, 40, 40],
        ansi_red: [204, 60, 60],
        ansi_green: [78, 201, 98],
        ansi_yellow: [229, 200, 88],
        ansi_blue: [80, 140, 220],
        ansi_magenta: [186, 100, 210],
        ansi_cyan: [80, 210, 210],
        ansi_white: [204, 204, 204],
        ansi_bright_black: [110, 110, 110],
        ansi_bright_red: [255, 100, 100],
        ansi_bright_green: [100, 255, 130],
        ansi_bright_yellow: [255, 240, 120],
        ansi_bright_blue: [120, 170, 255],
        ansi_bright_magenta: [220, 140, 255],
        ansi_bright_cyan: [120, 240, 240],
        ansi_bright_white: [240, 240, 240],
    }
}

/// Built-in light theme — dark text on a light background.
pub fn light_theme() -> Theme {
    Theme {
        name: "light".to_string(),
        bg: [250, 250, 248],
        fg: [40, 40, 40],
        cursor: [50, 50, 50, 200],
        selection: [180, 210, 255],
        tab_bar_bg: [230, 230, 228],
        tab_active_bg: [250, 250, 248],
        tab_inactive_bg: [220, 220, 218],
        tab_active_text: [30, 30, 30],
        tab_inactive_text: [120, 120, 120],
        focus_border: [50, 120, 200],
        divider: [200, 200, 200],
        overlay_panel_bg: [245, 245, 243],
        overlay_selected_bg: [50, 120, 200],
        overlay_normal_bg: [235, 235, 233],
        overlay_selected_text: [255, 255, 255],
        overlay_normal_text: [60, 60, 60],
        secondary_text: [100, 100, 100],
        session_search_bar_bg: [235, 235, 233],
        session_search_text: [40, 40, 40],
        session_search_placeholder: [160, 160, 160],
        session_active_indicator: [40, 160, 80],
        session_secondary_text: [130, 130, 130],
        session_header_selected_bg: [210, 210, 208],
        ansi_black: [0, 0, 0],
        ansi_red: [180, 30, 30],
        ansi_green: [30, 140, 50],
        ansi_yellow: [160, 130, 0],
        ansi_blue: [30, 100, 200],
        ansi_magenta: [150, 60, 180],
        ansi_cyan: [20, 150, 150],
        ansi_white: [200, 200, 200],
        ansi_bright_black: [80, 80, 80],
        ansi_bright_red: [220, 60, 60],
        ansi_bright_green: [50, 180, 70],
        ansi_bright_yellow: [200, 170, 30],
        ansi_bright_blue: [60, 140, 240],
        ansi_bright_magenta: [190, 90, 220],
        ansi_bright_cyan: [40, 190, 190],
        ansi_bright_white: [240, 240, 240],
    }
}

pub fn dracula_theme() -> Theme {
    Theme {
        name: "dracula".into(), bg: [40, 42, 54], fg: [248, 248, 242], cursor: [248, 248, 242, 200],
        selection: [68, 71, 90], tab_bar_bg: [33, 34, 44], tab_active_bg: [68, 71, 90],
        tab_inactive_bg: [40, 42, 54], tab_active_text: [248, 248, 242], tab_inactive_text: [150, 150, 170],
        focus_border: [139, 233, 253], divider: [68, 71, 90],
        overlay_panel_bg: [50, 52, 66], overlay_selected_bg: [98, 114, 164], overlay_normal_bg: [60, 62, 76],
        overlay_selected_text: [248, 248, 242], overlay_normal_text: [189, 147, 249],
        secondary_text: [150, 150, 170],
        session_search_bar_bg: [55, 57, 70], session_search_text: [248, 248, 242],
        session_search_placeholder: [100, 100, 120], session_active_indicator: [80, 250, 123],
        session_secondary_text: [130, 130, 150], session_header_selected_bg: [68, 71, 90],
        ansi_black: [33, 34, 44], ansi_red: [255, 85, 85], ansi_green: [80, 250, 123],
        ansi_yellow: [241, 250, 140], ansi_blue: [98, 114, 164], ansi_magenta: [189, 147, 249],
        ansi_cyan: [139, 233, 253], ansi_white: [248, 248, 242],
        ansi_bright_black: [98, 114, 164], ansi_bright_red: [255, 110, 110], ansi_bright_green: [105, 255, 148],
        ansi_bright_yellow: [255, 255, 165], ansi_bright_blue: [120, 140, 190], ansi_bright_magenta: [210, 170, 255],
        ansi_bright_cyan: [160, 240, 255], ansi_bright_white: [255, 255, 255],
    }
}

pub fn solarized_dark_theme() -> Theme {
    Theme {
        name: "solarized-dark".into(), bg: [0, 43, 54], fg: [131, 148, 150], cursor: [131, 148, 150, 200],
        selection: [7, 54, 66], tab_bar_bg: [0, 34, 43], tab_active_bg: [7, 54, 66],
        tab_inactive_bg: [0, 43, 54], tab_active_text: [147, 161, 161], tab_inactive_text: [88, 110, 117],
        focus_border: [38, 139, 210], divider: [7, 54, 66],
        overlay_panel_bg: [7, 54, 66], overlay_selected_bg: [38, 139, 210], overlay_normal_bg: [0, 43, 54],
        overlay_selected_text: [253, 246, 227], overlay_normal_text: [131, 148, 150],
        secondary_text: [88, 110, 117],
        session_search_bar_bg: [7, 54, 66], session_search_text: [147, 161, 161],
        session_search_placeholder: [88, 110, 117], session_active_indicator: [133, 153, 0],
        session_secondary_text: [88, 110, 117], session_header_selected_bg: [7, 54, 66],
        ansi_black: [7, 54, 66], ansi_red: [220, 50, 47], ansi_green: [133, 153, 0],
        ansi_yellow: [181, 137, 0], ansi_blue: [38, 139, 210], ansi_magenta: [211, 54, 130],
        ansi_cyan: [42, 161, 152], ansi_white: [238, 232, 213],
        ansi_bright_black: [0, 43, 54], ansi_bright_red: [203, 75, 22], ansi_bright_green: [88, 110, 117],
        ansi_bright_yellow: [101, 123, 131], ansi_bright_blue: [131, 148, 150], ansi_bright_magenta: [108, 113, 196],
        ansi_bright_cyan: [147, 161, 161], ansi_bright_white: [253, 246, 227],
    }
}

pub fn solarized_light_theme() -> Theme {
    Theme {
        name: "solarized-light".into(), bg: [253, 246, 227], fg: [101, 123, 131], cursor: [101, 123, 131, 200],
        selection: [238, 232, 213], tab_bar_bg: [238, 232, 213], tab_active_bg: [253, 246, 227],
        tab_inactive_bg: [228, 222, 203], tab_active_text: [88, 110, 117], tab_inactive_text: [147, 161, 161],
        focus_border: [38, 139, 210], divider: [218, 212, 193],
        overlay_panel_bg: [238, 232, 213], overlay_selected_bg: [38, 139, 210], overlay_normal_bg: [248, 241, 222],
        overlay_selected_text: [253, 246, 227], overlay_normal_text: [101, 123, 131],
        secondary_text: [147, 161, 161],
        session_search_bar_bg: [238, 232, 213], session_search_text: [88, 110, 117],
        session_search_placeholder: [147, 161, 161], session_active_indicator: [133, 153, 0],
        session_secondary_text: [147, 161, 161], session_header_selected_bg: [228, 222, 203],
        ansi_black: [7, 54, 66], ansi_red: [220, 50, 47], ansi_green: [133, 153, 0],
        ansi_yellow: [181, 137, 0], ansi_blue: [38, 139, 210], ansi_magenta: [211, 54, 130],
        ansi_cyan: [42, 161, 152], ansi_white: [238, 232, 213],
        ansi_bright_black: [0, 43, 54], ansi_bright_red: [203, 75, 22], ansi_bright_green: [88, 110, 117],
        ansi_bright_yellow: [101, 123, 131], ansi_bright_blue: [131, 148, 150], ansi_bright_magenta: [108, 113, 196],
        ansi_bright_cyan: [147, 161, 161], ansi_bright_white: [253, 246, 227],
    }
}

pub fn one_dark_theme() -> Theme {
    Theme {
        name: "one-dark".into(), bg: [40, 44, 52], fg: [171, 178, 191], cursor: [171, 178, 191, 200],
        selection: [62, 68, 81], tab_bar_bg: [33, 37, 43], tab_active_bg: [55, 59, 69],
        tab_inactive_bg: [40, 44, 52], tab_active_text: [200, 204, 212], tab_inactive_text: [120, 126, 138],
        focus_border: [97, 175, 239], divider: [55, 59, 69],
        overlay_panel_bg: [50, 54, 64], overlay_selected_bg: [62, 68, 81], overlay_normal_bg: [44, 48, 58],
        overlay_selected_text: [230, 233, 238], overlay_normal_text: [171, 178, 191],
        secondary_text: [120, 126, 138],
        session_search_bar_bg: [50, 54, 64], session_search_text: [200, 204, 212],
        session_search_placeholder: [100, 106, 118], session_active_indicator: [152, 195, 121],
        session_secondary_text: [120, 126, 138], session_header_selected_bg: [55, 59, 69],
        ansi_black: [40, 44, 52], ansi_red: [224, 108, 117], ansi_green: [152, 195, 121],
        ansi_yellow: [229, 192, 123], ansi_blue: [97, 175, 239], ansi_magenta: [198, 120, 221],
        ansi_cyan: [86, 182, 194], ansi_white: [171, 178, 191],
        ansi_bright_black: [92, 99, 112], ansi_bright_red: [240, 130, 140], ansi_bright_green: [170, 210, 140],
        ansi_bright_yellow: [240, 210, 145], ansi_bright_blue: [120, 195, 255], ansi_bright_magenta: [220, 145, 240],
        ansi_bright_cyan: [110, 200, 215], ansi_bright_white: [220, 223, 228],
    }
}

pub fn nord_theme() -> Theme {
    Theme {
        name: "nord".into(), bg: [46, 52, 64], fg: [216, 222, 233], cursor: [216, 222, 233, 200],
        selection: [67, 76, 94], tab_bar_bg: [36, 40, 50], tab_active_bg: [67, 76, 94],
        tab_inactive_bg: [46, 52, 64], tab_active_text: [236, 239, 244], tab_inactive_text: [136, 146, 166],
        focus_border: [136, 192, 208], divider: [67, 76, 94],
        overlay_panel_bg: [59, 66, 82], overlay_selected_bg: [76, 86, 106], overlay_normal_bg: [52, 58, 74],
        overlay_selected_text: [236, 239, 244], overlay_normal_text: [216, 222, 233],
        secondary_text: [136, 146, 166],
        session_search_bar_bg: [59, 66, 82], session_search_text: [216, 222, 233],
        session_search_placeholder: [107, 118, 140], session_active_indicator: [163, 190, 140],
        session_secondary_text: [136, 146, 166], session_header_selected_bg: [67, 76, 94],
        ansi_black: [59, 66, 82], ansi_red: [191, 97, 106], ansi_green: [163, 190, 140],
        ansi_yellow: [235, 203, 139], ansi_blue: [129, 161, 193], ansi_magenta: [180, 142, 173],
        ansi_cyan: [136, 192, 208], ansi_white: [229, 233, 240],
        ansi_bright_black: [76, 86, 106], ansi_bright_red: [210, 120, 130], ansi_bright_green: [180, 210, 160],
        ansi_bright_yellow: [245, 220, 160], ansi_bright_blue: [150, 180, 210], ansi_bright_magenta: [200, 165, 195],
        ansi_bright_cyan: [155, 210, 225], ansi_bright_white: [236, 239, 244],
    }
}

pub fn catppuccin_mocha_theme() -> Theme {
    Theme {
        name: "catppuccin-mocha".into(), bg: [30, 30, 46], fg: [205, 214, 244], cursor: [205, 214, 244, 200],
        selection: [69, 71, 90], tab_bar_bg: [24, 24, 37], tab_active_bg: [49, 50, 68],
        tab_inactive_bg: [30, 30, 46], tab_active_text: [205, 214, 244], tab_inactive_text: [127, 132, 156],
        focus_border: [137, 180, 250], divider: [49, 50, 68],
        overlay_panel_bg: [36, 39, 58], overlay_selected_bg: [69, 71, 90], overlay_normal_bg: [30, 30, 46],
        overlay_selected_text: [205, 214, 244], overlay_normal_text: [186, 194, 222],
        secondary_text: [127, 132, 156],
        session_search_bar_bg: [36, 39, 58], session_search_text: [205, 214, 244],
        session_search_placeholder: [108, 112, 134], session_active_indicator: [166, 227, 161],
        session_secondary_text: [127, 132, 156], session_header_selected_bg: [49, 50, 68],
        ansi_black: [69, 71, 90], ansi_red: [243, 139, 168], ansi_green: [166, 227, 161],
        ansi_yellow: [249, 226, 175], ansi_blue: [137, 180, 250], ansi_magenta: [203, 166, 247],
        ansi_cyan: [148, 226, 213], ansi_white: [186, 194, 222],
        ansi_bright_black: [88, 91, 112], ansi_bright_red: [250, 160, 185], ansi_bright_green: [185, 240, 180],
        ansi_bright_yellow: [255, 240, 195], ansi_bright_blue: [160, 200, 255], ansi_bright_magenta: [220, 185, 255],
        ansi_bright_cyan: [170, 240, 230], ansi_bright_white: [205, 214, 244],
    }
}

pub fn catppuccin_latte_theme() -> Theme {
    Theme {
        name: "catppuccin-latte".into(), bg: [239, 241, 245], fg: [76, 79, 105], cursor: [76, 79, 105, 200],
        selection: [188, 192, 204], tab_bar_bg: [230, 233, 239], tab_active_bg: [239, 241, 245],
        tab_inactive_bg: [220, 224, 232], tab_active_text: [76, 79, 105], tab_inactive_text: [140, 143, 161],
        focus_border: [30, 102, 245], divider: [204, 208, 218],
        overlay_panel_bg: [230, 233, 239], overlay_selected_bg: [30, 102, 245], overlay_normal_bg: [239, 241, 245],
        overlay_selected_text: [239, 241, 245], overlay_normal_text: [76, 79, 105],
        secondary_text: [140, 143, 161],
        session_search_bar_bg: [230, 233, 239], session_search_text: [76, 79, 105],
        session_search_placeholder: [156, 160, 176], session_active_indicator: [64, 160, 43],
        session_secondary_text: [140, 143, 161], session_header_selected_bg: [220, 224, 232],
        ansi_black: [76, 79, 105], ansi_red: [210, 15, 57], ansi_green: [64, 160, 43],
        ansi_yellow: [223, 142, 29], ansi_blue: [30, 102, 245], ansi_magenta: [136, 57, 239],
        ansi_cyan: [4, 165, 229], ansi_white: [204, 208, 218],
        ansi_bright_black: [108, 111, 133], ansi_bright_red: [230, 50, 85], ansi_bright_green: [90, 185, 70],
        ansi_bright_yellow: [240, 165, 55], ansi_bright_blue: [60, 130, 255], ansi_bright_magenta: [160, 85, 255],
        ansi_bright_cyan: [35, 190, 245], ansi_bright_white: [239, 241, 245],
    }
}

pub fn gruvbox_dark_theme() -> Theme {
    Theme {
        name: "gruvbox-dark".into(), bg: [40, 40, 40], fg: [235, 219, 178], cursor: [235, 219, 178, 200],
        selection: [80, 73, 69], tab_bar_bg: [29, 32, 33], tab_active_bg: [60, 56, 54],
        tab_inactive_bg: [40, 40, 40], tab_active_text: [235, 219, 178], tab_inactive_text: [146, 131, 116],
        focus_border: [69, 133, 136], divider: [60, 56, 54],
        overlay_panel_bg: [50, 48, 47], overlay_selected_bg: [80, 73, 69], overlay_normal_bg: [40, 40, 40],
        overlay_selected_text: [251, 241, 199], overlay_normal_text: [213, 196, 161],
        secondary_text: [146, 131, 116],
        session_search_bar_bg: [50, 48, 47], session_search_text: [235, 219, 178],
        session_search_placeholder: [124, 111, 100], session_active_indicator: [184, 187, 38],
        session_secondary_text: [146, 131, 116], session_header_selected_bg: [60, 56, 54],
        ansi_black: [40, 40, 40], ansi_red: [204, 36, 29], ansi_green: [152, 151, 26],
        ansi_yellow: [215, 153, 33], ansi_blue: [69, 133, 136], ansi_magenta: [177, 98, 134],
        ansi_cyan: [104, 157, 106], ansi_white: [168, 153, 132],
        ansi_bright_black: [146, 131, 116], ansi_bright_red: [251, 73, 52], ansi_bright_green: [184, 187, 38],
        ansi_bright_yellow: [250, 189, 47], ansi_bright_blue: [131, 165, 152], ansi_bright_magenta: [211, 134, 155],
        ansi_bright_cyan: [142, 192, 124], ansi_bright_white: [235, 219, 178],
    }
}

pub fn gruvbox_light_theme() -> Theme {
    Theme {
        name: "gruvbox-light".into(), bg: [251, 241, 199], fg: [60, 56, 54], cursor: [60, 56, 54, 200],
        selection: [213, 196, 161], tab_bar_bg: [242, 229, 188], tab_active_bg: [251, 241, 199],
        tab_inactive_bg: [230, 218, 176], tab_active_text: [60, 56, 54], tab_inactive_text: [146, 131, 116],
        focus_border: [69, 133, 136], divider: [213, 196, 161],
        overlay_panel_bg: [242, 229, 188], overlay_selected_bg: [69, 133, 136], overlay_normal_bg: [251, 241, 199],
        overlay_selected_text: [251, 241, 199], overlay_normal_text: [60, 56, 54],
        secondary_text: [146, 131, 116],
        session_search_bar_bg: [235, 222, 180], session_search_text: [60, 56, 54],
        session_search_placeholder: [146, 131, 116], session_active_indicator: [152, 151, 26],
        session_secondary_text: [146, 131, 116], session_header_selected_bg: [220, 208, 166],
        ansi_black: [60, 56, 54], ansi_red: [204, 36, 29], ansi_green: [152, 151, 26],
        ansi_yellow: [215, 153, 33], ansi_blue: [69, 133, 136], ansi_magenta: [177, 98, 134],
        ansi_cyan: [104, 157, 106], ansi_white: [213, 196, 161],
        ansi_bright_black: [124, 111, 100], ansi_bright_red: [157, 0, 6], ansi_bright_green: [121, 116, 14],
        ansi_bright_yellow: [181, 118, 20], ansi_bright_blue: [7, 102, 120], ansi_bright_magenta: [143, 63, 113],
        ansi_bright_cyan: [66, 123, 88], ansi_bright_white: [251, 241, 199],
    }
}

pub fn tokyo_night_theme() -> Theme {
    Theme {
        name: "tokyo-night".into(), bg: [26, 27, 38], fg: [169, 177, 214], cursor: [169, 177, 214, 200],
        selection: [51, 59, 91], tab_bar_bg: [22, 22, 30], tab_active_bg: [41, 46, 66],
        tab_inactive_bg: [26, 27, 38], tab_active_text: [192, 202, 245], tab_inactive_text: [110, 118, 152],
        focus_border: [122, 162, 247], divider: [41, 46, 66],
        overlay_panel_bg: [36, 40, 59], overlay_selected_bg: [51, 59, 91], overlay_normal_bg: [30, 33, 48],
        overlay_selected_text: [192, 202, 245], overlay_normal_text: [169, 177, 214],
        secondary_text: [110, 118, 152],
        session_search_bar_bg: [36, 40, 59], session_search_text: [192, 202, 245],
        session_search_placeholder: [86, 95, 137], session_active_indicator: [158, 206, 106],
        session_secondary_text: [110, 118, 152], session_header_selected_bg: [41, 46, 66],
        ansi_black: [65, 72, 104], ansi_red: [247, 118, 142], ansi_green: [158, 206, 106],
        ansi_yellow: [224, 175, 104], ansi_blue: [122, 162, 247], ansi_magenta: [187, 154, 247],
        ansi_cyan: [125, 207, 255], ansi_white: [169, 177, 214],
        ansi_bright_black: [86, 95, 137], ansi_bright_red: [255, 145, 170], ansi_bright_green: [180, 225, 130],
        ansi_bright_yellow: [240, 195, 130], ansi_bright_blue: [145, 185, 255], ansi_bright_magenta: [210, 175, 255],
        ansi_bright_cyan: [150, 225, 255], ansi_bright_white: [192, 202, 245],
    }
}

/// Get the platform-specific config directory.
pub fn config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("tonn")
}

/// Load configuration from the default path, falling back to defaults.
pub fn load_config() -> TonnConfig {
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
    TonnConfig::default()
}
