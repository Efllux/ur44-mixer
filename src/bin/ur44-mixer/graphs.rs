//! Simple graphs: EQ response, compressor transfer curve, reverb decay.

use iced::widget::canvas::{self, Frame, Geometry, Path, Stroke, Text};
use iced::{Color, Point, Rectangle, Renderer, Size, Theme, mouse};
use ur44::units;

const GRID: Color = Color { r: 0.30, g: 0.32, b: 0.42, a: 1.0 };
const LABEL: Color = Color { r: 0.62, g: 0.65, b: 0.75, a: 1.0 };
const ACCENT: Color = Color { r: 0.38, g: 0.80, b: 0.92, a: 1.0 };
const DIM: Color = Color { r: 0.45, g: 0.48, b: 0.58, a: 1.0 };

pub fn num(s: &str) -> f32 {
    let s = s.trim();
    if let Some(k) = s.strip_suffix('k') {
        return k.parse::<f32>().unwrap_or(0.0) * 1000.0;
    }
    if s.eq_ignore_ascii_case("INF") {
        return f32::INFINITY;
    }
    s.parse().unwrap_or(0.0)
}

fn label(frame: &mut Frame, s: &str, at: Point) {
    frame.fill_text(Text { content: s.to_string(), position: at, color: LABEL, size: iced::Pixels(10.0), ..Text::default() });
}

fn line(frame: &mut Frame, pts: &[Point], color: Color, width: f32) {
    if pts.len() < 2 {
        return;
    }
    let path = Path::new(|b| {
        b.move_to(pts[0]);
        for p in &pts[1..] {
            b.line_to(*p);
        }
    });
    frame.stroke(&path, Stroke::default().with_color(color).with_width(width));
}

// ---------------- EQ (Yamaha's FILTER2PEQ maths, see ur44::dsp) ----------------

/// Raw parameter values of one channel strip's EQ plus the current sample rate.
#[derive(Debug, Clone, Copy)]
pub struct Eq {
    pub on: bool,
    pub fs: f64,
    pub low_f: i32,
    pub low_g: i32,
    pub mid_q: i32,
    pub mid_f: i32,
    pub mid_g: i32,
    pub high_f: i32,
    pub high_g: i32,
}

impl<M> canvas::Program<M> for Eq {
    type State = ();
    fn draw(&self, _: &(), renderer: &Renderer, _: &Theme, bounds: Rectangle, _: mouse::Cursor) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let Size { width: w, height: h } = bounds.size();
        let (l, r, t, b) = (30.0, w - 6.0, 6.0, h - 16.0);
        let x_of = |f: f64| l + (r - l) * ((f / 20.0).log10() / 3.0) as f32;
        let y_of = |db: f64| t + (b - t) * (0.5 - (db.clamp(-20.0, 20.0) / 40.0) as f32);
        for db in [-18.0, -12.0, -6.0, 0.0, 6.0, 12.0, 18.0] {
            let y = y_of(db);
            line(&mut frame, &[Point::new(l, y), Point::new(r, y)], if db == 0.0 { DIM } else { GRID }, 1.0);
            label(&mut frame, &format!("{db:+}"), Point::new(2.0, y - 6.0));
        }
        for (f, s) in [(100.0, "100"), (1000.0, "1k"), (10000.0, "10k")] {
            let x = x_of(f);
            line(&mut frame, &[Point::new(x, t), Point::new(x, b)], GRID, 1.0);
            label(&mut frame, s, Point::new(x + 2.0, b + 3.0));
        }
        let eq = ur44::dsp::Eq::new(self.fs, (self.low_f, self.low_g), (self.mid_f, self.mid_g, self.mid_q), (self.high_f, self.high_g));
        let curve = |band: usize| -> Vec<Point> {
            (0..=240)
                .map(|i| {
                    let f = 20.0 * 1000f64.powf(f64::from(i) / 240.0);
                    Point::new(x_of(f), y_of(if self.on { eq.response_db(f, band) } else { 0.0 }))
                })
                .collect()
        };
        if self.on {
            // Per-band curves (dspMixFx draws these under the total).
            let colors = [Color::from_rgb(0.96, 0.62, 0.25), Color::from_rgb(0.55, 0.85, 0.45), Color::from_rgb(0.75, 0.55, 0.95)];
            for (band, c) in (1..=3).zip(colors) {
                line(&mut frame, &curve(band), Color { a: 0.55, ..c }, 1.0);
            }
        }
        line(&mut frame, &curve(0), if self.on { ACCENT } else { DIM }, 2.0);
        vec![frame.into_geometry()]
    }
}

// ---------------- Compressor (Channel_Strip::GetCompCurve) ----------------

#[derive(Debug, Clone, Copy)]
pub struct Comp {
    pub on: bool,
    pub drive: i32,
    pub ratio: i32,
    pub knee: i32,
    pub thresh_ofst: i32,
}

impl<M> canvas::Program<M> for Comp {
    type State = ();
    fn draw(&self, _: &(), renderer: &Renderer, _: &Theme, bounds: Rectangle, _: mouse::Cursor) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let Size { width: w, height: h } = bounds.size();
        let side = (w - 36.0).min(h - 22.0);
        let (l, t) = (30.0, 6.0);
        let (r, b) = (l + side, t + side);
        let xy = |inp: f64, out: f64| {
            Point::new(l + side * ((inp + 60.0) / 60.0) as f32, b - side * ((out.clamp(-60.0, 0.0) + 60.0) / 60.0) as f32)
        };
        for db in [-60.0, -40.0, -20.0, 0.0] {
            let p = xy(db, db);
            line(&mut frame, &[Point::new(l, p.y), Point::new(r, p.y)], GRID, 1.0);
            line(&mut frame, &[Point::new(p.x, t), Point::new(p.x, b)], GRID, 1.0);
            label(&mut frame, &format!("{db}"), Point::new(2.0, p.y - 6.0));
        }
        line(&mut frame, &[xy(-60.0, -60.0), xy(0.0, 0.0)], DIM, 1.0);
        let c = ur44::dsp::Comp::new(self.drive, self.ratio, self.knee, self.thresh_ofst);
        let pts: Vec<Point> = (0..=240)
            .map(|i| {
                let x = -60.0 + f64::from(i) * 0.25;
                xy(x, if self.on { c.curve(x) } else { x })
            })
            .collect();
        line(&mut frame, &pts, if self.on { ACCENT } else { DIM }, 2.0);
        label(&mut frame, &format!("threshold {:.1} dB", c.threshold_db()), Point::new(l + 4.0, b + 3.0));
        vec![frame.into_geometry()]
    }
}

// ---------------- Reverb ----------------

#[derive(Debug, Clone, Copy)]
pub struct Reverb {
    /// Actual reverb time (GetActualReverbTime).
    pub seconds: f64,
    pub ini_dly: i32,
    pub hi_ratio: i32,
    pub low_ratio: i32,
}

impl<M> canvas::Program<M> for Reverb {
    type State = ();
    fn draw(&self, _: &(), renderer: &Renderer, _: &Theme, bounds: Rectangle, _: mouse::Cursor) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let Size { width: w, height: h } = bounds.size();
        let (l, r, t, b) = (44.0, w - 8.0, 22.0, h - 32.0);
        let rt = self.seconds as f32;
        let delay = num(units::REV_INIDLY_MS[self.ini_dly.clamp(0, 127) as usize]) / 1000.0;
        let hi = num(units::REV_RATIO[(self.hi_ratio - 1).clamp(0, 13) as usize]);
        let lo = num(units::REV_RATIO[(self.low_ratio - 1).clamp(0, 13) as usize]);
        let span = (delay + rt * hi.max(lo).max(1.0)) * 1.15;
        let x_of = |s: f32| l + (r - l) * (s / span);
        let y_of = |db: f32| t + (b - t) * (-db / 60.0);
        for db in [0.0, -20.0, -40.0, -60.0] {
            let y = y_of(db);
            line(&mut frame, &[Point::new(l, y), Point::new(r, y)], GRID, 1.0);
            label(&mut frame, &format!("{db}"), Point::new(18.0, y - 6.0));
        }
        // Time ticks every 0.5 / 1 / 2 s, whichever gives a readable count.
        let step = if span <= 4.0 { 0.5 } else if span <= 10.0 { 1.0 } else { 2.0 };
        let mut s = 0.0f32;
        while s <= span {
            let x = x_of(s);
            line(&mut frame, &[Point::new(x, b), Point::new(x, b + 4.0)], DIM, 1.0);
            label(&mut frame, &format!("{s}"), Point::new(x - 4.0, b + 5.0));
            s += step;
        }
        frame.fill_text(Text { content: "Time (s)".into(), position: Point::new((l + r) / 2.0 - 20.0, b + 17.0), color: LABEL, size: iced::Pixels(11.0), ..Text::default() });
        frame.fill_text(Text { content: "Level (dB)".into(), position: Point::new(4.0, 2.0), color: LABEL, size: iced::Pixels(11.0), ..Text::default() });
        let bands = [("Low", rt * lo, Color::from_rgb(0.96, 0.62, 0.25)), ("Mid", rt, ACCENT), ("High", rt * hi, Color::from_rgb(0.75, 0.55, 0.95))];
        for (i, (name, t60, c)) in bands.iter().enumerate() {
            line(&mut frame, &[Point::new(l, y_of(-60.0)), Point::new(x_of(delay), y_of(-60.0)), Point::new(x_of(delay), y_of(0.0)), Point::new(x_of(delay + t60), y_of(-60.0))], *c, 2.0);
            frame.fill_text(Text {
                content: format!("{name} {t60:.1} s"),
                position: Point::new(r - 90.0, t + 4.0 + 14.0 * i as f32),
                color: *c,
                size: iced::Pixels(11.0),
                ..Text::default()
            });
        }
        vec![frame.into_geometry()]
    }
}


// ---------------- Fader scale + meters ----------------

/// Fader position (0..=127) of a gain in dB, following the UR44's fader law (units::FADER_DB).
pub fn fader_pos(db: f32) -> f32 {
    let t = &units::FADER_DB;
    let at = |i: usize| t[i].parse::<f32>().unwrap_or(-80.0);
    if db <= at(1) {
        // -INF (0) .. -74 dB (1); anything quieter than -80 sits at the bottom.
        return ((db + 80.0) / (at(1) + 80.0)).clamp(0.0, 1.0);
    }
    for i in 2..t.len() {
        let (a, b) = (at(i - 1), at(i));
        if db <= b {
            return (i - 1) as f32 + (db - a) / (b - a);
        }
    }
    (t.len() - 1) as f32
}

/// Layout of the fader unit (pixels): dB labels, fader lane, gap, meters.
pub const SCALE_W: f32 = 26.0;
pub const LANE_W: f32 = 28.0;
const GAP: f32 = 6.0;
const METER_GAP: f32 = 3.0;

pub fn meter_w(n: usize) -> f32 {
    if n >= 2 { 8.0 } else { 11.0 }
}

/// Total width of a fader unit with `n` meters.
pub fn unit_width(n: usize) -> f32 {
    let m = if n == 0 { 0.0 } else { GAP + n as f32 * meter_w(n) + (n as f32 - 1.0) * METER_GAP };
    SCALE_W + LANE_W + m + if n == 0 { SCALE_W } else { 0.0 }
}

/// Draws the shared dB scale for a fader and its meters. The meters use the same scale as the
/// fader (a level of -12 dBFS lines up with the fader's -12 dB mark); the clip LED sits at the top.
pub struct FaderScale {
    /// (level, peak-hold) raw meter values.
    pub meters: Vec<(u32, u32)>,
}

const TICKS: [(f32, &str); 7] = [(6.0, "+6"), (0.0, "0"), (-6.0, "-6"), (-12.0, "-12"), (-24.0, "-24"), (-48.0, "-48"), (f32::NEG_INFINITY, "-\u{221e}")];

const GREEN: Color = Color { r: 0.36, g: 0.84, b: 0.52, a: 1.0 };
const YELLOW: Color = Color { r: 0.98, g: 0.80, b: 0.28, a: 1.0 };
const RED: Color = Color { r: 0.96, g: 0.30, b: 0.34, a: 1.0 };

impl<M> canvas::Program<M> for FaderScale {
    type State = ();
    fn draw(&self, _: &(), renderer: &Renderer, _: &Theme, bounds: Rectangle, _: mouse::Cursor) -> Vec<Geometry> {
        use ur44::protocol::meter_db;
        let mut frame = Frame::new(renderer, bounds.size());
        let h = bounds.height;
        let cap = f32::from(crate::glass::CAP_H);
        // Centre of the fader cap at position p, exactly as iced's vertical slider places it.
        let y = |p: f32| cap / 2.0 + (h - cap) * (1.0 - p / 127.0);
        let lane = SCALE_W;
        let n = self.meters.len();
        let mw = meter_w(n);
        let mx = lane + LANE_W + GAP;
        let mend = mx + n as f32 * mw + n.saturating_sub(1) as f32 * METER_GAP;
        for (db, s) in TICKS {
            let p = if db.is_finite() { fader_pos(db) } else { 0.0 };
            let yy = y(p).round() + 0.5;
            let unity = db == 0.0;
            let c = if unity { Color::from_rgb(0.92, 0.94, 0.98) } else { LABEL };
            // Ticks either side of the fader lane, and across the meters.
            let tick = |frame: &mut Frame, x0: f32, x1: f32, a: f32| {
                frame.stroke(
                    &Path::line(Point::new(x0, yy), Point::new(x1, yy)),
                    Stroke::default().with_color(Color { a, ..c }).with_width(if unity { 1.5 } else { 1.0 }),
                );
            };
            tick(&mut frame, lane - 3.0, lane + 6.0, 0.9);
            tick(&mut frame, lane + LANE_W - 6.0, lane + LANE_W + 2.0, 0.9);
            if n > 0 {
                tick(&mut frame, mend + 1.0, mend + 4.0, 0.7);
            }
            frame.fill_text(Text {
                content: s.to_string(),
                position: Point::new(lane - 5.0, yy),
                color: c,
                size: iced::Pixels(if unity { 11.0 } else { 10.0 }),
                horizontal_alignment: iced::alignment::Horizontal::Right,
                vertical_alignment: iced::alignment::Vertical::Center,
                ..Text::default()
            });
        }
        // Meters: trough from -inf up to 0 dBFS, clip LED above.
        let top = y(fader_pos(0.0));
        let bottom = y(0.0);
        let led_h = 7.0;
        let led_y = (top - 10.0 - led_h).max(0.0);
        let level_y = |db: f32| y(fader_pos(db.min(0.0))).max(top);
        for (i, (lv, pk)) in self.meters.iter().enumerate() {
            let x = mx + i as f32 * (mw + METER_GAP);
            frame.fill_rectangle(Point::new(x, top), Size::new(mw, bottom - top), Color { a: 0.45, ..Color::BLACK });
            let l = meter_db(*lv);
            let p = meter_db(*pk).max(l);
            // Segments: green up to -6 dBFS, yellow to -1, red above.
            let zones = [(-80.0, -6.0, GREEN), (-6.0, -1.0, YELLOW), (-1.0, 0.0, RED)];
            for (lo, hi, c) in zones {
                if l > lo {
                    let y0 = level_y(l.min(hi));
                    let y1 = level_y(lo);
                    if y1 > y0 {
                        frame.fill_rectangle(Point::new(x, y0), Size::new(mw, y1 - y0), c);
                    }
                }
            }
            if p > -79.0 {
                let c = if p > -1.0 { RED } else if p > -6.0 { YELLOW } else { Color::from_rgb(0.90, 0.92, 0.96) };
                frame.fill_rectangle(Point::new(x, level_y(p) - 1.0), Size::new(mw, 2.0), c);
            }
            let clip = p >= -0.5;
            frame.fill(
                &Path::rounded_rectangle(Point::new(x, led_y), Size::new(mw, led_h), 2.0.into()),
                if clip { RED } else { Color::from_rgb(0.19, 0.20, 0.25) },
            );
        }
        vec![frame.into_geometry()]
    }
}
