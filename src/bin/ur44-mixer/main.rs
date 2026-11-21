//! UR44 Mixer — a modern replacement for dspMixFx UR44.
//! Talks to the UR44's DSP over the class-compliant MIDI "Port 2" (no Yamaha driver needed).

// Windows release builds are GUI apps: no console window.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod glass;
mod graphs;
mod os;
mod prefs;
mod tray;
mod worker;

use iced::widget::{
    canvas,
    Space, button, column, container, horizontal_space, row, scrollable, slider, text, text_input,
    tooltip, vertical_rule, vertical_slider,
};
use glass::pick_list;
use iced::{Alignment, Color, Element, Length, Subscription, Theme, window};
use std::sync::mpsc::Sender;
use ur44::logic::{self, FxKind, FxPos, Values, Writes};
use ur44::protocol::{meter_db, meter_slot};
use ur44::{params, scene, ssd, units};
use worker::{Command, Event};

pub fn main() -> iced::Result {
    let background = std::env::args().any(|a| a == "--background");
    // Screenshot rig (UR44_RIG=1): a separate instance with no socket, tray, saved state or glass.
    if rig_mode() {
        return iced::daemon(App::title, App::update, App::view)
            .subscription(App::subscription)
            .theme(|_, _| glass::app_theme())
            .style(|app: &App, theme: &Theme| iced::daemon::Appearance {
                background_color: glass::background(app.window_glass, theme),
                text_color: theme.palette().text,
            })
            .run_with(move || App::new(false));
    }
    // Single instance: if an agent is already running, ask it to show its window and exit.
    if ur44::ipc::send(if background { "ping" } else { "show" }) {
        return Ok(());
    }
    let _ = std::fs::create_dir_all(prefs::support_dir());
    worker::log_start();
    match ur44::ipc::Listener::bind() {
        Ok(l) => {
            let _ = tray::LISTENER.set(l);
        }
        Err(e) => worker::log(&format!("launcher channel unavailable: {e}")),
    }
    iced::daemon(App::title, App::update, App::view)
        .subscription(App::subscription)
        .theme(|_, _| glass::app_theme())
        .style(|app: &App, theme: &Theme| iced::daemon::Appearance {
            background_color: glass::background(app.window_glass, theme),
            text_color: theme.palette().text,
        })
        .run_with(move || App::new(background))
}

fn rig_tick() -> iced::Task<Msg> {
    iced::Task::perform(async { std::thread::sleep(std::time::Duration::from_millis(500)) }, |_| Msg::RigTick)
}

/// Write an RGBA screenshot as a 24-bit BMP (no image dependency; convert with `sips`).
fn write_bmp(path: &std::path::Path, shot: &window::Screenshot) -> std::io::Result<()> {
    let (w, h) = (shot.size.width as usize, shot.size.height as usize);
    let row = (w * 3 + 3) & !3;
    let data = row * h;
    let mut out = Vec::with_capacity(54 + data);
    let le32 = |v: u32| v.to_le_bytes();
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&le32((54 + data) as u32));
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&le32(54));
    out.extend_from_slice(&le32(40));
    out.extend_from_slice(&le32(w as u32));
    out.extend_from_slice(&le32(h as u32));
    out.extend_from_slice(&[1, 0, 24, 0]);
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&le32(data as u32));
    out.extend_from_slice(&[0; 16]);
    let px = &shot.bytes;
    for y in (0..h).rev() {
        let start = out.len();
        for x in 0..w {
            let i = (y * w + x) * 4;
            out.extend_from_slice(&[px[i + 2], px[i + 1], px[i]]);
        }
        out.resize(start + row, 0);
    }
    std::fs::write(path, out)
}

fn rig_mode() -> bool {
    std::env::var_os("UR44_RIG").is_some()
}

/// Fixed, plausible meter levels for rig screenshots (raw units: dB = v / 102.4 - 80).
fn rig_meters() -> Vec<u32> {
    let raw = |db: f32| ((db + 80.0) * 102.4) as u32;
    let mut m = vec![0u32; 44];
    let pairs: [(usize, f32, f32); 15] = [
        (0, -18.0, -12.0), (2, -24.0, -20.0), (4, -9.0, -5.0), (6, -42.0, -36.0), (8, -14.0, -10.0), (10, -15.0, -11.0),
        (12, -20.0, -16.0), (14, -21.0, -17.0), (16, -10.0, -6.0), (18, -11.0, -7.0),
        (28, -12.0, -8.0), (30, -13.0, -9.0), (32, -30.0, -26.0), (34, -31.0, -27.0), (40, -80.0, -80.0),
    ];
    for (slot, l, p) in pairs {
        m[slot] = raw(l);
        m[slot + 1] = raw(p);
    }
    for s in 0..4 {
        m[20 + 2 * s] = 3903; // gain reduction: idle
    }
    m
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Page {
    #[default]
    Mixer,
    Strip,
    Guitar,
    Reverb,
    Scenes,
    Settings,
}

#[derive(Debug, Clone)]
enum Conn {
    Searching(String),
    Loading(usize, usize),
    Ready(String),
}

struct App {
    cmd: Option<Sender<Command>>,
    values: Values,
    conn: Conn,
    mix: u8,
    page: Page,
    meters: Vec<u32>,
    strip: u16,
    presets: Vec<ssd::SweetSpot>,
    scenes: Vec<scene::Scene>,
    scene_name: String,
    notice: Option<String>,
    /// Waiting screen: technical details expanded.
    details: bool,
    /// Mixer: the input shown in the inspector beside the strips.
    inspect: Option<u16>,
    /// Effect pages: show the editor of an effect that is not on any input.
    preview: bool,
    // Background agent
    window: Option<window::Id>,
    prefs: prefs::Prefs,
    /// The open window was created with the glass background.
    window_glass: bool,
    /// Screenshot rig: fixed meters, nothing saved.
    rig: bool,
    rig_ticks: u32,
    rig_ready_ticks: u32,
    tray: Option<tray::Tray>,
    restored: bool,
    dirty: bool,
    last_save: std::time::Instant,
}

#[derive(Debug, Clone)]
enum Msg {
    Worker(Event),
    Init,
    Incoming(tray::Incoming),
    ShowWindow(Page),
    WindowOpened(window::Id),
    CloseRequested(window::Id),
    WindowClosed(window::Id),
    Quit,
    PrefLogin(bool),
    PrefTray(prefs::TrayMode),
    PrefRestore(bool),
    PrefGlass(bool),
    RigTick,
    RigShot(window::Screenshot),
    GlassInstalled(&'static str),
    Set(&'static str, u16, i32),
    SetAll(&'static str, i32),
    Insert(u16, FxKind, FxPos),
    EditFx(u16),
    Mix(u8),
    Page(Page),
    Strip(u16),
    SweetSpot(u16, usize),
    SceneName(String),
    SceneStore,
    SceneRecall(usize),
    SceneDelete(usize),
    ScenesFolder,
    Dismiss,
    Details(bool),
    /// Open (Some) or close (None) the input inspector on the mixer.
    Inspect(Option<u16>),
    /// Go to the mixer with this input's inspector open.
    ShowInput(u16),
    /// Put an effect on an input from an effect page, then edit it there.
    Assign(u16, FxKind),
    Preview(bool),
}

const FX_CHOICES: [&str; 3] = ["No effect", "Ch Strip", "Guitar Amp"];

fn mixp(prefix: &str, mix: u8, suffix: &str) -> &'static str {
    let name = format!("{prefix}{mix}{suffix}");
    params::by_name(&name).unwrap_or_else(|| panic!("unknown parameter {name}")).name
}

impl App {
    fn new(background: bool) -> (Self, iced::Task<Msg>) {
        // Developer aid: UR44_PAGE=strip|guitar|reverb|scenes|settings opens that page first
        // (rig: mixer-inspect = mixer with input 1's inspector open, same as UR44_INSPECT=1).
        let page_env = std::env::var("UR44_PAGE").unwrap_or_default();
        let page = match page_env.as_str() {
            "strip" => Page::Strip,
            "guitar" => Page::Guitar,
            "reverb" => Page::Reverb,
            "scenes" => Page::Scenes,
            "settings" => Page::Settings,
            _ => Page::Mixer,
        };
        let rig = rig_mode();
        let mut prefs = prefs::Prefs::load();
        if rig {
            prefs.glass = false;
            prefs.tray = prefs::TrayMode::Hidden;
            prefs.restore_on_connect = false;
        }
        let mix = if std::env::var("UR44_MIX").as_deref() == Ok("2") { 2 } else { 1 };
        let app = App {
            cmd: None,
            values: Values::new(),
            conn: Conn::Searching("Looking for the UR44…".into()),
            mix,
            page,
            meters: if rig { rig_meters() } else { vec![0; 44] },
            strip: 0,
            presets: ssd::installed(),
            scenes: scene::list(),
            scene_name: String::new(),
            notice: None,
            details: false,
            // Rig: UR44_INSPECT=1 opens the inspector for input 1.
            inspect: (rig && (std::env::var("UR44_INSPECT").as_deref() == Ok("1") || page_env == "mixer-inspect")).then_some(0),
            // Rig/testing: UR44_PREVIEW=1 shows the editors of effects that are not on an input.
            preview: rig && std::env::var("UR44_PREVIEW").as_deref() == Ok("1"),
            window: None,
            prefs,
            window_glass: false,
            rig,
            rig_ticks: 0,
            rig_ready_ticks: 0,
            tray: None,
            restored: false,
            dirty: false,
            last_save: std::time::Instant::now(),
        };
        let mut tasks = vec![iced::Task::done(Msg::Init)];
        if !background {
            tasks.push(iced::Task::done(Msg::ShowWindow(page)));
        }
        (app, iced::Task::batch(tasks))
    }

    fn title(&self, _: window::Id) -> String {
        "UR44 Mixer".into()
    }

    fn scene_names(&self) -> Vec<String> {
        self.scenes.iter().map(|s| s.name.clone()).collect()
    }

    fn sync_tray(&mut self) {
        match (self.prefs.tray, self.tray.is_some()) {
            (prefs::TrayMode::MenuBar, false) => match tray::Tray::new(&self.scene_names()) {
                Ok(t) => self.tray = Some(t),
                Err(e) => self.notice = Some(format!("{}: {e}", os::TRAY_UNAVAILABLE)),
            },
            (prefs::TrayMode::Hidden, true) => self.tray = None,
            _ => {}
        }
        let status = match &self.conn {
            Conn::Ready(_) => {
                let fs = units::format("SysFsCode", self.get("SysFsCode", 0));
                let p48 = match (self.on("HaP48VOnf", 0), self.on("HaP48VOnf", 1)) {
                    (true, true) => " \u{b7} +48V 1/2 3/4",
                    (true, false) => " \u{b7} +48V 1/2",
                    (false, true) => " \u{b7} +48V 3/4",
                    _ => "",
                };
                format!("UR44 \u{2014} Connected \u{b7} {fs}{p48}")
            }
            Conn::Loading(..) => "UR44 \u{2014} Reading mixer\u{2026}".into(),
            Conn::Searching(_) => "UR44 \u{2014} Not connected".into(),
        };
        if let Some(t) = &mut self.tray {
            t.set_status(&status);
        }
    }

    /// Restore the last known mixer state after the UR44 connects (only differing values).
    fn restore_last_state(&mut self) {
        self.restored = true;
        self.dirty = true; // capture a baseline of the current state at the next tick
        if !self.prefs.restore_on_connect {
            return;
        }
        let Ok(bytes) = std::fs::read(prefs::state_path()) else { return };
        let Ok(saved) = serde_json::from_slice::<scene::Scene>(&bytes) else { return };
        let w: Writes = scene::recall(&saved).into_iter().filter(|(a, v)| self.values.get(a) != Some(v)).collect();
        if !w.is_empty() {
            self.notice = Some(format!("Restored {} setting(s) from your last session.", w.len()));
            self.apply(w);
        }
    }

    fn save_state(&mut self) {
        if self.rig || !self.dirty || !self.restored {
            return;
        }
        let s = scene::capture("Last session", &self.values);
        let _ = std::fs::create_dir_all(prefs::support_dir());
        if let Ok(b) = serde_json::to_vec(&s) {
            let _ = std::fs::write(prefs::state_path(), b);
        }
        self.dirty = false;
        self.last_save = std::time::Instant::now();
    }

    fn update(&mut self, msg: Msg) -> iced::Task<Msg> {
        match msg {
            Msg::Init => {
                if !self.rig && self.prefs.start_at_login && !prefs::login_item_installed() {
                    let _ = prefs::set_login_item(true);
                }
                self.sync_tray();
                iced::Task::none()
            }
            Msg::Incoming(tray::Incoming::Menu(id)) => match self.tray.as_ref().and_then(|t| t.translate(&id)) {
                Some(c) => self.control(c),
                None => iced::Task::none(),
            },
            Msg::Incoming(tray::Incoming::Launcher(c)) => self.control(c),
            Msg::ShowWindow(page) => {
                self.page = page;
                if let Some(id) = self.window {
                    // Windows: bring a minimized window back before focusing it.
                    #[cfg(windows)]
                    return window::minimize(id, false).chain(window::gain_focus(id));
                    #[cfg(not(windows))]
                    return window::gain_focus(id);
                }
                self.window_glass = self.prefs.glass && os::GLASS;
                // UR44_SIZE=WxH (rig and testing); default 1360x880.
                let size = std::env::var("UR44_SIZE")
                    .ok()
                    .and_then(|v| v.split_once('x').and_then(|(w, h)| Some(iced::Size::new(w.parse().ok()?, h.parse().ok()?))))
                    .unwrap_or(iced::Size::new(1360.0, 880.0));
                let mut settings = glass::window_settings(self.window_glass, size);
                if self.rig {
                    // Off-screen and borderless (not moved back on screen); the app screenshots itself.
                    settings.position = window::Position::Specific(iced::Point::new(-20000.0, -20000.0));
                    settings.decorations = false;
                }
                let (id, open) = window::open(settings);
                self.window = Some(id);
                open.map(Msg::WindowOpened)
            }
            Msg::WindowOpened(id) => {
                if let Some(tx) = &self.cmd {
                    let _ = tx.send(Command::Meters(true));
                }
                if self.rig {
                    return rig_tick();
                }
                let focus = window::gain_focus(id);
                if self.window_glass {
                    window::run_with_handle(id, glass::install).map(Msg::GlassInstalled).chain(focus)
                } else {
                    focus
                }
            }
            Msg::RigTick => {
                // Shoot when the mixer state has been on screen for a moment, or after 6 s without
                // a device (UR44_SHOT_WAIT=1: the "waiting for the UR44" state).
                self.rig_ticks += 1;
                if matches!(self.conn, Conn::Ready(_)) {
                    self.rig_ready_ticks += 1;
                }
                let waiting = std::env::var_os("UR44_SHOT_WAIT").is_some();
                let due = if waiting { self.rig_ticks >= 12 } else { self.rig_ready_ticks >= 3 };
                match (due || self.rig_ticks >= 60, self.window) {
                    (true, Some(id)) => window::screenshot(id).map(Msg::RigShot),
                    _ => rig_tick(),
                }
            }
            Msg::RigShot(shot) => {
                if let Some(path) = std::env::var_os("UR44_SHOT") {
                    if let Err(e) = write_bmp(std::path::Path::new(&path), &shot) {
                        eprintln!("rig: cannot write {path:?}: {e}");
                    }
                }
                iced::exit()
            }
            Msg::GlassInstalled(what) => {
                eprintln!("glass: {what}");
                iced::Task::none()
            }
            Msg::PrefGlass(on) => {
                self.prefs.glass = on;
                self.prefs.save();
                // The window background is chosen when the window is created: reopen it.
                match self.window.take() {
                    Some(id) => window::close(id).chain(iced::Task::done(Msg::ShowWindow(self.page))),
                    None => iced::Task::none(),
                }
            }
            Msg::CloseRequested(id) => window::close(id),
            Msg::WindowClosed(id) => {
                if self.window == Some(id) {
                    self.window = None;
                    if let Some(tx) = &self.cmd {
                        let _ = tx.send(Command::Meters(false));
                    }
                }
                iced::Task::none()
            }
            Msg::Quit => {
                self.dirty = true;
                self.save_state();
                ur44::ipc::cleanup();
                iced::exit()
            }
            Msg::PrefLogin(on) => {
                match prefs::set_login_item(on) {
                    Ok(()) => {
                        self.prefs.start_at_login = on;
                        self.prefs.save();
                    }
                    Err(e) => self.notice = Some(e),
                }
                iced::Task::none()
            }
            Msg::PrefTray(mode) => {
                self.prefs.tray = mode;
                self.prefs.save();
                self.sync_tray();
                iced::Task::none()
            }
            Msg::PrefRestore(on) => {
                self.prefs.restore_on_connect = on;
                self.prefs.save();
                iced::Task::none()
            }
            other => {
                let is_initial_load = matches!(other, Msg::Worker(Event::Values(_))) && !self.restored;
                let edits = matches!(other, Msg::Set(..) | Msg::SetAll(..) | Msg::Insert(..) | Msg::Assign(..) | Msg::SweetSpot(..) | Msg::SceneRecall(_) | Msg::Worker(Event::Value(..)));
                let scenes_before = self.scenes.len();
                self.update_inner(other);
                if is_initial_load && matches!(self.conn, Conn::Ready(_)) {
                    self.restore_last_state();
                }
                if edits {
                    self.dirty = true;
                }
                if self.scenes.len() != scenes_before {
                    let names = self.scene_names();
                    if let Some(t) = &mut self.tray {
                        t.set_scenes(&names);
                    }
                }
                self.sync_tray();
                iced::Task::none()
            }
        }
    }

    fn control(&mut self, c: tray::Control) -> iced::Task<Msg> {
        match c {
            tray::Control::Show => iced::Task::done(Msg::ShowWindow(if self.window.is_some() { self.page } else { Page::Mixer })),
            tray::Control::ShowSettings => iced::Task::done(Msg::ShowWindow(Page::Settings)),
            tray::Control::RecallScene(i) => {
                self.scenes = scene::list();
                self.update_inner(Msg::SceneRecall(i));
                self.dirty = true;
                iced::Task::none()
            }
            tray::Control::Quit => iced::Task::done(Msg::Quit),
        }
    }

    fn get(&self, name: &str, ch: u16) -> i32 {
        logic::get(&self.values, name, ch)
    }

    fn on(&self, name: &str, ch: u16) -> bool {
        self.get(name, ch) != 0
    }

    fn subscription(&self) -> Subscription<Msg> {
        Subscription::batch([
            Subscription::run(worker::run).map(Msg::Worker),
            Subscription::run(tray::events).map(Msg::Incoming),
            window::close_requests().map(Msg::CloseRequested),
            window::close_events().map(Msg::WindowClosed),
            iced::keyboard::on_key_press(|key, _| match key {
                iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape) => Some(Msg::Inspect(None)),
                _ => None,
            }),
        ])
    }

    fn apply(&mut self, w: Writes) {
        if w.is_empty() {
            return;
        }
        for (a, v) in &w {
            self.values.insert(*a, *v);
        }
        if let Some(tx) = &self.cmd {
            let _ = tx.send(Command::Set(w));
        }
    }

    fn update_inner(&mut self, msg: Msg) {
        match msg {
            Msg::Worker(Event::Ready(tx)) => {
                // Meters stream only while a window is open.
                let _ = tx.send(Command::Meters(self.window.is_some()));
                self.cmd = Some(tx);
            }
            Msg::Worker(Event::Connected { firmware }) => self.conn = Conn::Ready(firmware),
            Msg::Worker(Event::Loading { done, total }) => self.conn = Conn::Loading(done, total),
            Msg::Worker(Event::Values(vs)) => {
                self.values.extend(vs);
                if let Conn::Loading(..) = self.conn {
                    self.conn = Conn::Ready(String::new());
                }
                // Meter source follows the selected mix.
                self.apply(vec![(logic::addr("PatchMasterMeter", 0), i32::from(self.mix - 1))]);
            }
            Msg::Worker(Event::Value(a, v)) => {
                self.values.insert(a, v);
            }
            Msg::Worker(Event::Meters(m)) => {
                if m.len() >= 44 && !self.rig {
                    self.meters = m;
                }
            }
            Msg::Worker(Event::Disconnected(why)) => {
                self.conn = Conn::Searching(why);
                self.restored = false;
            }
            Msg::Worker(Event::Tick) => {
                if self.dirty && self.last_save.elapsed() > std::time::Duration::from_secs(2) {
                    self.save_state();
                }
            }
            Msg::Set(name, ch, v) => {
                let w = logic::edit(&self.values, name, ch, v);
                self.apply(w);
            }
            Msg::SetAll(name, v) => {
                let n = params::by_name(name).map_or(1, |p| p.channels);
                let w = (0..n).map(|c| (logic::addr(name, c), v)).collect();
                self.apply(w);
            }
            Msg::Insert(ch, kind, pos) => match logic::set_insert(&self.values, ch, kind, pos) {
                Ok(w) => {
                    self.notice = None;
                    self.apply(w);
                }
                Err(e) => self.notice = Some(e),
            },
            Msg::EditFx(ch) => {
                let ins = logic::insert_of(&self.values, ch);
                match ins.kind {
                    FxKind::ChannelStrip => {
                        self.strip = ins.strip.saturating_sub(1);
                        self.page = Page::Strip;
                    }
                    FxKind::GuitarAmp => self.page = Page::Guitar,
                    FxKind::None => {}
                }
            }
            Msg::Mix(m) => {
                self.mix = m;
                self.apply(vec![(logic::addr("PatchMasterMeter", 0), i32::from(m - 1))]);
            }
            Msg::Page(p) => {
                if p == Page::Scenes {
                    self.scenes = scene::list();
                }
                self.page = p;
                self.preview = false;
            }
            Msg::Strip(s) => {
                self.strip = s;
                self.preview = false;
            }
            Msg::Inspect(sel) => self.inspect = sel,
            Msg::ShowInput(ch) => {
                self.page = Page::Mixer;
                self.inspect = Some(ch);
            }
            Msg::Assign(ch, kind) => {
                let pos = logic::insert_of(&self.values, ch).pos;
                match logic::set_insert(&self.values, ch, kind, pos) {
                    Ok(w) => {
                        self.notice = None;
                        self.apply(w);
                        self.preview = false;
                        self.update_inner(Msg::EditFx(ch));
                    }
                    Err(e) => self.notice = Some(e),
                }
            }
            Msg::Preview(on) => self.preview = on,
            Msg::SweetSpot(strip, i) => {
                if let Some(p) = self.presets.get(i) {
                    let w = logic::load_sweet_spot(&self.values, strip, &p.points);
                    self.apply(w);
                }
            }
            Msg::SceneName(s) => self.scene_name = s,
            Msg::SceneStore => {
                let name = if self.scene_name.trim().is_empty() { "Untitled".to_string() } else { self.scene_name.trim().to_string() };
                match scene::save(&scene::capture(&name, &self.values)) {
                    Ok(_) => {
                        self.notice = Some(format!("Stored scene \u{201c}{name}\u{201d}."));
                        self.scene_name.clear();
                    }
                    Err(e) => self.notice = Some(format!("Could not store scene: {e}")),
                }
                self.scenes = scene::list();
            }
            Msg::SceneRecall(i) => {
                if let Some(s) = self.scenes.get(i).cloned() {
                    self.apply(scene::recall(&s));
                    self.notice = Some(format!("Recalled scene \u{201c}{}\u{201d}.", s.name));
                }
            }
            Msg::SceneDelete(i) => {
                if let Some(s) = self.scenes.get(i) {
                    if let Err(e) = scene::delete(&s.name) {
                        self.notice = Some(e.to_string());
                    }
                }
                self.scenes = scene::list();
            }
            Msg::ScenesFolder => {
                let _ = std::fs::create_dir_all(scene::dir());
                ur44::paths::reveal(&scene::dir());
            }
            Msg::Dismiss => self.notice = None,
            Msg::Details(on) => self.details = on,
            _ => {}
        }
    }

    // ---------- view ----------

    fn view(&self, _: window::Id) -> Element<'_, Msg> {
        let body: Element<Msg> = match &self.conn {
            Conn::Searching(why) => self.waiting(why),
            Conn::Loading(done, total) => centered(column![
                text("Reading mixer state…").size(22),
                text(format!("{done} / {total}")).size(14),
            ]),
            Conn::Ready(_) => match self.page {
                Page::Mixer => self.mixer(),
                Page::Strip => self.strip_page(),
                Page::Guitar => self.guitar_page(),
                Page::Reverb => self.reverb_page(),
                Page::Scenes => self.scenes_page(),
                Page::Settings => self.settings_page(),
            },
        };
        let top = if self.window_glass { 16.0 + glass::TITLEBAR } else { 16.0 };
        let mut col = column![self.top_bar()].spacing(12).padding(iced::Padding { top, right: 16.0, bottom: 16.0, left: 16.0 });
        if let Some(n) = &self.notice {
            col = col.push(
                container(
                    row![text(n.clone()).size(14), horizontal_space(), button(text("OK").size(13)).style(glass::secondary).on_press(Msg::Dismiss)]
                        .align_y(Alignment::Center),
                )
                .padding([8, 12])
                .style(glass::well),
            );
        }
        col.push(body).into()
    }

    fn top_bar(&self) -> Element<'_, Msg> {
        let (dot, color, label) = match &self.conn {
            Conn::Ready(_) => ("\u{25cf}", glass::OK, format!("Connected \u{b7} {}", units::format("SysFsCode", self.get("SysFsCode", 0)))),
            Conn::Loading(done, total) => ("\u{25cf}", glass::WAIT, format!("Loading {}%", done * 100 / (*total).max(1))),
            Conn::Searching(_) => ("\u{25cb}", glass::OFF, "Not connected".into()),
        };
        let mut status: Element<Msg> = row![text(dot).size(13).color(color), text(label).size(13)].spacing(5).align_y(Alignment::Center).into();
        if let Conn::Ready(fw) = &self.conn {
            if !fw.is_empty() {
                status = tooltip(status, container(text(format!("Firmware: {fw}")).size(12)).padding(6).style(glass::well), tooltip::Position::Bottom).into();
            }
        }
        // Pages only exist once the UR44 is connected: dim the navigation until then.
        let ready = matches!(self.conn, Conn::Ready(_));
        let tab = |label: &'static str, active: bool, msg: Msg| {
            button(text(label).size(14)).padding([6, 12]).style(glass::tab(active && ready)).on_press_maybe(ready.then_some(msg))
        };
        row![
            text("UR44").size(26),
            Space::with_width(4),
            status,
            horizontal_space(),
            tab("Mixer", self.page == Page::Mixer, Msg::Page(Page::Mixer)),
            tab("Ch Strip", self.page == Page::Strip, Msg::Page(Page::Strip)),
            tab("Guitar Amp", self.page == Page::Guitar, Msg::Page(Page::Guitar)),
            tab("REV-X", self.page == Page::Reverb, Msg::Page(Page::Reverb)),
            tab("Scenes", self.page == Page::Scenes, Msg::Page(Page::Scenes)),
            tab("Settings", self.page == Page::Settings, Msg::Page(Page::Settings)),
        ]
        .spacing(6)
        .align_y(Alignment::Center)
        .into()
    }

    fn meter_pair(&self, slot: usize) -> (u32, u32) {
        (self.meters.get(slot).copied().unwrap_or(0), self.meters.get(slot + 1).copied().unwrap_or(0))
    }

    // ----- mixer page -----

    fn mixer(&self) -> Element<'_, Msg> {
        let mix = self.mix;
        let gap = if self.inspect.is_some() { 6 } else { 8 };
        let mut strips = row![].spacing(gap).height(Length::Fill);
        for ch in 0..6u16 {
            strips = strips.push(self.input_strip(ch, mix));
        }
        let strips = strips
            .push(vertical_rule(1))
            .push(self.daw_strip(mix))
            .push(self.reverb_strip(mix))
            .push(vertical_rule(1))
            .push(self.master_strip(mix));
        let body: Element<Msg> = match self.inspect {
            Some(ch) => row![strips, self.inspector(ch)].spacing(10).height(Length::Fill).into(),
            None => strips.into(),
        };
        column![self.mix_header(), body].spacing(8).into()
    }

    /// Where a mix is heard: its line outputs and the headphone outputs following it.
    fn mix_destination(&self, mix: u8) -> String {
        let ph2_mix = if self.on("PatchAuxPhoneOutput", 0) { 2 } else { 1 };
        let mut d = vec![if mix == 1 { "Line out 1/2" } else { "Line out 3/4" }];
        if mix == 1 {
            d.push("Phones 1");
        }
        if ph2_mix == mix {
            d.push("Phones 2");
        }
        d.join(" \u{b7} ")
    }

    /// The mix being edited: selector, destination and the Phones 2 source, tinted per mix.
    fn mix_header(&self) -> Element<'_, Msg> {
        let mix = self.mix;
        let tint = glass::mix_tint(mix);
        let choice = |m: u8| {
            button(text(format!("MIX {m}")).size(15).center().width(Length::Fill))
                .width(76)
                .padding([7, 8])
                .style(glass::mix_choice(m, m == mix))
                .on_press(Msg::Mix(m))
        };
        let ph2 = if self.on("PatchAuxPhoneOutput", 0) { "MIX 2" } else { "MIX 1" };
        // Legend: shaded = shared by both mixes, tinted = this mix only.
        let swatch_shared = container(Space::new(14, 10)).style(glass::shared_inset);
        let swatch_mix = container(Space::new(14, 3)).style(move |_: &Theme| container::Style::default().background(Color { a: 0.8, ..tint }).border(iced::border::rounded(1)));
        let legend = row![
            swatch_shared,
            text("Shared by both mixes").size(11).color(glass::TEXT_MUTED),
            Space::with_width(8),
            swatch_mix,
            text(format!("Below the line: MIX {mix} only")).size(11).color(glass::TEXT_MUTED),
            Space::with_width(8),
            text("Click an input name for its settings and effect.").size(11).color(glass::TEXT_MUTED),
        ]
        .spacing(5)
        .align_y(Alignment::Center);
        container(
            row![
                row![choice(1), choice(2)].spacing(4),
                column![
                    row![
                        text(format!("Editing MIX {mix}")).size(17).color(Color { a: 1.0, ..tint }),
                        text(format!("\u{2192}  {}", self.mix_destination(mix))).size(15).color(glass::TEXT_HI),
                    ]
                    .spacing(10)
                    .align_y(Alignment::Center),
                    legend,
                ]
                .spacing(3),
                horizontal_space(),
                text("Phones 2 hears").size(13),
                container(pick_list(["MIX 1", "MIX 2"], Some(ph2), |s: &str| Msg::Set("PatchAuxPhoneOutput", 0, i32::from(s == "MIX 2"))).text_size(13).width(Length::Fill)).width(92),
            ]
            .spacing(14)
            .align_y(Alignment::Center),
        )
        .padding([7, 10])
        .width(Length::Fill)
        .style(glass::mix_band(mix))
        .into()
    }

    fn waiting(&self, why: &str) -> Element<'_, Msg> {
        let step = |n: &'static str, head: &'static str, body: &'static str| -> Element<'_, Msg> {
            row![
                container(text(n).size(14).center().width(Length::Fill)).width(26).height(26).center_y(26).style(glass::badge(true, Color::from_rgb(0.30, 0.34, 0.46))),
                column![text(head).size(15).color(glass::TEXT_HI), text(body).size(12).color(glass::TEXT_MUTED)].spacing(2),
            ]
            .spacing(12)
            .into()
        };
        let detail = why.replace("?.", "?");
        let mut card_col = column![
            text("Can\u{2019}t reach your UR44").size(24).color(glass::TEXT_HI),
            text("UR44 Mixer keeps looking and connects by itself as soon as the UR44 answers.").size(13).color(glass::TEXT),
            Space::with_height(4),
            step("1", "Connect the USB cable", os::CONNECT_NOTE),
            step("2", "Switch the UR44 on", "Check that its power adapter is plugged in and the POWER switch is on."),
            container(
                column![
                    text(os::CC_MODE_TITLE).size(13).color(glass::TEXT),
                    text(os::CC_MODE_NOTE)
                        .size(12)
                        .color(glass::TEXT_MUTED),
                ]
                .spacing(3),
            )
            .padding([8, 12])
            .width(Length::Fill)
            .style(glass::shared_inset),
            Space::with_height(4),
            row![
                text("\u{25cf}").size(12).color(glass::WAIT),
                text("Searching\u{2026}").size(12).color(glass::TEXT),
                horizontal_space(),
                button(text(if self.details { "Hide details" } else { "Technical details" }).size(12)).padding([4, 10]).style(glass::secondary).on_press(Msg::Details(!self.details)),
            ]
            .spacing(6)
            .align_y(Alignment::Center),
        ]
        .spacing(14);
        if self.details {
            card_col = card_col.push(container(text(detail).size(11).color(glass::TEXT_MUTED)).padding(10).width(Length::Fill).style(glass::well));
        }
        container(container(card_col).padding(26).max_width(540).style(glass::panel)).center(Length::Fill).into()
    }

    /// The effect picker and MON/INS row of an input (full mixer, no inspector open).
    fn fx_slot(&self, ch: u16) -> Element<'_, Msg> {
        let ins = logic::insert_of(&self.values, ch);
        let pos = ins.pos;
        let picker = fx_picker(ch, ins.kind, pos);
        let active = ins.kind != FxKind::None;
        let posrow: Element<Msg> = if active {
            let label = if ins.kind == FxKind::ChannelStrip { format!("#{}", ins.strip) } else { "AMP".into() };
            row![
                toggle("MON", pos == FxPos::Mon, Msg::Insert(ch, ins.kind, FxPos::Mon), Kind::Accent),
                toggle("INS", pos == FxPos::Ins, Msg::Insert(ch, ins.kind, FxPos::Ins), Kind::Accent),
                button(text(label).size(11).center().width(Length::Fill))
                    .width(Length::Fill)
                    .height(ROW)
                    .padding([6, 2])
                    .style(glass::secondary)
                    .on_press(Msg::EditFx(ch)),
            ]
            .spacing(3)
            .into()
        } else if self.any_fx() {
            // Keep strips aligned while another input shows its MON/INS row.
            Space::with_height(ROW).into()
        } else {
            return picker;
        };
        column![picker, posrow].spacing(4).into()
    }

    /// Any input has an effect inserted (its strip shows an extra MON/INS row).
    fn any_fx(&self) -> bool {
        (0..6u16).any(|c| logic::insert_of(&self.values, c).kind != FxKind::None)
    }

    /// Height of the control area above the faders, identical for every strip.
    fn top_h(&self) -> f32 {
        if self.inspect.is_some() {
            COMPACT_TOP
        } else if self.any_fx() {
            TOP
        } else {
            TOP - ROW - 4.0
        }
    }

    /// Compact strip (inspector open): one button summarising the input's setup.
    fn input_summary(&self, ch: u16, selected: bool) -> Element<'_, Msg> {
        let ins = logic::insert_of(&self.values, ch);
        let mut parts: Vec<&str> = Vec::new();
        if ch < 4 && self.on("ChHpfOnf", ch) {
            parts.push("HPF");
        }
        if self.on("ChPhaseOnf", ch) {
            parts.push("\u{d8}");
        }
        if self.on("ChPairOnf", ch & !1) {
            parts.push("LINK");
        }
        match ins.kind {
            FxKind::ChannelStrip => parts.push("STRIP"),
            FxKind::GuitarAmp => parts.push("AMP"),
            FxKind::None => {}
        }
        let label = if parts.is_empty() { "SETUP".to_string() } else { parts.join(" ") };
        let lit = !parts.is_empty();
        button(text(label).size(10).center().width(Length::Fill))
            .width(Length::Fill)
            .height(ROW)
            .padding([6, 2])
            .style(move |t: &Theme, s| if selected || lit { glass::secondary(t, s) } else { glass::toggle_off(t, s) })
            .on_press(Msg::Inspect(Some(ch)))
            .into()
    }

    fn input_strip(&self, ch: u16, mix: u8) -> Element<'_, Msg> {
        let level_p = mixp("ChSendMixBus", mix, "Level");
        let pan_p = mixp("ChSendMixBus", mix, "Pan");
        let on_p = mixp("ChSendMixBus", mix, "Onf");
        let solo_p = mixp("ChSendMixBus", mix, "SoloOnf");
        let compact = self.inspect.is_some();
        let selected = self.inspect == Some(ch);

        // +48V is a hardware switch per input pair: shown as a status light, not a button.
        let source: Element<Msg> = if ch < 4 { self.phantom(ch / 2) } else { caption("LINE") };
        let name = button(text(format!("IN {}", ch + 1)).size(16)).padding([1, 5]).style(glass::name_button(selected)).on_press(Msg::Inspect(if selected { None } else { Some(ch) }));
        let name = tooltip(name, tip(if selected { "Close input settings" } else { "Input settings and effect" }), tooltip::Position::Bottom);
        let header = row![name, horizontal_space(), source].align_y(Alignment::Center).height(24);

        let send = shared_slider(if compact { "REV" } else { "REV SEND" }, "ChSendFxBusLevel", ch, self.get("ChSendFxBusLevel", ch));
        let shared: iced::widget::Column<'_, Msg> = if compact {
            column![self.input_summary(ch, selected), send]
        } else {
            let link: Element<Msg> = if ch % 2 == 0 {
                toggle("LINK", self.on("ChPairOnf", ch), Msg::Set("ChPairOnf", ch, flip(self.on("ChPairOnf", ch))), Kind::Accent)
            } else if self.on("ChPairOnf", ch & !1) {
                // Linked pair: the right channel offers fader link (levels follow each other).
                let l = ch & !1;
                let fl = self.on("ChFaderLinkOnf", l);
                toggle("FADER LINK", fl, Msg::Set("ChFaderLinkOnf", l, flip(fl)), Kind::Accent)
            } else {
                Space::with_height(ROW).into()
            };
            let hpf: Element<Msg> = if ch < 4 {
                toggle("HPF", self.on("ChHpfOnf", ch), Msg::Set("ChHpfOnf", ch, flip(self.on("ChHpfOnf", ch))), Kind::Accent)
            } else {
                Space::with_height(ROW).into()
            };
            column![
                link,
                row![hpf, toggle("\u{d8}", self.on("ChPhaseOnf", ch), Msg::Set("ChPhaseOnf", ch, flip(self.on("ChPhaseOnf", ch))), Kind::Accent)].spacing(4),
                self.fx_slot(ch),
                send,
            ]
        };

        let top = column![
            header,
            shared_box(shared),
            mix_rule(mix),
            mix_slider("PAN", pan_p, ch, -16..=16, self.get(pan_p, ch), mix),
            row![
                toggle("MUTE", !self.on(on_p, ch), Msg::Set(on_p, ch, i32::from(!self.on(on_p, ch))), Kind::Mute),
                toggle("SOLO", self.on(solo_p, ch), Msg::Set(solo_p, ch, flip(self.on(solo_p, ch))), Kind::Solo),
            ]
            .spacing(4),
        ];
        strip_box(mix, selected, self.top_h(), top, fader(mix, level_p, ch, self.get(level_p, ch), vec![self.meter_pair(meter_slot::input(ch as usize))]))
    }

    /// +48V status of an input pair: lit warm when on, quiet text when off. Read-only (hardware switch).
    fn phantom(&self, pair: u16) -> Element<'_, Msg> {
        let on = self.on("HaP48VOnf", pair);
        let badge: Element<Msg> = if on {
            container(text("+48V").size(11)).padding([2, 7]).style(glass::badge(true, glass::PHANTOM)).into()
        } else {
            text("+48V off").size(10).color(glass::TEXT_MUTED).into()
        };
        tooltip(
            badge,
            tip(format!(
                "Phantom power for inputs {}\u{2013}{} is {}. It is switched with the +48V button on the UR44 front panel; this app shows it but cannot change it.",
                pair * 2 + 1,
                pair * 2 + 2,
                if on { "ON" } else { "off" }
            )),
            tooltip::Position::Bottom,
        )
        .into()
    }

    fn daw_strip(&self, mix: u8) -> Element<'_, Msg> {
        let level_p = mixp("DawSendMixBus", mix, "Level");
        let bal_p = mixp("DawSendMixBus", mix, "Bal");
        let on_p = mixp("DawSendMixBus", mix, "Onf");
        let solo_p = mixp("DawSendMixBus", mix, "SoloOnf");
        let compact = self.inspect.is_some();
        let top = column![
            row![text("DAW").size(16).color(glass::TEXT_HI), horizontal_space(), caption("STEREO")].align_y(Alignment::Center).height(24),
            shared_box(column![shared_slider(if compact { "REV" } else { "REV SEND" }, "DawSendFxBusLevel", 0, self.get("DawSendFxBusLevel", 0))]),
            mix_rule(mix),
            mix_slider("BAL", bal_p, 0, -16..=16, self.get(bal_p, 0), mix),
            row![
                toggle("MUTE", !self.on(on_p, 0), Msg::Set(on_p, 0, i32::from(!self.on(on_p, 0))), Kind::Mute),
                toggle("SOLO", self.on(solo_p, 0), Msg::Set(solo_p, 0, flip(self.on(solo_p, 0))), Kind::Solo),
            ]
            .spacing(4),
        ];
        strip_box(
            mix,
            false,
            self.top_h(),
            top,
            fader(mix, level_p, 0, self.get(level_p, 0), vec![self.meter_pair(meter_slot::DAW_L), self.meter_pair(meter_slot::DAW_R)]),
        )
    }

    fn reverb_strip(&self, mix: u8) -> Element<'_, Msg> {
        let ret_p = mixp("FxRevBus", mix, "Level");
        let on_p = mixp("FxRevMixBus", mix, "Onf");
        let top = column![
            row![text("REV-X").size(16).color(glass::TEXT_HI), horizontal_space(), caption(if self.inspect.is_some() { "" } else { "RETURN" })].align_y(Alignment::Center).height(24),
            shared_box(column![
                self.choice("FxRevType", 0),
                column![
                    row![text("TIME").size(11), horizontal_space(), text(self.shown("FxRevTime", 0, self.get("FxRevTime", 0))).size(11).color(glass::TEXT_HI)],
                    slider(0..=69, self.get("FxRevTime", 0), |v| Msg::Set("FxRevTime", 0, v)).height(16).style(glass::slider_shared),
                ]
                .spacing(2),
                button(text("EDIT").size(11).center().width(Length::Fill)).width(Length::Fill).height(ROW).style(glass::secondary).on_press(Msg::Page(Page::Reverb)),
            ]),
            mix_rule(mix),
            toggle(if self.on(on_p, 0) { "IN MIX: ON" } else { "IN MIX: OFF" }, self.on(on_p, 0), Msg::Set(on_p, 0, flip(self.on(on_p, 0))), Kind::Accent),
        ];
        strip_box(mix, false, self.top_h(), top, fader(mix, ret_p, 0, self.get(ret_p, 0), vec![]))
    }

    fn master_strip(&self, mix: u8) -> Element<'_, Msg> {
        let level_p = mixp("MasterMixBus", mix, "Level");
        let bal_p = mixp("MasterMixBus", mix, "Bal");
        let on_p = mixp("MasterMixBus", mix, "Onf");
        let top = column![
            row![text(format!("MIX {mix}")).size(16).color(glass::mix_tint(mix)), horizontal_space(), caption(if mix == 1 { "OUT 1/2" } else { "OUT 3/4" })]
                .align_y(Alignment::Center)
                .height(24),
            mix_rule(mix),
            mix_slider("BAL", bal_p, 0, -16..=16, self.get(bal_p, 0), mix),
            toggle("MUTE", !self.on(on_p, 0), Msg::Set(on_p, 0, i32::from(!self.on(on_p, 0))), Kind::Mute),
        ];
        strip_box(
            mix,
            false,
            self.top_h(),
            top,
            fader(mix, level_p, 0, self.get(level_p, 0), vec![self.meter_pair(meter_slot::MIX_L), self.meter_pair(meter_slot::MIX_R)]),
        )
    }

    // ----- input inspector (beside the mixer strips) -----

    fn inspector(&self, ch: u16) -> Element<'_, Msg> {
        let ins = logic::insert_of(&self.values, ch);
        let nav = |label: &'static str, target: Option<u16>| {
            button(text(label).size(15).center().width(Length::Fill)).width(30).padding([2, 4]).style(glass::secondary).on_press_maybe(target.map(|t| Msg::Inspect(Some(t))))
        };
        let head = row![
            column![
                text(format!("IN {}", ch + 1)).size(22).color(glass::TEXT_HI),
                text(if ch < 4 { "Mic / instrument input" } else { "Line input" }).size(11).color(glass::TEXT_MUTED),
            ]
            .spacing(1),
            horizontal_space(),
            nav("\u{2039}", ch.checked_sub(1)),
            nav("\u{203a}", (ch < 5).then_some(ch + 1)),
            button(text("Close").size(12)).padding([4, 10]).style(glass::secondary).on_press(Msg::Inspect(None)),
        ]
        .spacing(6)
        .align_y(Alignment::Center);

        // Input setup: the same for both mixes.
        let line = setting_row;
        let mut input = column![section_title("INPUT", "same in both mixes")].spacing(8);
        if ch < 4 {
            let pair = ch / 2;
            input = input.push(column![
                line("Phantom power", self.phantom(pair)),
                text("Switched on the UR44 front panel (+48V).").size(11).color(glass::TEXT_MUTED),
            ]);
            let hpf = self.on("ChHpfOnf", ch);
            input = input.push(line(
                "Low cut (HPF)",
                row![
                    text(units::format("ChHpfFreq", self.get("ChHpfFreq", ch))).size(11).color(glass::TEXT_MUTED),
                    toggle_w(on_off(hpf), hpf, Msg::Set("ChHpfOnf", ch, flip(hpf)), 58),
                ]
                .spacing(8)
                .align_y(Alignment::Center)
                .into(),
            ));
        }
        let ph = self.on("ChPhaseOnf", ch);
        input = input.push(line("Phase invert (\u{d8})", toggle_w(on_off(ph), ph, Msg::Set("ChPhaseOnf", ch, flip(ph)), 58)));
        let l = ch & !1;
        let linked = self.on("ChPairOnf", l);
        let link_label: &'static str = ["Stereo link IN 1+2", "Stereo link IN 3+4", "Stereo link IN 5+6"][usize::from(l / 2)];
        input = input.push(line(link_label, toggle_w(on_off(linked), linked, Msg::Set("ChPairOnf", l, flip(linked)), 58)));
        if linked {
            let fl = self.on("ChFaderLinkOnf", l);
            input = input.push(line("Fader link", toggle_w(on_off(fl), fl, Msg::Set("ChFaderLinkOnf", l, flip(fl)), 58)));
        }
        input = input.push(shared_slider("Reverb send", "ChSendFxBusLevel", ch, self.get("ChSendFxBusLevel", ch)));

        // Effect: assignment and its main controls.
        let seg = |label: &'static str, kind: FxKind| {
            let on = ins.kind == kind;
            button(text(label).size(12).center().width(Length::Fill))
                .width(Length::Fill)
                .height(ROW + 2.0)
                .padding([6, 2])
                .style(move |t: &Theme, s| if on { glass::toggle_on(ACCENT)(t, s) } else { glass::secondary(t, s) })
                .on_press(Msg::Insert(ch, kind, ins.pos))
        };
        let mut fx = column![
            section_title("EFFECT", "on this input"),
            row![seg("None", FxKind::None), seg("Ch Strip", FxKind::ChannelStrip), seg("Guitar Amp", FxKind::GuitarAmp)].spacing(4),
        ]
        .spacing(8);
        match ins.kind {
            FxKind::None => {
                fx = fx.push(
                    column![
                        rich_line("Ch Strip", "compressor and EQ: evens out and shapes a voice or instrument."),
                        rich_line("Guitar Amp", "amp and speaker sound for a guitar plugged straight in (mono inputs)."),
                    ]
                    .spacing(6),
                );
            }
            kind => {
                fx = fx.push(column![
                    row![
                        text("Heard").size(12),
                        horizontal_space(),
                        container(toggle("MONITOR ONLY", ins.pos == FxPos::Mon, Msg::Insert(ch, kind, FxPos::Mon), Kind::Accent)).width(104),
                        container(toggle("RECORDED", ins.pos == FxPos::Ins, Msg::Insert(ch, kind, FxPos::Ins), Kind::Accent)).width(86),
                    ]
                    .spacing(4)
                    .align_y(Alignment::Center),
                    text(if ins.pos == FxPos::Mon { "MON.FX: you hear the effect; the computer records the dry input." } else { "INS.FX: the effect is recorded on the computer too." })
                        .size(11)
                        .color(glass::TEXT_MUTED),
                ]
                .spacing(4));
                fx = fx.push(if kind == FxKind::ChannelStrip { self.inspector_strip(ins.strip.saturating_sub(1)) } else { self.inspector_amp() });
                fx = fx.push(
                    button(text("Open full editor").size(13).center().width(Length::Fill)).width(Length::Fill).padding([7, 10]).style(glass::primary).on_press(Msg::EditFx(ch)),
                );
            }
        }

        let content = column![head, card_tight(input.into()), card_tight(fx.into())].spacing(10).padding(iced::Padding { right: 10.0, ..iced::Padding::ZERO });
        container(scrollable(content).style(glass::scroll).height(Length::Fill)).width(INSPECTOR_W).height(Length::Fill).padding([12, 12]).style(glass::panel).into()
    }

    /// Compact Ch Strip controls for the inspector: compressor and EQ on/off with their curves.
    fn inspector_strip(&self, s: u16) -> Element<'_, Msg> {
        let comp_on = !self.on("FxCompBypass", s);
        let eq_on = !self.on("FxEqBypass", s);
        column![
            text(format!("Uses Ch Strip {}", s + 1)).size(11).color(glass::TEXT_MUTED),
            row![text("Compressor").size(14).color(glass::TEXT_HI), horizontal_space(), toggle_w(on_off(comp_on), comp_on, Msg::Set("FxCompBypass", s, i32::from(comp_on)), 58)].align_y(Alignment::Center),
            canvas(graphs::Comp {
                on: comp_on,
                drive: self.get("FxCompDrive", s),
                ratio: self.get("FxCompRatio", s),
                knee: self.get("FxCompKnee", s),
                thresh_ofst: self.get("FxCompThreshOfst", s),
            })
            .width(Length::Fill)
            .height(110),
            self.knob("Drive (amount)", "FxCompDrive", s),
            row![text("EQ").size(14).color(glass::TEXT_HI), horizontal_space(), toggle_w(on_off(eq_on), eq_on, Msg::Set("FxEqBypass", s, i32::from(eq_on)), 58)].align_y(Alignment::Center),
            canvas(self.eq_graph(s, eq_on)).width(Length::Fill).height(90),
        ]
        .spacing(8)
        .into()
    }

    /// Compact Guitar Amp controls for the inspector: model, bypass and the first few knobs.
    fn inspector_amp(&self) -> Element<'_, Msg> {
        let amp = self.get("GtrFxType", 0).clamp(0, 3) as usize;
        let bypass = self.on("GtrFxBypass", 0);
        let mut col = column![
            row![container(self.choice("GtrFxType", 0)).width(Length::Fill), toggle_w("BYPASS", bypass, Msg::Set("GtrFxBypass", 0, flip(bypass)), 76)].spacing(6).align_y(Alignment::Center),
        ]
        .spacing(8);
        let key = ["Volume", "Distortion", "Gain", "PreAmp", "Master", "Bass", "Middle", "Treble"];
        for name in logic::guitar_items(amp).into_iter().filter(|n| key.contains(&gtr_tail(n))) {
            col = col.push(self.knob(gtr_label(name), name, 0));
        }
        col.into()
    }

    fn eq_graph(&self, s: u16, on: bool) -> graphs::Eq {
        graphs::Eq {
            on,
            fs: f64::from(ur44::units::format("SysFsCode", self.get("SysFsCode", 0)).trim_end_matches(" kHz").parse::<f32>().unwrap_or(48.0)) * 1000.0,
            low_f: self.get("FxEqLsvFreq", s),
            low_g: self.get("FxEqLsvGain", s),
            mid_q: self.get("FxEqMidQual", s),
            mid_f: self.get("FxEqMidFreq", s),
            mid_g: self.get("FxEqMidGain", s),
            high_f: self.get("FxEqHsvFreq", s),
            high_g: self.get("FxEqHsvGain", s),
        }
    }

    // ----- generic editors -----

    fn knob(&self, label: &'static str, name: &'static str, ch: u16) -> Element<'_, Msg> {
        let p = params::by_name(name).expect("param");
        let v = self.get(name, ch);
        column![
            row![text(label).size(13), horizontal_space(), text(self.shown(name, ch, v)).size(13)],
            slider(p.min..=p.max, v, move |x| Msg::Set(name, ch, x)),
        ]
        .spacing(4)
        .into()
    }

    /// Display text for a value; REV-X time depends on type and room size (GetActualReverbTime).
    fn shown(&self, name: &str, ch: u16, v: i32) -> String {
        if name == "FxRevTime" {
            let t = ur44::dsp::reverb_time_s(self.get("FxRevType", 0), self.get("FxRevRoomSize", 0), v);
            return ur44::dsp::reverb_time_label(t);
        }
        units::format(name, if ch == u16::MAX { v } else { v })
    }

    fn choice(&self, name: &'static str, ch: u16) -> Element<'_, Msg> {
        let p = params::by_name(name).expect("param");
        let opts: Vec<String> = (p.min..=p.max).map(|i| units::format(name, i)).collect();
        let cur = units::format(name, self.get(name, ch));
        let min = p.min;
        let lookup = opts.clone();
        pick_list(opts, Some(cur), move |s: String| Msg::Set(name, ch, min + lookup.iter().position(|o| *o == s).unwrap_or(0) as i32))
            .text_size(13)
            .width(Length::Fill)
            .into()
    }

    fn labelled_choice(&self, label: &'static str, name: &'static str, ch: u16) -> Element<'_, Msg> {
        column![text(label).size(13), self.choice(name, ch)].spacing(4).into()
    }

    // ----- channel strip page -----

    fn strip_page(&self) -> Element<'_, Msg> {
        let s = self.strip;
        let user_of = |k: u16| {
            (0..6u16).find(|&c| {
                let i = logic::insert_of(&self.values, c);
                i.kind == FxKind::ChannelStrip && i.strip == k + 1
            })
        };
        let user = user_of(s);
        // Tabs are named by the input each strip is on.
        let mut tabs = row![].spacing(6);
        for k in 0..4u16 {
            let label = match user_of(k) {
                Some(c) => format!("IN {} \u{b7} Strip {}", c + 1, k + 1),
                None => format!("Strip {} \u{b7} free", k + 1),
            };
            tabs = tabs.push(button(text(label).size(13)).padding([6, 12]).style(glass::tab(k == s)).on_press(Msg::Strip(k)));
        }

        let title = fx_title(match user {
            Some(c) => format!("Ch Strip on IN {}", c + 1),
            None => format!("Ch Strip {}", s + 1),
        }, user.map(|c| (c, logic::insert_of(&self.values, c).pos)));
        let body: Element<Msg> = match (user, self.preview) {
            (Some(_), _) => self.strip_editor(s),
            (None, true) => column![preview_note("Ch Strip"), self.strip_editor(s)].spacing(10).into(),
            (None, false) => {
                let free: Vec<u16> = (0..6u16).filter(|&c| logic::insert_of(&self.values, c).kind == FxKind::None).collect();
                empty_fx(
                    format!("Ch Strip {} isn\u{2019}t on an input", s + 1),
                    "A Ch Strip is a compressor and EQ for one input: it evens out a voice or instrument and shapes its tone.",
                    free,
                    FxKind::ChannelStrip,
                    "Edit its settings anyway",
                )
            }
        };
        let panel = column![tabs, title, body].spacing(14);
        scrollable(panel).style(glass::scroll).into()
    }

    fn strip_editor(&self, s: u16) -> Element<'_, Msg> {
        // Which Sweet Spot preset is loaded? Compare the strip's morph table with the files.
        let table: Vec<[i32; 17]> = (0..9u16)
            .map(|p| std::array::from_fn(|k| self.get(logic::MORPH_TABLE[k], s * 9 + p)))
            .collect();
        let current = self.presets.iter().position(|ps| ps.points.iter().zip(&table).all(|(a, b)| a == b));
        let names: Vec<String> = self.presets.iter().map(|p| p.name.clone()).collect();
        let lookup = names.clone();
        let preset: Element<Msg> = if names.is_empty() {
            text(os::presets_missing()).size(13).into()
        } else {
            pick_list(names, current.map(|i| self.presets[i].name.clone()), move |n: String| {
                Msg::SweetSpot(s, lookup.iter().position(|x| *x == n).unwrap_or(0))
            })
            .placeholder("Custom")
            .text_size(13)
            .width(Length::Fill)
            .into()
        };

        let (lvl, pk) = self.meter_pair(28 + 2 * s as usize);
        let header = row![
            column![text("Sweet Spot Data").size(13), preset, self.knob("MORPH", "FxMorphValue", s)].spacing(8).width(Length::FillPortion(3)),
            column![self.knob("DRIVE", "FxCompDrive", s), self.knob("TOTAL GAIN", "FxEqPeqGain", s)].spacing(8).width(Length::FillPortion(3)),
            container(meter(lvl, pk)).height(100).padding([0, 8]),
            self.gr_meter(s),
        ]
        .spacing(20);

        let comp_on = !self.on("FxCompBypass", s);
        let eq_on = !self.on("FxEqBypass", s);
        let pair = two_up;
        let comp = column![
            row![text("Compressor").size(18).color(glass::TEXT_HI), horizontal_space(), toggle_w(on_off(comp_on), comp_on, Msg::Set("FxCompBypass", s, i32::from(comp_on)), 70)],
            canvas(graphs::Comp {
                on: comp_on,
                drive: self.get("FxCompDrive", s),
                ratio: self.get("FxCompRatio", s),
                knee: self.get("FxCompKnee", s),
                thresh_ofst: self.get("FxCompThreshOfst", s),
            })
            .width(Length::Fill)
            .height(150),
            pair(self.knob("Attack", "FxCompAttack", s), self.knob("Release", "FxCompRelease", s)),
            pair(self.knob("Ratio", "FxCompRatio", s), self.labelled_choice("Knee", "FxCompKnee", s)),
            text("Side-chain filter (what the compressor listens to)").size(12).color(glass::TEXT_MUTED),
            pair(self.knob("Side-chain F", "FxCompScFreq", s), self.knob("Side-chain Q", "FxCompScQual", s)),
            pair(self.knob("Side-chain G", "FxCompScGain", s), Space::with_width(Length::Fill).into()),
        ]
        .spacing(12)
        .width(Length::Fill);
        let eq = column![
            row![text("Equalizer").size(18).color(glass::TEXT_HI), horizontal_space(), toggle_w(on_off(eq_on), eq_on, Msg::Set("FxEqBypass", s, i32::from(eq_on)), 70)],
            canvas(self.eq_graph(s, eq_on)).width(Length::Fill).height(150),
            pair(self.knob("Low F", "FxEqLsvFreq", s), self.knob("Low G", "FxEqLsvGain", s)),
            pair(self.knob("Mid F", "FxEqMidFreq", s), self.knob("Mid G", "FxEqMidGain", s)),
            pair(self.knob("Mid Q", "FxEqMidQual", s), Space::with_width(Length::Fill).into()),
            pair(self.knob("High F", "FxEqHsvFreq", s), self.knob("High G", "FxEqHsvGain", s)),
        ]
        .spacing(12)
        .width(Length::Fill);
        column![card(header.into()), row![card(comp.into()), card(eq.into())].spacing(16)].spacing(14).into()
    }

    /// Compressor gain reduction. Slot 20 + 2*strip (moves on strip 1 under compression,
    /// measured 2026-11-06); scale from Yamaha's Pjled_GR_Tbl. Idle strips 3/4 report 3903: shown as "–".
    fn gr_meter(&self, s: u16) -> Element<'_, Msg> {
        // Yamaha's hardware GR scale (Pjled_GR_Tbl): same units as the level meters, 8192 = 0 dB.
        let raw = self.meters.get(20 + 2 * s as usize).copied().unwrap_or(40960);
        let gr = if raw == 3903 || raw == 0 {
            None
        } else {
            let (_, label) = ur44::dsp::gr_segment(raw);
            Some(label.parse::<f32>().unwrap_or(0.0))
        };
        let frac = gr.map_or(0.0, |g| -g / 24.0);
        let bar = (frac * 1000.0) as u16;
        let mut col = column![].width(10).height(Length::Fill);
        if bar > 0 {
            col = col.push(container(Space::with_width(10)).height(Length::FillPortion(bar)).style(|_: &Theme| container::Style::default().background(Color::from_rgb(0.96, 0.62, 0.25))));
        }
        col = col.push(Space::with_height(Length::FillPortion(1000u16.saturating_sub(bar).max(1))));
        column![
            text("GR").size(11),
            container(col).width(10).height(80).style(glass::trough),
            text(gr.map_or("\u{2013}".to_string(), |g| format!("{:.1}", if g > -0.05 { 0.0 } else { g }))).size(11),
        ]
        .spacing(4)
        .align_x(Alignment::Center)
        .into()
    }

    // ----- guitar amp page -----

    fn guitar_page(&self) -> Element<'_, Msg> {
        let user = (0..6u16).find(|&c| logic::insert_of(&self.values, c).kind == FxKind::GuitarAmp);
        let title = fx_title(match user {
            Some(c) => format!("Guitar Amp on IN {}", c + 1),
            None => "Guitar Amp".into(),
        }, user.map(|c| (c, logic::insert_of(&self.values, c).pos)));
        let body: Element<Msg> = match (user, self.preview) {
            (Some(_), _) => self.amp_editor(),
            (None, true) => column![preview_note("Guitar Amp"), self.amp_editor()].spacing(10).into(),
            (None, false) => {
                let free: Vec<u16> = (0..6u16)
                    .filter(|&c| logic::insert_of(&self.values, c).kind == FxKind::None && !self.on("ChPairOnf", c & !1))
                    .collect();
                empty_fx(
                    "The Guitar Amp isn\u{2019}t on an input".into(),
                    "Amp and speaker sound for a guitar plugged straight into a mono input. One input at a time can use it.",
                    free,
                    FxKind::GuitarAmp,
                    "Edit amp settings anyway",
                )
            }
        };
        scrollable(column![title, body].spacing(14)).style(glass::scroll).into()
    }

    fn amp_editor(&self) -> Element<'_, Msg> {
        let amp = self.get("GtrFxType", 0).clamp(0, 3) as usize;
        let bypass = self.on("GtrFxBypass", 0);
        let mut amp_col = column![
            row![
                text("Amp").size(18).color(glass::TEXT_HI),
                horizontal_space(),
                container(self.choice("GtrFxType", 0)).width(170),
                toggle_w("BYPASS", bypass, Msg::Set("GtrFxBypass", 0, flip(bypass)), 90),
            ]
            .spacing(10)
            .align_y(Alignment::Center),
        ]
        .spacing(12);
        let mut cab = column![text("Speaker & mic").size(18).color(glass::TEXT_HI)].spacing(12);
        let mut modu = column![text("Chorus / vibrato").size(18).color(glass::TEXT_HI)].spacing(12);
        let mut has_mod = false;
        let chorus_off = logic::guitar_items(amp).into_iter().find(|n| n.ends_with("Chorus")).is_some_and(|n| units::format(n, self.get(n, 0)) == "Off");
        for name in logic::guitar_items(amp) {
            if name.contains("UserParam") || name.contains("Hid") {
                continue;
            }
            let p = params::by_name(name).expect("param");
            let label = gtr_label(name);
            let tail = gtr_tail(name);
            let is_enum = ["SpType", "MicPosition", "Chorus", "Type", "Sensitivity"].contains(&tail);
            let w: Element<Msg> = if is_enum { self.labelled_choice(label, p.name, 0) } else { self.knob(label, p.name, 0) };
            match tail {
                "SpType" | "MicPosition" | "SpAir" => cab = cab.push(w),
                "Chorus" => {
                    has_mod = true;
                    modu = modu.push(w);
                }
                "VibSpeed" | "VibDepth" => {
                    if !chorus_off {
                        modu = modu.push(w);
                    }
                }
                _ => amp_col = amp_col.push(w),
            }
        }
        if has_mod && chorus_off {
            modu = modu.push(text("Speed and depth appear when chorus or vibrato is on.").size(12).color(glass::TEXT_MUTED));
        }
        let mut side = column![card(cab.into())].spacing(16).width(Length::FillPortion(2));
        if has_mod {
            side = side.push(card(modu.into()));
        }
        row![container(card(amp_col.into())).width(Length::FillPortion(3)), side].spacing(16).into()
    }

    // ----- reverb page -----

    fn reverb_page(&self) -> Element<'_, Msg> {
        let bypass = self.on("FxRevBypass", 0);
        let panel = column![
            row![
                column![
                    text("REV-X reverb").size(22).color(glass::TEXT_HI),
                    text("One reverb shared by all inputs: each input\u{2019}s REV SEND sets how much of it goes in; each mix sets how loud the reverb is.").size(12).color(glass::TEXT_MUTED),
                ]
                .spacing(2),
                horizontal_space(),
                container(self.choice("FxRevType", 0)).width(140),
                toggle_w("BYPASS", bypass, Msg::Set("FxRevBypass", 0, flip(bypass)), 100),
            ]
            .spacing(12)
            .align_y(Alignment::Center),
            card(
                canvas(graphs::Reverb {
                    seconds: ur44::dsp::reverb_time_s(self.get("FxRevType", 0), self.get("FxRevRoomSize", 0), self.get("FxRevTime", 0)),
                    ini_dly: self.get("FxRevIniDly", 0),
                    hi_ratio: self.get("FxRevHighRatio", 0),
                    low_ratio: self.get("FxRevLowRatio", 0),
                })
                .width(Length::Fill)
                .height(190)
                .into()
            ),
            row![
                card(
                    column![
                        text("Time & space").size(16).color(glass::TEXT_HI),
                        self.knob("Reverb time", "FxRevTime", 0),
                        self.knob("Room size", "FxRevRoomSize", 0),
                        self.knob("Initial delay", "FxRevIniDly", 0),
                        self.knob("Decay", "FxRevDecay", 0),
                        self.knob("Diffusion", "FxRevDiff", 0),
                    ]
                    .spacing(12)
                    .into()
                ),
                card(
                    column![
                        text("Tone").size(16).color(glass::TEXT_HI),
                        self.knob("High-pass filter", "FxRevHpf", 0),
                        self.knob("Low-pass filter", "FxRevLpf", 0),
                        self.knob("High-band decay (\u{d7} reverb time)", "FxRevHighRatio", 0),
                        self.knob("Low-band decay (\u{d7} reverb time)", "FxRevLowRatio", 0),
                        self.knob("Low band up to", "FxRevLowFreq", 0),
                        self.knob("Balance (dry / wet)", "FxRevBal", 0),
                    ]
                    .spacing(12)
                    .into()
                ),
            ]
            .spacing(16),
        ]
        .spacing(14);
        scrollable(panel).style(glass::scroll).into()
    }

    // ----- scenes page -----

    fn scenes_page(&self) -> Element<'_, Msg> {
        let mut list = column![].spacing(6);
        if self.scenes.is_empty() {
            list = list.push(text("No scenes stored yet.").size(14));
        }
        for (i, s) in self.scenes.iter().enumerate() {
            list = list.push(
                row![
                    text(s.name.clone()).size(15),
                    horizontal_space(),
                    button(text("Recall").size(13)).style(glass::primary).on_press(Msg::SceneRecall(i)),
                    button(text("Delete").size(13)).style(glass::danger).on_press(Msg::SceneDelete(i)),
                ]
                .spacing(8)
                .align_y(Alignment::Center),
            );
        }
        let panel = column![
            text("Scenes").size(22),
            card(
                column![
                    text("Store the current mixer, effects and settings as a scene.").size(13),
                    row![
                        text_input("Scene name", &self.scene_name).on_input(Msg::SceneName).on_submit(Msg::SceneStore).padding(8).style(glass::field),
                        button(text("Store").size(14)).padding([8, 16]).style(glass::primary).on_press(Msg::SceneStore),
                    ]
                    .spacing(8),
                ]
                .spacing(8)
                .into()
            ),
            card(list.into()),
            row![
                text("Scenes are JSON files; copy them in or out of the folder to import or export.").size(13),
                horizontal_space(),
                button(text(os::FILE_MANAGER).size(13)).style(glass::secondary).on_press(Msg::ScenesFolder),
            ]
            .align_y(Alignment::Center),
        ]
        .spacing(14)
        .max_width(720);
        scrollable(panel).style(glass::scroll).into()
    }

    // ----- settings page -----

    fn settings_page(&self) -> Element<'_, Msg> {
        let line = |label: &'static str, control: Element<'static, Msg>, note: &'static str| -> Element<'static, Msg> {
            column![row![text(label).size(15), horizontal_space(), container(control).width(200)].align_y(Alignment::Center), text(note).size(12)]
                .spacing(4)
                .into()
        };
        let hpf = self.get("ChHpfFreq", 0);
        let hpf_opts: Vec<String> = (0..=4).map(|i| units::format("ChHpfFreq", i)).collect();
        let hpf_lookup = hpf_opts.clone();
        let loop_on = self.on("PatchLoopbackMode", 0);
        let panel = column![
            text("Settings").size(22),
            card(
                column![
                    line("Sample rate", text(units::format("SysFsCode", self.get("SysFsCode", 0))).size(15).into(), os::SAMPLE_RATE_NOTE),
                    line(
                        "Input 5/6 level",
                        pick_list(ur44::catalog::display::INPUT56_LEVEL, Some(ur44::catalog::display::INPUT56_LEVEL[self.get("HaInputGain", 2).clamp(0, 1) as usize]), |s: &str| {
                            Msg::Set("HaInputGain", 2, ur44::catalog::display::INPUT56_LEVEL.iter().position(|t| *t == s).unwrap_or(0) as i32)
                        })
                        .width(Length::Fill)
                        .into(),
                        "Reference level of LINE INPUT 5/6.",
                    ),
                    line(
                        "High-pass filter",
                        pick_list(hpf_opts, Some(units::format("ChHpfFreq", hpf)), move |s: String| {
                            Msg::SetAll("ChHpfFreq", hpf_lookup.iter().position(|t| *t == s).unwrap_or(2) as i32)
                        })
                        .width(Length::Fill)
                        .into(),
                        "Cutoff for inputs 1\u{2013}4 when their HPF button is on.",
                    ),
                    line("Loopback", toggle(if loop_on { "ON" } else { "OFF" }, loop_on, Msg::Set("PatchLoopbackMode", 0, flip(loop_on)), Kind::Accent), "Sends MIX 1 back to the computer for streaming. Turn off for multitrack recording."),
                    line(
                        "PHONES 2 source",
                        pick_list(["MIX 1", "MIX 2"], Some(if self.on("PatchAuxPhoneOutput", 0) { "MIX 2" } else { "MIX 1" }), |s: &str| {
                            Msg::Set("PatchAuxPhoneOutput", 0, i32::from(s == "MIX 2"))
                        })
                        .width(Length::Fill)
                        .into(),
                        "PHONES 1 always follows MIX 1.",
                    ),
                ]
                .spacing(18)
                .into()
            ),
            text("Background").size(22),
            card(
                column![
                    line(
                        "Start at login",
                        toggle(if self.prefs.start_at_login { "ON" } else { "OFF" }, self.prefs.start_at_login, Msg::PrefLogin(!self.prefs.start_at_login), Kind::Accent),
                        "Runs UR44 Mixer in the background when you log in, without opening a window.",
                    ),
                    line(
                        "Show in",
                        pick_list([os::TRAY, "Invisible"], Some(if self.prefs.tray == prefs::TrayMode::MenuBar { os::TRAY } else { "Invisible" }), |s: &str| {
                            Msg::PrefTray(if s == os::TRAY { prefs::TrayMode::MenuBar } else { prefs::TrayMode::Hidden })
                        })
                        .width(Length::Fill)
                        .into(),
                        os::HIDDEN_NOTE,
                    ),
                    line(
                        "Restore last settings",
                        toggle(if self.prefs.restore_on_connect { "ON" } else { "OFF" }, self.prefs.restore_on_connect, Msg::PrefRestore(!self.prefs.restore_on_connect), Kind::Accent),
                        "When the UR44 is switched on, put back any mixer settings that differ from your last session.",
                    ),
                ]
                .push_maybe(os::GLASS.then(|| {
                    line(
                        "Liquid Glass",
                        toggle(if self.prefs.glass { "ON" } else { "OFF" }, self.prefs.glass, Msg::PrefGlass(!self.prefs.glass), Kind::Accent),
                        "Translucent window with the macOS glass material behind the controls. Follows \u{201c}Reduce transparency\u{201d}.",
                    )
                }))
                .push(
                    line(
                        "Quit",
                        button(text("Quit UR44 Mixer").size(13).center().width(Length::Fill)).width(Length::Fill).style(glass::danger).on_press(Msg::Quit).into(),
                        "Stops the background app until you open it again or log in.",
                    ),
                )
                .spacing(18)
                .into()
            ),
        ]
        .spacing(14)
        .max_width(720);
        scrollable(panel).style(glass::scroll).into()
    }
}

// ---------- small widgets ----------

/// Height of every button row, so controls line up across strips.
const ROW: f32 = 26.0;
/// Height of the control area above the faders when an input shows its effect row.
const TOP: f32 = 282.0;
/// Height of the control area above the faders with the inspector open (compact strips).
const COMPACT_TOP: f32 = 196.0;
/// Width of the input inspector.
const INSPECTOR_W: f32 = 290.0;

fn on_off(on: bool) -> &'static str {
    if on { "ON" } else { "OFF" }
}

fn tip<'a>(s: impl text::IntoFragment<'a>) -> Element<'a, Msg> {
    container(text(s).size(12)).padding(6).max_width(320).style(glass::well).into()
}

/// The effect picker of an input (same messages as the inspector's segmented control).
fn fx_picker<'a>(ch: u16, kind: FxKind, pos: FxPos) -> Element<'a, Msg> {
    let cur = match kind {
        FxKind::None => FX_CHOICES[0],
        FxKind::ChannelStrip => FX_CHOICES[1],
        FxKind::GuitarAmp => FX_CHOICES[2],
    };
    pick_list(FX_CHOICES, Some(cur), move |s: &str| {
        let kind = match s {
            "Ch Strip" => FxKind::ChannelStrip,
            "Guitar Amp" => FxKind::GuitarAmp,
            _ => FxKind::None,
        };
        Msg::Insert(ch, kind, pos)
    })
    .text_size(11)
    .padding([4, 6])
    .width(Length::Fill)
    .into()
}

/// A label on the left, its control on the right (inspector rows).
fn setting_row<'a>(label: &'a str, control: Element<'a, Msg>) -> Element<'a, Msg> {
    row![text(label).size(13), horizontal_space(), control].align_y(Alignment::Center).height(ROW).into()
}

/// Inspector section heading: a caps title and a quiet qualifier.
fn section_title<'a>(title: &'a str, note: &'a str) -> Element<'a, Msg> {
    row![text(title).size(12).color(glass::TEXT_HI), text(note).size(11).color(glass::TEXT_MUTED)].spacing(8).align_y(Alignment::Center).into()
}

/// "Name: explanation" with the name emphasised.
fn rich_line<'a>(name: &'a str, body: &'a str) -> Element<'a, Msg> {
    column![text(name).size(12).color(glass::TEXT_HI), text(body).size(11).color(glass::TEXT_MUTED)].spacing(1).into()
}

/// Settings shared by both mixes sit on a neutral inset.
fn shared_box<'a>(content: iced::widget::Column<'a, Msg>) -> Element<'a, Msg> {
    container(content.spacing(6)).padding([5, 4]).width(Length::Fill).style(glass::shared_inset).into()
}

/// Reverb send (shared by both mixes): neutral style, with a tooltip saying so.
fn shared_slider<'a>(label: &'a str, name: &'static str, ch: u16, value: i32) -> Element<'a, Msg> {
    let body = column![
        row![text(label).size(11), horizontal_space(), text(value_text(name, value)).size(11).color(glass::TEXT_HI)],
        slider(0..=127, value, move |v| Msg::Set(name, ch, v)).height(16).style(glass::slider_shared),
    ]
    .spacing(2);
    tooltip(body, tip("Reverb send: shared by MIX 1 and MIX 2"), tooltip::Position::Top).into()
}

/// A per-mix slider (pan, balance) in the mix tint.
fn mix_slider<'a>(label: &'a str, name: &'static str, ch: u16, range: std::ops::RangeInclusive<i32>, value: i32, mix: u8) -> Element<'a, Msg> {
    column![
        row![text(label).size(11), horizontal_space(), text(value_text(name, value)).size(11).color(glass::TEXT_HI)],
        slider(range, value, move |v| Msg::Set(name, ch, v)).height(16).style(glass::slider_mix(mix)),
    ]
    .spacing(2)
    .into()
}

/// Title of an effect page, with where the effect is used and a way back to that input.
fn fx_title<'a>(title: String, used: Option<(u16, FxPos)>) -> Element<'a, Msg> {
    let mut r = row![text(title).size(22).color(glass::TEXT_HI)].spacing(12).align_y(Alignment::Center);
    match used {
        Some((c, pos)) => {
            r = r.push(text(if pos == FxPos::Mon { "MON.FX \u{b7} you hear it, the computer records the dry input" } else { "INS.FX \u{b7} recorded on the computer too" }).size(12).color(glass::TEXT_MUTED));
            r = r.push(horizontal_space());
            r = r.push(button(text(format!("Show IN {} in the mixer", c + 1)).size(13)).padding([6, 12]).style(glass::secondary).on_press(Msg::ShowInput(c)));
        }
        None => {
            r = r.push(container(text("Not on an input").size(11)).padding([3, 9]).style(glass::badge(false, glass::TEXT_MUTED)));
        }
    }
    r.into()
}

/// Empty state of an effect page: what it is, and buttons to put it on an input.
fn empty_fx<'a>(title: String, what: &'a str, free: Vec<u16>, kind: FxKind, anyway: &'a str) -> Element<'a, Msg> {
    let mut inputs = row![].spacing(8);
    for c in &free {
        inputs = inputs.push(button(text(format!("IN {}", c + 1)).size(14)).padding([8, 18]).style(glass::primary).on_press(Msg::Assign(*c, kind)));
    }
    let pick: Element<Msg> = if free.is_empty() {
        text("Every suitable input already has an effect. Set one to \u{201c}None\u{201d} in the mixer (click the input name) first.").size(13).color(glass::TEXT).into()
    } else {
        column![text("Put it on:").size(13).color(glass::TEXT), inputs].spacing(8).into()
    };
    let col = column![
        text(title).size(18).color(glass::TEXT_HI),
        text(what).size(13).color(glass::TEXT),
        Space::with_height(2),
        pick,
        Space::with_height(2),
        row![
            text("You can also choose the effect from an input\u{2019}s settings in the mixer.").size(12).color(glass::TEXT_MUTED),
            horizontal_space(),
            button(text(anyway).size(12)).padding([5, 12]).style(glass::secondary).on_press(Msg::Preview(true)),
        ]
        .spacing(12)
        .align_y(Alignment::Center),
    ]
    .spacing(10);
    container(col).padding(24).max_width(760).style(glass::panel).into()
}

/// Shown above the editor of an effect that is not on any input.
fn preview_note<'a>(what: &'a str) -> Element<'a, Msg> {
    container(
        row![
            text(format!("This {what} is not on an input: changes are kept and apply once it is.")).size(13).color(glass::TEXT),
            horizontal_space(),
            button(text("Done").size(12)).padding([4, 12]).style(glass::secondary).on_press(Msg::Preview(false)),
        ]
        .align_y(Alignment::Center),
    )
    .padding([8, 12])
    .width(Length::Fill)
    .style(glass::shared_inset)
    .into()
}

/// Two controls side by side, equal widths.
fn two_up<'a>(a: Element<'a, Msg>, b: Element<'a, Msg>) -> Element<'a, Msg> {
    row![container(a).width(Length::Fill), container(b).width(Length::Fill)].spacing(18).into()
}

fn card_tight<'a>(content: Element<'a, Msg>) -> Element<'a, Msg> {
    container(content).padding(12).width(Length::Fill).style(glass::panel).into()
}

#[derive(Clone, Copy)]
enum Kind {
    Accent,
    Mute,
    Solo,
}

const ACCENT: Color = Color { r: 0.16, g: 0.76, b: 0.87, a: 1.0 };
const MUTE: Color = Color { r: 0.93, g: 0.28, b: 0.32, a: 1.0 };
const SOLO: Color = Color { r: 0.98, g: 0.82, b: 0.24, a: 1.0 };

fn flip(on: bool) -> i32 {
    i32::from(!on)
}

/// A toggle: lit (solid fill, glow) when on, outlined and dim when off.
fn toggle<'a>(label: &'a str, on: bool, msg: Msg, kind: Kind) -> Element<'a, Msg> {
    let tint = match kind {
        Kind::Accent => ACCENT,
        Kind::Mute => MUTE,
        Kind::Solo => SOLO,
    };
    button(text(label).size(11).center().width(Length::Fill))
        .width(Length::Fill)
        .height(ROW)
        .padding([5, 2])
        .style(move |t: &Theme, s| if on { glass::toggle_on(tint)(t, s) } else { glass::toggle_off(t, s) })
        .on_press(msg)
        .into()
}

fn toggle_w<'a>(label: &'a str, on: bool, msg: Msg, width: u16) -> Element<'a, Msg> {
    container(toggle(label, on, msg, Kind::Accent)).width(width).into()
}

/// A quiet fixed property of a strip (LINE, STEREO, OUT 1/2).
fn caption<'a>(label: &'a str) -> Element<'a, Msg> {
    text(label).size(10).color(Color::from_rgba(0.80, 0.83, 0.92, 0.6)).into()
}

/// Thin line in the mix colour: controls below it belong to the selected mix.
fn mix_rule<'a>(mix: u8) -> Element<'a, Msg> {
    let t = glass::mix_tint(mix);
    container(Space::with_height(2))
        .width(Length::Fill)
        .style(move |_: &Theme| container::Style::default().background(Color { a: 0.7, ..t }).border(iced::border::rounded(1)))
        .into()
}

/// Gains as "-12.5 dB" / "+2.0 dB" / "-∞"; everything else as the parameter's own text.
fn value_text(name: &str, v: i32) -> String {
    let s = units::format(name, v);
    if name.ends_with("Level") {
        if let Some(n) = s.strip_suffix(" dB").and_then(|n| n.parse::<f32>().ok()) {
            return if n > 0.0 { format!("+{n:.1} dB") } else { format!("{n:.1} dB") };
        }
    }
    s
}

/// A plain vertical level meter (-72..0 dBFS) for editor pages.
fn meter<'a>(level: u32, peak: u32) -> Element<'a, Msg> {
    let frac = |v: u32| ((meter_db(v) + 72.0) / 72.0).clamp(0.0, 1.0);
    let l = (frac(level) * 1000.0) as u16;
    let p = (frac(peak) * 1000.0) as u16;
    let db = meter_db(level);
    let color = if db > -1.0 {
        Color::from_rgb(0.96, 0.30, 0.34)
    } else if db > -6.0 {
        Color::from_rgb(0.98, 0.80, 0.28)
    } else {
        Color::from_rgb(0.36, 0.84, 0.52)
    };
    let fill = move |c: Color| move |_: &Theme| container::Style::default().background(c);
    let mut col = column![].width(8).height(Length::Fill);
    col = col.push(Space::with_height(Length::FillPortion(1000u16.saturating_sub(p.max(l)).max(1))));
    if p > l + 10 {
        col = col.push(container(Space::with_height(2)).width(8).style(fill(Color::from_rgb(0.85, 0.87, 0.92))));
        col = col.push(Space::with_height(Length::FillPortion((p - l).max(1))));
    }
    if l > 0 {
        col = col.push(container(Space::with_width(8)).height(Length::FillPortion(l)).style(fill(color)));
    }
    container(col).width(8).height(Length::Fill).style(glass::trough).into()
}

/// Fader position of 0 dB (unity).
const UNITY: i32 = 103;

/// Fader + meters on one shared dB scale, with the gain readout below (click it to reset to 0 dB).
fn fader<'a>(mix: u8, name: &'static str, ch: u16, value: i32, meters: Vec<(u32, u32)>) -> Element<'a, Msg> {
    let n = meters.len();
    let w = graphs::unit_width(n);
    let unit = iced::widget::stack![
        canvas(graphs::FaderScale { meters }).width(Length::Fill).height(Length::Fill),
        row![
            Space::with_width(graphs::SCALE_W),
            vertical_slider(0..=127, value, move |v| Msg::Set(name, ch, v)).width(graphs::LANE_W).height(Length::Fill).style(glass::fader(mix)),
        ],
    ]
    .width(w)
    .height(Length::Fill);
    let readout = button(text(value_text(name, value)).size(13).center().width(Length::Fill))
        .width(Length::Fill)
        .padding([3, 4])
        .style(button::text)
        .on_press(Msg::Set(name, ch, UNITY));
    column![
        container(unit).center_x(Length::Fill).height(Length::Fill),
        tooltip(readout, container(text("Click to reset to 0 dB").size(12)).padding(6).style(glass::well), tooltip::Position::Top),
    ]
    .spacing(4)
    .height(Length::Fill)
    .into()
}

fn strip_box<'a>(mix: u8, selected: bool, top_h: f32, top: iced::widget::Column<'a, Msg>, fader: Element<'a, Msg>) -> Element<'a, Msg> {
    let content = column![container(top.spacing(6).align_x(Alignment::Center)).height(top_h), fader].spacing(4).height(Length::Fill);
    container(content).width(Length::Fill).max_width(160).height(Length::Fill).padding([8, 5]).style(glass::strip_panel(mix, selected)).into()
}

fn card<'a>(content: Element<'a, Msg>) -> Element<'a, Msg> {
    container(content).padding(16).width(Length::Fill).style(glass::panel).into()
}

fn centered<'a>(content: iced::widget::Column<'a, Msg>) -> Element<'a, Msg> {
    container(content.spacing(8).align_x(Alignment::Center)).center(Length::Fill).into()
}

/// The part of a guitar amp parameter name after the amp model ("GtrFxCleanBass" -> "Bass").
fn gtr_tail(name: &str) -> &str {
    name.trim_start_matches("GtrFx").trim_start_matches("Clean").trim_start_matches("Crunch").trim_start_matches("Lead").trim_start_matches("Drive")
}

fn gtr_label(name: &str) -> &'static str {
    match gtr_tail(name) {
        "Blend" => "Blend",
        "Volume" => "Volume",
        "Distortion" => "Distortion",
        "Bass" => "Bass",
        "Middle" => "Middle",
        "Treble" => "Treble",
        "HiTreble" => "Hi treble",
        "Presence" => "Presence",
        "Output" => "Output",
        "SpType" => "Speaker",
        "SpAir" => "Air",
        "MicPosition" => "Mic position",
        "Chorus" => "Chorus / Vibrato",
        "VibSpeed" => "Speed",
        "VibDepth" => "Depth",
        "Type" => "Character",
        "Sensitivity" => "Sensitivity",
        "PreAmp" => "Pre amp",
        "Master" => "Master",
        "Gain" => "Gain",
        _ => "Setting",
    }
}
