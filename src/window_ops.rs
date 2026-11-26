use crate::monitor::{MonitorInfo, Rect};
use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use windows::core::PWSTR;
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP,
    VIRTUAL_KEY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    ClipCursor, GetAncestor, GetClassNameW, GetWindowLongW, GetWindowRect,
    GetWindowThreadProcessId, IsWindow, IsWindowVisible, IsZoomed, PostMessageW,
    SendMessageTimeoutW, SetWindowPos, ShowWindow, WindowFromPoint, GA_ROOT, GWL_STYLE, HTCAPTION,
    SMTO_ABORTIFHUNG, SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER, SW_RESTORE, WM_CANCELMODE,
    WM_NCHITTEST, WS_CHILD, WS_THICKFRAME,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HorizontalEdge {
    Left,
    Center,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerticalEdge {
    Top,
    Center,
    Bottom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResizeEdges {
    pub horiz: HorizontalEdge,
    pub vert: VerticalEdge,
}

/// Resolve the top-level root window under screen coordinates `(x, y)`.
pub fn get_root_window_at(x: i32, y: i32) -> Option<HWND> {
    unsafe {
        let hwnd = WindowFromPoint(POINT { x, y });
        if hwnd.0.is_null() {
            return None;
        }
        let root = GetAncestor(hwnd, GA_ROOT);
        let target = if root.0.is_null() { hwnd } else { root };
        if target.0.is_null() {
            None
        } else {
            Some(target)
        }
    }
}

struct HandleGuard(HANDLE);
impl Drop for HandleGuard {
    fn drop(&mut self) {
        if !self.0.is_invalid() && !self.0.0.is_null() {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
}

/// Query the lowercase executable filename (e.g. `"mstsc.exe"`) owning `hwnd`.
/// Returns `None` silently if the process cannot be opened (e.g. elevated/protected processes).
pub fn get_process_exe_name(hwnd: HWND) -> Option<String> {
    if hwnd.0.is_null() {
        return None;
    }
    let mut pid: u32 = 0;
    unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
    }
    if pid == 0 {
        return None;
    }
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let _guard = HandleGuard(handle);

    let mut buf = [0u16; 1024];
    let mut len = buf.len() as u32;
    let ok = unsafe {
        QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
    };
    if ok.is_err() || len == 0 {
        return None;
    }
    let full = String::from_utf16_lossy(&buf[..len as usize]);
    let base = full.rsplit(['\\', '/']).next().unwrap_or(&full);
    Some(base.to_ascii_lowercase())
}

/// Check whether `hwnd` belongs to an application listed in `excluded`.
pub fn is_excluded_app(hwnd: HWND, excluded: &HashSet<String>) -> bool {
    if excluded.is_empty() {
        return false;
    }
    match get_process_exe_name(hwnd) {
        Some(name) => excluded.contains(&name),
        None => false,
    }
}

fn get_window_class(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    let len = unsafe { GetClassNameW(hwnd, &mut buf) };
    if len <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..len as usize])
}

/// Filter out desktop, taskbar, shell, child, invisible, and ZoneSnap's own windows.
pub fn is_manageable_window(hwnd: HWND, own_overlay_hwnd: isize) -> bool {
    if hwnd.0.is_null() {
        return false;
    }
    if own_overlay_hwnd != 0 && (hwnd.0 as isize) == own_overlay_hwnd {
        return false;
    }
    unsafe {
        if !IsWindowVisible(hwnd).as_bool() {
            return false;
        }
        let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
        if (style & WS_CHILD.0) != 0 {
            return false;
        }
    }

    let class = get_window_class(hwnd);
    !matches!(
        class.as_str(),
        "Shell_TrayWnd"
            | "Shell_SecondaryTrayWnd"
            | "Progman"
            | "WorkerW"
            | "NotifyIconOverflowWindow"
            | "TopLevelOverflowWindow"
            | "ZoneSnap_OverlayWindow"
            | "ZoneSnap_Daemon_MsgWindow"
    )
}

/// Returns true if `hwnd` still refers to an existing window handle.
pub fn is_valid_window(hwnd: HWND) -> bool {
    !hwnd.0.is_null() && unsafe { IsWindow(hwnd).as_bool() }
}

/// Returns true if the window has a sizing border (`WS_THICKFRAME`).
pub fn is_resizable_window(hwnd: HWND) -> bool {
    if hwnd.0.is_null() {
        return false;
    }
    let style = unsafe { GetWindowLongW(hwnd, GWL_STYLE) as u32 };
    (style & WS_THICKFRAME.0) != 0
}

/// If `hwnd` is maximized (`IsZoomed`), restore it with `SW_RESTORE` and return `true`.
pub fn restore_if_maximized(hwnd: HWND) -> bool {
    if hwnd.0.is_null() {
        return false;
    }
    unsafe {
        if IsZoomed(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
            return true;
        }
    }
    false
}

/// Read the window's bounding rectangle in physical screen coordinates.
pub fn get_window_rect(hwnd: HWND) -> Option<Rect> {
    if hwnd.0.is_null() {
        return None;
    }
    let mut rc = RECT::default();
    unsafe { GetWindowRect(hwnd, &mut rc) }.ok()?;
    Some(Rect::from_win32(rc))
}

/// Read the visible DWM extended frame bounds of `hwnd` in physical screen coordinates.
pub fn get_dwm_frame_rect(hwnd: HWND) -> Option<Rect> {
    if hwnd.0.is_null() {
        return None;
    }
    let mut rc = RECT::default();
    let res = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut rc as *mut RECT as *mut c_void,
            std::mem::size_of::<RECT>() as u32,
        )
    };
    if res.is_ok() {
        Some(Rect::from_win32(rc))
    } else {
        None
    }
}

/// Measure invisible DWM drop-shadow border thicknesses `(left, top, right, bottom)`.
pub fn get_dwm_border_offsets(hwnd: HWND) -> (i32, i32, i32, i32) {
    let Some(win) = get_window_rect(hwnd) else {
        return (0, 0, 0, 0);
    };
    let Some(ext) = get_dwm_frame_rect(hwnd) else {
        return (0, 0, 0, 0);
    };
    let l = (ext.left - win.left).clamp(-32, 32);
    let t = (ext.top - win.top).clamp(-32, 32);
    let r = (win.right - ext.right).clamp(-32, 32);
    let b = (win.bottom - ext.bottom).clamp(-32, 32);
    (l, t, r, b)
}

/// Expand `target` by DWM invisible border thicknesses so the visible frame aligns flush with `target`.
pub fn compute_compensated_rect(target: Rect, borders: (i32, i32, i32, i32)) -> Rect {
    Rect {
        left: target.left - borders.0,
        top: target.top - borders.1,
        right: target.right + borders.2,
        bottom: target.bottom + borders.3,
    }
}

/// Snap `hwnd` flush against `target` with DWM border compensation and mixed-DPI second-pass adjustment.
/// Caches pre-snap `RECT` in `snapped_cache` if not already recorded.
pub fn snap_window_flush(
    hwnd: HWND,
    target: Rect,
    snapped_cache: &mut HashMap<isize, Rect>,
    own_overlay_hwnd: isize,
) -> bool {
    if !is_manageable_window(hwnd, own_overlay_hwnd) || !is_resizable_window(hwnd) {
        return false;
    }

    unsafe {
        if IsZoomed(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
    }

    let key = hwnd_to_isize(hwnd);
    if !snapped_cache.contains_key(&key) {
        if let Some(orig) = get_window_rect(hwnd) {
            snapped_cache.insert(key, orig);
        }
    }

    let borders = get_dwm_border_offsets(hwnd);
    let comp = compute_compensated_rect(target, borders);
    let ok = unsafe {
        SetWindowPos(
            hwnd,
            HWND::default(),
            comp.left,
            comp.top,
            comp.width().max(1),
            comp.height().max(1),
            SWP_NOZORDER | SWP_NOACTIVATE,
        )
        .is_ok()
    };
    if !ok {
        return false;
    }

    // Second pass handles DPI scaling changes when snapping across monitors with different DPIs.
    if let Some(ext) = get_dwm_frame_rect(hwnd) {
        if (ext.left - target.left).abs() > 1
            || (ext.top - target.top).abs() > 1
            || (ext.right - target.right).abs() > 1
            || (ext.bottom - target.bottom).abs() > 1
        {
            let borders2 = get_dwm_border_offsets(hwnd);
            let comp2 = compute_compensated_rect(target, borders2);
            unsafe {
                let _ = SetWindowPos(
                    hwnd,
                    HWND::default(),
                    comp2.left,
                    comp2.top,
                    comp2.width().max(1),
                    comp2.height().max(1),
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
        }
    }

    true
}

/// Restore a previously snapped window's cached width & height centered around the cursor's grab point.
pub fn restore_unsnapped_window(
    hwnd: HWND,
    cursor_x: i32,
    cursor_y: i32,
    snapped_cache: &mut HashMap<isize, Rect>,
    is_alt_move: bool,
) -> Option<Rect> {
    let key = hwnd_to_isize(hwnd);
    let orig = snapped_cache.remove(&key)?;
    let w = orig.width().max(160);
    let h = orig.height().max(100);
    let new_left = cursor_x - w / 2;
    let new_top = if is_alt_move {
        cursor_y - h / 2
    } else {
        cursor_y - 16
    };
    let restored = Rect {
        left: new_left,
        top: new_top,
        right: new_left + w,
        bottom: new_top + h,
    };
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            HWND::default(),
            restored.left,
            restored.top,
            w,
            h,
            SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
    Some(restored)
}

/// Ask a window to cancel any modal drag/menu loop (`WM_CANCELMODE`) asynchronously.
pub fn cancel_modal_move(hwnd: HWND) {
    if !hwnd.0.is_null() {
        unsafe {
            let _ = PostMessageW(hwnd, WM_CANCELMODE, WPARAM(0), LPARAM(0));
        }
    }
}

/// Check if `(x, y)` lies on the window's titlebar (`HTCAPTION`) via non-blocking `SendMessageTimeoutW`.
pub fn is_titlebar_hit(hwnd: HWND, x: i32, y: i32) -> bool {
    if hwnd.0.is_null() {
        return false;
    }
    let lp = (((y as i16 as u16 as u32) << 16) | (x as i16 as u16 as u32)) as isize;
    let mut result: usize = 0;
    let ret = unsafe {
        SendMessageTimeoutW(
            hwnd,
            WM_NCHITTEST,
            WPARAM(0),
            LPARAM(lp),
            SMTO_ABORTIFHUNG,
            30,
            Some(&mut result),
        )
    };
    ret != LRESULT(0) && result == HTCAPTION as usize
}

/// Move a window without changing its size, z-order, or activation state.
/// Fails silently if the target window is elevated or rejects positioning.
pub fn move_window_nosize(hwnd: HWND, x: i32, y: i32) {
    if hwnd.0.is_null() {
        return;
    }
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            HWND::default(),
            x,
            y,
            0,
            0,
            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
}

/// Resize and position a window without changing its z-order or activation state.
pub fn resize_window(hwnd: HWND, rect: Rect) {
    if hwnd.0.is_null() {
        return;
    }
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            HWND::default(),
            rect.left,
            rect.top,
            rect.width(),
            rect.height(),
            SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
}

/// Divide `rect` into a 3x3 grid and determine which edge/corner `(x, y)` falls into.
/// The center cell `(Center, Center)` maps to `(Right, Bottom)`.
pub fn detect_resize_edges(rect: Rect, x: i32, y: i32) -> ResizeEdges {
    let w = rect.width().max(1);
    let h = rect.height().max(1);
    let rel_x = (x - rect.left).clamp(0, w - 1);
    let rel_y = (y - rect.top).clamp(0, h - 1);

    let horiz = if rel_x < w / 3 {
        HorizontalEdge::Left
    } else if rel_x >= (w * 2) / 3 {
        HorizontalEdge::Right
    } else {
        HorizontalEdge::Center
    };

    let vert = if rel_y < h / 3 {
        VerticalEdge::Top
    } else if rel_y >= (h * 2) / 3 {
        VerticalEdge::Bottom
    } else {
        VerticalEdge::Center
    };

    if horiz == HorizontalEdge::Center && vert == VerticalEdge::Center {
        ResizeEdges {
            horiz: HorizontalEdge::Right,
            vert: VerticalEdge::Bottom,
        }
    } else {
        ResizeEdges { horiz, vert }
    }
}

/// Split `rect` into 2x2 quadrants around its center and return the nearest corner.
pub fn detect_nearest_corner(rect: Rect, x: i32, y: i32) -> ResizeEdges {
    let mid_x = rect.left + rect.width() / 2;
    let mid_y = rect.top + rect.height() / 2;
    ResizeEdges {
        horiz: if x < mid_x {
            HorizontalEdge::Left
        } else {
            HorizontalEdge::Right
        },
        vert: if y < mid_y {
            VerticalEdge::Top
        } else {
            VerticalEdge::Bottom
        },
    }
}

/// Compute the resized rectangle from `orig`, `edges`, and cursor delta `(dx, dy)` while enforcing minimum dimensions.
pub fn compute_resized_rect(
    orig: Rect,
    edges: ResizeEdges,
    dx: i32,
    dy: i32,
    min_w: i32,
    min_h: i32,
) -> Rect {
    let mut left = orig.left;
    let mut top = orig.top;
    let mut right = orig.right;
    let mut bottom = orig.bottom;

    match edges.horiz {
        HorizontalEdge::Left => {
            left = (orig.left + dx).min(orig.right - min_w);
        }
        HorizontalEdge::Right => {
            right = (orig.right + dx).max(orig.left + min_w);
        }
        HorizontalEdge::Center => {}
    }

    match edges.vert {
        VerticalEdge::Top => {
            top = (orig.top + dy).min(orig.bottom - min_h);
        }
        VerticalEdge::Bottom => {
            bottom = (orig.bottom + dy).max(orig.top + min_h);
        }
        VerticalEdge::Center => {}
    }

    Rect {
        left,
        top,
        right,
        bottom,
    }
}

/// Confine the cursor to `monitor`'s physical screen bounds.
pub fn clip_cursor_to_monitor(monitor: &MonitorInfo) {
    let rc = monitor.rc_monitor.to_win32();
    unsafe {
        let _ = ClipCursor(Some(&rc));
    }
}

/// Release any active cursor confinement.
pub fn unclip_cursor() {
    unsafe {
        let _ = ClipCursor(None);
    }
}

/// Send an unassigned virtual key (`0xE8`) so releasing `Alt` after an Alt-drag does not focus the app menu bar.
pub fn send_menu_mask_key() {
    let make_input = |flags: KEYBD_EVENT_FLAGS| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(0xE8),
                wScan: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    let inputs = [
        make_input(KEYBD_EVENT_FLAGS(0)),
        make_input(KEYEVENTF_KEYUP),
    ];
    unsafe {
        let _ = SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
    }
}

#[inline]
pub fn hwnd_to_isize(hwnd: HWND) -> isize {
    hwnd.0 as isize
}

#[inline]
pub fn isize_to_hwnd(v: isize) -> HWND {
    HWND(v as *mut c_void)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dwm_border_compensation_expands_target_rect() {
        let target = Rect {
            left: 0,
            top: 0,
            right: 960,
            bottom: 1040,
        };
        let borders = (7, 0, 7, 7);
        assert_eq!(
            compute_compensated_rect(target, borders),
            Rect {
                left: -7,
                top: 0,
                right: 967,
                bottom: 1047,
            }
        );
    }

    #[test]
    fn detect_resize_edges_all_9_cells() {
        let r = Rect {
            left: 0,
            top: 0,
            right: 300,
            bottom: 300,
        };
        assert_eq!(
            detect_resize_edges(r, 10, 10),
            ResizeEdges {
                horiz: HorizontalEdge::Left,
                vert: VerticalEdge::Top
            }
        );
        assert_eq!(
            detect_resize_edges(r, 150, 10),
            ResizeEdges {
                horiz: HorizontalEdge::Center,
                vert: VerticalEdge::Top
            }
        );
        assert_eq!(
            detect_resize_edges(r, 250, 10),
            ResizeEdges {
                horiz: HorizontalEdge::Right,
                vert: VerticalEdge::Top
            }
        );
        assert_eq!(
            detect_resize_edges(r, 10, 150),
            ResizeEdges {
                horiz: HorizontalEdge::Left,
                vert: VerticalEdge::Center
            }
        );
        assert_eq!(
            detect_resize_edges(r, 150, 150),
            ResizeEdges {
                horiz: HorizontalEdge::Right,
                vert: VerticalEdge::Bottom
            }
        );
        assert_eq!(
            detect_resize_edges(r, 250, 150),
            ResizeEdges {
                horiz: HorizontalEdge::Right,
                vert: VerticalEdge::Center
            }
        );
        assert_eq!(
            detect_resize_edges(r, 10, 250),
            ResizeEdges {
                horiz: HorizontalEdge::Left,
                vert: VerticalEdge::Bottom
            }
        );
        assert_eq!(
            detect_resize_edges(r, 150, 250),
            ResizeEdges {
                horiz: HorizontalEdge::Center,
                vert: VerticalEdge::Bottom
            }
        );
        assert_eq!(
            detect_resize_edges(r, 250, 250),
            ResizeEdges {
                horiz: HorizontalEdge::Right,
                vert: VerticalEdge::Bottom
            }
        );
    }

    #[test]
    fn detect_nearest_corner_all_4_quadrants() {
        let r = Rect {
            left: 100,
            top: 200,
            right: 500,
            bottom: 600,
        };
        assert_eq!(
            detect_nearest_corner(r, 200, 300),
            ResizeEdges {
                horiz: HorizontalEdge::Left,
                vert: VerticalEdge::Top
            }
        );
        assert_eq!(
            detect_nearest_corner(r, 350, 300),
            ResizeEdges {
                horiz: HorizontalEdge::Right,
                vert: VerticalEdge::Top
            }
        );
        assert_eq!(
            detect_nearest_corner(r, 200, 450),
            ResizeEdges {
                horiz: HorizontalEdge::Left,
                vert: VerticalEdge::Bottom
            }
        );
        assert_eq!(
            detect_nearest_corner(r, 350, 450),
            ResizeEdges {
                horiz: HorizontalEdge::Right,
                vert: VerticalEdge::Bottom
            }
        );
    }

    #[test]
    fn compute_resized_rect_clamps_minimum_dimensions() {
        let orig = Rect {
            left: 100,
            top: 100,
            right: 500,
            bottom: 400,
        };
        let tl = ResizeEdges {
            horiz: HorizontalEdge::Left,
            vert: VerticalEdge::Top,
        };
        let clamped_tl = compute_resized_rect(orig, tl, 1000, 1000, 160, 100);
        assert_eq!(clamped_tl.width(), 160);
        assert_eq!(clamped_tl.height(), 100);
        assert_eq!(clamped_tl.right, 500);
        assert_eq!(clamped_tl.bottom, 400);

        let br = ResizeEdges {
            horiz: HorizontalEdge::Right,
            vert: VerticalEdge::Bottom,
        };
        let clamped_br = compute_resized_rect(orig, br, -1000, -1000, 160, 100);
        assert_eq!(clamped_br.width(), 160);
        assert_eq!(clamped_br.height(), 100);
        assert_eq!(clamped_br.left, 100);
        assert_eq!(clamped_br.top, 100);

        let expanded = compute_resized_rect(orig, br, 50, 25, 160, 100);
        assert_eq!(
            expanded,
            Rect {
                left: 100,
                top: 100,
                right: 550,
                bottom: 425
            }
        );
    }
}
