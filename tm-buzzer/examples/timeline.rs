// A POC of a timeline screen for tm-buzzer, on fake data: today's entries as blocks on a
// horizontal strip that scrolls under a fixed playhead. Driven by the timebuzzer knob, or by the
// keyboard and the mouse wheel standing in for it:
//   Left / Right     rotate one detent: 5 minutes, stopping at each edge of an entry on the way,
//                    so a turn through an entry goes start, end
//   Enter or Space   the knob's button: click on an edge offers to edit it or to put a new task in
//                    the free time beside it; click in a gap or on now opens the project and task
//                    drums, as tm-buzzer's, and a task picked there fills the free time or starts
//                    running; 2x goes to now; while editing a turn moves the edge, click keeps it
//                    and hold puts it back
//   Esc              quits
// The knob is read only if it is plugged in at launch; its LEDs and hand sensor are not used.
// cargo run -p tm-buzzer --example timeline
// cargo run -p tm-buzzer --example timeline -- empty    starts the day with no entries
use std::sync::mpsc;
use std::time::Instant;

use chrono::Local;
use raylib::prelude::*;

const W: f32 = 540.0;
const H: f32 = 300.0;
const SCALE: f32 = 2.0;
const DOUBLE: f64 = 0.18;
const LONG: f64 = 0.6;
const LONG_SHOW: f64 = 0.15;

// one detent, in minutes
const STEP: i32 = 5;
// points per minute, so four and a half hours fill the width
const PX: f32 = 2.0;
// the shortest entry an edit leaves
const MIN_LEN: i32 = STEP;
// how far past now the playhead goes
const AHEAD: i32 = 4 * 60;
// the fake clock starts here, in minutes since midnight, and runs in real time
const NOW_AT: i32 = 14 * 60 + 37;
// the fake projects and their tasks a new entry picks from
const PROJECTS: [(&str, &[&str]); 4] = [
    ("ABC", &["ABC-1", "ABC-2", "ABC-3"]),
    ("FAKE", &["FAKE-3", "FAKE-4"]),
    ("ops", &["mail", "standup", "review"]),
    ("tm", &["timeline poc"]),
];
// the drums of the pick, as tm-buzzer's but shorter
const ROW: f32 = 32.0;
const BAND: f32 = 30.0;

const RULE_Y: f32 = 36.0;
const STRIP: f32 = 30.0;
const STRIP_RULE_Y: f32 = RULE_Y + STRIP;
// the bottom of the ruler's ticks
const RULER_Y: f32 = STRIP_RULE_Y + 34.0;
const LANE_Y: f32 = RULER_Y + 8.0;
const LANE_H: f32 = 48.0;
const INFO_Y: f32 = LANE_Y + LANE_H + 22.0;
// the whole day, under the info
const MAP_Y: f32 = H - 62.0;
const MAP_H: f32 = 10.0;

const PANEL: Color = Color::new(10, 12, 10, 255);
const GREEN: Color = Color::new(51, 255, 102, 255);
const DIM: Color = Color::new(51, 255, 102, 120);
const GLOW: Color = Color::new(51, 255, 102, 50);
const DANGER: Color = Color::new(255, 85, 85, 255);
// the running entry: GREEN, DIM and GLOW in yellow
const YELLOW: Color = Color::new(255, 210, 60, 255);
const YELLOW_DIM: Color = Color::new(255, 210, 60, 120);
const YELLOW_GLOW: Color = Color::new(255, 210, 60, 50);
const DASH: f32 = 5.0;
const DASH_GAP: f32 = 4.0;

enum Gesture {
    Cw,
    Ccw,
    Click,
    Double,
    Long,
}

// the Button of src/main.rs: presses and releases of one key as click, double click and long press
#[derive(Default)]
struct Button {
    down_at: Option<f64>,
    long_fired: bool,
    released_at: Option<f64>,
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

    fn long_progress(&self, now: f64) -> Option<f32> {
        let t = self.down_at?;
        if self.long_fired || now - t < LONG_SHOW {
            return None;
        }
        Some(((now - t) / LONG) as f32)
    }
}

// times are minutes since today's midnight
#[derive(Clone)]
struct Entry {
    project: &'static str,
    task: &'static str,
    start: i32,
    // None while it runs; it then ends now
    end: Option<i32>,
}

// a place on the timeline the playhead stops at and an edit moves
#[derive(Clone, Copy, PartialEq)]
enum Edge {
    Start(usize),
    End(usize),
    // the end of one entry where the next one starts; an edit moves both
    Seam(usize, usize),
    // the end of the running entry, which is now and moves by itself
    Now(usize),
}

struct Edit {
    edge: Edge,
    // where the edge was, and the entries before the edit, for hold to put back
    from: i32,
    undo: Vec<Entry>,
}

// the side of the playhead a new entry fills
#[derive(Clone, Copy, PartialEq)]
enum Side {
    Before,
    After,
}

enum Mode {
    // home while an entry runs: the playhead stays on now and a turn picks 0, close, 1, stop, or
    // 2, switch
    Tracking { sel: usize },
    // home while nothing runs: the playhead moves over the timeline and edits it
    Browse,
    // on an edge with free time beside it: 0 edits the edge, 1 puts a new task in the free time
    Choose { edge: Edge, side: Side, sel: usize },
    // the drums for a new entry at the minute at: the projects while project is None, else that
    // project's tasks; row 0 goes back, as tm-buzzer's back row
    Pick { at: i32, side: Side, project: Option<usize>, turn: usize, scroll: f32 },
    Edit(Edit),
}

struct App {
    // in time order, never overlapping
    entries: Vec<Entry>,
    clock: Instant,
    // the minute under the playhead
    at: i32,
    // the minute in the middle of the view, easing toward at
    view: f32,
    // the playhead stays on now as now moves
    follow: bool,
    mode: Mode,
    flash: Option<(String, Instant)>,
    // set by the close choice; the main loop ends
    close: bool,
}

fn fake_day() -> Vec<Entry> {
    let m = |h: i32, min: i32| h * 60 + min;
    vec![
        Entry { project: "ops", task: "mail", start: m(8, 30), end: Some(m(9, 0)) },
        Entry { project: "ABC", task: "ABC-1", start: m(9, 0), end: Some(m(10, 45)) },
        Entry { project: "ops", task: "standup", start: m(11, 0), end: Some(m(11, 15)) },
        Entry { project: "ABC", task: "ABC-2", start: m(11, 15), end: Some(m(12, 40)) },
        Entry { project: "tm", task: "timeline poc", start: m(13, 20), end: Some(m(14, 5)) },
        Entry { project: "FAKE", task: "FAKE-3", start: m(14, 5), end: None },
    ]
}

impl App {
    fn now(&self) -> i32 {
        NOW_AT + (self.clock.elapsed().as_secs() / 60) as i32
    }

    // the now the drawing uses, with the seconds
    fn now_f(&self) -> f32 {
        NOW_AT as f32 + self.clock.elapsed().as_secs_f32() / 60.0
    }

    fn end(&self, i: usize) -> i32 {
        self.entries[i].end.unwrap_or_else(|| self.now())
    }

    fn say(&mut self, message: impl Into<String>) {
        self.flash = Some((message.into(), Instant::now()));
    }

    // every edge with its minute, in time order; two entries that touch share one seam
    fn edges(&self) -> Vec<(i32, Edge)> {
        let mut out = Vec::new();
        for (i, e) in self.entries.iter().enumerate() {
            let touches_prev = i > 0 && self.entries[i - 1].end == Some(e.start);
            out.push((e.start, if touches_prev { Edge::Seam(i - 1, i) } else { Edge::Start(i) }));
            match e.end {
                None => out.push((self.now(), Edge::Now(i))),
                Some(end) if self.entries.get(i + 1).is_some_and(|n| n.start == end) => {}
                Some(end) => out.push((end, Edge::End(i))),
            }
        }
        out
    }

    fn edge_at(&self, m: i32) -> Option<Edge> {
        self.edges().into_iter().find(|&(at, _)| at == m).map(|(_, e)| e)
    }

    fn inside(&self, m: i32) -> bool {
        (0..self.entries.len()).any(|i| self.entries[i].start < m && m < self.end(i))
    }

    // the entries an edge belongs to
    fn touched(&self, edge: Edge) -> Vec<usize> {
        match edge {
            Edge::Start(i) | Edge::End(i) | Edge::Now(i) => vec![i],
            Edge::Seam(a, b) => vec![a, b],
        }
    }

    // one detent of the playhead: the next 5-minute mark outside the entries, or the next edge,
    // whichever is closer
    fn step(&mut self, dir: i32) {
        let grid = if dir > 0 { (self.at.div_euclid(STEP) + 1) * STEP } else { (self.at - 1).div_euclid(STEP) * STEP };
        let grid = Some(grid).filter(|&g| !self.inside(g));
        // now stops the playhead too, as the place to start a task
        let edge = self
            .edges()
            .into_iter()
            .map(|(m, _)| m)
            .chain(std::iter::once(self.now()))
            .filter(|&m| (m - self.at) * dir > 0)
            .min_by_key(|&m| (m - self.at).abs());
        if let Some(m) = [grid, edge].into_iter().flatten().min_by_key(|&m| (m - self.at).abs()) {
            self.at = m.clamp(0, self.now() + AHEAD);
        }
        self.follow = self.at == self.now();
    }

    // one detent of the edge being edited, onto the 5-minute marks, kept between its neighbors
    // and before now
    fn nudge(&mut self, edge: Edge, dir: i32) {
        let m = self.at;
        let want = if dir > 0 { (m.div_euclid(STEP) + 1) * STEP } else { (m - 1).div_euclid(STEP) * STEP };
        let prev_end = |i: usize| if i == 0 { 0 } else { self.end(i - 1) };
        let next_start = |i: usize| self.entries.get(i + 1).map_or(self.now(), |n| n.start.min(self.now()));
        let (lo, hi) = match edge {
            Edge::Start(i) => (prev_end(i), self.end(i) - MIN_LEN),
            Edge::End(i) => (self.entries[i].start + MIN_LEN, next_start(i)),
            Edge::Seam(a, b) => (self.entries[a].start + MIN_LEN, self.end(b) - MIN_LEN),
            Edge::Now(_) => return,
        };
        // an entry already shorter than MIN_LEN, stopped soon after its start, has lo past hi;
        // with the edge's own minute in the range it can grow and not shrink
        let to = want.clamp(lo.min(m), hi.max(m));
        if to == m {
            self.say(if dir > 0 { "can't go later" } else { "can't go earlier" });
            return;
        }
        match edge {
            Edge::Start(i) => self.entries[i].start = to,
            Edge::End(i) => self.entries[i].end = Some(to),
            Edge::Seam(a, b) => {
                self.entries[a].end = Some(to);
                self.entries[b].start = to;
            }
            Edge::Now(_) => {}
        }
        self.at = to;
    }

    fn begin_edit(&self, edge: Edge) -> Mode {
        Mode::Edit(Edit { edge, from: self.at, undo: self.entries.clone() })
    }

    // the projects drum, on the project of the last entry before the playhead
    fn pick(&self, side: Side) -> Mode {
        let last = self.entries.iter().rev().find(|e| e.start < self.at);
        let p = last.and_then(|e| PROJECTS.iter().position(|(name, _)| *name == e.project));
        let turn = p.map_or(1, |p| p + 1);
        Mode::Pick { at: self.at, side, project: None, turn, scroll: turn as f32 }
    }

    // tracking while an entry runs, else browsing
    fn home(&self) -> Mode {
        if self.entries.iter().any(|e| e.end.is_none()) { Mode::Tracking { sel: 1 } } else { Mode::Browse }
    }

    // what a click in browse opens: an edge with free time beside it asks what to do, a seam has
    // none and edits at once; now and a gap pick a task to start there
    fn click(&mut self) -> Mode {
        let pick = |side| self.pick(side);
        match self.edge_at(self.at) {
            Some(edge @ Edge::Seam(..)) => self.begin_edit(edge),
            Some(edge @ Edge::Start(_)) => Mode::Choose { edge, side: Side::Before, sel: 0 },
            Some(edge @ Edge::End(_)) => Mode::Choose { edge, side: Side::After, sel: 0 },
            // an edge only a running entry has, and a running entry means tracking
            Some(Edge::Now(_)) => self.home(),
            None if self.at > self.now() => {
                self.say("planning comes later");
                Mode::Browse
            }
            None => pick(Side::After),
        }
    }

    // the start and end of a new entry at the minute at, None for an end that runs: it fills the
    // free time on its side of the playhead, and after the playhead it runs when that free time
    // reaches now, so on now it is a switch
    fn new_span(&self, at: i32, side: Side) -> (i32, Option<i32>) {
        match side {
            Side::After => {
                let next = self.entries.iter().map(|e| e.start).filter(|&s| s >= at).min();
                let running = self.entries.iter().any(|e| e.end.is_none());
                match next {
                    None if !running => (at, None),
                    None => (self.now(), None),
                    Some(next) => (at, Some(next)),
                }
            }
            Side::Before => {
                let prev = self.entries.iter().filter_map(|e| e.end).filter(|&e| e <= at).max().unwrap_or(0);
                (prev, Some(at))
            }
        }
    }

    // puts the task in at the minute at; a running one stops the running entry where it starts
    fn create(&mut self, at: i32, side: Side, (project, task): (&'static str, &'static str)) -> Mode {
        let (start, end) = self.new_span(at, side);
        if end == Some(start) {
            self.say("no room here");
            return Mode::Browse;
        }
        if end.is_none()
            && let Some(r) = self.entries.iter_mut().find(|e| e.end.is_none())
        {
            r.end = Some(start);
        }
        let i = self.entries.iter().position(|e| e.start > start).unwrap_or(self.entries.len());
        self.entries.insert(i, Entry { project, task, start, end });
        self.follow = self.at == self.now();
        self.say(match end {
            None => format!("started {task} at {}", hm(start)),
            Some(end) => format!("logged {task} {} - {}", hm(start), hm(end)),
        });
        self.home()
    }

    fn apply(&mut self, gesture: Gesture) {
        let mode = std::mem::replace(&mut self.mode, Mode::Browse);
        self.mode = match (mode, gesture) {
            (Mode::Edit(edit), Gesture::Cw) => {
                self.nudge(edit.edge, 1);
                Mode::Edit(edit)
            }
            (Mode::Edit(edit), Gesture::Ccw) => {
                self.nudge(edit.edge, -1);
                Mode::Edit(edit)
            }
            (Mode::Edit(edit), Gesture::Click) => {
                let delta = self.at - edit.from;
                let what = edge_name(&self.entries, edit.edge);
                self.say(if delta == 0 { format!("{what} unchanged") } else { format!("{what} {}", signed(delta)) });
                Mode::Browse
            }
            (Mode::Edit(edit), Gesture::Long) => {
                self.entries = edit.undo;
                self.at = edit.from;
                self.say("put back");
                Mode::Browse
            }
            (Mode::Choose { edge, side, .. }, Gesture::Cw) => Mode::Choose { edge, side, sel: 1 },
            (Mode::Choose { edge, side, .. }, Gesture::Ccw) => Mode::Choose { edge, side, sel: 0 },
            (Mode::Choose { edge, sel: 0, .. }, Gesture::Click) => self.begin_edit(edge),
            (Mode::Choose { side, .. }, Gesture::Click) => self.pick(side),
            (Mode::Pick { at, side, project, turn, scroll }, Gesture::Cw) => {
                let turn = (turn + 1).min(pick_rows(project) - 1);
                Mode::Pick { at, side, project, turn, scroll }
            }
            (Mode::Pick { at, side, project, turn, scroll }, Gesture::Ccw) => {
                Mode::Pick { at, side, project, turn: turn.saturating_sub(1), scroll }
            }
            // the back row and a hold: from the tasks to their project, from the projects out
            (Mode::Pick { at, side, project: Some(p), .. }, Gesture::Long)
            | (Mode::Pick { at, side, project: Some(p), turn: 0, .. }, Gesture::Click) => {
                Mode::Pick { at, side, project: None, turn: p + 1, scroll: (p + 1) as f32 }
            }
            (Mode::Pick { project: None, turn: 0, .. }, Gesture::Click) | (Mode::Pick { project: None, .. }, Gesture::Long) => {
                self.home()
            }
            (Mode::Pick { at, side, project: None, turn, .. }, Gesture::Click) => {
                Mode::Pick { at, side, project: Some(turn - 1), turn: 1, scroll: 1.0 }
            }
            (Mode::Pick { at, side, project: Some(p), turn, .. }, Gesture::Click) => {
                self.create(at, side, (PROJECTS[p].0, PROJECTS[p].1[turn - 1]))
            }
            (Mode::Choose { .. }, Gesture::Long) => Mode::Browse,
            (Mode::Tracking { sel }, Gesture::Cw) => Mode::Tracking { sel: (sel + 1).min(2) },
            (Mode::Tracking { sel }, Gesture::Ccw) => Mode::Tracking { sel: sel.saturating_sub(1) },
            // close stands in for tm-buzzer's close row, which hides the window; here it quits
            (Mode::Tracking { sel: 0 }, Gesture::Click) => {
                self.close = true;
                Mode::Tracking { sel: 0 }
            }
            // stop ends the running entry now and leaves the playhead on its end, to edit it
            (Mode::Tracking { sel: 1 }, Gesture::Click) => {
                let now = self.now();
                if let Some(r) = self.entries.iter_mut().find(|e| e.end.is_none()) {
                    r.end = Some(now);
                    let task = r.task;
                    self.say(format!("stopped {task}"));
                }
                self.at = now;
                self.follow = false;
                Mode::Browse
            }
            (Mode::Tracking { .. }, Gesture::Click) => self.pick(Side::After),
            (Mode::Browse, Gesture::Cw) => {
                self.step(1);
                Mode::Browse
            }
            (Mode::Browse, Gesture::Ccw) => {
                self.step(-1);
                Mode::Browse
            }
            (Mode::Browse, Gesture::Click) => self.click(),
            (Mode::Browse, Gesture::Double) => {
                self.at = self.now();
                self.follow = true;
                Mode::Browse
            }
            (mode, _) => mode,
        };
    }

    fn tick(&mut self, dt: f32) {
        match self.mode {
            Mode::Tracking { .. } => self.at = self.now(),
            Mode::Browse if self.follow => self.at = self.now(),
            _ => {}
        }
        if let Mode::Pick { turn, scroll, .. } = &mut self.mode {
            *scroll += (*turn as f32 - *scroll) * (1.0 - (-40.0 * dt).exp());
        }
        let ease = 1.0 - (-14.0 * dt).exp();
        self.view += (self.at as f32 - self.view) * ease;
    }
}

// the rows of a pick drum: back, then the projects or the project's tasks
fn pick_rows(project: Option<usize>) -> usize {
    1 + project.map_or(PROJECTS.len(), |p| PROJECTS[p].1.len())
}

fn edge_name(entries: &[Entry], edge: Edge) -> String {
    match edge {
        Edge::Start(i) => format!("{} start", entries[i].task),
        Edge::End(i) | Edge::Now(i) => format!("{} end", entries[i].task),
        Edge::Seam(a, b) => format!("{} | {}", entries[a].task, entries[b].task),
    }
}

fn hm(m: i32) -> String {
    format!("{:02}:{:02}", m.div_euclid(60), m.rem_euclid(60))
}

fn span(m: i32) -> String {
    format!("{}:{:02}", m / 60, m % 60)
}

fn signed(m: i32) -> String {
    let sign = if m < 0 { "-" } else { "+" };
    match m.abs() {
        a if a >= 60 => format!("{sign}{}h{:02}m", a / 60, a % 60),
        a => format!("{sign}{a}m"),
    }
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t.clamp(0.0, 1.0)
}

fn fade(color: Color, a: f32) -> Color {
    Color {
        a: (color.a as f32 * a.clamp(0.0, 1.0)) as u8,
        ..color
    }
}

type Canvas<'a, 'b> = RaylibMode2D<'a, RaylibDrawHandle<'b>>;

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
            let piece = if across { Rectangle::new(x + at, y, dash, h) } else { Rectangle::new(x, y + at, w, dash) };
            d.draw_rectangle_rec(piece, color);
            at += DASH + DASH_GAP;
        }
    }
}

fn main() {
    let (knob_tx, knob_rx) = mpsc::channel();
    let _knob = timebuzzer::listen(move |event| {
        let _ = knob_tx.send(event);
    });

    let (window_w, window_h) = ((W * SCALE) as i32, (H * SCALE) as i32);
    let (mut rl, thread) = raylib::init()
        .size(window_w, window_h)
        .title("tm timeline")
        .undecorated()
        .highdpi()
        .msaa_4x()
        .build();
    rl.set_target_fps(60);
    let font = rl
        .load_font_from_memory(&thread, ".ttf", include_bytes!("../src/font/Doto-Bold.ttf"), 96, None)
        .map(|font| {
            font.texture()
                .set_texture_filter(&thread, TextureFilter::TEXTURE_FILTER_BILINEAR);
            font.make_weak()
        })
        .unwrap_or_else(|_| rl.get_font_default());
    let user = std::env::var("USER").unwrap_or_else(|_| "root".to_string());
    let day = Local::now().format("%a %d %b").to_string().to_lowercase();

    let mut app = App {
        entries: if std::env::args().any(|a| a == "empty") { Vec::new() } else { fake_day() },
        clock: Instant::now(),
        at: NOW_AT,
        view: NOW_AT as f32,
        follow: true,
        mode: Mode::Browse,
        flash: None,
        close: false,
    };
    app.mode = app.home();
    let mut button = Button::default();
    let mut knob = Button::default();
    let mut gestures = Vec::new();

    while !rl.window_should_close() && !app.close {
        let now = rl.get_time();
        let dt = rl.get_frame_time().min(1.0 / 30.0);
        let key = |k| rl.is_key_pressed(k) || rl.is_key_pressed_repeat(k);
        if key(KeyboardKey::KEY_RIGHT) {
            gestures.push(Gesture::Cw);
        }
        if key(KeyboardKey::KEY_LEFT) {
            gestures.push(Gesture::Ccw);
        }
        let wheel = rl.get_mouse_wheel_move();
        if wheel > 0.0 {
            gestures.push(Gesture::Ccw);
        } else if wheel < 0.0 {
            gestures.push(Gesture::Cw);
        }
        let pressed = rl.is_key_pressed(KeyboardKey::KEY_ENTER) || rl.is_key_pressed(KeyboardKey::KEY_SPACE);
        let released = rl.is_key_released(KeyboardKey::KEY_ENTER) || rl.is_key_released(KeyboardKey::KEY_SPACE);
        button.update(now, pressed, released, &mut gestures);
        for event in knob_rx.try_iter() {
            match event {
                timebuzzer::Event::Right => gestures.push(Gesture::Cw),
                timebuzzer::Event::Left => gestures.push(Gesture::Ccw),
                timebuzzer::Event::ClickDown => knob.update(now, true, false, &mut gestures),
                timebuzzer::Event::ClickUp => knob.update(now, false, true, &mut gestures),
                _ => {}
            }
        }
        knob.update(now, false, false, &mut gestures);
        for g in gestures.drain(..) {
            app.apply(g);
        }
        app.tick(dt);

        let measure = |s: &str, size: f32| font.measure_text(s, size, 0.0).x;
        let text = |d: &mut Canvas, s: &str, x: f32, y: f32, size: f32, color: Color| {
            d.draw_text_ex(&font, s, Vector2::new(x, y), size, 0.0, color);
        };
        let right = |d: &mut Canvas, s: &str, x: f32, y: f32, size: f32, color: Color| {
            d.draw_text_ex(&font, s, Vector2::new(x - measure(s, size), y), size, 0.0, color);
        };
        let centered = |d: &mut Canvas, s: &str, x: f32, y: f32, size: f32, color: Color| {
            d.draw_text_ex(&font, s, Vector2::new(x - measure(s, size) / 2.0, y), size, 0.0, color);
        };
        let fit = |s: &str, size: f32, max: f32| {
            if measure(s, size) <= max {
                return s.to_string();
            }
            let mut cut = s.to_string();
            while !cut.is_empty() && measure(&format!("{cut}.."), size) > max {
                cut.pop();
            }
            if cut.is_empty() { String::new() } else { format!("{cut}..") }
        };
        // a minute's x on the panel; the playhead is the middle
        let x = |m: f32| W / 2.0 + (m - app.view) * PX;
        let now_m = app.now_f();
        let blink = if (now * 2.0) as u32 % 2 == 0 { "_" } else { "" };
        let edge = app.edge_at(app.at);
        let edit = match &app.mode {
            Mode::Edit(e) => Some(e),
            _ => None,
        };
        let editing = edit.map(|e| e.edge);
        // the edge the playhead is on, or the one the mode works on
        let focus = match app.mode {
            Mode::Choose { edge, .. } => Some(edge),
            _ => editing.or(edge),
        };
        let lit: Vec<usize> = focus.map_or(Vec::new(), |e| app.touched(e));
        let flash = app.flash.as_ref().and_then(|(message, at)| {
            let age = at.elapsed().as_secs_f32();
            (age < 1.4).then(|| (message.clone(), (1.0 - (age - 0.9).max(0.0) / 0.5).clamp(0.0, 1.0)))
        });

        let zoom = rl.get_render_width() as f32 / W;
        let point = W / window_w as f32;
        let mut window = rl.begin_drawing(&thread);
        window.clear_background(PANEL);
        let mut d = window.begin_mode2D(Camera2D {
            offset: Vector2::zero(),
            target: Vector2::zero(),
            rotation: 0.0,
            zoom,
        });
        d.draw_rectangle_lines_ex(Rectangle::new(0.0, 0.0, W, H), point, DIM);

        // header
        let prompt = format!("{user}@tm:~$ ");
        text(&mut d, &prompt, 14.0, 12.0, 12.0, DIM);
        let command = match &app.mode {
            Mode::Edit(e) => format!("edit {}", edge_name(&app.entries, e.edge)),
            Mode::Choose { edge, .. } => edge_name(&app.entries, *edge),
            Mode::Pick { project: None, .. } => "pick a project".to_string(),
            Mode::Pick { project: Some(p), .. } => format!("{}/tasks", PROJECTS[*p].0),
            Mode::Tracking { .. } => "tracking".to_string(),
            Mode::Browse => "timeline".to_string(),
        };
        text(&mut d, &format!("{command}{blink}"), 14.0 + measure(&prompt, 12.0), 12.0, 12.0, GREEN);
        right(&mut d, &day, W - 14.0, 12.0, 12.0, DIM);
        d.draw_line_ex(Vector2::new(0.0, RULE_Y), Vector2::new(W, RULE_Y), point, fade(DIM, 0.5));

        // the strip: the running entry
        let strip_y = RULE_Y + STRIP / 2.0;
        if let Some(r) = app.entries.iter().find(|e| e.end.is_none()) {
            let project = format!("{} / ", r.project);
            text(&mut d, &project, 14.0, strip_y - 7.0, 14.0, YELLOW_DIM);
            text(&mut d, r.task, 14.0 + measure(&project, 14.0), strip_y - 7.0, 14.0, YELLOW);
            let secs = ((now_m - r.start as f32) * 60.0) as i32;
            let elapsed = format!("{}:{:02}:{:02}", secs / 3600, secs % 3600 / 60, secs % 60);
            right(&mut d, &elapsed, W - 14.0, strip_y - 8.0, 16.0, YELLOW);
        } else {
            text(&mut d, "nothing running", 14.0, strip_y - 7.0, 14.0, fade(DIM, 0.6));
        }
        d.draw_line_ex(Vector2::new(0.0, STRIP_RULE_Y), Vector2::new(W, STRIP_RULE_Y), point, fade(DIM, 0.5));

        // the future, past now, behind the ruler and the lane
        let now_x = x(now_m);
        if now_x < W {
            let from = now_x.max(0.0);
            let future = Rectangle::new(from, RULER_Y - 12.0, W - from, LANE_Y + LANE_H + 6.0 - (RULER_Y - 12.0));
            d.draw_rectangle_rec(future, fade(GLOW, 0.25));
        }

        // the ruler: a tick every 5 minutes, longer each quarter and hour, hours named
        let first = ((app.view - W / 2.0 / PX) as i32).div_euclid(STEP) * STEP;
        let last = (app.view + W / 2.0 / PX) as i32 + STEP;
        let mut m = first;
        while m <= last {
            let tx = x(m as f32);
            let (len, color) = match m.rem_euclid(60) {
                0 => (10.0, DIM),
                15 | 30 | 45 => (6.0, fade(DIM, 0.7)),
                _ => (3.0, fade(DIM, 0.4)),
            };
            let color = if (m as f32) > now_m { fade(color, 0.5) } else { color };
            d.draw_line_ex(Vector2::new(tx, RULER_Y - len), Vector2::new(tx, RULER_Y), point, color);
            if m.rem_euclid(60) == 0 && (0..=24 * 60).contains(&m) {
                centered(&mut d, &hm(m), tx, RULER_Y - 24.0, 10.0, color);
            }
            m += STEP;
        }

        // the lane: a thread through the gaps, and the entries as blocks over it
        let lane_mid = LANE_Y + LANE_H / 2.0;
        let mut gx = 0.0;
        while gx < W {
            d.draw_line_ex(Vector2::new(gx, lane_mid), Vector2::new(gx + 2.0, lane_mid), point, fade(DIM, 0.4));
            gx += 6.0;
        }
        for (i, e) in app.entries.iter().enumerate() {
            let (x0, x1) = (x(e.start as f32), x(e.end.map_or(now_m, |m| m as f32)));
            if x1 < 0.0 || x0 > W {
                continue;
            }
            let block = Rectangle::new(x0, LANE_Y, x1 - x0, LANE_H);
            let on = lit.contains(&i);
            let (hi, dim, glow) = if e.end.is_none() { (YELLOW, YELLOW_DIM, YELLOW_GLOW) } else { (GREEN, DIM, GLOW) };
            d.draw_rectangle_rec(block, if on { glow } else { fade(glow, 0.6) });
            match (on, editing.is_some()) {
                (true, true) => dashed(&mut d, block, point, hi),
                (true, false) => d.draw_rectangle_lines_ex(block, point, hi),
                (false, _) => d.draw_rectangle_lines_ex(block, point, fade(dim, 0.7)),
            }
            // the labels stay in view while the block is partly off the left side
            let lx = x0.max(4.0) + 6.0;
            let room = x1.min(W - 4.0) - lx - 4.0;
            if room > 12.0 {
                let task_color = if on { hi } else { fade(hi, 0.75) };
                text(&mut d, &fit(e.project, 10.0, room), lx, LANE_Y + 7.0, 10.0, dim);
                text(&mut d, &fit(e.task, 14.0, room), lx, LANE_Y + 20.0, 14.0, task_color);
                let len = span(e.end.unwrap_or(now_m as i32) - e.start);
                if measure(&len, 10.0) + 8.0 < room - measure(e.task, 14.0) {
                    right(&mut d, &len, x1.min(W - 4.0) - 6.0, LANE_Y + LANE_H - 14.0, 10.0, fade(dim, 0.8));
                }
            }
        }
        // now, a solid line through the lane
        if (0.0..W).contains(&now_x) {
            d.draw_line_ex(Vector2::new(now_x, RULER_Y - 12.0), Vector2::new(now_x, LANE_Y + LANE_H + 6.0), point * 1.5, GREEN);
            text(&mut d, "now", now_x + 3.0, LANE_Y + LANE_H + 2.0, 9.0, GREEN);
        }

        // the playhead, with the minute under it; on an edge, a bracket opening toward its entry
        let px = W / 2.0;
        let (top, bottom) = (RULER_Y - 4.0, LANE_Y + LANE_H + 4.0);
        let head = if editing.is_some() { GREEN } else { fade(GREEN, 0.7) };
        d.draw_line_ex(Vector2::new(px, top), Vector2::new(px, bottom), point, head);
        if let Some(e) = edit {
            let ghost = x(e.from as f32);
            let mut y = LANE_Y - 4.0;
            while y < bottom {
                d.draw_line_ex(Vector2::new(ghost, y), Vector2::new(ghost, y + 3.0), point, DIM);
                y += 6.0;
            }
        }
        if let Some(e) = focus {
            let pulse = if editing.is_some() { 0.6 + 0.4 * ((now * 6.0).sin() as f32 + 1.0) / 2.0 } else { 1.0 };
            let color = fade(GREEN, pulse);
            d.draw_rectangle_rec(Rectangle::new(px - 1.0, LANE_Y - 4.0, 2.0, LANE_H + 8.0), color);
            let serif = |d: &mut Canvas, dir: f32| {
                for y in [LANE_Y - 4.0, LANE_Y + LANE_H + 2.0] {
                    d.draw_rectangle_rec(Rectangle::new(if dir > 0.0 { px } else { px - 6.0 }, y, 6.0, 2.0), color);
                }
            };
            match e {
                Edge::Start(_) => serif(&mut d, 1.0),
                Edge::End(_) | Edge::Now(_) => serif(&mut d, -1.0),
                Edge::Seam(_, _) => {
                    serif(&mut d, 1.0);
                    serif(&mut d, -1.0);
                }
            }
        }
        let label = hm(app.at);
        let bw = measure(&label, 12.0) + 10.0;
        let tag = Rectangle::new(px - bw / 2.0, RULER_Y - 30.0, bw, 18.0);
        d.draw_rectangle_rec(tag, PANEL);
        d.draw_rectangle_lines_ex(tag, point, head);
        centered(&mut d, &label, px, RULER_Y - 27.0, 12.0, GREEN);

        // the info: what the playhead is on
        d.draw_line_ex(Vector2::new(0.0, INFO_Y - 10.0), Vector2::new(W, INFO_Y - 10.0), point, fade(DIM, 0.3));
        let times = |i: usize| {
            let e = &app.entries[i];
            let end = e.end.map_or("now".to_string(), hm);
            format!("{} - {end} . {}", hm(e.start), span(app.end(i) - e.start))
        };
        let (what, detail) = match focus {
            Some(Edge::Seam(a, b)) => (
                format!("{} | {}", app.entries[a].task, app.entries[b].task),
                format!("{}  |  {}", span(app.end(a) - app.entries[a].start), span(app.end(b) - app.entries[b].start)),
            ),
            Some(Edge::Start(i)) => (format!("[ {}", app.entries[i].task), times(i)),
            Some(Edge::End(i)) => (format!("{} ]", app.entries[i].task), times(i)),
            Some(Edge::Now(i)) => (format!("{} > running", app.entries[i].task), times(i)),
            None if app.at > app.now() => ("later".to_string(), format!("{} from now", signed(app.at - app.now()))),
            None if app.at == app.now() => ("now".to_string(), "nothing running".to_string()),
            None => {
                let before = app.entries.iter().enumerate().filter(|(_, e)| e.start < app.at).map(|(i, _)| app.end(i)).max();
                let after = app.entries.iter().filter(|e| e.start > app.at).map(|e| e.start).min();
                let from = before.unwrap_or(0);
                let to = after.unwrap_or(app.now());
                ("gap".to_string(), format!("{} - {} . {} untracked", hm(from), hm(to), span(to - from)))
            }
        };
        // the choices as boxes side by side, the selected one outlined as an action; the running
        // entry's in its yellow
        let choice: Option<(&[&str], usize, (Color, Color, Color))> = match app.mode {
            Mode::Choose { side: Side::Before, sel, .. } => Some((&["edit edge", "+ task before"], sel, (GREEN, DIM, GLOW))),
            Mode::Choose { sel, .. } => Some((&["edit edge", "+ task after"], sel, (GREEN, DIM, GLOW))),
            Mode::Tracking { sel } => Some((&["close", "[] stop", "switch"], sel, (YELLOW, YELLOW_DIM, YELLOW_GLOW))),
            _ => None,
        };
        match choice {
            Some((labels, sel, (hi, dim, glow))) => {
                let width = (W - 28.0) / labels.len() as f32;
                for (k, label) in labels.iter().enumerate() {
                    let cell = Rectangle::new(14.0 + k as f32 * width, INFO_Y - 6.0, width - 6.0, 26.0);
                    if k == sel {
                        d.draw_rectangle_rec(cell, glow);
                        dashed(&mut d, cell, point, dim);
                    }
                    let color = if k == sel { hi } else { dim };
                    text(&mut d, &format!("{}{label}", if k == sel { "> " } else { "  " }), cell.x + 8.0, INFO_Y, 14.0, color);
                }
            }
            None => {
                text(&mut d, &what, 14.0, INFO_Y, 16.0, GREEN);
                right(&mut d, &detail, W - 14.0, INFO_Y + 2.0, 12.0, DIM);
            }
        }
        if let Some(e) = edit {
            let delta = app.at - e.from;
            let color = if delta == 0 { DIM } else { GREEN };
            text(&mut d, &format!("moved {}", signed(delta)), 14.0, INFO_Y + 22.0, 11.0, color);
        }

        // above the day map: the working day from the first start to the last end, and the time
        // the entries cover in it
        if let (Some(first), Some(last)) = (
            app.entries.iter().map(|e| e.start).min(),
            (0..app.entries.len()).map(|i| app.end(i)).max(),
        ) {
            let tracked: i32 = (0..app.entries.len()).map(|i| app.end(i) - app.entries[i].start).sum();
            let until = if app.entries.iter().any(|e| e.end.is_none()) { "now".to_string() } else { hm(last) };
            let day = format!("day {} - {until} . {}", hm(first), span(last - first));
            text(&mut d, &day, 14.0, MAP_Y - 18.0, 11.0, DIM);
            let gaps = last - first - tracked;
            let sum = format!("tracked {} . gaps {}", span(tracked), span(gaps));
            right(&mut d, &sum, W - 14.0, MAP_Y - 18.0, 11.0, GREEN);
        }

        // the whole day: entries, the view's span, the playhead
        let (mx, mw) = (14.0, W - 28.0);
        let mapx = |m: f32| mx + mw * m / (24.0 * 60.0);
        d.draw_rectangle_lines_ex(Rectangle::new(mx, MAP_Y, mw, MAP_H), point, fade(DIM, 0.4));
        for (i, e) in app.entries.iter().enumerate() {
            let (a, b) = (mapx(e.start as f32), mapx(app.end(i) as f32));
            let color = match (e.end.is_none(), lit.contains(&i)) {
                (true, true) => YELLOW,
                (true, false) => YELLOW_DIM,
                (false, true) => GREEN,
                (false, false) => DIM,
            };
            d.draw_rectangle_rec(Rectangle::new(a, MAP_Y + 2.0, (b - a).max(point), MAP_H - 4.0), color);
        }
        let (va, vb) = (mapx(app.view - W / 2.0 / PX), mapx(app.view + W / 2.0 / PX));
        d.draw_rectangle_lines_ex(Rectangle::new(va, MAP_Y - 3.0, vb - va, MAP_H + 6.0), point, GREEN);
        for h in [0, 6, 12, 18, 24] {
            centered(&mut d, &h.to_string(), mapx((h * 60) as f32), MAP_Y + MAP_H + 4.0, 9.0, fade(DIM, 0.7));
        }

        // the pick: tm-buzzer's drums over everything between the strip and the footer, with
        // what the task will do on the band's right side
        if let Mode::Pick { at, side, project, turn, scroll } = app.mode {
            let area = Rectangle::new(0.0, STRIP_RULE_Y + point, W, H - 30.0 - STRIP_RULE_Y - point);
            d.draw_rectangle_rec(area, PANEL);
            let band_y = area.y + area.height / 2.0;
            let running = app.entries.iter().find(|e| e.end.is_none());
            let (x0, x1) = match project {
                None => (8.0, 200.0),
                Some(_) => (92.0, W - 8.0),
            };
            match project {
                // the selected project's tasks, ghosted
                None if turn > 0 => {
                    for (k, task) in PROJECTS[turn - 1].1.iter().take(5).enumerate() {
                        text(&mut d, task, 220.0, band_y - 64.0 + k as f32 * 26.0, 14.0, fade(DIM, 0.6));
                    }
                }
                None => text(&mut d, "timeline", 220.0, band_y - 7.0, 14.0, fade(DIM, 0.6)),
                Some(p) => {
                    let tab = Rectangle::new(8.0, band_y - 18.0, 76.0, 36.0);
                    d.draw_rectangle_rec(tab, fade(GLOW, 0.6));
                    d.draw_rectangle_lines_ex(tab, point, fade(DIM, 0.6));
                    text(&mut d, PROJECTS[p].0, 16.0, band_y - 7.0, 13.0, DIM);
                }
            }
            let band = Rectangle::new(x0, band_y - BAND / 2.0, x1 - x0, BAND);
            d.draw_rectangle_rec(band, GLOW);
            if turn == 0 {
                dashed(&mut d, band, point, DIM);
            } else {
                d.draw_rectangle_lines_ex(band, point, DIM);
            }
            text(&mut d, ">", x0 + 6.0, band_y - 8.0, 16.0, GREEN);
            let (start, end) = app.new_span(at, side);
            for i in 0..pick_rows(project) {
                let dist = i as f32 - scroll;
                let far = dist.abs();
                if far > 2.6 {
                    continue;
                }
                let offset = far.min(1.0) * (BAND / 2.0 + ROW / 2.0) + (far - 1.0).max(0.0) * ROW;
                let y = band_y + dist.signum() * offset;
                let size = lerp(18.0, 13.0, (far / 2.5).min(1.0));
                let alpha = if far <= 1.0 { lerp(1.0, 0.65, far) } else { lerp(0.65, 0.0, (far - 1.0) / 1.6) };
                let settled = (1.0 - far * 2.0).max(0.0);
                let live = |p: &str, t: Option<&str>| {
                    running.is_some_and(|r| r.project == p && t.is_none_or(|t| r.task == t))
                };
                let (label, back) = match (project, i) {
                    (None, 0) => ("< timeline".to_string(), true),
                    (Some(_), 0) => ("< project".to_string(), true),
                    (None, i) => {
                        let name = PROJECTS[i - 1].0;
                        (format!("{name}{}", if live(name, None) { " *" } else { "" }), false)
                    }
                    (Some(p), i) => {
                        let task = PROJECTS[p].1[i - 1];
                        (format!("{task}{}", if live(PROJECTS[p].0, Some(task)) { " *" } else { "" }), false)
                    }
                };
                let (size, color) = match (back, i == turn) {
                    (true, true) => (size * 0.85, GREEN),
                    (true, false) => (size * 0.85, DIM),
                    (false, _) => (size, GREEN),
                };
                text(&mut d, &label, x0 + 22.0, y - size / 2.0, size, fade(color, alpha));
                // a task row says what picking it does: start it running, switch to it, or log it
                if project.is_some() && i == turn && i > 0 {
                    let (verb, when) = match end {
                        None if running.is_some() => ("switch".to_string(), format!("at {}", hm(start))),
                        None => ("start".to_string(), format!("from {}", hm(start))),
                        Some(end) => ("log".to_string(), format!("{} - {} . {}", hm(start), hm(end), span(end - start))),
                    };
                    right(&mut d, &verb, x1 - 12.0, y - 14.0, 12.0, fade(GREEN, settled));
                    right(&mut d, &when, x1 - 12.0, y + 2.0, 11.0, fade(DIM, settled));
                }
            }
        }

        // footer
        d.draw_line_ex(Vector2::new(0.0, H - 30.0), Vector2::new(W, H - 30.0), point, fade(DIM, 0.5));
        let hint = match (&app.mode, edge) {
            (Mode::Edit(_), _) => "turn: move edge . click: keep . hold: put back",
            (Mode::Choose { .. }, _) => "turn: choose . click: go . hold: back",
            (Mode::Pick { project: None, .. }, _) => "turn: move . click: open . hold: back",
            (Mode::Pick { .. }, _) => "turn: move . click: put it here . hold: back",
            (Mode::Tracking { .. }, _) => "turn: choose . click: go",
            (Mode::Browse, None) if app.at == app.now() => "turn: 5m . click: start task . 2x: now",
            (Mode::Browse, Some(Edge::Seam(..))) => "turn: 5m . click: edit seam . 2x: now",
            (Mode::Browse, Some(_)) => "turn: 5m . click: edit or add . 2x: now",
            (Mode::Browse, None) => "turn: 5m . click: start task here . 2x: now",
        };
        match flash {
            Some((message, alpha)) => {
                let color = if message.starts_with("can't") { DANGER } else { GREEN };
                text(&mut d, &message, 14.0, H - 21.0, 12.0, fade(color, alpha));
            }
            None => text(&mut d, hint, 14.0, H - 20.0, 11.0, DIM),
        }
        if button.released_at.is_some() || knob.released_at.is_some() {
            d.draw_rectangle_rec(Rectangle::new(W - 18.0, H - 18.0, 5.0, 5.0), GREEN);
        }
        if let Some(progress) = button.long_progress(now).or(knob.long_progress(now)) {
            d.draw_rectangle_rec(Rectangle::new(0.0, H - 3.0, W * progress.min(1.0), 3.0), GREEN);
        }

        let mut y = 0.0;
        while y < H {
            d.draw_line_ex(Vector2::new(0.0, y), Vector2::new(W, y), point, Color::new(0, 0, 0, 60));
            y += 3.0 * point;
        }
    }
}
