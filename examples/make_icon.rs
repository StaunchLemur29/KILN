//! Draws KILN's app icon: a dark rack-metal tile holding a small glowing CRT with the K on it.
//! `cargo run --example make_icon` writes assets/icon-1024.png and assets/icon-256.png.

use ab_glyph::{Font, FontRef, ScaleFont};
use image::{Rgba, RgbaImage, imageops::FilterType};

const S: usize = 1024;

/// Signed distance to a rounded rectangle centred at (cx, cy).
fn rounded(x: f32, y: f32, cx: f32, cy: f32, hw: f32, hh: f32, r: f32) -> f32 {
    let qx = (x - cx).abs() - (hw - r);
    let qy = (y - cy).abs() - (hh - r);
    let ox = qx.max(0.0);
    let oy = qy.max(0.0);
    (ox * ox + oy * oy).sqrt() + qx.max(qy).min(0.0) - r
}

fn cover(d: f32) -> f32 {
    (0.5 - d).clamp(0.0, 1.0)
}

fn mix(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
}

fn hex(c: u32) -> [f32; 3] {
    [((c >> 16) & 255) as f32 / 255.0, ((c >> 8) & 255) as f32 / 255.0, (c & 255) as f32 / 255.0]
}

/// Coverage of the K glyph, centred on (cx, cy), in a S×S buffer.
fn glyph_mask(font: &FontRef, ch: char, px: f32, cx: f32, cy: f32) -> Vec<f32> {
    let mut mask = vec![0.0f32; S * S];
    let scaled = font.as_scaled(px);
    let id = font.glyph_id(ch);
    let g = id.with_scale_and_position(px, ab_glyph::point(0.0, 0.0));
    if let Some(o) = font.outline_glyph(g) {
        let b = o.px_bounds();
        let dx = cx - (b.min.x + b.max.x) / 2.0;
        let dy = cy - (b.min.y + b.max.y) / 2.0;
        let _ = scaled.ascent();
        o.draw(|x, y, c| {
            let gx = (x as f32 + b.min.x + dx) as i32;
            let gy = (y as f32 + b.min.y + dy) as i32;
            if gx >= 0 && gy >= 0 && (gx as usize) < S && (gy as usize) < S {
                mask[gy as usize * S + gx as usize] = c;
            }
        });
    }
    mask
}

fn blur(src: &[f32], radius: i32) -> Vec<f32> {
    let mut tmp = vec![0.0f32; S * S];
    let mut out = vec![0.0f32; S * S];
    let n = (2 * radius + 1) as f32;
    for y in 0..S {
        for x in 0..S {
            let mut s = 0.0;
            for k in -radius..=radius {
                let xx = (x as i32 + k).clamp(0, S as i32 - 1) as usize;
                s += src[y * S + xx];
            }
            tmp[y * S + x] = s / n;
        }
    }
    for y in 0..S {
        for x in 0..S {
            let mut s = 0.0;
            for k in -radius..=radius {
                let yy = (y as i32 + k).clamp(0, S as i32 - 1) as usize;
                s += tmp[yy * S + x];
            }
            out[y * S + x] = s / n;
        }
    }
    out
}

fn main() {
    let font = FontRef::try_from_slice(include_bytes!("../assets/fonts/SpecialGothicExpandedOne-Regular.ttf")).expect("font");
    let c = S as f32 / 2.0;
    // the K, with its red and blue beams a little out of register, as on a real tube
    let k_px = 470.0;
    let k_cy = c - 6.0;
    let k_g = glyph_mask(&font, 'K', k_px, c, k_cy);
    let k_r = glyph_mask(&font, 'K', k_px, c - 9.0, k_cy);
    let k_b = glyph_mask(&font, 'K', k_px, c + 9.0, k_cy);
    let k_glow = blur(&k_g, 22);

    let mut img = RgbaImage::new(S as u32, S as u32);
    let tile_hw = 412.0;
    let tile_r = 185.0;
    let screen_hw = 300.0;
    let screen_hh = 262.0;
    let screen_cy = c - 18.0;
    for y in 0..S {
        for x in 0..S {
            let fx = x as f32 + 0.5;
            let fy = y as f32 + 0.5;
            let i = y * S + x;

            // soft shadow under the tile
            let ds = rounded(fx, fy - 14.0, c, c, tile_hw, tile_hw, tile_r);
            let shadow = (1.0 - (ds / 40.0).clamp(0.0, 1.0)).powi(2) * 0.45;

            let dt = rounded(fx, fy, c, c, tile_hw, tile_hw, tile_r);
            let tile_a = cover(dt);
            // brushed dark metal, lighter at the top, with a bevel line along the edge
            let t = (fy - (c - tile_hw)) / (2.0 * tile_hw);
            let mut col = mix(hex(0x262b31), hex(0x0c0e10), t.clamp(0.0, 1.0));
            let grain = ((fy * 0.9).sin() * 0.5 + 0.5) * 0.012;
            col = [col[0] + grain, col[1] + grain, col[2] + grain];
            let bevel = (1.0 - ((-dt - 3.0).abs() / 3.0).clamp(0.0, 1.0)) * (0.06 + 0.16 * (1.0 - t).clamp(0.0, 1.0));
            col = mix(col, [1.0, 1.0, 1.0], bevel);

            // the screen's black surround
            let dsur = rounded(fx, fy, c, screen_cy, screen_hw + 26.0, screen_hh + 26.0, 92.0);
            col = mix(col, hex(0x050607), cover(dsur));
            let rim = (1.0 - ((dsur + 1.5).abs() / 2.0).clamp(0.0, 1.0)) * 0.25;
            col = mix(col, hex(0x3a4048), rim);

            // the tube face: deep blue-black with a phosphor glow, scanlines and a vignette
            let dscr = rounded(fx, fy, c, screen_cy, screen_hw, screen_hh, 70.0);
            let sa = cover(dscr);
            if sa > 0.0 {
                let u = (fx - c) / screen_hw;
                let v = (fy - screen_cy) / screen_hh;
                let r2 = u * u + v * v;
                let mut face = mix(hex(0x14305a), hex(0x03060c), (r2 * 0.75).clamp(0.0, 1.0));
                // the K and its glow
                let g = k_glow[i] * 1.5;
                face = [face[0] + g * 0.20, face[1] + g * 0.45, face[2] + g * 0.95];
                face = [face[0] + k_r[i] * 0.95, face[1] + k_g[i] * 0.97, face[2] + k_b[i] * 1.0];
                let w = k_g[i].min(k_r[i]).min(k_b[i]);
                face = mix(face, hex(0xf2f5f8), w * 0.85);
                // scanlines
                let line = ((fy / 9.0) * std::f32::consts::TAU).sin() * 0.5 + 0.5;
                let scan = 1.0 - 0.28 * line;
                face = [face[0] * scan, face[1] * scan, face[2] * scan];
                // glass highlight across the top left
                let hl = (1.0 - ((u + 0.35) * (u + 0.35) * 2.0 + (v + 0.62) * (v + 0.62) * 9.0)).clamp(0.0, 1.0) * 0.10;
                face = [face[0] + hl, face[1] + hl, face[2] + hl];
                let vig = 1.0 - 0.45 * r2.powf(1.6).min(1.0);
                face = [face[0] * vig, face[1] * vig, face[2] * vig];
                col = mix(col, face, sa);
            }

            // the amber power lamp below the screen, and two dark knobs
            let lamp_y = c + 340.0;
            let dl = ((fx - (c + 250.0)).powi(2) + (fy - lamp_y).powi(2)).sqrt();
            col = mix(col, hex(0xff5a24), cover(dl - 13.0));
            let lg = (1.0 - (dl / 46.0).clamp(0.0, 1.0)).powi(2) * 0.35;
            col = [col[0] + lg, col[1] + lg * 0.35, col[2] + lg * 0.1];
            for kx in [c - 250.0, c - 175.0] {
                let dk = ((fx - kx).powi(2) + (fy - lamp_y).powi(2)).sqrt();
                col = mix(col, hex(0x1b1f23), cover(dk - 22.0));
                col = mix(col, hex(0x6fa8ff), cover((dk - 22.0).abs() - 1.6) * 0.8);
            }

            let a = tile_a + shadow * (1.0 - tile_a);
            let out = if a > 0.0 { [col[0] * tile_a / a, col[1] * tile_a / a, col[2] * tile_a / a] } else { [0.0; 3] };
            let px = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
            img.put_pixel(x as u32, y as u32, Rgba([px(out[0]), px(out[1]), px(out[2]), px(a)]));
        }
    }
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/assets");
    img.save(format!("{dir}/icon-1024.png")).expect("save 1024");
    image::imageops::resize(&img, 256, 256, FilterType::Lanczos3).save(format!("{dir}/icon-256.png")).expect("save 256");
    println!("wrote {dir}/icon-1024.png and icon-256.png");
}
