use std::sync::Arc;
use std::sync::mpsc::Sender;
use std::time::Duration;
use tm::db::Database;
use tm::ipc::DaemonMessage;
use tokio::sync::Mutex;
use tokio::time::interval;

use crate::state::TrackingState;

pub async fn run(state: Arc<Mutex<TrackingState>>, label_tx: Sender<Option<String>>) {
    reconcile_from_db(&state).await;

    let mut ticker = interval(Duration::from_secs(1));

    loop {
        ticker.tick().await;
        let label = state.lock().await.label();
        let _ = label_tx.send(label);
    }
}

async fn reconcile_from_db(state: &Arc<Mutex<TrackingState>>) {
    let result = tokio::task::spawn_blocking(|| {
        let db = Database::open()?;
        db.get_active_entry()
    })
    .await;

    match result {
        Ok(Ok(Some(entry))) => {
            let mut s = state.lock().await;
            if matches!(*s, TrackingState::Idle) {
                s.apply(DaemonMessage::Started {
                    project: entry.project_name,
                    task: entry.task_name,
                    started_at: entry.started_at,
                });
            }
        }
        Ok(Ok(None)) => {}
        Ok(Err(e)) => eprintln!("tm-daemon: db reconcile error: {}", e),
        Err(e) => eprintln!("tm-daemon: spawn_blocking error: {}", e),
    }
}
