//! The window, laid out like the original: a top bar of switches and knobs, the source and
//! presets (Easy) or module bank (Advanced) on the left, the preview with its transport in the
//! middle, and detail controls (Easy) or global knobs (Advanced) beside the export box below.

use crate::fx::{self, FrameTime, Img, Renderer};
use crate::media::{self, Decoder, ExportJob, Format, LiveError, LiveFeed, LiveInput, Recorder, Source, Take};
use crate::params::{self, Category, DEFS, Group, Kind, Params, Preset, Tier};
use crate::store;
use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontFamily, FontId, Id, Pos2, Rect, Response, Sense, Shape, Stroke, StrokeKind, Vec2, pos2, vec2,
};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Instant;

// ---- palette, sampled from pictures of the original ----
const BG: Color32 = Color32::from_rgb(0x12, 0x15, 0x18);
const PANEL: Color32 = Color32::from_rgb(0x19, 0x1d, 0x20);
const CARD: Color32 = Color32::from_rgb(0x1c, 0x20, 0x24);
const CARD_EDGE: Color32 = Color32::from_rgb(0x2a, 0x2e, 0x33);
const KEY: Color32 = Color32::from_rgb(0x1f, 0x22, 0x26);
const KEY_EDGE: Color32 = Color32::from_rgb(0x3a, 0x3e, 0x44);
const BEZEL: Color32 = Color32::from_rgb(0x0c, 0x0e, 0x10);
const SCREEN_BLACK: Color32 = Color32::from_rgb(0x05, 0x03, 0x0b);
const TEXT: Color32 = Color32::from_rgb(0xdc, 0xdf, 0xe3);
const TEXT_DIM: Color32 = Color32::from_rgb(0x8b, 0x91, 0x98);
const TEXT_FAINT: Color32 = Color32::from_rgb(0x55, 0x5a, 0x60);
const BLUE: Color32 = Color32::from_rgb(0x3d, 0x93, 0xec);
const BLUE_TEXT: Color32 = Color32::from_rgb(0x63, 0xb8, 0xff);
const SEL_FILL: Color32 = Color32::from_rgb(0x1d, 0x2e, 0x3a);
const GREEN_LIT: Color32 = Color32::from_rgb(0x4f, 0x80, 0x55);
const GREEN_LED: Color32 = Color32::from_rgb(0x6c, 0xe0, 0x6a);
const GREEN_TEXT: Color32 = Color32::from_rgb(0x6a, 0xc0, 0x8b);
const RED: Color32 = Color32::from_rgb(0xd0, 0x33, 0x2c);
const AMBER: Color32 = Color32::from_rgb(0xff, 0xae, 0x3c);

const TOP_H: f32 = 88.0;
const LEFT_W: f32 = 390.0;
const BOTTOM_H: f32 = 196.0;

fn gothic(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("gothic".into()))
}

fn mono(size: f32) -> FontId {
    FontId::monospace(size)
}

fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "gothic".into(),
        Arc::new(egui::FontData::from_static(include_bytes!("../assets/fonts/SpecialGothicExpandedOne-Regular.ttf"))),
    );
    fonts.font_data.insert("plex".into(), Arc::new(egui::FontData::from_static(include_bytes!("../assets/fonts/IBMPlexMono-SemiBold.ttf"))));
    let mut gothic_family = vec!["gothic".to_string()];
    gothic_family.extend(fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default());
    fonts.families.insert(FontFamily::Name("gothic".into()), gothic_family);
    fonts.families.entry(FontFamily::Monospace).or_default().insert(0, "plex".into());
    ctx.set_fonts(fonts);
}

struct Job {
    src: Arc<Img>,
    params: Params,
    ft: FrameTime,
    out: (usize, usize),
    preview_scale: f32,
    reset: bool,
}

/// Renders the preview on its own thread so the window stays responsive.
struct Worker {
    tx: Sender<Job>,
    result: Arc<Mutex<Option<Img>>>,
}

impl Worker {
    fn new(ctx: egui::Context) -> Worker {
        let (tx, rx): (Sender<Job>, Receiver<Job>) = channel();
        let result = Arc::new(Mutex::new(None));
        let out = result.clone();
        std::thread::spawn(move || {
            let mut renderer = Renderer::new();
            while let Ok(mut job) = rx.recv() {
                let mut reset = job.reset;
                while let Ok(newer) = rx.try_recv() {
                    reset |= newer.reset;
                    job = newer;
                }
                if reset {
                    renderer.reset();
                }
                let img = renderer.render(&job.src, &job.params, &job.ft, job.out.0, job.out.1, job.preview_scale);
                *out.lock().unwrap() = Some(img);
                ctx.request_repaint();
            }
        });
        Worker { tx, result }
    }
}

#[derive(PartialEq, Clone, Copy)]
enum Mode {
    Easy,
    Advanced,
}

#[derive(PartialEq, Clone, Copy)]
enum View {
    Fit,
    Actual,
    Double,
}

impl View {
    fn label(self) -> &'static str {
        match self {
            View::Fit => "FIT",
            View::Actual => "100%",
            View::Double => "200%",
        }
    }
}

pub struct App {
    ctx: egui::Context,
    params: Params,
    mode: Mode,
    presets: Vec<Preset>,
    active_preset: Option<String>,
    source: Source,
    /// true until the person opens a file: the screen shows the idle card
    idle: bool,
    source_name: String,
    signal_src: Arc<Img>,
    decoder: Option<Decoder>,
    video_t: f32,
    playing: bool,
    play_anchor: Option<(Instant, f32)>,
    clock: Instant,
    worker: Worker,
    texture: Option<egui::TextureHandle>,
    clean_texture: Option<egui::TextureHandle>,
    compare: bool,
    power: bool,
    view: View,
    dirty_reset: bool,
    thumbs: Arc<Mutex<Vec<Option<egui::ColorImage>>>>,
    thumb_textures: Vec<Option<egui::TextureHandle>>,
    thumbs_for: u64,
    source_gen: u64,
    open_modules: Vec<Group>,
    detail_group: Group,
    format: Format,
    still_seconds: f32,
    export: Option<ExportJob>,
    status: String,
    naming_preset: bool,
    new_preset_name: String,
    rec_armed: bool,
    take: Take,
    fullscreen: bool,
    screenshot: Option<PathBuf>,
    frames_drawn: u32,
    shot_taken: Arc<Mutex<Option<bool>>>,
    ffmpeg: bool,
    live: Option<LiveFeed>,
    recorder: Option<Recorder>,
    show_live: bool,
    cameras: Vec<(String, String)>,
    stream_url: String,
    /// a live source being opened in the background
    connecting: Option<(LiveInput, Receiver<Result<LiveFeed, LiveError>>, Instant)>,
    /// why the last live source failed, with the settings page that can fix it
    live_error: Option<(String, Option<&'static str>)>,
    show_about: bool,
    /// the session as last written to disk, so it is only saved when something changed
    saved_session: String,
    session_checked: Instant,
}

/// The dark screen shown before a file is opened (the look still runs over it).
fn idle_card(w: usize, h: usize) -> Img {
    let mut img = Img::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let u = x as f32 / w as f32 - 0.5;
            let v = y as f32 / h as f32 - 0.45;
            let r = (u * u * 1.4 + v * v * 2.2).sqrt();
            let k = (1.0 - r * 1.5).clamp(0.0, 1.0);
            img.px[y * w + x] = [0.015 + 0.03 * k, 0.02 + 0.045 * k, 0.04 + 0.10 * k];
        }
    }
    img
}

/// A neutral built-in landscape used for preset thumbnails before a file is opened.
fn test_card(w: usize, h: usize) -> Img {
    let mut img = Img::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let u = x as f32 / w as f32;
            let v = y as f32 / h as f32;
            let peak = 0.32 + 0.55 * (2.0 * (u - 0.5)).abs().powf(1.1);
            let snow = 0.32 + 0.12 * (2.0 * (u - 0.5)).abs();
            let c = if v > 0.82 {
                [0.10 + 0.08 * (u * 40.0).sin().abs(), 0.20, 0.12]
            } else if v > peak {
                let shade = if u < 0.5 { 0.75 } else { 1.0 };
                if v < snow {
                    [0.85 * shade, 0.86 * shade, 0.9 * shade]
                } else {
                    [0.36 * shade, 0.30 * shade, 0.30 * shade]
                }
            } else {
                [0.30 + 0.35 * v, 0.55 + 0.25 * v, 0.85 - 0.1 * v]
            };
            img.px[y * w + x] = c;
        }
    }
    img
}

fn fmt_time(t: f32) -> String {
    let t = t.max(0.0) as u32;
    format!("{}:{:02}", t / 60, t % 60)
}

// ------------------------------------------------------------------ drawing helpers

fn rr(r: f32) -> CornerRadius {
    CornerRadius::same(r as u8)
}

fn panel(p: &egui::Painter, rect: Rect, fill: Color32, edge: Color32, radius: f32) {
    p.rect(rect, rr(radius), fill, Stroke::new(1.0, edge), StrokeKind::Inside);
}

/// A recessed bezel that holds keys.
fn bezel(p: &egui::Painter, rect: Rect) {
    p.rect(rect.expand(1.0), rr(10.0), Color32::from_rgb(0x2c, 0x30, 0x35), Stroke::NONE, StrokeKind::Inside);
    p.rect(rect, rr(9.0), BEZEL, Stroke::new(1.0, Color32::from_rgb(0x07, 0x08, 0x09)), StrokeKind::Inside);
}

/// A hardware key with an optional LED above its legend.
fn key_at(ui: &mut egui::Ui, rect: Rect, id: Id, label: &str, selected: bool, led: bool, font: FontId) -> Response {
    let resp = ui.interact(rect, id, Sense::click());
    let p = ui.painter();
    let r = if resp.is_pointer_button_down_on() { rect.shrink(0.5) } else { rect };
    let fill = if resp.hovered() { Color32::from_rgb(0x27, 0x2b, 0x30) } else { KEY };
    p.rect(r, rr(5.0), fill, Stroke::new(1.0, if selected { BLUE } else { KEY_EDGE }), StrokeKind::Inside);
    p.line_segment([r.left_top() + vec2(5.0, 1.5), r.right_top() + vec2(-5.0, 1.5)], Stroke::new(1.0, Color32::from_white_alpha(14)));
    let mut text_c = r.center();
    if led {
        let lp = pos2(r.center().x, r.top() + r.height() * 0.28);
        p.circle_filled(lp, 2.2, if selected { GREEN_LED } else { Color32::from_rgb(0x30, 0x33, 0x36) });
        text_c.y += r.height() * 0.12;
    }
    p.text(text_c, Align2::CENTER_CENTER, label, font, if selected || resp.hovered() { TEXT } else { TEXT_DIM });
    resp
}

/// The big green-lit two-way switch (EASY / ADVANCED).
fn dual_switch(ui: &mut egui::Ui, rect: Rect, left_on: bool) -> Option<bool> {
    let p = ui.painter().clone();
    bezel(&p, rect);
    let inner = rect.shrink(8.0);
    let half = (inner.width() - 4.0) / 2.0;
    let mut out = None;
    for (i, on) in [(0, left_on), (1, !left_on)] {
        let r = Rect::from_min_size(pos2(inner.left() + i as f32 * (half + 4.0), inner.top()), vec2(half, inner.height()));
        let resp = ui.interact(r, Id::NULL.with(("dual", i)), Sense::click());
        let fill = if on { GREEN_LIT } else { Color32::from_rgb(0x1b, 0x1d, 0x1f) };
        p.rect(r, rr(6.0), fill, Stroke::new(1.0, Color32::from_rgb(0x05, 0x06, 0x06)), StrokeKind::Inside);
        p.line_segment([r.left_top() + vec2(6.0, 1.5), r.right_top() + vec2(-6.0, 1.5)], Stroke::new(1.0, Color32::from_white_alpha(if on { 50 } else { 12 })));
        p.circle_filled(r.center(), 2.0, if on { GREEN_LED } else { Color32::from_rgb(0x0a, 0x0a, 0x0a) });
        if resp.clicked() {
            out = Some(i == 0);
        }
    }
    out
}

/// A knurled black knob with a blue value arc. Drag up/down, double-click to reset.
#[allow(clippy::too_many_arguments)]
fn knob_at(ui: &mut egui::Ui, center: Pos2, radius: f32, id: Id, value: &mut f32, min: f32, max: f32, default: f32, arc: bool) -> bool {
    let rect = Rect::from_center_size(center, Vec2::splat(radius * 2.0 + 10.0));
    let resp = ui.interact(rect, id, Sense::click_and_drag());
    let mut changed = false;
    if resp.dragged() {
        let d = -resp.drag_delta().y + resp.drag_delta().x * 0.5;
        let fine = if ui.input(|i| i.modifiers.shift) { 0.2 } else { 1.0 };
        *value = (*value + d / 200.0 * (max - min) * fine).clamp(min, max);
        changed = true;
    }
    if resp.double_clicked() {
        *value = default;
        changed = true;
    }
    let p = ui.painter();
    let t = ((*value - min) / (max - min)).clamp(0.0, 1.0);
    let a0 = std::f32::consts::PI * 0.75;
    let sweep = std::f32::consts::PI * 1.5;
    if arc {
        let ring = radius + 4.0;
        let pts = |from: f32, to: f32| -> Vec<Pos2> { (0..=40).map(|i| center + Vec2::angled(from + (to - from) * i as f32 / 40.0) * ring).collect() };
        p.add(Shape::line(pts(a0, a0 + sweep), Stroke::new(2.5, Color32::from_rgb(0x22, 0x26, 0x2b))));
        if t > 0.002 {
            p.add(Shape::line(pts(a0, a0 + sweep * t), Stroke::new(2.5, if resp.dragged() { BLUE_TEXT } else { BLUE })));
        }
    }
    p.circle_filled(center + vec2(0.0, 2.0), radius, Color32::from_black_alpha(140));
    p.circle_filled(center, radius, Color32::from_rgb(0x15, 0x16, 0x18));
    // knurling
    let n = (radius * 1.6) as usize;
    for i in 0..n {
        let a = i as f32 / n as f32 * std::f32::consts::TAU;
        p.line_segment([center + Vec2::angled(a) * (radius - 2.5), center + Vec2::angled(a) * radius], Stroke::new(1.0, Color32::from_rgb(0x2a, 0x2c, 0x2f)));
    }
    p.circle_filled(center, radius - 3.0, Color32::from_rgb(0x1c, 0x1e, 0x21));
    p.circle_stroke(center, radius, Stroke::new(1.0, Color32::from_rgb(0x34, 0x37, 0x3b)));
    let a = a0 + sweep * t;
    p.line_segment([center + Vec2::angled(a) * (radius * 0.35), center + Vec2::angled(a) * (radius - 3.5)], Stroke::new(2.2, Color32::from_rgb(0xee, 0xee, 0xec)));
    if resp.hovered() {
        resp.on_hover_text("Drag to turn · Shift for fine · Double-click to reset");
    }
    changed
}

/// A labelled horizontal slider like the original's detail controls.
#[allow(clippy::too_many_arguments)]
fn slider_at(ui: &mut egui::Ui, rect: Rect, id: Id, label: &str, value: &mut f32, min: f32, max: f32, default: f32) -> bool {
    let p = ui.painter().clone();
    p.text(rect.left_top() + vec2(0.0, 6.0), Align2::LEFT_CENTER, label, gothic(8.5), TEXT);
    let shown = if max - min > 20.0 { format!("{:.0}", value) } else { format!("{:.2}", value) };
    p.text(rect.right_top() + vec2(0.0, 6.0), Align2::RIGHT_CENTER, shown, mono(11.0), BLUE_TEXT);
    let track = Rect::from_min_max(pos2(rect.left() + 8.0, rect.top() + 21.0), pos2(rect.right() - 8.0, rect.top() + 23.0));
    let hit = Rect::from_min_max(pos2(rect.left(), rect.top() + 12.0), pos2(rect.right(), rect.top() + 32.0));
    let resp = ui.interact(hit, id, Sense::click_and_drag());
    let mut changed = false;
    if resp.dragged() || resp.clicked() {
        if let Some(pos) = resp.interact_pointer_pos() {
            let f = ((pos.x - track.left()) / track.width()).clamp(0.0, 1.0);
            *value = min + f * (max - min);
            changed = true;
        }
    }
    if resp.double_clicked() {
        *value = default;
        changed = true;
    }
    let t = ((*value - min) / (max - min)).clamp(0.0, 1.0);
    let x = track.left() + track.width() * t;
    p.rect_filled(track, rr(1.0), Color32::from_rgb(0x30, 0x35, 0x3b));
    p.rect_filled(Rect::from_min_max(track.min, pos2(x, track.bottom())), rr(1.0), BLUE);
    p.circle_filled(pos2(x, track.center().y), 7.0, Color32::from_rgb(0x0e, 0x10, 0x12));
    p.circle_filled(pos2(x, track.center().y), 5.5, Color32::from_rgb(0xd6, 0xd8, 0xdb));
    changed
}

/// The little coloured mark beside each module name.
fn group_icon(p: &egui::Painter, c: Pos2, g: Group) {
    match g {
        Group::ColorSignal => {
            let cols = [Color32::from_rgb(0xe8, 0xa5, 0x2a), Color32::from_rgb(0xd8, 0x8f, 0x1e), Color32::from_rgb(0xf0, 0xc0, 0x40)];
            for (i, h) in [11.0, 15.0, 9.0].iter().enumerate() {
                let x = c.x - 7.0 + i as f32 * 6.0;
                p.rect_filled(Rect::from_min_max(pos2(x, c.y + 7.0 - h), pos2(x + 3.5, c.y + 7.0)), rr(1.0), cols[i]);
            }
        }
        Group::CircuitBend => {
            let col = Color32::from_rgb(0x3d, 0x7f, 0xe0);
            p.add(Shape::convex_polygon(vec![c + vec2(3.0, -9.0), c + vec2(-5.0, 2.0), c + vec2(0.5, 2.0)], col, Stroke::NONE));
            p.add(Shape::convex_polygon(vec![c + vec2(-0.5, -2.0), c + vec2(5.0, -2.0), c + vec2(-3.0, 9.0)], col, Stroke::NONE));
        }
        Group::Damage => {
            let cols = [RED, Color32::from_rgb(0xe0, 0x6a, 0x30), Color32::from_rgb(0xe0, 0x6a, 0x30), RED];
            for (i, col) in cols.iter().enumerate() {
                let o = vec2((i % 2) as f32 * 8.0 - 8.0, (i / 2) as f32 * 8.0 - 8.0);
                p.rect_filled(Rect::from_min_size(c + o, vec2(7.0, 7.0)), rr(1.0), *col);
            }
        }
        Group::Noise => {
            let g = Color32::from_rgb(0x5f, 0xb8, 0x56);
            for (dx, dy) in [(-7.0, -6.0), (2.0, -8.0), (7.0, -2.0), (-3.0, 0.0), (-8.0, 6.0), (4.0, 7.0), (0.0, 4.0)] {
                p.rect_filled(Rect::from_center_size(c + vec2(dx, dy), vec2(3.0, 3.0)), rr(0.0), g);
            }
        }
        Group::Waves => {
            let pts: Vec<Pos2> = (0..=20)
                .map(|i| {
                    let x = -9.0 + i as f32 * 0.9;
                    c + vec2(x, (x * 0.55).sin() * 5.0)
                })
                .collect();
            p.add(Shape::line(pts, Stroke::new(2.0, Color32::from_rgb(0x4a, 0x8f, 0xd8))));
        }
        Group::Screen => {
            let y = Color32::from_rgb(0xe8, 0xb0, 0x2a);
            p.rect(Rect::from_center_size(c, vec2(16.0, 15.0)), CornerRadius { nw: 2, sw: 2, ne: 7, se: 7 }, Color32::TRANSPARENT, Stroke::new(3.0, y), StrokeKind::Inside);
        }
        Group::Capture => {
            let b = Color32::from_rgb(0x3f, 0x8a, 0xb0);
            p.rect_filled(Rect::from_center_size(c + vec2(-2.0, 0.0), vec2(12.0, 11.0)), rr(2.0), b);
            p.add(Shape::convex_polygon(vec![c + vec2(4.0, 0.0), c + vec2(9.0, -5.0), c + vec2(9.0, 5.0)], b, Stroke::NONE));
        }
        Group::Convergence => {
            let r = Color32::from_rgb(0xd8, 0x50, 0x3a);
            p.circle_stroke(c, 6.5, Stroke::new(2.2, r));
            p.circle_filled(c, 2.0, r);
            for v in [vec2(0.0, -10.0), vec2(0.0, 10.0), vec2(-10.0, 0.0), vec2(10.0, 0.0)] {
                p.line_segment([c + v * 0.75, c + v], Stroke::new(2.0, r));
            }
        }
        Group::Optics | Group::Global => {
            let b = Color32::from_rgb(0x6f, 0x8f, 0xc8);
            p.circle_stroke(c, 8.0, Stroke::new(2.0, b));
            for a in 0..5 {
                p.circle_filled(c + Vec2::angled(a as f32 * 1.2566) * 4.0, 1.8, b);
            }
        }
    }
}

/// The wordmark: our own name, set wide and slanted in the same spirit as the original's.
fn wordmark(p: &egui::Painter, left_center: Pos2, size: f32, color: Color32, ghost: bool) -> Rect {
    let mut job = egui::text::LayoutJob::default();
    job.append("KILN", 0.0, egui::TextFormat { font_id: gothic(size), color, italics: true, ..Default::default() });
    let g = p.layout_job(job);
    let pos = left_center - vec2(0.0, g.size().y / 2.0);
    if ghost {
        p.galley(pos + vec2(-2.5, 0.0), g.clone(), Color32::from_rgba_unmultiplied(255, 60, 60, 60));
        p.galley(pos + vec2(2.5, 0.0), g.clone(), Color32::from_rgba_unmultiplied(60, 160, 255, 60));
    }
    let size_v = g.size();
    p.galley(pos, g, color);
    let box_r = Rect::from_min_size(pos2(pos.x + size_v.x + size * 0.3, left_center.y - size * 0.42), vec2(size * 1.55, size * 0.84));
    p.rect_stroke(box_r, rr(size * 0.2), Stroke::new(size * 0.06, color), StrokeKind::Inside);
    p.text(box_r.center(), Align2::CENTER_CENTER, "CRT", FontId::proportional(size * 0.56), color);
    // the subtitle is set in a light face with open letter spacing, as on the original's badge
    let spaced: String = "ANALOG VIDEO EMULATOR".chars().flat_map(|c| [c, '\u{200A}']).collect();
    let sub = p.text(pos2(pos.x + 3.0, left_center.y + size * 0.64), Align2::LEFT_CENTER, spaced, FontId::proportional(size * 0.36), color);
    Rect::from_min_max(pos, pos2(box_r.right(), sub.bottom()))
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, screenshot: Option<PathBuf>, open: Option<PathBuf>) -> App {
        let ctx = cc.egui_ctx.clone();
        install_fonts(&ctx);
        // always the dark rack, whatever the Mac's appearance setting
        ctx.set_theme(egui::ThemePreference::Dark);
        ctx.all_styles_mut(|style| {
            style.visuals = egui::Visuals::dark();
            style.visuals.panel_fill = BG;
            style.visuals.window_fill = PANEL;
            style.visuals.override_text_color = Some(TEXT);
            style.visuals.widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
            style.visuals.widgets.inactive.bg_stroke = Stroke::NONE;
            style.visuals.widgets.hovered.weak_bg_fill = Color32::from_rgb(0x22, 0x26, 0x2a);
        });

        let mut presets = params::factory_presets();
        let loaded = store::Store::default().load();
        presets.extend(loaded.presets);
        let start = presets.iter().find(|p| p.name == "HANDICAM").or_else(|| presets.first()).map(|p| (p.name.clone(), p.params.clone()));
        let (active, params) = match start {
            Some((n, p)) => (Some(n), p),
            None => (None, Params::default()),
        };
        let n = presets.len();
        let mut app = App {
            ctx: ctx.clone(),
            signal_src: Arc::new(Img::new(2, 2)),
            params,
            mode: Mode::Easy,
            presets,
            active_preset: active,
            source: Source::Image { img: Arc::new(idle_card(1280, 720)), path: PathBuf::new() },
            idle: true,
            source_name: "NO SOURCE".into(),
            decoder: None,
            video_t: 0.0,
            playing: false,
            play_anchor: None,
            clock: Instant::now(),
            worker: Worker::new(ctx.clone()),
            texture: None,
            clean_texture: None,
            compare: false,
            power: true,
            view: View::Fit,
            dirty_reset: true,
            thumbs: Arc::new(Mutex::new(vec![None; n])),
            thumb_textures: vec![None; n],
            thumbs_for: u64::MAX,
            source_gen: 0,
            open_modules: Vec::new(),
            detail_group: Group::Damage,
            format: Format::ProRes422,
            still_seconds: 5.0,
            export: None,
            status: loaded.notice.map(String::from).unwrap_or_default(),
            naming_preset: false,
            new_preset_name: String::new(),
            rec_armed: false,
            take: Vec::new(),
            fullscreen: false,
            screenshot,
            frames_drawn: 0,
            shot_taken: Arc::new(Mutex::new(None)),
            ffmpeg: media::ffmpeg_available(),
            live: None,
            recorder: None,
            show_live: false,
            cameras: Vec::new(),
            stream_url: String::new(),
            connecting: None,
            live_error: None,
            show_about: false,
            saved_session: String::new(),
            session_checked: Instant::now(),
        };
        // screenshots always start from the factory look, so they stay comparable
        if app.screenshot.is_none() {
            app.restore_session();
        }
        app.rebuild_signal_source();
        if std::env::args().any(|a| a == "--advanced") {
            app.mode = Mode::Advanced;
        }
        if let Some(p) = open {
            app.open_path(p);
        }
        app
    }

    fn src_dims(&self) -> (usize, usize) {
        match &self.source {
            Source::Image { img, .. } => (img.w, img.h),
            Source::Video { info, .. } => (info.w, info.h),
            Source::Live { .. } => (1280, 720),
        }
    }

    fn preview_dims(&self) -> (usize, usize, f32) {
        let (w, h) = self.src_dims();
        let s = (1280.0 / w as f32).min(720.0 / h as f32).min(1.0);
        (((w as f32 * s).round() as usize).max(2), ((h as f32 * s).round() as usize).max(2), s)
    }

    fn rebuild_signal_source(&mut self) {
        let (w, h) = self.src_dims();
        let (sw, sh) = fx::signal_size(self.params.tier, w, h, 1280);
        match &self.source {
            Source::Image { img, .. } => {
                self.signal_src = Arc::new(img.resized(sw, sh));
                self.decoder = None;
            }
            Source::Video { .. } => self.seek(self.video_t),
            Source::Live { input } => {
                let input = input.clone();
                self.connect_live(input);
            }
        }
        self.source_gen += 1;
        self.dirty_reset = true;
        self.clean_texture = None;
    }

    fn seek(&mut self, t: f32) {
        let Source::Video { path, info } = &self.source else { return };
        let (sw, sh) = fx::signal_size(self.params.tier, info.w, info.h, 1280);
        let t = t.clamp(0.0, (info.duration - 0.05).max(0.0));
        self.decoder = None;
        match Decoder::start(path, info, sw, sh, t) {
            Ok(d) => {
                if let Ok((ft, frame)) = d.rx.recv_timeout(std::time::Duration::from_secs(10)) {
                    self.signal_src = Arc::new(frame);
                    self.video_t = ft;
                } else {
                    self.status = "Could not refresh this preview frame. Seek again or reopen the clip.".into();
                }
                self.decoder = Some(d);
            }
            Err(e) => self.status = e,
        }
        if self.playing {
            self.play_anchor = Some((Instant::now(), self.video_t));
        }
        self.dirty_reset = true;
        self.clean_texture = None;
    }

    fn open_path(&mut self, path: PathBuf) {
        if let Some(rec) = self.recorder.take() {
            let _ = rec.finish();
        }
        self.live = None;
        let name = path.file_name().map(|n| n.to_string_lossy().to_uppercase()).unwrap_or_default();
        if media::is_image(&path) {
            match media::load_image(&path) {
                Ok(img) => {
                    self.source = Source::Image { img: Arc::new(img), path };
                    self.source_name = name;
                    self.idle = false;
                    self.playing = false;
                    self.take.clear();
                    self.status.clear();
                    self.rebuild_signal_source();
                }
                Err(e) => self.status = format!("{e} The current source was kept."),
            }
        } else {
            if !self.ffmpeg {
                self.status = "Opening videos needs ffmpeg (brew install ffmpeg). Pictures work without it.".into();
                return;
            }
            match media::probe(&path) {
                Ok(info) => {
                    self.source = Source::Video { path, info };
                    self.source_name = name;
                    self.idle = false;
                    self.video_t = 0.0;
                    self.playing = false;
                    self.take.clear();
                    self.status.clear();
                    self.rebuild_signal_source();
                }
                Err(e) => self.status = format!("{e} The current source was kept."),
            }
        }
    }

    /// Start opening a live camera or stream on its own thread (a camera can take a while: the
    /// system may ask the person for permission first). `poll_live_connect` picks up the result.
    fn connect_live(&mut self, input: LiveInput) {
        if !self.ffmpeg {
            self.status = "Live input needs ffmpeg (brew install ffmpeg).".into();
            return;
        }
        let (sw, sh) = fx::signal_size(self.params.tier, 1280, 720, 1280);
        let (tx, rx) = channel();
        let what = input.clone();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(LiveFeed::start(&what, sw, sh));
            ctx.request_repaint();
        });
        self.status = format!("CONNECTING TO {}…", input.label());
        self.live_error = None;
        self.connecting = Some((input, rx, Instant::now()));
    }

    fn session_json(&self) -> String {
        serde_json::json!({
            "mode": if self.mode == Mode::Advanced { "advanced" } else { "easy" },
            "preset": self.active_preset,
            "values": self.params.to_values(),
            "format": Format::ALL.iter().position(|f| *f == self.format).unwrap_or(2),
            "view": match self.view { View::Fit => 0, View::Actual => 1, View::Double => 2 },
        })
        .to_string()
    }

    /// Keep the look, mode and export choices for next time.
    fn save_session(&mut self) {
        self.session_checked = Instant::now();
        let text = self.session_json();
        if text != self.saved_session {
            let dir = params::support_dir();
            if std::fs::create_dir_all(&dir).is_ok() && std::fs::write(dir.join("session.json"), &text).is_ok() {
                self.saved_session = text;
            }
        }
    }

    fn restore_session(&mut self) {
        let Ok(text) = std::fs::read_to_string(params::support_dir().join("session.json")) else { return };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else { return };
        if let Some(values) = v["values"].as_object() {
            self.params = Params::from_values(values);
        }
        self.active_preset = v["preset"].as_str().filter(|n| self.presets.iter().any(|p| p.name == *n)).map(str::to_string);
        self.mode = if v["mode"] == "advanced" { Mode::Advanced } else { Mode::Easy };
        if let Some(f) = v["format"].as_u64().and_then(|i| Format::ALL.get(i as usize)) {
            self.format = *f;
        }
        self.view = match v["view"].as_u64() {
            Some(1) => View::Actual,
            Some(2) => View::Double,
            _ => View::Fit,
        };
        self.saved_session = self.session_json();
    }

    fn about_window(&mut self, ctx: &egui::Context) {
        if !self.show_about {
            return;
        }
        let mut open = true;
        egui::Window::new(egui::RichText::new("ABOUT KILN").font(gothic(12.0)))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(380.0)
            .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.label(egui::RichText::new("KILN CRT").font(gothic(18.0)).color(TEXT));
                ui.label(egui::RichText::new(format!("Analog video emulator · version {}", env!("CARGO_PKG_VERSION"))).size(11.0).color(TEXT_DIM));
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new(
                        "Runs your video, pictures or a live camera through a simulated analog signal, \
                         tape and CRT screen, then exports to H.264, HEVC or ProRes.",
                    )
                    .size(11.5),
                );
                ui.add_space(8.0);
                ui.label(egui::RichText::new("SHORTCUTS").font(gothic(8.5)).color(TEXT_DIM));
                ui.label(egui::RichText::new("Space  play / pause\nDrop a file on the window to open it").size(11.0));
                ui.add_space(8.0);
                ui.label(egui::RichText::new("CREDITS").font(gothic(8.5)).color(TEXT_DIM));
                ui.label(
                    egui::RichText::new(
                        "Special Gothic and IBM Plex Mono fonts (SIL Open Font License). \
                         Video decoding and encoding by FFmpeg (ffmpeg.org), included with its licence.",
                    )
                    .size(10.5)
                    .color(TEXT_DIM),
                );
                ui.add_space(6.0);
                if ui.button("SHOW SETTINGS FOLDER").clicked() {
                    let dir = params::support_dir();
                    let _ = std::fs::create_dir_all(&dir);
                    let _ = std::process::Command::new("/usr/bin/open").arg(dir).spawn();
                }
            });
        if !open {
            self.show_about = false;
        }
    }

    fn poll_live_connect(&mut self) {
        let Some((_, rx, _)) = &self.connecting else { return };
        let result = match rx.try_recv() {
            Ok(r) => r,
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => Err(LiveError { message: "The live source stopped while opening.".into(), settings: None }),
        };
        let (input, _, _) = self.connecting.take().unwrap();
        match result {
            Ok(feed) => {
                if let Some(rec) = self.recorder.take() {
                    let _ = rec.finish();
                }
                self.source_name = format!("LIVE · {}", input.label());
                self.source = Source::Live { input };
                self.live = Some(feed);
                self.decoder = None;
                self.idle = false;
                self.playing = false;
                self.take.clear();
                self.status.clear();
                self.source_gen += 1;
                self.dirty_reset = true;
                self.show_live = false;
            }
            Err(e) => {
                self.status = e.message.clone();
                self.live_error = Some((e.message, e.settings));
                self.show_live = true;
            }
        }
    }

    fn toggle_live_recording(&mut self) {
        if let Some(rec) = self.recorder.take() {
            self.status = match rec.finish() {
                Ok(p) => format!("SAVED {}", p.file_name().map(|n| n.to_string_lossy().to_uppercase()).unwrap_or_default()),
                Err(e) => e,
            };
            return;
        }
        let ext = self.format.ext();
        let Some(out) = rfd::FileDialog::new().set_title("Choose where to save the live recording").set_file_name(format!("live-kiln.{ext}")).add_filter(ext, &[ext]).save_file() else {
            return;
        };
        let (pw, ph, _) = self.preview_dims();
        match Recorder::start(out, self.format, pw, ph, 30.0) {
            Ok(r) => {
                self.recorder = Some(r);
                self.status = "RECORDING LIVE".into();
            }
            Err(e) => self.status = e,
        }
    }

    fn live_window(&mut self, ctx: &egui::Context) {
        if !self.show_live {
            return;
        }
        let mut open = true;
        let mut pick: Option<LiveInput> = None;
        let mut cancel = false;
        egui::Window::new(egui::RichText::new("LIVE INPUT").font(gothic(12.0)))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_pos(pos2(40.0, 240.0))
            .default_width(330.0)
            .frame(egui::Frame::new().fill(CARD).stroke(Stroke::new(1.0, KEY_EDGE)).corner_radius(rr(8.0)).inner_margin(egui::Margin::same(14)))
            .show(ctx, |ui| {
                if let Some((input, _, since)) = &self.connecting {
                    ui.label(egui::RichText::new(format!("CONNECTING TO {}…", input.label())).font(gothic(9.5)).color(AMBER));
                    let secs = since.elapsed().as_secs();
                    ui.label(
                        egui::RichText::new(if secs >= 2 {
                            "If macOS asks whether KILN may use the camera, choose Allow."
                        } else {
                            "Asking the device for its picture sizes."
                        })
                        .size(11.0)
                        .color(TEXT_DIM),
                    );
                    if ui.button("CANCEL").clicked() {
                        cancel = true;
                    }
                    ui.add_space(8.0);
                }
                if let Some((msg, settings)) = &self.live_error {
                    ui.label(egui::RichText::new(msg).size(11.0).color(AMBER));
                    if let Some(url) = settings {
                        if ui.button("OPEN PRIVACY SETTINGS").clicked() {
                            media::open_settings(url);
                        }
                    }
                    ui.add_space(8.0);
                }
                ui.label(egui::RichText::new("CAMERAS AND CAPTURE DEVICES").font(gothic(8.5)).color(TEXT_DIM));
                if self.cameras.is_empty() {
                    ui.label(egui::RichText::new("No camera found. Check camera access in System Settings → Privacy.").size(11.0).color(TEXT_DIM));
                }
                for (idx, name) in self.cameras.clone() {
                    if ui.add_sized(vec2(300.0, 28.0), egui::Button::new(egui::RichText::new(name.to_uppercase()).font(gothic(9.5)))).clicked() {
                        pick = Some(LiveInput::Camera { index: idx, name });
                    }
                }
                ui.add_space(8.0);
                ui.label(egui::RichText::new("VIDEO FEED ADDRESS (RTSP, RTMP, HTTP, SRT, UDP)").font(gothic(8.5)).color(TEXT_DIM));
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut self.stream_url).hint_text("rtsp://192.168.1.20/stream").desired_width(220.0).font(mono(11.0)));
                    if ui.button("CONNECT").clicked() && !self.stream_url.trim().is_empty() {
                        pick = Some(LiveInput::Stream(self.stream_url.trim().to_string()));
                    }
                });
                ui.add_space(4.0);
                if ui.button("REFRESH LIST").clicked() {
                    self.cameras = media::list_cameras();
                }
            });
        if !open {
            self.show_live = false;
        }
        if cancel {
            // dropping the receiver lets the opening thread shut its ffmpeg down when it finishes
            self.connecting = None;
            self.status = "Live connection cancelled.".into();
        }
        if let Some(input) = pick {
            if self.connecting.is_none() {
                self.connect_live(input);
            }
        }
    }

    fn choose_file(&mut self) {
        let picked = rfd::FileDialog::new()
            .set_title("Choose a video or image")
            .add_filter("Video or image", &["mp4", "mov", "m4v", "mkv", "avi", "webm", "mpg", "mpeg", "png", "jpg", "jpeg", "bmp", "gif", "tif", "tiff", "webp"])
            .pick_file();
        if let Some(p) = picked {
            self.open_path(p);
        }
    }

    fn apply_preset(&mut self, i: usize) {
        let tier_before = self.params.tier;
        self.params = self.presets[i].params.clone();
        self.active_preset = Some(self.presets[i].name.clone());
        self.dirty_reset = true;
        if self.params.tier != tier_before {
            self.rebuild_signal_source();
        }
    }

    fn step_preset(&mut self, d: isize) {
        let n = self.presets.len() as isize;
        let cur = self.active_preset.as_ref().and_then(|a| self.presets.iter().position(|p| &p.name == a)).map(|i| i as isize).unwrap_or(-1);
        let next = (cur + d).rem_euclid(n) as usize;
        self.apply_preset(next);
    }

    fn set_tier(&mut self, t: Tier) {
        if self.params.tier != t {
            self.params.tier = t;
            self.rebuild_signal_source();
        }
    }

    fn reset_all(&mut self) {
        let tier = self.params.tier;
        self.params = Params::default();
        self.params.tier = tier;
        self.active_preset = None;
        self.dirty_reset = true;
    }

    fn save_preset(&mut self) {
        let name = self.new_preset_name.trim().to_uppercase();
        if name.is_empty() {
            return;
        }
        // saving under an existing name replaces that preset and keeps its id
        let before: Vec<Preset> = self.presets.iter().filter(|x| !x.factory).cloned().collect();
        let id = self.presets.iter().find(|x| !x.factory && x.name == name).map(|x| x.id.clone()).unwrap_or_else(store::new_id);
        self.presets.retain(|x| x.factory || x.name != name);
        self.presets.push(Preset { id, name: name.clone(), params: self.params.clone(), factory: false, category: None, description: None });
        self.new_preset_name.clear();
        self.naming_preset = false;
        match store::Store::default().save(&self.presets) {
            Ok(()) => {
                self.active_preset = Some(name);
                self.status = "PRESET SAVED".into();
            }
            Err(e) => {
                // the bank on disk is unchanged, so the list goes back to match it
                self.presets.retain(|x| x.factory);
                self.presets.extend(before);
                self.status = format!("PRESET SAVE FAILED · {e}");
            }
        }
        self.thumbs_for = u64::MAX;
    }

    fn current_time(&self) -> f32 {
        match &self.source {
            Source::Video { .. } => self.video_t,
            Source::Image { .. } | Source::Live { .. } => self.clock.elapsed().as_secs_f32(),
        }
    }

    fn fps(&self) -> f32 {
        match &self.source {
            Source::Video { info, .. } => info.fps,
            Source::Image { .. } | Source::Live { .. } => 30.0,
        }
    }

    fn toggle_play(&mut self) {
        if let Source::Video { .. } = self.source {
            self.playing = !self.playing;
            if self.playing {
                self.play_anchor = Some((Instant::now(), self.video_t));
            }
        }
    }

    fn tick(&mut self, ctx: &egui::Context) {
        if self.playing {
            if let (Some((at, t0)), Some(dec)) = (self.play_anchor, &self.decoder) {
                let target = t0 + at.elapsed().as_secs_f32();
                loop {
                    match dec.rx.try_recv() {
                        Ok((t, frame)) => {
                            self.signal_src = Arc::new(frame);
                            self.video_t = t;
                            self.clean_texture = None;
                            if t >= target {
                                break;
                            }
                        }
                        Err(std::sync::mpsc::TryRecvError::Empty) => break,
                        Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                            self.playing = false;
                            self.seek(0.0);
                            break;
                        }
                    }
                }
            }
            // performance take: remember every change of the controls against clip time
            if self.rec_armed && self.take.last().map(|(_, p)| p != &self.params).unwrap_or(true) {
                self.take.push((self.video_t, self.params.clone()));
            }
        }
        if let Some(feed) = &mut self.live {
            match feed.latest() {
                Ok(Some(img)) => {
                    self.signal_src = Arc::new(img);
                    self.clean_texture = None;
                }
                Ok(None) => {}
                Err(()) => {
                    self.live = None;
                    self.status = "The live source stopped.".into();
                }
            }
        }
        let t = self.current_time();
        let (pw, ph, ps) = self.preview_dims();
        if self.power {
            let job = Job {
                src: self.signal_src.clone(),
                params: self.params.clone(),
                ft: FrameTime { t, frame: (t * self.fps()) as u32, fps: self.fps() },
                out: (pw, ph),
                preview_scale: ps,
                reset: std::mem::take(&mut self.dirty_reset),
            };
            let _ = self.worker.tx.send(job);
        }
        if let Some(img) = self.worker.result.lock().unwrap().take() {
            if let Some(rec) = &self.recorder {
                rec.push(&img);
            }
            let ci = egui::ColorImage::from_rgba_unmultiplied([img.w, img.h], &img.to_rgba8());
            match &mut self.texture {
                Some(t) => t.set(ci, egui::TextureOptions::LINEAR),
                None => self.texture = Some(ctx.load_texture("screen", ci, egui::TextureOptions::LINEAR)),
            }
        }
        if self.compare && self.clean_texture.is_none() {
            let img = &self.signal_src;
            let ci = egui::ColorImage::from_rgba_unmultiplied([img.w, img.h], &img.to_rgba8());
            self.clean_texture = Some(ctx.load_texture("clean", ci, egui::TextureOptions::LINEAR));
        }
        self.update_thumbs(ctx);
    }

    fn update_thumbs(&mut self, ctx: &egui::Context) {
        if self.thumb_textures.len() != self.presets.len() {
            self.thumb_textures = vec![None; self.presets.len()];
            *self.thumbs.lock().unwrap() = vec![None; self.presets.len()];
            self.thumbs_for = u64::MAX;
        }
        if self.thumbs_for != self.source_gen {
            self.thumbs_for = self.source_gen;
            let src = if self.idle { Arc::new(test_card(640, 360)) } else { self.signal_src.clone() };
            let list: Vec<Params> = self.presets.iter().map(|p| p.params.clone()).collect();
            let out = self.thumbs.clone();
            let ctx2 = ctx.clone();
            std::thread::spawn(move || {
                let tw = 176usize;
                let th = ((tw as f32 * src.h as f32 / src.w as f32).round() as usize).clamp(60, 176);
                let small = src.resized(tw * 2, th * 2);
                use rayon::prelude::*;
                list.par_iter().enumerate().for_each(|(i, p)| {
                    let mut r = Renderer::new();
                    let mut img = None;
                    for f in 0..3u32 {
                        let ft = FrameTime { t: 1.0 + f as f32 / 30.0, frame: 30 + f, fps: 30.0 };
                        img = Some(r.render(&small, p, &ft, tw, th, 0.3));
                    }
                    let img = img.unwrap();
                    let ci = egui::ColorImage::from_rgba_unmultiplied([img.w, img.h], &img.to_rgba8());
                    if let Some(slot) = out.lock().unwrap().get_mut(i) {
                        *slot = Some(ci);
                    }
                    ctx2.request_repaint();
                });
            });
        }
        let mut done = self.thumbs.lock().unwrap();
        for (i, slot) in done.iter_mut().enumerate() {
            if let Some(ci) = slot.take() {
                if let Some(t) = &mut self.thumb_textures[i] {
                    t.set(ci, egui::TextureOptions::LINEAR);
                } else {
                    self.thumb_textures[i] = Some(ctx.load_texture(format!("thumb{i}"), ci, egui::TextureOptions::LINEAR));
                }
            }
        }
    }

    fn start_export(&mut self) {
        if self.idle {
            self.status = "Load a video or image to enable export".into();
            return;
        }
        if !self.ffmpeg {
            self.status = "Export needs ffmpeg (brew install ffmpeg).".into();
            return;
        }
        let ext = self.format.ext();
        let stem = match &self.source {
            Source::Image { path, .. } | Source::Video { path, .. } => path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "clip".into()),
            Source::Live { .. } => "live".into(),
        };
        let Some(out) = rfd::FileDialog::new().set_title("Choose where to save the processed export").set_file_name(format!("{stem}-kiln.{ext}")).add_filter(ext, &[ext]).save_file() else {
            return;
        };
        if let Source::Video { path, .. } = &self.source {
            if *path == out {
                self.status = "Choose an export destination different from the source video.".into();
                return;
            }
        }
        self.status = "RENDERING EXPORT".into();
        let take = if matches!(self.source, Source::Video { .. }) { self.take.clone() } else { Vec::new() };
        self.export = Some(media::start_export(&self.source, self.params.clone(), take, self.format, out, self.still_seconds));
    }

    fn poll_export(&mut self) {
        let finished = self.export.as_ref().and_then(|j| j.result.lock().unwrap().take());
        if let Some(r) = finished {
            self.status = match r {
                Ok(p) => format!("SAVED {}", p.file_name().map(|n| n.to_string_lossy().to_uppercase()).unwrap_or_default()),
                Err(e) => e,
            };
            self.export = None;
        }
    }

    fn handle_screenshot(&mut self, ctx: &egui::Context) {
        let Some(path) = self.screenshot.clone() else { return };
        let state = *self.shot_taken.lock().unwrap();
        match state {
            None if self.frames_drawn >= 120 => {
                *self.shot_taken.lock().unwrap() = Some(false);
                let flag = self.shot_taken.clone();
                let ctx2 = ctx.clone();
                ctx.request_screenshot(move |img| {
                    let [w, h] = img.size;
                    let mut rgba = Vec::with_capacity(w * h * 4);
                    for p in &img.pixels {
                        rgba.extend_from_slice(&p.to_srgba_unmultiplied());
                    }
                    if let Some(dir) = path.parent() {
                        let _ = std::fs::create_dir_all(dir);
                    }
                    let _ = image::save_buffer(&path, &rgba, w as u32, h as u32, image::ColorType::Rgba8);
                    *flag.lock().unwrap() = Some(true);
                    ctx2.request_repaint();
                });
            }
            Some(true) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            _ => {}
        }
    }
}

// ------------------------------------------------------------------ the frame

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).filter(|p| !p.as_os_str().is_empty()).collect());
        if let Some(p) = dropped.into_iter().next() {
            self.open_path(p);
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Space)) && !ctx.egui_wants_keyboard_input() {
            self.toggle_play();
        }
        self.poll_live_connect();
        self.tick(&ctx);
        self.poll_export();

        egui::CentralPanel::default().frame(egui::Frame::new().fill(BG)).show(ui, |ui| {
            let full = ui.max_rect();
            let top = Rect::from_min_size(full.min, vec2(full.width(), TOP_H));
            let left = Rect::from_min_max(pos2(full.left(), top.bottom()), pos2(full.left() + LEFT_W, full.bottom()));
            let right = Rect::from_min_max(pos2(left.right(), top.bottom()), full.max);
            self.top_bar(ui, top);
            self.left_column(ui, left);
            let preview = Rect::from_min_max(right.min + vec2(14.0, 14.0), pos2(right.right() - 14.0, right.bottom() - BOTTOM_H));
            self.preview_panel(ui, preview);
            let bottom = Rect::from_min_max(pos2(right.left() + 14.0, preview.bottom() + 12.0), pos2(right.right() - 14.0, right.bottom() - 12.0));
            self.bottom_bay(ui, bottom);
        });

        self.live_window(&ctx);
        self.about_window(&ctx);
        if self.screenshot.is_none() {
            let closing = ctx.input(|i| i.viewport().close_requested());
            if closing || self.session_checked.elapsed().as_secs() >= 2 {
                self.save_session();
            }
        }
        ctx.request_repaint();
        self.frames_drawn += 1;
        self.handle_screenshot(&ctx);
    }
}

impl App {
    fn top_bar(&mut self, ui: &mut egui::Ui, rect: Rect) {
        let p = ui.painter().clone();
        p.rect_filled(rect, rr(0.0), BG);
        p.line_segment([rect.left_bottom(), rect.right_bottom()], Stroke::new(1.0, Color32::from_rgb(0x24, 0x28, 0x2c)));
        let cy = rect.center().y + 4.0;
        wordmark(&p, pos2(rect.left() + 24.0, cy - 10.0), 26.0, TEXT, false);

        // EASY / ADVANCED
        let x = rect.left() + 210.0;
        p.text(pos2(x + 40.0, rect.top() + 18.0), Align2::CENTER_CENTER, "EASY", gothic(9.0), TEXT_DIM);
        p.text(pos2(x + 120.0, rect.top() + 18.0), Align2::CENTER_CENTER, "ADVANCED", gothic(9.0), TEXT_DIM);
        if let Some(easy) = dual_switch(ui, Rect::from_min_size(pos2(x, rect.top() + 28.0), vec2(160.0, 48.0)), self.mode == Mode::Easy) {
            self.mode = if easy { Mode::Easy } else { Mode::Advanced };
        }

        // SIGNAL: source / process
        let mut x = x + 196.0;
        p.text(pos2(x + 56.0, rect.top() + 30.0), Align2::CENTER_CENTER, "SIGNAL", gothic(9.0), TEXT_DIM);
        let b = Rect::from_min_size(pos2(x, rect.top() + 40.0), vec2(112.0, 40.0));
        bezel(&p, b);
        let k1 = Rect::from_min_size(b.min + vec2(5.0, 5.0), vec2(49.0, 30.0));
        let k2 = Rect::from_min_size(b.min + vec2(58.0, 5.0), vec2(49.0, 30.0));
        if key_at(ui, k1, Id::NULL.with("sig_src"), "SOURCE", self.compare, true, gothic(7.0)).on_hover_text("Compare the unprocessed source with the complete signal path").clicked() {
            self.compare = true;
        }
        if key_at(ui, k2, Id::NULL.with("sig_proc"), "PROCESS", !self.compare, true, gothic(7.0)).clicked() {
            self.compare = false;
        }
        x += 132.0;

        // QUALITY (Advanced)
        if self.mode == Mode::Advanced {
            p.text(pos2(x, rect.top() + 30.0), Align2::LEFT_CENTER, "QUALITY", gothic(9.0), TEXT_DIM);
            let b = Rect::from_min_size(pos2(x, rect.top() + 40.0), vec2(156.0, 40.0));
            bezel(&p, b);
            for (i, (t, name)) in [(Tier::Sd, "SD"), (Tier::Hi, "HI"), (Tier::Full, "FULL")].into_iter().enumerate() {
                let k = Rect::from_min_size(b.min + vec2(5.0 + i as f32 * 49.0, 5.0), vec2(47.0, 30.0));
                let tip = ["910-sample broadcast raster", "1822-sample master-deck raster", "full-resolution signal path"][i];
                if key_at(ui, k, Id::NULL.with(("tier", i)), name, self.params.tier == t, true, gothic(8.5)).on_hover_text(tip).clicked() {
                    self.set_tier(t);
                }
            }
            x += 176.0;
        }

        // DRY / WET and SIGNAL CLEAN
        for (key, label) in [("dryWet", "DRY / WET"), ("signalClean", "SIGNAL CLEAN")] {
            let i = params::idx(key);
            let d = &DEFS[i];
            p.text(pos2(x + 34.0, rect.top() + 30.0), Align2::CENTER_CENTER, label, gothic(9.0), TEXT_DIM);
            let mut v = self.params.v[i];
            if knob_at(ui, pos2(x + 34.0, rect.top() + 61.0), 17.0, Id::NULL.with(("top", key)), &mut v, d.min, d.max, d.default, true) {
                self.params.v[i] = v;
                self.active_preset = None;
            }
            x += 92.0;
        }

        // right side: RECORD, POWER, VIEW, badge
        let rx = rect.right() - 76.0;
        let c = pos2(rx + 30.0, rect.top() + 52.0);
        p.circle_stroke(c, 24.0, Stroke::new(2.5, BLUE));
        p.circle_filled(c, 21.0, Color32::from_rgb(0x0b, 0x1a, 0x30));
        p.text(c, Align2::CENTER_CENTER, "K", gothic(20.0), BLUE_TEXT);
        let badge = ui.interact(Rect::from_center_size(c, vec2(50.0, 50.0)), Id::NULL.with("about"), egui::Sense::click());
        if badge.on_hover_text("About KILN").clicked() {
            self.show_about = true;
        }

        let vx = rx - 120.0;
        p.text(pos2(vx, rect.top() + 30.0), Align2::LEFT_CENTER, "VIEW", gothic(9.0), TEXT_DIM);
        let vr = Rect::from_min_size(pos2(vx, rect.top() + 40.0), vec2(104.0, 40.0));
        p.rect(vr, rr(6.0), Color32::from_rgb(0x0e, 0x10, 0x12), Stroke::new(1.0, KEY_EDGE), StrokeKind::Inside);
        ui.scope_builder(egui::UiBuilder::new().max_rect(vr.shrink2(vec2(10.0, 8.0))), |ui| {
            egui::ComboBox::from_id_salt("view")
                .selected_text(egui::RichText::new(self.view.label()).font(gothic(10.0)))
                .width(80.0)
                .show_ui(ui, |ui| {
                    for v in [View::Fit, View::Actual, View::Double] {
                        ui.selectable_value(&mut self.view, v, v.label());
                    }
                });
        });

        let px = vx - 70.0;
        p.text(pos2(px + 24.0, rect.top() + 30.0), Align2::CENTER_CENTER, "POWER", gothic(9.0), TEXT_DIM);
        let pr = Rect::from_min_size(pos2(px, rect.top() + 40.0), vec2(48.0, 40.0));
        if key_at(ui, pr, Id::NULL.with("power"), "", false, false, gothic(8.0)).on_hover_text("Switch the screen on or off").clicked() {
            self.power = !self.power;
            if !self.power {
                self.texture = None;
            }
            self.dirty_reset = true;
        }
        p.circle_filled(pos2(pr.center().x, pr.top() + 14.0), 2.6, if self.power { Color32::from_rgb(0xff, 0x3b, 0x30) } else { Color32::from_rgb(0x3a, 0x1a, 0x18) });

        let recx = px - 70.0;
        p.text(pos2(recx + 24.0, rect.top() + 30.0), Align2::CENTER_CENTER, "RECORD", gothic(9.0), TEXT_DIM);
        let rec_r = Rect::from_min_size(pos2(recx, rect.top() + 40.0), vec2(48.0, 40.0));
        let resp = key_at(ui, rec_r, Id::NULL.with("rec_top"), "", self.rec_armed, false, gothic(8.0))
            .on_hover_text("Record a performance take: knob moves while the clip plays go into the export. Right-click to clear the take.");
        if resp.clicked() {
            self.rec_armed = !self.rec_armed;
        }
        if resp.secondary_clicked() {
            self.take.clear();
            self.status = "TAKE CLEARED".into();
        }
        p.circle_filled(rec_r.center() - vec2(0.0, 4.0), 4.0, if self.rec_armed { RED } else { Color32::from_rgb(0xb8, 0xbb, 0xbe) });
    }

    fn left_column(&mut self, ui: &mut egui::Ui, rect: Rect) {
        let p = ui.painter().clone();
        // SOURCE block
        let src = Rect::from_min_size(rect.min, vec2(rect.width(), 140.0));
        p.rect_filled(src, rr(0.0), Color32::BLACK);
        p.text(src.left_top() + vec2(18.0, 26.0), Align2::LEFT_CENTER, "SOURCE", gothic(12.0), TEXT);
        let lcd = Rect::from_min_max(src.left_top() + vec2(18.0, 46.0), pos2(src.right() - 92.0, src.top() + 112.0));
        let live_key = Rect::from_min_max(pos2(lcd.right() + 8.0, lcd.top()), pos2(src.right() - 18.0, lcd.bottom()));
        let is_live = matches!(self.source, Source::Live { .. });
        if key_at(ui, live_key, Id::NULL.with("livekey"), "LIVE", is_live, true, gothic(9.0)).on_hover_text("Use a camera, capture device or video feed").clicked() {
            self.show_live = !self.show_live;
            if self.show_live {
                self.cameras = media::list_cameras();
            }
        }
        p.rect(lcd, rr(8.0), Color32::from_rgb(0x0d, 0x12, 0x15), Stroke::new(1.0, Color32::from_rgb(0x20, 0x29, 0x2e)), StrokeKind::Inside);
        p.line_segment([lcd.left_top() + vec2(8.0, 1.5), lcd.right_top() + vec2(-8.0, 1.5)], Stroke::new(1.0, Color32::from_white_alpha(12)));
        let ic = pos2(lcd.left() + 22.0, lcd.center().y - if self.idle { 0.0 } else { 8.0 });
        p.rect_stroke(Rect::from_center_size(ic + vec2(-2.0, 2.0), vec2(14.0, 11.0)), rr(2.0), Stroke::new(1.5, TEXT_DIM), StrokeKind::Inside);
        p.rect_stroke(Rect::from_center_size(ic + vec2(2.0, -2.0), vec2(14.0, 11.0)), rr(2.0), Stroke::new(1.5, TEXT_DIM), StrokeKind::Inside);
        let mut name = self.source_name.clone();
        if name.chars().count() > 18 {
            name = name.chars().take(17).collect::<String>() + "…";
        }
        p.text(pos2(lcd.left() + 40.0, ic.y), Align2::LEFT_CENTER, name, mono(17.0), BLUE_TEXT);
        if !self.idle {
            let kind = match &self.source {
                Source::Video { info, .. } => format!("VIDEO SOURCE · {}×{} · {:.2} FPS", info.w, info.h, info.fps),
                Source::Image { img, .. } => format!("IMAGE SOURCE · {}×{}", img.w, img.h),
                Source::Live { input: LiveInput::Camera { .. } } => "LIVE CAMERA · 1280×720".to_string(),
                Source::Live { input: LiveInput::Stream(_) } => "LIVE STREAM · 1280×720".to_string(),
            };
            p.text(pos2(lcd.left() + 40.0, ic.y + 20.0), Align2::LEFT_CENTER, kind, mono(9.5), TEXT_DIM);
        }
        let resp = ui.interact(lcd, Id::NULL.with("lcd"), Sense::click());
        if resp.on_hover_text("Choose a video or image").clicked() {
            self.choose_file();
        }

        let body = Rect::from_min_max(pos2(rect.left() + 16.0, src.bottom() + 12.0), pos2(rect.right() - 16.0, rect.bottom() - 12.0));
        match self.mode {
            Mode::Easy => self.easy_left(ui, body),
            Mode::Advanced => self.advanced_left(ui, body),
        }
    }

    fn preset_row(&mut self, ui: &mut egui::Ui, i: usize, width: f32) -> bool {
        let (r, resp) = ui.allocate_exact_size(vec2(width, 58.0), Sense::click());
        let sel = self.active_preset.as_deref() == Some(self.presets[i].name.as_str());
        let p = ui.painter();
        let (fill, edge, w) = if sel {
            (SEL_FILL, BLUE, 1.5)
        } else if resp.hovered() {
            (Color32::from_rgb(0x22, 0x26, 0x2a), CARD_EDGE, 1.0)
        } else {
            (Color32::from_rgb(0x1e, 0x20, 0x22), CARD_EDGE, 1.0)
        };
        p.rect(r, rr(6.0), fill, Stroke::new(w, edge), StrokeKind::Inside);
        let th = Rect::from_min_size(r.min + vec2(8.0, 6.0), vec2(82.0, 46.0));
        p.rect_filled(th, rr(3.0), Color32::BLACK);
        if let Some(t) = &self.thumb_textures[i] {
            p.image(t.id(), th.shrink(1.0), Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)), Color32::WHITE);
        }
        p.rect_stroke(th, rr(3.0), Stroke::new(1.0, Color32::from_rgb(0x3a, 0x3f, 0x45)), StrokeKind::Inside);
        let (_, desc) = self.presets[i].info();
        p.text(pos2(th.right() + 14.0, r.top() + 22.0), Align2::LEFT_CENTER, &self.presets[i].name, gothic(12.0), TEXT);
        p.text(pos2(th.right() + 14.0, r.top() + 40.0), Align2::LEFT_CENTER, desc, gothic(8.0), TEXT_DIM);
        let c = pos2(r.right() - 20.0, r.center().y);
        if sel {
            p.circle_filled(c, 8.0, BLUE);
            p.add(Shape::line(vec![c + vec2(-4.0, 0.0), c + vec2(-1.0, 3.0), c + vec2(4.0, -3.0)], Stroke::new(1.8, Color32::WHITE)));
        } else {
            p.add(Shape::line(vec![c + vec2(-2.0, -5.0), c + vec2(3.0, 0.0), c + vec2(-2.0, 5.0)], Stroke::new(1.5, TEXT_DIM)));
        }
        if !self.presets[i].factory {
            let mut delete = false;
            resp.context_menu(|ui| {
                if ui.button("Delete preset").clicked() {
                    delete = true;
                    ui.close();
                }
            });
            if delete {
                let removed = self.presets.remove(i);
                match store::Store::default().save(&self.presets) {
                    Ok(()) => {
                        if self.active_preset.as_deref() == Some(removed.name.as_str()) {
                            self.active_preset = None;
                        }
                        self.status = "PRESET DELETED".into();
                    }
                    Err(e) => {
                        self.presets.insert(i, removed);
                        self.status = format!("PRESET DELETE FAILED · {e}");
                    }
                }
                self.thumbs_for = u64::MAX;
                return false;
            }
        }
        resp.clicked()
    }

    fn easy_left(&mut self, ui: &mut egui::Ui, rect: Rect) {
        let p = ui.painter().clone();
        let quick = Rect::from_min_max(pos2(rect.left(), rect.bottom() - 166.0), rect.max);
        let card = Rect::from_min_max(rect.min, pos2(rect.right(), quick.top() - 12.0));
        panel(&p, card, CARD, CARD_EDGE, 8.0);
        p.text(card.left_top() + vec2(18.0, 28.0), Align2::LEFT_CENTER, "PRESETS", gothic(12.0), TEXT);
        let factory = self.presets.iter().filter(|p| p.factory).count();
        p.text(card.left_top() + vec2(104.0, 28.0), Align2::LEFT_CENTER, format!("{factory} FACTORY"), mono(10.0), BLUE_TEXT);
        let nk = Rect::from_min_size(pos2(card.right() - 134.0, card.top() + 13.0), vec2(118.0, 30.0));
        if key_at(ui, nk, Id::NULL.with("newpreset"), "+ NEW PRESET", self.naming_preset, false, gothic(8.0)).on_hover_text("Save the current look as a preset").clicked() {
            self.naming_preset = !self.naming_preset;
        }
        let rk = Rect::from_min_size(pos2(nk.left() - 36.0, card.top() + 13.0), vec2(30.0, 30.0));
        if key_at(ui, rk, Id::NULL.with("resetall"), "↺", false, false, FontId::proportional(14.0)).on_hover_text("Reset every control to clean").clicked() {
            self.reset_all();
        }
        let mut list_top = card.top() + 54.0;
        if self.naming_preset {
            let r = Rect::from_min_size(pos2(card.left() + 18.0, list_top), vec2(card.width() - 36.0, 28.0));
            ui.scope_builder(egui::UiBuilder::new().max_rect(r), |ui| {
                ui.horizontal(|ui| {
                    let te = ui.add(egui::TextEdit::singleline(&mut self.new_preset_name).hint_text("PRESET NAME").desired_width(r.width() - 70.0).font(mono(12.0)));
                    if ui.button("SAVE").clicked() || (te.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))) {
                        self.save_preset();
                    }
                });
            });
            list_top += 36.0;
        }
        let list = Rect::from_min_max(pos2(card.left() + 18.0, list_top), pos2(card.right() - 12.0, card.bottom() - 10.0));
        let mut clicked = None;
        ui.scope_builder(egui::UiBuilder::new().max_rect(list), |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 8.0;
                let w = list.width() - 8.0;
                for cat in Category::ALL {
                    let idxs: Vec<usize> = (0..self.presets.len()).filter(|&i| self.presets[i].info().0 == cat).collect();
                    if idxs.is_empty() {
                        continue;
                    }
                    let (hr, _) = ui.allocate_exact_size(vec2(w, 20.0), Sense::hover());
                    let g = ui.painter().text(hr.left_center(), Align2::LEFT_CENTER, cat.title(), gothic(9.0), TEXT_DIM);
                    ui.painter().line_segment([pos2(g.right() + 10.0, hr.center().y), pos2(hr.right(), hr.center().y)], Stroke::new(1.0, CARD_EDGE));
                    for i in idxs {
                        if i < self.presets.len() && self.preset_row(ui, i, w) {
                            clicked = Some(i);
                        }
                    }
                    ui.add_space(6.0);
                }
            });
        });
        if let Some(i) = clicked {
            self.apply_preset(i);
        }

        // QUICK SHAPE
        panel(&p, quick, CARD, CARD_EDGE, 8.0);
        p.text(quick.left_top() + vec2(18.0, 28.0), Align2::LEFT_CENTER, "QUICK SHAPE", gothic(12.0), TEXT);
        let tape_name = self.active_preset.clone().unwrap_or_else(|| "CUSTOM".into());
        let tape = Rect::from_center_size(pos2(quick.right() - 80.0, quick.top() + 28.0), vec2(118.0, 18.0));
        p.add(Shape::convex_polygon(
            vec![tape.left_top() + vec2(2.0, 1.0), tape.right_top(), tape.right_bottom() + vec2(-3.0, 0.0), tape.left_bottom() + vec2(0.0, -1.0)],
            Color32::from_rgb(0xb4, 0xad, 0x9e),
            Stroke::NONE,
        ));
        p.text(tape.center(), Align2::CENTER_CENTER, tape_name, gothic(7.5), Color32::from_rgb(0x4a, 0x47, 0x40));
        let groups = [Group::Damage, Group::Waves, Group::ColorSignal, Group::Screen];
        let step = quick.width() / 4.0;
        for (k, g) in groups.into_iter().enumerate() {
            let cx = quick.left() + step * (k as f32 + 0.5);
            let mut v = self.params.voice(g);
            if knob_at(ui, pos2(cx, quick.top() + 78.0), 24.0, Id::NULL.with(("quick", k)), &mut v, 0.0, 1.0, 0.0, true) {
                self.params.set_voice(g, v);
                self.detail_group = g;
                self.active_preset = None;
            }
            let lr = Rect::from_center_size(pos2(cx, quick.top() + 122.0), vec2(step, 16.0));
            let resp = ui.interact(lr, Id::NULL.with(("quicklabel", k)), Sense::click());
            if resp.clicked() {
                self.detail_group = g;
            }
            p.text(lr.center(), Align2::CENTER_CENTER, g.title(), gothic(9.0), if self.detail_group == g { TEXT } else { TEXT_DIM });
            p.text(pos2(cx, quick.top() + 145.0), Align2::CENTER_CENTER, format!("{:.2}", self.params.voice(g)), mono(12.0), BLUE_TEXT);
        }
    }

    fn advanced_left(&mut self, ui: &mut egui::Ui, rect: Rect) {
        let p = ui.painter().clone();
        p.text(rect.left_top() + vec2(0.0, 10.0), Align2::LEFT_CENTER, "PRESETS", gothic(10.0), TEXT_DIM);
        let card = Rect::from_min_size(rect.min + vec2(0.0, 24.0), vec2(rect.width(), 108.0));
        panel(&p, card, Color32::from_rgb(0x0f, 0x11, 0x13), CARD_EDGE, 10.0);
        let chip = Rect::from_min_size(card.min + vec2(28.0, 26.0), vec2(150.0, 40.0));
        p.rect(chip, rr(5.0), KEY, Stroke::new(1.0, KEY_EDGE), StrokeKind::Inside);
        let active_idx = self.active_preset.as_ref().and_then(|a| self.presets.iter().position(|p| &p.name == a));
        let th = Rect::from_min_size(chip.min + vec2(6.0, 6.0), vec2(48.0, 28.0));
        p.rect_filled(th, rr(2.0), Color32::BLACK);
        if let Some(t) = active_idx.and_then(|i| self.thumb_textures.get(i).and_then(|t| t.as_ref())) {
            p.image(t.id(), th, Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)), Color32::WHITE);
        }
        p.circle_stroke(pos2(th.right() + 12.0, chip.center().y), 4.0, Stroke::new(1.2, GREEN_LED));
        p.circle_filled(pos2(th.right() + 12.0, chip.center().y), 2.0, GREEN_LED);
        let name = self.active_preset.clone().unwrap_or_else(|| "CUSTOM".into());
        p.text(pos2(th.right() + 22.0, chip.center().y), Align2::LEFT_CENTER, name, gothic(9.0), TEXT_DIM);
        let mut x = chip.right() + 6.0;
        for (i, label) in ["‹", "›"].iter().enumerate() {
            let r = Rect::from_min_size(pos2(x, chip.top()), vec2(40.0, 40.0));
            if key_at(ui, r, Id::NULL.with(("pstep", i)), label, false, false, FontId::proportional(18.0)).clicked() {
                self.step_preset(if i == 0 { -1 } else { 1 });
            }
            x += 46.0;
        }
        let sr = Rect::from_min_size(pos2(x, chip.top()), vec2(40.0, 40.0));
        if key_at(ui, sr, Id::NULL.with("psave"), "", self.naming_preset, false, gothic(8.0)).on_hover_text("Save the current look as a preset").clicked() {
            self.naming_preset = !self.naming_preset;
        }
        let fl = Rect::from_center_size(sr.center(), vec2(14.0, 15.0));
        p.rect_stroke(fl, rr(2.0), Stroke::new(1.4, TEXT_DIM), StrokeKind::Inside);
        p.rect_filled(Rect::from_min_size(fl.min + vec2(3.5, 2.0), vec2(7.0, 4.0)), rr(0.0), TEXT_DIM);
        if self.naming_preset {
            let r = Rect::from_min_size(pos2(chip.left(), chip.bottom() + 8.0), vec2(card.width() - 56.0, 26.0));
            ui.scope_builder(egui::UiBuilder::new().max_rect(r), |ui| {
                ui.horizontal(|ui| {
                    let te = ui.add(egui::TextEdit::singleline(&mut self.new_preset_name).hint_text("PRESET NAME").desired_width(r.width() - 70.0).font(mono(11.0)));
                    if ui.button("SAVE").clicked() || (te.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))) {
                        self.save_preset();
                    }
                });
            });
        } else {
            p.text(pos2(chip.left(), chip.bottom() + 20.0), Align2::LEFT_CENTER, "FACTORY PRESETS INSTALLED", gothic(8.5), TEXT_DIM);
        }

        let list = Rect::from_min_max(pos2(rect.left(), card.bottom() + 12.0), rect.max);
        ui.scope_builder(egui::UiBuilder::new().max_rect(list), |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 8.0;
                let w = list.width() - 10.0;
                for (title, groups) in [("SIGNAL", &Group::SIGNAL[..]), ("SCREEN", &Group::SCREEN[..])] {
                    let (hr, _) = ui.allocate_exact_size(vec2(w, 26.0), Sense::hover());
                    ui.painter().text(hr.left_bottom() + vec2(0.0, -6.0), Align2::LEFT_BOTTOM, title, gothic(10.0), TEXT_DIM);
                    for &g in groups {
                        self.module_row(ui, g, w);
                    }
                }
            });
        });
    }

    fn module_row(&mut self, ui: &mut egui::Ui, g: Group, width: f32) {
        let open = self.open_modules.contains(&g);
        let (r, resp) = ui.allocate_exact_size(vec2(width, 56.0), Sense::click());
        let p = ui.painter().clone();
        p.rect(r, rr(6.0), if resp.hovered() { Color32::from_rgb(0x20, 0x24, 0x28) } else { CARD }, Stroke::new(1.0, CARD_EDGE), StrokeKind::Inside);
        p.text(pos2(r.left() + 20.0, r.center().y), Align2::CENTER_CENTER, if open { "−" } else { "+" }, FontId::proportional(18.0), TEXT_DIM);
        group_icon(&p, pos2(r.left() + 52.0, r.center().y), g);
        p.text(pos2(r.left() + 74.0, r.center().y), Align2::LEFT_CENTER, g.title(), gothic(12.5), TEXT);
        let reset = Rect::from_center_size(pos2(r.right() - 84.0, r.center().y), vec2(32.0, 32.0));
        if key_at(ui, reset, Id::NULL.with(("modreset", g.title())), "↺", false, false, FontId::proportional(14.0)).clicked() {
            self.params.reset_group(g);
            self.active_preset = None;
        }
        let mut v = self.params.voice(g);
        if knob_at(ui, pos2(r.right() - 34.0, r.center().y), 17.0, Id::NULL.with(("modknob", g.title())), &mut v, 0.0, 1.0, 0.0, false) {
            self.params.set_voice(g, v);
            self.active_preset = None;
        }
        if resp.clicked() {
            if open {
                self.open_modules.retain(|x| *x != g);
            } else {
                self.open_modules.push(g);
            }
        }
        if open {
            let idxs: Vec<usize> = DEFS.iter().enumerate().filter(|(_, d)| d.group == g).map(|(i, _)| i).collect();
            let rows = idxs.len().div_ceil(2);
            let (body, _) = ui.allocate_exact_size(vec2(width, rows as f32 * 52.0 + 12.0), Sense::hover());
            p.rect(body, rr(6.0), Color32::from_rgb(0x15, 0x18, 0x1b), Stroke::new(1.0, CARD_EDGE), StrokeKind::Inside);
            let cw = (body.width() - 36.0) / 2.0;
            for (n, i) in idxs.into_iter().enumerate() {
                let cell = Rect::from_min_size(pos2(body.left() + 12.0 + (n % 2) as f32 * (cw + 12.0), body.top() + 10.0 + (n / 2) as f32 * 52.0), vec2(cw, 44.0));
                self.control_cell(ui, cell, i);
            }
        }
    }

    /// One control: a slider, or a drop-down for a switch.
    fn control_cell(&mut self, ui: &mut egui::Ui, cell: Rect, i: usize) {
        let d = &DEFS[i];
        match d.kind {
            Kind::Knob => {
                let mut v = self.params.v[i];
                if slider_at(ui, cell, Id::NULL.with(("slider", d.key)), d.label, &mut v, d.min, d.max, d.default) {
                    self.params.v[i] = v;
                    self.active_preset = None;
                    if d.key == "feedbackMix" {
                        self.dirty_reset = true;
                    }
                }
            }
            Kind::Choice(names) => {
                ui.painter().text(cell.left_top() + vec2(0.0, 6.0), Align2::LEFT_CENTER, d.label, gothic(8.5), TEXT);
                let r = Rect::from_min_size(cell.left_top() + vec2(0.0, 13.0), vec2(cell.width(), 22.0));
                ui.painter().rect(r, rr(4.0), Color32::from_rgb(0x0e, 0x10, 0x12), Stroke::new(1.0, KEY_EDGE), StrokeKind::Inside);
                let cur = (self.params.v[i].round() as usize).min(names.len() - 1);
                ui.scope_builder(egui::UiBuilder::new().max_rect(r.shrink2(vec2(8.0, 1.0))), |ui| {
                    egui::ComboBox::from_id_salt(("choice", d.key))
                        .selected_text(egui::RichText::new(names[cur]).font(gothic(9.0)).color(TEXT))
                        .width(r.width() - 16.0)
                        .show_ui(ui, |ui| {
                            for (k, n) in names.iter().enumerate() {
                                if ui.selectable_label(cur == k, *n).clicked() {
                                    self.params.v[i] = k as f32;
                                    self.active_preset = None;
                                }
                            }
                        });
                });
            }
        }
    }

    fn preview_panel(&mut self, ui: &mut egui::Ui, rect: Rect) {
        let p = ui.painter().clone();
        panel(&p, rect, PANEL, CARD_EDGE, 8.0);
        let header = Rect::from_min_size(rect.min, vec2(rect.width(), 34.0));
        p.rect_filled(Rect::from_min_size(header.left_top() + vec2(20.0, 12.0), vec2(4.0, 10.0)), rr(1.0), Color32::from_rgb(0x0c, 0x0d, 0x0e));
        p.text(header.left_center() + vec2(32.0, 0.0), Align2::LEFT_CENTER, "PREVIEW", gothic(10.0), TEXT_DIM);
        p.text(header.right_center() - vec2(18.0, 0.0), Align2::RIGHT_CENTER, "VIDEO OUTPUT", gothic(10.0), TEXT_DIM);
        if !self.status.is_empty() {
            p.text(header.center(), Align2::CENTER_CENTER, &self.status, mono(10.0), AMBER);
        }
        let transport_h = 48.0;
        let body = Rect::from_min_max(pos2(rect.left() + 1.0, header.bottom()), pos2(rect.right() - 1.0, rect.bottom() - transport_h));
        p.rect_filled(body, rr(0.0), SCREEN_BLACK);

        // the picture
        let tex = if self.compare { self.clean_texture.as_ref().or(self.texture.as_ref()) } else { self.texture.as_ref() };
        let screen_area = body.shrink2(vec2(0.0, 22.0));
        let (sw, sh) = self.src_dims();
        let aspect = sw as f32 / sh as f32;
        let img_rect = match (self.view, tex) {
            (View::Fit, _) | (_, None) => {
                let mut size = screen_area.size();
                if size.x / size.y > aspect {
                    size.x = size.y * aspect;
                } else {
                    size.y = size.x / aspect;
                }
                Rect::from_center_size(screen_area.center(), size)
            }
            (v, Some(t)) => {
                let ppp = ui.ctx().pixels_per_point();
                let z = if v == View::Double { 2.0 } else { 1.0 };
                let [tw, th] = t.size();
                Rect::from_center_size(screen_area.center(), vec2(tw as f32, th as f32) * z / ppp)
            }
        };
        let clip = p.with_clip_rect(body);
        if self.power {
            if let Some(t) = tex {
                clip.image(t.id(), img_rect, Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)), Color32::WHITE);
            }
        } else {
            clip.rect_filled(img_rect, rr(0.0), Color32::BLACK);
            clip.circle_filled(img_rect.center(), 1.5, Color32::from_white_alpha(40));
        }
        if self.compare {
            clip.text(img_rect.left_top() + vec2(10.0, 10.0), Align2::LEFT_TOP, "SOURCE", mono(11.0), AMBER);
        }
        if self.idle && self.power {
            let c = pos2(img_rect.center().x - 105.0, img_rect.top() + img_rect.height() * 0.40);
            wordmark(&clip, c, 34.0, Color32::from_rgb(0xe8, 0xea, 0xee), true);
            let dy = img_rect.top() + img_rect.height() * 0.80;
            clip.text(pos2(img_rect.center().x, dy), Align2::CENTER_CENTER, "DROP VIDEO OR IMAGE", gothic(13.0), Color32::from_rgb(0x7d, 0x82, 0x88));
            let br = Rect::from_center_size(pos2(img_rect.center().x, dy + 42.0), vec2(150.0, 40.0));
            bezel(&clip, br);
            let inner = br.shrink(5.0);
            let resp = ui.interact(inner, Id::NULL.with("browse"), Sense::click());
            clip.rect(inner, rr(5.0), if resp.hovered() { Color32::from_rgb(0x26, 0x2a, 0x2e) } else { KEY }, Stroke::new(1.0, KEY_EDGE), StrokeKind::Inside);
            clip.text(inner.center(), Align2::CENTER_CENTER, "BROWSE", gothic(11.0), GREEN_TEXT);
            if resp.on_hover_text("Choose a video or image").clicked() {
                self.choose_file();
            }
        }

        // transport
        let tr = Rect::from_min_max(pos2(rect.left(), body.bottom()), rect.max);
        let group = Rect::from_min_size(tr.left_top() + vec2(14.0, 6.0), vec2(250.0, 36.0));
        p.rect(group, rr(6.0), BEZEL, Stroke::new(1.0, CARD_EDGE), StrokeKind::Inside);
        let is_video = matches!(self.source, Source::Video { .. });
        let keys: [(Color32, &str); 4] = [
            (Color32::from_rgb(0x2b, 0x5f, 0xd9), "play"),
            (Color32::from_rgb(0xd9, 0xa5, 0x20), "pause"),
            (Color32::from_rgb(0x6b, 0x6f, 0x73), "stop"),
            (Color32::from_rgb(0xc8, 0x30, 0x2c), "rec"),
        ];
        for (k, (col, what)) in keys.iter().enumerate() {
            let r = Rect::from_min_size(group.min + vec2(5.0 + k as f32 * 42.0, 4.0), vec2(38.0, 28.0));
            let resp = ui.interact(r, Id::NULL.with(("transport", k)), Sense::click());
            let lit = match *what {
                "play" => self.playing,
                "pause" => !self.playing && is_video,
                "rec" => self.rec_armed,
                _ => false,
            };
            let fill = if resp.hovered() || lit { *col } else { col.gamma_multiply(0.85) };
            p.rect(r, rr(3.0), fill, Stroke::new(1.0, Color32::from_black_alpha(160)), StrokeKind::Inside);
            let c = r.center();
            let ink = Color32::from_rgb(0x10, 0x12, 0x14);
            match *what {
                "play" => {
                    p.add(Shape::convex_polygon(vec![c + vec2(-4.0, -5.0), c + vec2(5.0, 0.0), c + vec2(-4.0, 5.0)], ink, Stroke::NONE));
                }
                "pause" => {
                    p.rect_filled(Rect::from_center_size(c + vec2(-2.5, 0.0), vec2(2.5, 9.0)), rr(0.0), ink);
                    p.rect_filled(Rect::from_center_size(c + vec2(2.5, 0.0), vec2(2.5, 9.0)), rr(0.0), ink);
                }
                "stop" => {
                    p.rect_filled(Rect::from_center_size(c, vec2(9.0, 9.0)), rr(0.0), ink);
                }
                _ => {
                    p.circle_filled(c, 4.5, ink);
                }
            }
            if resp.clicked() {
                match *what {
                    "play" if is_video && !self.playing => self.toggle_play(),
                    "pause" => self.playing = false,
                    "stop" => {
                        self.playing = false;
                        self.seek(0.0);
                    }
                    "rec" => self.rec_armed = !self.rec_armed,
                    _ => {}
                }
            }
            if *what == "rec" && resp.secondary_clicked() {
                self.take.clear();
                self.status = "TAKE CLEARED".into();
            }
        }
        let rec_label = if self.take.is_empty() { "RECORD".to_string() } else { format!("TAKE {}", self.take.len()) };
        let lp = group.min + vec2(178.0, 18.0);
        p.circle_filled(lp, 3.0, if self.rec_armed { RED } else { TEXT_FAINT });
        p.text(lp + vec2(8.0, 0.0), Align2::LEFT_CENTER, rec_label, gothic(8.5), if self.rec_armed { RED } else { TEXT_FAINT });

        let (dur, t) = match &self.source {
            Source::Video { info, .. } => (info.duration, self.video_t),
            Source::Image { .. } => (0.0, 0.0),
            Source::Live { .. } => (0.0, self.clock.elapsed().as_secs_f32()),
        };
        let fs = Rect::from_min_size(pos2(tr.right() - 44.0, tr.top() + 8.0), vec2(32.0, 32.0));
        let resp = key_at(ui, fs, Id::NULL.with("fullscreen"), "", self.fullscreen, false, gothic(8.0)).on_hover_text("Enter Full Screen (Control-Command-F)");
        let c = fs.center();
        for (a, b) in [(vec2(-6.0, -6.0), vec2(-2.0, -2.0)), (vec2(6.0, 6.0), vec2(2.0, 2.0))] {
            p.line_segment([c + a, c + b], Stroke::new(1.5, TEXT_DIM));
            p.line_segment([c + a, c + a + vec2(if a.x < 0.0 { 3.5 } else { -3.5 }, 0.0)], Stroke::new(1.5, TEXT_DIM));
            p.line_segment([c + a, c + a + vec2(0.0, if a.y < 0.0 { 3.5 } else { -3.5 })], Stroke::new(1.5, TEXT_DIM));
        }
        if resp.clicked() {
            self.fullscreen = !self.fullscreen;
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Fullscreen(self.fullscreen));
        }
        let time = format!("{} / {}", fmt_time(t), fmt_time(dur));
        let tg = p.text(pos2(fs.left() - 14.0, tr.center().y), Align2::RIGHT_CENTER, time, mono(12.0), TEXT_DIM);
        let track = Rect::from_min_max(pos2(group.right() + 22.0, tr.top() + 13.0), pos2(tg.left() - 24.0, tr.top() + 15.0));
        p.rect_filled(track, rr(1.0), Color32::from_rgb(0x2a, 0x2e, 0x33));
        let frac = if dur > 0.0 { (t / dur).clamp(0.0, 1.0) } else { 0.0 };
        p.circle_filled(pos2(track.left() + track.width() * frac, track.center().y), 6.5, Color32::from_rgb(0xd6, 0xd8, 0xdb));
        let sresp = ui.interact(track.expand2(vec2(6.0, 10.0)), Id::NULL.with("scrub"), Sense::click_and_drag());
        if is_video && (sresp.drag_stopped() || sresp.clicked()) {
            if let Some(pos) = sresp.interact_pointer_pos() {
                let f = ((pos.x - track.left()) / track.width()).clamp(0.0, 1.0);
                self.seek(f * dur);
            }
        }
    }

    fn bottom_bay(&mut self, ui: &mut egui::Ui, rect: Rect) {
        let export = Rect::from_min_max(pos2(rect.right() - 282.0, rect.top() + 4.0), pos2(rect.right(), rect.bottom()));
        let controls = Rect::from_min_max(rect.min, pos2(export.left() - 24.0, rect.bottom()));
        match self.mode {
            Mode::Easy => self.detail_controls(ui, controls),
            Mode::Advanced => self.global_controls(ui, controls),
        }
        self.export_box(ui, export);
    }

    fn detail_controls(&mut self, ui: &mut egui::Ui, rect: Rect) {
        let p = ui.painter().clone();
        panel(&p, rect, Color32::from_rgb(0x14, 0x17, 0x1a), Color32::from_rgb(0x22, 0x26, 0x2a), 6.0);
        let g = self.detail_group;
        p.text(rect.left_top() + vec2(22.0, 18.0), Align2::LEFT_CENTER, "DETAIL CONTROLS", gothic(8.5), TEXT_DIM);
        p.text(rect.left_top() + vec2(22.0, 36.0), Align2::LEFT_CENTER, g.title(), gothic(14.0), TEXT);
        let rk = Rect::from_min_size(pos2(rect.right() - 50.0, rect.top() + 12.0), vec2(32.0, 30.0));
        if key_at(ui, rk, Id::NULL.with("detailreset"), "↺", false, false, FontId::proportional(14.0)).clicked() {
            self.params.reset_group(g);
            self.active_preset = None;
        }
        let idxs: Vec<usize> = DEFS.iter().enumerate().filter(|(_, d)| d.group == g).map(|(i, _)| i).collect();
        let cols = 4;
        let area = Rect::from_min_max(pos2(rect.left() + 22.0, rect.top() + 50.0), pos2(rect.right() - 22.0, rect.bottom() - 4.0));
        let cw = (area.width() - 24.0 * (cols as f32 - 1.0)) / cols as f32;
        let rows = idxs.len().div_ceil(cols).max(1);
        let rh = (area.height() / rows as f32).min(44.0);
        for (n, i) in idxs.into_iter().enumerate() {
            let cell = Rect::from_min_size(pos2(area.left() + (n % cols) as f32 * (cw + 24.0), area.top() + (n / cols) as f32 * rh), vec2(cw, rh));
            self.control_cell(ui, cell, i);
        }
    }

    fn global_controls(&mut self, ui: &mut egui::Ui, rect: Rect) {
        let p = ui.painter().clone();
        let tab = Rect::from_min_size(rect.min + vec2(4.0, 0.0), vec2(126.0, 34.0));
        p.rect(tab, rr(5.0), PANEL, Stroke::new(1.0, CARD_EDGE), StrokeKind::Inside);
        p.text(tab.left_center() + vec2(14.0, 0.0), Align2::LEFT_CENTER, "GLOBAL", gothic(11.0), TEXT_DIM);
        let rk = Rect::from_min_size(pos2(tab.right() - 34.0, tab.top() + 4.0), vec2(28.0, 26.0));
        if key_at(ui, rk, Id::NULL.with("globalreset"), "↺", false, false, FontId::proportional(13.0)).clicked() {
            self.reset_all();
        }
        enum G {
            Key(&'static str),
            Voice(Group),
        }
        let items: [(G, &str); 11] = [
            (G::Key("outputBrightness"), "BRIGHTNESS"),
            (G::Key("outputContrast"), "CONTRAST"),
            (G::Voice(Group::ColorSignal), "COLOR SIGNAL"),
            (G::Voice(Group::CircuitBend), "CIRCUIT BEND"),
            (G::Voice(Group::Damage), "DAMAGE"),
            (G::Voice(Group::Noise), "NOISE"),
            (G::Voice(Group::Waves), "WAVES"),
            (G::Voice(Group::Screen), "SCREEN"),
            (G::Voice(Group::Capture), "CAPTURE"),
            (G::Voice(Group::Optics), "GLASS & OPTICS"),
            (G::Key("chromaBleed"), "CHROMA BLEED"),
        ];
        let step = rect.width() / items.len() as f32;
        let radius = (step * 0.40).min(31.0);
        for (k, (item, label)) in items.iter().enumerate() {
            let cx = rect.left() + step * (k as f32 + 0.5);
            let c = pos2(cx, rect.top() + 48.0 + radius);
            let shown = match item {
                G::Key(key) => {
                    let i = params::idx(key);
                    let d = &DEFS[i];
                    let mut v = self.params.v[i];
                    if knob_at(ui, c, radius, Id::NULL.with(("global", k)), &mut v, d.min, d.max, d.default, true) {
                        self.params.v[i] = v;
                        self.active_preset = None;
                    }
                    if *key == "outputBrightness" { format!("{:+.2}", self.params.v[i]) } else { format!("{:.2}", self.params.v[i]) }
                }
                G::Voice(g) => {
                    let mut v = self.params.voice(*g);
                    if knob_at(ui, c, radius, Id::NULL.with(("global", k)), &mut v, 0.0, 1.0, 0.0, true) {
                        self.params.set_voice(*g, v);
                        self.active_preset = None;
                    }
                    format!("{:.2}", self.params.voice(*g))
                }
            };
            let font = if label.len() > 12 { gothic(5.6) } else if label.len() > 10 { gothic(6.2) } else { gothic(7.5) };
            p.text(pos2(cx, c.y + radius + 18.0), Align2::CENTER_CENTER, *label, font, TEXT_DIM);
            let vb = Rect::from_center_size(pos2(cx, rect.bottom() - 18.0), vec2((step - 8.0).min(60.0), 24.0));
            p.rect(vb, rr(3.0), Color32::from_rgb(0x0a, 0x0c, 0x0e), Stroke::new(1.0, CARD_EDGE), StrokeKind::Inside);
            p.text(vb.center(), Align2::CENTER_CENTER, shown, mono(11.5), BLUE_TEXT);
        }
    }

    fn export_box(&mut self, ui: &mut egui::Ui, rect: Rect) {
        let p = ui.painter().clone();
        // brushed-metal frame around a black well
        p.rect(rect, rr(6.0), Color32::from_rgb(0x30, 0x33, 0x36), Stroke::new(2.0, Color32::from_rgb(0x6a, 0x6e, 0x72)), StrokeKind::Inside);
        let well = rect.shrink(8.0);
        p.rect(well, rr(4.0), Color32::from_rgb(0x0a, 0x0c, 0x0d), Stroke::new(1.0, Color32::BLACK), StrokeKind::Inside);
        let inner = well.shrink2(vec2(18.0, 14.0));
        p.text(inner.left_top() + vec2(0.0, 12.0), Align2::LEFT_CENTER, "EXPORT", gothic(14.0), TEXT);
        p.text(inner.left_top() + vec2(0.0, 30.0), Align2::LEFT_CENTER, "FORMAT", gothic(8.5), TEXT_DIM);
        if matches!(self.source, Source::Image { .. }) && !self.idle {
            let r = Rect::from_min_size(pos2(inner.right() - 112.0, inner.top() + 2.0), vec2(112.0, 22.0));
            ui.scope_builder(egui::UiBuilder::new().max_rect(r), |ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("LENGTH").font(gothic(7.5)).color(TEXT_DIM));
                    ui.add(egui::DragValue::new(&mut self.still_seconds).range(0.5..=86_400.0).speed(0.1).suffix(" s"))
                        .on_hover_text("Length of the animated video in seconds");
                });
            });
        }
        let kw = (inner.width() - 9.0) / 4.0;
        for (k, f) in Format::ALL.into_iter().enumerate() {
            let r = Rect::from_min_size(pos2(inner.left() + k as f32 * (kw + 3.0), inner.top() + 40.0), vec2(kw, 42.0));
            if key_at(ui, r, Id::NULL.with(("fmt", k)), f.key(), self.format == f, true, gothic(8.5)).clicked() {
                self.format = f;
            }
        }
        let er = Rect::from_min_max(pos2(inner.left(), inner.top() + 90.0), pos2(inner.right(), inner.bottom().max(inner.top() + 124.0)));
        if let Some(job) = &self.export {
            let f = job.fraction();
            let bar = Rect::from_min_size(er.min, vec2(er.width() - 78.0, er.height()));
            p.rect(bar, rr(5.0), KEY, Stroke::new(1.0, KEY_EDGE), StrokeKind::Inside);
            p.rect_filled(Rect::from_min_size(bar.min + vec2(2.0, 2.0), vec2((bar.width() - 4.0) * f, bar.height() - 4.0)), rr(4.0), Color32::from_rgb(0x2b, 0x5a, 0x8f));
            p.text(bar.center(), Align2::CENTER_CENTER, format!("{:.0}%", f * 100.0), gothic(11.0), TEXT);
            let cr = Rect::from_min_max(pos2(bar.right() + 6.0, er.top()), er.max);
            let cancel = job.cancel.clone();
            if key_at(ui, cr, Id::NULL.with("cancel"), "CANCEL", false, false, gothic(8.5)).on_hover_text("Cancel export and keep the previous output").clicked() {
                cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                self.status = "CANCELLING EXPORT".into();
            }
        } else {
            let resp = ui.interact(er, Id::NULL.with("export"), Sense::click());
            p.rect(er, rr(5.0), if resp.hovered() { Color32::from_rgb(0x1a, 0x1d, 0x20) } else { Color32::from_rgb(0x12, 0x14, 0x16) }, Stroke::new(1.0, KEY_EDGE), StrokeKind::Inside);
            let (label, colour) = match (&self.source, &self.recorder) {
                (Source::Live { .. }, Some(r)) => {
                    let secs = r.started.elapsed().as_secs();
                    (format!("STOP {}:{:02}", secs / 60, secs % 60), RED)
                }
                (Source::Live { .. }, None) => ("RECORD".to_string(), TEXT),
                _ => ("EXPORT".to_string(), TEXT),
            };
            let g = p.layout_no_wrap(label, gothic(15.0), colour);
            let total = g.size().x + 18.0;
            let start = er.center().x - total / 2.0;
            let c = pos2(start + 4.0, er.center().y);
            p.add(Shape::convex_polygon(vec![c + vec2(-4.0, -5.0), c + vec2(4.0, 0.0), c + vec2(-4.0, 5.0)], TEXT, Stroke::NONE));
            p.galley(pos2(start + 18.0, er.center().y - g.size().y / 2.0), g, TEXT);
            let tip = if self.idle { "Load a video or image to enable export" } else { "Export the processed result (no watermark)" };
            if resp.on_hover_text(tip).clicked() {
                if matches!(self.source, Source::Live { .. }) {
                    self.toggle_live_recording();
                } else {
                    self.start_export();
                }
            }
        }
    }
}
