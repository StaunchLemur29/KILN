//! The analog video chain: composite encode/decode, tape damage, wave bands, snow,
//! feedback, then the picture tube (geometry, convergence, phosphor mask, glow).
//! Rules and constants come from MECHANICS.md.

use crate::params::{Params, Tier};
use rayon::prelude::*;
use std::f32::consts::TAU;

#[derive(Clone)]
pub struct Img {
    pub w: usize,
    pub h: usize,
    pub px: Vec<[f32; 3]>,
}

impl Img {
    pub fn new(w: usize, h: usize) -> Img {
        Img { w, h, px: vec![[0.0; 3]; w * h] }
    }

    pub fn from_rgb8(w: usize, h: usize, data: &[u8]) -> Img {
        let px = data.chunks_exact(3).map(|c| [c[0] as f32 / 255.0, c[1] as f32 / 255.0, c[2] as f32 / 255.0]).collect();
        Img { w, h, px }
    }

    pub fn to_rgb8(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.w * self.h * 3);
        for p in &self.px {
            for c in p {
                out.push((c.clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
            }
        }
        out
    }

    pub fn to_rgba8(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.w * self.h * 4);
        for p in &self.px {
            for c in p {
                out.push((c.clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
            }
            out.push(255);
        }
        out
    }

    /// Bilinear sample in pixel coordinates (centre of pixel 0 at 0.5), clamped at the edges.
    #[inline]
    pub fn sample(&self, x: f32, y: f32) -> [f32; 3] {
        let fx = (x - 0.5).clamp(0.0, (self.w - 1) as f32);
        let fy = (y - 0.5).clamp(0.0, (self.h - 1) as f32);
        let x0 = fx as usize;
        let y0 = fy as usize;
        let x1 = (x0 + 1).min(self.w - 1);
        let y1 = (y0 + 1).min(self.h - 1);
        let tx = fx - x0 as f32;
        let ty = fy - y0 as f32;
        let a = self.px[y0 * self.w + x0];
        let b = self.px[y0 * self.w + x1];
        let c = self.px[y1 * self.w + x0];
        let d = self.px[y1 * self.w + x1];
        let mut o = [0.0; 3];
        for i in 0..3 {
            let top = a[i] + (b[i] - a[i]) * tx;
            let bot = c[i] + (d[i] - c[i]) * tx;
            o[i] = top + (bot - top) * ty;
        }
        o
    }

    #[inline]
    pub fn sample_uv(&self, u: f32, v: f32) -> [f32; 3] {
        self.sample(u * self.w as f32, v * self.h as f32)
    }

    /// Resize with area averaging (down) or bilinear (up).
    pub fn resized(&self, w: usize, h: usize) -> Img {
        if w == self.w && h == self.h {
            return self.clone();
        }
        let mut out = Img::new(w, h);
        let sx = self.w as f32 / w as f32;
        let sy = self.h as f32 / h as f32;
        out.px.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
            for (x, p) in row.iter_mut().enumerate() {
                if sx <= 1.0 && sy <= 1.0 {
                    *p = self.sample((x as f32 + 0.5) * sx, (y as f32 + 0.5) * sy);
                } else {
                    let x0 = (x as f32 * sx) as usize;
                    let x1 = (((x + 1) as f32 * sx).ceil() as usize).clamp(x0 + 1, self.w);
                    let y0 = (y as f32 * sy) as usize;
                    let y1 = (((y + 1) as f32 * sy).ceil() as usize).clamp(y0 + 1, self.h);
                    let mut acc = [0.0; 3];
                    for yy in y0..y1 {
                        for xx in x0..x1 {
                            let q = self.px[yy * self.w + xx];
                            acc[0] += q[0];
                            acc[1] += q[1];
                            acc[2] += q[2];
                        }
                    }
                    let n = ((x1 - x0) * (y1 - y0)) as f32;
                    *p = [acc[0] / n, acc[1] / n, acc[2] / n];
                }
            }
        });
        out
    }
}

/// The size the analog signal is processed at for a given quality tier.
pub fn signal_size(tier: Tier, src_w: usize, src_h: usize, full_cap: usize) -> (usize, usize) {
    let aspect = src_w as f32 / src_h.max(1) as f32;
    match tier {
        // 480 active NTSC lines; SD = broadcast raster, HI = double horizontal samples.
        Tier::Sd => (((480.0 * aspect).round() as usize).max(2), 480),
        Tier::Hi => (((960.0 * aspect).round() as usize).max(2), 480),
        Tier::Full => {
            let s = (full_cap as f32 / src_w.max(src_h) as f32).min(1.0);
            (((src_w as f32 * s).round() as usize).max(2), ((src_h as f32 * s).round() as usize).max(2))
        }
    }
}

// ---------- noise helpers (integer hash, as in the original's PCG-style hash) ----------

#[inline]
pub(crate) fn hash(x: u32, y: u32, n: u32, seed: u32) -> f32 {
    let mut h = x.wrapping_mul(0x9E37_79B1) ^ y.wrapping_mul(0x85EB_CA77) ^ n.wrapping_mul(0xC2B2_AE3D) ^ seed.wrapping_mul(0x27D4_EB2F);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2C1B_3C6D);
    h ^= h >> 12;
    h = h.wrapping_mul(0x297A_2D39);
    h ^= h >> 15;
    (h & 0x00FF_FFFF) as f32 / 16_777_216.0
}



#[inline]
fn smoothstep(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[inline]
fn mix(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

#[inline]
fn mix3(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [mix(a[0], b[0], t), mix(a[1], b[1], t), mix(a[2], b[2], t)]
}

#[inline]
fn rgb2yiq(c: [f32; 3]) -> [f32; 3] {
    [
        0.299 * c[0] + 0.587 * c[1] + 0.114 * c[2],
        0.595716 * c[0] - 0.274453 * c[1] - 0.321263 * c[2],
        0.211456 * c[0] - 0.522591 * c[1] + 0.311135 * c[2],
    ]
}

#[inline]
fn yiq2rgb(c: [f32; 3]) -> [f32; 3] {
    [
        c[0] + 0.9563 * c[1] + 0.6210 * c[2],
        c[0] - 0.2721 * c[1] - 0.6474 * c[2],
        c[0] - 1.1070 * c[1] + 1.7046 * c[2],
    ]
}

#[inline]
fn luma(c: [f32; 3]) -> f32 {
    0.299 * c[0] + 0.587 * c[1] + 0.114 * c[2]
}

fn hue_rotate(c: [f32; 3], a: f32) -> [f32; 3] {
    let y = rgb2yiq(c);
    let (s, co) = a.sin_cos();
    yiq2rgb([y[0], y[1] * co - y[2] * s, y[1] * s + y[2] * co])
}

fn hue2rgb(h: f32) -> [f32; 3] {
    let h = h - h.floor();
    [
        ((h * 6.0 - 3.0).abs() - 1.0).clamp(0.0, 1.0),
        (2.0 - (h * 6.0 - 2.0).abs()).clamp(0.0, 1.0),
        (2.0 - (h * 6.0 - 4.0).abs()).clamp(0.0, 1.0),
    ]
}



// ---------- renderer ----------

const SEED: u32 = 0x5EED_1234;

pub struct Renderer {
    prev: Option<Img>,
    rx: crate::rx::RxState,
}

pub struct FrameTime {
    /// seconds into the clip (or since the preview started for a still)
    pub t: f32,
    /// frame number at the clip's rate
    pub frame: u32,
    /// the clip's frame rate
    pub fps: f32,
}

impl Renderer {
    pub fn new() -> Renderer {
        Renderer { prev: None, rx: crate::rx::RxState::default() }
    }

    pub fn reset(&mut self) {
        self.prev = None;
        self.rx = crate::rx::RxState::default();
    }

    /// `src` is the clean picture already at the signal size. Returns the finished picture at
    /// `out_w` x `out_h`. `preview_scale` = output pixels / full-resolution pixels.
    pub fn render(&mut self, src: &Img, p: &Params, ft: &FrameTime, out_w: usize, out_h: usize, preview_scale: f32) -> Img {
        let mixed = p.intelligent_mix();
        let p = &mixed;
        let glowed;
        let src = if p.get("postGlow") > 0.0 || p.get("halation") > 0.0 {
            glowed = signal_glow(src, p);
            &glowed
        } else {
            src
        };
        let mut sig = crate::rx::receive(src, p, ft, &mut self.rx);
        if p.get("modMix") > 0.0 {
            sig = waves(&sig, &p, ft);
        }
        sig = self.feedback(sig, &p);
        display(&sig, &p, ft, out_w, out_h, preview_scale)
    }

    fn feedback(&mut self, cur: Img, p: &Params) -> Img {
        let k = p.get("feedbackMix").clamp(0.0, 1.0);
        let prev = match self.prev.take() {
            Some(prev) if k >= 0.02 && prev.w == cur.w && prev.h == cur.h => prev,
            _ => {
                if k >= 0.02 {
                    self.prev = Some(cur.clone());
                }
                return cur;
            }
        };
        let (w, h) = (cur.w, cur.h);
        let aspect = w as f32 / h as f32;
        let (sa, ca) = p.get("fbRotate").sin_cos();
        let zoom = p.get("fbZoom").max(1e-3);
        let hue = p.get("fbHue") * TAU;
        // fbGain = 1, fbDecay = min(fbTrails, 0.99)
        let gain = p.get("fbTrails").min(0.99) * mix(0.75, 1.35, k);
        let in_k = mix(1.0, 0.15, smoothstep(0.40, 1.0, k));
        let sat = 1.0 + smoothstep(0.5, 1.0, k) * 1.6;
        let mut out = Img::new(w, h);
        out.px.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
            for (x, o) in row.iter_mut().enumerate() {
                let mut cx = ((x as f32 + 0.5) / w as f32 - 0.5) * aspect;
                let mut cy = (y as f32 + 0.5) / h as f32 - 0.5;
                let rx = cx * ca - cy * sa;
                let ry = cx * sa + cy * ca;
                cx = rx / zoom / aspect + 0.5;
                cy = ry / zoom + 0.5;
                let pv = hue_rotate(prev.sample_uv(cx, cy), hue);
                let c = cur.px[y * w + x];
                let mut m = [0.0; 3];
                for i in 0..3 {
                    m[i] = (c[i] * in_k).max(pv[i] * gain);
                }
                let mut yq = rgb2yiq(m);
                yq[1] *= sat;
                yq[2] *= sat;
                let r = yiq2rgb(yq);
                *o = [r[0].clamp(0.0, 1.0), r[1].clamp(0.0, 1.0), r[2].clamp(0.0, 1.0)];
            }
        });
        self.prev = Some(out.clone());
        out
    }
}

/// Signal-domain highlight bloom and halation, added to the source before it is encoded
///: highlights above a linear knee of
/// 0.22..0.95 are smeared mostly to the right over mix(0.0022, 0.011, amount) of the width,
/// compressed, then added at 0.55 x amount; halation is a red-weighted wide halo
/// (1.30, 0.62, 0.40) x 0.85 x amount.
fn signal_glow(src: &Img, p: &Params) -> Img {
    let (w, h) = (src.w, src.h);
    let amount = p.get("postGlow").clamp(0.0, 1.0);
    let hal = p.get("halation").clamp(0.0, 1.0);
    let lin = |c: [f32; 3]| [c[0].max(0.0).powf(2.2), c[1].max(0.0).powf(2.2), c[2].max(0.0).powf(2.2)];
    let mut halo = Img::new(w, h);
    halo.px.par_iter_mut().zip(src.px.par_iter()).for_each(|(o, c)| {
        let l = lin(*c);
        let k = smoothstep(0.22, 0.95, l[0].max(l[1]).max(l[2]));
        *o = [l[0] * k, l[1] * k, l[2] * k];
    });
    let spread = (mix(0.0022, 0.011, amount) * w as f32).max(1.0);
    let wide = if hal > 0.0 {
        let small = halo.resized((w / 4).max(2), (h / 4).max(2));
        Some((blur(&small, 2, 2), blur(&small, 5, 2), blur(&small, 9, 2)))
    } else {
        None
    };
    let mut out = Img::new(w, h);
    out.px.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (x, o) in row.iter_mut().enumerate() {
            let base = lin(src.px[y * w + x]);
            let mut add = [0.0f32; 3];
            if amount > 0.0 {
                let (mut acc, mut wsum) = ([0.0f32; 3], 0.0f32);
                for i in 1..=20 {
                    let d = i as f32 / 20.0;
                    let px = d * spread;
                    let wt = (-d * 2.8).exp();
                    let r = halo.sample(x as f32 + 0.5 + px, y as f32 + 0.5);
                    let l = halo.sample(x as f32 + 0.5 - px, y as f32 + 0.5);
                    for c in 0..3 {
                        acc[c] += r[c] * wt + l[c] * wt * 0.32;
                    }
                    wsum += wt * 1.32;
                }
                for c in 0..3 {
                    let b = acc[c] / wsum.max(1e-5);
                    let b = b / (1.0 + b * 1.6);
                    let n = (b * 2.55).clamp(0.0, 1.0);
                    add[c] += n * n * (3.0 - 2.0 * n) * amount * 0.55;
                }
            }
            if let Some((a, b, c3)) = &wide {
                let (sx, sy) = (a.w as f32 / w as f32, a.h as f32 / h as f32);
                let (bx, by) = ((x as f32 + 0.5) * sx, (y as f32 + 0.5) * sy);
                let (na, nb, nc) = (a.sample(bx, by), b.sample(bx, by), c3.sample(bx, by));
                // red returns from furthest out
                let ch = [nc[0] * 0.5 + nb[0] * 0.3 + na[0] * 0.2, nb[1] * 0.5 + na[1] * 0.5, na[2]];
                let tint = [1.3, 0.62, 0.4];
                for c in 0..3 {
                    add[c] += ch[c] * tint[c] * hal * 0.85;
                }
            }
            *o = [
                (base[0] + add[0]).powf(1.0 / 2.2).clamp(0.0, 1.0),
                (base[1] + add[1]).powf(1.0 / 2.2).clamp(0.0, 1.0),
                (base[2] + add[2]).powf(1.0 / 2.2).clamp(0.0, 1.0),
            ];
        }
    });
    out
}

/// "Waves": each scan line drives an oscillator whose pitch follows the picture brightness,
/// is AM-modulated, lowpassed and resynthesised; the result is blended over the picture.
fn waves(orig: &Img, p: &Params, ft: &FrameTime) -> Img {
    let (w, h) = (orig.w, orig.h);
    let horiz = p.get("modOrient") < 0.5;
    let backward = p.get("modDir") > 0.5;
    let rgb_mode = p.get("modColor") > 0.5;
    let line_len = if horiz { h } else { w };
    let lines = if horiz { w } else { h };
    // modOmega is fixed at 0.95 by the original
    let base = p.get("modWaves") * 0.95 / line_len as f32;
    let react = p.get("modReact");
    let alpha = 1.0 / (1.0 + p.get("modLowpass").max(1.0));
    let shift = ft.t * 0.04;
    let k = p.get("modMix").clamp(0.0, 1.0);
    let mode = p.get("modBlend").round() as i32;

    let modded: Vec<Vec<[f32; 3]>> = (0..lines)
        .into_par_iter()
        .map(|li| {
            let mut ph = [shift, shift + 0.23, shift + 0.47];
            let mut lp = [0.0f32; 3];
            let mut out = vec![[0.0f32; 3]; line_len];
            for step in 0..line_len {
                let pos = if backward { line_len - 1 - step } else { step };
                let (x, y) = if horiz { (li, pos) } else { (pos, li) };
                let s = orig.px[y * w + x];
                let vals = if rgb_mode { s } else { [luma(s); 3] };
                let mut o = [0.0; 3];
                let chans = if rgb_mode { 3 } else { 1 };
                for c in 0..chans {
                    ph[c] += base + react * vals[c];
                    let sig = vals[c] * (0.5 + 0.5 * (TAU * ph[c]).sin());
                    lp[c] = alpha * sig + (1.0 - alpha) * lp[c];
                    o[c] = (lp[c] * 2.0 + (sig - lp[c])).clamp(0.0, 1.0);
                }
                if !rgb_mode {
                    o = [o[0]; 3];
                }
                out[pos] = o;
            }
            out
        })
        .collect();

    let mut out = Img::new(w, h);
    out.px.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (x, o) in row.iter_mut().enumerate() {
            let a = orig.px[y * w + x];
            let m = if horiz { modded[x][y] } else { modded[y][x] };
            let b = match mode {
                1 => {
                    let mut r = [0.0; 3];
                    for i in 0..3 {
                        r[i] = ((m[i] - a[i]).max(0.0) * 3.5).clamp(0.0, 1.0).powf(0.7);
                    }
                    r
                }
                2 => [1.0 - (1.0 - a[0]) * (1.0 - m[0]), 1.0 - (1.0 - a[1]) * (1.0 - m[1]), 1.0 - (1.0 - a[2]) * (1.0 - m[2])],
                3 => [(a[0] + m[0]).min(1.0), (a[1] + m[1]).min(1.0), (a[2] + m[2]).min(1.0)],
                _ => m,
            };
            *o = mix3(a, b, k);
        }
    });
    out
}

// ---------- picture tube ----------

const BEAM_SIGMA_BASE: f32 = 0.95;
const BEAM_SIGMA_SOFTEN: f32 = 0.85;
const MASK_PITCH_MAX: [f32; 5] = [12.0, 24.0, 11.0, 24.0, 8.0];
const MASK_PITCH_SPAN: f32 = 4.0;

fn mask_pitch(dtype: usize, scale: f32, preview_scale: f32) -> f32 {
    let hi = MASK_PITCH_MAX[dtype];
    let lo = hi / MASK_PITCH_SPAN;
    let authored = (lo * MASK_PITCH_SPAN.powf(scale.clamp(0.0, 1.0)) + 0.5).floor().clamp(lo, hi);
    (authored * preview_scale.clamp(1.0 / 999.0, 1.0) + 0.5).floor().max(1.0)
}

/// Phosphor transmittance (per channel) at a pixel, and the pattern's mean.
fn mask_at(dtype: usize, pitch: f32, x: f32, y: f32) -> ([f32; 3], f32) {
    let soft = |t: f32| 0.5 + 0.5 * (TAU * t).cos(); // 1 at the stripe centre
    match dtype {
        1 => {
            // aperture grille: continuous vertical R/G/B stripes
            let t = (x / pitch).fract();
            let m = [soft(t - 1.0 / 6.0).powf(1.5), soft(t - 0.5).powf(1.5), soft(t - 5.0 / 6.0).powf(1.5)];
            (lift(m), 0.40)
        }
        2 => {
            // delta dots: triads on a hexagonal lattice
            let row = (y / (pitch * 0.866)).floor();
            let xo = x / pitch + if row as i64 % 2 == 0 { 0.0 } else { 0.5 };
            let fx = xo.fract();
            let fy = (y / (pitch * 0.866)).fract();
            let ch = ((fx * 3.0) as usize).min(2);
            let lx = (fx * 3.0).fract() - 0.5;
            let ly = fy - 0.5;
            let d = (lx * lx * 1.0 + ly * ly * 1.4).sqrt();
            let dot = (1.0 - smoothstep(0.28, 0.5, d)).max(0.0);
            let mut m = [0.0; 3];
            m[ch] = dot;
            (lift(m), 0.22)
        }
        4 => {
            // square grid: square cells with a black matrix, RGB sub-columns
            let fx = (x / pitch).fract();
            let fy = (y / pitch).fract();
            let edge = smoothstep(0.0, 0.12, fx.min(1.0 - fx)) * smoothstep(0.0, 0.12, fy.min(1.0 - fy));
            let ch = ((fx * 3.0) as usize).min(2);
            let mut m = [0.15 * edge; 3];
            m[ch] = edge;
            (lift(m), 0.38)
        }
        _ => {
            // slot mask (0) and fine slot (3): vertical slots with staggered breaks
            let fine = dtype == 3;
            let triad = (x / pitch).floor();
            let t = (x / pitch).fract();
            let stripe = if fine { 1.6 } else { 1.2 };
            let m0 = [soft(t - 1.0 / 6.0).powf(stripe), soft(t - 0.5).powf(stripe), soft(t - 5.0 / 6.0).powf(stripe)];
            let slot_h = if fine { pitch * 0.5 } else { pitch };
            let off = if triad as i64 % 2 == 0 { 0.0 } else { 0.5 };
            let fy = (y / slot_h + off).fract();
            let gap = smoothstep(0.0, 0.10, fy.min(1.0 - fy) - 0.04);
            let m = [m0[0] * gap, m0[1] * gap, m0[2] * gap];
            (lift(m), if fine { 0.30 } else { 0.33 })
        }
    }
}

#[inline]
fn lift(m: [f32; 3]) -> [f32; 3] {
    // the black matrix is not perfectly black
    [0.06 + 0.94 * m[0], 0.06 + 0.94 * m[1], 0.06 + 0.94 * m[2]]
}

#[inline]
fn phosphor_mask(lin: [f32; 3], m: [f32; 3], amt: f32, mean: f32) -> [f32; 3] {
    let mut o = [0.0; 3];
    for i in 0..3 {
        let fill = smoothstep(0.18, 0.85, lin[i]) * 0.85;
        let m_eff = mix(m[i], 1.0, fill);
        let mean_eff = mix(mean, 1.0, fill);
        let applied = mix(1.0, m_eff, amt);
        let norm = mix(1.0, mean_eff, amt);
        o[i] = lin[i] * applied / norm.max(0.30);
    }
    o
}

/// Separable box blur, repeated for a soft gaussian-like falloff.
fn blur(img: &Img, radius: usize, passes: usize) -> Img {
    let mut cur = img.clone();
    if radius == 0 {
        return cur;
    }
    let (w, h) = (img.w, img.h);
    for _ in 0..passes {
        let mut tmp = Img::new(w, h);
        tmp.px.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
            let src = &cur.px[y * w..(y + 1) * w];
            let mut acc = [0.0f32; 3];
            let r = radius as isize;
            for k in -r..=r {
                let q = src[k.clamp(0, w as isize - 1) as usize];
                acc[0] += q[0];
                acc[1] += q[1];
                acc[2] += q[2];
            }
            let n = (2 * radius + 1) as f32;
            for x in 0..w {
                row[x] = [acc[0] / n, acc[1] / n, acc[2] / n];
                let add = src[(x as isize + r + 1).clamp(0, w as isize - 1) as usize];
                let sub = src[(x as isize - r).clamp(0, w as isize - 1) as usize];
                for i in 0..3 {
                    acc[i] += add[i] - sub[i];
                }
            }
        });
        let mut out = Img::new(w, h);
        let cols: Vec<Vec<[f32; 3]>> = (0..w)
            .into_par_iter()
            .map(|x| {
                let r = radius as isize;
                let mut col = vec![[0.0f32; 3]; h];
                let at = |y: isize| tmp.px[y.clamp(0, h as isize - 1) as usize * w + x];
                let mut acc = [0.0f32; 3];
                for k in -r..=r {
                    let q = at(k);
                    acc[0] += q[0];
                    acc[1] += q[1];
                    acc[2] += q[2];
                }
                let n = (2 * radius + 1) as f32;
                for y in 0..h {
                    col[y] = [acc[0] / n, acc[1] / n, acc[2] / n];
                    let add = at(y as isize + r + 1);
                    let sub = at(y as isize - r);
                    for i in 0..3 {
                        acc[i] += add[i] - sub[i];
                    }
                }
                col
            })
            .collect();
        for (x, col) in cols.iter().enumerate() {
            for y in 0..h {
                out.px[y * w + x] = col[y];
            }
        }
        cur = out;
    }
    cur
}

/// Bright rolling exposure flashes (CRT exposureFlash block): flashes come in blocks of
/// three 25 fps frames, held 1..3 frames with ages 1 / 0.86 / 0.63; each block draws one band
/// (sometimes two) across the picture with a hard and a soft edge, then raises exposure by
/// 2^(1.62 x field) in linear light with highlight protection.
struct Flash {
    amount: f32,
    age: f32,
    bands: Vec<(f32, f32, f32, f32, f32)>, // low, high, low width, high width, intensity
    sa: f32,
    ca: f32,
}

fn fract_sin(x: f32) -> f32 {
    let v = (x as f64).sin() * 43758.5453123;
    (v - v.floor()) as f32
}

impl Flash {
    fn new(p: &Params, ft: &FrameTime) -> Option<Flash> {
        let amount = p.get("exposureFlash").clamp(0.0, 1.0);
        if amount <= 0.0 {
            return None;
        }
        let frame_dur = 1.0 / ft.fps.max(1.0);
        let sampled = (ft.t / frame_dur + 1e-4).floor() * frame_dur;
        let seq = (sampled * 25.0 + 1e-4).floor();
        let block = (seq / 3.0).floor();
        let local = (seq - block * 3.0) as i32;
        let hold = 1 + ((fract_sin(block * 29.17 + 8.31) * 3.0).floor() as i32).min(2);
        if local >= hold {
            return None;
        }
        let age = [1.0, 0.86, 0.63][local.clamp(0, 2) as usize];
        let softness = 0.65f32;
        let band = |salt: f32, soft: f32| {
            let sv = fract_sin(block * 19.19 + salt);
            let pos = fract_sin(block * 47.73 + salt * 1.91);
            let ext = fract_sin(block * 73.11 + salt * 2.37);
            let (lo, hi) = if pos < 0.21 {
                (-0.14, 0.20 + 0.31 * ext)
            } else if pos < 0.51 {
                let lo = 0.17 + 0.31 * ext;
                (lo, (lo + 0.16 + 0.27 * fract_sin(block * 31.37 + salt)).min(0.86))
            } else {
                (0.48 + 0.31 * ext, 1.14)
            };
            let hard_first = fract_sin(block * 11.83 + salt * 3.13) > 0.48;
            let hard = 0.0025 + 0.013 * soft * soft;
            let softw = 0.035 + 0.145 * soft * soft;
            let (lw, hw) = if hard_first { (hard, softw) } else { (softw, hard) };
            (lo, hi, lw, hw, 0.48 + 0.52 * sv)
        };
        let mut bands = vec![band(17.0, softness)];
        if fract_sin(block * 61.71 + 2.9) > 0.78 {
            let mut b = band(83.0, softness * 0.84);
            b.4 *= 0.66;
            bands.push(b);
        }
        let (sa, ca) = p.get("screenPulseAngle").to_radians().sin_cos();
        Some(Flash { amount, age, bands, sa, ca })
    }

    fn apply(&self, rgb: [f32; 3], u: f32, v: f32) -> [f32; 3] {
        let axis = (u - 0.5) * self.sa + (v - 0.5) * self.ca + 0.5;
        let field = self
            .bands
            .iter()
            .map(|&(lo, hi, lw, hw, it)| smoothstep(lo - lw, lo + lw, axis) * (1.0 - smoothstep(hi - hw, hi + hw, axis)) * it)
            .fold(0.0, f32::max);
        let exposure = 1.62 * self.amount * self.age * field;
        if exposure <= 1e-6 {
            return rgb;
        }
        let lin = [rgb[0].max(0.0).powf(2.2), rgb[1].max(0.0).powf(2.2), rgb[2].max(0.0).powf(2.2)];
        let gain = 2f32.powf(exposure);
        let luma = (lin[0] * 0.2126 + lin[1] * 0.7152 + lin[2] * 0.0722).max(0.0);
        let protect = smoothstep(0.72, 1.18, luma);
        let protected = gain + (1.0 - gain) * protect * 0.72;
        let coupling = smoothstep(0.006, 0.35, luma);
        let applied = 1.0 + (protected - 1.0) * mix(0.32, 1.0, coupling);
        let lift = 0.02 * (gain - 1.0).clamp(0.0, 1.0) * (0.65 + 0.35 * smoothstep(0.0, 0.18, luma));
        [
            (lin[0] * applied + lift).powf(1.0 / 2.2),
            (lin[1] * applied + lift).powf(1.0 / 2.2),
            (lin[2] * applied + lift).powf(1.0 / 2.2),
        ]
    }
}

/// The filmed-CRT exposure model (CRT SCREEN PULSE): a camera with shutter 1/S watching a
/// tube refreshing at R Hz integrates N = R/S fields; each screen row's light is the sum of the
/// fields the beam painted while the shutter was open, with phosphor decay (persistence),
/// mains ripple on the beam current (HV ripple), sync wander and slips (sync) and rolling-shutter
/// readout, normalised by the mean so only the banding remains.
struct Pulse {
    amount: f32,
    n: f32,
    mains: f32,
    tau: [f32; 3],
    ripple: f32,
    tube_time: f32,
    refresh: f32,
    beam: f32,
    lines: f32,
    read: f32,
    bc: f32,
    bs: f32,
    theta: f32,
    aspect: f32,
    slip: Option<(f32, f32, f32)>, // slip position, abort row, new beam row
    mean: [f32; 3],
    den: [f32; 3],
    en: [f32; 3],
    ratio: f32,
}

impl Pulse {
    fn new(p: &Params, ft: &FrameTime, raster_lines: f32, aspect: f32) -> Option<Pulse> {
        let amount = p.get("screenPulse").clamp(0.0, 1.0);
        if amount <= 0.0 {
            return None;
        }
        let nearest = |v: f32, ladder: &[f32]| *ladder.iter().min_by(|a, b| (v - **a).abs().total_cmp(&(v - **b).abs())).unwrap();
        let refresh = nearest(p.get("screenPulseRefresh"), &[50.0, 59.94, 60.0, 75.0, 85.0, 100.0, 120.0]);
        let shutter = nearest(
            p.get("screenPulseShutter"),
            &[24.0, 30.0, 40.0, 48.0, 50.0, 60.0, 90.0, 96.0, 100.0, 120.0, 125.0, 144.0, 180.0, 250.0, 500.0],
        );
        let n = (refresh / shutter).clamp(0.02, 8.0);
        let mains = if (refresh - 50.0).abs() < 2.0 || (refresh - 100.0).abs() < 3.0 { 50.0 } else { 60.0 };
        let fps = if ft.fps > 1.0 { ft.fps } else { 24.0 };
        let persist = p.get("screenPulsePersistence").clamp(0.0, 1.0);
        let tau_ms = mix(0.05, 2.0, persist * persist);
        let tau = [
            (tau_ms * 0.001 * refresh * 1.15).max(1e-4),
            (tau_ms * 0.001 * refresh * 1.0).max(1e-4),
            (tau_ms * 0.001 * refresh * 0.45).max(1e-4),
        ];
        let ripple = 0.55 * p.get("screenPulseEdge").clamp(0.0, 1.0);
        let lock = p.get("screenPulseJitter").clamp(0.0, 1.0);
        let frame = (ft.frame as f32).floor();
        let tube_time = frame / ft.fps.max(1.0);
        let mut beam = (frame * (refresh / fps).fract()).fract();
        if lock > 0.0 {
            let fi = (frame / 8.0).floor();
            let ff = (frame / 8.0).fract();
            let w0 = hash(7, 31, fi as u32, SEED);
            let w1 = hash(7, 31, fi as u32 + 1, SEED);
            let wander = mix(w0, w1, ff * ff * (3.0 - 2.0 * ff)) - 0.5;
            let white = hash(13, 71, frame as u32, SEED ^ 0x5F) - 0.5;
            beam += lock * (0.45 * wander + 0.03 * white);
        }
        let mut slip = None;
        if lock > 0.0 && hash(23, 97, frame as u32, SEED ^ 0xA3) < lock * lock * 0.40 {
            let c = hash(29, 53, frame as u32, SEED ^ 0x2B) * n;
            slip = Some((c, (beam - c).rem_euclid(1.0), c.fract()));
        }
        let theta = p.get("screenPulseAngle").to_radians();
        let q = [(-1.0 / tau[0]).exp(), (-1.0 / tau[1]).exp(), (-1.0 / tau[2]).exp()];
        let den = [(1.0 - q[0]).max(1e-5), (1.0 - q[1]).max(1e-5), (1.0 - q[2]).max(1e-5)];
        let en = [(-n / tau[0]).exp(), (-n / tau[1]).exp(), (-n / tau[2]).exp()];
        let wd = n - n.floor();
        let kh = n.floor() + 1.0;
        let mut mean = [0.0; 3];
        for c in 0..3 {
            let e0w = tau[c] * (1.0 - (-wd / tau[c]).exp());
            let ew1 = tau[c] * ((-wd / tau[c]).exp() - (-1.0 / tau[c]).exp());
            let corr = ((1.0 - q[c].powf(kh)) * e0w + (1.0 - q[c].powf((kh - 1.0).max(0.0))) * ew1) / den[c];
            mean[c] = (n - corr + tau[c] * (1.0 - en[c])).max(1e-4);
        }
        Some(Pulse {
            amount,
            n,
            mains,
            tau,
            ripple,
            tube_time,
            refresh,
            beam,
            lines: (raster_lines * 0.5).max(1.0),
            read: p.get("screenPulseReadout").clamp(0.0, 1.5),
            bc: theta.cos(),
            bs: theta.sin(),
            theta,
            aspect,
            slip,
            mean,
            den,
            en,
            ratio: p.get("screenPulseRatio").clamp(0.0, 2.0),
        })
    }

    fn apply(&self, rgb: [f32; 3], gu: f32, gv: f32, soften: f32) -> [f32; 3] {
        let phase = if self.theta.abs() < 1e-7 {
            self.read * gv - gv - gu / self.lines
        } else {
            (self.read - 1.0) * (gv * self.bc - (gu - 0.5) * self.aspect * self.bs) - gu / self.lines
        };
        let edge_soft = (1.5 + 2.5 * soften) / self.lines + mix(0.02, 0.05, soften) * self.n;
        let mut light = [0.0f32; 3];
        let add = |a: f32, w: f32, light: &mut [f32; 3]| {
            let cur = 1.0 + self.ripple * (TAU * (2.0 * self.mains * 1.0012) * (self.tube_time - a / self.refresh)).sin();
            for c in 0..3 {
                light[c] += w * cur * (1.0 - (-a / self.tau[c]).exp());
            }
        };
        let so = (self.beam + phase).rem_euclid(1.0);
        match self.slip {
            None => {
                for k in 0..9 {
                    let a = so + k as f32;
                    let w = 1.0 - smoothstep(self.n - edge_soft, self.n + edge_soft, a);
                    if w <= 0.001 {
                        break;
                    }
                    add(a, w, &mut light);
                }
            }
            Some((c, y_abort, b_new)) => {
                let sn = (b_new + phase).rem_euclid(1.0);
                for k in 0..9 {
                    let a = sn + k as f32;
                    if a >= c {
                        break;
                    }
                    add(a, 1.0, &mut light);
                }
                let missed = (-phase).rem_euclid(1.0) > y_abort;
                for k in 0..9 {
                    let a = so + k as f32;
                    let w = 1.0 - smoothstep(self.n - edge_soft, self.n + edge_soft, a);
                    if w <= 0.001 {
                        break;
                    }
                    if a < c || (missed && a < c + 1.0) {
                        continue;
                    }
                    add(a, w, &mut light);
                }
            }
        }
        let r = (self.n - so).ceil() - (self.n - so);
        let mut out = [0.0f32; 3];
        for c in 0..3 {
            light[c] += (1.0 - self.en[c]) * (-r / self.tau[c]).exp() / self.den[c];
            let lit = light[c] / self.mean[c];
            let mut gain = (1.0 + (lit - 1.0) * (self.amount * self.ratio)).max(0.0);
            const SHOULDER: f32 = 2.9;
            let over = (gain - SHOULDER).max(0.0);
            gain = gain.min(SHOULDER) + 1.2 * (1.0 - (-over / 1.2).exp());
            out[c] = (rgb[c].max(0.0).powf(2.2) * gain).powf(1.0 / 2.2);
        }
        out
    }
}

fn display(sig: &Img, p: &Params, ft: &FrameTime, ow: usize, oh: usize, preview_scale: f32) -> Img {
    let dtype = (p.get("displayType").round() as usize).min(4);
    // The TUBE control feeds several display inputs at once:
    // mask + 1.0, curvature + 0.55, vignette 0.7, moire + 0.5, convergence + 0.35, flicker + 0.2.
    let tube = p.get("tube").clamp(0.0, 1.0);
    let mask_amt = (p.get("phosphor") + tube).clamp(0.0, 1.0);
    let pitch = mask_pitch(dtype, p.get("maskScale"), preview_scale);
    let curve = (p.get("geoCurve") + 0.55 * tube).min(1.0) * [1.0, 0.58, 0.82, 0.70, 0.78][dtype];
    let geo_lin = p.get("geoStretch");
    let zoom = p.get("geoZoom");
    let fisheye = p.get("lensFisheye");
    let vignette = 0.7 * tube;
    let flicker = (p.get("flicker") + 0.2 * tube).min(1.0);
    let soften = p.get("soften").clamp(0.0, 1.0);
    let moire = (p.get("moire") + 0.5 * tube).min(1.0);
    let fringe = (p.get("fringe") + 0.35 * tube).clamp(0.0, 1.0);
    let angle = p.get("beamConvergeAngle").to_radians();
    let pconv = p.get("phosphorConverge").clamp(0.0, 1.0);
    let split = p.get("rgbShift");
    let rainbow = p.get("rainbowInterference").clamp(0.0, 1.0);
    let aspect = ow as f32 / oh as f32;
    let lines = sig.h as f32;
    let scale_px = ow as f32 / 854.0; // offsets authored against a 854-wide picture
    let frame = ft.frame;

    // whole-picture brightness flicker (field-to-field beam current wobble)
    // camera capture: the filmed-CRT exposure bands and exposure flashes (CRT)
    let pulse = Pulse::new(p, ft, sig.h as f32 * 525.0 / 480.0, aspect);
    let flash = Flash::new(p, ft);
    let flick = if flicker > 0.0 {
        1.0 - flicker * (0.05 * hash(frame, 1, 51, SEED) + 0.03 * (ft.t * TAU * 7.3).sin().abs())
    } else {
        1.0
    };

    let mut out = Img::new(ow, oh);
    out.px.par_chunks_mut(ow).enumerate().for_each(|(y, row)| {
        for (x, o) in row.iter_mut().enumerate() {
            let u = (x as f32 + 0.5) / ow as f32;
            let v = (y as f32 + 0.5) / oh as f32;
            let mut cx = u * 2.0 - 1.0;
            let mut cy = v * 2.0 - 1.0;
            if fisheye != 0.0 {
                let r2 = (cx * cx * aspect * aspect + cy * cy) / (aspect * aspect + 1.0);
                let f = 1.0 + fisheye * r2;
                cx *= f;
                cy *= f;
            }
            cx /= 1.0 + zoom * 0.12;
            cy /= 1.0 + zoom * 0.12;
            if geo_lin != 0.0 {
                // scan-linearity error
                cx += geo_lin * 0.09 * (1.0 - cx * cx);
                cy += geo_lin * 0.05 * (1.0 - cy * cy);
            }
            let pax = cx * aspect;
            let rn = (pax * pax + cy * cy) / (aspect * aspect + 1.0);
            let k = 1.0 + curve * (0.035 * rn + 0.16 * rn * rn);
            cx *= k;
            cy *= k;
            let gu = cx * 0.5 + 0.5;
            let gv = cy * 0.5 + 0.5;
            // anti-aliased raster edge
            let px_u = 1.5 / ow as f32;
            let px_v = 1.5 / oh as f32;
            let cover = smoothstep(0.0, px_u, gu) * smoothstep(0.0, px_u, 1.0 - gu) * smoothstep(0.0, px_v, gv) * smoothstep(0.0, px_v, 1.0 - gv);
            if cover <= 0.0 {
                *o = [0.0; 3];
                continue;
            }
            // beam convergence: red and blue land away from green, growing toward the edges
            let defl = rn;
            let conv = 0.32 * fringe * defl * 6.0 * scale_px;
            let (sa, ca) = angle.sin_cos();
            let dr = (ca * conv + split * 0.01 * ow as f32, sa * conv);
            let db = (-ca * conv - split * 0.01 * ow as f32, -sa * conv);
            let sx = gu * sig.w as f32;
            let sy = gv * sig.h as f32;
            let fx_r = dr.0 / ow as f32 * sig.w as f32;
            let fy_r = dr.1 / oh as f32 * sig.h as f32;
            let fx_b = db.0 / ow as f32 * sig.w as f32;
            let fy_b = db.1 / oh as f32 * sig.h as f32;
            let samp = |ox: f32, oy: f32| -> [f32; 3] {
                if soften > 0.0 {
                    // beam spread across neighbouring lines and along the line
                    let s = 0.6 + 1.6 * soften;
                    let a = sig.sample(sx + ox, sy + oy);
                    let b = sig.sample(sx + ox - s, sy + oy - s * 0.5);
                    let c = sig.sample(sx + ox + s, sy + oy + s * 0.5);
                    let wgt = 0.25 + 0.25 * soften;
                    mix3(a, [(b[0] + c[0]) * 0.5, (b[1] + c[1]) * 0.5, (b[2] + c[2]) * 0.5], wgt)
                } else {
                    sig.sample(sx + ox, sy + oy)
                }
            };
            let g = samp(0.0, 0.0);
            let r = if conv != 0.0 || split != 0.0 { samp(fx_r, fy_r)[0] } else { g[0] };
            let b = if conv != 0.0 || split != 0.0 { samp(fx_b, fy_b)[2] } else { g[2] };
            let mut lin = [r.max(0.0).powf(2.2), g[1].max(0.0).powf(2.2), b.max(0.0).powf(2.2)];

            if mask_amt > 0.0 {
                // scanlines: the beam traces discrete lines of the signal raster
                let lpos = gv * lines;
                let fy = lpos.fract() - 0.5;
                // beam spot width from the receiver: 0.95 + 0.85 * soften (in line pairs)
                let sigma = (BEAM_SIGMA_BASE + BEAM_SIGMA_SOFTEN * soften) * 0.32;
                let beam = (-(fy * fy) / (2.0 * sigma * sigma)).exp();
                let beam_mean = 0.66 + 0.2 * soften;
                let scan = mix(1.0, beam / beam_mean, mask_amt * 0.65 * (oh as f32 / lines / 2.0).min(1.0));
                // phosphor landing error spreads the mask per channel toward the edges
                let shift = 1.8 * pconv * defl * pitch;
                let mx = x as f32;
                let my = y as f32;
                let (m, mean) = if pconv > 0.0 {
                    let (mr, mean) = mask_at(dtype, pitch, mx + shift, my);
                    let (mg, _) = mask_at(dtype, pitch, mx, my);
                    let (mb, _) = mask_at(dtype, pitch, mx - shift, my);
                    ([mr[0], mg[1], mb[2]], mean)
                } else {
                    mask_at(dtype, pitch, mx, my)
                };
                lin = phosphor_mask(lin, m, mask_amt, mean);
                for c in lin.iter_mut() {
                    *c *= scan;
                }
            }
            let mut rgb = [lin[0].max(0.0).powf(1.0 / 2.2), lin[1].max(0.0).powf(1.0 / 2.2), lin[2].max(0.0).powf(1.0 / 2.2)];

            if moire > 0.0 {
                let g1 = (TAU * (x as f32 * 0.0131 + y as f32 * 0.0071)).sin();
                let g2 = (TAU * (x as f32 * 0.0089 - y as f32 * 0.0113 + ft.t * 0.05)).sin();
                let gain = 1.0 + moire * 0.2 * g1 * g2;
                for c in rgb.iter_mut() {
                    *c *= gain;
                }
            }
            if rainbow > 0.0 {
                let s = rainbow * 0.20;
                let ph = TAU * (gv * 38.0 + gu * 6.0 - ft.t * 0.6);
                for (i, c) in rgb.iter_mut().enumerate() {
                    *c *= 1.0 + s * (ph + i as f32 * TAU / 3.0).cos();
                }
            }
            if let Some(f) = &flash {
                rgb = f.apply(rgb, gu, gv);
            }
            if let Some(pl) = &pulse {
                rgb = pl.apply(rgb, gu, gv, soften);
            }
            if vignette > 0.0 {
                let vg = 1.0 - vignette * 0.85 * smoothstep(0.15, 1.25, rn * 1.6);
                for c in rgb.iter_mut() {
                    *c *= vg;
                }
            }
            for c in rgb.iter_mut() {
                *c *= flick * cover;
            }
            *o = rgb;
        }
    });

    // ---- glass: bloom, halation, spectral diffusion, sharpen ----
    // crtBloom = min(max(glow, 0)^0.72, 1); phosphorBloom + 0.32 * tube
    let glow = p.get("glow").max(0.0).powf(0.72).min(1.0);
    let pbloom = (p.get("phosphorBloom") + 0.32 * tube).clamp(0.0, 1.0);
    let spectral = p.get("spectral").clamp(0.0, 1.0);
    let sharpen = p.get("postSharpen").clamp(0.0, 1.0);
    if glow > 0.0 || pbloom > 0.0 || spectral > 0.0 {
        let small = out.resized((ow / 4).max(2), (oh / 4).max(2));
        let near = blur(&small, 2, 2);
        let wide = if spectral > 0.0 { Some(blur(&small, 6, 3)) } else { None };
        let (sw, sh) = (small.w as f32 / ow as f32, small.h as f32 / oh as f32);
        out.px.par_chunks_mut(ow).enumerate().for_each(|(y, row)| {
            for (x, o) in row.iter_mut().enumerate() {
                let bx = (x as f32 + 0.5) * sw;
                let by = (y as f32 + 0.5) * sh;
                let n = near.sample(bx, by);
                let mut c = *o;
                if glow > 0.0 || pbloom > 0.0 {
                    let amt = glow * 0.7 + pbloom * 0.35;
                    for i in 0..3 {
                        // light scattered from the bright parts, screened over the picture
                        let add = (n[i] * n[i] * 1.4 + n[i] * 0.3) * amt;
                        c[i] = 1.0 - (1.0 - c[i]) * (1.0 - add.min(1.0));
                    }
                }
                if let Some(wd) = &wide {
                    let wv = wd.sample(bx, by);
                    if spectral > 0.0 {
                        let u = (x as f32 + 0.5) / ow as f32 - 0.5;
                        let v = (y as f32 + 0.5) / oh as f32 - 0.5;
                        let hue = luma(wv) * 0.8 + (u * u + v * v).sqrt() * 1.3 + ft.t * 0.03;
                        let tint = hue2rgb(hue);
                        let l = luma(wv);
                        for i in 0..3 {
                            let add = (tint[i] * l * spectral * 0.6).min(1.0);
                            c[i] = 1.0 - (1.0 - c[i]) * (1.0 - add);
                        }
                    }
                }
                *o = c;
            }
        });
    }
    if sharpen > 0.0 {
        let soft = blur(&out, 1, 1);
        out.px.par_iter_mut().zip(soft.px.par_iter()).for_each(|(o, s)| {
            for i in 0..3 {
                o[i] += (o[i] - s[i]) * sharpen * 1.5;
            }
        });
    }

    // ---- output levels ----
    let bright = p.get("outputBrightness").clamp(-0.25, 0.25);
    let contrast = p.get("outputContrast").clamp(0.5, 2.5);
    if bright.abs() > 1e-7 || (contrast - 1.0).abs() > 1e-7 {
        out.px.par_iter_mut().for_each(|o| {
            let mut c = [0.0f32; 3];
            for i in 0..3 {
                c[i] = ((o[i] - 0.5) * contrast + 0.5).clamp(0.0, 1.0);
            }
            // brightness lifts the highlights, leaving blacks where they are
            let value = c[0].max(c[1]).max(c[2]);
            let gain = 1.0 + bright * 2.0 * smoothstep(0.12, 0.90, value);
            for i in 0..3 {
                o[i] = (c[i] * gain).clamp(0.0, 1.0);
            }
        });
    }
    out
}
