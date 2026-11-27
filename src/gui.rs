use crate::config::{
    config_path, format_hex_rgb, parse_hex_rgb, Config, CustomZone, GuiTheme, Layout, LayoutMode,
};
use crate::hotkeys::{is_mouse_vk, modifier_to_vk, HotkeySpec};
use crate::monitor::{enumerate_monitors, MonitorInfo};
use crate::startup::{self, StartupMode, StartupStatus};
use eframe::egui::{self, Color32, Pos2, Rect as EguiRect, Sense, Stroke, Vec2};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE, HWND, LPARAM, WPARAM};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowExW, FindWindowW, PostMessageW, SetForegroundWindow, ShowWindow, HWND_MESSAGE,
    SW_RESTORE, WM_CLOSE,
};

const SETTINGS_TITLE: &str = "ZoneSnap \u{2014} Settings & Zone Editor";
const REPO_URL: &str = "https://github.com/PlatyLord/zonesnap-rs";

fn parse_gui_scale_input(s: &str) -> Option<f64> {
    let cleaned = s.trim().trim_end_matches('%').trim();
    let v = cleaned.parse::<f64>().ok()?;
    if !v.is_finite() || v <= 0.0 {
        return None;
    }
    Some(if v > 3.0 { v / 100.0 } else { v })
}

fn contrasting_text_color(bg: Color32) -> Color32 {
    let linearize = |c: u8| -> f32 {
        let s = c as f32 / 255.0;
        if s <= 0.04045 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        }
    };
    let lum = 0.2126 * linearize(bg.r()) + 0.7152 * linearize(bg.g()) + 0.0722 * linearize(bg.b());
    if lum > 0.20 {
        Color32::from_rgb(15, 23, 42)
    } else {
        Color32::WHITE
    }
}

fn paint_color_swatch(ui: &mut egui::Ui, fill: Color32, border: Color32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(12.0), Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, 2.5, fill);
    p.rect_stroke(rect, 2.5, Stroke::new(1.5_f32, border));
}

fn open_config_folder() -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    let path = config_path().map_err(|e| format!("Could not resolve config path: {e}"))?;
    let mut cmd = std::process::Command::new("explorer.exe");
    if path.exists() {
        cmd.raw_arg(format!("/select,\"{}\"", path.display()));
    } else if let Some(parent) = path.parent() {
        cmd.arg(parent);
    }
    cmd.spawn()
        .map_err(|e| format!("Failed to open Explorer: {e}"))?;
    Ok(())
}

fn folder_icon_button(ui: &mut egui::Ui, palette: ThemePalette) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(24.0, 20.0), Sense::click());
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }

    let bg = if resp.is_pointer_button_down_on() {
        palette.widget_active
    } else if resp.hovered() {
        palette.widget_hovered
    } else {
        palette.widget_bg
    };
    let border_col = if resp.hovered() {
        palette.accent
    } else {
        palette.border
    };
    let icon_col = if resp.hovered() {
        palette.accent
    } else {
        palette.text
    };

    let p = ui.painter();
    p.rect_filled(rect, 4.0, bg);
    p.rect_stroke(rect, 4.0, Stroke::new(1.0_f32, border_col));

    let c = rect.center();
    let folder_pts = vec![
        Pos2::new(c.x - 6.5, c.y - 4.5),
        Pos2::new(c.x - 1.5, c.y - 4.5),
        Pos2::new(c.x + 0.0, c.y - 2.5),
        Pos2::new(c.x + 6.5, c.y - 2.5),
        Pos2::new(c.x + 6.5, c.y + 4.5),
        Pos2::new(c.x - 6.5, c.y + 4.5),
    ];
    p.add(egui::Shape::closed_line(
        folder_pts,
        Stroke::new(1.3_f32, icon_col),
    ));
    p.line_segment(
        [
            Pos2::new(c.x - 6.5, c.y - 2.5),
            Pos2::new(c.x + 0.0, c.y - 2.5),
        ],
        Stroke::new(1.1_f32, icon_col),
    );

    resp.on_hover_text("Open containing folder")
}

fn rect_overlap_area(a: EguiRect, b: EguiRect) -> f32 {
    let inter = a.intersect(b);
    if inter.is_positive() {
        inter.width() * inter.height()
    } else {
        0.0
    }
}

fn pick_non_overlapping_center(
    sr: EguiRect,
    pill_size: Vec2,
    placed: &[EguiRect],
    avoid: &[EguiRect],
) -> Pos2 {
    let pad = 10.0_f32;
    let half_w = pill_size.x * 0.5;
    let half_h = pill_size.y * 0.5;
    let min_x = (sr.left() + half_w + pad).min(sr.center().x);
    let max_x = (sr.right() - half_w - pad).max(sr.center().x);
    let min_y = (sr.top() + half_h + pad).min(sr.center().y);
    let max_y = (sr.bottom() - half_h - pad).max(sr.center().y);

    let candidates = [
        sr.center(),
        Pos2::new(min_x, min_y),
        Pos2::new(max_x, min_y),
        Pos2::new(min_x, max_y),
        Pos2::new(max_x, max_y),
        Pos2::new(sr.center().x, min_y),
        Pos2::new(sr.center().x, max_y),
        Pos2::new(min_x, sr.center().y),
        Pos2::new(max_x, sr.center().y),
    ];

    let mut best_pos = sr.center();
    let mut best_area = f32::INFINITY;
    for pos in candidates {
        let clamped = Pos2::new(pos.x.clamp(min_x, max_x), pos.y.clamp(min_y, max_y));
        let cand_rect = EguiRect::from_center_size(clamped, pill_size);
        let area: f32 = placed
            .iter()
            .chain(avoid.iter())
            .map(|r| rect_overlap_area(cand_rect, *r))
            .sum();
        if area <= 0.0 {
            return clamped;
        }
        if area < best_area {
            best_area = area;
            best_pos = clamped;
        }
    }
    best_pos
}

#[derive(Debug, Clone, Copy)]
struct ThemePalette {
    dark_mode: bool,
    panel_bg: Color32,
    surface_bg: Color32,
    extreme_bg: Color32,
    widget_bg: Color32,
    widget_hovered: Color32,
    widget_active: Color32,
    border: Color32,
    text: Color32,
    muted_text: Color32,
    accent: Color32,
    accent_text: Color32,
    warn: Color32,
    success: Color32,
    error: Color32,
    canvas_bg: Color32,
    canvas_grid: Color32,
    canvas_label: Color32,
    canvas_label_active: Color32,
}

fn theme_palette(theme: GuiTheme) -> ThemePalette {
    match theme {
        GuiTheme::Dark => ThemePalette {
            dark_mode: true,
            panel_bg: Color32::from_rgb(20, 24, 33),
            surface_bg: Color32::from_rgb(28, 33, 46),
            extreme_bg: Color32::from_rgb(13, 17, 25),
            widget_bg: Color32::from_rgb(38, 45, 61),
            widget_hovered: Color32::from_rgb(51, 61, 82),
            widget_active: Color32::from_rgb(63, 75, 100),
            border: Color32::from_rgb(71, 85, 105),
            text: Color32::from_rgb(226, 232, 240),
            muted_text: Color32::from_rgb(148, 163, 184),
            accent: Color32::from_rgb(56, 189, 248),
            accent_text: Color32::from_rgb(15, 23, 42),
            warn: Color32::from_rgb(251, 191, 36),
            success: Color32::from_rgb(34, 197, 94),
            error: Color32::from_rgb(239, 68, 68),
            canvas_bg: Color32::from_rgb(15, 23, 42),
            canvas_grid: Color32::from_rgba_unmultiplied(203, 213, 225, 80),
            canvas_label: Color32::from_rgb(203, 213, 225),
            canvas_label_active: Color32::WHITE,
        },
        GuiTheme::Light => ThemePalette {
            dark_mode: false,
            panel_bg: Color32::from_rgb(241, 245, 249),
            surface_bg: Color32::from_rgb(248, 250, 252),
            extreme_bg: Color32::WHITE,
            widget_bg: Color32::from_rgb(226, 232, 240),
            widget_hovered: Color32::from_rgb(203, 213, 225),
            widget_active: Color32::from_rgb(186, 200, 216),
            border: Color32::from_rgb(148, 163, 184),
            text: Color32::from_rgb(15, 23, 42),
            muted_text: Color32::from_rgb(71, 85, 105),
            accent: Color32::from_rgb(2, 132, 199),
            accent_text: Color32::WHITE,
            warn: Color32::from_rgb(180, 83, 9),
            success: Color32::from_rgb(21, 128, 61),
            error: Color32::from_rgb(185, 28, 28),
            canvas_bg: Color32::from_rgb(226, 232, 240),
            canvas_grid: Color32::from_rgba_unmultiplied(51, 65, 85, 80),
            canvas_label: Color32::from_rgb(51, 65, 85),
            canvas_label_active: Color32::from_rgb(15, 23, 42),
        },
        GuiTheme::CatppuccinMocha => ThemePalette {
            dark_mode: true,
            panel_bg: Color32::from_rgb(30, 30, 46),
            surface_bg: Color32::from_rgb(24, 24, 37),
            extreme_bg: Color32::from_rgb(17, 17, 27),
            widget_bg: Color32::from_rgb(49, 50, 68),
            widget_hovered: Color32::from_rgb(69, 71, 90),
            widget_active: Color32::from_rgb(88, 91, 112),
            border: Color32::from_rgb(108, 112, 134),
            text: Color32::from_rgb(205, 214, 244),
            muted_text: Color32::from_rgb(166, 173, 200),
            accent: Color32::from_rgb(203, 166, 247),
            accent_text: Color32::from_rgb(17, 17, 27),
            warn: Color32::from_rgb(249, 226, 175),
            success: Color32::from_rgb(166, 227, 161),
            error: Color32::from_rgb(243, 139, 168),
            canvas_bg: Color32::from_rgb(17, 17, 27),
            canvas_grid: Color32::from_rgba_unmultiplied(166, 173, 200, 80),
            canvas_label: Color32::from_rgb(186, 194, 222),
            canvas_label_active: Color32::from_rgb(205, 214, 244),
        },
        GuiTheme::CatppuccinMacchiato => ThemePalette {
            dark_mode: true,
            panel_bg: Color32::from_rgb(36, 39, 58),
            surface_bg: Color32::from_rgb(30, 32, 48),
            extreme_bg: Color32::from_rgb(24, 25, 38),
            widget_bg: Color32::from_rgb(54, 58, 79),
            widget_hovered: Color32::from_rgb(73, 77, 100),
            widget_active: Color32::from_rgb(91, 96, 120),
            border: Color32::from_rgb(110, 115, 141),
            text: Color32::from_rgb(202, 211, 245),
            muted_text: Color32::from_rgb(165, 173, 203),
            accent: Color32::from_rgb(198, 160, 246),
            accent_text: Color32::from_rgb(24, 25, 38),
            warn: Color32::from_rgb(238, 212, 159),
            success: Color32::from_rgb(166, 218, 149),
            error: Color32::from_rgb(237, 135, 150),
            canvas_bg: Color32::from_rgb(24, 25, 38),
            canvas_grid: Color32::from_rgba_unmultiplied(165, 173, 203, 80),
            canvas_label: Color32::from_rgb(184, 192, 224),
            canvas_label_active: Color32::from_rgb(202, 211, 245),
        },
        GuiTheme::CatppuccinFrappe => ThemePalette {
            dark_mode: true,
            panel_bg: Color32::from_rgb(48, 52, 70),
            surface_bg: Color32::from_rgb(41, 44, 60),
            extreme_bg: Color32::from_rgb(35, 38, 52),
            widget_bg: Color32::from_rgb(65, 69, 89),
            widget_hovered: Color32::from_rgb(81, 87, 109),
            widget_active: Color32::from_rgb(98, 104, 128),
            border: Color32::from_rgb(115, 121, 148),
            text: Color32::from_rgb(198, 208, 245),
            muted_text: Color32::from_rgb(165, 173, 206),
            accent: Color32::from_rgb(202, 158, 230),
            accent_text: Color32::from_rgb(35, 38, 52),
            warn: Color32::from_rgb(229, 200, 144),
            success: Color32::from_rgb(166, 209, 137),
            error: Color32::from_rgb(231, 130, 132),
            canvas_bg: Color32::from_rgb(35, 38, 52),
            canvas_grid: Color32::from_rgba_unmultiplied(165, 173, 206, 80),
            canvas_label: Color32::from_rgb(181, 191, 226),
            canvas_label_active: Color32::from_rgb(198, 208, 245),
        },
        GuiTheme::CatppuccinLatte => ThemePalette {
            dark_mode: false,
            panel_bg: Color32::from_rgb(239, 241, 245),
            surface_bg: Color32::from_rgb(230, 233, 239),
            extreme_bg: Color32::from_rgb(220, 224, 232),
            widget_bg: Color32::from_rgb(204, 208, 218),
            widget_hovered: Color32::from_rgb(188, 192, 204),
            widget_active: Color32::from_rgb(172, 176, 190),
            border: Color32::from_rgb(140, 143, 161),
            text: Color32::from_rgb(76, 79, 105),
            muted_text: Color32::from_rgb(108, 111, 133),
            accent: Color32::from_rgb(136, 57, 239),
            accent_text: Color32::WHITE,
            warn: Color32::from_rgb(223, 142, 29),
            success: Color32::from_rgb(64, 160, 43),
            error: Color32::from_rgb(210, 15, 57),
            canvas_bg: Color32::from_rgb(220, 224, 232),
            canvas_grid: Color32::from_rgba_unmultiplied(108, 111, 133, 80),
            canvas_label: Color32::from_rgb(92, 95, 119),
            canvas_label_active: Color32::from_rgb(76, 79, 105),
        },
        GuiTheme::Nord => ThemePalette {
            dark_mode: true,
            panel_bg: Color32::from_rgb(46, 52, 64),
            surface_bg: Color32::from_rgb(59, 66, 82),
            extreme_bg: Color32::from_rgb(36, 41, 51),
            widget_bg: Color32::from_rgb(67, 76, 94),
            widget_hovered: Color32::from_rgb(76, 86, 106),
            widget_active: Color32::from_rgb(94, 106, 129),
            border: Color32::from_rgb(97, 110, 136),
            text: Color32::from_rgb(236, 239, 244),
            muted_text: Color32::from_rgb(216, 222, 233),
            accent: Color32::from_rgb(136, 192, 208),
            accent_text: Color32::from_rgb(46, 52, 64),
            warn: Color32::from_rgb(235, 203, 139),
            success: Color32::from_rgb(163, 190, 140),
            error: Color32::from_rgb(191, 97, 106),
            canvas_bg: Color32::from_rgb(36, 41, 51),
            canvas_grid: Color32::from_rgba_unmultiplied(216, 222, 233, 75),
            canvas_label: Color32::from_rgb(216, 222, 233),
            canvas_label_active: Color32::from_rgb(236, 239, 244),
        },
        GuiTheme::TokyoNight => ThemePalette {
            dark_mode: true,
            panel_bg: Color32::from_rgb(26, 27, 38),
            surface_bg: Color32::from_rgb(36, 40, 59),
            extreme_bg: Color32::from_rgb(22, 22, 30),
            widget_bg: Color32::from_rgb(41, 46, 66),
            widget_hovered: Color32::from_rgb(59, 66, 97),
            widget_active: Color32::from_rgb(84, 92, 126),
            border: Color32::from_rgb(86, 95, 137),
            text: Color32::from_rgb(192, 202, 245),
            muted_text: Color32::from_rgb(169, 177, 214),
            accent: Color32::from_rgb(122, 162, 247),
            accent_text: Color32::from_rgb(22, 22, 30),
            warn: Color32::from_rgb(224, 175, 104),
            success: Color32::from_rgb(158, 206, 106),
            error: Color32::from_rgb(247, 118, 142),
            canvas_bg: Color32::from_rgb(22, 22, 30),
            canvas_grid: Color32::from_rgba_unmultiplied(169, 177, 214, 75),
            canvas_label: Color32::from_rgb(169, 177, 214),
            canvas_label_active: Color32::from_rgb(192, 202, 245),
        },
        GuiTheme::GruvboxDark => ThemePalette {
            dark_mode: true,
            panel_bg: Color32::from_rgb(40, 40, 40),
            surface_bg: Color32::from_rgb(50, 48, 47),
            extreme_bg: Color32::from_rgb(29, 32, 33),
            widget_bg: Color32::from_rgb(60, 56, 54),
            widget_hovered: Color32::from_rgb(80, 73, 69),
            widget_active: Color32::from_rgb(102, 92, 84),
            border: Color32::from_rgb(124, 111, 100),
            text: Color32::from_rgb(235, 219, 178),
            muted_text: Color32::from_rgb(189, 174, 147),
            accent: Color32::from_rgb(250, 189, 47),
            accent_text: Color32::from_rgb(29, 32, 33),
            warn: Color32::from_rgb(254, 128, 25),
            success: Color32::from_rgb(184, 187, 38),
            error: Color32::from_rgb(251, 73, 52),
            canvas_bg: Color32::from_rgb(29, 32, 33),
            canvas_grid: Color32::from_rgba_unmultiplied(213, 196, 161, 75),
            canvas_label: Color32::from_rgb(213, 196, 161),
            canvas_label_active: Color32::from_rgb(251, 241, 199),
        },
        GuiTheme::SolarizedLight => ThemePalette {
            dark_mode: false,
            panel_bg: Color32::from_rgb(253, 246, 227),
            surface_bg: Color32::from_rgb(238, 232, 213),
            extreme_bg: Color32::from_rgb(255, 250, 236),
            widget_bg: Color32::from_rgb(230, 223, 202),
            widget_hovered: Color32::from_rgb(214, 207, 185),
            widget_active: Color32::from_rgb(198, 191, 170),
            border: Color32::from_rgb(147, 161, 161),
            text: Color32::from_rgb(88, 110, 117),
            muted_text: Color32::from_rgb(131, 148, 150),
            accent: Color32::from_rgb(38, 139, 210),
            accent_text: Color32::from_rgb(253, 246, 227),
            warn: Color32::from_rgb(181, 137, 0),
            success: Color32::from_rgb(133, 153, 0),
            error: Color32::from_rgb(220, 50, 47),
            canvas_bg: Color32::from_rgb(238, 232, 213),
            canvas_grid: Color32::from_rgba_unmultiplied(88, 110, 117, 75),
            canvas_label: Color32::from_rgb(88, 110, 117),
            canvas_label_active: Color32::from_rgb(7, 54, 66),
        },
    }
}

fn apply_theme(ctx: &egui::Context, theme: GuiTheme) {
    let p = theme_palette(theme);
    let mut visuals = if p.dark_mode {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };

    visuals.window_fill = p.panel_bg;
    visuals.panel_fill = p.panel_bg;
    visuals.faint_bg_color = p.surface_bg;
    visuals.extreme_bg_color = p.extreme_bg;
    visuals.override_text_color = Some(p.text);
    visuals.hyperlink_color = p.accent;
    visuals.warn_fg_color = p.warn;
    visuals.error_fg_color = p.error;

    let sel_alpha = if p.dark_mode { 75 } else { 55 };
    visuals.selection.bg_fill =
        Color32::from_rgba_unmultiplied(p.accent.r(), p.accent.g(), p.accent.b(), sel_alpha);
    visuals.selection.stroke = Stroke::new(1.0_f32, p.accent);

    visuals.widgets.noninteractive.bg_fill = p.surface_bg;
    visuals.widgets.noninteractive.weak_bg_fill = p.surface_bg;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, p.border);
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, p.text);

    visuals.widgets.inactive.bg_fill = p.widget_bg;
    visuals.widgets.inactive.weak_bg_fill = p.widget_bg;
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, p.border);
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, p.text);

    visuals.widgets.hovered.bg_fill = p.widget_hovered;
    visuals.widgets.hovered.weak_bg_fill = p.widget_hovered;
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, p.accent);
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.5_f32, p.text);

    visuals.widgets.active.bg_fill = p.widget_active;
    visuals.widgets.active.weak_bg_fill = p.widget_active;
    visuals.widgets.active.bg_stroke = Stroke::new(1.5_f32, p.accent);
    visuals.widgets.active.fg_stroke = Stroke::new(1.5_f32, p.text);

    visuals.widgets.open.bg_fill = p.widget_hovered;
    visuals.widgets.open.weak_bg_fill = p.widget_hovered;
    visuals.widgets.open.bg_stroke = Stroke::new(1.0_f32, p.accent);
    visuals.widgets.open.fg_stroke = Stroke::new(1.0_f32, p.text);

    ctx.set_visuals(visuals);
}

struct MutexGuard(HANDLE);
impl Drop for MutexGuard {
    fn drop(&mut self) {
        if !self.0.is_invalid() && !self.0.0.is_null() {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SettingsTab {
    Layouts,
    Hotkeys,
    Appearance,
    About,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResizeHandle {
    NW,
    N,
    NE,
    E,
    SE,
    S,
    SW,
    W,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ZoneEditTarget {
    Bounds,
    Trigger,
}

#[derive(Debug, Clone)]
enum CanvasDrag {
    None,
    Drawing {
        start_norm: [f32; 2],
        cur_norm: [f32; 2],
    },
    Moving {
        idx: usize,
        target: ZoneEditTarget,
        grab_offset: [f32; 2],
        orig: [f32; 4],
    },
    Resizing {
        idx: usize,
        target: ZoneEditTarget,
        handle: ResizeHandle,
        orig: [f32; 4],
    },
}

struct SettingsApp {
    config: Config,
    saved_config: Config,
    monitors: Vec<MonitorInfo>,
    selected_monitor_key: String,
    selected_layout_idx: usize,
    selected_zone_idx: Option<usize>,
    zone_edit_target: ZoneEditTarget,
    canvas_drag: CanvasDrag,
    snap_1pct: bool,
    tab: SettingsTab,
    new_excluded_app: String,
    applied_gui_scale: f32,
    applied_theme: Option<GuiTheme>,
    edge_handle_hex: String,
    about_icon_tex: Option<egui::TextureHandle>,
    about_avatar_tex: Option<egui::TextureHandle>,
    status_msg: Option<String>,
    status_is_error: bool,
    reload_msg: u32,
    startup_status: StartupStatus,
    startup_feedback: Option<(String, bool)>,
}

impl SettingsApp {
    fn new(config: Config, monitors: Vec<MonitorInfo>, reload_msg: u32) -> Self {
        let selected_monitor_key = monitors
            .iter()
            .find(|m| m.is_primary)
            .or_else(|| monitors.first())
            .map(|m| m.profile_key())
            .or_else(|| config.monitors.keys().next().cloned())
            .unwrap_or_default();

        let selected_layout_idx = config
            .monitors
            .get(&selected_monitor_key)
            .map(|p| p.active_layout_index)
            .unwrap_or(0);

        let applied_gui_scale = config.appearance.gui_scale.clamp(0.85, 1.75);
        let edge_handle_hex = format_hex_rgb(config.edge_resize.handle_color);
        let saved_config = config.clone();
        let startup_status = startup::query_startup_status();
        Self {
            config,
            saved_config,
            monitors,
            selected_monitor_key,
            selected_layout_idx,
            selected_zone_idx: Some(0),
            zone_edit_target: ZoneEditTarget::Bounds,
            canvas_drag: CanvasDrag::None,
            snap_1pct: true,
            tab: SettingsTab::Layouts,
            new_excluded_app: String::new(),
            applied_gui_scale,
            applied_theme: None,
            edge_handle_hex,
            about_icon_tex: None,
            about_avatar_tex: None,
            status_msg: None,
            status_is_error: false,
            reload_msg,
            startup_status,
            startup_feedback: None,
        }
    }

    fn has_unsaved_changes(&self) -> bool {
        self.config != self.saved_config
    }

    fn save_and_notify_daemon(&mut self) -> bool {
        let path_str = config_path()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "zones.json".to_string());
        match self.config.save() {
            Ok(()) => {
                self.saved_config = self.config.clone();
                self.status_is_error = false;
                let mut reloaded = false;
                unsafe {
                    let mut daemon_hwnd = FindWindowW(w!("ZoneSnap_Daemon_MsgWindow"), PCWSTR::null())
                        .unwrap_or_default();
                    if daemon_hwnd.0.is_null() {
                        daemon_hwnd = FindWindowExW(
                            HWND_MESSAGE,
                            HWND::default(),
                            w!("ZoneSnap_Daemon_MsgWindow"),
                            PCWSTR::null(),
                        )
                        .unwrap_or_default();
                    }
                    if !daemon_hwnd.0.is_null() {
                        let _ = PostMessageW(daemon_hwnd, self.reload_msg, WPARAM(0), LPARAM(0));
                        reloaded = true;
                    }
                }
                self.status_msg = Some(if reloaded {
                    format!("Saved to {path_str} and applied to ZoneSnap.")
                } else {
                    format!("Saved to {path_str}.")
                });
                true
            }
            Err(e) => {
                self.status_is_error = true;
                self.status_msg = Some(format!("Failed to save {path_str}: {e}"));
                false
            }
        }
    }

    fn reload_from_disk(&mut self) {
        self.monitors = enumerate_monitors();
        self.config = Config::load_or_create(&self.monitors);
        self.saved_config = self.config.clone();
        self.edge_handle_hex = format_hex_rgb(self.config.edge_resize.handle_color);
        self.status_is_error = false;
        if !self.config.monitors.contains_key(&self.selected_monitor_key) {
            if let Some(first) = self.config.monitors.keys().next().cloned() {
                self.selected_monitor_key = first;
            }
        }
        self.selected_layout_idx = self
            .config
            .monitors
            .get(&self.selected_monitor_key)
            .map(|p| p.active_layout_index)
            .unwrap_or(0);
        self.selected_zone_idx = Some(0);
        self.applied_gui_scale = self.config.appearance.gui_scale.clamp(0.85, 1.75);
        self.applied_theme = None;
        self.status_msg = Some("Reloaded configuration from zones.json.".to_string());
    }

    fn snap_val(&self, v: f32) -> f32 {
        let clamped = v.clamp(0.0, 1.0);
        if self.snap_1pct {
            (clamped * 100.0).round() / 100.0
        } else {
            clamped
        }
    }

    fn render_layouts_tab(&mut self, ui: &mut egui::Ui) {
        let keys: Vec<String> = self.config.monitors.keys().cloned().collect();
        if keys.is_empty() {
            ui.label("No monitor profiles found.");
            return;
        }
        if !self.config.monitors.contains_key(&self.selected_monitor_key) {
            self.selected_monitor_key = keys[0].clone();
        }

        ui.horizontal(|ui| {
            ui.label("Monitor:");
            let label_for = |k: &str| -> String {
                if let Some(m) = self.monitors.iter().find(|m| m.profile_key() == k) {
                    let pri = if m.is_primary { " [Primary]" } else { "" };
                    format!("{} ({}x{}){}", m.device_name, m.rc_monitor.width(), m.rc_monitor.height(), pri)
                } else if let Some(p) = self.config.monitors.get(k) {
                    format!("{} ({}x{}) [Disconnected]", p.device_name, p.width, p.height)
                } else {
                    k.to_string()
                }
            };

            egui::ComboBox::from_id_salt("monitor_select")
                .selected_text(label_for(&self.selected_monitor_key))
                .show_ui(ui, |ui| {
                    for k in &keys {
                        if ui
                            .selectable_label(*k == self.selected_monitor_key, label_for(k))
                            .clicked()
                        {
                            self.selected_monitor_key = k.clone();
                            self.selected_layout_idx = self
                                .config
                                .monitors
                                .get(k)
                                .map(|p| p.active_layout_index)
                                .unwrap_or(0);
                            self.selected_zone_idx = Some(0);
                            self.canvas_drag = CanvasDrag::None;
                        }
                    }
                });
        });

        ui.separator();

        let (mon_w, mon_h, layout_count, active_idx) = {
            let prof = self.config.monitors.get(&self.selected_monitor_key).unwrap();
            (prof.width, prof.height, prof.layouts.len(), prof.active_layout_index)
        };
        if self.selected_layout_idx >= layout_count {
            self.selected_layout_idx = layout_count.saturating_sub(1);
        }

        // Layout selection & management toolbar (selecting a layout makes it active for this monitor)
        let mut clicked_layout: Option<usize> = None;
        ui.horizontal_wrapped(|ui| {
            let layout_names: Vec<String> = self
                .config
                .monitors
                .get(&self.selected_monitor_key)
                .map(|p| p.layouts.iter().map(|l| l.name.clone()).collect())
                .unwrap_or_default();

            for (idx, name) in layout_names.iter().enumerate() {
                let marker = if idx == active_idx { " ★" } else { "" };
                let text = format!("{name}{marker}");
                if ui
                    .selectable_label(idx == self.selected_layout_idx, text)
                    .clicked()
                {
                    clicked_layout = Some(idx);
                }
            }

            if ui.button("+ Add Layout").clicked() {
                if let Some(prof) = self.config.monitors.get_mut(&self.selected_monitor_key) {
                    let n = prof.layouts.len() + 1;
                    prof.layouts.push(Layout {
                        name: format!("Custom Layout {n}"),
                        mode: LayoutMode::CustomZones {
                            zones: vec![
                                CustomZone::simple([0.0, 0.0, 0.5, 1.0]),
                                CustomZone::simple([0.5, 0.0, 1.0, 1.0]),
                            ],
                        },
                    });
                    let new_idx = prof.layouts.len() - 1;
                    prof.active_layout_index = new_idx;
                    self.selected_layout_idx = new_idx;
                    self.selected_zone_idx = Some(0);
                    self.canvas_drag = CanvasDrag::None;
                }
            }
        });

        let other_monitors: Vec<(String, String)> = self
            .config
            .monitors
            .iter()
            .filter(|(k, _)| *k != &self.selected_monitor_key)
            .map(|(k, p)| (k.clone(), format!("{} ({}x{})", p.device_name, p.width, p.height)))
            .collect();

        let palette = theme_palette(self.config.appearance.gui_theme);
        let Some(prof) = self.config.monitors.get_mut(&self.selected_monitor_key) else {
            return;
        };
        if let Some(idx) = clicked_layout {
            if idx < prof.layouts.len() {
                prof.active_layout_index = idx;
                self.selected_layout_idx = idx;
                self.selected_zone_idx = Some(0);
                self.canvas_drag = CanvasDrag::None;
            }
        }
        if prof.layouts.is_empty() {
            return;
        }
        let sel_idx = self.selected_layout_idx;
        let can_delete = prof.layouts.len() > 1;

        let mut delete_layout = false;
        let mut duplicate_layout = false;
        let mut copy_to_monitor: Option<String> = None;

        ui.horizontal_wrapped(|ui| {
            ui.label("Name:");
            ui.add(egui::TextEdit::singleline(&mut prof.layouts[sel_idx].name).desired_width(180.0));

            let is_grid = matches!(prof.layouts[sel_idx].mode, LayoutMode::DynamicGrid { .. });
            if ui.radio(is_grid, "Dynamic Grid").clicked() && !is_grid {
                prof.layouts[sel_idx].mode = LayoutMode::DynamicGrid { cols: 12, rows: 6 };
            }
            if ui.radio(!is_grid, "Custom Zones").clicked() && is_grid {
                prof.layouts[sel_idx].mode = LayoutMode::CustomZones {
                    zones: vec![
                        CustomZone::simple([0.0, 0.0, 0.5, 1.0]),
                        CustomZone::simple([0.5, 0.0, 1.0, 1.0]),
                    ],
                };
                self.selected_zone_idx = Some(0);
            }

            ui.colored_label(palette.accent, "★ Active");
            if ui.button("Duplicate").clicked() {
                duplicate_layout = true;
            }
            if !other_monitors.is_empty() {
                egui::ComboBox::from_id_salt("copy_to_monitor")
                    .selected_text("Copy to Monitor...")
                    .show_ui(ui, |ui| {
                        for (dest_key, dest_label) in &other_monitors {
                            if ui.selectable_label(false, dest_label).clicked() {
                                copy_to_monitor = Some(dest_key.clone());
                            }
                        }
                    });
            }
            if ui
                .add_enabled(can_delete, egui::Button::new("Delete Layout"))
                .clicked()
            {
                delete_layout = true;
            }
        });

        if duplicate_layout {
            let mut dup = prof.layouts[sel_idx].clone();
            dup.name = format!("{} (Copy)", dup.name);
            prof.layouts.push(dup);
            let new_idx = prof.layouts.len() - 1;
            prof.active_layout_index = new_idx;
            self.selected_layout_idx = new_idx;
            self.selected_zone_idx = Some(0);
            return;
        }

        if let Some(dest_key) = copy_to_monitor {
            let cloned_layout = prof.layouts[sel_idx].clone();
            if let Some(dest_prof) = self.config.monitors.get_mut(&dest_key) {
                let mut new_name = cloned_layout.name.clone();
                while dest_prof.layouts.iter().any(|l| l.name == new_name) {
                    new_name.push_str(" (Copy)");
                }
                let dest_label = format!("{} ({}x{})", dest_prof.device_name, dest_prof.width, dest_prof.height);
                dest_prof.layouts.push(Layout {
                    name: new_name.clone(),
                    mode: cloned_layout.mode,
                });
                self.status_is_error = false;
                self.status_msg = Some(format!(
                    "Copied '{new_name}' to {dest_label}. Click Save & Apply to apply."
                ));
            }
            return;
        }

        if delete_layout && prof.layouts.len() > 1 {
            prof.layouts.remove(sel_idx);
            if prof.active_layout_index >= prof.layouts.len() {
                prof.active_layout_index = prof.layouts.len() - 1;
            }
            self.selected_layout_idx = sel_idx.min(prof.layouts.len() - 1);
            self.selected_zone_idx = Some(0);
            return;
        }

        ui.add_space(4.0);

        let mut current_layout = prof.layouts[sel_idx].clone();
        match &mut current_layout.mode {
            LayoutMode::DynamicGrid { cols, rows } => {
                ui.horizontal(|ui| {
                    ui.label("Columns:");
                    ui.add(egui::Slider::new(cols, 2..=24));
                    ui.add_space(16.0);
                    ui.label("Rows:");
                    ui.add(egui::Slider::new(rows, 2..=16));
                });
                ui.add_space(6.0);
                self.draw_grid_preview(ui, mon_w, mon_h, *cols, *rows);
            }
            LayoutMode::CustomZones { zones } => {
                self.draw_custom_zones_editor(ui, mon_w, mon_h, zones);
            }
        }
        if let Some(prof) = self.config.monitors.get_mut(&self.selected_monitor_key) {
            if sel_idx < prof.layouts.len() {
                prof.layouts[sel_idx] = current_layout;
            }
        }
    }

    fn draw_grid_preview(&self, ui: &mut egui::Ui, mon_w: i32, mon_h: i32, cols: u32, rows: u32) {
        let aspect = (mon_w.max(1) as f32) / (mon_h.max(1) as f32);
        let avail = ui.available_size();
        let max_h = (avail.y - 20.0).max(200.0);
        let mut w = avail.x.min(max_h * aspect);
        let mut h = w / aspect;
        if h > max_h {
            h = max_h;
            w = h * aspect;
        }
        let palette = theme_palette(self.config.appearance.gui_theme);
        let (rect, _) = ui.allocate_exact_size(Vec2::new(w, h), Sense::hover());
        let p = ui.painter_at(rect);
        p.rect_filled(rect, 4.0, palette.canvas_bg);
        p.rect_stroke(rect, 4.0, Stroke::new(1.5_f32, palette.border));

        let hi = self.config.appearance.highlight_color;
        let fill = Color32::from_rgba_unmultiplied(hi[0], hi[1], hi[2], 70);
        let border = Color32::from_rgb(
            self.config.appearance.border_color[0],
            self.config.appearance.border_color[1],
            self.config.appearance.border_color[2],
        );

        let cols = cols.max(1);
        let rows = rows.max(1);
        for c in 1..cols {
            let x = rect.left() + rect.width() * (c as f32 / cols as f32);
            p.line_segment(
                [Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())],
                Stroke::new(1.0_f32, palette.canvas_grid),
            );
        }
        for r in 1..rows {
            let y = rect.top() + rect.height() * (r as f32 / rows as f32);
            p.line_segment(
                [Pos2::new(rect.left(), y), Pos2::new(rect.right(), y)],
                Stroke::new(1.0_f32, palette.canvas_grid),
            );
        }

        // Sample highlight span on top-left quarter
        let sample_c = (cols / 2).max(1);
        let sample_r = (rows / 2).max(1);
        let sample_rect = EguiRect::from_min_max(
            rect.min,
            Pos2::new(
                rect.left() + rect.width() * (sample_c as f32 / cols as f32),
                rect.top() + rect.height() * (sample_r as f32 / rows as f32),
            ),
        );
        p.rect_filled(sample_rect, 0.0, fill);
        p.rect_stroke(sample_rect, 0.0, Stroke::new(2.0_f32, border));

        let label = format!("{sample_c} \u{00D7} {sample_r}");
        let galley = p.layout_no_wrap(
            label,
            egui::FontId::proportional(13.0),
            palette.canvas_label_active,
        );
        let pill_size = galley.size() + Vec2::new(14.0, 8.0);
        let pill_rect = EguiRect::from_center_size(sample_rect.center(), pill_size);
        let pill_bg = Color32::from_rgba_unmultiplied(
            palette.extreme_bg.r(),
            palette.extreme_bg.g(),
            palette.extreme_bg.b(),
            232,
        );
        p.rect_filled(pill_rect, 5.0, pill_bg);
        p.rect_stroke(pill_rect, 5.0, Stroke::new(1.5_f32, border));
        p.galley(
            Pos2::new(
                pill_rect.center().x - galley.size().x * 0.5,
                pill_rect.center().y - galley.size().y * 0.5,
            ),
            galley,
            palette.canvas_label_active,
        );
    }

    fn draw_custom_zones_editor(
        &mut self,
        ui: &mut egui::Ui,
        mon_w: i32,
        mon_h: i32,
        zones: &mut Vec<CustomZone>,
    ) {
        if let Some(idx) = self.selected_zone_idx {
            if idx >= zones.len() {
                self.selected_zone_idx = if zones.is_empty() { None } else { Some(zones.len() - 1) };
            }
        } else if !zones.is_empty() {
            self.selected_zone_idx = Some(0);
        }

        if let Some(idx) = self.selected_zone_idx {
            if let Some(z) = zones.get(idx) {
                if !z.has_custom_trigger() {
                    self.zone_edit_target = ZoneEditTarget::Bounds;
                }
            }
        }

        ui.horizontal_wrapped(|ui| {
            if ui.button("Add Zone").clicked() {
                zones.push(CustomZone::simple([0.1, 0.1, 0.5, 0.5]));
                self.selected_zone_idx = Some(zones.len() - 1);
                self.zone_edit_target = ZoneEditTarget::Bounds;
            }
            let has_sel = self.selected_zone_idx.is_some();
            if ui
                .add_enabled(has_sel, egui::Button::new("Split V (L|R)"))
                .clicked()
            {
                if let Some(idx) = self.selected_zone_idx {
                    let b = zones[idx].bounds;
                    let mid_x = (b[0] + b[2]) * 0.5;
                    zones[idx] = CustomZone::simple([b[0], b[1], mid_x, b[3]]);
                    zones.insert(idx + 1, CustomZone::simple([mid_x, b[1], b[2], b[3]]));
                    self.zone_edit_target = ZoneEditTarget::Bounds;
                }
            }
            if ui
                .add_enabled(has_sel, egui::Button::new("Split H (T/B)"))
                .clicked()
            {
                if let Some(idx) = self.selected_zone_idx {
                    let b = zones[idx].bounds;
                    let mid_y = (b[1] + b[3]) * 0.5;
                    zones[idx] = CustomZone::simple([b[0], b[1], b[2], mid_y]);
                    zones.insert(idx + 1, CustomZone::simple([b[0], mid_y, b[2], b[3]]));
                    self.zone_edit_target = ZoneEditTarget::Bounds;
                }
            }
            if ui
                .add_enabled(has_sel && zones.len() > 1, egui::Button::new("Delete Zone"))
                .clicked()
            {
                if let Some(idx) = self.selected_zone_idx {
                    zones.remove(idx);
                    self.selected_zone_idx = if zones.is_empty() {
                        None
                    } else {
                        Some(idx.min(zones.len() - 1))
                    };
                    self.zone_edit_target = ZoneEditTarget::Bounds;
                }
            }
            ui.separator();
            ui.label("Presets:");
            if ui.button("2 Cols").clicked() {
                *zones = vec![
                    CustomZone::simple([0.0, 0.0, 0.5, 1.0]),
                    CustomZone::simple([0.5, 0.0, 1.0, 1.0]),
                ];
                self.selected_zone_idx = Some(0);
                self.zone_edit_target = ZoneEditTarget::Bounds;
            }
            if ui.button("3 Cols").clicked() {
                *zones = vec![
                    CustomZone::simple([0.0, 0.0, 1.0 / 3.0, 1.0]),
                    CustomZone::simple([1.0 / 3.0, 0.0, 2.0 / 3.0, 1.0]),
                    CustomZone::simple([2.0 / 3.0, 0.0, 1.0, 1.0]),
                ];
                self.selected_zone_idx = Some(0);
                self.zone_edit_target = ZoneEditTarget::Bounds;
            }
            if ui.button("2x2").clicked() {
                *zones = vec![
                    CustomZone::simple([0.0, 0.0, 0.5, 0.5]),
                    CustomZone::simple([0.5, 0.0, 1.0, 0.5]),
                    CustomZone::simple([0.0, 0.5, 0.5, 1.0]),
                    CustomZone::simple([0.5, 0.5, 1.0, 1.0]),
                ];
                self.selected_zone_idx = Some(0);
                self.zone_edit_target = ZoneEditTarget::Bounds;
            }
            if ui.button("Focus+Stack").clicked() {
                *zones = vec![
                    CustomZone::simple([0.0, 0.0, 0.65, 1.0]),
                    CustomZone::simple([0.65, 0.0, 1.0, 0.5]),
                    CustomZone::simple([0.65, 0.5, 1.0, 1.0]),
                ];
                self.selected_zone_idx = Some(0);
                self.zone_edit_target = ZoneEditTarget::Bounds;
            }
            if ui.button("Center Focus + Sides").clicked() {
                *zones = vec![
                    CustomZone::simple([0.0, 0.0, 0.5, 1.0]),
                    CustomZone::simple([0.5, 0.0, 1.0, 1.0]),
                    CustomZone::with_trigger([0.15, 0.0, 0.85, 1.0], [0.42, 0.25, 0.58, 0.75]),
                ];
                self.selected_zone_idx = Some(2);
                self.zone_edit_target = ZoneEditTarget::Trigger;
            }
            ui.checkbox(&mut self.snap_1pct, "Snap 1%");
        });

        ui.label(
            egui::RichText::new("Tip: Drag empty space to draw a zone, drag a zone to move it, or enable 'Custom Trigger Area' below to decouple the trigger pad from the snapped output.")
                .small()
                .weak(),
        );

        let aspect = (mon_w.max(1) as f32) / (mon_h.max(1) as f32);
        let avail = ui.available_size();
        let max_h = (avail.y - 96.0).max(190.0);
        let mut cw = avail.x.min(max_h * aspect);
        let mut ch = cw / aspect;
        if ch > max_h {
            ch = max_h;
            cw = ch * aspect;
        }

        let (canvas_rect, resp) =
            ui.allocate_exact_size(Vec2::new(cw, ch), Sense::click_and_drag());

        let to_screen = |norm: [f32; 4]| -> EguiRect {
            let x1 = canvas_rect.left() + norm[0] * canvas_rect.width();
            let y1 = canvas_rect.top() + norm[1] * canvas_rect.height();
            let x2 = canvas_rect.left() + norm[2] * canvas_rect.width();
            let y2 = canvas_rect.top() + norm[3] * canvas_rect.height();
            EguiRect::from_min_max(Pos2::new(x1, y1), Pos2::new(x2, y2))
        };

        let to_norm = |pos: Pos2| -> [f32; 2] {
            let nx = ((pos.x - canvas_rect.left()) / canvas_rect.width()).clamp(0.0, 1.0);
            let ny = ((pos.y - canvas_rect.top()) / canvas_rect.height()).clamp(0.0, 1.0);
            [nx, ny]
        };

        let handle_positions = |r: EguiRect| -> [(ResizeHandle, Pos2); 8] {
            [
                (ResizeHandle::NW, r.left_top()),
                (ResizeHandle::N, Pos2::new(r.center().x, r.top())),
                (ResizeHandle::NE, r.right_top()),
                (ResizeHandle::E, Pos2::new(r.right(), r.center().y)),
                (ResizeHandle::SE, r.right_bottom()),
                (ResizeHandle::S, Pos2::new(r.center().x, r.bottom())),
                (ResizeHandle::SW, r.left_bottom()),
                (ResizeHandle::W, Pos2::new(r.left(), r.center().y)),
            ]
        };

        let pick_zone_at = |zones: &[CustomZone], pos: Pos2| -> Option<(usize, ZoneEditTarget)> {
            // Check explicit trigger boxes first so compact center pads can be clicked over wide columns
            let mut trig_hit: Option<(usize, f32)> = None;
            for (i, z) in zones.iter().enumerate() {
                if let Some(tr) = z.trigger {
                    let sr = to_screen(tr);
                    if sr.contains(pos) {
                        let area = sr.width() * sr.height();
                        if trig_hit.map(|(_, a)| area < a).unwrap_or(true) {
                            trig_hit = Some((i, area));
                        }
                    }
                }
            }
            if let Some((idx, _)) = trig_hit {
                return Some((idx, ZoneEditTarget::Trigger));
            }
            let mut hit: Option<(usize, f32)> = None;
            for (i, z) in zones.iter().enumerate() {
                let sr = to_screen(z.bounds);
                if sr.contains(pos) {
                    let area = sr.width() * sr.height();
                    if hit.map(|(_, a)| area < a).unwrap_or(true) {
                        hit = Some((i, area));
                    }
                }
            }
            hit.map(|(idx, _)| (idx, ZoneEditTarget::Bounds))
        };

        if resp.drag_started() {
            if let Some(pos) = resp.interact_pointer_pos() {
                let mut started = false;
                if let Some(sel) = self.selected_zone_idx {
                    if let Some(z) = zones.get(sel) {
                        let (target, rect_norm) =
                            if self.zone_edit_target == ZoneEditTarget::Trigger {
                                if let Some(tr) = z.trigger {
                                    (ZoneEditTarget::Trigger, tr)
                                } else {
                                    (ZoneEditTarget::Bounds, z.bounds)
                                }
                            } else {
                                (ZoneEditTarget::Bounds, z.bounds)
                            };
                        let sr = to_screen(rect_norm);
                        for (h, hp) in handle_positions(sr) {
                            if pos.distance(hp) <= 10.0 {
                                self.canvas_drag = CanvasDrag::Resizing {
                                    idx: sel,
                                    target,
                                    handle: h,
                                    orig: rect_norm,
                                };
                                started = true;
                                break;
                            }
                        }
                    }
                }
                if !started {
                    if let Some((idx, target)) = pick_zone_at(zones, pos) {
                        self.selected_zone_idx = Some(idx);
                        self.zone_edit_target = target;
                        let n = to_norm(pos);
                        let orig = match target {
                            ZoneEditTarget::Trigger => zones[idx].effective_trigger(),
                            ZoneEditTarget::Bounds => zones[idx].bounds,
                        };
                        self.canvas_drag = CanvasDrag::Moving {
                            idx,
                            target,
                            grab_offset: [n[0] - orig[0], n[1] - orig[1]],
                            orig,
                        };
                    } else {
                        let n = to_norm(pos);
                        self.canvas_drag = CanvasDrag::Drawing {
                            start_norm: n,
                            cur_norm: n,
                        };
                    }
                }
            }
        } else if resp.clicked() {
            if let Some(pos) = resp.interact_pointer_pos() {
                if let Some((idx, target)) = pick_zone_at(zones, pos) {
                    self.selected_zone_idx = Some(idx);
                    self.zone_edit_target = target;
                }
            }
        }

        if resp.dragged() {
            if let Some(pos) = resp.interact_pointer_pos() {
                let cur = to_norm(pos);
                let snap_1pct = self.snap_1pct;
                let snap = |v: f32| -> f32 {
                    let clamped = v.clamp(0.0, 1.0);
                    if snap_1pct {
                        (clamped * 100.0).round() / 100.0
                    } else {
                        clamped
                    }
                };
                match &mut self.canvas_drag {
                    CanvasDrag::Drawing { cur_norm, .. } => {
                        *cur_norm = cur;
                    }
                    CanvasDrag::Moving {
                        idx,
                        target,
                        grab_offset,
                        orig,
                    } => {
                        let i = *idx;
                        let tgt = *target;
                        let w = orig[2] - orig[0];
                        let h = orig[3] - orig[1];
                        let nx1 = snap((cur[0] - grab_offset[0]).clamp(0.0, 1.0 - w));
                        let ny1 = snap((cur[1] - grab_offset[1]).clamp(0.0, 1.0 - h));
                        let next_rect = [nx1, ny1, (nx1 + w).min(1.0), (ny1 + h).min(1.0)];
                        if let Some(z) = zones.get_mut(i) {
                            match tgt {
                                ZoneEditTarget::Bounds => z.bounds = next_rect,
                                ZoneEditTarget::Trigger => z.trigger = Some(next_rect),
                            }
                        }
                    }
                    CanvasDrag::Resizing {
                        idx,
                        target,
                        handle,
                        orig,
                    } => {
                        let i = *idx;
                        let tgt = *target;
                        let handle = *handle;
                        let mut r = *orig;
                        let sx = snap(cur[0]);
                        let sy = snap(cur[1]);
                        const MIN_Z: f32 = 0.05;
                        match handle {
                            ResizeHandle::NW => {
                                r[0] = sx.min(r[2] - MIN_Z);
                                r[1] = sy.min(r[3] - MIN_Z);
                            }
                            ResizeHandle::N => {
                                r[1] = sy.min(r[3] - MIN_Z);
                            }
                            ResizeHandle::NE => {
                                r[2] = sx.max(r[0] + MIN_Z);
                                r[1] = sy.min(r[3] - MIN_Z);
                            }
                            ResizeHandle::E => {
                                r[2] = sx.max(r[0] + MIN_Z);
                            }
                            ResizeHandle::SE => {
                                r[2] = sx.max(r[0] + MIN_Z);
                                r[3] = sy.max(r[1] + MIN_Z);
                            }
                            ResizeHandle::S => {
                                r[3] = sy.max(r[1] + MIN_Z);
                            }
                            ResizeHandle::SW => {
                                r[0] = sx.min(r[2] - MIN_Z);
                                r[3] = sy.max(r[1] + MIN_Z);
                            }
                            ResizeHandle::W => {
                                r[0] = sx.min(r[2] - MIN_Z);
                            }
                        }
                        if let Some(z) = zones.get_mut(i) {
                            match tgt {
                                ZoneEditTarget::Bounds => z.bounds = r,
                                ZoneEditTarget::Trigger => z.trigger = Some(r),
                            }
                        }
                    }
                    CanvasDrag::None => {}
                }
            }
        }

        if resp.drag_stopped() {
            if let CanvasDrag::Drawing {
                start_norm,
                cur_norm,
            } = self.canvas_drag
            {
                let x1 = self.snap_val(start_norm[0].min(cur_norm[0]));
                let y1 = self.snap_val(start_norm[1].min(cur_norm[1]));
                let x2 = self.snap_val(start_norm[0].max(cur_norm[0]));
                let y2 = self.snap_val(start_norm[1].max(cur_norm[1]));
                if (x2 - x1) >= 0.05 && (y2 - y1) >= 0.05 {
                    zones.push(CustomZone::simple([x1, y1, x2, y2]));
                    self.selected_zone_idx = Some(zones.len() - 1);
                    self.zone_edit_target = ZoneEditTarget::Bounds;
                }
            }
            self.canvas_drag = CanvasDrag::None;
        }

        // Paint canvas
        let palette = theme_palette(self.config.appearance.gui_theme);
        let p = ui.painter_at(canvas_rect);
        p.rect_filled(canvas_rect, 4.0, palette.canvas_bg);

        // Subtle 4x4 alignment guides on the editor canvas
        for step in 1..4 {
            let frac = step as f32 / 4.0;
            let gx = canvas_rect.left() + canvas_rect.width() * frac;
            let gy = canvas_rect.top() + canvas_rect.height() * frac;
            let guide_col = Color32::from_rgba_unmultiplied(
                palette.canvas_grid.r(),
                palette.canvas_grid.g(),
                palette.canvas_grid.b(),
                40,
            );
            p.line_segment(
                [Pos2::new(gx, canvas_rect.top()), Pos2::new(gx, canvas_rect.bottom())],
                Stroke::new(1.0_f32, guide_col),
            );
            p.line_segment(
                [Pos2::new(canvas_rect.left(), gy), Pos2::new(canvas_rect.right(), gy)],
                Stroke::new(1.0_f32, guide_col),
            );
        }
        p.rect_stroke(
            canvas_rect,
            4.0,
            Stroke::new(1.5_f32, palette.border),
        );

        let hi = self.config.appearance.highlight_color;
        let bd = self.config.appearance.border_color;
        let tr_rgb = self.config.appearance.trigger_color;
        let bd_col = Color32::from_rgb(bd[0], bd[1], bd[2]);
        let tr_col = Color32::from_rgb(tr_rgb[0], tr_rgb[1], tr_rgb[2]);

        let unsel_fill = if palette.dark_mode {
            Color32::from_rgba_unmultiplied(
                palette.widget_bg.r(),
                palette.widget_bg.g(),
                palette.widget_bg.b(),
                120,
            )
        } else {
            Color32::from_rgba_unmultiplied(
                palette.surface_bg.r(),
                palette.surface_bg.g(),
                palette.surface_bg.b(),
                150,
            )
        };
        let sel_fill = Color32::from_rgba_unmultiplied(
            hi[0],
            hi[1],
            hi[2],
            if palette.dark_mode { 64 } else { 52 },
        );

        // Pass 1: Unselected zone bounds
        for (i, z) in zones.iter().enumerate() {
            if self.selected_zone_idx == Some(i) {
                continue;
            }
            let sr = to_screen(z.bounds).shrink(2.0);
            p.rect_filled(sr, 2.0, unsel_fill);
            p.rect_stroke(sr, 2.0, Stroke::new(1.5_f32, palette.border));
        }

        // Pass 2: Selected zone bounds on top
        if let Some(sel) = self.selected_zone_idx {
            if let Some(z) = zones.get(sel) {
                let sr = to_screen(z.bounds).shrink(2.0);
                let bw = if self.zone_edit_target == ZoneEditTarget::Bounds {
                    2.5_f32
                } else {
                    1.75_f32
                };
                p.rect_filled(sr, 2.0, sel_fill);
                p.rect_stroke(sr, 2.0, Stroke::new(bw, bd_col));
            }
        }

        // Collect trigger rects so bounds label pills avoid overlapping trigger pads
        let trigger_rects: Vec<EguiRect> = zones
            .iter()
            .filter_map(|z| z.trigger.map(|tr| to_screen(tr).shrink(1.0)))
            .collect();

        // Pass 3: High-contrast zone bounds label pills with anti-collision placement
        let mut placed_pills: Vec<EguiRect> = Vec::with_capacity(zones.len());
        for (i, z) in zones.iter().enumerate() {
            let sr = to_screen(z.bounds).shrink(2.0);
            let is_sel = self.selected_zone_idx == Some(i);
            let pw = ((z.bounds[2] - z.bounds[0]) * mon_w as f32).round() as i32;
            let ph = ((z.bounds[3] - z.bounds[1]) * mon_h as f32).round() as i32;
            let label = format!("{}\n{}x{}", i + 1, pw, ph);
            let text_col = if is_sel {
                palette.canvas_label_active
            } else {
                palette.canvas_label
            };
            let galley = p.layout_no_wrap(label, egui::FontId::proportional(12.5), text_col);
            let pill_size = galley.size() + Vec2::new(12.0, 6.0);
            let center = pick_non_overlapping_center(sr, pill_size, &placed_pills, &trigger_rects);
            let pill_rect = EguiRect::from_center_size(center, pill_size);
            placed_pills.push(pill_rect);

            let pill_bg = Color32::from_rgba_unmultiplied(
                palette.extreme_bg.r(),
                palette.extreme_bg.g(),
                palette.extreme_bg.b(),
                if is_sel { 238 } else { 225 },
            );
            let pill_border = if is_sel {
                Stroke::new(1.5_f32, bd_col)
            } else {
                Stroke::new(1.0_f32, palette.border)
            };
            p.rect_filled(pill_rect, 4.0, pill_bg);
            p.rect_stroke(pill_rect, 4.0, pill_border);
            p.galley(
                Pos2::new(
                    pill_rect.center().x - galley.size().x * 0.5,
                    pill_rect.center().y - galley.size().y * 0.5,
                ),
                galley,
                text_col,
            );
        }

        // Pass 4: Trigger pads (unselected first, then selected on top)
        for pass_selected in [false, true] {
            for (i, z) in zones.iter().enumerate() {
                let is_sel = self.selected_zone_idx == Some(i);
                if is_sel != pass_selected {
                    continue;
                }
                let Some(tr) = z.trigger else {
                    continue;
                };
                let tr_rect = to_screen(tr).shrink(1.0);
                let tr_fill = Color32::from_rgba_unmultiplied(
                    tr_col.r(),
                    tr_col.g(),
                    tr_col.b(),
                    if is_sel { 88 } else { 54 },
                );
                let tr_stroke_w = if is_sel {
                    if self.zone_edit_target == ZoneEditTarget::Trigger {
                        2.75_f32
                    } else {
                        2.25_f32
                    }
                } else {
                    1.75_f32
                };
                p.rect_filled(tr_rect, 2.0, tr_fill);
                p.rect_stroke(tr_rect, 2.0, Stroke::new(tr_stroke_w, tr_col));

                let badge_text_col = contrasting_text_color(tr_col);
                let galley = p.layout_no_wrap(
                    format!("Trigger {}", i + 1),
                    egui::FontId::proportional(11.5),
                    badge_text_col,
                );
                let pill_size = galley.size() + Vec2::new(10.0, 5.0);
                let pill_rect = EguiRect::from_center_size(tr_rect.center(), pill_size);
                let pill_bg = Color32::from_rgba_unmultiplied(tr_col.r(), tr_col.g(), tr_col.b(), 235);
                p.rect_filled(pill_rect, 4.0, pill_bg);
                p.galley(
                    Pos2::new(
                        pill_rect.center().x - galley.size().x * 0.5,
                        pill_rect.center().y - galley.size().y * 0.5,
                    ),
                    galley,
                    badge_text_col,
                );
            }
        }

        // Pass 5: Resize handles always topmost for selected zone's active target
        if let Some(sel) = self.selected_zone_idx {
            if let Some(z) = zones.get(sel) {
                let handle_rect = if self.zone_edit_target == ZoneEditTarget::Trigger {
                    z.trigger.unwrap_or(z.bounds)
                } else {
                    z.bounds
                };
                let raw_sr = to_screen(handle_rect);
                let handle_col = if self.zone_edit_target == ZoneEditTarget::Trigger {
                    tr_col
                } else {
                    bd_col
                };
                for (_, hp) in handle_positions(raw_sr) {
                    p.circle_filled(hp, 5.0, Color32::WHITE);
                    p.circle_stroke(hp, 5.0, Stroke::new(2.0_f32, handle_col));
                }
            }
        }

        // Pass 6: Drawing preview rect
        if let CanvasDrag::Drawing {
            start_norm,
            cur_norm,
        } = self.canvas_drag
        {
            let dr = to_screen([
                start_norm[0].min(cur_norm[0]),
                start_norm[1].min(cur_norm[1]),
                start_norm[0].max(cur_norm[0]),
                start_norm[1].max(cur_norm[1]),
            ]);
            p.rect_filled(
                dr,
                0.0,
                Color32::from_rgba_unmultiplied(hi[0], hi[1], hi[2], 70),
            );
            p.rect_stroke(dr, 0.0, Stroke::new(1.5_f32, palette.canvas_label_active));
        }

        // Custom Trigger toggle & numeric fine-tuning row for selected zone
        if let Some(sel) = self.selected_zone_idx {
            if let Some(z) = zones.get_mut(sel) {
                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| {
                    ui.label(egui::RichText::new(format!("Zone {}:", sel + 1)).strong());
                    let mut has_trigger = z.trigger.is_some();
                    if ui
                        .checkbox(&mut has_trigger, "Custom Trigger Area")
                        .changed()
                    {
                        if has_trigger {
                            let b = z.bounds;
                            let cx = (b[0] + b[2]) * 0.5;
                            let cy = (b[1] + b[3]) * 0.5;
                            let hw = ((b[2] - b[0]) * 0.25).max(0.06);
                            let hh = ((b[3] - b[1]) * 0.25).max(0.08);
                            z.trigger = Some([
                                (cx - hw).clamp(0.0, 0.9),
                                (cy - hh).clamp(0.0, 0.9),
                                (cx + hw).clamp(0.1, 1.0),
                                (cy + hh).clamp(0.1, 1.0),
                            ]);
                            self.zone_edit_target = ZoneEditTarget::Trigger;
                        } else {
                            z.trigger = None;
                            self.zone_edit_target = ZoneEditTarget::Bounds;
                        }
                    }
                    if z.trigger.is_some() {
                        ui.separator();
                        paint_color_swatch(
                            ui,
                            Color32::from_rgba_unmultiplied(hi[0], hi[1], hi[2], 180),
                            bd_col,
                        );
                        ui.selectable_value(
                            &mut self.zone_edit_target,
                            ZoneEditTarget::Bounds,
                            "Snap Output",
                        );
                        ui.add_space(4.0);
                        paint_color_swatch(ui, tr_col, tr_col);
                        ui.selectable_value(
                            &mut self.zone_edit_target,
                            ZoneEditTarget::Trigger,
                            "Trigger Pad",
                        );
                    }
                });

                let is_trigger_target =
                    self.zone_edit_target == ZoneEditTarget::Trigger && z.trigger.is_some();
                let (tag, active_rect) = match (self.zone_edit_target, &mut z.trigger) {
                    (ZoneEditTarget::Trigger, Some(tr)) => ("Trigger:", tr),
                    _ => ("Output:", &mut z.bounds),
                };
                ui.horizontal(|ui| {
                    let max_x1 = active_rect[2] - 0.05;
                    let max_y1 = active_rect[3] - 0.05;
                    let min_x2 = active_rect[0] + 0.05;
                    let min_y2 = active_rect[1] + 0.05;
                    if is_trigger_target {
                        paint_color_swatch(ui, tr_col, tr_col);
                    } else {
                        paint_color_swatch(
                            ui,
                            Color32::from_rgba_unmultiplied(hi[0], hi[1], hi[2], 180),
                            bd_col,
                        );
                    }
                    ui.label(tag);
                    ui.label("X1");
                    ui.add(egui::DragValue::new(&mut active_rect[0]).speed(0.005).range(0.0..=max_x1));
                    ui.label("Y1");
                    ui.add(egui::DragValue::new(&mut active_rect[1]).speed(0.005).range(0.0..=max_y1));
                    ui.label("X2");
                    ui.add(egui::DragValue::new(&mut active_rect[2]).speed(0.005).range(min_x2..=1.0));
                    ui.label("Y2");
                    ui.add(egui::DragValue::new(&mut active_rect[3]).speed(0.005).range(min_y2..=1.0));
                });
            }
        }
    }

    fn format_hotkey(spec: &HotkeySpec) -> String {
        if spec.modifiers.is_empty() {
            spec.key.clone()
        } else {
            format!("{} + {}", spec.modifiers.join(" + "), spec.key)
        }
    }

    fn render_hotkey_editor(
        ui: &mut egui::Ui,
        id_salt: &str,
        label: &str,
        description: &str,
        spec: &mut HotkeySpec,
    ) {
        ui.group(|ui| {
            ui.label(egui::RichText::new(label).strong());
            ui.label(egui::RichText::new(description).small().weak());
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                for m in ["Win", "Alt", "Ctrl", "Shift"] {
                    let mut present = spec
                        .modifiers
                        .iter()
                        .any(|s| s.eq_ignore_ascii_case(m));
                    if ui.checkbox(&mut present, m).changed() {
                        spec.modifiers.retain(|s| !s.eq_ignore_ascii_case(m));
                        if present {
                            spec.modifiers.push(m.to_string());
                        }
                    }
                }
                ui.add_space(12.0);
                ui.label("Key:");
                let mut key_choices: Vec<String> = ('A'..='Z').map(|c| c.to_string()).collect();
                key_choices.extend(('0'..='9').map(|c| c.to_string()));
                key_choices.extend((1..=12).map(|n| format!("F{n}")));
                key_choices.extend(["Space", "Tab", "Enter"].into_iter().map(String::from));

                egui::ComboBox::from_id_salt(id_salt)
                    .selected_text(&spec.key)
                    .show_ui(ui, |ui| {
                        for k in &key_choices {
                            ui.selectable_value(&mut spec.key, k.clone(), k);
                        }
                    });
            });
        });
    }

    fn render_hotkeys_tab(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                Self::render_hotkey_editor(
                    ui,
                    "hk_activate",
                    "Activate Zone Overlay (Keyboard Trigger)",
                    "Opens the zone overlay for the currently focused window without starting a mouse drag.",
                    &mut self.config.hotkeys.activate_overlay,
                );
                ui.add_space(8.0);
                Self::render_hotkey_editor(
                    ui,
                    "hk_settings",
                    "Open Settings & Zone Editor",
                    "Launches or brings this Settings & Visual Zone Editor window to the front.",
                    &mut self.config.hotkeys.open_settings,
                );
                ui.add_space(8.0);
                ui.group(|ui| {
                    ui.label(egui::RichText::new("Mouse Drag Modifiers").strong());
                    ui.horizontal(|ui| {
                        ui.label("Move / Resize Modifier:");
                        egui::ComboBox::from_id_salt("mod_move_resize")
                            .selected_text(&self.config.hotkeys.move_resize_modifier)
                            .show_ui(ui, |ui| {
                                for m in [
                                    "Alt",
                                    "Ctrl",
                                    "Shift",
                                    "Win",
                                    "Mouse Back",
                                    "Mouse Forward",
                                    "Middle Click",
                                ] {
                                    ui.selectable_value(
                                        &mut self.config.hotkeys.move_resize_modifier,
                                        m.to_string(),
                                        m,
                                    );
                                }
                            });
                        ui.label(
                            egui::RichText::new("Left-Drag moves window; Right-Drag resizes from edge/corner")
                                .small()
                                .weak(),
                        );
                    });
                    if is_mouse_vk(modifier_to_vk(&self.config.hotkeys.move_resize_modifier)) {
                        ui.label(
                            egui::RichText::new("Quick taps of this mouse button fire normally on release; hold + Left-Drag to move or Right-Drag to resize.")
                                .small()
                                .weak(),
                        );
                    }
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.label("Monitor-Lock Modifier:");
                        egui::ComboBox::from_id_salt("mod_lock")
                            .selected_text(&self.config.hotkeys.monitor_lock_modifier)
                            .show_ui(ui, |ui| {
                                for m in ["Ctrl", "Shift", "Alt", "Win"] {
                                    ui.selectable_value(
                                        &mut self.config.hotkeys.monitor_lock_modifier,
                                        m.to_string(),
                                        m,
                                    );
                                }
                            });
                        ui.label(
                            egui::RichText::new("Hold while dragging to clamp cursor inside the active monitor")
                                .small()
                                .weak(),
                        );
                    });
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.label("Nearest-Corner Resize Modifier:");
                        egui::ComboBox::from_id_salt("mod_corner_resize")
                            .selected_text(&self.config.hotkeys.corner_resize_modifier)
                            .show_ui(ui, |ui| {
                                for m in ["Shift", "Ctrl", "Alt", "Win", "Disabled"] {
                                    ui.selectable_value(
                                        &mut self.config.hotkeys.corner_resize_modifier,
                                        m.to_string(),
                                        m,
                                    );
                                }
                            });
                        ui.label(
                            egui::RichText::new("Hold with Move/Resize + Right-Drag to force 2x2 nearest-corner resize")
                                .small()
                                .weak(),
                        );
                    });
                    let warn_col = theme_palette(self.config.appearance.gui_theme).warn;
                    if self
                        .config
                        .hotkeys
                        .move_resize_modifier
                        .eq_ignore_ascii_case(&self.config.hotkeys.monitor_lock_modifier)
                    {
                        ui.colored_label(
                            warn_col,
                            "Warning: Move/Resize and Monitor-Lock modifiers are set to the same key.",
                        );
                    }
                    if !self
                        .config
                        .hotkeys
                        .corner_resize_modifier
                        .eq_ignore_ascii_case("Disabled")
                        && self
                            .config
                            .hotkeys
                            .move_resize_modifier
                            .eq_ignore_ascii_case(&self.config.hotkeys.corner_resize_modifier)
                    {
                        ui.colored_label(
                            warn_col,
                            "Warning: Move/Resize and Nearest-Corner Resize modifiers are set to the same key.",
                        );
                    }
                });

                ui.add_space(8.0);
                ui.group(|ui| {
                    ui.label(egui::RichText::new("Complete Controls & Shortcuts Reference").strong());
                    ui.label(
                        egui::RichText::new("Live reference reflecting your currently selected modifiers and hotkeys:")
                            .small()
                            .weak(),
                    );
                    ui.add_space(4.0);

                    let move_mod = &self.config.hotkeys.move_resize_modifier;
                    let lock_mod = &self.config.hotkeys.monitor_lock_modifier;
                    let corner_mod = &self.config.hotkeys.corner_resize_modifier;
                    let activate_hk = Self::format_hotkey(&self.config.hotkeys.activate_overlay);
                    let settings_hk = Self::format_hotkey(&self.config.hotkeys.open_settings);
                    let corner_combo = if corner_mod.eq_ignore_ascii_case("Disabled") {
                        "Disabled".to_string()
                    } else {
                        format!("{move_mod} + {corner_mod} + Right-Drag")
                    };

                    let rows: [(&str, String, &str); 14] = [
                        (
                            "Standard Drag + Snap",
                            "Drag Title Bar + Right-Click".to_string(),
                            "Suppresses context menu and opens zone overlay; release Left-Click to snap.",
                        ),
                        (
                            "Move Window Anywhere",
                            format!("{move_mod} + Left-Drag"),
                            "Smoothly moves any window from anywhere inside it without needing the title bar.",
                        ),
                        (
                            "Drag-to-Top Maximize",
                            format!("{move_mod} + Left-Drag to Top Edge"),
                            "Shows a Maximize preview at the top of any monitor; release Left-Click to maximize.",
                        ),
                        (
                            "Hand Off Move to Zones",
                            format!("{move_mod} + Left-Drag + Right-Click"),
                            "Opens the zone overlay mid-drag; release Left-Click to snap into highlighted zone.",
                        ),
                        (
                            "Edge / Corner Resize (3x3)",
                            format!("{move_mod} + Right-Drag"),
                            "Divides window into a 3x3 grid: side edges resize one side; corners/center resize two.",
                        ),
                        (
                            "Nearest-Corner Resize (2x2)",
                            corner_combo,
                            "Splits window into 2x2 quadrants and always resizes from the nearest corner.",
                        ),
                        (
                            "Monitor-Lock Cursor",
                            format!("Hold {lock_mod} while dragging"),
                            "Clamps cursor inside the active monitor's bounds so you can snap cleanly at shared edges.",
                        ),
                        (
                            "Cycle Monitor Layouts",
                            "Mouse Wheel (overlay open)".to_string(),
                            "Cycles through saved layouts on the cursor's monitor and shows a layout toast pill.",
                        ),
                        (
                            "Merge Custom Zones",
                            "Hold Shift or Hover Border (±16px)".to_string(),
                            "In Custom Zones mode, merges hovered zones or adjacent zones into their bounding box.",
                        ),
                        (
                            "Cancel Overlay",
                            "Esc or 2nd Right-Click".to_string(),
                            "Dismisses the overlay immediately without snapping so you can drop the window freely.",
                        ),
                        (
                            "Keyboard Zone Overlay",
                            format!("{activate_hk} -> Left-Click"),
                            "Opens overlay for the focused window; Left-Click snaps, Right-Click/Esc cancels.",
                        ),
                        (
                            "Open Settings & Editor",
                            format!("{settings_hk} or Tray Double-Click"),
                            "Opens or focuses this visual layout editor and settings window.",
                        ),
                        (
                            "Quick-Switch Layout",
                            "Tray Right-Click -> Switch Layout".to_string(),
                            "Switches the active layout for any connected monitor without opening Settings.",
                        ),
                        (
                            "Save Settings",
                            "Ctrl + S".to_string(),
                            "Writes zones.json beside zonesnap.exe and hot-reloads the background daemon.",
                        ),
                    ];

                    egui::Grid::new("shortcuts_ref_grid")
                        .striped(true)
                        .num_columns(3)
                        .spacing([14.0, 6.0])
                        .show(ui, |ui| {
                            ui.strong("Action");
                            ui.strong("Shortcut / Gesture");
                            ui.strong("How It Works");
                            ui.end_row();
                            for (action, shortcut, desc) in rows {
                                ui.label(egui::RichText::new(action).strong());
                                ui.monospace(shortcut);
                                ui.label(egui::RichText::new(desc).small());
                                ui.end_row();
                            }
                        });
                });
            });
    }

    fn render_about_tab(&mut self, ui: &mut egui::Ui) {
        let palette = theme_palette(self.config.appearance.gui_theme);
        if self.about_icon_tex.is_none() {
            let rgba = crate::icon::app_icon_rgba(64);
            let img = egui::ColorImage::from_rgba_unmultiplied([64, 64], &rgba);
            self.about_icon_tex = Some(ui.ctx().load_texture(
                "about_app_icon",
                img,
                egui::TextureOptions::LINEAR,
            ));
        }
        if self.about_avatar_tex.is_none() {
            let sz = crate::icon::AVATAR_SIZE as usize;
            let rgba = crate::icon::author_avatar_rgba();
            let img = egui::ColorImage::from_rgba_unmultiplied([sz, sz], &rgba);
            self.about_avatar_tex = Some(ui.ctx().load_texture(
                "about_author_avatar",
                img,
                egui::TextureOptions::LINEAR,
            ));
        }

        let path_str = config_path()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "zones.json".to_string());

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.add_space(12.0);
                let avail_w = ui.available_width();
                let card_w = (avail_w - 32.0).clamp(320.0, 560.0);
                let pad_x = ((avail_w - card_w) * 0.5).max(0.0);

                ui.horizontal(|ui| {
                    ui.add_space(pad_x);
                    ui.allocate_ui_with_layout(
                        Vec2::new(card_w, 0.0),
                        egui::Layout::top_down(egui::Align::Center),
                        |ui| {
                            if let Some(tex) = &self.about_icon_tex {
                                ui.image((tex.id(), Vec2::splat(56.0)));
                                ui.add_space(8.0);
                            }

                            ui.label(egui::RichText::new("ZoneSnap").size(24.0).strong());
                            ui.add_space(4.0);

                            egui::Frame::none()
                                .fill(palette.widget_bg)
                                .stroke(Stroke::new(1.0_f32, palette.accent))
                                .rounding(12.0)
                                .inner_margin(egui::Margin::symmetric(12.0, 3.0))
                                .show(ui, |ui| {
                                    ui.label(
                                        egui::RichText::new(format!(
                                            "v{}",
                                            env!("CARGO_PKG_VERSION")
                                        ))
                                        .small()
                                        .strong()
                                        .color(palette.accent),
                                    );
                                });

                            ui.add_space(6.0);
                            ui.label(
                                egui::RichText::new(
                                    "Lightweight, single-binary portable Windows window-snapping and window-management utility.",
                                )
                                .color(palette.muted_text),
                            );

                            ui.add_space(14.0);

                            let about_card = |ui: &mut egui::Ui, add_contents: &dyn Fn(&mut egui::Ui)| {
                                egui::Frame::group(ui.style())
                                    .fill(palette.surface_bg)
                                    .stroke(Stroke::new(1.0_f32, palette.border))
                                    .rounding(8.0)
                                    .inner_margin(egui::Margin::symmetric(18.0, 12.0))
                                    .show(ui, |ui| {
                                        ui.set_width((card_w - 36.0).max(200.0));
                                        ui.with_layout(
                                            egui::Layout::top_down(egui::Align::Center),
                                            |ui| add_contents(ui),
                                        );
                                    });
                            };

                            about_card(ui, &|ui| {
                                ui.label(egui::RichText::new("CREDITS").small().strong().color(palette.muted_text));
                                ui.add_space(6.0);
                                if let Some(avatar) = &self.about_avatar_tex {
                                    ui.image((avatar.id(), Vec2::splat(52.0)));
                                    ui.add_space(6.0);
                                }
                                ui.label(egui::RichText::new("Author: Tom (PlatyLord)").strong());
                                ui.label("Co-Built With: Antigravity (Google DeepMind)");
                            });

                            ui.add_space(10.0);

                            about_card(ui, &|ui| {
                                ui.label(egui::RichText::new("LICENSE").small().strong().color(palette.muted_text));
                                ui.add_space(4.0);
                                ui.label(
                                    egui::RichText::new("GNU General Public License v3.0 (GPL-3.0)")
                                        .strong()
                                        .color(palette.accent),
                                );
                                ui.add_space(2.0);
                                ui.label(
                                    egui::RichText::new(
                                        "Free to use, modify, and fork; derivative works must also be released under GPL-3.0. Provided as-is with no warranty.",
                                    )
                                    .small()
                                    .color(palette.muted_text),
                                );
                            });

                            ui.add_space(10.0);

                            about_card(ui, &|ui| {
                                ui.label(egui::RichText::new("PORTABILITY & PROJECT").small().strong().color(palette.muted_text));
                                ui.add_space(4.0);
                                ui.label(egui::RichText::new(&path_str).monospace().small());
                                ui.add_space(2.0);
                                ui.label(
                                    egui::RichText::new(
                                        "Zero installer, zero external DLL dependencies, and zero Windows Registry writes (Windows startup integration is strictly opt-in).",
                                    )
                                    .small()
                                    .color(palette.muted_text),
                                );
                                ui.add_space(4.0);
                                ui.hyperlink_to("github.com/PlatyLord/zonesnap-rs", REPO_URL);
                            });

                            ui.add_space(12.0);
                        },
                    );
                });
            });
    }

    fn render_appearance_tab(&mut self, ui: &mut egui::Ui) {
        let palette = theme_palette(self.config.appearance.gui_theme);
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.group(|ui| {
                    ui.label(egui::RichText::new("Settings Window Theme & Scaling").strong());
                    ui.horizontal_wrapped(|ui| {
                        ui.label("Theme:");
                        egui::ComboBox::from_id_salt("gui_theme_select")
                            .selected_text(self.config.appearance.gui_theme.label())
                            .show_ui(ui, |ui| {
                                for &t in GuiTheme::ALL {
                                    ui.selectable_value(
                                        &mut self.config.appearance.gui_theme,
                                        t,
                                        t.label(),
                                    );
                                }
                            });

                        let (quick_label, quick_target) = match self.config.appearance.gui_theme {
                            GuiTheme::CatppuccinMocha
                            | GuiTheme::CatppuccinMacchiato
                            | GuiTheme::CatppuccinFrappe => ("Switch to Latte", GuiTheme::CatppuccinLatte),
                            GuiTheme::CatppuccinLatte => ("Switch to Mocha", GuiTheme::CatppuccinMocha),
                            t if t.is_light() => ("Switch to Dark", GuiTheme::Dark),
                            _ => ("Switch to Light", GuiTheme::Light),
                        };
                        if ui.button(quick_label).clicked() {
                            self.config.appearance.gui_theme = quick_target;
                        }
                        if ui.button("Match Overlay Colors").clicked() {
                            let (hi, bd, tr) =
                                self.config.appearance.gui_theme.recommended_overlay_colors();
                            self.config.appearance.highlight_color = hi;
                            self.config.appearance.border_color = bd;
                            self.config.appearance.trigger_color = tr;
                            self.config.edge_resize.handle_color = bd;
                            self.edge_handle_hex = format_hex_rgb(bd);
                        }
                    });

                    ui.add_space(2.0);
                    ui.horizontal_wrapped(|ui| {
                        ui.label("Settings Window Text Scale:");
                        let slider_resp = ui.add(
                            egui::Slider::new(&mut self.config.appearance.gui_scale, 0.85..=1.75)
                                .custom_formatter(|v, _| format!("{:.0}%", v * 100.0))
                                .custom_parser(parse_gui_scale_input),
                        );
                        let pointer_down = ui.input(|i| i.pointer.any_down());
                        let editing_text = ui.ctx().wants_keyboard_input();
                        if !slider_resp.dragged() && !pointer_down && !editing_text {
                            let target = self.config.appearance.gui_scale.clamp(0.85, 1.75);
                            if (self.applied_gui_scale - target).abs() > 0.001 {
                                self.applied_gui_scale = target;
                            }
                        }
                        if ui.button("Reset 100%").clicked() {
                            self.config.appearance.gui_scale = 1.0;
                            self.applied_gui_scale = 1.0;
                        }
                        if (self.applied_gui_scale - self.config.appearance.gui_scale.clamp(0.85, 1.75)).abs() > 0.001 {
                            ui.label(
                                egui::RichText::new("(release to apply)")
                                    .small()
                                    .weak(),
                            );
                        } else {
                            ui.label(
                                egui::RichText::new("(Type e.g. 125 or Ctrl+ / Ctrl- / Ctrl+0)")
                                    .small()
                                    .weak(),
                            );
                        }
                    });
                });

                ui.add_space(8.0);
                ui.group(|ui| {
                    ui.label(egui::RichText::new("Overlay & Editor Colors").strong());
                    ui.horizontal_wrapped(|ui| {
                        ui.label("Zone Fill:");
                        ui.color_edit_button_srgb(&mut self.config.appearance.highlight_color);
                        ui.add_space(12.0);
                        ui.label("Zone Border:");
                        ui.color_edit_button_srgb(&mut self.config.appearance.border_color);
                        ui.add_space(12.0);
                        ui.label("Trigger Pad:");
                        ui.color_edit_button_srgb(&mut self.config.appearance.trigger_color);
                    });
                    ui.horizontal(|ui| {
                        ui.label("Highlight Opacity:");
                        ui.add(egui::Slider::new(
                            &mut self.config.appearance.highlight_opacity,
                            0.05..=0.85,
                        ));
                    });
                    ui.horizontal(|ui| {
                        ui.label("Zone Background Opacity:");
                        ui.add(egui::Slider::new(
                            &mut self.config.appearance.zone_bg_opacity,
                            0.0..=0.50,
                        ));
                    });
                    ui.horizontal(|ui| {
                        ui.label("Trigger Pad Opacity:");
                        ui.add(egui::Slider::new(
                            &mut self.config.appearance.trigger_opacity,
                            0.05..=0.85,
                        ));
                    });
                });

                ui.add_space(8.0);
                ui.group(|ui| {
                    ui.label(egui::RichText::new("Expanded Resize Borders & Grab Handles").strong());
                    ui.checkbox(
                        &mut self.config.edge_resize.enabled,
                        "Enable expanded window resize boundaries",
                    );
                    ui.label(
                        egui::RichText::new("Expands resize hit-boxes slightly outside the active window's borders; zero overhead when disabled.")
                            .small()
                            .weak(),
                    );
                    ui.add_enabled_ui(self.config.edge_resize.enabled, |ui| {
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            ui.label("Grab Padding (px):");
                            ui.add(egui::Slider::new(
                                &mut self.config.edge_resize.grab_padding_px,
                                4..=24,
                            ));
                        });
                        ui.checkbox(
                            &mut self.config.edge_resize.corners_only,
                            "Corners only (disable straight-edge grab zones)",
                        );
                        ui.checkbox(
                            &mut self.config.edge_resize.show_handles,
                            "Show visual colored circular handles on hover",
                        );
                        ui.add_enabled_ui(self.config.edge_resize.show_handles, |ui| {
                            ui.horizontal(|ui| {
                                ui.label("Handle Circle Size (px):");
                                ui.add(egui::Slider::new(
                                    &mut self.config.edge_resize.handle_size_px,
                                    12..=36,
                                ));
                            });
                            ui.horizontal(|ui| {
                                ui.label("Handle Color:");
                                if ui
                                    .color_edit_button_srgb(
                                        &mut self.config.edge_resize.handle_color,
                                    )
                                    .changed()
                                {
                                    self.edge_handle_hex =
                                        format_hex_rgb(self.config.edge_resize.handle_color);
                                }
                                ui.label("Hex:");
                                let hex_resp = ui.add(
                                    egui::TextEdit::singleline(&mut self.edge_handle_hex)
                                        .desired_width(84.0),
                                );
                                if hex_resp.changed() {
                                    if let Some(rgb) = parse_hex_rgb(&self.edge_handle_hex) {
                                        self.config.edge_resize.handle_color = rgb;
                                    }
                                } else if hex_resp.lost_focus() {
                                    self.edge_handle_hex =
                                        format_hex_rgb(self.config.edge_resize.handle_color);
                                }
                            });
                        });
                    });
                });

                ui.add_space(8.0);
                ui.group(|ui| {
                    ui.label(egui::RichText::new("Window Behavior").strong());
                    ui.checkbox(
                        &mut self.config.behavior.restore_on_unsnap,
                        "Restore original window size when dragging a snapped window away (unsnap)",
                    );
                    ui.checkbox(
                        &mut self.config.behavior.drag_top_maximize,
                        "Maximize window when Alt-dragging to the top edge of a monitor",
                    );
                });

                ui.add_space(8.0);
                ui.group(|ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(egui::RichText::new("Start with Windows").strong());
                        let (role_text, role_color) = if self.startup_status.running_as_admin {
                            ("Currently running as: Administrator", palette.accent)
                        } else {
                            ("Currently running as: Standard", palette.muted_text)
                        };
                        ui.label(egui::RichText::new(role_text).small().color(role_color));
                    });

                    let current_mode = self.startup_status.mode;
                    let mut requested_mode: Option<StartupMode> = None;
                    ui.horizontal_wrapped(|ui| {
                        for (mode, label) in [
                            (StartupMode::Off, "Off"),
                            (StartupMode::Standard, "Standard (Startup Folder)"),
                            (StartupMode::Administrator, "Administrator (Task Scheduler)"),
                        ] {
                            if ui.selectable_label(current_mode == mode, label).clicked()
                                && current_mode != mode
                            {
                                requested_mode = Some(mode);
                            }
                        }
                    });

                    if let Some(target_mode) = requested_mode {
                        match startup::apply_startup_mode(target_mode) {
                            Ok((st, msg)) => {
                                self.startup_status = st;
                                self.startup_feedback = Some((msg, false));
                            }
                            Err(err) => {
                                self.startup_status = startup::query_startup_status();
                                self.startup_feedback = Some((err, true));
                            }
                        }
                    }

                    ui.label(
                        egui::RichText::new(
                            "Applied immediately and stored in Windows (not saved in zones.json) so ZoneSnap stays portable.",
                        )
                        .small()
                        .weak(),
                    );

                    if self.startup_status.path_mismatch {
                        ui.add_space(4.0);
                        let warn_fill = Color32::from_rgba_unmultiplied(
                            palette.warn.r(),
                            palette.warn.g(),
                            palette.warn.b(),
                            24,
                        );
                        egui::Frame::none()
                            .fill(warn_fill)
                            .stroke(Stroke::new(1.0_f32, palette.warn))
                            .rounding(6.0)
                            .inner_margin(egui::Margin::symmetric(10.0, 8.0))
                            .show(ui, |ui| {
                                ui.label(
                                    egui::RichText::new(
                                        "Warning: Windows startup currently points to a different location:",
                                    )
                                    .strong()
                                    .color(palette.warn),
                                );
                                if let Some(reg) = &self.startup_status.registered_path {
                                    ui.label(
                                        egui::RichText::new(reg.display().to_string())
                                            .monospace()
                                            .small(),
                                    );
                                }
                                if let Some(cur) = &self.startup_status.current_exe {
                                    ui.label(
                                        egui::RichText::new(format!(
                                            "This copy: {}",
                                            cur.display()
                                        ))
                                        .monospace()
                                        .small()
                                        .weak(),
                                    );
                                }
                                ui.label(
                                    egui::RichText::new(
                                        "Because you moved zonesnap.exe, startup won't launch this copy until you update it.",
                                    )
                                    .small(),
                                );
                                ui.add_space(4.0);
                                if ui.button("Update to Current Location").clicked() {
                                    match startup::apply_startup_mode(self.startup_status.mode) {
                                        Ok((st, msg)) => {
                                            self.startup_status = st;
                                            self.startup_feedback = Some((msg, false));
                                        }
                                        Err(err) => {
                                            self.startup_status = startup::query_startup_status();
                                            self.startup_feedback = Some((err, true));
                                        }
                                    }
                                }
                            });
                    }

                    if self.startup_status.mode != StartupMode::Off {
                        ui.add_space(4.0);
                        if !self.startup_status.path_mismatch {
                            if let Some(reg) = &self.startup_status.registered_path {
                                ui.label(
                                    egui::RichText::new(format!(
                                        "Registered path: {}",
                                        reg.display()
                                    ))
                                    .monospace()
                                    .small()
                                    .weak(),
                                );
                            }
                        }

                        match self.startup_status.mode {
                            StartupMode::Standard => {
                                ui.label(
                                    egui::RichText::new(
                                        "Note: If you move zonesnap.exe to another folder, come back here to update it. Switch back to Off before deleting zonesnap.exe so no shortcut is left behind.",
                                    )
                                    .small()
                                    .weak(),
                                );
                                if self.startup_status.startup_folder.is_some()
                                    && ui.button("Open Startup Folder").clicked()
                                {
                                    if let Err(err) = startup::open_startup_folder() {
                                        self.startup_feedback = Some((err, true));
                                    }
                                }
                            }
                            StartupMode::Administrator => {
                                ui.label(
                                    egui::RichText::new(
                                        "Starts with Administrator rights at login (no UAC popup) so ZoneSnap can also snap Admin windows. If you move zonesnap.exe to another folder, come back here to update it—and switch back to Off before deleting zonesnap.exe so the scheduled task isn't left behind.",
                                    )
                                    .small()
                                    .weak(),
                                );
                            }
                            StartupMode::Off => {}
                        }
                    } else {
                        ui.label(
                            egui::RichText::new(
                                "Off by default. Standard adds a shortcut to shell:startup; Administrator uses Windows Task Scheduler so ZoneSnap can also manage elevated (Admin) windows.",
                            )
                            .small()
                            .weak(),
                        );
                    }

                    if let Some((msg, is_err)) = &self.startup_feedback {
                        ui.add_space(4.0);
                        let color = if *is_err {
                            palette.error
                        } else {
                            palette.success
                        };
                        ui.label(egui::RichText::new(msg).small().color(color));
                    }
                });

                ui.add_space(8.0);
                ui.group(|ui| {
                    ui.label(egui::RichText::new("Excluded Applications (excluded_apps)").strong());
                    ui.label(
                        egui::RichText::new("Mouse hooks pass through untouched for these process executables.")
                            .small()
                            .weak(),
                    );
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut self.new_excluded_app)
                                .hint_text("e.g. game.exe")
                                .desired_width(220.0),
                        );
                        if ui.button("Add").clicked() {
                            let norm = self.new_excluded_app.trim().to_ascii_lowercase();
                            if !norm.is_empty()
                                && !self
                                    .config
                                    .behavior
                                    .excluded_apps
                                    .iter()
                                    .any(|x| x.eq_ignore_ascii_case(&norm))
                            {
                                self.config.behavior.excluded_apps.push(norm);
                            }
                            self.new_excluded_app.clear();
                        }
                    });

                    ui.add_space(4.0);
                    let mut remove_idx: Option<usize> = None;
                    egui::ScrollArea::vertical()
                        .max_height(180.0)
                        .show(ui, |ui| {
                            for (i, app) in self.config.behavior.excluded_apps.iter().enumerate() {
                                ui.horizontal(|ui| {
                                    if ui.small_button("Remove").clicked() {
                                        remove_idx = Some(i);
                                    }
                                    ui.label(app);
                                });
                            }
                        });
                    if let Some(idx) = remove_idx {
                        self.config.behavior.excluded_apps.remove(idx);
                    }
                });
            });
    }
}

impl eframe::App for SettingsApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.applied_theme != Some(self.config.appearance.gui_theme) {
            apply_theme(ctx, self.config.appearance.gui_theme);
            self.applied_theme = Some(self.config.appearance.gui_theme);
        }
        let palette = theme_palette(self.config.appearance.gui_theme);

        if ctx.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::S)) {
            self.save_and_notify_daemon();
        }
        if ctx.input(|i| i.modifiers.ctrl && (i.key_pressed(egui::Key::Plus) || i.key_pressed(egui::Key::Equals))) {
            let next = (self.config.appearance.gui_scale + 0.05).clamp(0.85, 1.75);
            self.config.appearance.gui_scale = next;
            self.applied_gui_scale = next;
        }
        if ctx.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::Minus)) {
            let next = (self.config.appearance.gui_scale - 0.05).clamp(0.85, 1.75);
            self.config.appearance.gui_scale = next;
            self.applied_gui_scale = next;
        }
        if ctx.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::Num0)) {
            self.config.appearance.gui_scale = 1.0;
            self.applied_gui_scale = 1.0;
        }
        ctx.set_zoom_factor(self.applied_gui_scale.clamp(0.85, 1.75));

        egui::TopBottomPanel::top("tabs_bar").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.selectable_value(
                    &mut self.tab,
                    SettingsTab::Layouts,
                    "Layouts & Visual Zone Editor",
                );
                ui.selectable_value(&mut self.tab, SettingsTab::Hotkeys, "Shortcuts & Modifiers");
                ui.selectable_value(
                    &mut self.tab,
                    SettingsTab::Appearance,
                    "Appearance & Behavior",
                );
                ui.selectable_value(&mut self.tab, SettingsTab::About, "About & Credits");
            });
            ui.add_space(2.0);
        });

        egui::TopBottomPanel::bottom("footer_bar").show(ctx, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let save_btn = egui::Button::new(
                    egui::RichText::new("Save & Apply").color(palette.accent_text).strong(),
                )
                .fill(palette.accent);
                if ui.add(save_btn).clicked() {
                    self.save_and_notify_daemon();
                }
                if ui.button("Save & Close").clicked() && self.save_and_notify_daemon() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                if ui.button("Reload").clicked() {
                    self.reload_from_disk();
                }
                if self.has_unsaved_changes() {
                    ui.add_space(8.0);
                    ui.colored_label(
                        palette.warn,
                        "● Unsaved changes (Ctrl+S to save)",
                    );
                }
            });
            ui.horizontal_wrapped(|ui| {
                if folder_icon_button(ui, palette).clicked() {
                    if let Err(err) = open_config_folder() {
                        self.status_msg = Some(err);
                        self.status_is_error = true;
                    }
                }
                let path_str = config_path()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|_| "zones.json".to_string());
                ui.label(egui::RichText::new(format!("Config: {path_str}")).small().weak());
                if let Some(msg) = &self.status_msg {
                    ui.add_space(12.0);
                    let color = if self.status_is_error {
                        palette.error
                    } else {
                        palette.success
                    };
                    ui.colored_label(color, msg);
                }
            });
            ui.add_space(4.0);
        });

        egui::CentralPanel::default().show(ctx, |ui| match self.tab {
            SettingsTab::Layouts => self.render_layouts_tab(ui),
            SettingsTab::Hotkeys => self.render_hotkeys_tab(ui),
            SettingsTab::Appearance => self.render_appearance_tab(ui),
            SettingsTab::About => self.render_about_tab(ui),
        });
    }
}

/// Ask any open Settings & Zone Editor window to close cleanly.
pub fn close_settings_window() {
    let wide_title: Vec<u16> = SETTINGS_TITLE
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    unsafe {
        if let Ok(existing) = FindWindowW(PCWSTR::null(), PCWSTR(wide_title.as_ptr())) {
            if !existing.0.is_null() {
                let _ = PostMessageW(existing, WM_CLOSE, WPARAM(0), LPARAM(0));
            }
        }
    }
}

/// Launch the single-instance egui Settings & Visual Zone Editor window.
pub fn run_settings_window(reload_msg: u32) {
    let mutex_handle = unsafe { CreateMutexW(None, false, w!("Local\\ZoneSnap_Settings_Mutex")) };
    let Ok(handle) = mutex_handle else {
        return;
    };
    let _guard = MutexGuard(handle);
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        unsafe {
            let wide_title: Vec<u16> = SETTINGS_TITLE
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();
            if let Ok(existing) = FindWindowW(PCWSTR::null(), PCWSTR(wide_title.as_ptr())) {
                if !existing.0.is_null() {
                    let _ = ShowWindow(existing, SW_RESTORE);
                    let _ = SetForegroundWindow(existing);
                }
            }
        }
        return;
    }

    let monitors = enumerate_monitors();
    let config = Config::load_or_create(&monitors);

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([920.0, 660.0])
            .with_min_inner_size([720.0, 520.0])
            .with_icon(std::sync::Arc::new(egui::IconData {
                rgba: crate::icon::app_icon_rgba(64),
                width: 64,
                height: 64,
            })),
        ..Default::default()
    };

    let _ = eframe::run_native(
        SETTINGS_TITLE,
        options,
        Box::new(move |cc| {
            let mut style = (*cc.egui_ctx.style()).clone();
            style.text_styles.insert(
                egui::TextStyle::Body,
                egui::FontId::proportional(14.5),
            );
            style.text_styles.insert(
                egui::TextStyle::Button,
                egui::FontId::proportional(14.5),
            );
            style.text_styles.insert(
                egui::TextStyle::Monospace,
                egui::FontId::monospace(13.5),
            );
            style.text_styles.insert(
                egui::TextStyle::Small,
                egui::FontId::proportional(12.0),
            );
            style.text_styles.insert(
                egui::TextStyle::Heading,
                egui::FontId::proportional(21.0),
            );
            cc.egui_ctx.set_style(style);
            apply_theme(&cc.egui_ctx, config.appearance.gui_theme);
            cc.egui_ctx
                .set_zoom_factor(config.appearance.gui_scale.clamp(0.85, 1.75));
            Ok(Box::new(SettingsApp::new(config, monitors, reload_msg)))
        }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gui_scale_parser_accepts_percentages_and_decimals() {
        assert_eq!(parse_gui_scale_input("120"), Some(1.2));
        assert_eq!(parse_gui_scale_input("120%"), Some(1.2));
        assert_eq!(parse_gui_scale_input(" 150 % "), Some(1.5));
        assert_eq!(parse_gui_scale_input("1.25"), Some(1.25));
        assert_eq!(parse_gui_scale_input("85%"), Some(0.85));
        assert_eq!(parse_gui_scale_input(""), None);
        assert_eq!(parse_gui_scale_input("abc"), None);
        assert_eq!(parse_gui_scale_input("-10"), None);
    }

    #[test]
    fn all_themes_match_light_dark_polarity() {
        assert_eq!(GuiTheme::ALL.len(), 10);
        for &theme in GuiTheme::ALL {
            let p = theme_palette(theme);
            assert_eq!(p.dark_mode, !theme.is_light(), "mismatch for {:?}", theme);
            let (hi, _bd, tr) = theme.recommended_overlay_colors();
            assert_ne!(hi, tr, "highlight and trigger should differ for {:?}", theme);
        }
    }

    #[test]
    fn contrasting_text_color_picks_dark_on_bright_and_white_on_dark() {
        let dark = Color32::from_rgb(15, 23, 42);
        assert_eq!(contrasting_text_color(Color32::WHITE), dark);
        assert_eq!(contrasting_text_color(Color32::from_rgb(245, 158, 11)), dark);
        assert_eq!(contrasting_text_color(Color32::from_rgb(250, 179, 135)), dark);
        assert_eq!(contrasting_text_color(Color32::from_rgb(250, 189, 47)), dark);

        assert_eq!(contrasting_text_color(Color32::BLACK), Color32::WHITE);
        assert_eq!(contrasting_text_color(Color32::from_rgb(15, 23, 42)), Color32::WHITE);
        assert_eq!(contrasting_text_color(Color32::from_rgb(203, 75, 22)), Color32::WHITE);
    }
}
