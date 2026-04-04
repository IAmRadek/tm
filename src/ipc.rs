use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::PathBuf;

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "event")]
pub enum DaemonMessage {
    Started {
        project: String,
        task: String,
        started_at: DateTime<Utc>,
    },
    Stopped,
    Cancelled,
    Continued {
        project: String,
        task: String,
        started_at: DateTime<Utc>,
    },
}

pub fn socket_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home).join(".config/tm/tm.sock")
}

/// Fire-and-forget: send a message to the daemon if it is running.
/// Errors are silently ignored — the CLI must not fail if the daemon is down.
pub fn notify_daemon(msg: &DaemonMessage) {
    let path = socket_path();
    let Ok(mut stream) = std::os::unix::net::UnixStream::connect(&path) else {
        return;
    };
    if let Ok(json) = serde_json::to_string(msg) {
        let _ = stream.write_all(json.as_bytes());
        let _ = stream.write_all(b"\n");
    }
}
