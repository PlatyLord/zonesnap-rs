use crate::icon::app_icon_bgra_u32;
use std::ffi::c_void;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateBitmap, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, BITMAPINFO,
    BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ,
};
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreateIconIndirect, CreatePopupMenu, DestroyIcon, DestroyMenu, GetCursorPos,
    PostMessageW, SetForegroundWindow, TrackPopupMenu, HICON, HMENU, ICONINFO, MF_CHECKED,
    MF_POPUP, MF_SEPARATOR, MF_STRING, MF_UNCHECKED, TPM_BOTTOMALIGN, TPM_LEFTALIGN,
    TPM_RIGHTBUTTON, WM_APP, WM_NULL,
};

pub const WM_TRAYICON: u32 = WM_APP + 20;
pub const IDM_SETTINGS: usize = 1001;
pub const IDM_RELOAD: usize = 1002;
pub const IDM_EXIT: usize = 1003;
pub const IDM_LAYOUT_BASE: usize = 2000;

pub struct TrayMonitorLayouts {
    pub monitor_label: String,
    pub items: Vec<(usize, String, bool)>,
}

fn to_wide_null(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn create_tray_hicon() -> Option<HICON> {
    const SIZE: i32 = 32;
    let pixels = app_icon_bgra_u32(SIZE as u32);

    unsafe {
        let hdc = CreateCompatibleDC(HDC::default());
        if hdc.0.is_null() {
            return None;
        }
        let mut bmi = BITMAPINFO::default();
        bmi.bmiHeader = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: SIZE,
            biHeight: -SIZE,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        };
        let mut bits: *mut c_void = std::ptr::null_mut();
        let Ok(hbm_color) = CreateDIBSection(hdc, &bmi, DIB_RGB_COLORS, &mut bits, None, 0) else {
            let _ = DeleteDC(hdc);
            return None;
        };
        if !bits.is_null() {
            std::ptr::copy_nonoverlapping(pixels.as_ptr(), bits as *mut u32, pixels.len());
        }
        let hbm_mask: HBITMAP = CreateBitmap(SIZE, SIZE, 1, 1, None);
        let ii = ICONINFO {
            fIcon: windows::Win32::Foundation::BOOL(1),
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: hbm_mask,
            hbmColor: hbm_color,
        };
        let hicon = CreateIconIndirect(&ii).ok();
        let _ = DeleteObject(HGDIOBJ(hbm_color.0));
        let _ = DeleteObject(HGDIOBJ(hbm_mask.0));
        let _ = DeleteDC(hdc);
        hicon
    }
}

pub struct TrayIcon {
    nid: NOTIFYICONDATAW,
    hicon: HICON,
}

impl TrayIcon {
    pub fn new(hwnd: HWND, callback_msg: u32) -> Option<Self> {
        let hicon = create_tray_hicon()?;
        let mut nid = NOTIFYICONDATAW::default();
        nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = hwnd;
        nid.uID = 1;
        nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
        nid.uCallbackMessage = callback_msg;
        nid.hIcon = hicon;

        let tip: Vec<u16> = "ZoneSnap \u{2014} Portable Window Manager"
            .encode_utf16()
            .collect();
        let copy_len = tip.len().min(nid.szTip.len() - 1);
        nid.szTip[..copy_len].copy_from_slice(&tip[..copy_len]);

        let ok = unsafe { Shell_NotifyIconW(NIM_ADD, &nid) };
        if !ok.as_bool() {
            unsafe {
                let _ = DestroyIcon(hicon);
            }
            return None;
        }
        Some(Self { nid, hicon })
    }

    pub fn re_add(&self) {
        unsafe {
            let _ = Shell_NotifyIconW(NIM_ADD, &self.nid);
        }
    }

    unsafe fn populate_monitor_submenu(sub: HMENU, items: &[(usize, String, bool)]) {
        for (cmd_id, name, active) in items {
            let wname = to_wide_null(name);
            let flags = MF_STRING | if *active { MF_CHECKED } else { MF_UNCHECKED };
            let _ = AppendMenuW(sub, flags, *cmd_id, PCWSTR(wname.as_ptr()));
        }
    }

    pub fn show_context_menu(hwnd: HWND, monitor_menus: &[TrayMonitorLayouts]) {
        unsafe {
            let Ok(menu) = CreatePopupMenu() else {
                return;
            };

            if monitor_menus.len() == 1 {
                if let Ok(sub) = CreatePopupMenu() {
                    Self::populate_monitor_submenu(sub, &monitor_menus[0].items);
                    let _ = AppendMenuW(
                        menu,
                        MF_POPUP,
                        sub.0 as usize,
                        w!("Switch Layout"),
                    );
                    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
                }
            } else if monitor_menus.len() > 1 {
                if let Ok(layouts_root) = CreatePopupMenu() {
                    for mon in monitor_menus {
                        if let Ok(mon_sub) = CreatePopupMenu() {
                            Self::populate_monitor_submenu(mon_sub, &mon.items);
                            let wlabel = to_wide_null(&mon.monitor_label);
                            let _ = AppendMenuW(
                                layouts_root,
                                MF_POPUP,
                                mon_sub.0 as usize,
                                PCWSTR(wlabel.as_ptr()),
                            );
                        }
                    }
                    let _ = AppendMenuW(
                        menu,
                        MF_POPUP,
                        layouts_root.0 as usize,
                        w!("Switch Layout"),
                    );
                    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
                }
            }

            let _ = AppendMenuW(
                menu,
                MF_STRING,
                IDM_SETTINGS,
                w!("Configure Zones & Settings"),
            );
            let _ = AppendMenuW(menu, MF_STRING, IDM_RELOAD, w!("Reload zones.json"));
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
            let _ = AppendMenuW(menu, MF_STRING, IDM_EXIT, w!("Exit"));

            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            let _ = SetForegroundWindow(hwnd);
            let _ = TrackPopupMenu(
                menu,
                TPM_RIGHTBUTTON | TPM_BOTTOMALIGN | TPM_LEFTALIGN,
                pt.x,
                pt.y,
                0,
                hwnd,
                None,
            );
            let _ = PostMessageW(hwnd, WM_NULL, WPARAM(0), LPARAM(0));
            let _ = DestroyMenu(menu);
        }
    }
}

impl Drop for TrayIcon {
    fn drop(&mut self) {
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &self.nid);
            if !self.hicon.0.is_null() {
                let _ = DestroyIcon(self.hicon);
            }
        }
    }
}
