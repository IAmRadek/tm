// A drum-list modal for tm in whoami's terminal style, driven by the timebuzzer knob: a turn
// moves one row, and the button gives click, double click and long press. The keyboard stands in
// for the knob:
//   Left / Right     rotate one detent (row up / row down)
//   Enter or Space   the knob's button: click, double click, long press
//   Esc              quit
// The drums show tm's database, read again each time the window shows and after each write;
// start, stop and discard run the tm CLI, which writes it and tells tm-daemon.
use std::process::Command;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use raylib::prelude::*;

mod macos;
mod store;

use chrono::Utc;
use store::Project;

// the tm CLI, found on PATH
const TM: &str = "tm";

// the layout is drawn on a W × H canvas and zoomed by SCALE into the window
const W: f32 = 360.0;
const H: f32 = 320.0;
const SCALE: f32 = 2.0;
// a second press inside this window makes a double click, so a click fires only after it
const DOUBLE: f64 = 0.18;
const LONG: f64 = 0.6;
// a press shorter than this shows no long-press progress
const LONG_SHOW: f64 = 0.15;
// the drum springs back to the running entry after this long without input
const SPRING_BACK: f32 = 5.0;
// the cancel confirmation returns to tracking after this long without input
const CONFIRM_IDLE: f32 = 4.0;

const ROW: f32 = 38.0;
const BAND: f32 = 32.0;
// the band holding the running entry, tall enough for the timer
const BAND_LIVE: f32 = 48.0;
const BAND_Y: f32 = 166.0;
const RULE_Y: f32 = 36.0;
// the text size of an action row (close, stop, back, + new ...) against a project or task row
const ACTION_SIZE: f32 = 0.85;
// the running entry's row on the tracking drum, after close, cancel and stop
const CURRENT: usize = 3;

// whoami's terminal palette: phosphor green on near-black
const PANEL: Color = Color::new(10, 12, 10, 255);
const GREEN: Color = Color::new(51, 255, 102, 255);
const DIM: Color = Color::new(51, 255, 102, 120);
const GLOW: Color = Color::new(51, 255, 102, 50);
const DANGER: Color = Color::new(255, 85, 85, 255);
// the dashes of the band's outline around a selected action
const DASH: f32 = 5.0;
const DASH_GAP: f32 = 4.0;

// the knob's three LED zones, each channel 0 to 127: the phosphor green, dim while tracking and
// bright in the center while the window is open; red while a cancel waits for its confirmation
const LED_TRACK: timebuzzer::Color = timebuzzer::Color { r: 6, g: 40, b: 12 };
const LED_OPEN: timebuzzer::Color = timebuzzer::Color { r: 25, g: 127, b: 51 };
const LED_IDLE_OPEN: timebuzzer::Color = timebuzzer::Color { r: 3, g: 20, b: 6 };
// the cancel confirmation pulses all zones red, once per LED_PULSE, between these fractions of
// full red
const LED_PULSE: Duration = Duration::from_secs(1);
const LED_PULSE_LOW: f32 = 0.1;
const LED_PULSE_HIGH: f32 = 1.0;
const LED_RESEND: Duration = Duration::from_millis(150);
// the hidden tracking wave: one pass over the zones per LED_WAVE, each zone between these
// fractions of LED_WAVE_COLOR, a violet of red and blue, written every LED_FRAME
const LED_WAVE_COLOR: timebuzzer::Color = timebuzzer::Color { r: 64, g: 0, b: 127 };
const LED_WAVE: Duration = Duration::from_secs(3);
const LED_WAVE_LOW: f32 = 0.1;
const LED_WAVE_HIGH: f32 = 1.0;
const LED_FRAME: Duration = Duration::from_millis(50);
// how often the hidden app reads the database, so the LEDs follow tm's CLI
const HIDDEN_POLL: Duration = Duration::from_secs(2);

enum Gesture {
    Cw,
    Ccw,
    Click,
    Double,
    Long,
}

// turns presses and releases of one key into click, double click and long press
#[derive(Default)]
struct Button {
    down_at: Option<f64>,
    long_fired: bool,
    // the release of a first click, waiting for a second press
    released_at: Option<f64>,
    // the press that completed a double click, whose release means nothing
    swallow: bool,
}

impl Button {
    fn update(&mut self, now: f64, pressed: bool, released: bool, out: &mut Vec<Gesture>) {
        if pressed {
            if self.released_at.is_some_and(|t| now - t < DOUBLE) {
                self.released_at = None;
                self.swallow = true;
                out.push(Gesture::Double);
            } else {
                self.down_at = Some(now);
                self.long_fired = false;
            }
        }
        if let Some(t) = self.down_at
            && !self.long_fired
            && now - t >= LONG
        {
            self.long_fired = true;
            out.push(Gesture::Long);
        }
        if released {
            if self.swallow {
                self.swallow = false;
            } else if self.down_at.is_some() && !self.long_fired {
                self.released_at = Some(now);
            }
            self.down_at = None;
        }
        if self.released_at.is_some_and(|t| now - t >= DOUBLE) {
            self.released_at = None;
            out.push(Gesture::Click);
        }
    }

    // how far a long press has come, 0 to 1, while the key is held
    fn long_progress(&self, now: f64) -> Option<f32> {
        let t = self.down_at?;
        if self.long_fired || now - t < LONG_SHOW {
            return None;
        }
        Some(((now - t) / LONG) as f32)
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Screen {
    // the tracking drum; with nothing running the app opens on Projects
    Home,
    Projects,
    Tasks(usize),
    Confirm,
    // typing a name for a new project or a new task in a project
    Input(Target),
}

#[derive(Clone, Copy, PartialEq)]
enum Target {
    Project,
    Task(usize),
}

#[derive(Clone, Copy, PartialEq)]
enum Row {
    Continue(usize, usize),
    Task(usize, usize),
    AllProjects,
    Project(usize),
    Back,
    Stop,
    Cancel,
    // the running entry
    Current,
    Keep,
    // an empty row that keeps discard a few deliberate detents away
    Gap,
    Discard,
    NewProject,
    NewTask(usize),
    // the name being typed on the input screen
    Entry,
    // hides the window until the next press of the knob
    Close,
}

struct Running {
    project: usize,
    task: usize,
    since: Instant,
}

struct App {
    // reads tm's database; writes go through the tm CLI
    conn: rusqlite::Connection,
    projects: Vec<Project>,
    screen: Screen,
    // the row in the band
    turn: usize,
    // where the drum is drawn, easing toward turn
    scroll: f32,
    // the band's height, easing between BAND and BAND_LIVE
    band: f32,
    // the projects-drum turn, restored when going back from a task drum
    project_turn: usize,
    running: Option<Running>,
    // tasks, most recently started first
    recent: Vec<(usize, usize)>,
    // time of today's stopped entries
    logged: Duration,
    last_input: Instant,
    flash: Option<(String, Instant)>,
    // the name typed on the input screen
    entry: String,
    // set by the close row; the main loop hides the window and clears it
    close: bool,
}

impl App {
    fn rows(&self) -> Vec<Row> {
        match self.screen {
            Screen::Home => match &self.running {
                Some(r) => {
                    let current = (r.project, r.task);
                    // close is the top row on every drum, one detent up from the default row
                    let mut rows = vec![Row::Close, Row::Cancel, Row::Stop, Row::Current];
                    rows.extend(
                        self.recent
                            .iter()
                            .filter(|&&task| task != current)
                            .map(|&(p, t)| Row::Task(p, t)),
                    );
                    rows.push(Row::AllProjects);
                    rows
                }
                None => vec![Row::Close, Row::AllProjects],
            },
            // the row after close goes back while tracking, and offers "continue last" when idle
            Screen::Projects => {
                let first = match (&self.running, self.recent.first()) {
                    (Some(_), _) => Some(Row::Back),
                    (None, Some(&(p, t))) => Some(Row::Continue(p, t)),
                    (None, None) => None,
                };
                std::iter::once(Row::Close)
                    .chain(first)
                    .chain((0..self.projects.len()).map(Row::Project))
                    .chain(std::iter::once(Row::NewProject))
                    .collect()
            }
            Screen::Tasks(p) => [Row::Close, Row::Back]
                .into_iter()
                .chain((0..self.projects[p].tasks.len()).map(|t| Row::Task(p, t)))
                .chain(std::iter::once(Row::NewTask(p)))
                .collect(),
            Screen::Confirm => vec![Row::Keep, Row::Gap, Row::Gap, Row::Discard],
            Screen::Input(_) => vec![Row::Entry],
        }
    }

    // the row of project p on the projects drum
    fn project_row(&self, p: usize) -> usize {
        1 + p + usize::from(self.running.is_some() || !self.recent.is_empty())
    }

    fn say(&mut self, message: impl Into<String>) {
        self.flash = Some((message.into(), Instant::now()));
    }

    fn go(&mut self, screen: Screen, turn: usize) {
        self.screen = screen;
        self.turn = turn;
        self.scroll = turn as f32;
    }

    // the tracking drum on the running entry, else the projects drum on its first row after close
    fn go_home(&mut self) {
        if self.running.is_some() {
            self.go(Screen::Home, CURRENT);
        } else {
            self.go(Screen::Projects, 1);
        }
    }

    // replaces the projects, recent tasks, running entry and today's time with the database's;
    // a project typed in but never started is dropped, because only starting a task saves it
    fn reload(&mut self) {
        let snapshot = match store::load(&self.conn) {
            Ok(snapshot) => snapshot,
            Err(e) => return self.fail("read", e),
        };
        self.projects = snapshot.projects;
        self.recent = snapshot.recent;
        self.logged = snapshot.logged;
        self.running = snapshot.running.map(|(project, task, started)| {
            let elapsed = (Utc::now() - started).to_std().unwrap_or_default();
            Running {
                project,
                task,
                since: Instant::now().checked_sub(elapsed).unwrap_or_else(Instant::now),
            }
        });
    }

    fn fail(&mut self, what: &str, e: impl std::fmt::Display) {
        eprintln!("tm-buzzer: {what}: {e}");
        self.say(format!("{what} failed"));
    }

    // runs the tm CLI, which writes the database and tells tm-daemon; tm prints its errors to
    // stderr and can still exit 0, so any stderr output counts as a failure
    fn tm(&mut self, what: &str, args: &[&str]) -> bool {
        match Command::new(TM).args(args).output() {
            Ok(out) if out.status.success() && out.stderr.is_empty() => true,
            Ok(out) => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                self.fail(what, format!("tm {}: {} {}", args.join(" "), out.status, stderr.trim()));
                false
            }
            Err(e) => {
                self.fail(what, format!("running {TM}: {e}"));
                false
            }
        }
    }

    // the write methods below call tm, reload, and return whether the write worked; a failure
    // shows its own message

    fn stop(&mut self) -> bool {
        let ok = self.tm("stop", &["stop"]);
        self.reload();
        ok
    }

    // deletes the running entry
    fn discard(&mut self) -> bool {
        let ok = self.tm("discard", &["cancel"]);
        self.reload();
        ok
    }

    // starts a task now; tm start refuses while an entry runs, so a switch stops it first and
    // starts the new task at that stop time, which leaves no gap. If the start fails after the
    // stop, nothing runs
    fn start(&mut self, p: usize, t: usize) -> bool {
        let project = self.projects[p].name.clone();
        let task = self.projects[p].tasks[t].clone();
        let ok = if self.running.is_some() {
            self.tm("stop", &["stop"]) && self.tm("start", &["start", &project, &task, "--from-last-stop"])
        } else {
            self.tm("start", &["start", &project, &task])
        };
        self.reload();
        ok
    }

    fn today(&self) -> Duration {
        self.logged + self.running.as_ref().map_or(Duration::ZERO, |r| r.since.elapsed())
    }

    fn back(&mut self) {
        match self.screen {
            Screen::Tasks(_) => self.go(Screen::Projects, self.project_turn),
            // the projects drum is home while idle
            Screen::Projects if self.running.is_none() => {}
            // back on the "+ new" row, the last one
            Screen::Input(Target::Project) => {
                self.screen = Screen::Projects;
                let new = self.rows().len() - 1;
                self.go(Screen::Projects, new);
            }
            Screen::Input(Target::Task(p)) => {
                self.screen = Screen::Tasks(p);
                let new = self.rows().len() - 1;
                self.go(Screen::Tasks(p), new);
            }
            _ => self.go_home(),
        }
    }

    // saves the typed name; a new project opens its task drum, a new task starts tracking
    fn submit(&mut self) {
        let name = self.entry.trim().to_string();
        if name.is_empty() {
            self.say("type a name first");
            return;
        }
        match self.screen {
            Screen::Input(Target::Project) => {
                let p = match self.projects.iter().position(|p| p.name == name) {
                    Some(p) => p,
                    None => {
                        self.projects.push(Project {
                            name,
                            tasks: Vec::new(),
                        });
                        self.say("project added");
                        self.projects.len() - 1
                    }
                };
                self.project_turn = self.project_row(p);
                // row 2, after close and back, is the first task, or "+ new task" in a project
                // without tasks
                self.go(Screen::Tasks(p), 2);
            }
            Screen::Input(Target::Task(p)) => {
                let tasks = &mut self.projects[p].tasks;
                let t = match tasks.iter().position(|t| *t == name) {
                    Some(t) => t,
                    None => {
                        tasks.push(name);
                        tasks.len() - 1
                    }
                };
                self.run(Row::Task(p, t));
            }
            _ => {}
        }
    }

    fn run(&mut self, row: Row) {
        match row {
            Row::Continue(p, t) => {
                if self.start(p, t) {
                    self.say("continued");
                }
                self.go_home();
            }
            Row::Task(p, t) => {
                if self.running.as_ref().is_some_and(|r| (r.project, r.task) == (p, t)) {
                    self.say("already tracking");
                } else {
                    let switched = self.running.is_some();
                    if self.start(p, t) {
                        self.say(if switched { "switched" } else { "started" });
                    }
                }
                self.go_home();
            }
            Row::AllProjects => {
                let turn = self.running.as_ref().map_or(1, |r| self.project_row(r.project));
                self.go(Screen::Projects, turn);
            }
            Row::NewProject => {
                self.entry.clear();
                self.go(Screen::Input(Target::Project), 0);
            }
            Row::NewTask(p) => {
                self.entry.clear();
                self.go(Screen::Input(Target::Task(p)), 0);
            }
            Row::Entry => self.submit(),
            Row::Close => self.close = true,
            Row::Project(p) => {
                self.project_turn = self.turn;
                let turn = match &self.running {
                    // tasks start at row 2, after close and back
                    Some(r) if r.project == p => r.task + 2,
                    _ => 2,
                };
                self.go(Screen::Tasks(p), turn);
            }
            Row::Back => self.back(),
            Row::Stop => {
                if self.stop() {
                    self.say("stopped");
                }
                self.go_home();
            }
            Row::Cancel => self.go(Screen::Confirm, 0),
            Row::Current => {}
            Row::Keep | Row::Gap => {
                self.say("kept");
                self.go_home();
            }
            Row::Discard => {
                if self.discard() {
                    self.say("entry discarded");
                }
                self.go_home();
            }
        }
    }

    fn apply(&mut self, gesture: Gesture) {
        self.last_input = Instant::now();
        match (gesture, self.screen) {
            (Gesture::Double, _) => {
                if self.running.is_some() {
                    if self.stop() {
                        self.say("stopped");
                    }
                } else if let Some(&(p, t)) = self.recent.first() {
                    if self.start(p, t) {
                        self.say("continued");
                    }
                } else {
                    self.say("nothing to continue");
                }
                self.go_home();
            }
            (Gesture::Long, Screen::Home) => {
                if self.running.is_some() {
                    self.go(Screen::Confirm, 0);
                }
            }
            (Gesture::Long, _) => self.back(),
            (Gesture::Cw, _) => self.turn = (self.turn + 1).min(self.rows().len() - 1),
            (Gesture::Ccw, _) => self.turn = self.turn.saturating_sub(1),
            (Gesture::Click, _) => {
                let row = self.rows()[self.turn];
                self.run(row);
            }
        }
    }

    // springs the drum back and closes the confirmation when the knob rests
    fn tick(&mut self, dt: f32) {
        let idle = self.last_input.elapsed().as_secs_f32();
        match self.screen {
            Screen::Home if self.running.is_some() && idle > SPRING_BACK => self.turn = CURRENT,
            Screen::Confirm if idle > CONFIRM_IDLE => self.go_home(),
            _ => {}
        }
        let ease = 1.0 - (-40.0 * dt).exp();
        self.scroll += (self.turn as f32 - self.scroll) * ease;
        let band = if self.rows()[self.turn] == Row::Current { BAND_LIVE } else { BAND };
        self.band += (band - self.band) * ease;
    }
}

// a row that does something to the drum or the entry, as opposed to a project or task to pick;
// the confirm screen's rows are all actions and keep their own look
fn is_action(row: Row) -> bool {
    matches!(
        row,
        Row::Close | Row::Cancel | Row::Stop | Row::Back | Row::AllProjects | Row::NewProject | Row::NewTask(_)
    )
}

// the colors of the left, center and right zones for the app's state
fn led_colors(app: &App, visible: bool) -> [timebuzzer::Color; 3] {
    let base = if app.running.is_some() { LED_TRACK } else { timebuzzer::Color::OFF };
    match (visible, app.screen, &app.running) {
        // timed from the last input, which opened the confirmation, so a pulse starts bright
        (true, Screen::Confirm, _) => {
            let t = app.last_input.elapsed().as_secs_f32() / LED_PULSE.as_secs_f32();
            let level = (1.0 + (t * std::f32::consts::TAU).cos()) / 2.0;
            [timebuzzer::Color::RED.dim(LED_PULSE_LOW + (LED_PULSE_HIGH - LED_PULSE_LOW) * level); 3]
        }
        (true, _, Some(_)) => [base, LED_OPEN, base],
        (true, _, None) => [base, LED_IDLE_OPEN, base],
        // hidden while tracking: a wave of green runs left to right, timed from the entry's start
        (false, _, Some(r)) => {
            let t = r.since.elapsed().as_secs_f32() / LED_WAVE.as_secs_f32();
            [0.0, 1.0, 2.0].map(|zone| {
                let phase = (t - zone / 3.0) * std::f32::consts::TAU;
                let level = (1.0 - phase.cos()) / 2.0;
                LED_WAVE_COLOR.dim(LED_WAVE_LOW + (LED_WAVE_HIGH - LED_WAVE_LOW) * level)
            })
        }
        (false, _, None) => [base; 3],
    }
}

// the knob's LEDs, written when the colors change; the device drops a message now and then, so
// every zone is written again LED_RESEND after a change
struct Lights {
    leds: Option<timebuzzer::Leds>,
    lit: Option<[timebuzzer::Color; 3]>,
    // when the colors last changed, until the second write
    changed_at: Option<Instant>,
}

impl Lights {
    fn show(&mut self, colors: [timebuzzer::Color; 3]) {
        let Some(leds) = &mut self.leds else { return };
        let changed = self.lit != Some(colors);
        let resend = self.changed_at.is_some_and(|t| t.elapsed() >= LED_RESEND);
        if !changed && !resend {
            return;
        }
        let zones = [timebuzzer::Zone::Left, timebuzzer::Zone::Center, timebuzzer::Zone::Right];
        for (zone, color) in zones.into_iter().zip(colors) {
            leds.set(zone, color);
        }
        self.lit = Some(colors);
        self.changed_at = changed.then(Instant::now);
    }

    fn off(&mut self) {
        if let Some(leds) = &mut self.leds {
            leds.off();
        }
        self.lit = None;
        self.changed_at = None;
    }
}

fn clock(d: Duration) -> String {
    let s = d.as_secs();
    format!("{}:{:02}:{:02}", s / 3600, s % 3600 / 60, s % 60)
}

fn hours(d: Duration) -> String {
    let m = d.as_secs() / 60;
    format!("{}:{:02}", m / 60, m % 60)
}

// the colour with its own alpha scaled by a, 0 to 1
fn fade(color: Color, a: f32) -> Color {
    Color {
        a: (color.a as f32 * a.clamp(0.0, 1.0)) as u8,
        ..color
    }
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t.clamp(0.0, 1.0)
}

type Canvas<'a, 'b, 'c> = RaylibMode2D<'a, RaylibBlendMode<'b, RaylibDrawHandle<'c>>>;

// the outline of rect in dashes of DASH with gaps of DASH_GAP, `thick` wide, inside the rect as
// draw_rectangle_lines_ex draws it
fn dashed(d: &mut Canvas, rect: Rectangle, thick: f32, color: Color) {
    let edges = [
        (rect.x, rect.y, rect.width, thick, true),
        (rect.x, rect.y + rect.height - thick, rect.width, thick, true),
        (rect.x, rect.y, thick, rect.height, false),
        (rect.x + rect.width - thick, rect.y, thick, rect.height, false),
    ];
    for (x, y, w, h, across) in edges {
        let len = if across { w } else { h };
        let mut at = 0.0;
        while at < len {
            let dash = DASH.min(len - at);
            let piece = if across {
                Rectangle::new(x + at, y, dash, h)
            } else {
                Rectangle::new(x, y + at, w, dash)
            };
            d.draw_rectangle_rec(piece, color);
            at += DASH + DASH_GAP;
        }
    }
}

fn main() {
    // the device calls back on midir's thread, so its events wait in a channel for the next frame;
    // the connection lives as long as `_knob`
    let (knob_tx, knob_rx) = mpsc::channel();
    let _knob = timebuzzer::listen(move |event| {
        let _ = knob_tx.send(event);
    });
    if _knob.is_none() {
        eprintln!("tm-buzzer: no timebuzzer found, using the keyboard only");
    }
    // with a knob the app starts hidden and waits for its button; without one it shows at once
    let mut visible = _knob.is_none();

    let (window_w, window_h) = ((W * SCALE) as i32, (H * SCALE) as i32);
    let mut builder = raylib::init();
    builder
        .size(window_w, window_h)
        .title("tm")
        .transparent()
        .undecorated()
        .highdpi()
        .topmost()
        .msaa_4x();
    if !visible {
        builder.hidden();
    }
    let (mut rl, thread) = builder.build();
    macos::set_accessory();
    unsafe { macos::move_to_active_space(rl.get_window_handle()) };
    rl.set_target_fps(60);
    // Esc goes back from the input screen, so the loop decides when it quits
    rl.set_exit_key(None);
    // SAFETY: the call stores blend factors in rlgl's global state and makes no GL call; this is
    // the only thread that touches rlgl, and raylib::init above set that state up. raylib's alpha
    // blending also scales the window's alpha by the source alpha, so translucent drawing over the
    // opaque panel lets the desktop through; with these factors alpha adds up over the panel instead
    unsafe {
        raylib::ffi::rlSetBlendFactorsSeparate(
            raylib::ffi::RL_SRC_ALPHA as i32,
            raylib::ffi::RL_ONE_MINUS_SRC_ALPHA as i32,
            raylib::ffi::RL_ONE as i32,
            raylib::ffi::RL_ONE_MINUS_SRC_ALPHA as i32,
            raylib::ffi::RL_FUNC_ADD as i32,
            raylib::ffi::RL_FUNC_ADD as i32,
        )
    };
    let monitor = get_current_monitor();
    rl.set_window_position(
        (get_monitor_width(monitor) - window_w) / 2,
        (get_monitor_height(monitor) - window_h) / 2,
    );
    // Doto, a dot-matrix font under the OFL (src/font/OFL.txt), built into the binary;
    // loaded large so the high-DPI framebuffer draws it sharp at every size used here
    let font = rl
        .load_font_from_memory(&thread, ".ttf", include_bytes!("font/Doto-Bold.ttf"), 96, None)
        .map(|font| {
            font.texture()
                .set_texture_filter(&thread, TextureFilter::TEXTURE_FILTER_BILINEAR);
            font.make_weak()
        })
        .unwrap_or_else(|e| {
            eprintln!("tm-buzzer: font: {e}, drawing with raylib's default");
            rl.get_font_default()
        });
    let user = std::env::var("USER").unwrap_or_else(|_| "root".to_string());

    let conn = store::open().expect("open tm's database read-only; run tm once to create it");
    let mut app = App {
        conn,
        projects: Vec::new(),
        screen: Screen::Projects,
        turn: 0,
        scroll: 0.0,
        band: BAND,
        project_turn: 1,
        running: None,
        recent: Vec::new(),
        logged: Duration::ZERO,
        last_input: Instant::now(),
        flash: None,
        entry: String::new(),
        close: false,
    };
    app.reload();
    app.go_home();
    let mut button = Button::default();
    // the knob's button has its own state, so a key and the knob never make one gesture together
    let mut knob = Button::default();
    let mut gestures = Vec::new();
    // the header command and when it changed, for typing it out
    let mut command = String::new();
    let mut command_at = Instant::now();
    let hidden = WindowState::default().set_window_hidden(true);
    let mut lights = Lights {
        leds: _knob.as_ref().and_then(|_| timebuzzer::open_leds()),
        lit: None,
        changed_at: None,
    };
    // whatever an earlier run left lit
    lights.off();
    lights.show(led_colors(&app, visible));

    'frames: while !rl.window_should_close() {
        if !visible {
            // nothing is on screen, so draw no frames until the knob's button goes down; turns and
            // the release of that press do nothing, because the knob's button never saw the press.
            // Between events the database is read again, for the LEDs
            // While tracking, it wakes every LED_FRAME for the wave
            let mut polled = Instant::now();
            loop {
                let wait = if app.running.is_some() { LED_FRAME } else { HIDDEN_POLL };
                match knob_rx.recv_timeout(wait) {
                    Ok(timebuzzer::Event::ClickDown) => break,
                    Ok(_) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => break 'frames,
                }
                if polled.elapsed() >= HIDDEN_POLL {
                    app.reload();
                    polled = Instant::now();
                }
                lights.show(led_colors(&app, false));
            }
            rl.clear_window_state(hidden);
            rl.set_window_focused();
            // tm's CLI may have changed the entries while the window was hidden
            app.reload();
            app.go_home();
            app.last_input = Instant::now();
            app.flash = None;
            // the header types its command out again
            command.clear();
            visible = true;
        }
        let now = rl.get_time();
        let dt = rl.get_frame_time().min(1.0 / 30.0);

        // the input screen takes the keyboard as a keyboard; everywhere else it stands in for the knob
        let mut typed = Vec::new();
        while let Some(ch) = rl.get_char_pressed() {
            typed.push(ch);
        }
        let key = |k| rl.is_key_pressed(k) || rl.is_key_pressed_repeat(k);
        if let Screen::Input(_) = app.screen {
            app.last_input = Instant::now();
            for ch in typed.into_iter().filter(|ch| !ch.is_control()) {
                if app.entry.chars().count() < 32 {
                    app.entry.push(ch);
                }
            }
            if key(KeyboardKey::KEY_BACKSPACE) {
                app.entry.pop();
            }
            if rl.is_key_pressed(KeyboardKey::KEY_ENTER) {
                app.submit();
            } else if rl.is_key_pressed(KeyboardKey::KEY_ESCAPE) {
                app.back();
            }
        } else {
            if rl.is_key_pressed(KeyboardKey::KEY_ESCAPE) {
                break;
            }
            if key(KeyboardKey::KEY_RIGHT) {
                gestures.push(Gesture::Cw);
            }
            if key(KeyboardKey::KEY_LEFT) {
                gestures.push(Gesture::Ccw);
            }
            let pressed = rl.is_key_pressed(KeyboardKey::KEY_ENTER)
                || rl.is_key_pressed(KeyboardKey::KEY_SPACE);
            let released = rl.is_key_released(KeyboardKey::KEY_ENTER)
                || rl.is_key_released(KeyboardKey::KEY_SPACE);
            button.update(now, pressed, released, &mut gestures);
            for g in gestures.drain(..) {
                app.apply(g);
            }
        }
        // the knob works on every screen; on the input screen a click saves and a long press goes back.
        // Each event updates the button alone, so a press and release in one frame still count apart
        for event in knob_rx.try_iter() {
            match event {
                timebuzzer::Event::Right => gestures.push(Gesture::Cw),
                timebuzzer::Event::Left => gestures.push(Gesture::Ccw),
                timebuzzer::Event::ClickDown => knob.update(now, true, false, &mut gestures),
                timebuzzer::Event::ClickUp => knob.update(now, false, true, &mut gestures),
            }
        }
        knob.update(now, false, false, &mut gestures);
        for g in gestures.drain(..) {
            app.apply(g);
        }
        if app.close {
            app.close = false;
            // without a knob nothing could show the window again, so close quits
            if _knob.is_none() {
                break;
            }
            rl.set_window_state(hidden);
            macos::hide();
            visible = false;
            lights.show(led_colors(&app, visible));
            // one empty frame before the wait for the knob: ending it polls the window's events,
            // and macOS takes the window off screen only when those are handled
            let mut d = rl.begin_drawing(&thread);
            d.clear_background(Color::BLANK);
            continue;
        }
        app.tick(dt);
        lights.show(led_colors(&app, visible));

        let measure = |s: &str, size: f32| font.measure_text(s, size, 0.0).x;
        let text = |d: &mut Canvas, s: &str, x: f32, y: f32, size: f32, color: Color| {
            d.draw_text_ex(&font, s, Vector2::new(x, y), size, 0.0, color);
        };
        let right = |d: &mut Canvas, s: &str, x: f32, y: f32, size: f32, color: Color| {
            d.draw_text_ex(&font, s, Vector2::new(x - measure(s, size), y), size, 0.0, color);
        };
        // digits on a fixed advance, so the timer does not jitter as they change
        let timer = |d: &mut Canvas, s: &str, x: f32, y: f32, size: f32, color: Color| {
            let digit = measure("0", size);
            let mut x = x;
            for ch in s.chars() {
                let advance = if ch == ':' { digit * 0.5 } else { digit };
                let g = ch.to_string();
                let pos = Vector2::new(x + (advance - measure(&g, size)) / 2.0, y);
                d.draw_text_ex(&font, &g, pos, size, 0.0, color);
                x += advance;
            }
        };

        let rows = app.rows();
        let n = rows.len();
        let sel = app.turn;
        let running = app.running.as_ref();
        let current = running.map(|r| (r.project, r.task));
        let current_row = rows.iter().position(|&r| r == Row::Current);
        let task_name = |p: usize, t: usize| app.projects[p].tasks[t].as_str();

        let (next, command_color) = match app.screen {
            Screen::Home => match current_row {
                Some(c) if sel > c => ("switch".to_string(), GREEN),
                Some(c) if sel < c => ("actions".to_string(), GREEN),
                Some(_) => (format!("track {}", app.projects[running.unwrap().project].name), GREEN),
                None => ("idle".to_string(), GREEN),
            },
            Screen::Projects if running.is_some() => ("projects".to_string(), GREEN),
            Screen::Projects => ("start".to_string(), GREEN),
            Screen::Tasks(p) => (format!("{}/tasks", app.projects[p].name), GREEN),
            Screen::Confirm => ("cancel entry?".to_string(), DANGER),
            Screen::Input(Target::Project) => ("new project".to_string(), GREEN),
            Screen::Input(Target::Task(p)) => (format!("{}/new task", app.projects[p].name), GREEN),
        };
        if next != command {
            command = next;
            command_at = Instant::now();
        }
        // the command types out at 50 characters a second, as whoami's does
        let typed = ((command_at.elapsed().as_secs_f32() * 50.0) as usize).min(command.len());
        let blink = if (now * 2.0) as u32 % 2 == 0 { "_" } else { "" };
        let status = match (app.screen, running) {
            (Screen::Projects, _) => format!("{sel}/{}", n - 1),
            (Screen::Tasks(_), _) => format!("{sel}/{}", n - 1),
            (Screen::Confirm, Some(r)) => format!("{} . {}", task_name(r.project, r.task), clock(r.since.elapsed())),
            _ => format!("today {}", hours(app.today())),
        };
        let hint = match app.screen {
            Screen::Home => match current_row {
                Some(c) if sel > c => "click: switch . 5s idle: spring back",
                Some(_) => "up: actions . down: switch . hold: cancel",
                None => "turn: move . click: select . 2x: continue",
            },
            Screen::Projects if running.is_some() => "click: open . hold: back",
            Screen::Projects => "turn: move . click: open . 2x: continue",
            Screen::Tasks(_) => "click: start . 2x: stop . hold: back",
            Screen::Confirm => "click on discard cancels . hold: keep",
            Screen::Input(_) => "type a name . enter: save . esc: back",
        };
        let flash = app.flash.as_ref().and_then(|(message, at)| {
            let age = at.elapsed().as_secs_f32();
            (age < 1.4).then(|| (message.clone(), (1.0 - (age - 0.9).max(0.0) / 0.5).clamp(0.0, 1.0)))
        });

        // BeginMode2D drops raylib's high-DPI scale, so the zoom maps the layout onto the
        // framebuffer's pixels
        let zoom = rl.get_render_width() as f32 / W;
        let point = W / window_w as f32;
        let mut window = rl.begin_drawing(&thread);
        window.clear_background(Color::BLANK);
        // the blend factors set after raylib::init apply only in this mode
        let mut blend = window.begin_blend_mode(BlendMode::BLEND_CUSTOM_SEPARATE);
        let mut d = blend.begin_mode2D(Camera2D {
            offset: Vector2::zero(),
            target: Vector2::zero(),
            rotation: 0.0,
            zoom,
        });
        d.draw_rectangle_rec(Rectangle::new(0.0, 0.0, W, H), PANEL);
        d.draw_rectangle_lines_ex(Rectangle::new(0.0, 0.0, W, H), point, DIM);

        // header: a shell prompt typing the screen's name, and a status on the right
        let prompt = format!("{user}@tm:~$ ");
        text(&mut d, &prompt, 14.0, 12.0, 12.0, DIM);
        let typing = format!("{}{blink}", &command[..typed]);
        text(&mut d, &typing, 14.0 + measure(&prompt, 12.0), 12.0, 12.0, command_color);
        right(&mut d, &status, W - 14.0, 12.0, 12.0, DIM);
        d.draw_line_ex(Vector2::new(0.0, RULE_Y), Vector2::new(W, RULE_Y), point, fade(DIM, 0.5));

        // the column the drum turns in; the other side holds a preview or a tab
        let (x0, x1) = match app.screen {
            Screen::Projects => (8.0, 168.0),
            Screen::Tasks(_) | Screen::Input(Target::Task(_)) => (92.0, W - 20.0),
            _ => (8.0, W - 8.0),
        };
        match app.screen {
            Screen::Projects => {
                // the selected project's tasks, ghosted
                match rows[sel] {
                    Row::Project(p) => {
                        for (k, task) in app.projects[p].tasks.iter().take(7).enumerate() {
                            let y = BAND_Y - 96.0 + k as f32 * 30.0;
                            text(&mut d, task, 188.0, y, 14.0, fade(DIM, 0.6));
                        }
                    }
                    Row::Continue(p, _) => {
                        text(&mut d, "continue", 188.0, BAND_Y - 18.0, 14.0, GREEN);
                        text(&mut d, &app.projects[p].name, 188.0, BAND_Y + 2.0, 14.0, fade(DIM, 0.6));
                    }
                    Row::NewProject => text(&mut d, "type a name", 188.0, BAND_Y - 7.0, 14.0, fade(DIM, 0.6)),
                    _ => {}
                }
            }
            Screen::Tasks(p) | Screen::Input(Target::Task(p)) => {
                let tab = Rectangle::new(8.0, BAND_Y - 18.0, 76.0, 36.0);
                d.draw_rectangle_rec(tab, fade(GLOW, 0.6));
                d.draw_rectangle_lines_ex(tab, point, fade(DIM, 0.6));
                text(&mut d, &app.projects[p].name, 16.0, BAND_Y - 7.0, 13.0, DIM);
                // where the drum sits in the whole list
                if n > 8 {
                    let (top, bottom) = (RULE_Y + 16.0, H - 40.0);
                    for i in 0..n {
                        let y = top + (bottom - top) * i as f32 / (n - 1) as f32;
                        let (len, color) = if i == sel { (10.0, GREEN) } else { (4.0, DIM) };
                        d.draw_line_ex(Vector2::new(W - 8.0 - len, y), Vector2::new(W - 8.0, y), point, color);
                    }
                }
            }
            _ => {}
        }

        // an action in the band gets a dashed outline, a project or task a solid one
        let band = Rectangle::new(x0, BAND_Y - app.band / 2.0, x1 - x0, app.band);
        d.draw_rectangle_rec(band, GLOW);
        if is_action(rows[sel]) {
            dashed(&mut d, band, point, DIM);
        } else {
            d.draw_rectangle_lines_ex(band, point, DIM);
        }
        text(&mut d, ">", x0 + 6.0, BAND_Y - 8.0, 16.0, GREEN);

        // rows shrink and fade with their distance from the band
        for (i, &row) in rows.iter().enumerate() {
            let dist = i as f32 - app.scroll;
            let far = dist.abs();
            if far > 3.2 {
                continue;
            }
            let offset = far.min(1.0) * (app.band / 2.0 + ROW / 2.0) + (far - 1.0).max(0.0) * ROW;
            let y = BAND_Y + dist.signum() * offset;
            let size = if far <= 1.0 { lerp(16.0, 22.0, far) } else { lerp(22.0, 16.0, far - 1.0) };
            let alpha = if far <= 1.0 { lerp(1.0, 0.65, far) } else { lerp(0.65, 0.0, (far - 1.0) / 2.2) };
            // the band's own details fade in as the row settles into it
            let settled = (1.0 - far * 2.0).max(0.0);
            let x = x0 + 22.0;

            if row == Row::Current && i == sel {
                let r = running.unwrap();
                text(&mut d, task_name(r.project, r.task), x, y - 21.0, 16.0, fade(GREEN, alpha));
                timer(&mut d, &clock(r.since.elapsed()), x, y - 3.0, 24.0, fade(GREEN, alpha));
                let project = &app.projects[r.project].name;
                right(&mut d, project, x1 - 12.0, y - 19.0, 12.0, fade(DIM, settled));
                continue;
            }

            let mark = |p: usize, t: usize| if Some((p, t)) == current { " *" } else { "" };
            let (label, color) = match row {
                Row::Continue(p, t) | Row::Task(p, t) => (format!("{}{}", task_name(p, t), mark(p, t)), GREEN),
                Row::AllProjects => ("all projects >".to_string(), GREEN),
                Row::Project(p) => {
                    let live = if current.is_some_and(|(cp, _)| cp == p) { " *" } else { "" };
                    (format!("{}{live}", app.projects[p].name), GREEN)
                }
                Row::Back if matches!(app.screen, Screen::Tasks(_)) => ("< project".to_string(), GREEN),
                Row::Back => ("< back".to_string(), GREEN),
                Row::Stop => ("[] stop".to_string(), GREEN),
                Row::Cancel => ("x cancel".to_string(), GREEN),
                Row::Current => (task_name(current.unwrap().0, current.unwrap().1).to_string(), GREEN),
                Row::Keep => ("keep running".to_string(), GREEN),
                Row::Gap => (".".to_string(), DIM),
                Row::Discard => (format!("x discard {}", clock(running.unwrap().since.elapsed())), DANGER),
                Row::NewProject => ("+ new project".to_string(), GREEN),
                Row::NewTask(_) => ("+ new task".to_string(), GREEN),
                Row::Entry => (format!("{}{blink}", app.entry), GREEN),
                Row::Close => ("close".to_string(), GREEN),
            };
            // actions sit back from the projects and tasks: dimmer and smaller until selected
            let (size, color) = match (is_action(row), i == sel) {
                (true, true) => (size * ACTION_SIZE, color),
                (true, false) => (size * ACTION_SIZE, DIM),
                (false, _) => (size, color),
            };
            text(&mut d, &label, x, y - size / 2.0, size, fade(color, alpha));

            // the right side: the running time on the current row, details in the band
            if row == Row::Current {
                let elapsed = clock(running.unwrap().since.elapsed());
                right(&mut d, &elapsed, x1 - 12.0, y - 7.0, 14.0, fade(GREEN, alpha));
            }
            if i != sel || settled == 0.0 {
                continue;
            }
            let (top, bottom) = match (row, app.screen) {
                (Row::Task(..), Screen::Home) if running.is_some() => {
                    ("switch".to_string(), format!("stops {}", clock(running.unwrap().since.elapsed())))
                }
                (Row::Task(p, _), Screen::Home) => (String::new(), app.projects[p].name.clone()),
                (Row::NewTask(_), _) => (String::new(), "keyboard".to_string()),
                (Row::Entry, _) => (String::new(), "enter: save".to_string()),
                _ => (String::new(), String::new()),
            };
            right(&mut d, &top, x1 - 12.0, y - 14.0, 12.0, fade(GREEN, settled));
            right(&mut d, &bottom, x1 - 12.0, y + 2.0, 11.0, fade(DIM, settled));
        }

        // footer: the hint, replaced by a flash message while one shows
        d.draw_line_ex(Vector2::new(0.0, H - 30.0), Vector2::new(W, H - 30.0), point, fade(DIM, 0.5));
        match flash {
            Some((message, alpha)) => text(&mut d, &message, 14.0, H - 21.0, 12.0, fade(GREEN, alpha)),
            None => text(&mut d, hint, 14.0, H - 20.0, 11.0, DIM),
        }
        // lit while a click waits to see if a second press follows
        if button.released_at.is_some() || knob.released_at.is_some() {
            d.draw_rectangle_rec(Rectangle::new(W - 18.0, H - 18.0, 5.0, 5.0), GREEN);
        }
        if let Some(progress) = button.long_progress(now).or(knob.long_progress(now)) {
            d.draw_rectangle_rec(Rectangle::new(0.0, H - 3.0, W * progress.min(1.0), 3.0), GREEN);
        }

        // CRT scanlines over the whole panel, one point thick every three
        let mut y = 0.0;
        while y < H {
            d.draw_line_ex(Vector2::new(0.0, y), Vector2::new(W, y), point, Color::new(0, 0, 0, 60));
            y += 3.0 * point;
        }
    }
    lights.off();
}
