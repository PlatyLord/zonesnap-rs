/// Generate the ZoneSnap application icon as `[r, g, b, a]` bytes (RGBA8888).
pub fn app_icon_rgba(size: u32) -> Vec<u8> {
    let bgra = app_icon_bgra_u32(size);
    let mut out = Vec::with_capacity(bgra.len() * 4);
    for px in bgra {
        let a = ((px >> 24) & 0xFF) as u8;
        let r = ((px >> 16) & 0xFF) as u8;
        let g = ((px >> 8) & 0xFF) as u8;
        let b = (px & 0xFF) as u8;
        out.extend_from_slice(&[r, g, b, a]);
    }
    out
}

/// Generate the ZoneSnap application icon as packed `0xAA_RR_GG_BB` `u32` pixels.
pub fn app_icon_bgra_u32(size: u32) -> Vec<u32> {
    let s = size.max(16) as i32;
    let mut pixels = vec![0u32; (s * s) as usize];

    let pad = (s / 16).max(1);
    let inner_min = pad;
    let inner_max = s - pad - 1;
    let corner = (s / 10).max(2);

    // Rounded slate tile (#0F172A) with subtle #334155 border
    for y in inner_min..=inner_max {
        for x in inner_min..=inner_max {
            let dx = if x < inner_min + corner {
                (inner_min + corner) - x
            } else if x > inner_max - corner {
                x - (inner_max - corner)
            } else {
                0
            };
            let dy = if y < inner_min + corner {
                (inner_min + corner) - y
            } else if y > inner_max - corner {
                y - (inner_max - corner)
            } else {
                0
            };
            let dist2 = dx * dx + dy * dy;
            if dist2 > corner * corner {
                continue;
            }
            let is_border = x == inner_min
                || x == inner_max
                || y == inner_min
                || y == inner_max
                || (dx > 0 && dy > 0 && dist2 >= (corner - 1) * (corner - 1));
            pixels[(y * s + x) as usize] = if is_border {
                0xFF33_4155
            } else {
                0xFF0F_172A
            };
        }
    }

    let paint_frac = |buf: &mut [u32], fx0: f32, fy0: f32, fx1: f32, fy1: f32, argb: u32| {
        let x0 = (fx0 * s as f32).round() as i32;
        let y0 = (fy0 * s as f32).round() as i32;
        let x1 = (fx1 * s as f32).round() as i32;
        let y1 = (fy1 * s as f32).round() as i32;
        for y in y0.max(0)..y1.min(s) {
            for x in x0.max(0)..x1.min(s) {
                buf[(y * s + x) as usize] = argb;
            }
        }
    };

    // Left focus pane + two right stacked panes
    paint_frac(&mut pixels, 0.16, 0.19, 0.50, 0.81, 0xFF38_BDF8);
    paint_frac(&mut pixels, 0.56, 0.19, 0.84, 0.47, 0xFF02_84C7);
    paint_frac(&mut pixels, 0.56, 0.53, 0.84, 0.81, 0xFF7D_D3FC);

    pixels
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_dimensions_and_rounded_corners_are_valid() {
        let rgba = app_icon_rgba(64);
        assert_eq!(rgba.len(), 64 * 64 * 4);
        // Top-left corner pixel is transparent
        assert_eq!(rgba[3], 0);
        // Center pixel is opaque
        let mid = (32 * 64 + 32) * 4;
        assert_eq!(rgba[mid + 3], 255);
    }
}
