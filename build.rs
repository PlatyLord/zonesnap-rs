use std::env;
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;

fn render_icon_bgra(size: u32) -> Vec<u32> {
    let s = size as i32;
    let mut pixels = vec![0u32; (s * s) as usize];

    let pad = (s / 16).max(1);
    let inner_min = pad;
    let inner_max = s - pad - 1;
    let corner = (s / 10).max(2);

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

    paint_frac(&mut pixels, 0.16, 0.19, 0.50, 0.81, 0xFF38_BDF8);
    paint_frac(&mut pixels, 0.56, 0.19, 0.84, 0.47, 0xFF02_84C7);
    paint_frac(&mut pixels, 0.56, 0.53, 0.84, 0.81, 0xFF7D_D3FC);

    pixels
}

fn build_ico(sizes: &[u32]) -> Vec<u8> {
    let mut images: Vec<(u32, Vec<u8>)> = Vec::with_capacity(sizes.len());
    for &sz in sizes {
        let pixels = render_icon_bgra(sz);
        let mask_stride = ((sz + 31) / 32) * 4;
        let xor_bytes = sz * sz * 4;
        let and_bytes = mask_stride * sz;
        let mut img = Vec::with_capacity((40 + xor_bytes + and_bytes) as usize);

        // BITMAPINFOHEADER (40 bytes)
        img.extend_from_slice(&40u32.to_le_bytes());
        img.extend_from_slice(&(sz as i32).to_le_bytes());
        img.extend_from_slice(&((sz * 2) as i32).to_le_bytes());
        img.extend_from_slice(&1u16.to_le_bytes());
        img.extend_from_slice(&32u16.to_le_bytes());
        img.extend_from_slice(&0u32.to_le_bytes());
        img.extend_from_slice(&xor_bytes.to_le_bytes());
        img.extend_from_slice(&0i32.to_le_bytes());
        img.extend_from_slice(&0i32.to_le_bytes());
        img.extend_from_slice(&0u32.to_le_bytes());
        img.extend_from_slice(&0u32.to_le_bytes());

        // Bottom-up 32bpp BGRA XOR pixels
        for y in (0..sz).rev() {
            for x in 0..sz {
                let px = pixels[(y * sz + x) as usize];
                let a = ((px >> 24) & 0xFF) as u8;
                let r = ((px >> 16) & 0xFF) as u8;
                let g = ((px >> 8) & 0xFF) as u8;
                let b = (px & 0xFF) as u8;
                img.extend_from_slice(&[b, g, r, a]);
            }
        }

        // Bottom-up 1bpp AND mask (1 = transparent)
        for y in (0..sz).rev() {
            let mut row = vec![0u8; mask_stride as usize];
            for x in 0..sz {
                let px = pixels[(y * sz + x) as usize];
                let a = (px >> 24) & 0xFF;
                if a == 0 {
                    row[(x / 8) as usize] |= 0x80 >> (x % 8);
                }
            }
            img.extend_from_slice(&row);
        }

        images.push((sz, img));
    }

    let count = images.len() as u16;
    let mut ico = Vec::new();
    ico.extend_from_slice(&0u16.to_le_bytes());
    ico.extend_from_slice(&1u16.to_le_bytes());
    ico.extend_from_slice(&count.to_le_bytes());

    let mut offset = 6u32 + (images.len() as u32) * 16u32;
    for (sz, img) in &images {
        let dim = if *sz >= 256 { 0u8 } else { *sz as u8 };
        ico.push(dim);
        ico.push(dim);
        ico.push(0);
        ico.push(0);
        ico.extend_from_slice(&1u16.to_le_bytes());
        ico.extend_from_slice(&32u16.to_le_bytes());
        ico.extend_from_slice(&(img.len() as u32).to_le_bytes());
        ico.extend_from_slice(&offset.to_le_bytes());
        offset += img.len() as u32;
    }

    for (_, img) in images {
        ico.extend_from_slice(&img);
    }
    ico
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR not set"));
        let ico_path = out_dir.join("zonesnap.ico");
        let ico_bytes = build_ico(&[16, 32, 48, 64]);
        let mut f = File::create(&ico_path).expect("failed to create zonesnap.ico");
        f.write_all(&ico_bytes).expect("failed to write zonesnap.ico");

        let mut res = winresource::WindowsResource::new();
        res.set_icon(ico_path.to_str().expect("valid ico path"));
        res.set("ProductName", "ZoneSnap");
        res.set("FileDescription", "ZoneSnap \u{2014} Portable Windows Window Manager");
        res.compile().expect("failed to compile Windows resources");
    }
}
