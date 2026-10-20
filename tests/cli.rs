use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_HOME: AtomicUsize = AtomicUsize::new(0);

/// Runs the `tm` binary with `HOME` set to a fresh temporary directory, so each
/// test gets its own database and daemon socket path.
struct Tm {
    home: PathBuf,
}

struct Output {
    stdout: String,
    stderr: String,
    code: Option<i32>,
}

impl Tm {
    fn new() -> Self {
        let home = std::env::temp_dir().join(format!(
            "tm-cli-test-{}-{}",
            std::process::id(),
            NEXT_HOME.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).expect("create test home");
        Tm { home }
    }

    fn run(&self, args: &[&str]) -> Output {
        let output = Command::new(env!("CARGO_BIN_EXE_tm"))
            .args(args)
            .arg("--no-color")
            .env("HOME", &self.home)
            .output()
            .expect("run tm");
        Output {
            stdout: String::from_utf8(output.stdout).expect("stdout is UTF-8"),
            stderr: String::from_utf8(output.stderr).expect("stderr is UTF-8"),
            code: output.status.code(),
        }
    }
}

impl Drop for Tm {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

/// Returns the lines after the `today:` header line.
fn task_lines(log: &str) -> Vec<&str> {
    log.lines().skip(1).collect()
}

#[test]
fn log_without_entries_shows_no_tasks() {
    let tm = Tm::new();

    let log = tm.run(&["log"]);

    assert!(log.stdout.starts_with("today: "), "{}", log.stdout);
    assert!(log.stdout.contains("(total time: 0s)"), "{}", log.stdout);
    assert_eq!(task_lines(&log.stdout), vec!["  No tasks."]);
}

#[test]
fn start_and_stop_at_show_task_total_in_log() {
    let tm = Tm::new();

    let start = tm.run(&["start", "acme", "review", "--at", "09:00"]);
    let stop = tm.run(&["stop", "--at", "10:30"]);
    let log = tm.run(&["log"]);

    assert_eq!(start.stdout, "Started tracking: acme / review\n");
    assert_eq!(stop.stdout, "Stopped tracking.\n");
    assert!(log.stdout.contains("(total time: 1h 30m)"), "{}", log.stdout);
    assert_eq!(task_lines(&log.stdout), vec!["  - acme / review (1h 30m)"]);
}

#[test]
fn log_verbose_shows_day_span_and_ranges() {
    let tm = Tm::new();
    tm.run(&["start", "acme", "review", "--at", "09:00"]);
    tm.run(&["stop", "--at", "10:00"]);
    tm.run(&["start", "acme", "deploy", "--at", "10:00"]);
    tm.run(&["stop", "--at", "10:15"]);

    let log = tm.run(&["log", "--verbose"]);

    assert!(
        log.stdout
            .contains("(from 09:00 to 10:15, total time: 1h 15m)"),
        "{}",
        log.stdout
    );
    assert_eq!(
        task_lines(&log.stdout),
        vec![
            "  - acme / review (1h) 09:00 to 10:00",
            "  - acme / deploy (15m) 10:00 to 10:15",
        ]
    );
}

#[test]
fn stop_at_before_start_is_refused() {
    let tm = Tm::new();
    tm.run(&["start", "acme", "review", "--at", "10:00"]);

    let stop = tm.run(&["stop", "--at", "9:00"]);
    let status = tm.run(&["status"]);

    assert_eq!(stop.stdout, "");
    assert_eq!(
        stop.stderr,
        "Invalid time range: --at must be after the start time.\n"
    );
    assert!(
        status.stdout.starts_with("Tracking: acme / review"),
        "{}",
        status.stdout
    );
    tm.run(&["cancel"]);
}

#[test]
fn status_short_prints_running_task_with_elapsed_time() {
    let tm = Tm::new();
    tm.run(&["start", "acme", "review", "--at", "2026-01-01 09:00"]);

    let status = tm.run(&["status", "--short"]);

    assert_eq!(status.code, Some(0));
    assert!(
        status.stdout.starts_with("acme / review · "),
        "{}",
        status.stdout
    );
    assert!(status.stdout.ends_with("s\n"), "{}", status.stdout);
    assert_eq!(status.stdout.lines().count(), 1);
    tm.run(&["cancel"]);
}

#[test]
fn status_short_prints_nothing_and_fails_when_idle() {
    let tm = Tm::new();

    let status = tm.run(&["status", "--short"]);

    assert_eq!(status.code, Some(1));
    assert_eq!(status.stdout, "");
    assert_eq!(status.stderr, "");
}

#[test]
fn squash_merges_fragments_of_one_task() {
    let tm = Tm::new();
    tm.run(&["start", "acme", "review", "--at", "09:00"]);
    tm.run(&["stop", "--at", "09:30"]);
    tm.run(&["start", "acme", "review", "--at", "09:45"]);
    tm.run(&["stop", "--at", "10:15"]);

    tm.run(&["squash"]);
    let log = tm.run(&["log", "--verbose"]);

    assert_eq!(
        task_lines(&log.stdout),
        vec!["  - acme / review (1h 15m) 09:00 to 10:15"]
    );
}
