//! Every control of the effect chain, its range, default and module, plus presets.

use serde_json::{Map, Value};
use std::path::PathBuf;

#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    Knob,
    /// A switch with named positions; the value is the index.
    Choice(&'static [&'static str]),
}

pub struct Def {
    pub key: &'static str,
    pub label: &'static str,
    pub group: Group,
    pub min: f32,
    pub max: f32,
    pub default: f32,
    pub kind: Kind,
}

/// The modules of the rack, in the order the original lists them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Group {
    ColorSignal,
    CircuitBend,
    Damage,
    Noise,
    Waves,
    Screen,
    Capture,
    Convergence,
    Optics,
    Global,
}

impl Group {
    pub const SIGNAL: [Group; 5] = [Group::ColorSignal, Group::CircuitBend, Group::Damage, Group::Noise, Group::Waves];
    pub const SCREEN: [Group; 4] = [Group::Screen, Group::Capture, Group::Convergence, Group::Optics];

    pub fn title(self) -> &'static str {
        match self {
            Group::ColorSignal => "COLOR SIGNAL",
            Group::CircuitBend => "CIRCUIT BEND",
            Group::Damage => "DAMAGE",
            Group::Noise => "NOISE",
            Group::Waves => "WAVES",
            Group::Screen => "SCREEN",
            Group::Capture => "CAPTURE",
            Group::Convergence => "CONVERGENCE",
            Group::Optics => "GLASS & OPTICS",
            Group::Global => "GLOBAL",
        }
    }

    /// The controls a module knob drives and their (low, high) range.
    fn axes(self) -> &'static [(&'static str, f32, f32)] {
        match self {
            Group::ColorSignal => &[
                ("bleed", 1.0, 2.5),
                ("drift", 0.0, 1.2),
                ("burn", 1.0, 2.0),
                ("signalClean", 0.0, 1.0),
                ("chromaBleed", 0.0, 1.0),
                ("rgbShift", 0.0, 1.0),
                ("postSharpen", 0.0, 1.0),
            ],
            Group::CircuitBend => &[("circuitBend", 0.0, 1.0)],
            Group::Damage => &[
                ("jitter", 0.0, 1.0),
                ("tear", 0.0, 1.0),
                ("vroll", 0.0, 1.0),
                ("exposureFlash", 0.0, 1.0),
                ("dropouts", 0.0, 1.0),
                ("humBar", 0.0, 1.0),
                ("ghosting", 0.0, 1.0),
                ("headSwitch", 0.0, 1.0),
                ("topSpectral", 0.0, 1.0),
            ],
            Group::Noise => &[("sparkle", 0.0, 1.0), ("grit", 0.0, 1.0)],
            Group::Waves => &[("modMix", 0.0, 1.0)],
            Group::Screen => &[
                ("tube", 0.0, 1.0),
                ("screenPulsePersistence", 0.35, 1.0),
                ("screenPulseJitter", 0.0, 1.0),
                ("screenPulseEdge", 0.25, 1.0),
                ("soften", 0.0, 1.0),
                ("geoZoom", 0.0, 1.0),
                ("phosphor", 0.0, 1.0),
                ("phosphorBloom", 0.0, 1.0),
                ("flicker", 0.0, 1.0),
            ],
            Group::Capture => &[("screenPulse", 0.0, 1.0), ("moire", 0.0, 1.0), ("rainbowInterference", 0.0, 1.0)],
            Group::Convergence => &[("fringe", 0.0, 1.0), ("phosphorConverge", 0.0, 1.0)],
            Group::Optics => &[("glow", 0.0, 1.0), ("spectral", 0.0, 1.0), ("geoCurve", 0.0, 1.0), ("geoStretch", 0.0, 1.0), ("lensFisheye", 0.0, 1.0)],
            Group::Global => &[],
        }
    }

    /// Settings that move along with the module knob: (control, value at 0, value at 1).
    fn support(self) -> &'static [(&'static str, f32, f32)] {
        match self {
            Group::Waves => &[("modWaves", 50.0, 72.0), ("modReact", 0.0, 0.08)],
            Group::CircuitBend => &[("bendMotion", 0.0, 0.15), ("bendQ", 5.0, 7.0)],
            Group::Capture => &[("screenPulseShutter", 48.0, 96.0)],
            _ => &[],
        }
    }

    /// From nothing, how strongly each control of the module comes up, one weight per axis in
    /// `axes()` order.
    fn voices(self) -> &'static [f32] {
        match self {
            Group::ColorSignal => &[1.0, 0.1, 0.3, 0.0, 0.3, 0.25, 0.2],
            Group::CircuitBend => &[1.0],
            Group::Damage => &[0.18, 1.0, 0.25, 0.2, 0.7, 0.4, 0.35, 0.55, 0.3],
            Group::Noise => &[1.0, 0.65],
            Group::Waves => &[1.0],
            Group::Screen => &[0.45, 0.2, 0.08, 0.25, 0.35, 0.12, 1.0, 0.32, 0.18],
            Group::Capture => &[1.0, 0.35, 0.3],
            Group::Convergence => &[1.0, 0.75],
            Group::Optics => &[0.55, 0.5, 1.0, 0.0, 0.2],
            Group::Global => &[],
        }
    }
}

const BLEND: &[&str] = &["NORMAL", "MULTIPLY", "SCREEN", "ADD"];
const DIR: &[&str] = &["FORWARD", "BACKWARD"];
const ORIENT: &[&str] = &["HORIZONTAL", "VERTICAL"];
const COLOR: &[&str] = &["MONO", "RGB"];
const SCREEN: &[&str] = &["SLOT MASK", "APERTURE GRILLE", "DELTA DOT", "FINE SLOT MASK", "SQUARE GRID"];
pub const DROPOUT: &[&str] = &["WHITE", "BLACK", "LINE HOLD", "CHROMA LOSS", "TBC TEAR"];

macro_rules! d {
    ($k:expr, $l:expr, $g:ident, $min:expr, $max:expr, $def:expr) => {
        Def { key: $k, label: $l, group: Group::$g, min: $min, max: $max, default: $def, kind: Kind::Knob }
    };
    ($k:expr, $l:expr, $g:ident, $choices:expr, $def:expr) => {
        Def {
            key: $k,
            label: $l,
            group: Group::$g,
            min: 0.0,
            max: ($choices.len() - 1) as f32,
            default: $def,
            kind: Kind::Choice($choices),
        }
    };
}

// Defaults are the original's clean state.
pub const DEFS: &[Def] = &[
    d!("bleed", "CHROMA GAIN", ColorSignal, 0.0, 3.0, 1.0),
    d!("burn", "LUMA BURN", ColorSignal, 0.0, 2.0, 1.0),
    d!("drift", "PHASE DRIFT", ColorSignal, 0.0, 1.5, 0.0),
    d!("signalClean", "SIGNAL CLEAN", ColorSignal, 0.0, 1.0, 0.0),
    d!("chromaBleed", "CHROMA BLEED", ColorSignal, 0.0, 1.0, 0.0),
    d!("rgbShift", "RGB SPLIT", ColorSignal, 0.0, 1.0, 0.0),
    d!("postSharpen", "PEAKING", ColorSignal, 0.0, 1.0, 0.0),
    d!("circuitBend", "BEND", CircuitBend, 0.0, 1.0, 0.0),
    d!("bendFrequencyMHz", "FREQUENCY MHZ", CircuitBend, 0.05, 5.5, 0.914),
    d!("bendQ", "RESONANCE", CircuitBend, 0.25, 30.0, 5.0),
    d!("bendBoostDB", "BOOST DB", CircuitBend, 0.0, 48.0, 24.0),
    d!("bendThresholdLowIRE", "LOW IRE", CircuitBend, -40.0, 160.0, 18.0),
    d!("bendThresholdHighIRE", "HIGH IRE", CircuitBend, -40.0, 160.0, 160.0),
    d!("bendMotion", "MOTION", CircuitBend, -1.0, 1.0, 0.0),
    d!("bendFeedback", "FEEDBACK", CircuitBend, 0.0, 0.25, 0.009),
    d!("bendFeedbackDelay", "DELAY", CircuitBend, 0.0, 1.5, 0.0),
    d!("jitter", "SHAKE", Damage, 0.0, 1.0, 0.0),
    d!("tear", "TEAR", Damage, 0.0, 1.0, 0.0),
    d!("vroll", "VERTICAL ROLL", Damage, 0.0, 1.0, 0.0),
    d!("dropouts", "DROPOUTS", Damage, 0.0, 1.0, 0.0),
    d!("dropoutStyle", "DROPOUT STYLE", Damage, DROPOUT, 0.0),
    d!("ghosting", "SIGNAL ECHO", Damage, 0.0, 1.0, 0.0),
    d!("exposureFlash", "EXPOSURE FLASH", Damage, 0.0, 1.0, 0.0),
    d!("humBar", "HUM BAND", Damage, 0.0, 1.0, 0.0),
    d!("headSwitch", "HEAD-SWITCH BAND", Damage, 0.0, 1.0, 0.0),
    d!("topSpectral", "TOP-LINE SPECTRAL", Damage, 0.0, 1.0, 0.0),
    d!("sparkle", "RF SPECKS", Noise, 0.0, 1.0, 0.0),
    d!("grit", "TAPE GRAIN", Noise, 0.0, 1.0, 0.0),
    d!("sparkleBand", "SPECK SIZE", Noise, 0.0, 1.0, 0.5),
    d!("modMix", "WAVE MIX", Waves, 0.0, 1.0, 0.0),
    d!("modWaves", "WAVE COUNT", Waves, 10.0, 150.0, 50.0),
    d!("modReact", "REACT", Waves, 0.0, 0.5, 0.0),
    d!("modLowpass", "LOWPASS", Waves, 1.0, 8.0, 3.0),
    d!("feedbackMix", "FEEDBACK", Waves, 0.0, 1.0, 0.0),
    d!("fbZoom", "FB ZOOM", Waves, 0.9, 1.1, 1.0),
    d!("fbRotate", "FB ROTATE", Waves, -0.2, 0.2, 0.0),
    d!("fbHue", "FB HUE", Waves, -0.5, 0.5, 0.0),
    d!("fbTrails", "FB TRAILS", Waves, 0.5, 0.99, 0.94),
    d!("modBlend", "BLEND", Waves, BLEND, 2.0),
    d!("modDir", "DIRECTION", Waves, DIR, 0.0),
    d!("modOrient", "ORIENTATION", Waves, ORIENT, 1.0),
    d!("modColor", "COLOR", Waves, COLOR, 1.0),
    d!("tube", "TUBE", Screen, 0.0, 1.0, 0.0),
    d!("phosphor", "MASK DEPTH", Screen, 0.0, 1.0, 0.0),
    d!("maskScale", "MASK SCALE", Screen, 0.0, 1.0, 0.5),
    d!("phosphorBloom", "PHOSPHOR BLOOM", Screen, 0.0, 1.0, 0.0),
    d!("soften", "BEAM SOFTNESS", Screen, 0.0, 1.0, 0.0),
    d!("geoZoom", "OVERSCAN", Screen, 0.0, 1.0, 0.0),
    d!("flicker", "FLICKER", Screen, 0.0, 1.0, 0.0),
    d!("screenPulsePersistence", "PERSISTENCE", Screen, 0.0, 1.0, 0.35),
    d!("screenPulseJitter", "SYNC", Screen, 0.0, 1.0, 0.0),
    d!("screenPulseEdge", "HV RIPPLE", Screen, 0.0, 1.0, 0.25),
    d!("displayType", "SCREEN TYPE", Screen, SCREEN, 0.0),
    d!("screenPulse", "SCREEN PULSE", Capture, 0.0, 1.0, 0.0),
    d!("screenPulseShutter", "SHUTTER 1/X", Capture, 24.0, 500.0, 48.0),
    d!("screenPulseRefresh", "REFRESH HZ", Capture, 25.0, 120.0, 59.94),
    d!("screenPulseReadout", "READOUT", Capture, 0.0, 1.5, 0.0),
    d!("screenPulseRatio", "RATIO", Capture, 0.0, 2.0, 1.0),
    d!("screenPulseAngle", "BAND ANGLE", Capture, -45.0, 45.0, 0.0),
    d!("moire", "MOIRE", Capture, 0.0, 1.0, 0.0),
    d!("rainbowInterference", "RAINBOW", Capture, 0.0, 1.0, 0.0),
    d!("fringe", "BEAM CONVERGENCE", Convergence, 0.0, 1.0, 0.0),
    d!("beamConvergeAngle", "BEAM ANGLE", Convergence, 0.0, 360.0, 333.4349),
    d!("phosphorConverge", "PHOSPHOR CONVERGENCE", Convergence, 0.0, 1.0, 0.0),
    d!("glow", "GLOW", Optics, 0.0, 1.0, 0.0),
    d!("halation", "HALATION", Optics, 0.0, 1.0, 0.0),
    d!("postGlow", "SIGNAL GLOW", Optics, 0.0, 1.0, 0.0),
    d!("spectral", "SPECTRAL", Optics, 0.0, 1.0, 0.0),
    d!("geoCurve", "CURVATURE", Optics, 0.0, 1.0, 0.0),
    d!("geoStretch", "SCAN LINEARITY", Optics, 0.0, 1.0, 0.0),
    d!("lensFisheye", "LENS", Optics, -1.0, 1.0, 0.0),
    // master output limits
    d!("outputBrightness", "BRIGHTNESS", Global, -0.25, 0.25, 0.0),
    d!("outputContrast", "CONTRAST", Global, 0.5, 2.5, 1.0),
    d!("dryWet", "DRY / WET", Global, 0.0, 1.0, 1.0),
];

pub const N: usize = DEFS.len();

/// Index of a control by key (compile-time list, so a missing key is a bug).
pub fn idx(key: &str) -> usize {
    DEFS.iter().position(|d| d.key == key).unwrap_or_else(|| panic!("unknown control {key}"))
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    Sd,
    Hi,
    Full,
}

impl Tier {
    pub fn from_f32(v: f32) -> Tier {
        match v.round() as i32 {
            1 => Tier::Hi,
            2 => Tier::Full,
            _ => Tier::Sd,
        }
    }
    pub fn as_f32(self) -> f32 {
        match self {
            Tier::Sd => 0.0,
            Tier::Hi => 1.0,
            Tier::Full => 2.0,
        }
    }
}

#[derive(Clone, PartialEq)]
pub struct Params {
    pub v: [f32; N],
    pub tier: Tier,
}

impl Default for Params {
    fn default() -> Self {
        let mut v = [0.0; N];
        for (i, d) in DEFS.iter().enumerate() {
            v[i] = d.default;
        }
        Params { v, tier: Tier::Full }
    }
}

impl Params {
    pub fn get(&self, key: &str) -> f32 {
        self.v[idx(key)]
    }

    /// The module knob: the largest deviation of any of its controls across its range.
    pub fn voice(&self, g: Group) -> f32 {
        g.axes().iter().map(|(k, lo, hi)| ((self.get(k) - lo) / (hi - lo)).abs()).fold(0.0, f32::max).min(1.0)
    }

    /// Turn the module knob: every control in the module is rebased together so the largest one
    /// lands on the new value; the support settings follow linearly. From nothing, the canonical voices come up.
    pub fn set_voice(&mut self, g: Group, v: f32) {
        let v = v.clamp(0.0, 1.0);
        let old = self.voice(g);
        if old > 1e-9 {
            for (k, lo, hi) in g.axes() {
                let i = idx(k);
                let n = (self.v[i] - lo) / (hi - lo) * v / old;
                self.v[i] = (lo + n * (hi - lo)).clamp(DEFS[i].min, DEFS[i].max);
            }
        } else {
            for ((k, lo, hi), w) in g.axes().iter().zip(g.voices()) {
                let i = idx(k);
                self.v[i] = (lo + v * w * (hi - lo)).clamp(DEFS[i].min, DEFS[i].max);
            }
        }
        for (k, at0, at1) in g.support() {
            let i = idx(k);
            self.v[i] = at0 + (at1 - at0) * v;
        }
    }

    /// DRY / WET: below 1 every effect amount is scaled by the mix, gains are pulled toward 1 and signal clean is
    /// pushed toward clean, so the picture moves toward the source through the signal chain
    /// instead of a crossfade.
    pub fn intelligent_mix(&self) -> Params {
        let m = self.get("dryWet").clamp(0.0, 1.0);
        let mut p = self.clone();
        p.v[idx("dryWet")] = 1.0;
        if m >= 1.0 {
            return p;
        }
        const AMOUNTS: &[&str] = &[
            "sparkle", "grit", "drift", "postSharpen", "feedbackMix", "fbRotate", "fbHue", "tube", "phosphor", "geoCurve", "glow",
            "modReact", "soften", "moire", "modMix", "geoStretch", "rgbShift", "geoZoom", "jitter", "tear", "vroll", "dropouts",
            "humBar", "ghosting", "headSwitch", "fringe", "flicker", "spectral", "chromaBleed", "rainbowInterference",
            "exposureFlash", "screenPulse", "topSpectral", "phosphorConverge", "lensFisheye", "screenPulseJitter", "circuitBend",
            "phosphorBloom", "postGlow", "halation",
        ];
        for k in AMOUNTS {
            p.v[idx(k)] *= m;
        }
        for k in ["bleed", "burn", "fbZoom"] {
            let i = idx(k);
            p.v[i] = 1.0 + m * (p.v[i] - 1.0);
        }
        let i = idx("signalClean");
        p.v[i] = 1.0 - m * (1.0 - p.v[i]);
        p
    }

    pub fn reset_group(&mut self, g: Group) {
        for (i, d) in DEFS.iter().enumerate() {
            if d.group == g {
                self.v[i] = d.default;
            }
        }
    }

    pub fn from_values(values: &Map<String, Value>) -> Params {
        let mut p = Params::default();
        for (i, d) in DEFS.iter().enumerate() {
            if let Some(x) = values.get(d.key).and_then(|x| x.as_f64()) {
                p.v[i] = x as f32;
            }
        }
        if let Some(t) = values.get("rasterTier").and_then(|x| x.as_f64()) {
            p.tier = Tier::from_f32(t as f32);
        }
        p
    }

    pub fn to_values(&self) -> Map<String, Value> {
        let mut m = Map::new();
        for (i, d) in DEFS.iter().enumerate() {
            m.insert(d.key.into(), Value::from(self.v[i] as f64));
        }
        m.insert("rasterTier".into(), Value::from(self.tier.as_f32() as f64));
        m
    }
}

#[derive(Clone)]
pub struct Preset {
    /// stays the same when the preset is renamed (factory presets use their name)
    pub id: String,
    pub name: String,
    pub params: Params,
    pub factory: bool,
    /// shelf and one-line description given by the preset file itself
    pub category: Option<Category>,
    pub description: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Camera,
    Tv,
    Fx,
    Saved,
}

impl Category {
    pub const ALL: [Category; 4] = [Category::Camera, Category::Tv, Category::Fx, Category::Saved];
    pub fn title(self) -> &'static str {
        match self {
            Category::Camera => "CAMERA LOOKS",
            Category::Tv => "TV LOOKS",
            Category::Fx => "SIGNAL FX",
            Category::Saved => "SAVED LOOKS",
        }
    }
}

impl Preset {
    /// Shelf and one-line description for the preset list, as given by the preset file.
    pub fn info(&self) -> (Category, &str) {
        if !self.factory {
            return (Category::Saved, "Custom signal chain");
        }
        if let (Some(c), Some(d)) = (self.category, self.description.as_deref()) {
            return (c, d);
        }
        (Category::Fx, "Custom signal chain")
    }
}

pub fn factory_presets() -> Vec<Preset> {
    // chosen by build.rs: the private bank when present, else KILN's own looks
    let text = include_str!(concat!(env!("OUT_DIR"), "/factory-presets.json"));
    let root: Value = serde_json::from_str(text).expect("factory presets");
    root["presets"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| {
            Some(Preset {
                id: p["name"].as_str()?.to_string(),
                name: p["name"].as_str()?.to_string(),
                params: Params::from_values(p["values"].as_object()?),
                factory: true,
                category: match p["category"].as_str() {
                    Some("camera") => Some(Category::Camera),
                    Some("tv") => Some(Category::Tv),
                    Some("fx") => Some(Category::Fx),
                    _ => None,
                },
                description: p["description"].as_str().map(str::to_string),
            })
        })
        .collect()
}

/// Where KILN keeps the person's presets and last session.
pub fn support_dir() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    home.join("Library/Application Support/KILN")
}
