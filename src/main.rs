#![windows_subsystem = "windows"]

mod hook;
mod monitor;
mod window_ops;

use crate::hook::{HookEvent, HookMode, HookSharedState, HookThread, WM_ZONESNAP_EVENT};
use crate::monitor::{monitor_from_point, Rect};
use crate::window_ops::{
    clip_cursor_to_monitor, compute_resized_rect, detect_resize_edges, get_root_window_at,
    get_window_rect, hwnd_to_isize, is_manageable_window, is_resizable_window, is_titlebar_hit,
    isize_to_hwnd, move_window_nosize, resize_window, send_menu_mask_key, unclip_cursor,
    ResizeEdges,
};
use std::cell::RefCell;
use std::collections::HashSet;
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE, HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW, PostQuitMessage,
    RegisterClassW, TranslateMessage, HWND_MESSAGE, MSG, WINDOW_EX_STYLE, WINDOW_STYLE,
    WM_DESTROY, WNDCLASSW,
};

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

struct DaemonState {
    shared: Arc<HookSharedState>,
    rx: Receiver<HookEvent>,
    drag: DragSession,
    cursor_locked: bool,
}

impl DaemonState {
    fn new(shared: Arc<HookSharedState>, rx: Receiver<HookEvent>) -> Self {
        Self {
            shared,
            rx,
            drag: DragSession::None,
            cursor_locked: false,
        }
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
        self.drag = DragSession::None;
        self.shared.set_mode(HookMode::Idle);
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
                let overlay_hwnd = self.shared.overlay_hwnd.load(std::sync::atomic::Ordering::Acquire);
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
                let overlay_hwnd = self.shared.overlay_hwnd.load(std::sync::atomic::Ordering::Acquire);
                if !is_manageable_window(hwnd, overlay_hwnd) {
                    self.end_drag();
                    return;
                }
                let Some(rect) = get_window_rect(hwnd) else {
                    self.end_drag();
                    return;
                };
                send_menu_mask_key();
                self.drag = DragSession::AltMove {
                    hwnd: hwnd_to_isize(hwnd),
                    grab_offset_x: x - rect.left,
                    grab_offset_y: y - rect.top,
                };
                let lock_vk = self.shared.lock_mod_vk.load(std::sync::atomic::Ordering::Acquire);
                let lock_down = unsafe { ((GetAsyncKeyState(lock_vk) as u16) & 0x8000) != 0 };
                self.update_cursor_lock(lock_down, x, y);
            }
            HookEvent::StartAltResize { x, y } => {
                let Some(hwnd) = get_root_window_at(x, y) else {
                    self.end_drag();
                    return;
                };
                let overlay_hwnd = self.shared.overlay_hwnd.load(std::sync::atomic::Ordering::Acquire);
                if !is_manageable_window(hwnd, overlay_hwnd) || !is_resizable_window(hwnd) {
                    self.end_drag();
                    return;
                }
                let Some(orig_rect) = get_window_rect(hwnd) else {
                    self.end_drag();
                    return;
                };
                send_menu_mask_key();
                let edges = detect_resize_edges(orig_rect, x, y);
                self.drag = DragSession::AltResize {
                    hwnd: hwnd_to_isize(hwnd),
                    start_x: x,
                    start_y: y,
                    orig_rect,
                    edges,
                };
            }
            HookEvent::CtrlChanged(down) => {
                if matches!(
                    self.drag,
                    DragSession::Standard { .. } | DragSession::AltMove { .. }
                ) {
                    let mut pt = windows::Win32::Foundation::POINT::default();
                    unsafe {
                        let _ = windows::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut pt);
                    }
                    self.update_cursor_lock(down, pt.x, pt.y);
                }
            }
            HookEvent::LeftUp { .. } => {
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
            HookEvent::TriggerOverlayFromDrag { .. }
            | HookEvent::CancelOverlay
            | HookEvent::OverlayHotkeyConfirm { .. }
            | HookEvent::WheelCycle { .. }
            | HookEvent::ShiftChanged(_) => {}
        }
    }

    fn handle_move(&mut self, x: i32, y: i32) {
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
                        self.drag = DragSession::Standard { hwnd };
                        self.shared.set_mode(HookMode::StandardDragging);
                        let lock_vk = self
                            .shared
                            .lock_mod_vk
                            .load(std::sync::atomic::Ordering::Acquire);
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

    let mut default_excluded = HashSet::new();
    default_excluded.insert("mstsc.exe".to_string());
    default_excluded.insert("vmconnect.exe".to_string());

    let shared = Arc::new(HookSharedState::new(
        hwnd_to_isize(daemon_hwnd),
        default_excluded,
    ));
    let (_hook_thread, rx) = HookThread::spawn(Arc::clone(&shared));

    DAEMON_STATE.with(|cell| {
        *cell.borrow_mut() = Some(DaemonState::new(shared, rx));
    });

    unsafe {
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, HWND::default(), 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        let _ = DestroyWindow(daemon_hwnd);
    }
    unclip_cursor();
}
