#![windows_subsystem = "windows"]

mod hook;
mod hotkeys;
mod monitor;
mod overlay;
mod window_ops;

use crate::hook::{HookEvent, HookMode, HookSharedState, HookThread, WM_ZONESNAP_EVENT};
use crate::hotkeys::{
    HotkeyManager, HotkeySpec, HOTKEY_ACTIVATE_OVERLAY, HOTKEY_OPEN_SETTINGS,
};
use crate::monitor::{monitor_from_point, MonitorInfo, Rect};
use crate::overlay::{
    custom_zone_pixel_rect, grid_cell_at, grid_span_rect, hit_test_custom_zones, union_rects,
    Layout, LayoutMode, OverlayRenderState, OverlayStyle, OverlayWindow,
};
use crate::window_ops::{
    cancel_modal_move, clip_cursor_to_monitor, compute_resized_rect, detect_resize_edges,
    get_root_window_at, get_window_rect, hwnd_to_isize, is_excluded_app, is_manageable_window,
    is_resizable_window, is_titlebar_hit, isize_to_hwnd, move_window_nosize, resize_window,
    restore_unsnapped_window, send_menu_mask_key, snap_window_flush, unclip_cursor, ResizeEdges,
};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;
use std::sync::mpsc::Receiver;
use std::sync::Arc;
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
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetAncestor, GetCursorPos,
    GetForegroundWindow, GetMessageW, KillTimer, PostQuitMessage, RegisterClassW, SetTimer,
    TranslateMessage, GA_ROOT, HWND_MESSAGE, MSG, WINDOW_EX_STYLE, WINDOW_STYLE, WM_DESTROY,
    WM_HOTKEY, WM_TIMER, WNDCLASSW,
};

const TIMER_TOAST_HIDE: usize = 1;
const TIMER_POST_SNAP_REVERIFY: usize = 2;

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

struct MonitorLayouts {
    active_index: usize,
    layouts: Vec<Layout>,
}

fn default_layouts() -> Vec<Layout> {
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

struct DaemonState {
    daemon_hwnd: HWND,
    shared: Arc<HookSharedState>,
    rx: Receiver<HookEvent>,
    overlay: OverlayWindow,
    style: OverlayStyle,
    restore_on_unsnap: bool,
    monitor_layouts: HashMap<String, MonitorLayouts>,
    snapped_cache: HashMap<isize, Rect>,
    pending_reverify_snap: Option<(isize, Rect)>,
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
        hotkeys: HotkeyManager,
    ) -> Self {
        Self {
            daemon_hwnd,
            shared,
            rx,
            overlay,
            style: OverlayStyle::default(),
            restore_on_unsnap: true,
            monitor_layouts: HashMap::new(),
            snapped_cache: HashMap::new(),
            pending_reverify_snap: None,
            hotkeys,
            drag: DragSession::None,
            overlay_session: None,
            cursor_locked: false,
        }
    }

    fn ensure_monitor_layouts(&mut self, mon: &MonitorInfo) -> &mut MonitorLayouts {
        let key = mon.profile_key();
        self.monitor_layouts
            .entry(key)
            .or_insert_with(|| MonitorLayouts {
                active_index: 0,
                layouts: default_layouts(),
            })
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
        let ml = self.ensure_monitor_layouts(&sess.monitor);
        let idx = ml.active_index.min(ml.layouts.len().saturating_sub(1));
        let layout = &ml.layouts[idx];
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

        let ml = self.ensure_monitor_layouts(&sess.monitor);
        let idx = ml.active_index.min(ml.layouts.len().saturating_sub(1));
        let layout = &ml.layouts[idx];

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
        let (active_idx, layouts) = match self.monitor_layouts.get(&key) {
            Some(ml) => (ml.active_index, &ml.layouts),
            None => return,
        };
        if layouts.is_empty() {
            return;
        }
        let layout = &layouts[active_idx.min(layouts.len() - 1)];
        let render_state = OverlayRenderState {
            work: sess.monitor.rc_work,
            layout,
            style: &self.style,
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
        let layout_name = {
            let ml = self.ensure_monitor_layouts(&sess.monitor);
            let len = ml.layouts.len();
            if len > 0 {
                if delta > 0 {
                    ml.active_index = (ml.active_index + len - 1) % len;
                } else {
                    ml.active_index = (ml.active_index + 1) % len;
                }
                ml.layouts[ml.active_index].name.clone()
            } else {
                String::new()
            }
        };

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
                // Settings GUI wired in Milestone 6
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
                let Some(mut rect) = get_window_rect(hwnd) else {
                    self.end_drag();
                    return;
                };
                send_menu_mask_key();
                if self.restore_on_unsnap {
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
            HookEvent::StartAltResize { x, y } => {
                let Some(hwnd) = get_root_window_at(x, y) else {
                    self.end_drag();
                    return;
                };
                let overlay_hwnd = self.shared.overlay_hwnd.load(Ordering::Acquire);
                if !is_manageable_window(hwnd, overlay_hwnd) || !is_resizable_window(hwnd) {
                    self.end_drag();
                    return;
                }
                let Some(orig_rect) = get_window_rect(hwnd) else {
                    self.end_drag();
                    return;
                };
                send_menu_mask_key();
                self.snapped_cache.remove(&hwnd_to_isize(hwnd));
                let edges = detect_resize_edges(orig_rect, x, y);
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
                    if self.restore_on_unsnap && (x - start_x).abs() + (y - start_y).abs() > 6 {
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
    match msg {
        WM_ZONESNAP_EVENT => {
            DAEMON_STATE.with(|cell| {
                if let Some(state) = cell.borrow_mut().as_mut() {
                    state.process_events();
                }
            });
            LRESULT(0)
        }
        WM_HOTKEY => {
            let id = wparam.0 as i32;
            DAEMON_STATE.with(|cell| {
                if let Some(state) = cell.borrow_mut().as_mut() {
                    state.handle_hotkey(id);
                }
            });
            LRESULT(0)
        }
        WM_TIMER => {
            let timer_id = wparam.0;
            let _ = KillTimer(hwnd, timer_id);
            DAEMON_STATE.with(|cell| {
                if let Some(state) = cell.borrow_mut().as_mut() {
                    if timer_id == TIMER_TOAST_HIDE {
                        state.clear_toast();
                    } else if timer_id == TIMER_POST_SNAP_REVERIFY {
                        state.reverify_pending_snap();
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
            HWND_MESSAGE,
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

    let _monitors = monitor::enumerate_monitors();

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

    let mut hotkeys = HotkeyManager::new();
    let default_activate = HotkeySpec {
        modifiers: vec!["Win".to_string(), "Alt".to_string()],
        key: "Z".to_string(),
    };
    let default_settings = HotkeySpec {
        modifiers: vec!["Win".to_string(), "Alt".to_string()],
        key: "E".to_string(),
    };
    hotkeys.register_all(daemon_hwnd, &default_activate, &default_settings);

    let mut default_excluded = HashSet::new();
    default_excluded.insert("mstsc.exe".to_string());
    default_excluded.insert("vmconnect.exe".to_string());

    let shared = Arc::new(HookSharedState::new(
        hwnd_to_isize(daemon_hwnd),
        default_excluded,
    ));
    shared
        .overlay_hwnd
        .store(overlay.hwnd_raw(), Ordering::Release);

    let (_hook_thread, rx) = HookThread::spawn(Arc::clone(&shared));

    DAEMON_STATE.with(|cell| {
        *cell.borrow_mut() = Some(DaemonState::new(daemon_hwnd, shared, rx, overlay, hotkeys));
    });

    unsafe {
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
