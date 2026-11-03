use midir::{Ignore, MidiInput, MidiOutput, MidiOutputConnection};
use std::sync::Mutex;
use std::thread::sleep;
use std::time::Duration;

// ── MIDI constants ────────────────────────────────────────────────────────────

const MIDI_CH: u8 = 0xBB;          // Control Change, channel 12
const CC_POSITION: u8 = 0x50;
const CC_STEP: u8 = 0x51;
const CC_CLICK: u8 = 0x52;

// Minimum inter-message delay confirmed via hardware sweep (250µs < ok ≤ 500µs).
// At 250µs, changes that turn one channel off and another on (green to red) sometimes kept the
// old channel lit or skipped a zone, so this uses the sweep's upper bound.
const MSG_DELAY: Duration = Duration::from_micros(500);

// ── LED zone layout ───────────────────────────────────────────────────────────
//
//   Each zone has independent R/G/B channels (additive mixing).
//   Value 0 = off, 1–127 = brightness.
//   Zeros must be sent before non-zeros to avoid clearing active channels.

struct ZoneCcs { r: u8, g: u8, b: u8 }

const LEFT:   ZoneCcs = ZoneCcs { r: 0x46, g: 0x47, b: 0x48 };
const CENTER: ZoneCcs = ZoneCcs { r: 0x49, g: 0x4A, b: 0x4B };
const RIGHT:  ZoneCcs = ZoneCcs { r: 0x4C, g: 0x4D, b: 0x4E };
const ALL_ZONES: [&ZoneCcs; 3] = [&LEFT, &CENTER, &RIGHT];

// ── Public types ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// Encoder turned clockwise (position increased).
    Right,
    /// Encoder turned counter-clockwise (position decreased).
    Left,
    /// Push button pressed.
    ClickDown,
    /// Push button released.
    ClickUp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Zone {
    Left,
    Center,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Color {
    pub const OFF:     Self = Self { r: 0,   g: 0,   b: 0   };
    pub const RED:     Self = Self { r: 127, g: 0,   b: 0   };
    pub const GREEN:   Self = Self { r: 0,   g: 127, b: 0   };
    pub const BLUE:    Self = Self { r: 0,   g: 0,   b: 127 };
    pub const YELLOW:  Self = Self { r: 127, g: 127, b: 0   };
    pub const CYAN:    Self = Self { r: 0,   g: 127, b: 127 };
    pub const MAGENTA: Self = Self { r: 127, g: 0,   b: 127 };
    pub const WHITE:   Self = Self { r: 127, g: 127, b: 127 };

    pub fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r: r.min(127), g: g.min(127), b: b.min(127) }
    }

    pub fn dim(self, factor: f32) -> Self {
        let d = |v: u8| ((v as f32 * factor.clamp(0.0, 1.0)) as u8).min(127);
        Self { r: d(self.r), g: d(self.g), b: d(self.b) }
    }
}

// ── LED output ────────────────────────────────────────────────────────────────

pub struct Leds(MidiOutputConnection);

impl Leds {
    pub fn set(&mut self, zone: Zone, color: Color) {
        let ccs = match zone {
            Zone::Left   => &LEFT,
            Zone::Center => &CENTER,
            Zone::Right  => &RIGHT,
        };
        self.write_zone(ccs, color);
    }

    pub fn set_all(&mut self, color: Color) {
        for ccs in ALL_ZONES {
            self.write_zone(ccs, color);
        }
    }

    pub fn off(&mut self) {
        // Double-pass ensures reliable clear even when channels had active state.
        for _ in 0..2 {
            for cc in 0x46u8..=0x4E {
                self.send(cc, 0x00);
            }
        }
    }

    fn write_zone(&mut self, ccs: &ZoneCcs, color: Color) {
        let channels = [(ccs.r, color.r), (ccs.g, color.g), (ccs.b, color.b)];
        // zeros first — prevents clearing already-active channels on the device
        for &(cc, val) in channels.iter().filter(|(_, v)| *v == 0) {
            self.send(cc, val);
        }
        for &(cc, val) in channels.iter().filter(|(_, v)| *v > 0) {
            self.send(cc, val);
        }
    }

    fn send(&mut self, cc: u8, val: u8) {
        self.0.send(&[MIDI_CH, cc, val]).ok();
        sleep(MSG_DELAY);
    }
}

// ── Input parser ──────────────────────────────────────────────────────────────

struct InputState {
    prev_pos: Option<u8>,
    click_down: bool,
}

impl InputState {
    fn new() -> Self { Self { prev_pos: None, click_down: false } }

    fn parse(&mut self, msg: &[u8]) -> Option<Event> {
        if msg.len() != 3 || msg[0] != MIDI_CH { return None; }
        let (cc, val) = (msg[1], msg[2]);
        match cc {
            CC_POSITION => {
                // the position stops at 0 and does not wrap: each further detent left sends 0
                // again, so a repeated 0 is a turn left. A repeated 127 is taken as a turn right
                // on the same pattern; no capture has reached 127 yet
                let ev = match self.prev_pos {
                    Some(p) if val > p => Some(Event::Right),
                    Some(p) if val < p => Some(Event::Left),
                    Some(0) if val == 0 => Some(Event::Left),
                    Some(0x7F) if val == 0x7F => Some(Event::Right),
                    _ => None,
                };
                self.prev_pos = Some(val);
                ev
            }
            CC_CLICK if val == 0x7F => {
                self.click_down = true;
                Some(Event::ClickDown)
            }
            CC_CLICK if val == 0x00 && self.click_down => {
                self.click_down = false;
                Some(Event::ClickUp)
            }
            CC_CLICK | CC_STEP => None,
            _ => None,
        }
    }
}

// ── Top-level connection ──────────────────────────────────────────────────────

fn find_port<T: midir::MidiIO>(io: &T) -> Option<T::Port> {
    io.ports().into_iter().find(|p| {
        io.port_name(p)
            .unwrap_or_default()
            .to_lowercase()
            .contains("timebuzzer")
    })
}

/// Open the timeBuzzer's LED output. Returns `None` if device is not connected.
pub fn open_leds() -> Option<Leds> {
    let out = MidiOutput::new("tb-cap").ok()?;
    let port = find_port(&out)?;
    Some(Leds(out.connect(&port, "tb-leds").ok()?))
}

/// Open the timeBuzzer's input and call `callback` for every decoded event.
/// Blocks until the returned connection is dropped.
pub fn listen(callback: impl Fn(Event) + Send + 'static) -> Option<midir::MidiInputConnection<()>> {
    let mut input = MidiInput::new("tb-cap").ok()?;
    input.ignore(Ignore::None);
    let port = find_port(&input)?;
    let state = Mutex::new(InputState::new());
    input.connect(&port, "tb-input", move |_, msg, _| {
        if let Some(ev) = state.lock().unwrap().parse(msg) {
            callback(ev);
        }
    }, ()).ok()
}
