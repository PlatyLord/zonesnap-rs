use crate::config::{config_path, Config, Layout, LayoutMode};
use crate::hotkeys::HotkeySpec;
use crate::monitor::{enumerate_monitors, MonitorInfo};
use eframe::egui::{self, Color32, Pos2, Rect as EguiRect, Sense, Stroke, Vec2};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE, HWND, LPARAM, WPARAM};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowExW, FindWindowW, PostMessageW, SetForegroundWindow, ShowWindow, HWND_MESSAGE,
    SW_RESTORE,
};

const SETTINGS_TITLE: &str = "ZoneSnap \u{2014} Settings & Zone Editor";

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

#[derive(Debug, Clone)]
enum CanvasDrag {
    None,
    Drawing { start_norm: [f32; 2], cur_norm: [f32; 2] },
    Moving { idx: usize, grab_offset: [f32; 2], orig: [f32; 4] },
    Resizing { idx: usize, handle: ResizeHandle, orig: [f32; 4] },
}

struct SettingsApp {
    config: Config,
    saved_config: Config,
    monitors: Vec<MonitorInfo>,
    selected_monitor_key: String,
    selected_layout_idx: usize,
    selected_zone_idx: Option<usize>,
    canvas_drag: CanvasDrag,
    snap_1pct: bool,
    tab: SettingsTab,
    new_excluded_app: String,
    status_msg: Option<String>,
    status_is_error: bool,
    reload_msg: u32,
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

        let saved_config = config.clone();
        Self {
            config,
            saved_config,
            monitors,
            selected_monitor_key,
            selected_layout_idx,
            selected_zone_idx: Some(0),
            canvas_drag: CanvasDrag::None,
            snap_1pct: true,
            tab: SettingsTab::Layouts,
            new_excluded_app: String::new(),
            status_msg: None,
            status_is_error: false,
            reload_msg,
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
                            zones: vec![[0.0, 0.0, 0.5, 1.0], [0.5, 0.0, 1.0, 1.0]],
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
                    zones: vec![[0.0, 0.0, 0.5, 1.0], [0.5, 0.0, 1.0, 1.0]],
                };
                self.selected_zone_idx = Some(0);
            }

            ui.colored_label(Color32::from_rgb(56, 189, 248), "★ Active");
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
        let (rect, _) = ui.allocate_exact_size(Vec2::new(w, h), Sense::hover());
        let p = ui.painter_at(rect);
        p.rect_filled(rect, 4.0, Color32::from_rgb(18, 24, 38));
        p.rect_stroke(rect, 4.0, Stroke::new(1.5_f32, Color32::from_rgb(71, 85, 105)));

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
                Stroke::new(1.0_f32, Color32::from_rgba_unmultiplied(203, 213, 225, 65)),
            );
        }
        for r in 1..rows {
            let y = rect.top() + rect.height() * (r as f32 / rows as f32);
            p.line_segment(
                [Pos2::new(rect.left(), y), Pos2::new(rect.right(), y)],
                Stroke::new(1.0_f32, Color32::from_rgba_unmultiplied(203, 213, 225, 65)),
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
    }

    fn draw_custom_zones_editor(
        &mut self,
        ui: &mut egui::Ui,
        mon_w: i32,
        mon_h: i32,
        zones: &mut Vec<[f32; 4]>,
    ) {
        if let Some(idx) = self.selected_zone_idx {
            if idx >= zones.len() {
                self.selected_zone_idx = if zones.is_empty() { None } else { Some(zones.len() - 1) };
            }
        } else if !zones.is_empty() {
            self.selected_zone_idx = Some(0);
        }

        ui.horizontal_wrapped(|ui| {
            if ui.button("Add Zone").clicked() {
                zones.push([0.1, 0.1, 0.5, 0.5]);
                self.selected_zone_idx = Some(zones.len() - 1);
            }
            let has_sel = self.selected_zone_idx.is_some();
            if ui
                .add_enabled(has_sel, egui::Button::new("Split V (L|R)"))
                .clicked()
            {
                if let Some(idx) = self.selected_zone_idx {
                    let z = zones[idx];
                    let mid_x = (z[0] + z[2]) * 0.5;
                    zones[idx] = [z[0], z[1], mid_x, z[3]];
                    zones.insert(idx + 1, [mid_x, z[1], z[2], z[3]]);
                }
            }
            if ui
                .add_enabled(has_sel, egui::Button::new("Split H (T/B)"))
                .clicked()
            {
                if let Some(idx) = self.selected_zone_idx {
                    let z = zones[idx];
                    let mid_y = (z[1] + z[3]) * 0.5;
                    zones[idx] = [z[0], z[1], z[2], mid_y];
                    zones.insert(idx + 1, [z[0], mid_y, z[2], z[3]]);
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
                }
            }
            ui.separator();
            ui.label("Presets:");
            if ui.button("2 Cols").clicked() {
                *zones = vec![[0.0, 0.0, 0.5, 1.0], [0.5, 0.0, 1.0, 1.0]];
                self.selected_zone_idx = Some(0);
            }
            if ui.button("3 Cols").clicked() {
                *zones = vec![
                    [0.0, 0.0, 1.0 / 3.0, 1.0],
                    [1.0 / 3.0, 0.0, 2.0 / 3.0, 1.0],
                    [2.0 / 3.0, 0.0, 1.0, 1.0],
                ];
                self.selected_zone_idx = Some(0);
            }
            if ui.button("2x2").clicked() {
                *zones = vec![
                    [0.0, 0.0, 0.5, 0.5],
                    [0.5, 0.0, 1.0, 0.5],
                    [0.0, 0.5, 0.5, 1.0],
                    [0.5, 0.5, 1.0, 1.0],
                ];
                self.selected_zone_idx = Some(0);
            }
            if ui.button("Focus+Stack").clicked() {
                *zones = vec![
                    [0.0, 0.0, 0.65, 1.0],
                    [0.65, 0.0, 1.0, 0.5],
                    [0.65, 0.5, 1.0, 1.0],
                ];
                self.selected_zone_idx = Some(0);
            }
            ui.checkbox(&mut self.snap_1pct, "Snap 1%");
        });

        ui.label(
            egui::RichText::new("Tip: Drag empty space to draw a zone, drag a zone to move it, or drag its handles to resize.")
                .small()
                .weak(),
        );

        let aspect = (mon_w.max(1) as f32) / (mon_h.max(1) as f32);
        let avail = ui.available_size();
        let max_h = (avail.y - 68.0).max(200.0);
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

        if resp.drag_started() {
            if let Some(pos) = resp.interact_pointer_pos() {
                let mut started = false;
                if let Some(sel) = self.selected_zone_idx {
                    if let Some(&z) = zones.get(sel) {
                        let sr = to_screen(z);
                        for (h, hp) in handle_positions(sr) {
                            if pos.distance(hp) <= 10.0 {
                                self.canvas_drag = CanvasDrag::Resizing {
                                    idx: sel,
                                    handle: h,
                                    orig: z,
                                };
                                started = true;
                                break;
                            }
                        }
                    }
                }
                if !started {
                    // Check if pointer clicked inside an existing zone (prefer smallest area)
                    let mut hit: Option<(usize, f32)> = None;
                    for (i, z) in zones.iter().enumerate() {
                        let sr = to_screen(*z);
                        if sr.contains(pos) {
                            let area = sr.width() * sr.height();
                            if hit.map(|(_, a)| area < a).unwrap_or(true) {
                                hit = Some((i, area));
                            }
                        }
                    }
                    if let Some((idx, _)) = hit {
                        self.selected_zone_idx = Some(idx);
                        let n = to_norm(pos);
                        let z = zones[idx];
                        self.canvas_drag = CanvasDrag::Moving {
                            idx,
                            grab_offset: [n[0] - z[0], n[1] - z[1]],
                            orig: z,
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
                let mut hit: Option<(usize, f32)> = None;
                for (i, z) in zones.iter().enumerate() {
                    let sr = to_screen(*z);
                    if sr.contains(pos) {
                        let area = sr.width() * sr.height();
                        if hit.map(|(_, a)| area < a).unwrap_or(true) {
                            hit = Some((i, area));
                        }
                    }
                }
                if let Some((idx, _)) = hit {
                    self.selected_zone_idx = Some(idx);
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
                        grab_offset,
                        orig,
                    } => {
                        let i = *idx;
                        let w = orig[2] - orig[0];
                        let h = orig[3] - orig[1];
                        let nx1 = snap((cur[0] - grab_offset[0]).clamp(0.0, 1.0 - w));
                        let ny1 = snap((cur[1] - grab_offset[1]).clamp(0.0, 1.0 - h));
                        if let Some(z) = zones.get_mut(i) {
                            *z = [nx1, ny1, (nx1 + w).min(1.0), (ny1 + h).min(1.0)];
                        }
                    }
                    CanvasDrag::Resizing { idx, handle, orig } => {
                        let i = *idx;
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
                            *z = r;
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
                    zones.push([x1, y1, x2, y2]);
                    self.selected_zone_idx = Some(zones.len() - 1);
                }
            }
            self.canvas_drag = CanvasDrag::None;
        }

        // Paint canvas
        let p = ui.painter_at(canvas_rect);
        p.rect_filled(canvas_rect, 4.0, Color32::from_rgb(15, 23, 42));
        p.rect_stroke(
            canvas_rect,
            4.0,
            Stroke::new(1.5_f32, Color32::from_rgb(71, 85, 105)),
        );

        let hi = self.config.appearance.highlight_color;
        let bd = self.config.appearance.border_color;
        for (i, z) in zones.iter().enumerate() {
            let sr = to_screen(*z).shrink(2.0);
            let is_sel = self.selected_zone_idx == Some(i);
            let fill = if is_sel {
                Color32::from_rgba_unmultiplied(hi[0], hi[1], hi[2], 95)
            } else {
                Color32::from_rgba_unmultiplied(148, 163, 184, 36)
            };
            let stroke = if is_sel {
                Stroke::new(2.0_f32, Color32::from_rgb(bd[0], bd[1], bd[2]))
            } else {
                Stroke::new(1.0_f32, Color32::from_rgb(148, 163, 184))
            };
            p.rect_filled(sr, 2.0, fill);
            p.rect_stroke(sr, 2.0, stroke);

            let pw = ((z[2] - z[0]) * mon_w as f32).round() as i32;
            let ph = ((z[3] - z[1]) * mon_h as f32).round() as i32;
            let label = format!("{}\n{}x{}", i + 1, pw, ph);
            p.text(
                sr.center(),
                egui::Align2::CENTER_CENTER,
                label,
                egui::FontId::proportional(13.0),
                if is_sel {
                    Color32::WHITE
                } else {
                    Color32::from_rgb(203, 213, 225)
                },
            );

            if is_sel {
                let raw_sr = to_screen(*z);
                for (_, hp) in handle_positions(raw_sr) {
                    p.circle_filled(hp, 5.0, Color32::WHITE);
                    p.circle_stroke(hp, 5.0, Stroke::new(1.5_f32, Color32::from_rgb(bd[0], bd[1], bd[2])));
                }
            }
        }

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
            p.rect_stroke(dr, 0.0, Stroke::new(1.5_f32, Color32::WHITE));
        }

        // Numeric fine-tuning row for selected zone
        if let Some(sel) = self.selected_zone_idx {
            if let Some(z) = zones.get_mut(sel) {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    let max_x1 = z[2] - 0.05;
                    let max_y1 = z[3] - 0.05;
                    let min_x2 = z[0] + 0.05;
                    let min_y2 = z[1] + 0.05;
                    ui.label(format!("Zone {}:", sel + 1));
                    ui.label("X1");
                    ui.add(egui::DragValue::new(&mut z[0]).speed(0.005).range(0.0..=max_x1));
                    ui.label("Y1");
                    ui.add(egui::DragValue::new(&mut z[1]).speed(0.005).range(0.0..=max_y1));
                    ui.label("X2");
                    ui.add(egui::DragValue::new(&mut z[2]).speed(0.005).range(min_x2..=1.0));
                    ui.label("Y2");
                    ui.add(egui::DragValue::new(&mut z[3]).speed(0.005).range(min_y2..=1.0));
                });
            }
        }
    }

    fn render_hotkey_editor(ui: &mut egui::Ui, id_salt: &str, label: &str, spec: &mut HotkeySpec) {
        ui.group(|ui| {
            ui.label(egui::RichText::new(label).strong());
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
        Self::render_hotkey_editor(
            ui,
            "hk_activate",
            "Activate Zone Overlay (Keyboard Trigger)",
            &mut self.config.hotkeys.activate_overlay,
        );
        ui.add_space(8.0);
        Self::render_hotkey_editor(
            ui,
            "hk_settings",
            "Open Settings & Zone Editor",
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
                        for m in ["Alt", "Ctrl", "Shift", "Win"] {
                            ui.selectable_value(
                                &mut self.config.hotkeys.move_resize_modifier,
                                m.to_string(),
                                m,
                            );
                        }
                    });

                ui.add_space(20.0);
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
            });
            if self
                .config
                .hotkeys
                .move_resize_modifier
                .eq_ignore_ascii_case(&self.config.hotkeys.monitor_lock_modifier)
            {
                ui.colored_label(
                    Color32::from_rgb(251, 191, 36),
                    "Warning: Move/Resize and Monitor-Lock modifiers are set to the same key.",
                );
            }
        });
    }

    fn render_appearance_tab(&mut self, ui: &mut egui::Ui) {
        ui.group(|ui| {
            ui.label(egui::RichText::new("Overlay Visual Style").strong());
            ui.horizontal(|ui| {
                ui.label("Highlight Fill Color:");
                ui.color_edit_button_srgb(&mut self.config.appearance.highlight_color);
                ui.add_space(20.0);
                ui.label("Border Color:");
                ui.color_edit_button_srgb(&mut self.config.appearance.border_color);
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
        });

        ui.add_space(8.0);
        ui.group(|ui| {
            ui.label(egui::RichText::new("Window Behavior").strong());
            ui.checkbox(
                &mut self.config.behavior.restore_on_unsnap,
                "Restore original window size when dragging a snapped window away (unsnap)",
            );
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
    }
}

impl eframe::App for SettingsApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if ctx.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::S)) {
            self.save_and_notify_daemon();
        }

        egui::TopBottomPanel::top("tabs_bar").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.selectable_value(
                    &mut self.tab,
                    SettingsTab::Layouts,
                    "Layouts & Visual Zone Editor",
                );
                ui.selectable_value(&mut self.tab, SettingsTab::Hotkeys, "Hotkeys & Modifiers");
                ui.selectable_value(
                    &mut self.tab,
                    SettingsTab::Appearance,
                    "Appearance & Behavior",
                );
            });
            ui.add_space(2.0);
        });

        egui::TopBottomPanel::bottom("footer_bar").show(ctx, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let save_btn = egui::Button::new(
                    egui::RichText::new("Save & Apply").color(Color32::WHITE).strong(),
                )
                .fill(Color32::from_rgb(2, 132, 199));
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
                        Color32::from_rgb(251, 191, 36),
                        "● Unsaved changes (Ctrl+S to save)",
                    );
                }
            });
            ui.horizontal_wrapped(|ui| {
                let path_str = config_path()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|_| "zones.json".to_string());
                ui.label(egui::RichText::new(format!("Config: {path_str}")).small().weak());
                if let Some(msg) = &self.status_msg {
                    ui.add_space(12.0);
                    let color = if self.status_is_error {
                        Color32::from_rgb(239, 68, 68)
                    } else {
                        Color32::from_rgb(34, 197, 94)
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
        });
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
        Box::new(move |_cc| Ok(Box::new(SettingsApp::new(config, monitors, reload_msg)))),
    );
}
