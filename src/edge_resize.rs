use crate::config::EdgeResizeConfig;
use crate::hook::{pack_point, HookSharedState};
use crate::monitor::{MonitorInfo, Rect};
use crate::window_ops::{
    can_resize_target_uipi, get_dwm_frame_rect, get_root_window_at, is_excluded_app,
    is_manageable_window, is_resizable_window, HorizontalEdge, ResizeEdges, VerticalEdge,
};
use std::collections::HashSet;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU8, Ordering};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, SelectObject, AC_SRC_ALPHA,
    AC_SRC_OVER, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION, DIB_RGB_COLORS, HBITMAP, HDC,
    HGDIOBJ,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetAncestor, GetForegroundWindow, IsZoomed,
    LoadCursorW, PostMessageW, RegisterClassW, SetCursor, SetWindowPos, ShowWindow,
    UpdateLayeredWindow, GA_ROOT, HWND_TOPMOST, IDC_ARROW, IDC_SIZENESW, IDC_SIZENS, IDC_SIZENWSE,
    IDC_SIZEWE, MA_NOACTIVATE, SWP_NOACTIVATE, SWP_NOSIZE, SW_HIDE, SW_SHOWNA, ULW_ALPHA, WM_APP, WM_CAPTURECHANGED, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MOUSEACTIVATE, WM_SETCURSOR, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_POPUP,
};

pub const WM_ZONESNAP_FG_CHANGED: u32 = WM_APP + 11;
pub const WM_ZONESNAP_HANDLE_DOWN: u32 = WM_APP + 12;
pub const WM_ZONESNAP_HANDLE_UP: u32 = WM_APP + 13;

const EVENT_SYSTEM_FOREGROUND: u32 = 0x0003;
const EVENT_SYSTEM_MOVESIZEEND: u32 = 0x000B;
const EVENT_SYSTEM_MINIMIZESTART: u32 = 0x0016;
const EVENT_OBJECT_DESTROY: u32 = 0x8001;
const EVENT_OBJECT_LOCATIONCHANGE: u32 = 0x800B;
const WINEVENT_OUTOFCONTEXT: u32 = 0x0000;
const WINEVENT_SKIPOWNPROCESS: u32 = 0x0002;

static HANDLE_DAEMON_HWND: AtomicIsize = AtomicIsize::new(0);
static HANDLE_CURSOR_KIND: AtomicU8 = AtomicU8::new(0);
static HANDLE_DRAGGING: AtomicBool = AtomicBool::new(false);

static WINEVENT_DAEMON_HWND: AtomicIsize = AtomicIsize::new(0);
static WINEVENT_TRACKED_FG: AtomicIsize = AtomicIsize::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EdgeHit {
    pub edges: ResizeEdges,
    pub anchor_x: i32,
    pub anchor_y: i32,
    pub is_corner: bool,
}

fn cursor_kind_for_edges(edges: ResizeEdges) -> u8 {
    match (edges.horiz, edges.vert) {
        (HorizontalEdge::Left, VerticalEdge::Top)
        | (HorizontalEdge::Right, VerticalEdge::Bottom) => 1, // NWSE
        (HorizontalEdge::Right, VerticalEdge::Top)
        | (HorizontalEdge::Left, VerticalEdge::Bottom) => 2, // NESW
        (HorizontalEdge::Left | HorizontalEdge::Right, VerticalEdge::Center) => 3, // WE
        (HorizontalEdge::Center, VerticalEdge::Top | VerticalEdge::Bottom) => 4, // NS
        _ => 0,
    }
}

/// Compute outer and inner integer bounds `(outer_lt, outer_rb, inner_lt, inner_rb)`
/// for fast pure-integer ring rejection inside `WH_MOUSE_LL`.
pub fn compute_ring_bounds(dwm: Rect, pad_px: i32, scale: f32) -> (i64, i64, i64, i64) {
    let out_pad = pad_px.max(4);
    let in_pad = (2.0 * scale).round().max(1.0) as i32;
    let in_top = (1.0 * scale).round().max(1.0) as i32;
    let outer_lt = pack_point(dwm.left - out_pad, dwm.top - out_pad);
    let outer_rb = pack_point(dwm.right + out_pad, dwm.bottom + out_pad);
    let inner_lt = pack_point(dwm.left + in_pad, dwm.top + in_top);
    let inner_rb = pack_point(dwm.right - in_pad, dwm.bottom - in_pad);
    (outer_lt, outer_rb, inner_lt, inner_rb)
}

/// Hit-test whether `(x, y)` lies in the expanded resize border/corner ring around `dwm`.
/// Biases expansion outward to avoid eating scrollbars, title bars, or top-right caption buttons.
pub fn hit_test_edge_resize(
    dwm: Rect,
    x: i32,
    y: i32,
    out_pad_px: i32,
    scale: f32,
    corners_only: bool,
) -> Option<EdgeHit> {
    if dwm.width() < 80 || dwm.height() < 60 {
        return None;
    }
    let scale = scale.clamp(0.75, 4.0);
    let out_pad = out_pad_px.max(4);
    let in_pad = (2.0 * scale).round().max(1.0) as i32;
    let in_top = (1.0 * scale).round().max(1.0) as i32;
    let corner_arm = (20.0 * scale).round().max(out_pad as f32 * 1.5) as i32;

    if x < dwm.left - out_pad
        || x > dwm.right + out_pad
        || y < dwm.top - out_pad
        || y > dwm.bottom + out_pad
    {
        return None;
    }

    if x > dwm.left + in_pad
        && x < dwm.right - in_pad
        && y > dwm.top + in_top
        && y < dwm.bottom - in_pad
    {
        return None;
    }

    // Protect top-right caption buttons (Minimize / Maximize / Close) from inward grab overlap.
    let cap_w = (140.0 * scale).round() as i32;
    let cap_h = (34.0 * scale).round() as i32;
    if x >= dwm.right - cap_w
        && x < dwm.right - 1
        && y > dwm.top + 1
        && y < dwm.top + cap_h
    {
        return None;
    }

    let near_left_c = x <= dwm.left + corner_arm;
    let near_right_c = x >= dwm.right - corner_arm;
    let near_top_c = y <= dwm.top + corner_arm;
    let near_bottom_c = y >= dwm.bottom - corner_arm;

    if near_left_c && near_top_c {
        return Some(EdgeHit {
            edges: ResizeEdges {
                horiz: HorizontalEdge::Left,
                vert: VerticalEdge::Top,
            },
            anchor_x: dwm.left,
            anchor_y: dwm.top,
            is_corner: true,
        });
    }
    if near_right_c && near_top_c {
        return Some(EdgeHit {
            edges: ResizeEdges {
                horiz: HorizontalEdge::Right,
                vert: VerticalEdge::Top,
            },
            anchor_x: dwm.right,
            anchor_y: dwm.top,
            is_corner: true,
        });
    }
    if near_left_c && near_bottom_c {
        return Some(EdgeHit {
            edges: ResizeEdges {
                horiz: HorizontalEdge::Left,
                vert: VerticalEdge::Bottom,
            },
            anchor_x: dwm.left,
            anchor_y: dwm.bottom,
            is_corner: true,
        });
    }
    if near_right_c && near_bottom_c {
        return Some(EdgeHit {
            edges: ResizeEdges {
                horiz: HorizontalEdge::Right,
                vert: VerticalEdge::Bottom,
            },
            anchor_x: dwm.right,
            anchor_y: dwm.bottom,
            is_corner: true,
        });
    }

    if corners_only {
        return None;
    }

    if x <= dwm.left + in_pad {
        return Some(EdgeHit {
            edges: ResizeEdges {
                horiz: HorizontalEdge::Left,
                vert: VerticalEdge::Center,
            },
            anchor_x: dwm.left,
            anchor_y: y.clamp(dwm.top + corner_arm, dwm.bottom - corner_arm),
            is_corner: false,
        });
    }
    if x >= dwm.right - in_pad {
        return Some(EdgeHit {
            edges: ResizeEdges {
                horiz: HorizontalEdge::Right,
                vert: VerticalEdge::Center,
            },
            anchor_x: dwm.right,
            anchor_y: y.clamp(dwm.top + corner_arm, dwm.bottom - corner_arm),
            is_corner: false,
        });
    }
    if y <= dwm.top + in_top {
        return Some(EdgeHit {
            edges: ResizeEdges {
                horiz: HorizontalEdge::Center,
                vert: VerticalEdge::Top,
            },
            anchor_x: x.clamp(dwm.left + corner_arm, dwm.right - corner_arm),
            anchor_y: dwm.top,
            is_corner: false,
        });
    }
    if y >= dwm.bottom - in_pad {
        return Some(EdgeHit {
            edges: ResizeEdges {
                horiz: HorizontalEdge::Center,
                vert: VerticalEdge::Bottom,
            },
            anchor_x: x.clamp(dwm.left + corner_arm, dwm.right - corner_arm),
            anchor_y: dwm.bottom,
            is_corner: false,
        });
    }

    None
}

/// Sample a point 4px inside `dwm` near `hit.anchor` to verify `fg_hwnd` isn't occluded by a topmost/taskbar window.
fn is_edge_visible_at_anchor(fg_hwnd: HWND, dwm: Rect, hit: EdgeHit) -> bool {
    let sx = if hit.anchor_x <= dwm.left + 4 {
        dwm.left + 6
    } else if hit.anchor_x >= dwm.right - 4 {
        dwm.right - 6
    } else {
        hit.anchor_x
    };
    let sy = if hit.anchor_y <= dwm.top + 4 {
        dwm.top + 6
    } else if hit.anchor_y >= dwm.bottom - 4 {
        dwm.bottom - 6
    } else {
        hit.anchor_y
    };
    let Some(top_root) = get_root_window_at(sx, sy) else {
        return false;
    };
    top_root.0 == fg_hwnd.0
}

unsafe extern "system" fn handle_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_SETCURSOR => {
            let id = match HANDLE_CURSOR_KIND.load(Ordering::Acquire) {
                1 => IDC_SIZENWSE,
                2 => IDC_SIZENESW,
                3 => IDC_SIZEWE,
                4 => IDC_SIZENS,
                _ => IDC_ARROW,
            };
            if let Ok(cur) = LoadCursorW(None, id) {
                SetCursor(cur);
            }
            LRESULT(1)
        }
        WM_LBUTTONDOWN => {
            if !HANDLE_DRAGGING.swap(true, Ordering::AcqRel) {
                SetCapture(hwnd);
                let dh = HANDLE_DAEMON_HWND.load(Ordering::Acquire);
                if dh != 0 {
                    let _ = PostMessageW(
                        HWND(dh as *mut c_void),
                        WM_ZONESNAP_HANDLE_DOWN,
                        WPARAM(0),
                        LPARAM(0),
                    );
                }
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            if HANDLE_DRAGGING.swap(false, Ordering::AcqRel) {
                let _ = ReleaseCapture();
                let dh = HANDLE_DAEMON_HWND.load(Ordering::Acquire);
                if dh != 0 {
                    let _ = PostMessageW(
                        HWND(dh as *mut c_void),
                        WM_ZONESNAP_HANDLE_UP,
                        WPARAM(0),
                        LPARAM(0),
                    );
                }
            }
            LRESULT(0)
        }
        WM_CAPTURECHANGED => {
            if HANDLE_DRAGGING.swap(false, Ordering::AcqRel) {
                let dh = HANDLE_DAEMON_HWND.load(Ordering::Acquire);
                if dh != 0 {
                    let _ = PostMessageW(
                        HWND(dh as *mut c_void),
                        WM_ZONESNAP_HANDLE_UP,
                        WPARAM(0),
                        LPARAM(0),
                    );
                }
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HandleSurfaceKey {
    win_size: i32,
    circle_diam: i32,
    show_handles: bool,
    color: [u8; 3],
}

struct ResizeHandleWindow {
    hwnd: HWND,
    visible: bool,
    pos_x: i32,
    pos_y: i32,
    mem_dc: HDC,
    hbmp: HBITMAP,
    old_bmp: HGDIOBJ,
    bits_ptr: *mut u32,
    buf_size: i32,
    surface_key: Option<HandleSurfaceKey>,
}

impl ResizeHandleWindow {
    fn new() -> Self {
        Self {
            hwnd: HWND::default(),
            visible: false,
            pos_x: 0,
            pos_y: 0,
            mem_dc: HDC::default(),
            hbmp: HBITMAP::default(),
            old_bmp: HGDIOBJ::default(),
            bits_ptr: std::ptr::null_mut(),
            buf_size: 0,
            surface_key: None,
        }
    }

    fn ensure_window(&mut self, daemon_hwnd: HWND) -> bool {
        HANDLE_DAEMON_HWND.store(daemon_hwnd.0 as isize, Ordering::Release);
        if !self.hwnd.0.is_null() {
            return true;
        }
        unsafe {
            let hinstance: HINSTANCE = GetModuleHandleW(None).unwrap_or_default().into();
            let class_name = w!("ZoneSnap_ResizeHandleWindow");
            let wc = WNDCLASSW {
                lpfnWndProc: Some(handle_wnd_proc),
                hInstance: hinstance,
                lpszClassName: class_name,
                ..Default::default()
            };
            RegisterClassW(&wc);

            let ex_style = WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE;
            let Ok(hwnd) = CreateWindowExW(
                ex_style,
                class_name,
                PCWSTR::null(),
                WS_POPUP,
                0,
                0,
                1,
                1,
                HWND::default(),
                None,
                hinstance,
                None,
            ) else {
                return false;
            };
            self.hwnd = hwnd;
            true
        }
    }

    fn release_dib(&mut self) {
        unsafe {
            if !self.mem_dc.0.is_null() {
                if !self.old_bmp.0.is_null() {
                    SelectObject(self.mem_dc, self.old_bmp);
                    self.old_bmp = HGDIOBJ::default();
                }
                if !self.hbmp.0.is_null() {
                    let _ = DeleteObject(HGDIOBJ(self.hbmp.0));
                    self.hbmp = HBITMAP::default();
                }
                let _ = DeleteDC(self.mem_dc);
                self.mem_dc = HDC::default();
            }
        }
        self.bits_ptr = std::ptr::null_mut();
        self.buf_size = 0;
        self.surface_key = None;
    }

    fn destroy(&mut self) {
        self.hide();
        self.release_dib();
        if !self.hwnd.0.is_null() {
            unsafe {
                let _ = DestroyWindow(self.hwnd);
            }
            self.hwnd = HWND::default();
        }
    }

    fn hide(&mut self) {
        if self.visible && !self.hwnd.0.is_null() {
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_HIDE);
            }
            self.visible = false;
        }
        if !HANDLE_DRAGGING.load(Ordering::Acquire) {
            HANDLE_CURSOR_KIND.store(0, Ordering::Release);
        }
    }

    fn enter_capture_stealth(&mut self) {
        if self.hwnd.0.is_null() {
            return;
        }
        let key = HandleSurfaceKey {
            win_size: 8,
            circle_diam: 8,
            show_handles: false,
            color: [0, 0, 0],
        };
        if self.ensure_dib(8) {
            self.paint_surface(key);
            unsafe {
                let pt_dst = POINT {
                    x: self.pos_x,
                    y: self.pos_y,
                };
                let size = SIZE { cx: 8, cy: 8 };
                let pt_src = POINT { x: 0, y: 0 };
                let blend = BLENDFUNCTION {
                    BlendOp: AC_SRC_OVER as u8,
                    BlendFlags: 0,
                    SourceConstantAlpha: 255,
                    AlphaFormat: AC_SRC_ALPHA as u8,
                };
                let _ = UpdateLayeredWindow(
                    self.hwnd,
                    HDC::default(),
                    Some(&pt_dst),
                    Some(&size),
                    self.mem_dc,
                    Some(&pt_src),
                    COLORREF(0),
                    Some(&blend),
                    ULW_ALPHA,
                );
            }
        }
    }

    fn ensure_dib(&mut self, sz: i32) -> bool {
        if !self.bits_ptr.is_null() && self.buf_size == sz {
            return true;
        }
        self.release_dib();
        unsafe {
            let hdc = CreateCompatibleDC(HDC::default());
            if hdc.0.is_null() {
                return false;
            }
            let mut bmi = BITMAPINFO::default();
            bmi.bmiHeader = BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: sz,
                biHeight: -sz,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            };
            let mut bits: *mut c_void = std::ptr::null_mut();
            let Ok(hbmp) = CreateDIBSection(hdc, &bmi, DIB_RGB_COLORS, &mut bits, None, 0) else {
                let _ = DeleteDC(hdc);
                return false;
            };
            if bits.is_null() {
                let _ = DeleteObject(HGDIOBJ(hbmp.0));
                let _ = DeleteDC(hdc);
                return false;
            }
            let old_bmp = SelectObject(hdc, HGDIOBJ(hbmp.0));
            self.mem_dc = hdc;
            self.hbmp = hbmp;
            self.old_bmp = old_bmp;
            self.bits_ptr = bits as *mut u32;
            self.buf_size = sz;
            true
        }
    }

    fn paint_surface(&mut self, key: HandleSurfaceKey) {
        if self.bits_ptr.is_null() {
            return;
        }
        let sz = key.win_size;
        let buf = unsafe { std::slice::from_raw_parts_mut(self.bits_ptr, (sz * sz) as usize) };
        let c = (sz as f32 - 1.0) * 0.5;
        let hit_r = (sz as f32) * 0.5;

        if !key.show_handles {
            // 1/255 alpha is visually invisible while remaining 100% hit-testable by DWM.
            let hit_px = 0x0100_0000u32;
            for y in 0..sz {
                for x in 0..sz {
                    let dx = x as f32 - c;
                    let dy = y as f32 - c;
                    buf[(y * sz + x) as usize] = if (dx * dx + dy * dy) <= hit_r * hit_r {
                        hit_px
                    } else {
                        0
                    };
                }
            }
            self.surface_key = Some(key);
            return;
        }

        let vis_r = (key.circle_diam as f32 * 0.5).min(hit_r - 0.5).max(4.0);
        let [cr, cg, cb] = key.color;
        let lum = 0.2126 * (cr as f32 / 255.0)
            + 0.7152 * (cg as f32 / 255.0)
            + 0.0722 * (cb as f32 / 255.0);
        let (or, og, ob) = if lum > 0.35 {
            (15u8, 23u8, 42u8)
        } else {
            (248u8, 250u8, 252u8)
        };

        for y in 0..sz {
            for x in 0..sz {
                let dx = x as f32 - c;
                let dy = y as f32 - c;
                let dist = (dx * dx + dy * dy).sqrt();
                let px = if dist > hit_r {
                    0u32
                } else if dist > vis_r + 0.75 {
                    0x0100_0000u32
                } else {
                    let edge_cov = (vis_r + 0.75 - dist).clamp(0.0, 1.0);
                    let border_t = ((dist - (vis_r - 1.8)).clamp(0.0, 1.0)) * 0.9;
                    let r = cr as f32 * (1.0 - border_t) + or as f32 * border_t;
                    let g = cg as f32 * (1.0 - border_t) + og as f32 * border_t;
                    let b = cb as f32 * (1.0 - border_t) + ob as f32 * border_t;
                    let a = (232.0 * edge_cov).round().clamp(1.0, 255.0) as u32;
                    let pr = ((r * (a as f32 / 255.0)).round() as u32).min(255);
                    let pg = ((g * (a as f32 / 255.0)).round() as u32).min(255);
                    let pb = ((b * (a as f32 / 255.0)).round() as u32).min(255);
                    (a << 24) | (pr << 16) | (pg << 8) | pb
                };
                buf[(y * sz + x) as usize] = px;
            }
        }
        self.surface_key = Some(key);
    }

    fn show_at(
        &mut self,
        daemon_hwnd: HWND,
        x: i32,
        y: i32,
        key: HandleSurfaceKey,
        edges: ResizeEdges,
    ) {
        if !self.ensure_window(daemon_hwnd) {
            return;
        }
        HANDLE_CURSOR_KIND.store(cursor_kind_for_edges(edges), Ordering::Release);

        let needs_surface = !self.visible || self.surface_key != Some(key);
        if needs_surface {
            if !self.ensure_dib(key.win_size) {
                return;
            }
            if self.surface_key != Some(key) {
                self.paint_surface(key);
            }
            self.pos_x = x;
            self.pos_y = y;
            unsafe {
                let pt_dst = POINT { x, y };
                let size = SIZE {
                    cx: key.win_size,
                    cy: key.win_size,
                };
                let pt_src = POINT { x: 0, y: 0 };
                let blend = BLENDFUNCTION {
                    BlendOp: AC_SRC_OVER as u8,
                    BlendFlags: 0,
                    SourceConstantAlpha: 255,
                    AlphaFormat: AC_SRC_ALPHA as u8,
                };
                let _ = UpdateLayeredWindow(
                    self.hwnd,
                    HDC::default(),
                    Some(&pt_dst),
                    Some(&size),
                    self.mem_dc,
                    Some(&pt_src),
                    COLORREF(0),
                    Some(&blend),
                    ULW_ALPHA,
                );
                let _ = SetWindowPos(
                    self.hwnd,
                    HWND_TOPMOST,
                    x,
                    y,
                    key.win_size,
                    key.win_size,
                    SWP_NOACTIVATE,
                );
                if !self.visible {
                    let _ = ShowWindow(self.hwnd, SW_SHOWNA);
                    self.visible = true;
                }
            }
        } else if self.pos_x != x || self.pos_y != y {
            self.pos_x = x;
            self.pos_y = y;
            unsafe {
                let _ = SetWindowPos(
                    self.hwnd,
                    HWND_TOPMOST,
                    x,
                    y,
                    0,
                    0,
                    SWP_NOSIZE | SWP_NOACTIVATE,
                );
            }
        }
    }
}

impl Drop for ResizeHandleWindow {
    fn drop(&mut self) {
        self.destroy();
    }
}

unsafe extern "system" fn win_event_proc(
    _hook: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    id_object: i32,
    id_child: i32,
    _id_thread: u32,
    _time: u32,
) {
    let daemon = WINEVENT_DAEMON_HWND.load(Ordering::Acquire);
    if daemon == 0 {
        return;
    }
    if event == EVENT_OBJECT_LOCATIONCHANGE || event == EVENT_OBJECT_DESTROY {
        if id_object != 0 || id_child != 0 || hwnd.0.is_null() {
            return;
        }
        if (hwnd.0 as isize) != WINEVENT_TRACKED_FG.load(Ordering::Acquire) {
            return;
        }
    }
    let _ = PostMessageW(
        HWND(daemon as *mut c_void),
        WM_ZONESNAP_FG_CHANGED,
        WPARAM(0),
        LPARAM(0),
    );
}

struct WinEventTracker {
    hooks: Vec<HWINEVENTHOOK>,
}

impl WinEventTracker {
    fn new() -> Self {
        Self { hooks: Vec::new() }
    }

    fn set_enabled(&mut self, enabled: bool, daemon_hwnd: HWND) {
        if !enabled {
            self.unhook_all();
            WINEVENT_TRACKED_FG.store(0, Ordering::Release);
            return;
        }
        WINEVENT_DAEMON_HWND.store(daemon_hwnd.0 as isize, Ordering::Release);
        if !self.hooks.is_empty() {
            return;
        }
        let flags = WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS;
        let events = [
            EVENT_SYSTEM_FOREGROUND,
            EVENT_SYSTEM_MOVESIZEEND,
            EVENT_SYSTEM_MINIMIZESTART,
            EVENT_OBJECT_DESTROY,
            EVENT_OBJECT_LOCATIONCHANGE,
        ];
        unsafe {
            for ev in events {
                let h = SetWinEventHook(ev, ev, HINSTANCE::default(), Some(win_event_proc), 0, 0, flags);
                if !h.0.is_null() {
                    self.hooks.push(h);
                }
            }
        }
    }

    fn unhook_all(&mut self) {
        unsafe {
            for h in self.hooks.drain(..) {
                if !h.0.is_null() {
                    let _ = UnhookWinEvent(h);
                }
            }
        }
    }
}

impl Drop for WinEventTracker {
    fn drop(&mut self) {
        self.unhook_all();
    }
}

#[derive(Debug, Clone, Copy)]
struct TrackedWindow {
    hwnd_raw: isize,
    dwm: Rect,
    mon_rc: Rect,
    dpi_scale: f32,
}

pub struct EdgeResizeManager {
    tracker: WinEventTracker,
    handle_win: ResizeHandleWindow,
    tracked: Option<TrackedWindow>,
    current_hit: Option<EdgeHit>,
}

impl EdgeResizeManager {
    pub fn new() -> Self {
        Self {
            tracker: WinEventTracker::new(),
            handle_win: ResizeHandleWindow::new(),
            tracked: None,
            current_hit: None,
        }
    }

    pub fn apply_config(
        &mut self,
        cfg: &EdgeResizeConfig,
        daemon_hwnd: HWND,
        shared: &HookSharedState,
        monitors: &mut Vec<MonitorInfo>,
        excluded: &HashSet<String>,
        overlay_hwnd: isize,
    ) {
        shared
            .edge_resize_enabled
            .store(cfg.enabled, Ordering::Release);
        self.tracker.set_enabled(cfg.enabled, daemon_hwnd);
        if !cfg.enabled {
            self.hide_handle(shared);
            self.handle_win.destroy();
            self.tracked = None;
            shared.clear_edge_target();
            return;
        }
        self.refresh_foreground(cfg, shared, monitors, excluded, overlay_hwnd);
    }

    pub fn hide_handle(&mut self, shared: &HookSharedState) {
        self.current_hit = None;
        shared.edge_hover_active.store(false, Ordering::Release);
        shared.set_handle_bounds(false, 0, 0);
        self.handle_win.hide();
    }

    pub fn begin_drag_capture(&mut self, shared: &HookSharedState) -> Option<(isize, Rect, ResizeEdges)> {
        let tracked = self.tracked?;
        let hit = self.current_hit?;
        self.handle_win.enter_capture_stealth();
        shared.set_handle_bounds(false, 0, 0);
        Some((tracked.hwnd_raw, tracked.dwm, hit.edges))
    }

    pub fn end_drag_capture(&mut self, shared: &HookSharedState) {
        if HANDLE_DRAGGING.swap(false, Ordering::AcqRel) {
            unsafe {
                let _ = ReleaseCapture();
            }
        }
        self.hide_handle(shared);
    }

    pub fn refresh_foreground(
        &mut self,
        cfg: &EdgeResizeConfig,
        shared: &HookSharedState,
        monitors: &mut Vec<MonitorInfo>,
        excluded: &HashSet<String>,
        overlay_hwnd: isize,
    ) {
        if !cfg.enabled {
            return;
        }
        let fg = unsafe { GetForegroundWindow() };
        if fg.0.is_null() {
            self.clear_tracked(shared);
            return;
        }
        let root = unsafe { GetAncestor(fg, GA_ROOT) };
        let target = if root.0.is_null() { fg } else { root };
        let target_raw = target.0 as isize;
        if target_raw == (self.handle_win.hwnd.0 as isize) {
            return;
        }

        if !is_manageable_window(target, overlay_hwnd)
            || !is_resizable_window(target)
            || unsafe { IsZoomed(target).as_bool() }
            || is_excluded_app(target, excluded)
            || !can_resize_target_uipi(target)
        {
            self.clear_tracked(shared);
            return;
        }

        let Some(dwm) = get_dwm_frame_rect(target) else {
            self.clear_tracked(shared);
            return;
        };
        if dwm.width() < 80 || dwm.height() < 60 {
            self.clear_tracked(shared);
            return;
        }

        let cx = dwm.left + dwm.width() / 2;
        let cy = dwm.top + dwm.height() / 2;
        let mon = monitors
            .iter()
            .find(|m| m.rc_monitor.contains(cx, cy))
            .cloned()
            .unwrap_or_else(|| {
                let refreshed = crate::monitor::enumerate_monitors();
                *monitors = refreshed.clone();
                refreshed
                    .into_iter()
                    .find(|m| m.rc_monitor.contains(cx, cy))
                    .unwrap_or(MonitorInfo {
                        hmonitor: 0,
                        device_name: String::new(),
                        rc_monitor: dwm,
                        rc_work: dwm,
                        is_primary: true,
                        dpi_x: 96,
                        dpi_y: 96,
                    })
            });

        // Skip borderless fullscreen apps/games covering the entire monitor.
        if dwm == mon.rc_monitor {
            self.clear_tracked(shared);
            return;
        }

        let dpi_scale = (mon.dpi_x as f32 / 96.0).clamp(1.0, 4.0);
        let pad_scaled = (cfg.grab_padding_px as f32 * dpi_scale).round() as i32;
        let (olt, orb, ilt, irb) = compute_ring_bounds(dwm, pad_scaled, dpi_scale);

        WINEVENT_TRACKED_FG.store(target_raw, Ordering::Release);
        self.tracked = Some(TrackedWindow {
            hwnd_raw: target_raw,
            dwm,
            mon_rc: mon.rc_monitor,
            dpi_scale,
        });
        shared.set_edge_target(olt, orb, ilt, irb);
    }

    fn clear_tracked(&mut self, shared: &HookSharedState) {
        WINEVENT_TRACKED_FG.store(0, Ordering::Release);
        self.tracked = None;
        shared.clear_edge_target();
        self.hide_handle(shared);
    }

    pub fn on_hover_move(
        &mut self,
        x: i32,
        y: i32,
        cfg: &EdgeResizeConfig,
        daemon_hwnd: HWND,
        shared: &HookSharedState,
    ) {
        if !cfg.enabled {
            return;
        }
        let Some(tracked) = self.tracked else {
            self.hide_handle(shared);
            return;
        };
        let pad_scaled = (cfg.grab_padding_px as f32 * tracked.dpi_scale).round() as i32;
        let Some(hit) = hit_test_edge_resize(
            tracked.dwm,
            x,
            y,
            pad_scaled,
            tracked.dpi_scale,
            cfg.corners_only,
        ) else {
            self.hide_handle(shared);
            return;
        };

        let fg_hwnd = HWND(tracked.hwnd_raw as *mut c_void);
        if self.current_hit.is_none() && !is_edge_visible_at_anchor(fg_hwnd, tracked.dwm, hit) {
            self.hide_handle(shared);
            return;
        }

        let circle_diam = (cfg.handle_size_px as f32 * tracked.dpi_scale)
            .round()
            .clamp(12.0, 56.0) as i32;
        let win_size = (circle_diam + 6).max(pad_scaled * 2 + 6).clamp(22, 64);

        let half = win_size / 2;
        let min_x = tracked.mon_rc.left;
        let max_x = (tracked.mon_rc.right - win_size).max(min_x);
        let min_y = tracked.mon_rc.top;
        let max_y = (tracked.mon_rc.bottom - win_size).max(min_y);
        let wx = (hit.anchor_x - half).clamp(min_x, max_x);
        let wy = (hit.anchor_y - half).clamp(min_y, max_y);

        let key = HandleSurfaceKey {
            win_size,
            circle_diam,
            show_handles: cfg.show_handles,
            color: cfg.handle_color,
        };
        self.handle_win
            .show_at(daemon_hwnd, wx, wy, key, hit.edges);
        self.current_hit = Some(hit);
        shared.edge_hover_active.store(true, Ordering::Release);
        shared.set_handle_bounds(
            true,
            pack_point(wx, wy),
            pack_point(wx + win_size, wy + win_size),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_dwm() -> Rect {
        Rect {
            left: 200,
            top: 100,
            right: 1000,
            bottom: 700,
        }
    }

    #[test]
    fn hit_test_detects_all_corners_and_edges() {
        let r = sample_dwm();
        // Outside left-top corner within 10px outward pad
        let nw = hit_test_edge_resize(r, 194, 95, 10, 1.0, false).expect("nw hit");
        assert!(nw.is_corner);
        assert_eq!(nw.edges.horiz, HorizontalEdge::Left);
        assert_eq!(nw.edges.vert, VerticalEdge::Top);
        assert_eq!((nw.anchor_x, nw.anchor_y), (200, 100));

        // Outside right-bottom corner
        let se = hit_test_edge_resize(r, 1008, 706, 10, 1.0, false).expect("se hit");
        assert!(se.is_corner);
        assert_eq!(se.edges.horiz, HorizontalEdge::Right);
        assert_eq!(se.edges.vert, VerticalEdge::Bottom);
        assert_eq!((se.anchor_x, se.anchor_y), (1000, 700));

        // Mid right edge outside by 6px
        let east = hit_test_edge_resize(r, 1006, 400, 10, 1.0, false).expect("east hit");
        assert!(!east.is_corner);
        assert_eq!(east.edges.horiz, HorizontalEdge::Right);
        assert_eq!(east.edges.vert, VerticalEdge::Center);
        assert_eq!((east.anchor_x, east.anchor_y), (1000, 400));

        // Mid bottom edge outside by 5px
        let south = hit_test_edge_resize(r, 600, 705, 10, 1.0, false).expect("south hit");
        assert!(!south.is_corner);
        assert_eq!(south.edges.horiz, HorizontalEdge::Center);
        assert_eq!(south.edges.vert, VerticalEdge::Bottom);
    }

    #[test]
    fn hit_test_respects_corners_only_interior_and_caption_guard() {
        let r = sample_dwm();
        // Straight edge rejected when corners_only = true
        assert!(hit_test_edge_resize(r, 1006, 400, 10, 1.0, true).is_none());
        // Corner still hits when corners_only = true
        assert!(hit_test_edge_resize(r, 1006, 705, 10, 1.0, true).is_some());
        // Interior rejected
        assert!(hit_test_edge_resize(r, 500, 400, 10, 1.0, false).is_none());
        // Far outside rejected
        assert!(hit_test_edge_resize(r, 1025, 400, 10, 1.0, false).is_none());
        // Top-right caption button area (inward) is protected
        assert!(hit_test_edge_resize(r, 980, 115, 10, 1.0, false).is_none());
        // Outside top-right corner still hits NE
        let ne = hit_test_edge_resize(r, 1006, 95, 10, 1.0, false).expect("ne hit");
        assert_eq!(ne.edges.horiz, HorizontalEdge::Right);
        assert_eq!(ne.edges.vert, VerticalEdge::Top);
    }
}
