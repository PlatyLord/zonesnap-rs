#![windows_subsystem = "windows"]

mod config;
mod gui;
mod hook;
mod hotkeys;
mod icon;
mod monitor;
mod overlay;
mod tray;
mod window_ops;

use crate::config::{config_modified_time, Config, LayoutMode};
use crate::hook::{HookEvent, HookMode, HookSharedState, HookThread, WM_ZONESNAP_EVENT};
use crate::hotkeys::{
    modifier_to_vk, optional_modifier_to_vk, HotkeyManager, HOTKEY_ACTIVATE_OVERLAY,
    HOTKEY_OPEN_SETTINGS,
};
use crate::monitor::{enumerate_monitors, monitor_from_point, MonitorInfo, Rect};
use crate::overlay::{
    custom_zone_pixel_rect, grid_cell_at, grid_span_rect, hit_test_custom_zones, union_rects,
    OverlayRenderState, OverlayWindow,
};
use crate::tray::{
    TrayIcon, TrayMonitorLayouts, IDM_EXIT, IDM_LAYOUT_BASE, IDM_RELOAD, IDM_SETTINGS, WM_TRAYICON,
};
use crate::window_ops::{
    cancel_modal_move, clip_cursor_to_monitor, compute_resized_rect, detect_nearest_corner,
    detect_resize_edges, get_root_window_at, get_window_rect, hwnd_to_isize, is_excluded_app,
    is_manageable_window, is_resizable_window, is_titlebar_hit, is_valid_window, isize_to_hwnd,
    move_window_nosize, resize_window, restore_if_maximized, restore_unsnapped_window,
    send_menu_mask_key, snap_window_flush, unclip_cursor, ResizeEdges,
};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::time::SystemTime;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE, HINSTANCE, HWND, LPARAM, LRESULT,
    POINT, WPARAM,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_SHIFT};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, FindWindowW, GetAncestor,
    GetCursorPos, GetForegroundWindow, GetMessageW, KillTimer, PostQuitMessage, RegisterClassW,
    RegisterWindowMessageW, SetForegroundWindow, SetTimer, ShowWindow, TranslateMessage, GA_ROOT,
    MSG, SW_RESTORE, WINDOW_EX_STYLE, WINDOW_STYLE, WM_COMMAND, WM_CONTEXTMENU, WM_DESTROY,
    WM_DISPLAYCHANGE, WM_HOTKEY, WM_LBUTTONDBLCLK, WM_RBUTTONUP, WM_TIMER, WNDCLASSW,
};

const TIMER_TOAST_HIDE: usize = 1;
const TIMER_POST_SNAP_REVERIFY: usize = 2;
const TIMER_CONFIG_WATCH: usize = 3;

pub fn reload_window_message() -> u32 {
    unsafe { RegisterWindowMessageW(w!("ZoneSnap_ReloadConfig")) }
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

#[derive(Debug, Clone)]
enum DragSession {
    None,
    MaybeStandard {
        hwnd: isize,
        start_x: i32,
        start_y: i32,
        start_rect: Rect,
        titlebar_hit: bool,
    },
    Standard {
        hwnd: isize,
        start_x: i32,
        start_y: i32,
    },
    AltMove {
        hwnd: isize,
        grab_offset_x: i32,
        grab_offset_y: i32,
    },
    AltResize {
        hwnd: isize,
        start_x: i32,
        start_y: i32,
        orig_rect: Rect,
        edges: ResizeEdges,
    },
}

struct OverlaySession {
    target_hwnd: isize,
    from_hotkey: bool,
    return_drag: DragSession,
    monitor: MonitorInfo,
    grid_anchor: Option<(u32, u32)>,
    grid_current: Option<(u32, u32)>,
    shift_held: bool,
    merged_zones: Vec<usize>,
    active_zones: HashSet<usize>,
    target_rect: Option<Rect>,
    toast_text: Option<String>,
}

struct DaemonState {
    daemon_hwnd: HWND,
    shared: Arc<HookSharedState>,
    rx: Receiver<HookEvent>,
    overlay: OverlayWindow,
    config: Config,
    last_config_mtime: Option<SystemTime>,
    reload_msg: u32,
    tray: Option<TrayIcon>,
    taskbar_created_msg: u32,
    tray_layout_map: Vec<(String, usize)>,
    snapped_cache: HashMap<isize, Rect>,
    pending_reverify_snap: Option<(isize, Rect)>,
    watch_ticks: u32,
    hotkeys: HotkeyManager,
    drag: DragSession,
    overlay_session: Option<OverlaySession>,
    cursor_locked: bool,
}

impl DaemonState {
    fn new(
        daemon_hwnd: HWND,
        shared: Arc<HookSharedState>,
        rx: Receiver<HookEvent>,
        overlay: OverlayWindow,
        config: Config,
        last_config_mtime: Option<SystemTime>,
        reload_msg: u32,
        tray: Option<TrayIcon>,
        taskbar_created_msg: u32,
        hotkeys: HotkeyManager,
    ) -> Self {
        Self {
            daemon_hwnd,
            shared,
            rx,
            overlay,
            config,
            last_config_mtime,
            reload_msg,
            tray,
            taskbar_created_msg,
            tray_layout_map: Vec::new(),
            snapped_cache: HashMap::new(),
            pending_reverify_snap: None,
            watch_ticks: 0,
            hotkeys,
            drag: DragSession::None,
            overlay_session: None,
            cursor_locked: false,
        }
    }

    fn prune_snapped_cache(&mut self) {
        self.snapped_cache
            .retain(|&raw, _| is_valid_window(isize_to_hwnd(raw)));
    }

    fn prepare_tray_menu(&mut self) -> Vec<TrayMonitorLayouts> {
        self.check_and_reload_if_changed();
        let monitors = enumerate_monitors();
        self.config.ensure_monitors(&monitors);

        self.tray_layout_map.clear();
        let mut menus = Vec::with_capacity(monitors.len());
        for m in &monitors {
            let key = m.profile_key();
            if let Some(prof) = self.config.monitors.get(&key) {
                let pri = if m.is_primary { " [Primary]" } else { "" };
                let monitor_label = format!(
                    "{} ({}x{}){}",
                    m.device_name,
                    m.rc_monitor.width(),
                    m.rc_monitor.height(),
                    pri
                );
                let active_idx = prof
                    .active_layout_index
                    .min(prof.layouts.len().saturating_sub(1));
                let mut items = Vec::with_capacity(prof.layouts.len());
                for (idx, layout) in prof.layouts.iter().enumerate() {
                    let cmd_id = IDM_LAYOUT_BASE + self.tray_layout_map.len();
                    self.tray_layout_map.push((key.clone(), idx));
                    items.push((cmd_id, layout.name.clone(), idx == active_idx));
                }
                menus.push(TrayMonitorLayouts {
                    monitor_label,
                    items,
                });
            }
        }
        menus
    }

    fn select_tray_layout(&mut self, cmd_id: usize) {
        let Some(offset) = cmd_id.checked_sub(IDM_LAYOUT_BASE) else {
            return;
        };
        let Some((key, layout_idx)) = self.tray_layout_map.get(offset).cloned() else {
            return;
        };
        if let Some(prof) = self.config.monitors.get_mut(&key) {
            if layout_idx < prof.layouts.len() {
                prof.active_layout_index = layout_idx;
                let _ = self.config.save();
                self.last_config_mtime = config_modified_time();
            }
        }
    }

    fn reload_config(&mut self) {
        let monitors = enumerate_monitors();
        let cfg = Config::load_or_create(&monitors);
        self.apply_config(cfg);
        self.last_config_mtime = config_modified_time();
    }

    fn check_and_reload_if_changed(&mut self) {
        let mtime = config_modified_time();
        if mtime.is_some() && mtime != self.last_config_mtime {
            self.reload_config();
        }
    }

    fn apply_config(&mut self, cfg: Config) {
        self.shared.set_excluded_apps(cfg.excluded_set());
        self.shared.move_mod_vk.store(
            modifier_to_vk(&cfg.hotkeys.move_resize_modifier),
            Ordering::Release,
        );
        self.shared.lock_mod_vk.store(
            modifier_to_vk(&cfg.hotkeys.monitor_lock_modifier),
            Ordering::Release,
        );
        self.shared.corner_mod_vk.store(
            optional_modifier_to_vk(&cfg.hotkeys.corner_resize_modifier),
            Ordering::Release,
        );
        self.hotkeys.register_all(
            self.daemon_hwnd,
            &cfg.hotkeys.activate_overlay,
            &cfg.hotkeys.open_settings,
        );
        self.config = cfg;
    }

    fn handle_display_change(&mut self) {
        let monitors = enumerate_monitors();
        self.config.ensure_monitors(&monitors);
    }

    fn update_cursor_lock(&mut self, locked: bool, cursor_x: i32, cursor_y: i32) {
        if locked == self.cursor_locked {
            return;
        }
        if locked {
            if let Some(mon) = monitor_from_point(cursor_x, cursor_y) {
                clip_cursor_to_monitor(&mon);
                self.cursor_locked = true;
            }
        } else {
            unclip_cursor();
            self.cursor_locked = false;
        }
    }

    fn end_drag(&mut self) {
        if self.cursor_locked {
            unclip_cursor();
            self.cursor_locked = false;
        }
        self.overlay.hide();
        self.overlay_session = None;
        self.drag = DragSession::None;
        self.shared.set_mode(HookMode::Idle);
    }

    fn clear_toast(&mut self) {
        if let Some(sess) = &mut self.overlay_session {
            sess.toast_text = None;
            self.render_overlay();
        }
    }

    fn reverify_pending_snap(&mut self) {
        if let Some((hwnd_raw, target)) = self.pending_reverify_snap.take() {
            let overlay_hwnd = self.shared.overlay_hwnd.load(Ordering::Acquire);
            snap_window_flush(
                isize_to_hwnd(hwnd_raw),
                target,
                &mut self.snapped_cache,
                overlay_hwnd,
            );
        }
    }

    fn show_overlay(&mut self, target_hwnd: isize, x: i32, y: i32, from_hotkey: bool) {
        self.check_and_reload_if_changed();
        let Some(mon) = monitor_from_point(x, y) else {
            return;
        };
        let shift_held = unsafe { ((GetAsyncKeyState(VK_SHIFT.0 as i32) as u16) & 0x8000) != 0 };
        let return_drag = if from_hotkey {
            DragSession::None
        } else {
            std::mem::replace(&mut self.drag, DragSession::None)
        };

        let mut sess = OverlaySession {
            target_hwnd,
            from_hotkey,
            return_drag,
            monitor: mon,
            grid_anchor: None,
            grid_current: None,
            shift_held,
            merged_zones: Vec::new(),
            active_zones: HashSet::new(),
            target_rect: None,
            toast_text: None,
        };
        self.init_overlay_selection(&mut sess, x, y);
        self.overlay_session = Some(sess);
        self.render_overlay();
    }

    fn init_overlay_selection(&mut self, sess: &mut OverlaySession, x: i32, y: i32) {
        self.config
            .ensure_monitors(std::slice::from_ref(&sess.monitor));
        let key = sess.monitor.profile_key();
        let Some(prof) = self.config.monitors.get(&key) else {
            return;
        };
        let idx = prof
            .active_layout_index
            .min(prof.layouts.len().saturating_sub(1));
        let layout = &prof.layouts[idx];
        sess.merged_zones.clear();
        sess.active_zones.clear();
        match &layout.mode {
            LayoutMode::DynamicGrid { cols, rows } => {
                let cell = grid_cell_at(sess.monitor.rc_work, *cols, *rows, x, y);
                sess.grid_anchor = Some(cell);
                sess.grid_current = Some(cell);
                sess.target_rect =
                    Some(grid_span_rect(sess.monitor.rc_work, *cols, *rows, cell, cell));
            }
            LayoutMode::CustomZones { .. } => {
                sess.grid_anchor = None;
                sess.grid_current = None;
                self.update_overlay_cursor(sess, x, y);
            }
        }
    }

    fn update_overlay_cursor(&mut self, sess: &mut OverlaySession, x: i32, y: i32) {
        if !sess.monitor.rc_monitor.contains(x, y) {
            if let Some(new_mon) = monitor_from_point(x, y) {
                if new_mon.profile_key() != sess.monitor.profile_key()
                    || new_mon.rc_work != sess.monitor.rc_work
                {
                    sess.monitor = new_mon;
                    self.init_overlay_selection(sess, x, y);
                    return;
                }
            }
        }

        self.config
            .ensure_monitors(std::slice::from_ref(&sess.monitor));
        let key = sess.monitor.profile_key();
        let Some(prof) = self.config.monitors.get(&key) else {
            return;
        };
        let idx = prof
            .active_layout_index
            .min(prof.layouts.len().saturating_sub(1));
        let layout = &prof.layouts[idx];

        match &layout.mode {
            LayoutMode::DynamicGrid { cols, rows } => {
                let cur = grid_cell_at(sess.monitor.rc_work, *cols, *rows, x, y);
                let anchor = sess.grid_anchor.unwrap_or(cur);
                sess.grid_anchor = Some(anchor);
                sess.grid_current = Some(cur);
                sess.target_rect = Some(grid_span_rect(
                    sess.monitor.rc_work,
                    *cols,
                    *rows,
                    anchor,
                    cur,
                ));
            }
            LayoutMode::CustomZones { zones } => {
                let hits = hit_test_custom_zones(sess.monitor.rc_work, zones, x, y, 16);
                if sess.shift_held {
                    for h in hits {
                        if !sess.merged_zones.contains(&h) {
                            sess.merged_zones.push(h);
                        }
                    }
                    if sess.merged_zones.is_empty() {
                        sess.active_zones.clear();
                        sess.target_rect = None;
                    } else {
                        sess.active_zones = sess.merged_zones.iter().copied().collect();
                        let rects: Vec<Rect> = sess
                            .merged_zones
                            .iter()
                            .filter_map(|&i| zones.get(i))
                            .map(|z| custom_zone_pixel_rect(sess.monitor.rc_work, *z))
                            .collect();
                        sess.target_rect = union_rects(&rects);
                    }
                } else {
                    sess.merged_zones = hits.clone();
                    sess.active_zones = hits.iter().copied().collect();
                    let rects: Vec<Rect> = hits
                        .iter()
                        .filter_map(|&i| zones.get(i))
                        .map(|z| custom_zone_pixel_rect(sess.monitor.rc_work, *z))
                        .collect();
                    sess.target_rect = union_rects(&rects);
                }
            }
        }
    }

    fn render_overlay(&mut self) {
        let Some(sess) = &self.overlay_session else {
            return;
        };
        let key = sess.monitor.profile_key();
        let Some(prof) = self.config.monitors.get(&key) else {
            return;
        };
        if prof.layouts.is_empty() {
            return;
        }
        let layout = &prof.layouts[prof.active_layout_index.min(prof.layouts.len() - 1)];
        let render_state = OverlayRenderState {
            work: sess.monitor.rc_work,
            layout,
            style: &self.config.appearance,
            grid_anchor: sess.grid_anchor,
            grid_current: sess.grid_current,
            active_zones: &sess.active_zones,
            target_rect: sess.target_rect,
            toast_text: sess.toast_text.as_deref(),
        };
        self.overlay.render(&render_state);
    }

    fn cancel_overlay(&mut self) {
        self.overlay.hide();
        unsafe {
            let _ = KillTimer(self.daemon_hwnd, TIMER_TOAST_HIDE);
        }
        let Some(sess) = self.overlay_session.take() else {
            self.end_drag();
            return;
        };
        if sess.from_hotkey {
            self.end_drag();
        } else {
            match sess.return_drag {
                DragSession::Standard {
                    hwnd,
                    start_x,
                    start_y,
                } => {
                    self.drag = DragSession::Standard {
                        hwnd,
                        start_x,
                        start_y,
                    };
                    self.shared.set_mode(HookMode::StandardDragging);
                }
                DragSession::AltMove {
                    hwnd,
                    grab_offset_x,
                    grab_offset_y,
                } => {
                    self.drag = DragSession::AltMove {
                        hwnd,
                        grab_offset_x,
                        grab_offset_y,
                    };
                    self.shared.set_mode(HookMode::AltMoving);
                }
                _ => self.end_drag(),
            }
        }
    }

    fn commit_overlay_snap(&mut self) {
        let Some(sess) = self.overlay_session.take() else {
            self.end_drag();
            return;
        };
        self.overlay.hide();
        unsafe {
            let _ = KillTimer(self.daemon_hwnd, TIMER_TOAST_HIDE);
        }

        if let Some(target) = sess.target_rect {
            if self.snapped_cache.len() > 64 {
                self.prune_snapped_cache();
            }
            let hwnd = isize_to_hwnd(sess.target_hwnd);
            let overlay_hwnd = self.shared.overlay_hwnd.load(Ordering::Acquire);
            let was_standard = matches!(sess.return_drag, DragSession::Standard { .. });
            if was_standard {
                cancel_modal_move(hwnd);
            }
            if snap_window_flush(hwnd, target, &mut self.snapped_cache, overlay_hwnd)
                && was_standard
            {
                self.pending_reverify_snap = Some((sess.target_hwnd, target));
                unsafe {
                    let _ = SetTimer(self.daemon_hwnd, TIMER_POST_SNAP_REVERIFY, 25, None);
                }
            }
        }
        self.end_drag();
    }

    fn cycle_layout(&mut self, delta: i16) {
        let Some(mut sess) = self.overlay_session.take() else {
            return;
        };
        self.config
            .ensure_monitors(std::slice::from_ref(&sess.monitor));
        let key = sess.monitor.profile_key();
        let layout_name = if let Some(prof) = self.config.monitors.get_mut(&key) {
            let len = prof.layouts.len();
            if len > 0 {
                if delta > 0 {
                    prof.active_layout_index = (prof.active_layout_index + len - 1) % len;
                } else {
                    prof.active_layout_index = (prof.active_layout_index + 1) % len;
                }
                prof.layouts[prof.active_layout_index].name.clone()
            } else {
                String::new()
            }
        } else {
            String::new()
        };
        let _ = self.config.save();
        self.last_config_mtime = config_modified_time();

        let mut pt = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut pt);
        }
        self.init_overlay_selection(&mut sess, pt.x, pt.y);
        sess.toast_text = Some(layout_name);
        self.overlay_session = Some(sess);
        self.render_overlay();

        unsafe {
            let _ = SetTimer(self.daemon_hwnd, TIMER_TOAST_HIDE, 1500, None);
        }
    }

    fn open_settings(&self) {
        unsafe {
            let title: Vec<u16> = "ZoneSnap \u{2014} Settings & Zone Editor"
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();
            if let Ok(existing) = FindWindowW(PCWSTR::null(), PCWSTR(title.as_ptr())) {
                if !existing.0.is_null() {
                    let _ = ShowWindow(existing, SW_RESTORE);
                    let _ = SetForegroundWindow(existing);
                    return;
                }
            }
        }
        if let Ok(exe) = std::env::current_exe() {
            let _ = std::process::Command::new(exe).arg("--settings").spawn();
        }
    }

    fn handle_hotkey(&mut self, id: i32) {
        match id {
            HOTKEY_ACTIVATE_OVERLAY => {
                if self.overlay_session.is_some() {
                    self.cancel_overlay();
                    return;
                }
                let fg = unsafe { GetForegroundWindow() };
                if fg.0.is_null() {
                    return;
                }
                let root = unsafe { GetAncestor(fg, GA_ROOT) };
                let target = if root.0.is_null() { fg } else { root };
                let overlay_hwnd = self.shared.overlay_hwnd.load(Ordering::Acquire);
                if !is_manageable_window(target, overlay_hwnd) {
                    return;
                }
                let excluded = self
                    .shared
                    .excluded_apps
                    .read()
                    .map(|g| is_excluded_app(target, &g))
                    .unwrap_or(false);
                if excluded {
                    return;
                }
                let mut pt = POINT::default();
                unsafe {
                    let _ = GetCursorPos(&mut pt);
                }
                self.shared.set_mode(HookMode::OverlayFromHotkey);
                self.show_overlay(hwnd_to_isize(target), pt.x, pt.y, true);
            }
            HOTKEY_OPEN_SETTINGS => {
                self.open_settings();
            }
            _ => {}
        }
    }

    fn process_events(&mut self) {
        while let Ok(ev) = self.rx.try_recv() {
            self.handle_event(ev);
        }
        if let Some((x, y)) = self.shared.take_latest_move() {
            self.handle_move(x, y);
        }
    }

    fn handle_event(&mut self, ev: HookEvent) {
        match ev {
            HookEvent::LeftDownCandidate { x, y } => {
                let Some(hwnd) = get_root_window_at(x, y) else {
                    self.end_drag();
                    return;
                };
                let overlay_hwnd = self.shared.overlay_hwnd.load(Ordering::Acquire);
                if !is_manageable_window(hwnd, overlay_hwnd) {
                    self.end_drag();
                    return;
                }
                let Some(start_rect) = get_window_rect(hwnd) else {
                    self.end_drag();
                    return;
                };
                let titlebar_hit = is_titlebar_hit(hwnd, x, y);
                self.drag = DragSession::MaybeStandard {
                    hwnd: hwnd_to_isize(hwnd),
                    start_x: x,
                    start_y: y,
                    start_rect,
                    titlebar_hit,
                };
            }
            HookEvent::StartAltMove { x, y } => {
                let Some(hwnd) = get_root_window_at(x, y) else {
                    self.end_drag();
                    return;
                };
                let overlay_hwnd = self.shared.overlay_hwnd.load(Ordering::Acquire);
                if !is_manageable_window(hwnd, overlay_hwnd) {
                    self.end_drag();
                    return;
                }
                send_menu_mask_key();
                let was_max = restore_if_maximized(hwnd);
                let Some(mut rect) = get_window_rect(hwnd) else {
                    self.end_drag();
                    return;
                };
                if was_max {
                    move_window_nosize(hwnd, x - rect.width() / 2, (y - 16).max(0));
                    if let Some(updated) = get_window_rect(hwnd) {
                        rect = updated;
                    }
                } else if self.config.behavior.restore_on_unsnap {
                    if let Some(restored) =
                        restore_unsnapped_window(hwnd, x, y, &mut self.snapped_cache, true)
                    {
                        rect = restored;
                    }
                }
                self.drag = DragSession::AltMove {
                    hwnd: hwnd_to_isize(hwnd),
                    grab_offset_x: x - rect.left,
                    grab_offset_y: y - rect.top,
                };
                let lock_vk = self.shared.lock_mod_vk.load(Ordering::Acquire);
                let lock_down = unsafe { ((GetAsyncKeyState(lock_vk) as u16) & 0x8000) != 0 };
                self.update_cursor_lock(lock_down, x, y);
            }
            HookEvent::StartAltResize { x, y, corner_mode } => {
                let Some(hwnd) = get_root_window_at(x, y) else {
                    self.end_drag();
                    return;
                };
                let overlay_hwnd = self.shared.overlay_hwnd.load(Ordering::Acquire);
                if !is_manageable_window(hwnd, overlay_hwnd) || !is_resizable_window(hwnd) {
                    self.end_drag();
                    return;
                }
                send_menu_mask_key();
                restore_if_maximized(hwnd);
                let Some(orig_rect) = get_window_rect(hwnd) else {
                    self.end_drag();
                    return;
                };
                self.snapped_cache.remove(&hwnd_to_isize(hwnd));
                let edges = if corner_mode {
                    detect_nearest_corner(orig_rect, x, y)
                } else {
                    detect_resize_edges(orig_rect, x, y)
                };
                self.drag = DragSession::AltResize {
                    hwnd: hwnd_to_isize(hwnd),
                    start_x: x,
                    start_y: y,
                    orig_rect,
                    edges,
                };
            }
            HookEvent::TriggerOverlayFromDrag { x, y } => {
                let target_hwnd = match &self.drag {
                    DragSession::Standard { hwnd, .. } | DragSession::AltMove { hwnd, .. } => *hwnd,
                    _ => {
                        self.end_drag();
                        return;
                    }
                };
                self.show_overlay(target_hwnd, x, y, false);
            }
            HookEvent::CancelOverlay => {
                self.cancel_overlay();
            }
            HookEvent::OverlayHotkeyConfirm { .. } => {
                self.commit_overlay_snap();
            }
            HookEvent::WheelCycle { delta } => {
                self.cycle_layout(delta);
            }
            HookEvent::ShiftChanged(down) => {
                if let Some(mut sess) = self.overlay_session.take() {
                    sess.shift_held = down;
                    if !down {
                        sess.merged_zones.clear();
                    }
                    let mut pt = POINT::default();
                    unsafe {
                        let _ = GetCursorPos(&mut pt);
                    }
                    self.update_overlay_cursor(&mut sess, pt.x, pt.y);
                    self.overlay_session = Some(sess);
                    self.render_overlay();
                }
            }
            HookEvent::CtrlChanged(down) => {
                if matches!(
                    self.drag,
                    DragSession::Standard { .. } | DragSession::AltMove { .. }
                ) || self.overlay_session.is_some()
                {
                    let mut pt = POINT::default();
                    unsafe {
                        let _ = GetCursorPos(&mut pt);
                    }
                    self.update_cursor_lock(down, pt.x, pt.y);
                }
            }
            HookEvent::LeftUp { x, y } => {
                if let Some(sess) = &self.overlay_session {
                    if !sess.from_hotkey {
                        self.commit_overlay_snap();
                        return;
                    }
                }
                if let DragSession::Standard {
                    hwnd,
                    start_x,
                    start_y,
                } = self.drag
                {
                    if self.config.behavior.restore_on_unsnap
                        && (x - start_x).abs() + (y - start_y).abs() > 6
                    {
                        restore_unsnapped_window(
                            isize_to_hwnd(hwnd),
                            x,
                            y,
                            &mut self.snapped_cache,
                            false,
                        );
                    }
                }
                if matches!(
                    self.drag,
                    DragSession::MaybeStandard { .. }
                        | DragSession::Standard { .. }
                        | DragSession::AltMove { .. }
                ) {
                    self.end_drag();
                }
            }
            HookEvent::RightUp { .. } => {
                if matches!(self.drag, DragSession::AltResize { .. }) {
                    self.end_drag();
                }
            }
        }
    }

    fn handle_move(&mut self, x: i32, y: i32) {
        if let Some(mut sess) = self.overlay_session.take() {
            if let DragSession::AltMove {
                hwnd,
                grab_offset_x,
                grab_offset_y,
            } = sess.return_drag
            {
                move_window_nosize(isize_to_hwnd(hwnd), x - grab_offset_x, y - grab_offset_y);
            }
            self.update_overlay_cursor(&mut sess, x, y);
            self.overlay_session = Some(sess);
            self.render_overlay();
            return;
        }

        match self.drag {
            DragSession::MaybeStandard {
                hwnd,
                start_x,
                start_y,
                start_rect,
                titlebar_hit,
            } => {
                let dist = (x - start_x).abs() + (y - start_y).abs();
                if dist >= 4 {
                    let target = isize_to_hwnd(hwnd);
                    let moved = get_window_rect(target)
                        .map(|r| r.left != start_rect.left || r.top != start_rect.top)
                        .unwrap_or(false);
                    if titlebar_hit || moved {
                        self.drag = DragSession::Standard {
                            hwnd,
                            start_x,
                            start_y,
                        };
                        self.shared.set_mode(HookMode::StandardDragging);
                        let lock_vk = self.shared.lock_mod_vk.load(Ordering::Acquire);
                        let lock_down =
                            unsafe { ((GetAsyncKeyState(lock_vk) as u16) & 0x8000) != 0 };
                        self.update_cursor_lock(lock_down, x, y);
                    } else if dist > 24 {
                        self.end_drag();
                    }
                }
            }
            DragSession::AltMove {
                hwnd,
                grab_offset_x,
                grab_offset_y,
            } => {
                move_window_nosize(isize_to_hwnd(hwnd), x - grab_offset_x, y - grab_offset_y);
            }
            DragSession::AltResize {
                hwnd,
                start_x,
                start_y,
                orig_rect,
                edges,
            } => {
                let new_rect =
                    compute_resized_rect(orig_rect, edges, x - start_x, y - start_y, 160, 100);
                resize_window(isize_to_hwnd(hwnd), new_rect);
            }
            _ => {}
        }
    }
}

impl Drop for DaemonState {
    fn drop(&mut self) {
        self.hotkeys.unregister_all(self.daemon_hwnd);
    }
}

thread_local! {
    static DAEMON_STATE: RefCell<Option<DaemonState>> = const { RefCell::new(None) };
}

unsafe extern "system" fn daemon_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let (taskbar_msg, reload_msg) = DAEMON_STATE.with(|cell| {
        cell.borrow()
            .as_ref()
            .map(|s| (s.taskbar_created_msg, s.reload_msg))
            .unwrap_or((0, 0))
    });
    if taskbar_msg != 0 && msg == taskbar_msg {
        DAEMON_STATE.with(|cell| {
            if let Some(state) = cell.borrow().as_ref() {
                if let Some(tray) = &state.tray {
                    tray.re_add();
                }
            }
        });
        return LRESULT(0);
    }
    if reload_msg != 0 && msg == reload_msg {
        DAEMON_STATE.with(|cell| {
            if let Some(state) = cell.borrow_mut().as_mut() {
                state.reload_config();
            }
        });
        return LRESULT(0);
    }

    match msg {
        WM_ZONESNAP_EVENT => {
            DAEMON_STATE.with(|cell| {
                if let Ok(mut guard) = cell.try_borrow_mut() {
                    if let Some(state) = guard.as_mut() {
                        state.process_events();
                    }
                }
            });
            LRESULT(0)
        }
        WM_DISPLAYCHANGE => {
            DAEMON_STATE.with(|cell| {
                if let Ok(mut guard) = cell.try_borrow_mut() {
                    if let Some(state) = guard.as_mut() {
                        state.handle_display_change();
                    }
                }
            });
            LRESULT(0)
        }
        WM_HOTKEY => {
            let id = wparam.0 as i32;
            DAEMON_STATE.with(|cell| {
                if let Ok(mut guard) = cell.try_borrow_mut() {
                    if let Some(state) = guard.as_mut() {
                        state.handle_hotkey(id);
                    }
                }
            });
            LRESULT(0)
        }
        WM_TRAYICON => {
            let ev = (lparam.0 as u32) & 0xFFFF;
            if ev == WM_RBUTTONUP || ev == WM_CONTEXTMENU {
                let menus = DAEMON_STATE.with(|cell| {
                    cell.try_borrow_mut()
                        .ok()
                        .and_then(|mut g| g.as_mut().map(|s| s.prepare_tray_menu()))
                });
                if let Some(menus) = menus {
                    TrayIcon::show_context_menu(hwnd, &menus);
                }
            } else if ev == WM_LBUTTONDBLCLK {
                DAEMON_STATE.with(|cell| {
                    if let Ok(guard) = cell.try_borrow() {
                        if let Some(state) = guard.as_ref() {
                            state.open_settings();
                        }
                    }
                });
            }
            LRESULT(0)
        }
        WM_COMMAND => {
            let cmd = (wparam.0 & 0xFFFF) as usize;
            if cmd >= IDM_LAYOUT_BASE {
                DAEMON_STATE.with(|cell| {
                    if let Ok(mut guard) = cell.try_borrow_mut() {
                        if let Some(state) = guard.as_mut() {
                            state.select_tray_layout(cmd);
                        }
                    }
                });
                return LRESULT(0);
            }
            match cmd {
                IDM_SETTINGS => {
                    DAEMON_STATE.with(|cell| {
                        if let Ok(guard) = cell.try_borrow() {
                            if let Some(state) = guard.as_ref() {
                                state.open_settings();
                            }
                        }
                    });
                }
                IDM_RELOAD => {
                    DAEMON_STATE.with(|cell| {
                        if let Ok(mut guard) = cell.try_borrow_mut() {
                            if let Some(state) = guard.as_mut() {
                                state.reload_config();
                            }
                        }
                    });
                }
                IDM_EXIT => {
                    PostQuitMessage(0);
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_TIMER => {
            let timer_id = wparam.0;
            if timer_id != TIMER_CONFIG_WATCH {
                let _ = KillTimer(hwnd, timer_id);
            }
            DAEMON_STATE.with(|cell| {
                if let Ok(mut guard) = cell.try_borrow_mut() {
                    if let Some(state) = guard.as_mut() {
                        if timer_id == TIMER_TOAST_HIDE {
                            state.clear_toast();
                        } else if timer_id == TIMER_POST_SNAP_REVERIFY {
                            state.reverify_pending_snap();
                        } else if timer_id == TIMER_CONFIG_WATCH {
                            state.check_and_reload_if_changed();
                            state.watch_ticks = state.watch_ticks.wrapping_add(1);
                            if state.watch_ticks % 60 == 0 {
                                state.prune_snapped_cache();
                            }
                        }
                    }
                }
            });
            LRESULT(0)
        }
        WM_DESTROY => {
            unclip_cursor();
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

fn create_daemon_window() -> Option<HWND> {
    unsafe {
        let hinstance: HINSTANCE = GetModuleHandleW(None).unwrap_or_default().into();
        let class_name = w!("ZoneSnap_Daemon_MsgWindow");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(daemon_wnd_proc),
            hInstance: hinstance,
            lpszClassName: class_name,
            ..Default::default()
        };
        RegisterClassW(&wc);
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class_name,
            PCWSTR::null(),
            WINDOW_STYLE::default(),
            0,
            0,
            0,
            0,
            HWND::default(),
            None,
            hinstance,
            None,
        )
        .ok()
    }
}

fn main() {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }

    let reload_msg = reload_window_message();
    if std::env::args().any(|a| a == "--settings") {
        gui::run_settings_window(reload_msg);
        return;
    }

    let monitors = enumerate_monitors();

    let mutex_handle = unsafe { CreateMutexW(None, false, w!("Local\\ZoneSnap_Daemon_Mutex")) };
    let Ok(handle) = mutex_handle else {
        return;
    };
    let _mutex_guard = MutexGuard(handle);
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        return;
    }

    let Some(daemon_hwnd) = create_daemon_window() else {
        return;
    };
    let Some(overlay) = OverlayWindow::new() else {
        return;
    };

    let cfg = Config::load_or_create(&monitors);
    let last_mtime = config_modified_time();
    let mut hotkeys = HotkeyManager::new();
    hotkeys.register_all(
        daemon_hwnd,
        &cfg.hotkeys.activate_overlay,
        &cfg.hotkeys.open_settings,
    );

    let shared = Arc::new(HookSharedState::new(
        hwnd_to_isize(daemon_hwnd),
        cfg.excluded_set(),
    ));
    shared.move_mod_vk.store(
        modifier_to_vk(&cfg.hotkeys.move_resize_modifier),
        Ordering::Release,
    );
    shared.lock_mod_vk.store(
        modifier_to_vk(&cfg.hotkeys.monitor_lock_modifier),
        Ordering::Release,
    );
    shared.corner_mod_vk.store(
        optional_modifier_to_vk(&cfg.hotkeys.corner_resize_modifier),
        Ordering::Release,
    );
    shared
        .overlay_hwnd
        .store(overlay.hwnd_raw(), Ordering::Release);

    let (_hook_thread, rx) = HookThread::spawn(Arc::clone(&shared));
    let tray = TrayIcon::new(daemon_hwnd, WM_TRAYICON);
    let taskbar_created_msg = unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) };

    DAEMON_STATE.with(|cell| {
        *cell.borrow_mut() = Some(DaemonState::new(
            daemon_hwnd,
            shared,
            rx,
            overlay,
            cfg,
            last_mtime,
            reload_msg,
            tray,
            taskbar_created_msg,
            hotkeys,
        ));
    });

    unsafe {
        let _ = SetTimer(daemon_hwnd, TIMER_CONFIG_WATCH, 1000, None);
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, HWND::default(), 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        DAEMON_STATE.with(|cell| {
            *cell.borrow_mut() = None;
        });
        let _ = DestroyWindow(daemon_hwnd);
    }
    unclip_cursor();
}
