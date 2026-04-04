mod menubar;
mod socket;
mod state;
mod timer;

use std::sync::Arc;
use tokio::sync::Mutex;

use state::TrackingState;

fn main() {
    let (label_tx, label_rx) = std::sync::mpsc::channel::<Option<String>>();

    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("failed to build tokio runtime");

        rt.block_on(async move {
            let state = Arc::new(Mutex::new(TrackingState::Idle));
            tokio::join!(
                socket::run(state.clone()),
                timer::run(state.clone(), label_tx),
            );
        });
    });

    menubar::run(label_rx);
}
