// Prints every MIDI message the timebuzzer sends, as hex, until Enter.
//
// Usage: cargo run -p timebuzzer --example raw
use std::io::BufRead;

use midir::{Ignore, MidiInput};

fn main() {
    let mut input = MidiInput::new("timebuzzer-raw").expect("init MIDI input");
    input.ignore(Ignore::None);
    let port = input
        .ports()
        .into_iter()
        .find(|p| {
            input
                .port_name(p)
                .unwrap_or_default()
                .to_lowercase()
                .contains("timebuzzer")
        })
        .expect("no timebuzzer port");
    let _conn = input
        .connect(
            &port,
            "timebuzzer-raw",
            |stamp, msg, _| {
                let hex: Vec<String> = msg.iter().map(|b| format!("{b:02X}")).collect();
                println!("{stamp:>12}  {}", hex.join(" "));
            },
            (),
        )
        .expect("connect");
    println!("turn and press the knob; Enter quits");
    std::io::stdin().lock().lines().next();
}
