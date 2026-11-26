use crate::hotkeys::HotkeySpec;
use crate::monitor::MonitorInfo;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LayoutMode {
    DynamicGrid { cols: u32, rows: u32 },
    CustomZones { zones: Vec<[f32; 4]> },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Layout {
    pub name: String,
    pub mode: LayoutMode,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct OverlayStyle {
    pub highlight_color: [u8; 3],
    pub highlight_opacity: f32,
    pub border_color: [u8; 3],
    pub zone_bg_opacity: f32,
}

impl Default for OverlayStyle {
    fn default() -> Self {
        Self {
            highlight_color: [56, 189, 248],
            highlight_opacity: 0.35,
            border_color: [56, 189, 248],
            zone_bg_opacity: 0.14,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct BehaviorConfig {
    pub restore_on_unsnap: bool,
    pub excluded_apps: Vec<String>,
}

impl Default for BehaviorConfig {
    fn default() -> Self {
        Self {
            restore_on_unsnap: true,
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
                    [0.0, 0.0, 1.0 / 3.0, 1.0],
                    [1.0 / 3.0, 0.0, 2.0 / 3.0, 1.0],
                    [2.0 / 3.0, 0.0, 1.0, 1.0],
                ],
            },
        },
        Layout {
            name: "Focus + Stack".to_string(),
            mode: LayoutMode::CustomZones {
                zones: vec![
                    [0.0, 0.0, 0.65, 1.0],
                    [0.65, 0.0, 1.0, 0.5],
                    [0.65, 0.5, 1.0, 1.0],
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
        if let Ok(raw) = std::fs::read_to_string(path) {
            if let Ok(mut cfg) = serde_json::from_str::<Config>(&raw) {
                cfg.ensure_monitors(monitors);
                return cfg;
            }
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
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, &json).is_ok() {
            if std::fs::rename(&tmp, path).is_ok() {
                return Ok(());
            }
            let _ = std::fs::remove_file(&tmp);
        }
        std::fs::write(path, json)
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
        assert_eq!(partial.hotkeys.move_resize_modifier, "Alt");
        assert!(partial.monitors.contains_key(r"\\.\DISPLAY1_2560x1440"));

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }
}
