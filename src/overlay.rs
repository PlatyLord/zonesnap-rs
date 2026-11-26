use crate::config::{Layout, LayoutMode, OverlayStyle};
use crate::monitor::Rect;
use std::collections::HashSet;
use std::ffi::c_void;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, CreateFontW, DeleteDC, DeleteObject, DrawTextW,
    SelectObject, SetBkMode, SetTextColor, AC_SRC_ALPHA, AC_SRC_OVER, BITMAPINFO,
    BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS,
    DEFAULT_CHARSET, DEFAULT_PITCH, DIB_RGB_COLORS, DT_CENTER, DT_NOPREFIX, DT_SINGLELINE,
    DT_VCENTER, FF_SWISS, FW_SEMIBOLD, HDC, HGDIOBJ, OUT_DEFAULT_PRECIS, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, RegisterClassW, SetWindowPos, ShowWindow,
    UpdateLayeredWindow, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SW_HIDE,
    SW_SHOWNA, ULW_ALPHA, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};

/// Map a screen coordinate `(x, y)` inside `work` to a 0-based grid cell `(col, row)`.
pub fn grid_cell_at(work: Rect, cols: u32, rows: u32, x: i32, y: i32) -> (u32, u32) {
    let cols = cols.max(1);
    let rows = rows.max(1);
    let w = work.width().max(1);
    let h = work.height().max(1);
    let rel_x = (x - work.left).clamp(0, w - 1) as i64;
    let rel_y = (y - work.top).clamp(0, h - 1) as i64;
    let col = ((rel_x * cols as i64) / w as i64).clamp(0, cols as i64 - 1) as u32;
    let row = ((rel_y * rows as i64) / h as i64).clamp(0, rows as i64 - 1) as u32;
    (col, row)
}

/// Compute the physical pixel rectangle spanning from `anchor` cell to `current` cell.
/// Outer edges align flush with `work` using proportional rounding.
pub fn grid_span_rect(
    work: Rect,
    cols: u32,
    rows: u32,
    anchor: (u32, u32),
    current: (u32, u32),
) -> Rect {
    let cols = cols.max(1);
    let rows = rows.max(1);
    let c1 = anchor.0.min(current.0).min(cols - 1);
    let c2 = anchor.0.max(current.0).min(cols - 1);
    let r1 = anchor.1.min(current.1).min(rows - 1);
    let r2 = anchor.1.max(current.1).min(rows - 1);

    let w = work.width() as f64;
    let h = work.height() as f64;

    let left = work.left + ((c1 as f64 * w) / cols as f64).round() as i32;
    let right = work.left + (((c2 + 1) as f64 * w) / cols as f64).round() as i32;
    let top = work.top + ((r1 as f64 * h) / rows as f64).round() as i32;
    let bottom = work.top + (((r2 + 1) as f64 * h) / rows as f64).round() as i32;

    Rect {
        left,
        top,
        right,
        bottom,
    }
}

/// Convert a normalized `[x1, y1, x2, y2]` (`0.0..=1.0`) zone into physical screen coordinates inside `work`.
pub fn custom_zone_pixel_rect(work: Rect, norm: [f32; 4]) -> Rect {
    let x1 = norm[0].min(norm[2]).clamp(0.0, 1.0) as f64;
    let y1 = norm[1].min(norm[3]).clamp(0.0, 1.0) as f64;
    let x2 = norm[0].max(norm[2]).clamp(0.0, 1.0) as f64;
    let y2 = norm[1].max(norm[3]).clamp(0.0, 1.0) as f64;

    let w = work.width() as f64;
    let h = work.height() as f64;

    Rect {
        left: work.left + (x1 * w).round() as i32,
        top: work.top + (y1 * h).round() as i32,
        right: work.left + (x2 * w).round() as i32,
        bottom: work.top + (y2 * h).round() as i32,
    }
}

/// Compute the combined bounding box of a non-empty slice of `Rect`s.
pub fn union_rects(rects: &[Rect]) -> Option<Rect> {
    let first = *rects.first()?;
    let mut acc = first;
    for r in &rects[1..] {
        acc.left = acc.left.min(r.left);
        acc.top = acc.top.min(r.top);
        acc.right = acc.right.max(r.right);
        acc.bottom = acc.bottom.max(r.bottom);
    }
    Some(acc)
}

fn is_near_zone_border(r: Rect, x: i32, y: i32, thresh: i32) -> bool {
    if x < r.left - thresh || x > r.right + thresh || y < r.top - thresh || y > r.bottom + thresh {
        return false;
    }
    let near_v_edge =
        ((x - r.left).abs() <= thresh || (x - r.right).abs() <= thresh) && y >= r.top && y <= r.bottom;
    let near_h_edge =
        ((y - r.top).abs() <= thresh || (y - r.bottom).abs() <= thresh) && x >= r.left && x <= r.right;
    near_v_edge || near_h_edge
}

/// Hit-test normalized custom zones at `(x, y)`.
/// If `(x, y)` sits within `border_threshold_px` of a shared border between 2+ zones, returns all bordering zones.
/// Otherwise returns the smallest-area zone containing `(x, y)`.
pub fn hit_test_custom_zones(
    work: Rect,
    zones: &[[f32; 4]],
    x: i32,
    y: i32,
    border_threshold_px: i32,
) -> Vec<usize> {
    let px_rects: Vec<Rect> = zones
        .iter()
        .map(|z| custom_zone_pixel_rect(work, *z))
        .collect();

    if border_threshold_px > 0 {
        let mut bordering: Vec<usize> = Vec::new();
        for (i, r) in px_rects.iter().enumerate() {
            if is_near_zone_border(*r, x, y, border_threshold_px) {
                bordering.push(i);
            }
        }
        if bordering.len() >= 2 {
            return bordering;
        }
    }

    let mut best: Option<(usize, i64)> = None;
    for (i, r) in px_rects.iter().enumerate() {
        if r.contains(x, y) {
            let area = r.width() as i64 * r.height() as i64;
            if best.map(|(_, a)| area < a).unwrap_or(true) {
                best = Some((i, area));
            }
        }
    }
    match best {
        Some((idx, _)) => vec![idx],
        None => Vec::new(),
    }
}

pub struct OverlayRenderState<'a> {
    pub work: Rect,
    pub layout: &'a Layout,
    pub style: &'a OverlayStyle,
    pub grid_anchor: Option<(u32, u32)>,
    pub grid_current: Option<(u32, u32)>,
    pub active_zones: &'a HashSet<usize>,
    pub target_rect: Option<Rect>,
    pub toast_text: Option<&'a str>,
}

pub struct OverlayWindow {
    hwnd: HWND,
    visible: bool,
    current_work: Rect,
}

unsafe extern "system" fn overlay_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

impl OverlayWindow {
    pub fn new() -> Option<Self> {
        unsafe {
            let hinstance: HINSTANCE = GetModuleHandleW(None).unwrap_or_default().into();
            let class_name = w!("ZoneSnap_OverlayWindow");
            let wc = WNDCLASSW {
                lpfnWndProc: Some(overlay_wnd_proc),
                hInstance: hinstance,
                lpszClassName: class_name,
                ..Default::default()
            };
            RegisterClassW(&wc);

            let ex_style = WS_EX_LAYERED
                | WS_EX_TRANSPARENT
                | WS_EX_TOPMOST
                | WS_EX_TOOLWINDOW
                | WS_EX_NOACTIVATE;

            let hwnd = CreateWindowExW(
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
            )
            .ok()?;

            Some(Self {
                hwnd,
                visible: false,
                current_work: Rect::default(),
            })
        }
    }

    pub fn hwnd_raw(&self) -> isize {
        self.hwnd.0 as isize
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    pub fn show_for_monitor(&mut self, work: Rect) {
        self.current_work = work;
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                HWND_TOPMOST,
                work.left,
                work.top,
                work.width().max(1),
                work.height().max(1),
                SWP_NOACTIVATE,
            );
            let _ = ShowWindow(self.hwnd, SW_SHOWNA);
        }
        self.visible = true;
    }

    pub fn hide(&mut self) {
        if self.visible {
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_HIDE);
            }
            self.visible = false;
        }
    }

    pub fn render(&mut self, state: &OverlayRenderState<'_>) {
        if state.work != self.current_work || !self.visible {
            self.show_for_monitor(state.work);
        }

        let w = state.work.width().max(1);
        let h = state.work.height().max(1);

        unsafe {
            let hdc_screen = CreateCompatibleDC(HDC::default());
            if hdc_screen.0.is_null() {
                return;
            }
            let mut bmi = BITMAPINFO::default();
            bmi.bmiHeader = BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            };

            let mut bits: *mut c_void = std::ptr::null_mut();
            let Ok(hbmp) = CreateDIBSection(hdc_screen, &bmi, DIB_RGB_COLORS, &mut bits, None, 0)
            else {
                let _ = DeleteDC(hdc_screen);
                return;
            };

            if !bits.is_null() {
                let old_bmp = SelectObject(hdc_screen, HGDIOBJ(hbmp.0));
                let buf = std::slice::from_raw_parts_mut(bits as *mut u32, (w * h) as usize);
                paint_overlay_buffer(buf, w, h, state);

                let pt_dst = POINT {
                    x: state.work.left,
                    y: state.work.top,
                };
                let size = SIZE { cx: w, cy: h };
                let pt_src = POINT { x: 0, y: 0 };
                let blend = BLENDFUNCTION {
                    BlendOp: AC_SRC_OVER as u8,
                    BlendFlags: 0,
                    SourceConstantAlpha: 255,
                    AlphaFormat: AC_SRC_ALPHA as u8,
                };

                let _ = SetWindowPos(
                    self.hwnd,
                    HWND_TOPMOST,
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                );
                let _ = UpdateLayeredWindow(
                    self.hwnd,
                    HDC::default(),
                    Some(&pt_dst),
                    Some(&size),
                    hdc_screen,
                    Some(&pt_src),
                    COLORREF(0),
                    Some(&blend),
                    ULW_ALPHA,
                );
                SelectObject(hdc_screen, old_bmp);
            }
            let _ = DeleteObject(HGDIOBJ(hbmp.0));
            let _ = DeleteDC(hdc_screen);
        }
    }
}

impl Drop for OverlayWindow {
    fn drop(&mut self) {
        if !self.hwnd.0.is_null() {
            unsafe {
                let _ = DestroyWindow(self.hwnd);
            }
            self.hwnd = HWND::default();
        }
    }
}

#[inline]
fn premul_bgra(r: u8, g: u8, b: u8, a: u8) -> u32 {
    let a32 = a as u32;
    let pr = ((r as u32 * a32) + 127) / 255;
    let pg = ((g as u32 * a32) + 127) / 255;
    let pb = ((b as u32 * a32) + 127) / 255;
    (a32 << 24) | (pr << 16) | (pg << 8) | pb
}

#[inline]
fn blend_pixel(dst: u32, src_r: u8, src_g: u8, src_b: u8, src_a: u8) -> u32 {
    if src_a == 0 {
        return dst;
    }
    if src_a == 255 {
        return premul_bgra(src_r, src_g, src_b, 255);
    }
    let sa = src_a as u32;
    let inv = 255 - sa;
    let da = (dst >> 24) & 0xFF;
    let dr = (dst >> 16) & 0xFF;
    let dg = (dst >> 8) & 0xFF;
    let db = dst & 0xFF;

    let sr = (src_r as u32 * sa + 127) / 255;
    let sg = (src_g as u32 * sa + 127) / 255;
    let sb = (src_b as u32 * sa + 127) / 255;

    let out_a = (sa + ((da * inv + 127) / 255)).min(255);
    let out_r = (sr + ((dr * inv + 127) / 255)).min(255);
    let out_g = (sg + ((dg * inv + 127) / 255)).min(255);
    let out_b = (sb + ((db * inv + 127) / 255)).min(255);
    (out_a << 24) | (out_r << 16) | (out_g << 8) | out_b
}

fn fill_rect(buf: &mut [u32], bw: i32, bh: i32, rx: i32, ry: i32, rw: i32, rh: i32, r: u8, g: u8, b: u8, a: u8) {
    let x0 = rx.max(0);
    let y0 = ry.max(0);
    let x1 = (rx + rw).min(bw);
    let y1 = (ry + rh).min(bh);
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    for y in y0..y1 {
        let row_off = (y * bw) as usize;
        for x in x0..x1 {
            let idx = row_off + x as usize;
            buf[idx] = blend_pixel(buf[idx], r, g, b, a);
        }
    }
}

fn stroke_rect(
    buf: &mut [u32],
    bw: i32,
    bh: i32,
    rx: i32,
    ry: i32,
    rw: i32,
    rh: i32,
    thickness: i32,
    r: u8,
    g: u8,
    b: u8,
    a: u8,
) {
    let t = thickness.max(1);
    if rw <= 0 || rh <= 0 {
        return;
    }
    fill_rect(buf, bw, bh, rx, ry, rw, t, r, g, b, a);
    fill_rect(buf, bw, bh, rx, ry + rh - t, rw, t, r, g, b, a);
    fill_rect(buf, bw, bh, rx, ry + t, t, (rh - 2 * t).max(0), r, g, b, a);
    fill_rect(buf, bw, bh, rx + rw - t, ry + t, t, (rh - 2 * t).max(0), r, g, b, a);
}

fn fill_rounded_rect(
    buf: &mut [u32],
    bw: i32,
    bh: i32,
    rx: i32,
    ry: i32,
    rw: i32,
    rh: i32,
    radius: i32,
    r: u8,
    g: u8,
    b: u8,
    a: u8,
) {
    let rad = radius.min(rw / 2).min(rh / 2).max(0);
    let x0 = rx.max(0);
    let y0 = ry.max(0);
    let x1 = (rx + rw).min(bw);
    let y1 = (ry + rh).min(bh);
    let r2 = rad * rad;

    for y in y0..y1 {
        let dy = if y < ry + rad {
            (ry + rad) - y
        } else if y >= ry + rh - rad {
            y - (ry + rh - rad - 1)
        } else {
            0
        };
        let row_off = (y * bw) as usize;
        for x in x0..x1 {
            if dy > 0 {
                let dx = if x < rx + rad {
                    (rx + rad) - x
                } else if x >= rx + rw - rad {
                    x - (rx + rw - rad - 1)
                } else {
                    0
                };
                if dx > 0 && dx * dx + dy * dy > r2 {
                    continue;
                }
            }
            let idx = row_off + x as usize;
            buf[idx] = blend_pixel(buf[idx], r, g, b, a);
        }
    }
}

/// Draw crisp anti-aliased white/tinted text onto a 32-bit premultiplied BGRA buffer
/// using a scratch black-background GDI DIB whose luminance becomes alpha.
fn draw_aa_text_on_bgra(
    buf: &mut [u32],
    bw: i32,
    bh: i32,
    rx: i32,
    ry: i32,
    rw: i32,
    rh: i32,
    text: &str,
    font_px: i32,
    color: [u8; 3],
) {
    if rw <= 0 || rh <= 0 || text.is_empty() {
        return;
    }
    unsafe {
        let hdc = CreateCompatibleDC(HDC::default());
        if hdc.0.is_null() {
            return;
        }
        let mut bmi = BITMAPINFO::default();
        bmi.bmiHeader = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: rw,
            biHeight: -rh,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        };
        let mut bits: *mut c_void = std::ptr::null_mut();
        let Ok(hbmp) = CreateDIBSection(hdc, &bmi, DIB_RGB_COLORS, &mut bits, None, 0) else {
            let _ = DeleteDC(hdc);
            return;
        };

        if !bits.is_null() {
            let old_bmp = SelectObject(hdc, HGDIOBJ(hbmp.0));
            let scratch = std::slice::from_raw_parts_mut(bits as *mut u32, (rw * rh) as usize);
            scratch.fill(0);

            let font = CreateFontW(
                -font_px,
                0,
                0,
                0,
                FW_SEMIBOLD.0 as i32,
                0,
                0,
                0,
                DEFAULT_CHARSET.0 as u32,
                OUT_DEFAULT_PRECIS.0 as u32,
                CLIP_DEFAULT_PRECIS.0 as u32,
                CLEARTYPE_QUALITY.0 as u32,
                (DEFAULT_PITCH.0 | FF_SWISS.0) as u32,
                w!("Segoe UI"),
            );
            let old_font = SelectObject(hdc, HGDIOBJ(font.0));
            SetBkMode(hdc, TRANSPARENT);
            SetTextColor(hdc, COLORREF(0x00FF_FFFF));

            let mut wide: Vec<u16> = text.encode_utf16().collect();
            let mut tr = RECT {
                left: 0,
                top: 0,
                right: rw,
                bottom: rh,
            };
            DrawTextW(
                hdc,
                &mut wide,
                &mut tr,
                DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
            );

            for sy in 0..rh {
                let dy = ry + sy;
                if dy < 0 || dy >= bh {
                    continue;
                }
                for sx in 0..rw {
                    let dx = rx + sx;
                    if dx < 0 || dx >= bw {
                        continue;
                    }
                    let px = scratch[(sy * rw + sx) as usize];
                    let r = ((px >> 16) & 0xFF) as u32;
                    let g = ((px >> 8) & 0xFF) as u32;
                    let b = (px & 0xFF) as u32;
                    let lum = ((r * 77 + g * 150 + b * 29) >> 8).min(255) as u8;
                    if lum > 0 {
                        let dst_idx = (dy * bw + dx) as usize;
                        buf[dst_idx] = blend_pixel(buf[dst_idx], color[0], color[1], color[2], lum);
                    }
                }
            }

            SelectObject(hdc, old_font);
            let _ = DeleteObject(HGDIOBJ(font.0));
            SelectObject(hdc, old_bmp);
        }
        let _ = DeleteObject(HGDIOBJ(hbmp.0));
        let _ = DeleteDC(hdc);
    }
}

fn paint_overlay_buffer(buf: &mut [u32], w: i32, h: i32, state: &OverlayRenderState<'_>) {
    // Translucent dark-glass backdrop
    buf.fill(premul_bgra(12, 16, 24, 96));

    let hi = state.style.highlight_color;
    let hi_a = (state.style.highlight_opacity.clamp(0.05, 0.95) * 255.0).round() as u8;
    let bd = state.style.border_color;
    let bg_a = (state.style.zone_bg_opacity.clamp(0.0, 0.8) * 255.0).round() as u8;

    match &state.layout.mode {
        LayoutMode::DynamicGrid { cols, rows } => {
            let cols = (*cols).max(1);
            let rows = (*rows).max(1);
            for c in 1..cols {
                let gx = ((c as f64 * w as f64) / cols as f64).round() as i32;
                fill_rect(buf, w, h, gx, 0, 1, h, 255, 255, 255, 42);
            }
            for r in 1..rows {
                let gy = ((r as f64 * h as f64) / rows as f64).round() as i32;
                fill_rect(buf, w, h, 0, gy, w, 1, 255, 255, 255, 42);
            }
            stroke_rect(buf, w, h, 0, 0, w, h, 1, 255, 255, 255, 55);

            if let Some(target) = state.target_rect {
                let rx = target.left - state.work.left;
                let ry = target.top - state.work.top;
                let rw = target.width();
                let rh = target.height();
                fill_rect(buf, w, h, rx, ry, rw, rh, hi[0], hi[1], hi[2], hi_a);
                stroke_rect(buf, w, h, rx, ry, rw, rh, 2, bd[0], bd[1], bd[2], 235);

                if let (Some(a), Some(c)) = (state.grid_anchor, state.grid_current) {
                    let span_cols = a.0.abs_diff(c.0) + 1;
                    let span_rows = a.1.abs_diff(c.1) + 1;
                    let label = format!("{span_cols} \u{00D7} {span_rows}");
                    let pill_w = 104.min(rw.max(40));
                    let pill_h = 34.min(rh.max(24));
                    let px = rx + (rw - pill_w) / 2;
                    let py = ry + (rh - pill_h) / 2;
                    fill_rounded_rect(buf, w, h, px, py, pill_w, pill_h, 8, 15, 23, 42, 210);
                    draw_aa_text_on_bgra(
                        buf,
                        w,
                        h,
                        px,
                        py,
                        pill_w,
                        pill_h,
                        &label,
                        18,
                        [248, 250, 252],
                    );
                }
            }
        }
        LayoutMode::CustomZones { zones } => {
            const GAP: i32 = 4;
            for (idx, z) in zones.iter().enumerate() {
                let pxr = custom_zone_pixel_rect(state.work, *z);
                let rx = pxr.left - state.work.left + GAP;
                let ry = pxr.top - state.work.top + GAP;
                let rw = (pxr.width() - GAP * 2).max(4);
                let rh = (pxr.height() - GAP * 2).max(4);

                let active = state.active_zones.contains(&idx);
                if active {
                    fill_rect(buf, w, h, rx, ry, rw, rh, hi[0], hi[1], hi[2], hi_a);
                    stroke_rect(buf, w, h, rx, ry, rw, rh, 2, bd[0], bd[1], bd[2], 220);
                } else {
                    fill_rect(buf, w, h, rx, ry, rw, rh, 255, 255, 255, bg_a);
                    stroke_rect(buf, w, h, rx, ry, rw, rh, 1, 226, 232, 240, 110);
                }

                let num = format!("{}", idx + 1);
                let badge_w = 48.min(rw);
                let badge_h = 36.min(rh);
                draw_aa_text_on_bgra(
                    buf,
                    w,
                    h,
                    rx + (rw - badge_w) / 2,
                    ry + (rh - badge_h) / 2,
                    badge_w,
                    badge_h,
                    &num,
                    22,
                    if active {
                        [255, 255, 255]
                    } else {
                        [203, 213, 225]
                    },
                );
            }

            if let Some(target) = state.target_rect {
                let rx = target.left - state.work.left;
                let ry = target.top - state.work.top;
                let rw = target.width();
                let rh = target.height();
                if state.active_zones.len() > 1 {
                    fill_rect(buf, w, h, rx, ry, rw, rh, hi[0], hi[1], hi[2], (hi_a as u16 * 3 / 4) as u8);
                }
                stroke_rect(buf, w, h, rx, ry, rw, rh, 3, bd[0], bd[1], bd[2], 245);
            }
        }
    }

    if let Some(toast) = state.toast_text {
        let pill_w = ((toast.chars().count() as i32) * 11 + 48).clamp(160, (w - 40).max(160));
        let pill_h = 40;
        let px = (w - pill_w) / 2;
        let py = 22;
        fill_rounded_rect(buf, w, h, px, py, pill_w, pill_h, 10, 15, 23, 42, 235);
        stroke_rect(buf, w, h, px + 2, py + 2, pill_w - 4, pill_h - 4, 1, bd[0], bd[1], bd[2], 170);
        draw_aa_text_on_bgra(
            buf,
            w,
            h,
            px,
            py,
            pill_w,
            pill_h,
            toast,
            17,
            [248, 250, 252],
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_cell_and_span_align_flush_with_work_area() {
        let work = Rect {
            left: 1920,
            top: 0,
            right: 3840,
            bottom: 1040,
        };
        assert_eq!(grid_cell_at(work, 12, 6, 1920, 0), (0, 0));
        assert_eq!(grid_cell_at(work, 12, 6, 3839, 1039), (11, 5));

        let full = grid_span_rect(work, 12, 6, (0, 0), (11, 5));
        assert_eq!(full, work);

        let left_half = grid_span_rect(work, 12, 6, (5, 5), (0, 0));
        assert_eq!(left_half.left, 1920);
        assert_eq!(left_half.right, 2880);
        assert_eq!(left_half.top, 0);
        assert_eq!(left_half.bottom, 1040);
    }

    #[test]
    fn custom_zones_shared_border_and_union_merge() {
        let work = Rect {
            left: 0,
            top: 0,
            right: 1000,
            bottom: 1000,
        };
        let zones = vec![[0.0, 0.0, 0.5, 1.0], [0.5, 0.0, 1.0, 1.0]];

        // Center of zone 0 -> only zone 0
        assert_eq!(hit_test_custom_zones(work, &zones, 250, 500, 16), vec![0]);
        // Center of zone 1 -> only zone 1
        assert_eq!(hit_test_custom_zones(work, &zones, 750, 500, 16), vec![1]);
        // Near shared border x=500 -> merges both zones
        assert_eq!(
            hit_test_custom_zones(work, &zones, 495, 500, 16),
            vec![0, 1]
        );

        let r0 = custom_zone_pixel_rect(work, zones[0]);
        let r1 = custom_zone_pixel_rect(work, zones[1]);
        assert_eq!(union_rects(&[r0, r1]), Some(work));
    }
}
