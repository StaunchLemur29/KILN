//! The analog receiver, in stages: wave encode, FM transport, receiver measure, receiver
//! analyze, line decode and receiver decode. The picture is
//! laid onto a full NTSC raster with sync and colour burst, sent through an FM tape transport
//! where the damage happens, then a receiver finds sync and burst again and decodes it.

use crate::fx::{FrameTime, Img};
use crate::params::{Params, Tier};
use rayon::prelude::*;
use std::f32::consts::{PI, TAU};

const NTSC_SAMPLES: f32 = 910.0;
const NTSC_LINES: f32 = 525.0;
const SEED: u32 = 0x5EED_1234;

/// Receiver memory carried from frame to frame (receiver analyze `state[]`).
#[derive(Clone)]
pub struct RxState {
    h_phase: f32,
    h_velocity: f32,
    burst_phase: f32,
    burst_lock: f32,
    agc: f32,
    v_phase: f32,
    v_velocity: f32,
    carrier_lock: f32,
}

impl Default for RxState {
    fn default() -> Self {
        RxState { h_phase: 0.0, h_velocity: 0.0, burst_phase: 0.0, burst_lock: 1.0, agc: 1.0, v_phase: 0.0, v_velocity: 0.0, carrier_lock: 1.0 }
    }
}

/// Raster geometry: SD is the 910 x 525 broadcast raster with a 752 x 480 active picture at
/// (135, 20); HI doubles the samples (1822); FULL sizes the active area to the picture.
struct Raster {
    w: usize,
    lines: usize,
    ax0: f32,
    aw: f32,
    ay0: f32,
    ah: f32,
    sd: bool,
    source: bool,
}

fn raster_for(tier: Tier, out_w: usize, out_h: usize) -> Raster {
    match tier {
        Tier::Sd => Raster { w: 910, lines: 525, ax0: 135.0, aw: 752.0, ay0: 20.0, ah: 480.0, sd: true, source: false },
        Tier::Hi => Raster { w: 1822, lines: 525, ax0: 270.0, aw: 1504.0, ay0: 20.0, ah: 480.0, sd: false, source: false },
        Tier::Full => {
            // the active area is the picture (at least 64 x 48); the raster keeps NTSC's
            // proportions, its width rounded to 2 mod 4 and wide enough for the picture
            let aw = out_w.max(64);
            let ah = out_h.max(48);
            let mut w = (aw as f32 * NTSC_SAMPLES / 752.0).round() as usize;
            let r = w % 4;
            if r != 2 {
                w += if r < 2 { 2 - r } else { 6 - r };
            }
            let ax0 = (w as f32 * 135.0 / NTSC_SAMPLES).round() as usize;
            while w < ax0 + aw + 2 {
                w += 4;
            }
            let mut lines = (ah as f32 * NTSC_LINES / 480.0).round() as usize;
            let ay0 = ((lines as f32 * 20.0 / NTSC_LINES).round() as usize).max(1);
            lines = lines.max(ay0 + ah + 1);
            Raster { w, lines, ax0: ax0 as f32, aw: aw as f32, ay0: ay0 as f32, ah: ah as f32, sd: false, source: true }
        }
    }
}

#[inline]
fn hash(x: u32, y: u32, n: u32, seed: u32) -> f32 {
    let mut h = x.wrapping_mul(0x9E37_79B1) ^ y.wrapping_mul(0x85EB_CA77) ^ n.wrapping_mul(0xC2B2_AE3D) ^ seed.wrapping_mul(0x27D4_EB2F);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2C1B_3C6D);
    h ^= h >> 12;
    h = h.wrapping_mul(0x297A_2D39);
    h ^= h >> 15;
    (h & 0x00FF_FFFF) as f32 / 16_777_216.0
}

#[inline]
fn gauss(x: u32, y: u32, n: u32, seed: u32) -> f32 {
    let a = hash(x, y, n, seed).max(1e-6);
    let b = hash(x, y, n, seed ^ 0x68BC_21EB);
    (-2.0 * a.ln()).sqrt() * (TAU * b).cos()
}

fn value_noise(px: f32, py: f32, epoch: u32, seed: u32) -> f32 {
    let (cx, cy) = (px.floor(), py.floor());
    let (mut fx, mut fy) = (px - cx, py - cy);
    fx = fx * fx * (3.0 - 2.0 * fx);
    fy = fy * fy * (3.0 - 2.0 * fy);
    let (x, y) = (cx.max(0.0) as u32, cy.max(0.0) as u32);
    let a = hash(x, y, epoch, seed);
    let b = hash(x + 1, y, epoch, seed);
    let d = hash(x, y + 1, epoch, seed);
    let e = hash(x + 1, y + 1, epoch, seed);
    let top = a + (b - a) * fx;
    let bot = d + (e - d) * fx;
    top + (bot - top) * fy
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

/// A wandering control voltage through an RC low-pass (RC contact).
fn rc_contact(time: f32, cutoff_hz: f32, seed: u32) -> f32 {
    let drive_rate = cutoff_hz.max(0.001) * 8.0;
    let st = time.max(0.0) * drive_rate;
    let cell = st.floor() as u32;
    let frac = st.fract();
    let alpha = (-TAU / 8.0).exp();
    let (mut boundary, mut weight) = (0.0, 1.0 - alpha);
    for k in 0..12u32 {
        let prior = if cell > k { cell - k - 1 } else { 0 };
        boundary += weight * (hash(prior, 0, 92, seed) * 2.0 - 1.0);
        weight *= alpha;
    }
    let current = hash(cell, 0, 92, seed) * 2.0 - 1.0;
    current + (boundary - current) * alpha.powf(frac)
}

/// NTSC subcarrier phase of a line: 90 degrees per field, 180 degrees line to line.
#[inline]
fn raster_phase(line: usize, clock: f32) -> f32 {
    let field = ((clock * 2.0).round().max(0.0) as u32) & 3;
    field as f32 * (PI * 0.5) + (line & 1) as f32 * PI
}

#[inline]
fn yiq(c: [f32; 3]) -> [f32; 3] {
    [
        0.299 * c[0] + 0.587 * c[1] + 0.114 * c[2],
        0.595716 * c[0] - 0.274453 * c[1] - 0.321263 * c[2],
        0.211456 * c[0] - 0.522591 * c[1] + 0.311135 * c[2],
    ]
}

#[inline]
fn rgb(c: [f32; 3]) -> [f32; 3] {
    [
        c[0] + 0.9563 * c[1] + 0.6210 * c[2],
        c[0] - 0.2721 * c[1] - 0.6474 * c[2],
        c[0] - 1.1070 * c[1] + 1.7046 * c[2],
    ]
}

/// The whole receiver path. `src` is the clean picture at the output (signal) size; the result
/// has the same size.
pub fn receive(src: &Img, p: &Params, ft: &FrameTime, st: &mut RxState) -> Img {
    let (w, h) = (src.w, src.h);
    let r = raster_for(p.tier, w, h);
    let tube = p.get("tube").clamp(0.0, 1.0);
    // uniforms
    let sig_jitter = (p.get("jitter") + 0.12 * tube).min(1.0);
    let sig_tear = p.get("tear").min(1.0);
    let sig_roll = p.get("vroll");
    let sig_drop = p.get("dropouts").min(1.0);
    let sig_hum = p.get("humBar");
    let sig_ghost = p.get("ghosting");
    let sig_head = p.get("headSwitch");
    let sparkle = p.get("sparkle").min(1.0);
    let grit = p.get("grit").min(1.0);
    let drift = p.get("drift");
    let chroma_gain = p.get("bleed");
    let burn = p.get("burn");
    let chroma_bleed = p.get("chromaBleed").clamp(0.0, 1.0);
    let top_amt = p.get("topSpectral").clamp(0.0, 1.0);
    let drop_style = p.get("dropoutStyle").clamp(0.0, 4.0).round() as i32;
    let soften = p.get("soften").clamp(0.0, 1.0);
    let clean = p.get("signalClean").clamp(0.0, 1.0);
    let tf = ft.frame;
    let clock = ft.frame as f32; // vshimmer == vframe in playback
    let time = ft.t;
    let vfps = ft.fps.max(1.0);

    let rw = r.w;
    let lines = r.lines;
    let sx = rw as f32 / NTSC_SAMPLES;
    let sy = lines as f32 / NTSC_LINES;
    let spc = rw as f32 / 227.5; // samples per subcarrier cycle

    // ---------------- wave encode ----------------
    let sync_w = (67.0 * sx).round() as usize;
    let eq_w = (34.0 * sx).round() as usize;
    let half = rw / 2;
    let spc_q = ((rw as f32 / 227.5).round() as usize).max(1);
    let burst0 = spc_q * (76.0 * sx / spc_q as f32).round() as usize;
    let burst1 = burst0 + spc_q * (36.0 * sx / spc_q as f32).round() as usize;
    let eq_end = (3.0 * sy).round() as usize;
    let broad_end = (6.0 * sy).round() as usize;
    let vbi_end = (9.0 * sy).round() as usize;
    let (ax0, ax1) = (r.ax0 as usize, (r.ax0 + r.aw) as usize);
    let (ay0, ay1) = (r.ay0 as usize, (r.ay0 + r.ah) as usize);
    let (cos_x, sin_x): (Vec<f32>, Vec<f32>) = (0..rw).map(|x| {
        let (s, c) = (TAU * x as f32 / spc).sin_cos();
        (c, s)
    }).unzip();
    let chroma_step = spc * 0.5 / r.aw;
    let grit_cell = 26.0 * 4.0; // 26 px cells at os = 4

    let mut wave = vec![0.0f32; rw * lines];
    wave.par_chunks_mut(rw).enumerate().for_each(|(y, row)| {
        let equalizing = y < eq_end || (y >= broad_end && y < vbi_end);
        let broad = y >= eq_end && y < broad_end;
        let lp = raster_phase(y, clock);
        let (lps, lpc) = lp.sin_cos();
        for (x, v) in row.iter_mut().enumerate() {
            *v = if equalizing {
                if x < eq_w || (x >= half && x < half + eq_w) { -0.40 } else { 0.0 }
            } else if broad {
                if x < half - sync_w || (x >= half && x < rw - sync_w) { -0.40 } else { 0.0 }
            } else if x < sync_w {
                -0.40
            } else if x >= burst0 && x < burst1 {
                0.20 * (cos_x[x] * lpc - sin_x[x] * lps)
            } else if x >= ax0 && x < ax1 && y >= ay0 && y < ay1 {
                let u = ((x - ax0) as f32 + 0.5) / r.aw;
                let vv = ((y - ay0) as f32 + 0.5) / r.ah;
                let mut c = yiq(src.sample_uv(u, vv));
                if top_amt > 0.0 {
                    let top_h = (1.5 + 5.0 * top_amt) / r.ah;
                    let mask = 1.0 - smoothstep(0.0, top_h, vv);
                    if mask > 0.0 {
                        let ph = TAU * hash(tf, y as u32, 313, SEED);
                        let drive = mask * top_amt;
                        c[0] += drive * (hash(x as u32 >> 3, tf, 317, SEED) - 0.5) * 0.32;
                        c[1] += ph.cos() * drive * 0.52;
                        c[2] += ph.sin() * drive * 0.52;
                    }
                }
                let cross = if chroma_bleed > 0.0 {
                    let ym = yiq(src.sample_uv((u - chroma_step).clamp(0.0, 1.0), vv))[0];
                    let yp = yiq(src.sample_uv((u + chroma_step).clamp(0.0, 1.0), vv))[0];
                    let band = c[0] - 0.5 * (ym + yp);
                    0.3 * (band * chroma_bleed / 0.3).tanh()
                } else {
                    0.0
                };
                let (cp, sp) = (cos_x[x] * lpc - sin_x[x] * lps, sin_x[x] * lpc + cos_x[x] * lps);
                let mut val = 0.075 + 0.75 * c[0] + 0.35 * (c[1] * cp + c[2] * sp) + cross;
                if grit > 0.0 {
                    let ni = value_noise(x as f32 / grit_cell, y as f32 / grit_cell, tf, SEED ^ 0x00AC_1DED) - 0.5;
                    let nq = value_noise(x as f32 / grit_cell + 3.7, y as f32 / grit_cell + 3.7, tf, SEED ^ 0x00C0_FFEE) - 0.5;
                    val += (ni * cp + nq * sp) * grit * 0.012;
                }
                val
            } else {
                0.0
            };
        }
    });

    // ---------------- FM transport ----------------
    let tear_drive = sig_tear * sig_tear * sig_tear;
    let tear_e0 = 1.0 - 0.55 * tear_drive;
    let tear_e1 = 1.0 - 0.10 * tear_drive;
    let sync_scan = (120.0 * sx).round() as usize;
    let sparkle_drive = sparkle * sparkle * sparkle;
    let rf_shift = mix(1.0, 5.0, p.get("sparkleBand").clamp(0.0, 1.0)).round() as u32;
    let bend_wet = p.get("circuitBend").clamp(0.0, 1.0);
    let bend_drive = 2f32.powf(p.get("bendBoostDB").clamp(0.0, 48.0) / 6.020599913) * (1440.0 / w as f32).min(1.0);
    let bend_fb = p.get("bendFeedback").clamp(0.0, 0.25);
    let (lo_ire, hi_ire) = {
        let (a, b) = (p.get("bendThresholdLowIRE"), p.get("bendThresholdHighIRE"));
        (a.min(b), a.max(b))
    };
    let sample_mhz = 14.31818 * sx;
    let delay_len = (p.get("bendFeedbackDelay") * sample_mhz).round().clamp(0.0, 255.0) as usize;
    let motion01 = (p.get("bendMotion") * 0.5 + 0.5).clamp(0.0, 1.0);
    let bandwidth_hz = 2f32.powf(mix(0.08f32.log2(), 8f32.log2(), motion01));
    let field_rate = (vfps * 2.0).max(1.0);
    let fm_carrier = 0.155 / sx;
    let fm_dev = 0.050 / sx;
    let inv_sx = 1.0 / sx;

    let mut recovered = vec![[0.0f32; 2]; rw * lines];
    recovered.par_chunks_mut(rw).enumerate().for_each(|(line, out)| {
        let l = line as u32;
        let wrow = &wave[line * rw..(line + 1) * rw];
        let read = |x: i64| wrow[x.clamp(0, rw as i64 - 1) as usize];
        let mut phase = hash(l, tf, 41, SEED) * TAU;
        let mut prev = (phase.cos(), phase.sin());
        let line_fault = hash(l, tf, 43, SEED);
        let sync_fault = if tear_e0 < tear_e1 { smoothstep(tear_e0, tear_e1, line_fault) } else { 0.0 };
        let mut edge_shift = (hash(l, tf, 47, SEED) - 0.5) * 42.0 * sig_jitter * sx;
        let bottom = smoothstep(450.0 * sy, 505.0 * sy, line as f32);
        edge_shift += (hash(l, tf, 49, SEED) - 0.5) * 52.0 * sig_head * bottom * sx;
        let head = bottom * sig_head.clamp(0.0, 1.0);
        let head_slip = (hash(l, tf, 50, SEED) - 0.5) * 12.0 * head * sx;
        let has_drop = hash(l, tf, 53, SEED) < sig_drop * sig_drop * 0.24;
        let drop_start = (110.0 + 720.0 * hash(l, tf, 59, SEED)) * sx;
        let drop_len = (8.0 + 230.0 * hash(l, tf, 61, SEED)) * sx;
        let tbc_dir = if hash(l, tf, 67, SEED) < 0.5 { -1.0 } else { 1.0 };
        let tbc_off = tbc_dir * (24.0 + 170.0 * hash(l, tf, 71, SEED)) * sx;
        // circuit bend: band-pass whose centre wanders with an RC control voltage
        let (mut b0, mut b2, mut a1, mut a2) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
        if bend_wet > 0.0 {
            let line_time = time.max(0.0) + line as f32 / (field_rate * lines as f32);
            let control = rc_contact(line_time, bandwidth_hz, SEED ^ 0xB37D);
            let centre = (p.get("bendFrequencyMHz") * (1.0 + 0.08 * control)).clamp(0.05, sample_mhz * 0.45);
            let omega = TAU * centre / sample_mhz;
            let q = p.get("bendQ").clamp(0.25, 30.0);
            let alpha = omega.sin() / (2.0 * q);
            let a0 = 1.0 + alpha;
            b0 = alpha / a0;
            b2 = -alpha / a0;
            a1 = -2.0 * omega.cos() / a0;
            a2 = (1.0 - alpha) / a0;
        }
        let (mut x1, mut x2, mut y1, mut y2) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
        let mut delay = [0.0f32; 256];
        let mut di = 0usize;
        for x in 0..rw {
            let source_x = (x as f32 - edge_shift).round().clamp(0.0, rw as f32 - 1.0) as i64;
            let mut base = read(source_x);
            let mut drop_run = 0.0;
            if has_drop {
                let q = (x as f32 - drop_start) / drop_len;
                drop_run = smoothstep(0.0, 0.04, q) * (1.0 - smoothstep(0.55, 1.0, q));
            }
            if drop_run > 0.0 {
                base = match drop_style {
                    0 => mix(base, 0.825, drop_run * 0.92),
                    1 => mix(base, 0.075, drop_run),
                    2 => {
                        let held = wave[line.saturating_sub(1) * rw + source_x as usize];
                        mix(base, held, drop_run)
                    }
                    3 => {
                        let d1 = (1.0 * sx).round() as i64;
                        let d2 = (2.0 * sx).round() as i64;
                        let luma = (read(source_x - d2) + read(source_x - d1) + base + read(source_x + d1) + read(source_x + d2)) * 0.2;
                        mix(base, luma, drop_run)
                    }
                    _ => mix(base, read((source_x as f32 - tbc_off * drop_run).round() as i64), drop_run),
                };
            }
            if head > 0.0 {
                let hx = ((x as f32 * inv_sx) as u32) >> 4;
                let segment = hash(hx, l, tf, SEED ^ 0xB41D);
                let seg_slip = (segment - 0.5) * 7.0 * head * sx;
                let slipped = read((source_x as f32 + head_slip + seg_slip).round() as i64);
                base = mix(base, slipped, head * (0.28 + 0.58 * segment));
                let unstable = TAU * x as f32 / spc + raster_phase(line, clock) + TAU * (segment + hash(l, tf, 51, SEED));
                base += unstable.cos() * head * (0.18 + 0.34 * segment);
                let burst_loss = smoothstep(0.72, 0.98, segment) * head;
                let rf = hash(((x as f32 * inv_sx) as u32) >> 2, l, tf, SEED ^ 0xC731);
                base = mix(base, -0.10 + 0.95 * rf, burst_loss * 0.62);
            }
            if sig_ghost != 0.0 {
                let mag = sig_ghost.abs();
                let d = ((8.0 + 48.0 * mag) * sx).round() as i64;
                let echo = read(source_x - sig_ghost.signum() as i64 * d);
                base = (base + echo * 0.34 * mag) / (1.0 + 0.34 * mag);
            }
            if sig_hum > 0.0 {
                let frac = line as f32 / lines as f32;
                base += (TAU * (frac - time * 0.06)).sin() * 0.055 * sig_hum;
            }
            if bend_wet > 0.0 && x >= ax0 && x < ax1 {
                let ire = (base - 0.075) * (100.0 / 0.75);
                let gate = smoothstep(lo_ire - 2.0, lo_ire + 2.0, ire) * (1.0 - smoothstep(hi_ire - 2.0, hi_ire + 2.0, ire));
                let delayed = if delay_len == 0 { y1 } else { delay[di] };
                let input = (base - 0.075) / 0.75;
                let driven = input * bend_drive + delayed * bend_fb * bend_drive;
                let filtered = b0 * driven + b2 * x2 - a1 * y1 - a2 * y2;
                x2 = x1;
                x1 = driven;
                y2 = y1;
                y1 = filtered;
                let limited = filtered.tanh();
                if delay_len > 0 {
                    delay[di] = limited;
                    di = (di + 1) % delay_len;
                }
                base = mix(base, 0.075 + 0.42 * limited * gate, bend_wet);
            }
            if x < sync_scan && sync_fault > 0.0 {
                base = mix(base, 0.0, sync_fault);
                if hash(x as u32, l, tf, SEED ^ 0x51A9) > 0.995 - 0.08 * tear_drive {
                    base = -0.40;
                }
            }
            if line < vbi_end && sig_roll > 0.0 {
                base = mix(base, 0.0, (sig_roll * 1.8).clamp(0.0, 1.0));
            }
            let mut env = 1.0 - bottom * sig_head * (0.20 + 0.55 * hash(((x as f32 * inv_sx) as u32) >> 3, l, tf, SEED ^ 0xE11));
            if drop_style == 0 && drop_run > 0.0 {
                env *= 1.0 - drop_run * (0.38 + 0.22 * sig_drop);
            }
            let hf = (base - wrow[x.saturating_sub(1)]).abs();
            let marginal = smoothstep(0.25, 0.9, hash(((x as f32 * inv_sx) as u32) >> rf_shift, l, tf, SEED ^ 0xA701) + hf * 0.7);
            env *= 1.0 - marginal * sparkle_drive * 0.88;
            let freq = fm_carrier + fm_dev * (base + 0.40).clamp(0.0, 1.70);
            phase += TAU * freq;
            let mut car = (env * phase.cos(), env * phase.sin());
            let noise_amp = 0.004 + 0.010 * grit + (1.0 - env) * (0.12 + 0.32 * sparkle_drive);
            car.0 += noise_amp * gauss(x as u32, l, tf, SEED ^ 0x91);
            car.1 += noise_amp * gauss(x as u32, l, tf, SEED ^ 0xD3);
            let len_p = (prev.0 * prev.0 + prev.1 * prev.1).sqrt();
            let len_c = (car.0 * car.0 + car.1 * car.1).sqrt();
            let denom = (len_p * len_c).max(1e-5);
            let cross = prev.0 * car.1 - prev.1 * car.0;
            let dot = prev.0 * car.0 + prev.1 * car.1;
            let delta = (cross / denom).atan2(dot / denom) / TAU;
            let out_sig = (delta - fm_carrier) / fm_dev - 0.40;
            let conf = (len_c / (len_c + noise_amp * 3.0)).clamp(0.0, 1.0);
            out[x] = [out_sig, conf];
            prev = car;
        }
    });

    // ---------------- receiver measure ----------------
    let mb0 = 4 * (19.0 * sx).round() as usize;
    let mb1 = mb0 + 4 * (9.0 * sx).round() as usize;
    let measures: Vec<[f32; 4]> = (0..lines)
        .into_par_iter()
        .map(|line| {
            let row = &recovered[line * rw..(line + 1) * rw];
            let mut edge = -1i32;
            let mut tip = 0.0f32;
            for (x, s) in row.iter().enumerate().take(sync_scan) {
                tip = tip.min(s[0]);
                if edge < 0 && s[0] < -0.18 && s[1] > 0.12 {
                    edge = x as i32;
                }
            }
            let (mut sc, mut ss, mut bconf) = (0.0f32, 0.0f32, 0.0f32);
            for x in mb0..mb1.min(rw) {
                sc += row[x][0] * cos_x[x];
                ss += row[x][0] * sin_x[x];
                bconf += row[x][1];
            }
            let measured = (-ss).atan2(sc);
            let n = (mb1 - mb0) as f32;
            let (amp, conf01) = if r.sd { ((sc * sc + ss * ss).sqrt() / 18.0, (bconf / 36.0).clamp(0.0, 1.0)) } else { ((sc * sc + ss * ss).sqrt() / (0.5 * n), (bconf / n).clamp(0.0, 1.0)) };
            let lock = smoothstep(0.018, 0.12, amp) * conf01;
            let mut carrier: f32 = 0.0;
            let mut cnt: f32 = 0.0;
            let mut x = ax0;
            while x < ax1 {
                carrier += row[x][1];
                cnt += 1.0;
                x += 8;
            }
            carrier /= cnt.max(1.0);
            [edge as f32, (-tip).max(0.0), measured, lock.min(carrier)]
        })
        .collect();

    // ---------------- receiver analyze (sequential PLLs) ----------------
    let mut line_state = vec![[0.0f32; 4]; lines]; // h shift, agc, burst phase, lock
    let vband = ((9.0 * sy).round() as usize).max(1);
    let h_clamp = 90.0 * sx;
    let mut vertical_evidence = 0.0;
    for (line, m) in measures.iter().enumerate() {
        let edge = m[0] as i32;
        let depth = m[1];
        let mut sync_lock = smoothstep(0.10, 0.34, depth);
        if edge >= 0 {
            let err = edge as f32 - st.h_phase;
            st.h_velocity = st.h_velocity * 0.82 + err * 0.018;
            st.h_phase += st.h_velocity + err * 0.16;
        } else {
            st.h_phase += st.h_velocity;
            st.h_velocity *= 1.008;
            sync_lock = 0.0;
        }
        st.h_phase = st.h_phase.clamp(-h_clamp, h_clamp);
        let new_lock = m[3];
        let perr = (m[2] - st.burst_phase).sin().atan2((m[2] - st.burst_phase).cos());
        st.burst_phase += perr * mix(0.035, 1.0, new_lock);
        st.burst_lock = mix(st.burst_lock * 0.985, new_lock, 0.24);
        st.carrier_lock = mix(st.carrier_lock, m[3], 0.08);
        let target_agc = (0.40 / depth.max(0.08)).clamp(0.55, 2.8);
        st.agc += (target_agc - st.agc) * (0.025 + 0.08 * sync_lock);
        let lock = sync_lock.min(st.burst_lock.min(st.carrier_lock));
        line_state[line] = [st.h_phase, st.agc, st.burst_phase, lock];
        if line < vband {
            vertical_evidence += depth;
        }
    }
    let v_lock = smoothstep(2.4, 3.4, vertical_evidence * (9.0 / vband as f32));
    if sig_roll > 0.0 && v_lock < 0.9 {
        let n = hash(tf, 9, 71, SEED) - 0.5;
        let target = (1.0 + 38.0 * sig_roll.powf(1.20) + n * (1.0 + 1.5 * sig_roll)) * sy;
        st.v_velocity += (target - st.v_velocity) * 0.10;
        st.v_phase += st.v_velocity;
    } else {
        st.v_velocity *= 0.72;
        st.v_phase *= 0.84;
    }
    st.v_phase %= lines as f32;
    // keep the burst phase bounded between frames
    st.burst_phase = st.burst_phase.sin().atan2(st.burst_phase.cos());

    // ---------------- line decode, one row per active line ----------------
    let taps = (((((4.0 * rw as f32 / 227.5).round() as i32) + 1) / 2) * 2).clamp(4, 64) as usize;
    let hann: Vec<f32> = (0..taps).map(|k| 0.5 - 0.5 * (TAU * (k as f32 + 0.5) / taps as f32).cos()).collect();
    let hann_sum: f32 = hann.iter().sum();
    let decode_gain = if r.source { 1.0 + 0.22 * ((r.aw / 1440.0).max(1.0) - 1.0) } else { 1.0 };
    // demodulate each active line at each output column: (Y, I, Q, confidence)
    let n_active = ay1 - ay0;
    let demod: Vec<Vec<[f32; 4]>> = (0..n_active)
        .into_par_iter()
        .map(|li| {
            let line = ay0 + li;
            let rx = line_state[line];
            let row = &recovered[line * rw..(line + 1) * rw];
            let off = rx[2] + drift * mix(0.25, 1.0, 1.0 - rx[3]);
            let (so, co) = off.sin_cos();
            (0..w)
                .map(|px| {
                    let ax = r.ax0 + (px as f32 + 0.5) / w as f32 * r.aw + rx[0];
                    let cf = ax.clamp(r.ax0 + (taps / 2) as f32, r.ax0 + r.aw - 1.0 - (taps / 2) as f32);
                    let ci = cf.floor() as i64;
                    let fr = cf - ci as f32;
                    let mut acc = [0.0f32; 4];
                    for wnd in 0..2i64 {
                        let c0 = ci + wnd;
                        let (mut sy_, mut sc, mut ss, mut conf) = (0.0, 0.0, 0.0, 0.0);
                        for (k, wk) in hann.iter().enumerate() {
                            let sxi = (c0 + k as i64 - (taps / 2) as i64).clamp(0, rw as i64 - 1) as usize;
                            let s = row[sxi];
                            let wire = s[0] * rx[1] * decode_gain;
                            sy_ += wk * wire;
                            sc += wk * wire * cos_x[sxi];
                            ss += wk * wire * sin_x[sxi];
                            conf += wk * s[1];
                        }
                        // rotate the line's burst offset in: cos(a+b), sin(a+b)
                        let si = sc * co - ss * so;
                        let sq = ss * co + sc * so;
                        let wl = if wnd == 0 { 1.0 - fr } else { fr };
                        acc[0] += wl * (sy_ / hann_sum - 0.075) / 0.75;
                        acc[1] += wl * (2.0 * si / hann_sum) / 0.35;
                        acc[2] += wl * (2.0 * sq / hann_sum) / 0.35;
                        acc[3] += wl * conf / hann_sum;
                    }
                    acc
                })
                .collect()
        })
        .collect();
    let comb_amt = 1.0 - chroma_bleed;
    let decoded: Vec<Vec<[f32; 3]>> = (0..n_active)
        .into_par_iter()
        .map(|li| {
            let lock = line_state[ay0 + li][3];
            let prev = &demod[li.saturating_sub(1)];
            demod[li]
                .iter()
                .enumerate()
                .map(|(px, d)| {
                    // comb with the line above (its own demodulation, reused)
                    let i = mix(d[1], 0.5 * (d[1] + prev[px][1]), comb_amt);
                    let q = mix(d[2], 0.5 * (d[2] + prev[px][2]), comb_amt);
                    let color_lock = mix(0.32, 1.0, (lock * d[3]).clamp(0.0, 1.0));
                    let mut c = rgb([d[0] * burn, i * chroma_gain * color_lock, q * chroma_gain * color_lock]);
                    const KNEE: f32 = 0.82;
                    for v in c.iter_mut() {
                        if *v > KNEE {
                            *v = KNEE + (1.0 - KNEE) * (1.0 - (-(*v - KNEE) / (1.0 - KNEE)).exp());
                        }
                    }
                    c
                })
                .collect()
        })
        .collect();

    // ---------------- receiver decode: vertical hold and beam reconstruction ----------------
    let beam_sigma = 0.95 + 0.85 * soften;
    let inv_s2 = 1.0 / (beam_sigma * beam_sigma).max(1e-6);
    let field_parity = ((clock.max(0.0) * 2.0) as u32 & 1) as i64;
    let (y_lo, y_hi) = (ay0 as i64, ay1 as i64 - 1);
    let mut out = Img::new(w, h);
    out.px.par_chunks_mut(w).enumerate().for_each(|(py, row)| {
        let vertical = (py as f32 + 0.5) / h as f32;
        let active_line = r.ay0 + vertical * r.ah + st.v_phase;
        let mut li = active_line.floor() as i64;
        if !r.source {
            li = (li & !1) | field_parity;
        }
        li = li.rem_euclid(lines as i64);
        if li < y_lo || li > y_hi {
            let n = recovered[li as usize * rw + ax0][0];
            let blank = (0.018 + n * 0.035).clamp(0.0, 0.08);
            for o in row.iter_mut() {
                *o = [blank; 3];
            }
            return;
        }
        let cell_centre = 2.0 * (active_line * 0.5).floor() + 1.0;
        let d_cell = active_line - cell_centre;
        let w_up = (-2.0 * (1.0 - d_cell) * inv_s2).exp();
        let w_dn = (-2.0 * (1.0 + d_cell) * inv_s2).exp();
        let r0 = &decoded[(li - y_lo) as usize];
        let up = (!r.source && w_up > 0.02).then(|| &decoded[((li + 2).clamp(y_lo, y_hi) - y_lo) as usize]);
        let dn = (!r.source && w_dn > 0.02).then(|| &decoded[((li - 2).clamp(y_lo, y_hi) - y_lo) as usize]);
        for (px, o) in row.iter_mut().enumerate() {
            let mut c = r0[px];
            let mut wsum = 1.0;
            if let Some(u) = up {
                for k in 0..3 {
                    c[k] += w_up * u[px][k];
                }
                wsum += w_up;
            }
            if let Some(d) = dn {
                for k in 0..3 {
                    c[k] += w_dn * d[px][k];
                }
                wsum += w_dn;
            }
            let mut v = [c[0] / wsum, c[1] / wsum, c[2] / wsum];
            if clean > 0.0 {
                let raw = src.px[py * w + px];
                for k in 0..3 {
                    v[k] = mix(v[k], raw[k], clean);
                }
            }
            *o = [v[0].clamp(0.0, 1.0), v[1].clamp(0.0, 1.0), v[2].clamp(0.0, 1.0)];
        }
    });
    out
}
