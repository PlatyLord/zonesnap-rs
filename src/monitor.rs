use serde::{Deserialize, Serialize};
use std::ffi::c_void;
use windows::Win32::Foundation::{BOOL, LPARAM, POINT, RECT};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, MonitorFromPoint, HDC, HMONITOR, MONITORINFOEXW,
    MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};

/// Physical-pixel rectangle matching Win32 RECT semantics (`right` and `bottom` exclusive).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Rect {
    pub fn width(self) -> i32 {
        (self.right - self.left).max(0)
    }

    pub fn height(self) -> i32 {
        (self.bottom - self.top).max(0)
    }

    pub fn contains(self, x: i32, y: i32) -> bool {
        x >= self.left && x < self.right && y >= self.top && y < self.bottom
    }

    pub fn to_win32(self) -> RECT {
        RECT {
            left: self.left,
            top: self.top,
            right: self.right,
            bottom: self.bottom,
        }
    }

    pub fn from_win32(r: RECT) -> Self {
        Self {
            left: r.left,
            top: r.top,
            right: r.right,
            bottom: r.bottom,
        }
    }
}

/// Information about a physical display monitor in PerMonitorV2 physical coordinates.
#[derive(Debug, Clone)]
pub struct MonitorInfo {
    pub hmonitor: isize,
    pub device_name: String,
    pub rc_monitor: Rect,
    pub rc_work: Rect,
    pub is_primary: bool,
    pub dpi_x: u32,
    pub dpi_y: u32,
}

impl MonitorInfo {
    /// Stable profile key combining device name and full monitor physical resolution.
    pub fn profile_key(&self) -> String {
        format!(
            "{}_{}x{}",
            self.device_name,
            self.rc_monitor.width(),
            self.rc_monitor.height()
        )
    }
}

fn query_monitor_info(hmonitor: HMONITOR) -> Option<MonitorInfo> {
    let mut mi = MONITORINFOEXW::default();
    mi.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;

    let ok = unsafe { GetMonitorInfoW(hmonitor, &mut mi as *mut MONITORINFOEXW as *mut _) };
    if !ok.as_bool() {
        return None;
    }

    let len = mi
        .szDevice
        .iter()
        .position(|&c| c == 0)
        .unwrap_or(mi.szDevice.len());
    let device_name = String::from_utf16_lossy(&mi.szDevice[..len]);

    let mut dpi_x = 96u32;
    let mut dpi_y = 96u32;
    unsafe {
        if GetDpiForMonitor(hmonitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y).is_err() {
            dpi_x = 96;
            dpi_y = 96;
        }
    }

    const MONITORINFOF_PRIMARY_FLAG: u32 = 1;
    Some(MonitorInfo {
        hmonitor: hmonitor.0 as isize,
        device_name,
        rc_monitor: Rect::from_win32(mi.monitorInfo.rcMonitor),
        rc_work: Rect::from_win32(mi.monitorInfo.rcWork),
        is_primary: (mi.monitorInfo.dwFlags & MONITORINFOF_PRIMARY_FLAG) != 0,
        dpi_x,
        dpi_y,
    })
}

unsafe extern "system" fn enum_monitor_proc(
    hmonitor: HMONITOR,
    _hdc: HDC,
    _lprect: *mut RECT,
    lparam: LPARAM,
) -> BOOL {
    let monitors = &mut *(lparam.0 as *mut Vec<MonitorInfo>);
    if let Some(info) = query_monitor_info(hmonitor) {
        monitors.push(info);
    }
    BOOL(1)
}

/// Enumerate all active monitors with PerMonitorV2 physical pixel coordinates and work areas.
pub fn enumerate_monitors() -> Vec<MonitorInfo> {
    let mut monitors: Vec<MonitorInfo> = Vec::new();
    let ptr = &mut monitors as *mut Vec<MonitorInfo> as isize;
    unsafe {
        let _ = EnumDisplayMonitors(
            HDC::default(),
            None,
            Some(enum_monitor_proc),
            LPARAM(ptr),
        );
    }
    monitors
}

/// Resolve the monitor containing `(x, y)` (or nearest monitor if out of bounds).
pub fn monitor_from_point(x: i32, y: i32) -> Option<MonitorInfo> {
    let hmonitor = unsafe { MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONEAREST) };
    if hmonitor.0 == std::ptr::null_mut::<c_void>() {
        return None;
    }
    query_monitor_info(hmonitor)
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::UI::HiDpi::{
        SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    };

    #[test]
    fn enumerates_at_least_one_monitor_with_valid_work_area() {
        unsafe {
            let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        }
        let monitors = enumerate_monitors();
        assert!(!monitors.is_empty(), "expected at least one display monitor");
        for m in &monitors {
            assert!(m.rc_monitor.width() > 0);
            assert!(m.rc_monitor.height() > 0);
            assert!(m.rc_work.width() > 0);
            assert!(m.rc_work.height() > 0);
            assert!(!m.device_name.is_empty());
            assert!(m.profile_key().contains('x'));
        }
        let primary = monitors.iter().find(|m| m.is_primary).unwrap_or(&monitors[0]);
        let found = monitor_from_point(primary.rc_work.left + 10, primary.rc_work.top + 10);
        assert!(found.is_some());
    }
}
