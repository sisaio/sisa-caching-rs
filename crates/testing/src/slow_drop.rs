//! A value whose drop waits for the test.

use std::{
    sync::{Mutex, mpsc},
    time::Duration,
};

use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

/// A value whose drop says on its oneshot that it started, then waits up to five seconds until the
/// test sends on its gate, or drops the gate's sender. A store that frees it on the test's own
/// thread holds the test up that long, and the test's send then fails. Its clones, which reads
/// make, drop at once.
#[derive(Serialize, Deserialize)]
pub struct SlowDrop {
    #[serde(skip)]
    gate: Option<(oneshot::Sender<()>, Mutex<mpsc::Receiver<()>>)>,
}

impl SlowDrop {
    /// The value, the receiver that hears its drop start, and the sender that lets the drop finish.
    pub fn new() -> (Self, oneshot::Receiver<()>, mpsc::Sender<()>) {
        let (dropping, dropped) = oneshot::channel();
        let (release, gate) = mpsc::channel();
        let value = Self {
            gate: Some((dropping, Mutex::new(gate))),
        };

        (value, dropped, release)
    }
}

impl Clone for SlowDrop {
    fn clone(&self) -> Self {
        Self { gate: None }
    }
}

impl Drop for SlowDrop {
    fn drop(&mut self) {
        if let Some((dropping, gate)) = self.gate.take() {
            _ = dropping.send(());
            _ = gate.lock().unwrap().recv_timeout(Duration::from_secs(5));
        }
    }
}
