use std::sync::Arc;
use tokio::io::AsyncBufReadExt;
use tokio::net::UnixListener;
use tokio::sync::Mutex;
use tm::ipc::{socket_path, DaemonMessage};

use crate::state::TrackingState;

pub async fn run(state: Arc<Mutex<TrackingState>>) {
    let path = socket_path();

    // Remove stale socket file from a previous run.
    let _ = std::fs::remove_file(&path);

    let listener = match UnixListener::bind(&path) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("tm-daemon: failed to bind socket at {}: {}", path.display(), e);
            return;
        }
    };

    loop {
        let (stream, _) = match listener.accept().await {
            Ok(conn) => conn,
            Err(e) => {
                eprintln!("tm-daemon: accept error: {}", e);
                continue;
            }
        };

        let state = state.clone();
        tokio::spawn(async move {
            let mut lines = tokio::io::BufReader::new(stream).lines();
            if let Ok(Some(line)) = lines.next_line().await {
                match serde_json::from_str::<DaemonMessage>(&line) {
                    Ok(msg) => state.lock().await.apply(msg),
                    Err(e) => eprintln!("tm-daemon: bad message: {}", e),
                }
            }
        });
    }
}
