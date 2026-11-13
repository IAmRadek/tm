// A timeline of tm's entries in whoami's terminal style, driven by the timebuzzer knob: today's
// entries are blocks on a horizontal strip that scrolls under a fixed playhead. While an entry
// runs, home holds the playhead on now and offers close, stop and switch. While nothing runs,
// home is the timeline: a turn moves the playhead 5 minutes, stopping at each edge of an entry on
// the way, and a click on an edge edits it or puts a new task in the free time beside it. New
// tasks are picked on drums of projects and tasks. The keyboard stands in for the knob:
//   Left / Right     rotate one detent
//   Enter or Space   the knob's button: click, double click, long press
//   Esc              hide the window, as close does; the app keeps running
// The timeline shows tm's database, read again each time the window shows and after each write;
// writes run the tm CLI, which writes it and tells tm-daemon. tm lets entries overlap; the
// timeline stacks overlapping entries and does not edit them.
use std::process::Command;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use raylib::prelude::*;

mod config;
mod jira;
mod macos;
mod store;

use chrono::{DateTime, Local, TimeZone, Timelike, Utc};
use store::Project;

// the tm CLI: the path TM_BIN held at build time, as the Nix package sets it, else tm on PATH
const TM: &str = match option_env!("TM_BIN") {
    Some(path) => path,
    None => "tm",
};

// the layout is drawn on a W × H canvas and zoomed by SCALE into the window
const W: f32 = 540.0;
const H: f32 = 350.0;
const SCALE: f32 = 2.0;
// a second press inside this window makes a double click, so a click fires only after it
const DOUBLE: f64 = 0.18;
const LONG: f64 = 0.6;
// a press shorter than this shows no long-press progress
const LONG_SHOW: f64 = 0.15;
// the window hides this long after the hand goes away from the knob, or after the last input
// while the hand is away; typing counts as input
const CLOSE_AWAY: Duration = Duration::from_secs(2);
// an Away counts only when no Near follows it within this long
const AWAY_SETTLE: Duration = Duration::from_secs(1);
// the header rule counts down a timeout in this many dashes
const IDLE_DASHES: usize = 12;

const ROW: f32 = 38.0;
const BAND: f32 = 32.0;
const RULE_Y: f32 = 36.0;
// the strip under the header that shows the running entry, and the rule below it
const STRIP: f32 = 30.0;
const STRIP_RULE_Y: f32 = RULE_Y + STRIP;
const BAND_Y: f32 = 166.0 + STRIP;
// the text size of an action row (back, + new ...) against a project or task row
const ACTION_SIZE: f32 = 0.85;

// one detent of the playhead, in seconds
const STEP: i64 = 300;
// points per minute on the timeline
const PX: f32 = 2.0;
// the shortest entry an edit leaves, in seconds
const MIN_LEN: i64 = STEP;
// how far past now the playhead goes, in seconds
const AHEAD: i64 = 4 * 3600;
// how far before now the playhead goes, in seconds
const HISTORY: i64 = 365 * 86400;
// a detent this soon after one the same way makes a fast spin, whose marks are FAST_STEP apart
const FAST: Duration = Duration::from_millis(80);
const FAST_STEP: i64 = 3600;
// the bottom of the ruler's ticks
const RULER_Y: f32 = STRIP_RULE_Y + 34.0;
const LANE_Y: f32 = RULER_Y + 8.0;
const LANE_H: f32 = 48.0;
const INFO_Y: f32 = LANE_Y + LANE_H + 22.0;
// the whole day, under the info
const MAP_Y: f32 = H - 62.0;
const MAP_H: f32 = 10.0;

// whoami's terminal palette: phosphor green on near-black
const PANEL: Color = Color::new(10, 12, 10, 255);
const GREEN: Color = Color::new(51, 255, 102, 255);
const DIM: Color = Color::new(51, 255, 102, 120);
const GLOW: Color = Color::new(51, 255, 102, 50);
const DANGER: Color = Color::new(255, 85, 85, 255);
// the running entry: GREEN, DIM and GLOW in yellow
const YELLOW: Color = Color::new(255, 210, 60, 255);
const YELLOW_DIM: Color = Color::new(255, 210, 60, 120);
const YELLOW_GLOW: Color = Color::new(255, 210, 60, 50);
// the dashes of the band's outline around a selected action
const DASH: f32 = 5.0;
const DASH_GAP: f32 = 4.0;

// the knob's three LED zones, each channel 0 to 127: the phosphor green of the timeline, and the
// yellow of the running entry
const LED_GREEN: timebuzzer::Color = timebuzzer::Color { r: 25, g: 127, b: 51 };
const LED_YELLOW: timebuzzer::Color = timebuzzer::Color { r: 127, g: 80, b: 0 };
const LED_RESEND: Duration = Duration::from_millis(150);
// while an entry runs, the center breathes between this fraction of LED_YELLOW and full, once
// per LED_BREATH, and the sides hold LED_SIDE of it
const LED_BREATH: Duration = Duration::from_secs(3);
const LED_BREATH_LOW: f32 = 0.4;
const LED_SIDE: f32 = 0.12;
// on the free timeline, the side zone a turn goes toward lights and fades out over LED_BLIP
const LED_BLIP: Duration = Duration::from_millis(200);
// while an edge is edited, every zone pulses between this fraction of LED_GREEN and full, once
// per LED_EDIT_PULSE from the last input
const LED_EDIT_PULSE: Duration = Duration::from_secs(1);
const LED_EDIT_LOW: f32 = 0.15;
// while a removal waits for its confirmation, every zone pulses red the same way
const LED_REMOVE_LOW: f32 = 0.1;
// the hidden tracking wave: one pass over the zones per LED_WAVE, each zone between these
// fractions of LED_WAVE_COLOR, the running entry's yellow, written every LED_FRAME
const LED_WAVE_COLOR: timebuzzer::Color = LED_YELLOW;
const LED_WAVE: Duration = Duration::from_secs(3);
const LED_WAVE_LOW: f32 = 0.1;
const LED_WAVE_HIGH: f32 = 1.0;
const LED_FRAME: Duration = Duration::from_millis(50);
// how often the app looks for the knob being plugged in or out
const KNOB_CHECK: Duration = Duration::from_secs(2);
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
    // home while an entry runs: the playhead stays on now; turn 0 closes, 1 stops, 2 switches
    Tracking,
    // home while nothing runs: the playhead moves over the timeline
    Timeline,
    // on an edge with free time beside it: turn 0 goes back, 1 edits the edge, 2 puts a new task
    // there
    Choose(Edge, Side),
    // moving an edge, which was at the second it holds; the entries change in memory until a
    // click writes them
    Edit(Edge, i64),
    // on an entry's middle: turn 0 goes back, 1 renames it, 2 asks to remove it
    Manage(usize),
    // the removal of an entry waits for a click on its last choice; the others keep it
    Remove(usize),
    // the drums that pick the task for App::pick
    Projects,
    Tasks(usize),
    // typing a name for a new project or a new task in a project
    Input(Target),
}

#[derive(Clone, Copy, PartialEq)]
enum Target {
    Project,
    Task(usize),
}

// a place on the timeline the playhead stops at: an entry's edges, which an edit moves, and its
// middle; the indices are into App::entries
#[derive(Clone, Copy, PartialEq)]
enum Edge {
    Start(usize),
    End(usize),
    // the end of one entry where the next one starts; an edit moves both
    Seam(usize, usize),
    // the end of the running entry, which is now and moves by itself
    Now(usize),
    // halfway between a stopped entry's start and end, where the entry is renamed or removed
    Middle(usize),
}

// what the drums pick a task for
#[derive(Clone, PartialEq)]
enum Pick {
    // a new entry at a second, filling the free time on a side of it
    New(i64, Side),
    // the entry of this tm id
    Rename(String),
}

// the side of the playhead a new entry fills
#[derive(Clone, Copy, PartialEq)]
enum Side {
    Before,
    After,
}

#[derive(Clone, Copy, PartialEq)]
enum Row {
    Task(usize, usize),
    Project(usize),
    // to the timeline from the projects, to the projects from the tasks
    Back,
    NewProject,
    NewTask(usize),
    // the name being typed on the input screen
    Entry,
}

struct App {
    // reads tm's database; writes go through the tm CLI
    conn: rusqlite::Connection,
    projects: Vec<Project>,
    // the entries from `from` on, in start order; the running one has no stop
    entries: Vec<store::Entry>,
    // the local midnight the loaded entries start at, in unix seconds: the one of the day before
    // the playhead's
    from: i64,
    // when the playhead last stepped and which way, to tell a fast spin
    last_step: Option<(Instant, i64)>,
    screen: Screen,
    // the row in the band, or the choice on tracking and choose
    turn: usize,
    // where the drum is drawn, easing toward turn
    scroll: f32,
    // the projects-drum turn, restored when going back from a task drum
    project_turn: usize,
    // the playhead, in unix seconds
    at: i64,
    // the second in the middle of the view, easing toward at
    view: f64,
    // the playhead stays on now as now moves
    follow: bool,
    // what the drums pick a task for, set when they open
    pick: Pick,
    last_input: Instant,
    // when the hand went away from the knob; None while it is near or since the window opened
    away: Option<Instant>,
    flash: Option<(String, Instant)>,
    // the name typed on the input screen
    entry: String,
    // set by close; the main loop hides the window and clears it
    close: bool,
    // the Jira search thread, None without jira.toml; issues holds its latest answer
    jira: Option<jira::Watch>,
    issues: Vec<jira::Task>,
    // project and task names the drums skip
    ignore: Vec<String>,
}

impl App {
    fn rows(&self) -> Vec<Row> {
        match self.screen {
            Screen::Projects => std::iter::once(Row::Back)
                .chain(self.shown_projects().into_iter().map(Row::Project))
                .chain(std::iter::once(Row::NewProject))
                .collect(),
            Screen::Tasks(p) => std::iter::once(Row::Back)
                .chain(self.shown_tasks(p).into_iter().map(|t| Row::Task(p, t)))
                .chain(std::iter::once(Row::NewTask(p)))
                .collect(),
            Screen::Input(_) => vec![Row::Entry],
            _ => Vec::new(),
        }
    }

    // the choices or rows a turn moves over
    fn count(&self) -> usize {
        match self.screen {
            Screen::Tracking => 3,
            Screen::Choose(..) | Screen::Manage(_) => 3,
            Screen::Remove(_) => 4,
            _ => self.rows().len().max(1),
        }
    }

    fn ignored(&self, name: &str) -> bool {
        self.ignore.iter().any(|i| i == name)
    }

    // the projects the drum shows, as indices into projects
    fn shown_projects(&self) -> Vec<usize> {
        (0..self.projects.len())
            .filter(|&p| !self.ignored(&self.projects[p].name))
            .collect()
    }

    // the tasks of project p the drum shows, as indices into its tasks
    fn shown_tasks(&self, p: usize) -> Vec<usize> {
        (0..self.projects[p].tasks.len())
            .filter(|&t| !self.ignored(&self.projects[p].tasks[t]))
            .collect()
    }

    // the row of project p on the projects drum, after back; None when it is ignored
    fn project_row(&self, p: usize) -> Option<usize> {
        Some(1 + self.shown_projects().iter().position(|&q| q == p)?)
    }

    // the row of task t on project p's drum, after back; None when it is ignored
    fn task_row(&self, p: usize, t: usize) -> Option<usize> {
        Some(1 + self.shown_tasks(p).iter().position(|&u| u == t)?)
    }

    fn say(&mut self, message: impl Into<String>) {
        self.flash = Some((message.into(), Instant::now()));
    }

    fn go(&mut self, screen: Screen, turn: usize) {
        self.screen = screen;
        self.turn = turn;
        self.scroll = turn as f32;
    }

    // tracking on stop while an entry runs, else the timeline with the playhead where it is
    fn go_home(&mut self) {
        if self.running().is_some() {
            self.at = self.now();
            self.go(Screen::Tracking, 1);
        } else {
            self.go(Screen::Timeline, 0);
        }
    }

    fn now(&self) -> i64 {
        Utc::now().timestamp()
    }

    fn started(&self, i: usize) -> i64 {
        self.entries[i].start.timestamp()
    }

    // the stop of entry i, now while it runs
    fn ended(&self, i: usize) -> i64 {
        self.entries[i].stop.map_or_else(|| self.now(), |t| t.timestamp())
    }

    fn running(&self) -> Option<usize> {
        self.entries.iter().position(|e| e.stop.is_none())
    }

    fn running_for(&self) -> Option<Duration> {
        let r = &self.entries[self.running()?];
        Some((Utc::now() - r.start).to_std().unwrap_or_default())
    }

    // replaces the projects and today's entries with the database's; a project typed in but never
    // started is dropped, because only starting a task saves it
    fn reload(&mut self) {
        let snapshot = match store::load(&self.conn) {
            Ok(snapshot) => snapshot,
            Err(e) => return self.fail("read", e),
        };
        self.projects = snapshot.projects;
        self.take_issues();
        // Jira issues tm has not seen yet go after the projects' own tasks, and a project only
        // Jira knows goes after the projects; appending keeps the snapshot's indices valid
        for issue in &self.issues {
            let p = match self.projects.iter().position(|p| p.name == issue.project) {
                Some(p) => p,
                None => {
                    self.projects.push(Project {
                        name: issue.project.clone(),
                        tasks: Vec::new(),
                    });
                    self.projects.len() - 1
                }
            };
            if !self.projects[p].tasks.contains(&issue.key) {
                self.projects[p].tasks.push(issue.key.clone());
            }
        }
        self.load_entries();
    }

    // replaces the entries with the database's from the day before the playhead's to now; the
    // day before holds the entries the playhead meets first when it moves back over midnight
    fn load_entries(&mut self) {
        self.from = day_start(day_start(self.at) - 1);
        let from = DateTime::from_timestamp(self.from, 0).unwrap_or_default();
        match store::entries(&self.conn, from, Utc::now() + chrono::Duration::days(1)) {
            Ok(entries) => self.entries = entries,
            Err(e) => self.fail("read", e),
        }
    }

    // keeps the newest Jira answer that arrived since the last call; true if one did
    fn take_issues(&mut self) -> bool {
        match self.jira.as_ref().and_then(jira::Watch::latest) {
            Some(latest) => {
                self.issues = latest;
                true
            }
            None => false,
        }
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

    // starts a task now; tm start refuses while an entry runs, so a switch stops it first and
    // starts the new task at that stop time, which leaves no gap. If the start fails after the
    // stop, nothing runs
    fn start(&mut self, p: usize, t: usize) -> bool {
        let project = self.projects[p].name.clone();
        let task = self.projects[p].tasks[t].clone();
        let ok = if self.running().is_some() {
            self.tm("stop", &["stop"]) && self.tm("start", &["start", &project, &task, "--from-last-stop"])
        } else {
            self.tm("start", &["start", &project, &task])
        };
        self.reload();
        ok
    }

    // starts a task running from a second in the past; only while nothing runs
    fn start_at(&mut self, project: &str, task: &str, start: i64) -> bool {
        let ok = self.tm("start", &["start", project, task, "--at", &rfc(start)]);
        self.reload();
        ok
    }

    // logs a task from start to stop in the past, while nothing runs: tm adds an entry only by
    // starting one, so it starts the task at start and stops it at stop; a stop that fails
    // cancels the half-made entry
    fn log(&mut self, project: &str, task: &str, start: i64, stop: i64) -> bool {
        let ok = self.tm("log", &["start", project, task, "--at", &rfc(start)])
            && (self.tm("log", &["stop", "--at", &rfc(stop)]) || {
                self.tm("log", &["cancel"]);
                false
            });
        self.reload();
        ok
    }

    // writes the edge at the playhead: tm amend on each entry the edge belongs to
    fn amend(&mut self, edge: Edge) -> bool {
        let at = rfc(self.at);
        let id = |i: usize| self.entries[i].id.clone();
        let writes = match edge {
            Edge::Start(i) => vec![(id(i), "--started-at")],
            Edge::End(i) => vec![(id(i), "--stopped-at")],
            Edge::Seam(a, b) => vec![(id(a), "--stopped-at"), (id(b), "--started-at")],
            Edge::Now(_) | Edge::Middle(_) => Vec::new(),
        };
        let mut ok = true;
        for (id, flag) in writes {
            ok = ok && self.tm("amend", &["amend", &id, flag, &at]);
        }
        self.reload();
        ok
    }

    // every edge and middle with its second, in time order; two entries that touch share one
    // seam, and a running entry has no middle
    fn edges(&self) -> Vec<(i64, Edge)> {
        let mut out = Vec::new();
        for (i, e) in self.entries.iter().enumerate() {
            let touches_prev = i > 0 && self.entries[i - 1].stop == Some(e.start);
            out.push((e.start.timestamp(), if touches_prev { Edge::Seam(i - 1, i) } else { Edge::Start(i) }));
            match e.stop {
                None => out.push((self.now(), Edge::Now(i))),
                Some(stop) => {
                    out.push(((e.start.timestamp() + stop.timestamp()) / 2, Edge::Middle(i)));
                    if !self.entries.get(i + 1).is_some_and(|n| n.start == stop) {
                        out.push((stop.timestamp(), Edge::End(i)));
                    }
                }
            }
        }
        out
    }

    fn edge_at(&self, t: i64) -> Option<Edge> {
        self.edges().into_iter().find(|&(at, _)| at == t).map(|(_, e)| e)
    }

    fn inside(&self, t: i64) -> bool {
        (0..self.entries.len()).any(|i| self.started(i) < t && t < self.ended(i))
    }

    // the entries an edge belongs to
    fn touched(&self, edge: Edge) -> Vec<usize> {
        match edge {
            Edge::Start(i) | Edge::End(i) | Edge::Now(i) | Edge::Middle(i) => vec![i],
            Edge::Seam(a, b) => vec![a, b],
        }
    }

    // each entry's lane among the entries it overlaps, and how many lanes that group of entries
    // takes; an entry that overlaps none has (0, 1)
    fn lanes(&self) -> Vec<(usize, usize)> {
        let mut out = vec![(0, 1); self.entries.len()];
        let mut group: Vec<usize> = Vec::new();
        // the end of the last entry in each lane of the group
        let mut ends: Vec<i64> = Vec::new();
        let mut group_end = i64::MIN;
        for i in 0..self.entries.len() {
            let (start, end) = (self.started(i), self.ended(i));
            if start >= group_end {
                for &j in &group {
                    out[j].1 = ends.len();
                }
                group.clear();
                ends.clear();
            }
            let lane = match ends.iter().position(|&e| e <= start) {
                Some(lane) => {
                    ends[lane] = end;
                    lane
                }
                None => {
                    ends.push(end);
                    ends.len() - 1
                }
            };
            out[i].0 = lane;
            group.push(i);
            group_end = group_end.max(end);
        }
        for &j in &group {
            out[j].1 = ends.len();
        }
        out
    }

    fn edge_name(&self, edge: Edge) -> String {
        let task = |i: usize| &self.entries[i].task;
        match edge {
            Edge::Start(i) => format!("{} start", task(i)),
            Edge::End(i) | Edge::Now(i) => format!("{} end", task(i)),
            Edge::Seam(a, b) => format!("{} | {}", task(a), task(b)),
            Edge::Middle(i) => task(i).clone(),
        }
    }

    // one detent of the playhead: the next 5-minute mark outside the entries, or the next edge,
    // now or midnight, whichever is closer. In a fast spin the marks are an hour apart, so a
    // night takes a few detents; the edges, now and midnight still stop it. A playhead that
    // moves to another day loads the entries around it
    fn step(&mut self, dir: i64) {
        let fast = self.last_step.is_some_and(|(t, d)| d == dir && t.elapsed() < FAST);
        self.last_step = Some((Instant::now(), dir));
        let stride = if fast { FAST_STEP } else { STEP };
        let grid = if dir > 0 { (self.at.div_euclid(stride) + 1) * stride } else { (self.at - 1).div_euclid(stride) * stride };
        let grid = Some(grid).filter(|&g| !self.inside(g));
        let midnight = if dir > 0 { day_start(day_start(self.at) + 26 * 3600) } else { day_start(self.at - 1) };
        let edge = self
            .edges()
            .into_iter()
            .map(|(t, _)| t)
            .chain([self.now(), midnight])
            .filter(|&t| (t - self.at) * dir > 0)
            .min_by_key(|&t| (t - self.at).abs());
        if let Some(t) = [grid, edge].into_iter().flatten().min_by_key(|&t| (t - self.at).abs()) {
            self.at = t.clamp(self.now() - HISTORY, self.now() + AHEAD);
        }
        self.follow = self.at == self.now();
        if day_start(day_start(self.at) - 1) != self.from {
            self.load_entries();
        }
    }

    // one detent of the edge being edited, onto the 5-minute marks, kept between its neighbors
    // and before now; the entries change in memory only
    fn nudge(&mut self, edge: Edge, dir: i64) {
        let t = self.at;
        let now = self.now();
        let want = if dir > 0 { (t.div_euclid(STEP) + 1) * STEP } else { (t - 1).div_euclid(STEP) * STEP };
        let n = self.entries.len();
        let prev_end = |i: usize| {
            (0..n).filter(|&j| j != i).map(|j| self.ended(j)).filter(|&e| e <= self.started(i)).max().unwrap_or(self.from)
        };
        let next_start = |i: usize| {
            let next = (0..n).filter(|&j| j != i).map(|j| self.started(j)).filter(|&s| s >= self.ended(i)).min();
            next.unwrap_or(now).min(now)
        };
        let (lo, hi) = match edge {
            Edge::Start(i) => (prev_end(i), self.ended(i) - MIN_LEN),
            Edge::End(i) => (self.started(i) + MIN_LEN, next_start(i)),
            Edge::Seam(a, b) => (self.started(a) + MIN_LEN, self.ended(b) - MIN_LEN),
            Edge::Now(_) | Edge::Middle(_) => return,
        };
        // an entry already shorter than MIN_LEN has lo past hi; with the edge's own second in
        // the range it can grow and not shrink
        let to = want.clamp(lo.min(t), hi.max(t));
        if to == t {
            self.say(if dir > 0 { "can't go later" } else { "can't go earlier" });
            return;
        }
        let at = DateTime::from_timestamp(to, 0).unwrap_or_default();
        match edge {
            Edge::Start(i) => self.entries[i].start = at,
            Edge::End(i) => self.entries[i].stop = Some(at),
            Edge::Seam(a, b) => {
                self.entries[a].stop = Some(at);
                self.entries[b].start = at;
            }
            Edge::Now(_) | Edge::Middle(_) => {}
        }
        self.at = to;
    }

    // the start and end of a new entry at the second at, None for an end that runs: it fills the
    // free time on its side of the playhead, and after the playhead it runs when that free time
    // reaches now, so on now while an entry runs it is a switch. The free time stays inside the
    // playhead's day, so only today's can run
    fn new_span(&self, at: i64, side: Side) -> (i64, Option<i64>) {
        let n = self.entries.len();
        match side {
            Side::After => {
                let next = (0..n).map(|i| self.started(i)).filter(|&s| s >= at).min();
                let day_end = day_start(day_start(at) + 26 * 3600);
                match next {
                    _ if day_end <= self.now() => (at, Some(next.map_or(day_end, |next| next.min(day_end)))),
                    None if self.running().is_none() => (at, None),
                    None => (self.now(), None),
                    Some(next) => (at, Some(next)),
                }
            }
            Side::Before => {
                let prev = (0..n).map(|i| self.ended(i)).filter(|&e| e <= at).max();
                (prev.unwrap_or(0).max(day_start(at)), Some(at))
            }
        }
    }

    // what a click on the timeline opens: an edge with free time beside it asks what to do, a
    // seam has none and edits at once; a middle asks to rename or remove its entry; a gap and now
    // pick a task to start there. An edge or middle of overlapping entries opens nothing
    fn click(&mut self) {
        let lanes = self.lanes();
        match self.edge_at(self.at) {
            Some(edge) if self.touched(edge).iter().any(|&i| lanes[i].1 > 1) => {
                self.say("overlapping entries: fix them with tm")
            }
            Some(edge @ Edge::Seam(..)) => self.go(Screen::Edit(edge, self.at), 0),
            // the choices open on the first one after back
            Some(edge @ Edge::Start(_)) => self.go(Screen::Choose(edge, Side::Before), 1),
            Some(edge @ Edge::End(_)) => self.go(Screen::Choose(edge, Side::After), 1),
            Some(Edge::Middle(i)) => self.go(Screen::Manage(i), 1),
            // an edge only a running entry has, and a running entry means tracking
            Some(Edge::Now(_)) => self.go_home(),
            None if self.at > self.now() => self.say("planning comes later"),
            None => self.open_pick(Side::After),
        }
    }

    // the projects drum for a new entry at the playhead, on the project of the last entry before it
    fn open_pick(&mut self, side: Side) {
        self.pick = Pick::New(self.at, side);
        let last = self.entries.iter().rev().find(|e| e.start.timestamp() < self.at);
        let turn = last
            .and_then(|e| self.projects.iter().position(|p| p.name == e.project))
            .and_then(|p| self.project_row(p))
            .unwrap_or(1);
        self.go(Screen::Projects, turn);
    }

    // the projects drum for renaming entry i, on its own project
    fn open_rename(&mut self, i: usize) {
        let e = &self.entries[i];
        let turn = self
            .projects
            .iter()
            .position(|p| p.name == e.project)
            .and_then(|p| self.project_row(p))
            .unwrap_or(1);
        self.pick = Pick::Rename(e.id.clone());
        self.go(Screen::Projects, turn);
    }

    // the entry the drums open on: the one being renamed, else the running one
    fn current(&self) -> Option<&store::Entry> {
        match &self.pick {
            Pick::Rename(id) => self.entries.iter().find(|e| e.id == *id),
            Pick::New(..) => self.running().map(|r| &self.entries[r]),
        }
    }

    // moves the entry of tm id `id` to task t of project p
    fn rename(&mut self, id: &str, p: usize, t: usize) {
        let project = self.projects[p].name.clone();
        let task = self.projects[p].tasks[t].clone();
        if self.tm("rename", &["amend", id, "--project", &project, "--task", &task]) {
            self.say(format!("renamed to {task}"));
        }
        self.reload();
        self.go_home();
    }

    // deletes entry i with tm retract
    fn remove(&mut self, i: usize) {
        let (id, task) = (self.entries[i].id.clone(), self.entries[i].task.clone());
        if self.tm("remove", &["retract", &id]) {
            self.say(format!("removed {task}"));
        }
        self.reload();
        self.go_home();
    }

    // puts task t of project p in at the pick: a switch on now while an entry runs, an entry that
    // runs when the free time reaches now, else one that fills the free time; or renames the
    // pick's entry to it
    fn create(&mut self, p: usize, t: usize) {
        let (at, side) = match &self.pick {
            Pick::New(at, side) => (*at, *side),
            Pick::Rename(id) => return self.rename(&id.clone(), p, t),
        };
        let project = self.projects[p].name.clone();
        let task = self.projects[p].tasks[t].clone();
        let (start, end) = self.new_span(at, side);
        match end {
            Some(end) if end <= start => self.say("no room here"),
            Some(end) => {
                if self.log(&project, &task, start, end) {
                    self.say(format!("logged {task} {} - {}", hm(start), hm(end)));
                }
            }
            None if self.running().is_some() => {
                if self.start(p, t) {
                    self.say("switched");
                }
            }
            // the playhead follows now a frame behind, so a second off is now
            None if self.now() - start <= 1 => {
                if self.start(p, t) {
                    self.say("started");
                }
            }
            None => {
                if self.start_at(&project, &task, start) {
                    self.say(format!("started {task} at {}", hm(start)));
                }
            }
        }
        self.go_home();
    }

    // writes the edited edge, or nothing when it is back where it was
    fn keep(&mut self, edge: Edge, from: i64) {
        let delta = self.at - from;
        let what = self.edge_name(edge);
        if delta == 0 {
            self.say(format!("{what} unchanged"));
        } else if self.amend(edge) {
            self.say(format!("{what} {}", signed(delta)));
        }
        self.go(Screen::Timeline, 0);
    }

    // seconds left before the window hides, None while the hand is near: CLOSE_AWAY after the
    // hand stayed away for AWAY_SETTLE or after the last input, whichever came later, so typing
    // keeps it open. A hand moving over the knob sends Away and Near within the settle time
    fn closing(&self) -> Option<f32> {
        let away = self.away?;
        if away.elapsed() < AWAY_SETTLE {
            return None;
        }
        let since = (away + AWAY_SETTLE).max(self.last_input);
        Some(CLOSE_AWAY.as_secs_f32() - since.elapsed().as_secs_f32())
    }

    fn back(&mut self) {
        match self.screen {
            Screen::Tasks(_) => self.go(Screen::Projects, self.project_turn),
            Screen::Projects | Screen::Choose(..) | Screen::Manage(_) => self.go_home(),
            Screen::Remove(_) => {
                self.say("kept");
                self.go_home();
            }
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
            // the edge goes back where it was: the entries come again from the database
            Screen::Edit(_, from) => {
                self.reload();
                self.at = from;
                self.say("put back");
                self.go(Screen::Timeline, 0);
            }
            Screen::Tracking | Screen::Timeline => {}
        }
    }

    // saves the typed name; a new project opens its task drum, a new task goes in at the pick
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
                self.project_turn = self.project_row(p).unwrap_or(1);
                // row 1, after back, is the first task, or "+ new task" in a project without tasks
                self.go(Screen::Tasks(p), 1);
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
            Row::Task(p, t) => self.create(p, t),
            Row::NewProject => {
                self.entry.clear();
                self.go(Screen::Input(Target::Project), 0);
            }
            Row::NewTask(p) => {
                self.entry.clear();
                self.go(Screen::Input(Target::Task(p)), 0);
            }
            Row::Entry => self.submit(),
            Row::Project(p) => {
                self.project_turn = self.turn;
                // tasks start at row 1, after back; the drum opens on the current entry's task
                let t = self
                    .current()
                    .filter(|r| r.project == self.projects[p].name)
                    .and_then(|r| self.projects[p].tasks.iter().position(|t| *t == r.task));
                let turn = t.and_then(|t| self.task_row(p, t)).unwrap_or(1);
                self.go(Screen::Tasks(p), turn);
            }
            Row::Back => self.back(),
        }
    }

    fn apply(&mut self, gesture: Gesture) {
        self.last_input = Instant::now();
        match (gesture, self.screen) {
            (Gesture::Double, Screen::Timeline) => {
                self.at = self.now();
                self.follow = true;
            }
            (Gesture::Double, _) => {}
            // on the timeline a hold hides the window, as close does while tracking
            (Gesture::Long, Screen::Timeline) => self.close = true,
            (Gesture::Long, _) => self.back(),
            (Gesture::Cw, Screen::Timeline) => self.step(1),
            (Gesture::Ccw, Screen::Timeline) => self.step(-1),
            (Gesture::Cw, Screen::Edit(edge, _)) => self.nudge(edge, 1),
            (Gesture::Ccw, Screen::Edit(edge, _)) => self.nudge(edge, -1),
            (Gesture::Cw, _) => self.turn = (self.turn + 1).min(self.count() - 1),
            (Gesture::Ccw, _) => self.turn = self.turn.saturating_sub(1),
            (Gesture::Click, Screen::Timeline) => self.click(),
            (Gesture::Click, Screen::Edit(edge, from)) => self.keep(edge, from),
            (Gesture::Click, Screen::Tracking) => match self.turn {
                0 => self.close = true,
                // the playhead stays on the stopped entry's end, to edit it
                1 => {
                    if self.stop() {
                        self.say("stopped");
                    }
                    self.at = self.now();
                    self.follow = false;
                    self.go_home();
                }
                _ => {
                    self.at = self.now();
                    self.open_pick(Side::After);
                }
            },
            (Gesture::Click, Screen::Choose(edge, side)) => match self.turn {
                0 => self.back(),
                1 => self.go(Screen::Edit(edge, self.at), 0),
                _ => self.open_pick(side),
            },
            (Gesture::Click, Screen::Manage(i)) => match self.turn {
                0 => self.back(),
                1 => self.open_rename(i),
                _ => self.go(Screen::Remove(i), 0),
            },
            // only the last choice removes; the ones before it keep the entry
            (Gesture::Click, Screen::Remove(i)) => {
                if self.turn == 3 {
                    self.remove(i);
                } else {
                    self.back();
                }
            }
            (Gesture::Click, _) => {
                let row = self.rows()[self.turn];
                self.run(row);
            }
        }
    }

    // keeps the playhead on now while tracking and while following, and eases the drum toward
    // the turn and the view toward the playhead
    fn tick(&mut self, dt: f32) {
        match self.screen {
            Screen::Tracking => self.at = self.now(),
            Screen::Timeline if self.follow => self.at = self.now(),
            _ => {}
        }
        let ease = 1.0 - (-40.0 * dt).exp();
        self.scroll += (self.turn as f32 - self.scroll) * ease;
        let ease = 1.0 - (-14.0 * dt as f64).exp();
        self.view += (self.at as f64 - self.view) * ease;
    }
}

// a drum row that does something to the drum, as opposed to a project or task to pick
fn is_action(row: Row) -> bool {
    matches!(row, Row::Back | Row::NewProject | Row::NewTask(_))
}

// the colors of the left, center and right zones for the app's state; while the window shows,
// they tell the mode: removing an entry, editing an edge, an entry running, or the free timeline
fn led_colors(app: &App, visible: bool) -> [timebuzzer::Color; 3] {
    let running = app.running_for();
    match (visible, running) {
        (true, _) if matches!(app.screen, Screen::Remove(_)) => {
            let t = app.last_input.elapsed().as_secs_f32() / LED_EDIT_PULSE.as_secs_f32();
            let level = (1.0 + (t * std::f32::consts::TAU).cos()) / 2.0;
            [timebuzzer::Color::RED.dim(LED_REMOVE_LOW + (1.0 - LED_REMOVE_LOW) * level); 3]
        }
        // timed from the last input, so every nudge starts a pulse bright
        (true, _) if matches!(app.screen, Screen::Edit(..)) => {
            let t = app.last_input.elapsed().as_secs_f32() / LED_EDIT_PULSE.as_secs_f32();
            let level = (1.0 + (t * std::f32::consts::TAU).cos()) / 2.0;
            [LED_GREEN.dim(LED_EDIT_LOW + (1.0 - LED_EDIT_LOW) * level); 3]
        }
        // timed from the entry's start
        (true, Some(elapsed)) => {
            let t = elapsed.as_secs_f32() / LED_BREATH.as_secs_f32();
            let level = (1.0 - (t * std::f32::consts::TAU).cos()) / 2.0;
            let side = LED_YELLOW.dim(LED_SIDE);
            [side, LED_YELLOW.dim(LED_BREATH_LOW + (1.0 - LED_BREATH_LOW) * level), side]
        }
        (true, None) => {
            let mut zones = [timebuzzer::Color::OFF, LED_GREEN, timebuzzer::Color::OFF];
            if let Some((at, dir)) = app.last_step {
                let left = 1.0 - at.elapsed().as_secs_f32() / LED_BLIP.as_secs_f32();
                if left > 0.0 {
                    zones[if dir > 0 { 2 } else { 0 }] = LED_GREEN.dim(left);
                }
            }
            zones
        }
        // hidden while tracking: a wave of green runs left to right, timed from the entry's start
        (false, Some(elapsed)) => {
            let t = elapsed.as_secs_f32() / LED_WAVE.as_secs_f32();
            [0.0, 1.0, 2.0].map(|zone| {
                let phase = (t - zone / 3.0) * std::f32::consts::TAU;
                let level = (1.0 - phase.cos()) / 2.0;
                LED_WAVE_COLOR.dim(LED_WAVE_LOW + (LED_WAVE_HIGH - LED_WAVE_LOW) * level)
            })
        }
        (false, None) => [timebuzzer::Color::OFF; 3],
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

// the knob, which comes and goes with the dock it hangs on; its events go to tx
struct Knob {
    tx: mpsc::Sender<timebuzzer::Event>,
    input: Option<timebuzzer::Input>,
    checked: Instant,
}

impl Knob {
    fn connect(&mut self, lights: &mut Lights) {
        let tx = self.tx.clone();
        self.input = timebuzzer::listen(move |event| {
            let _ = tx.send(event);
        });
        lights.leds = self.input.as_ref().and_then(|_| timebuzzer::open_leds());
        // a device plugged in again has lost what it showed, so every zone is written
        lights.lit = None;
        lights.changed_at = None;
    }

    fn present(&self) -> bool {
        self.input.is_some()
    }

    // every KNOB_CHECK, opens the knob when it appears and drops it when it goes; midir reports
    // neither, and a connection to an unplugged device stays open but silent. Returns whether
    // the knob went away in this call
    fn check(&mut self, lights: &mut Lights) -> bool {
        if self.checked.elapsed() < KNOB_CHECK {
            return false;
        }
        self.checked = Instant::now();
        match (timebuzzer::connected(), self.present()) {
            (true, false) => {
                self.connect(lights);
                if self.present() {
                    eprintln!("tm-buzzer: timebuzzer connected");
                }
            }
            (false, true) => {
                self.input = None;
                lights.leds = None;
                eprintln!("tm-buzzer: timebuzzer disconnected");
                return true;
            }
            _ => {}
        }
        false
    }
}

fn clock(d: Duration) -> String {
    let s = d.as_secs();
    format!("{}:{:02}:{:02}", s / 3600, s % 3600 / 60, s % 60)
}

// the local midnight that starts the day of the unix second t; t itself, when the local zone
// skips that midnight. The next day starts at day_start(day_start(t) + 26 hours), which holds
// over a daylight-saving change
fn day_start(t: i64) -> i64 {
    Local
        .timestamp_opt(t, 0)
        .single()
        .and_then(|l| l.date_naive().and_hms_opt(0, 0, 0))
        .and_then(|m| m.and_local_timezone(Local).earliest())
        .map_or(t, |m| m.timestamp())
}

// a unix second as local hours and minutes
fn hm(t: i64) -> String {
    Local
        .timestamp_opt(t, 0)
        .single()
        .map_or_else(|| "--:--".to_string(), |l| l.format("%H:%M").to_string())
}

// a unix second as tm's --at takes it, RFC 3339 in UTC
fn rfc(t: i64) -> String {
    DateTime::from_timestamp(t, 0).map_or_else(String::new, |d| d.to_rfc3339())
}

// seconds as hours and minutes
fn span(s: i64) -> String {
    let m = s.max(0) / 60;
    format!("{}:{:02}", m / 60, m % 60)
}

// a change in seconds as +15m or -1h05m
fn signed(s: i64) -> String {
    let sign = if s < 0 { "-" } else { "+" };
    match s.abs() / 60 {
        m if m >= 60 => format!("{sign}{}h{:02}m", m / 60, m % 60),
        m => format!("{sign}{m}m"),
    }
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

// puts the window in the middle of the screen in use; positions are in points, from the top left
// of the first screen, so a screen beside or above it has its own origin
fn center(rl: &mut RaylibHandle, window_w: i32, window_h: i32) {
    let (x, y, w, h) = macos::active_screen().unwrap_or_else(|| {
        let monitor = get_current_monitor();
        let origin = get_monitor_position(monitor);
        let (w, h) = (get_monitor_width(monitor), get_monitor_height(monitor));
        (origin.x as f64, origin.y as f64, w as f64, h as f64)
    });
    rl.set_window_position(
        (x + (w - window_w as f64) / 2.0) as i32,
        (y + (h - window_h as f64) / 2.0) as i32,
    );
}

fn main() {
    // the device calls back on midir's thread, so its events wait in a channel for the next frame
    let (knob_tx, knob_rx) = mpsc::channel();
    let mut lights = Lights {
        leds: None,
        lit: None,
        changed_at: None,
    };
    let mut device = Knob {
        tx: knob_tx,
        input: None,
        checked: Instant::now(),
    };
    device.connect(&mut lights);
    if !device.present() {
        eprintln!("tm-buzzer: no timebuzzer found, using the keyboard until one is plugged in");
    }
    // with a knob the app starts hidden and waits for its button; without one it shows at once
    let mut visible = !device.present();

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
    // Esc goes back from the input screen and hides the window elsewhere; nothing quits the app
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
    center(&mut rl, window_w, window_h);
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
    let config = config::load();
    let now = Utc::now().timestamp();
    let mut app = App {
        conn,
        projects: Vec::new(),
        entries: Vec::new(),
        from: day_start(day_start(now) - 1),
        last_step: None,
        screen: Screen::Timeline,
        turn: 0,
        scroll: 0.0,
        project_turn: 1,
        at: now,
        view: now as f64,
        follow: true,
        pick: Pick::New(now, Side::After),
        last_input: Instant::now(),
        away: None,
        flash: None,
        entry: String::new(),
        close: false,
        jira: config.jira.and_then(jira::watch),
        issues: Vec::new(),
        ignore: config.ignore,
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
    // whatever an earlier run left lit
    lights.off();
    lights.show(led_colors(&app, visible));

    while !rl.window_should_close() {
        if !visible {
            // nothing is on screen, so draw no frames until the knob's button goes down or a hand
            // comes near it; turns and the release of that press do nothing, because the knob's
            // button never saw the press.
            // Between events the database is read again, for the LEDs
            // It waits on the main run loop in LED_FRAME slices, so CoreMIDI can report the knob
            // being plugged in, and looks at the knob's events between slices
            let mut polled = Instant::now();
            'wait: loop {
                for event in knob_rx.try_iter() {
                    // a press, or a hand coming near the knob
                    if matches!(event, timebuzzer::Event::ClickDown | timebuzzer::Event::Near) {
                        break 'wait;
                    }
                }
                macos::run_loop(LED_FRAME.as_secs_f64());
                if polled.elapsed() >= HIDDEN_POLL {
                    app.reload();
                    polled = Instant::now();
                }
                device.check(&mut lights);
                lights.show(led_colors(&app, false));
            }
            // the screen in use may be another one than at the last show
            center(&mut rl, window_w, window_h);
            macos::unhide();
            rl.clear_window_state(hidden);
            rl.set_window_focused();
            // tm's CLI may have changed the entries while the window was hidden
            app.reload();
            // the answer lands a moment later, and the loop below reloads with it
            if let Some(jira) = &app.jira {
                jira.refresh();
            }
            // the playhead opens on now
            app.at = app.now();
            app.view = app.at as f64;
            app.follow = true;
            app.go_home();
            app.last_input = Instant::now();
            app.away = None;
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
                app.close = true;
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
                timebuzzer::Event::Near => app.away = None,
                timebuzzer::Event::Away => app.away = Some(Instant::now()),
            }
        }
        knob.update(now, false, false, &mut gestures);
        for g in gestures.drain(..) {
            app.apply(g);
        }
        // a hidden window waits for the knob, which may be plugged in again later. An unplugged
        // knob closes the window with it
        if device.check(&mut lights) || app.closing().is_some_and(|left| left <= 0.0) {
            app.close = true;
        }
        if app.close {
            app.close = false;
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
        // the window reloads only on writes, so a Jira answer that lands while it is open, such
        // as the first search after launch, reloads here
        if app.take_issues() {
            app.reload();
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
        let centered = |d: &mut Canvas, s: &str, x: f32, y: f32, size: f32, color: Color| {
            d.draw_text_ex(&font, s, Vector2::new(x - measure(s, size) / 2.0, y), size, 0.0, color);
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
        // s cut to max wide, ending in "..." when it is cut
        let fit = |s: &str, size: f32, max: f32| {
            if measure(s, size) <= max {
                return s.to_string();
            }
            let mut cut: String = s.to_string();
            while !cut.is_empty() && measure(&format!("{cut}..."), size) > max {
                cut.pop();
            }
            if cut.is_empty() { String::new() } else { format!("{}...", cut.trim_end()) }
        };

        let rows = app.rows();
        let n = rows.len();
        let sel = app.turn;
        let running = app.running().map(|r| &app.entries[r]);
        let running_for = app.running_for();
        let task_name = |p: usize, t: usize| app.projects[p].tasks[t].as_str();
        // the Jira issue of a task named by an issue key
        let issue = |p: usize, t: usize| {
            app.issues
                .iter()
                .find(|i| i.project == app.projects[p].name && i.key == task_name(p, t))
        };
        let live_task = |p: usize, t: usize| running.is_some_and(|r| r.project == app.projects[p].name && r.task == task_name(p, t));
        let live_project = |p: usize| running.is_some_and(|r| r.project == app.projects[p].name);
        let drums = matches!(app.screen, Screen::Projects | Screen::Tasks(_) | Screen::Input(_));
        let now_s = Utc::now().timestamp_millis() as f64 / 1000.0;
        let now_i = app.now();
        // what a task picked on the drums does: start it, switch to it, log it, or rename to it
        let (verb, when) = match &app.pick {
            Pick::Rename(_) => ("rename", format!("was {}", app.current().map_or("", |e| e.task.as_str()))),
            Pick::New(at, side) => match app.new_span(*at, *side) {
                (_, None) if running.is_some() => ("switch", format!("stops {}", clock(running_for.unwrap_or_default()))),
                (start, None) if now_i - start <= 1 => ("start", "now".to_string()),
                (start, None) => ("start", format!("from {}", hm(start))),
                (start, Some(end)) => ("log", format!("{} - {} . {}", hm(start), hm(end), span(end - start))),
            },
        };
        let edge = app.edge_at(app.at);
        let (editing, from) = match app.screen {
            Screen::Edit(edge, from) => (Some(edge), Some(from)),
            _ => (None, None),
        };
        // the edge the playhead is on, or the one the screen works on
        let focus = match app.screen {
            Screen::Choose(edge, _) | Screen::Edit(edge, _) => Some(edge),
            Screen::Manage(i) | Screen::Remove(i) => Some(Edge::Middle(i)),
            _ => edge,
        };
        let lanes = app.lanes();
        let lit: Vec<usize> = focus.map_or(Vec::new(), |e| app.touched(e));

        let (next, command_color) = match app.screen {
            Screen::Tracking => ("tracking".to_string(), GREEN),
            Screen::Timeline => ("timeline".to_string(), GREEN),
            Screen::Choose(edge, _) => (app.edge_name(edge), GREEN),
            Screen::Edit(edge, _) => (format!("edit {}", app.edge_name(edge)), GREEN),
            Screen::Manage(i) => (app.entries[i].task.clone(), GREEN),
            Screen::Remove(i) => (format!("remove {}?", app.entries[i].task), DANGER),
            Screen::Projects => (verb.to_string(), GREEN),
            Screen::Tasks(p) => (format!("{}/tasks", app.projects[p].name), GREEN),
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
        let status = match app.screen {
            Screen::Projects | Screen::Tasks(_) => format!("{sel}/{}", n - 1),
            _ => Local
                .timestamp_opt(app.at, 0)
                .single()
                .map_or_else(String::new, |l| l.format("%a %d %b").to_string().to_lowercase()),
        };
        let hint = match (app.screen, edge) {
            (Screen::Tracking, _) => "turn: choose . click: go",
            (Screen::Timeline, Some(Edge::Seam(..))) => "turn: 5m . click: edit seam . 2x: now . hold: close",
            (Screen::Timeline, Some(Edge::Middle(_))) => "turn: 5m . click: rename or remove . 2x: now",
            (Screen::Timeline, Some(_)) => "turn: 5m . click: edit or add . 2x: now . hold: close",
            (Screen::Timeline, None) if app.at == now_i => "turn: 5m . click: start task . hold: close",
            (Screen::Timeline, None) => "turn: 5m . click: start task here . 2x: now",
            (Screen::Choose(..) | Screen::Manage(_), _) => "turn: choose . click: go",
            (Screen::Remove(_), _) => "click on remove deletes, anywhere else keeps",
            (Screen::Edit(..), _) => "turn: move edge . click: keep . hold: put back",
            (Screen::Projects, _) => "turn: move . click: open",
            (Screen::Tasks(_), _) => "turn: move . click: pick",
            (Screen::Input(_), _) => "type a name . enter: save . esc: back",
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
        // the rule's dashes count down the time before the window closes once the hand is away,
        // each fading out in its share of it, from the right; no dashes while the hand is near
        let left = app.closing().unwrap_or(0.0);
        // how many dashes' worth of time is left, e.g. 6.5 lights six and half of the seventh
        let left = (left / CLOSE_AWAY.as_secs_f32()).max(0.0) * IDLE_DASHES as f32;
        let step = W / IDLE_DASHES as f32;
        for k in 0..IDLE_DASHES {
            let alpha = (left - k as f32).clamp(0.0, 1.0);
            let x = k as f32 * step + DASH_GAP / 2.0;
            let dash = Rectangle::new(x, RULE_Y - point, step - DASH_GAP, 2.0 * point);
            d.draw_rectangle_rec(dash, fade(GREEN, 0.8 * alpha));
        }

        // the strip: the running entry as project / task and its timer, on every screen
        let strip_y = RULE_Y + STRIP / 2.0;
        match (running, running_for) {
            (Some(r), Some(elapsed)) => {
                let project = format!("{} / ", r.project);
                text(&mut d, &project, 14.0, strip_y - 7.0, 14.0, YELLOW_DIM);
                text(&mut d, &r.task, 14.0 + measure(&project, 14.0), strip_y - 7.0, 14.0, YELLOW);
                // right-aligned on the advances timer draws with
                let elapsed = clock(elapsed);
                let digit = measure("0", 16.0);
                let width: f32 = elapsed.chars().map(|ch| if ch == ':' { digit * 0.5 } else { digit }).sum();
                timer(&mut d, &elapsed, W - 14.0 - width, strip_y - 8.0, 16.0, YELLOW);
            }
            _ => text(&mut d, "nothing running", 14.0, strip_y - 7.0, 14.0, fade(DIM, 0.6)),
        }
        d.draw_line_ex(Vector2::new(0.0, STRIP_RULE_Y), Vector2::new(W, STRIP_RULE_Y), point, fade(DIM, 0.5));

        if !drums {
            // a second's x on the panel; the playhead is the middle
            let x = |t: f64| W / 2.0 + ((t - app.view) / 60.0) as f32 * PX;
            // seconds from the middle of the view to either side
            let half = (W / 2.0 / PX) as f64 * 60.0;
            // the playhead's day, from its midnight to the next
            let ds = day_start(app.at);
            let de = day_start(ds + 26 * 3600);

            // the future, past now, behind the ruler and the lane
            let now_x = x(now_s);
            if now_x < W {
                let from = now_x.max(0.0);
                let future = Rectangle::new(from, RULER_Y - 12.0, W - from, LANE_Y + LANE_H + 6.0 - (RULER_Y - 12.0));
                d.draw_rectangle_rec(future, fade(GLOW, 0.25));
            }

            // the ruler: a tick every 5 minutes, longer each quarter and hour, hours named; a
            // midnight is named by its day and draws a dotted line through the lane
            let mut t = ((app.view - half) as i64).div_euclid(STEP) * STEP;
            while (t as f64) <= app.view + half + STEP as f64 {
                let tx = x(t as f64);
                let local = Local.timestamp_opt(t, 0).single();
                let minute = local.map_or(1, |l| l.minute());
                if let Some(l) = local.filter(|l| l.hour() == 0 && l.minute() == 0) {
                    let date = l.format("%a %d").to_string().to_lowercase();
                    text(&mut d, &date, tx + 3.0, RULER_Y - 12.0, 9.0, DIM);
                    let mut y = RULER_Y;
                    while y < LANE_Y + LANE_H + 6.0 {
                        d.draw_line_ex(Vector2::new(tx, y), Vector2::new(tx, y + 2.0), point, fade(DIM, 0.6));
                        y += 5.0;
                    }
                }
                let (len, color) = match minute {
                    0 => (10.0, DIM),
                    15 | 30 | 45 => (6.0, fade(DIM, 0.7)),
                    _ => (3.0, fade(DIM, 0.4)),
                };
                let color = if t as f64 > now_s { fade(color, 0.5) } else { color };
                d.draw_line_ex(Vector2::new(tx, RULER_Y - len), Vector2::new(tx, RULER_Y), point, color);
                if minute == 0 {
                    centered(&mut d, &hm(t), tx, RULER_Y - 24.0, 10.0, color);
                }
                t += STEP;
            }

            // the lane: a thread through the gaps, and the entries as blocks over it; overlapping
            // entries split the lane's height and get a red outline
            let lane_mid = LANE_Y + LANE_H / 2.0;
            let mut gx = 0.0;
            while gx < W {
                d.draw_line_ex(Vector2::new(gx, lane_mid), Vector2::new(gx + 2.0, lane_mid), point, fade(DIM, 0.4));
                gx += 6.0;
            }
            for (i, e) in app.entries.iter().enumerate() {
                let x0 = x(e.start.timestamp() as f64);
                let x1 = x(e.stop.map_or(now_s, |t| t.timestamp() as f64));
                if x1 < 0.0 || x0 > W {
                    continue;
                }
                let (lane, count) = lanes[i];
                let h = LANE_H / count as f32;
                let block = Rectangle::new(x0, LANE_Y + lane as f32 * h, x1 - x0, h);
                let on = lit.contains(&i);
                let (hi, dim, glow) = if e.stop.is_none() { (YELLOW, YELLOW_DIM, YELLOW_GLOW) } else { (GREEN, DIM, GLOW) };
                d.draw_rectangle_rec(block, if on { glow } else { fade(glow, 0.6) });
                match (count > 1, on, editing.is_some()) {
                    (true, _, _) => d.draw_rectangle_lines_ex(block, point, fade(DANGER, 0.7)),
                    (false, true, true) => dashed(&mut d, block, point, hi),
                    (false, true, false) => d.draw_rectangle_lines_ex(block, point, hi),
                    (false, false, _) => d.draw_rectangle_lines_ex(block, point, fade(dim, 0.7)),
                }
                // the labels stay in view while the block is partly off the left side
                let lx = x0.max(4.0) + 6.0;
                let room = x1.min(W - 4.0) - lx - 4.0;
                if room <= 12.0 {
                    continue;
                }
                let task_color = if on { hi } else { fade(hi, 0.75) };
                if count > 1 {
                    text(&mut d, &fit(&e.task, 11.0, room), lx, block.y + (h - 11.0) / 2.0, 11.0, task_color);
                    continue;
                }
                text(&mut d, &fit(&e.project, 10.0, room), lx, LANE_Y + 7.0, 10.0, dim);
                text(&mut d, &fit(&e.task, 14.0, room), lx, LANE_Y + 20.0, 14.0, task_color);
                let len = span(app.ended(i) - app.started(i));
                if measure(&len, 10.0) + 8.0 < room - measure(&e.task, 14.0) {
                    right(&mut d, &len, x1.min(W - 4.0) - 6.0, LANE_Y + LANE_H - 14.0, 10.0, fade(dim, 0.8));
                }
            }
            // now, a solid line through the lane
            if (0.0..W).contains(&now_x) {
                d.draw_line_ex(Vector2::new(now_x, RULER_Y - 12.0), Vector2::new(now_x, LANE_Y + LANE_H + 6.0), point * 1.5, GREEN);
                text(&mut d, "now", now_x + 3.0, LANE_Y + LANE_H + 2.0, 9.0, GREEN);
            }

            // the playhead, with the minute under it; on an edge, a bracket opening toward its
            // entry, and while editing a dotted line where the edge was
            let px = W / 2.0;
            let (top, bottom) = (RULER_Y - 4.0, LANE_Y + LANE_H + 4.0);
            let head = if editing.is_some() { GREEN } else { fade(GREEN, 0.7) };
            d.draw_line_ex(Vector2::new(px, top), Vector2::new(px, bottom), point, head);
            if let Some(from) = from {
                let ghost = x(from as f64);
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
                    // a middle is marked by short serifs on both sides
                    Edge::Middle(_) => {
                        for y in [LANE_Y - 4.0, LANE_Y + LANE_H + 2.0] {
                            d.draw_rectangle_rec(Rectangle::new(px - 3.0, y, 6.0, 2.0), color);
                        }
                    }
                }
            }
            let label = hm(app.at);
            let bw = measure(&label, 12.0) + 10.0;
            let tag = Rectangle::new(px - bw / 2.0, RULER_Y - 30.0, bw, 18.0);
            d.draw_rectangle_rec(tag, PANEL);
            d.draw_rectangle_lines_ex(tag, point, head);
            centered(&mut d, &label, px, RULER_Y - 27.0, 12.0, GREEN);

            // the info: the choices of a screen that has them as boxes side by side, the
            // selected one outlined as an action, the running entry's in its yellow and a
            // removal's in red; else what the playhead is on
            d.draw_line_ex(Vector2::new(0.0, INFO_Y - 10.0), Vector2::new(W, INFO_Y - 10.0), point, fade(DIM, 0.3));
            let labels: &[&str] = match app.screen {
                Screen::Choose(_, Side::Before) => &["< back", "edit edge", "+ task before"],
                Screen::Choose(_, Side::After) => &["< back", "edit edge", "+ task after"],
                Screen::Tracking => &["close", "[] stop", "switch"],
                Screen::Manage(_) => &["< back", "rename", "x remove"],
                // two empty boxes keep remove a few deliberate detents away
                Screen::Remove(_) => &["keep", ".", ".", "x remove"],
                _ => &[],
            };
            let (hi, dim, glow) = match app.screen {
                Screen::Tracking => (YELLOW, YELLOW_DIM, YELLOW_GLOW),
                Screen::Remove(_) => (DANGER, fade(DANGER, 0.5), fade(DANGER, 0.2)),
                _ => (GREEN, DIM, GLOW),
            };
            match labels {
                [_, ..] => {
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
                [] => {
                    let times = |i: usize| {
                        let e = &app.entries[i];
                        let end = e.stop.map_or_else(|| "now".to_string(), |t| hm(t.timestamp()));
                        format!("{} - {end} . {}", hm(app.started(i)), span(app.ended(i) - app.started(i)))
                    };
                    let length = |i: usize| span(app.ended(i) - app.started(i));
                    let (what, detail) = match focus {
                        Some(Edge::Seam(a, b)) => (
                            format!("{} | {}", app.entries[a].task, app.entries[b].task),
                            format!("{}  |  {}", length(a), length(b)),
                        ),
                        Some(Edge::Start(i)) => (format!("[ {}", app.entries[i].task), times(i)),
                        Some(Edge::End(i)) => (format!("{} ]", app.entries[i].task), times(i)),
                        Some(Edge::Now(i)) => (format!("{} > running", app.entries[i].task), times(i)),
                        Some(Edge::Middle(i)) => {
                            (format!("{} / {}", app.entries[i].project, app.entries[i].task), times(i))
                        }
                        None if app.at > now_i => ("later".to_string(), format!("{} from now", signed(app.at - now_i))),
                        None if app.at == now_i => ("now".to_string(), "nothing running".to_string()),
                        None => {
                            let all = 0..app.entries.len();
                            let before = all.clone().filter(|&i| app.started(i) < app.at).map(|i| app.ended(i)).max();
                            let after = all.filter(|&i| app.started(i) > app.at).map(|i| app.started(i)).min();
                            let from = before.unwrap_or(ds).max(ds);
                            let to = after.unwrap_or(now_i).min(de);
                            ("gap".to_string(), format!("{} - {} . {} untracked", hm(from), hm(to), span(to - from)))
                        }
                    };
                    text(&mut d, &what, 14.0, INFO_Y, 16.0, GREEN);
                    right(&mut d, &detail, W - 14.0, INFO_Y + 2.0, 12.0, DIM);
                }
            }
            if let Some(from) = from {
                let delta = app.at - from;
                let color = if delta == 0 { DIM } else { GREEN };
                text(&mut d, &format!("moved {}", signed(delta)), 14.0, INFO_Y + 22.0, 11.0, color);
            }

            // above the day map: the playhead's working day from the first start to the last end,
            // and the time the entries cover in it, each entry cut to the day
            let in_day: Vec<usize> = (0..app.entries.len()).filter(|&i| app.started(i) < de && app.ended(i) > ds).collect();
            let cut = |i: usize| (app.started(i).max(ds), app.ended(i).min(de));
            if let (Some(first), Some(last)) = (
                in_day.iter().map(|&i| cut(i).0).min(),
                in_day.iter().map(|&i| cut(i).1).max(),
            ) {
                let tracked: i64 = in_day.iter().map(|&i| cut(i).1 - cut(i).0).sum();
                let runs = in_day.iter().any(|&i| app.entries[i].stop.is_none());
                let until = if runs { "now".to_string() } else { hm(last) };
                let day = format!("day {} - {until} . {}", hm(first), span(last - first));
                text(&mut d, &day, 14.0, MAP_Y - 18.0, 11.0, DIM);
                let sum = format!("tracked {} . gaps {}", span(tracked), span(last - first - tracked));
                right(&mut d, &sum, W - 14.0, MAP_Y - 18.0, 11.0, GREEN);
            }

            // the playhead's whole day: entries, the view's span
            let (mx, mw) = (14.0, W - 28.0);
            let mapx = |t: f64| mx + mw * ((t - ds as f64) / (de - ds) as f64).clamp(0.0, 1.0) as f32;
            d.draw_rectangle_lines_ex(Rectangle::new(mx, MAP_Y, mw, MAP_H), point, fade(DIM, 0.4));
            for (i, e) in app.entries.iter().enumerate() {
                let (a, b) = (mapx(app.started(i) as f64), mapx(app.ended(i) as f64));
                let color = match (e.stop.is_none(), lit.contains(&i)) {
                    (true, true) => YELLOW,
                    (true, false) => YELLOW_DIM,
                    (false, true) => GREEN,
                    (false, false) => DIM,
                };
                d.draw_rectangle_rec(Rectangle::new(a, MAP_Y + 2.0, (b - a).max(point), MAP_H - 4.0), color);
            }
            let (va, vb) = (mapx(app.view - half), mapx(app.view + half));
            d.draw_rectangle_lines_ex(Rectangle::new(va, MAP_Y - 3.0, vb - va, MAP_H + 6.0), point, GREEN);
            for h in [0, 6, 12, 18, 24] {
                let hx = mapx((ds + h * 3600) as f64);
                centered(&mut d, &h.to_string(), hx, MAP_Y + MAP_H + 4.0, 9.0, fade(DIM, 0.7));
            }
        } else {
            // the drums: the column the drum turns in; the other side holds a preview or a tab
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
                            for (k, t) in app.shown_tasks(p).into_iter().take(7).enumerate() {
                                let y = BAND_Y - 96.0 + k as f32 * 30.0;
                                text(&mut d, task_name(p, t), 188.0, y, 14.0, fade(DIM, 0.6));
                            }
                        }
                        Row::Back => text(&mut d, "timeline", 188.0, BAND_Y - 7.0, 14.0, fade(DIM, 0.6)),
                        Row::NewProject => text(&mut d, "type a name", 188.0, BAND_Y - 7.0, 14.0, fade(DIM, 0.6)),
                        _ => {}
                    }
                }
                Screen::Tasks(p) | Screen::Input(Target::Task(p)) => {
                    let tab = Rectangle::new(8.0, BAND_Y - 18.0, 76.0, 36.0);
                    d.draw_rectangle_rec(tab, fade(GLOW, 0.6));
                    d.draw_rectangle_lines_ex(tab, point, fade(DIM, 0.6));
                    text(&mut d, &fit(&app.projects[p].name, 13.0, 64.0), 16.0, BAND_Y - 7.0, 13.0, DIM);
                    // where the drum sits in the whole list
                    if n > 8 {
                        let (top, bottom) = (STRIP_RULE_Y + 16.0, H - 40.0);
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
            let band = Rectangle::new(x0, BAND_Y - BAND / 2.0, x1 - x0, BAND);
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
                let offset = far.min(1.0) * (BAND / 2.0 + ROW / 2.0) + (far - 1.0).max(0.0) * ROW;
                let y = BAND_Y + dist.signum() * offset;
                // largest in the band, smaller with every row away; 20 leaves room in the band for a
                // Jira title under the key
                let size = lerp(20.0, 14.0, (far / 3.0).min(1.0));
                let alpha = if far <= 1.0 { lerp(1.0, 0.65, far) } else { lerp(0.65, 0.0, (far - 1.0) / 2.2) };
                // the band's own details fade in as the row settles into it
                let settled = (1.0 - far * 2.0).max(0.0);
                let x = x0 + 22.0;

                let (label, color) = match row {
                    Row::Task(p, t) => (format!("{}{}", task_name(p, t), if live_task(p, t) { " *" } else { "" }), GREEN),
                    Row::Project(p) => (format!("{}{}", app.projects[p].name, if live_project(p) { " *" } else { "" }), GREEN),
                    Row::Back if matches!(app.screen, Screen::Tasks(_)) => ("< project".to_string(), GREEN),
                    Row::Back => ("< timeline".to_string(), GREEN),
                    Row::NewProject => ("+ new project".to_string(), GREEN),
                    Row::NewTask(_) => ("+ new task".to_string(), GREEN),
                    Row::Entry => (format!("{}{blink}", app.entry), GREEN),
                };
                // actions sit back from the projects and tasks: dimmer and smaller until selected
                let (size, color) = match (is_action(row), i == sel) {
                    (true, true) => (size * ACTION_SIZE, color),
                    (true, false) => (size * ACTION_SIZE, DIM),
                    (false, _) => (size, color),
                };

                // the right side of the band: what picking the selected row does
                let (top, bottom) = match row {
                    _ if i != sel || settled == 0.0 => (String::new(), String::new()),
                    Row::Task(..) => (verb.to_string(), when.clone()),
                    Row::NewTask(_) => (String::new(), "keyboard".to_string()),
                    Row::Entry => (String::new(), "enter: save".to_string()),
                    _ => (String::new(), String::new()),
                };

                // a Jira task carries its assignee after the key and its title under it, both smaller,
                // each cut to the room it has
                match row {
                    Row::Task(p, t) if let Some(issue) = issue(p, t) => {
                        let small = (size * 0.55).max(9.0);
                        let has_title = !issue.summary.is_empty();
                        let key_y = if has_title { y - (size + small) / 2.0 } else { y - size / 2.0 };
                        text(&mut d, &label, x, key_y, size, fade(color, alpha));
                        let end = x1 - 12.0;
                        if let Some(name) = &issue.assignee {
                            // on the key's lower part, as a suffix to it
                            let after = x + measure(&label, size);
                            let room = if top.is_empty() { end } else { end - measure(&top, 12.0) - 8.0 };
                            let name = fit(&format!(" . @{name}"), small, room - after);
                            text(&mut d, &name, after, key_y + (size - small) * 0.75, small, fade(DIM, alpha));
                        }
                        if has_title {
                            let reserve = if bottom.is_empty() { 0.0 } else { measure(&bottom, 11.0) + 8.0 };
                            let title = fit(&issue.summary, small, end - reserve - x);
                            text(&mut d, &title, x, key_y + size, small, fade(DIM, alpha));
                        }
                    }
                    _ => text(&mut d, &label, x, y - size / 2.0, size, fade(color, alpha)),
                }

                right(&mut d, &top, x1 - 12.0, y - 14.0, 12.0, fade(GREEN, settled));
                right(&mut d, &bottom, x1 - 12.0, y + 2.0, 11.0, fade(DIM, settled));
            }
        }

        // footer: the hint, replaced by a flash message while one shows
        d.draw_line_ex(Vector2::new(0.0, H - 30.0), Vector2::new(W, H - 30.0), point, fade(DIM, 0.5));
        match flash {
            Some((message, alpha)) => {
                let color = if message.starts_with("can't") || message.starts_with("overlapping") { DANGER } else { GREEN };
                text(&mut d, &message, 14.0, H - 21.0, 12.0, fade(color, alpha));
            }
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
