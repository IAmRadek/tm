use chrono::{DateTime, Utc};
use tm::ipc::DaemonMessage;

pub enum TrackingState {
    Idle,
    Active {
        project: String,
        task: String,
        started_at: DateTime<Utc>,
    },
}

impl TrackingState {
    pub fn apply(&mut self, msg: DaemonMessage) {
        *self = match msg {
            DaemonMessage::Started {
                project,
                task,
                started_at,
            }
            | DaemonMessage::Continued {
                project,
                task,
                started_at,
            } => TrackingState::Active {
                project,
                task,
                started_at,
            },
            DaemonMessage::Stopped | DaemonMessage::Cancelled => TrackingState::Idle,
        };
    }

    /// Returns the menu bar label string, or None when idle.
    pub fn label(&self) -> Option<String> {
        let TrackingState::Active {
            project,
            task,
            started_at,
        } = self
        else {
            return None;
        };
        let elapsed = Utc::now()
            .signed_duration_since(*started_at)
            .num_seconds()
            .max(0);
        let h = elapsed / 3600;
        let m = (elapsed % 3600) / 60;
        let s = elapsed % 60;

        let duration = format!("{}:{:02}:{:02}", h, m, s);

        Some(format!("{} · {} · {}", project, task, duration))
    }
}
