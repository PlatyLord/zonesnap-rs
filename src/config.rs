use crate::hotkeys::HotkeySpec;
use crate::monitor::MonitorInfo;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::{BTreeMap, HashSet};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Debug, Clone, PartialEq)]
pub struct CustomZone {
    pub bounds: [f32; 4],
    pub trigger: Option<[f32; 4]>,
}

impl CustomZone {
    pub fn simple(bounds: [f32; 4]) -> Self {
        Self {
            bounds,
            trigger: None,
        }
    }

    pub fn with_trigger(bounds: [f32; 4], trigger: [f32; 4]) -> Self {
        Self {
            bounds,
            trigger: Some(trigger),
        }
    }

    pub fn effective_trigger(&self) -> [f32; 4] {
        self.trigger.unwrap_or(self.bounds)
    }

    pub fn has_custom_trigger(&self) -> bool {
        self.trigger.is_some()
    }
}

impl Serialize for CustomZone {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Detailed<'a> {
            bounds: &'a [f32; 4],
            trigger: &'a [f32; 4],
        }
        match &self.trigger {
            Some(tr) => Detailed {
                bounds: &self.bounds,
                trigger: tr,
            }
            .serialize(serializer),
            None => self.bounds.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for CustomZone {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            Simple([f32; 4]),
            Detailed {
                bounds: [f32; 4],
                #[serde(default)]
                trigger: Option<[f32; 4]>,
            },
        }
        match Repr::deserialize(deserializer)? {
            Repr::Simple(bounds) => Ok(Self::simple(bounds)),
            Repr::Detailed { bounds, trigger } => Ok(Self { bounds, trigger }),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LayoutMode {
    DynamicGrid { cols: u32, rows: u32 },
    CustomZones { zones: Vec<CustomZone> },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Layout {
    pub name: String,
    pub mode: LayoutMode,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum GuiTheme {
    #[default]
    Dark,
    Light,
    CatppuccinMocha,
    CatppuccinMacchiato,
    CatppuccinFrappe,
    CatppuccinLatte,
    Nord,
    TokyoNight,
    GruvboxDark,
    SolarizedLight,
}

impl GuiTheme {
    pub const ALL: &'static [GuiTheme] = &[
        GuiTheme::Dark,
        GuiTheme::Light,
        GuiTheme::CatppuccinMocha,
        GuiTheme::CatppuccinMacchiato,
        GuiTheme::CatppuccinFrappe,
        GuiTheme::CatppuccinLatte,
        GuiTheme::Nord,
        GuiTheme::TokyoNight,
        GuiTheme::GruvboxDark,
        GuiTheme::SolarizedLight,
    ];

    pub fn label(self) -> &'static str {
        match self {
            GuiTheme::Dark => "Dark (Default)",
            GuiTheme::Light => "Light",
            GuiTheme::CatppuccinMocha => "Catppuccin Mocha (Dark)",
            GuiTheme::CatppuccinMacchiato => "Catppuccin Macchiato (Dark)",
            GuiTheme::CatppuccinFrappe => "Catppuccin Frappé (Dark)",
            GuiTheme::CatppuccinLatte => "Catppuccin Latte (Light)",
            GuiTheme::Nord => "Nord (Dark)",
            GuiTheme::TokyoNight => "Tokyo Night (Dark)",
            GuiTheme::GruvboxDark => "Gruvbox Dark",
            GuiTheme::SolarizedLight => "Solarized Light",
        }
    }

    pub fn is_light(self) -> bool {
        matches!(
            self,
            Self::Light | Self::CatppuccinLatte | Self::SolarizedLight
        )
    }

    pub fn recommended_overlay_rgb(self) -> ([u8; 3], [u8; 3]) {
        match self {
            GuiTheme::Dark => ([56, 189, 248], [56, 189, 248]),
            GuiTheme::Light => ([2, 132, 199], [2, 132, 199]),
            GuiTheme::CatppuccinMocha => ([203, 166, 247], [180, 190, 254]),
            GuiTheme::CatppuccinMacchiato => ([198, 160, 246], [138, 173, 244]),
            GuiTheme::CatppuccinFrappe => ([202, 158, 230], [140, 170, 238]),
            GuiTheme::CatppuccinLatte => ([136, 57, 239], [30, 102, 245]),
            GuiTheme::Nord => ([136, 192, 208], [129, 161, 193]),
            GuiTheme::TokyoNight => ([122, 162, 247], [187, 154, 247]),
            GuiTheme::GruvboxDark => ([250, 189, 47], [215, 153, 33]),
            GuiTheme::SolarizedLight => ([38, 139, 210], [42, 161, 152]),
        }
    }
}

fn default_gui_scale() -> f32 {
    1.0
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct OverlayStyle {
    pub highlight_color: [u8; 3],
    pub highlight_opacity: f32,
    pub border_color: [u8; 3],
    pub zone_bg_opacity: f32,
    #[serde(default = "default_gui_scale")]
    pub gui_scale: f32,
    #[serde(default)]
    pub gui_theme: GuiTheme,
}

impl Default for OverlayStyle {
    fn default() -> Self {
        Self {
            highlight_color: [56, 189, 248],
            highlight_opacity: 0.35,
            border_color: [56, 189, 248],
            zone_bg_opacity: 0.14,
            gui_scale: 1.0,
            gui_theme: GuiTheme::default(),
        }
    }
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct BehaviorConfig {
    pub restore_on_unsnap: bool,
    #[serde(default = "default_true")]
    pub drag_top_maximize: bool,
    pub excluded_apps: Vec<String>,
}

impl Default for BehaviorConfig {
    fn default() -> Self {
        Self {
            restore_on_unsnap: true,
            drag_top_maximize: true,
            excluded_apps: vec!["mstsc.exe".to_string(), "vmconnect.exe".to_string()],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct HotkeysConfig {
    pub activate_overlay: HotkeySpec,
    pub open_settings: HotkeySpec,
    pub move_resize_modifier: String,
    pub monitor_lock_modifier: String,
    #[serde(default = "default_corner_resize_modifier")]
    pub corner_resize_modifier: String,
}

fn default_corner_resize_modifier() -> String {
    "Shift".to_string()
}

impl Default for HotkeysConfig {
    fn default() -> Self {
        Self {
            activate_overlay: HotkeySpec {
                modifiers: vec!["Win".to_string(), "Alt".to_string()],
                key: "Z".to_string(),
            },
            open_settings: HotkeySpec {
                modifiers: vec!["Win".to_string(), "Alt".to_string()],
                key: "E".to_string(),
            },
            move_resize_modifier: "Alt".to_string(),
            monitor_lock_modifier: "Ctrl".to_string(),
            corner_resize_modifier: default_corner_resize_modifier(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct MonitorProfile {
    pub device_name: String,
    pub width: i32,
    pub height: i32,
    pub active_layout_index: usize,
    pub layouts: Vec<Layout>,
}

impl Default for MonitorProfile {
    fn default() -> Self {
        Self {
            device_name: "DISPLAY".to_string(),
            width: 1920,
            height: 1080,
            active_layout_index: 0,
            layouts: default_layouts(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Config {
    pub version: u32,
    pub appearance: OverlayStyle,
    pub behavior: BehaviorConfig,
    pub hotkeys: HotkeysConfig,
    pub monitors: BTreeMap<String, MonitorProfile>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: 1,
            appearance: OverlayStyle::default(),
            behavior: BehaviorConfig::default(),
            hotkeys: HotkeysConfig::default(),
            monitors: BTreeMap::new(),
        }
    }
}

pub fn default_layouts() -> Vec<Layout> {
    vec![
        Layout {
            name: "Dynamic Grid (12x6)".to_string(),
            mode: LayoutMode::DynamicGrid { cols: 12, rows: 6 },
        },
        Layout {
            name: "Columns (3-Up)".to_string(),
            mode: LayoutMode::CustomZones {
                zones: vec![
                    CustomZone::simple([0.0, 0.0, 1.0 / 3.0, 1.0]),
                    CustomZone::simple([1.0 / 3.0, 0.0, 2.0 / 3.0, 1.0]),
                    CustomZone::simple([2.0 / 3.0, 0.0, 1.0, 1.0]),
                ],
            },
        },
        Layout {
            name: "Focus + Stack".to_string(),
            mode: LayoutMode::CustomZones {
                zones: vec![
                    CustomZone::simple([0.0, 0.0, 0.65, 1.0]),
                    CustomZone::simple([0.65, 0.0, 1.0, 0.5]),
                    CustomZone::simple([0.65, 0.5, 1.0, 1.0]),
                ],
            },
        },
    ]
}

/// Strictly resolve `zones.json` in the exact directory of the running executable.
pub fn config_path() -> io::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let dir = exe
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "executable has no parent dir"))?;
    Ok(dir.join("zones.json"))
}

/// Return the last-modified timestamp of `zones.json` if accessible.
pub fn config_modified_time() -> Option<SystemTime> {
    let path = config_path().ok()?;
    std::fs::metadata(path).ok()?.modified().ok()
}

impl Config {
    pub fn ensure_monitors(&mut self, monitors: &[MonitorInfo]) -> bool {
        let mut changed = false;
        for m in monitors {
            let key = m.profile_key();
            if let Some(prof) = self.monitors.get_mut(&key) {
                if prof.layouts.is_empty() {
                    prof.layouts = default_layouts();
                    prof.active_layout_index = 0;
                    changed = true;
                } else if prof.active_layout_index >= prof.layouts.len() {
                    prof.active_layout_index = 0;
                    changed = true;
                }
            } else {
                self.monitors.insert(
                    key,
                    MonitorProfile {
                        device_name: m.device_name.clone(),
                        width: m.rc_monitor.width(),
                        height: m.rc_monitor.height(),
                        active_layout_index: 0,
                        layouts: default_layouts(),
                    },
                );
                changed = true;
            }
        }
        changed
    }

    pub fn load_or_create(monitors: &[MonitorInfo]) -> Self {
        match config_path() {
            Ok(path) => Self::load_or_create_at(&path, monitors),
            Err(_) => {
                let mut cfg = Self::default();
                cfg.ensure_monitors(monitors);
                cfg
            }
        }
    }

    pub fn load_or_create_at(path: &Path, monitors: &[MonitorInfo]) -> Self {
        if path.exists() {
            if let Ok(raw) = std::fs::read_to_string(path) {
                if let Ok(mut cfg) = serde_json::from_str::<Config>(&raw) {
                    cfg.ensure_monitors(monitors);
                    return cfg;
                }
            }
            // Existing file could not be read or parsed; return in-memory defaults without overwriting disk.
            let mut cfg = Self::default();
            cfg.ensure_monitors(monitors);
            return cfg;
        }

        let mut cfg = Self::default();
        cfg.ensure_monitors(monitors);
        let _ = cfg.save_at(path);
        cfg
    }

    pub fn save(&self) -> io::Result<()> {
        let path = config_path()?;
        self.save_at(&path)
    }

    pub fn save_at(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        let tmp = path.with_extension("json.tmp");
        let write_tmp = (|| -> io::Result<()> {
            let mut file = std::fs::File::create(&tmp)?;
            file.write_all(json.as_bytes())?;
            file.sync_all()?;
            std::fs::rename(&tmp, path)
        })();
        if write_tmp.is_ok() {
            return Ok(());
        }
        let _ = std::fs::remove_file(&tmp);

        let mut file = std::fs::File::create(path)?;
        file.write_all(json.as_bytes())?;
        file.sync_all()
    }

    pub fn excluded_set(&self) -> HashSet<String> {
        self.behavior
            .excluded_apps
            .iter()
            .map(|s| s.trim().to_ascii_lowercase())
            .filter(|s| !s.is_empty())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::monitor::Rect;

    fn sample_monitor() -> MonitorInfo {
        MonitorInfo {
            hmonitor: 1,
            device_name: r"\\.\DISPLAY1".to_string(),
            rc_monitor: Rect {
                left: 0,
                top: 0,
                right: 2560,
                bottom: 1440,
            },
            rc_work: Rect {
                left: 0,
                top: 0,
                right: 2560,
                bottom: 1400,
            },
            is_primary: true,
            dpi_x: 96,
            dpi_y: 96,
        }
    }

    #[test]
    fn config_round_trip_and_missing_field_defaults() {
        let dir = std::env::temp_dir().join(format!(
            "zonesnap_cfg_test_{}",
            std::process::id()
        ));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("zones.json");
        let _ = std::fs::remove_file(&path);

        let monitors = vec![sample_monitor()];
        let cfg = Config::load_or_create_at(&path, &monitors);
        assert!(path.exists());
        assert!(cfg.monitors.contains_key(r"\\.\DISPLAY1_2560x1440"));
        assert_eq!(
            cfg.excluded_set(),
            HashSet::from(["mstsc.exe".to_string(), "vmconnect.exe".to_string()])
        );

        let reloaded = Config::load_or_create_at(&path, &monitors);
        assert_eq!(cfg, reloaded);

        // Partial JSON should gracefully populate defaults
        std::fs::write(&path, r#"{"version":1}"#).unwrap();
        let partial = Config::load_or_create_at(&path, &monitors);
        assert!(partial.behavior.restore_on_unsnap);
        assert_eq!(partial.appearance.gui_theme, GuiTheme::Dark);
        assert_eq!(partial.hotkeys.move_resize_modifier, "Alt");
        assert!(partial.monitors.contains_key(r"\\.\DISPLAY1_2560x1440"));

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn config_overwrite_preserves_all_edited_settings() {
        let dir = std::env::temp_dir().join(format!(
            "zonesnap_overwrite_test_{}",
            std::process::id()
        ));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("zones.json");
        let _ = std::fs::remove_file(&path);

        let monitors = vec![sample_monitor()];
        let mut cfg = Config::load_or_create_at(&path, &monitors);
        let key = monitors[0].profile_key();

        let prof = cfg.monitors.get_mut(&key).unwrap();
        prof.active_layout_index = 1;
        prof.layouts[0].mode = LayoutMode::DynamicGrid { cols: 8, rows: 4 };
        prof.layouts[1].mode = LayoutMode::CustomZones {
            zones: vec![
                CustomZone::simple([0.0, 0.0, 0.4, 1.0]),
                CustomZone::with_trigger([0.15, 0.0, 0.85, 1.0], [0.42, 0.25, 0.58, 0.75]),
            ],
        };

        cfg.appearance.highlight_color = [255, 100, 50];
        cfg.appearance.highlight_opacity = 0.6;
        cfg.appearance.border_color = [10, 200, 120];
        cfg.appearance.zone_bg_opacity = 0.25;
        cfg.appearance.gui_scale = 1.25;
        cfg.appearance.gui_theme = GuiTheme::CatppuccinMocha;
        cfg.behavior.restore_on_unsnap = false;
        cfg.behavior.drag_top_maximize = false;
        cfg.behavior.excluded_apps = vec!["notepad.exe".to_string()];
        cfg.hotkeys.activate_overlay = HotkeySpec {
            modifiers: vec!["Ctrl".to_string(), "Alt".to_string()],
            key: "Q".to_string(),
        };
        cfg.hotkeys.move_resize_modifier = "Ctrl".to_string();
        cfg.hotkeys.monitor_lock_modifier = "Shift".to_string();
        cfg.hotkeys.corner_resize_modifier = "Win".to_string();

        cfg.save_at(&path).expect("save edited config");
        let reloaded = Config::load_or_create_at(&path, &monitors);
        assert_eq!(cfg, reloaded);

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn custom_zone_legacy_array_and_trigger_deserialize() {
        let json = r#"[
            [0.0, 0.0, 0.5, 1.0],
            { "bounds": [0.15, 0.0, 0.85, 1.0], "trigger": [0.42, 0.25, 0.58, 0.75] }
        ]"#;
        let parsed: Vec<CustomZone> = serde_json::from_str(json).expect("deserialize mixed zones");
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0], CustomZone::simple([0.0, 0.0, 0.5, 1.0]));
        assert_eq!(
            parsed[1],
            CustomZone::with_trigger([0.15, 0.0, 0.85, 1.0], [0.42, 0.25, 0.58, 0.75])
        );
    }
}
