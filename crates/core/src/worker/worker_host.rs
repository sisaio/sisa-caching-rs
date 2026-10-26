use tokio_util::task::TaskTracker;

use crate::worker::CancellationToken;

use super::worker_service::WorkerService;

/// Runs workers on the tokio runtime and stops them together.
#[derive(Debug)]
pub struct WorkerHost {
    shutdown: CancellationToken,
    tasks: TaskTracker,
}

impl WorkerHost {
    pub fn new() -> Self {
        Self {
            shutdown: CancellationToken::new(),
            tasks: TaskTracker::new(),
        }
    }

    /// Starts `service` on the current tokio runtime. Panics outside one, as `tokio::spawn` does.
    pub fn register<S>(&self, service: S)
    where
        S: WorkerService,
    {
        let name = service.name();
        let shutdown = self.shutdown.child_token();

        self.tasks.spawn(async move {
            tracing::info!(service = name, "background service started");

            // Logs the stop on every exit, a panic included.
            let _stopped = Stopped(name);

            tokio::select! {
                biased;

                _ = shutdown.cancelled() => {}
                _ = service.run(shutdown.clone()) => {}
            }
        });
    }

    /// Stops every worker and waits for them to finish.
    pub async fn shutdown(&self) {
        // Let the tracker finish once all tasks exit; cancellation stops registered services.
        self.tasks.close();

        // Notify all services to terminate.
        self.shutdown.cancel();

        // Wait for services to complete.
        self.tasks.wait().await;
    }
}

/// Dropping the host stops its workers without waiting; `shutdown` also waits for them to finish.
impl Drop for WorkerHost {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

struct Stopped(&'static str);

impl Drop for Stopped {
    fn drop(&mut self) {
        if std::thread::panicking() {
            tracing::error!(service = self.0, "background service panicked");
        } else {
            tracing::info!(service = self.0, "background service stopped");
        }
    }
}

impl Default for WorkerHost {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::{future::pending, time::Duration};

    use tokio::sync::oneshot;

    use super::*;

    /// Waits forever once started, and closes `stopped` when its future is dropped.
    struct WaitingWorker {
        started: oneshot::Sender<()>,
        stopped: oneshot::Sender<()>,
    }

    impl WorkerService for WaitingWorker {
        fn name(&self) -> &'static str {
            "waiting-worker"
        }

        async fn run(self, _shutdown: CancellationToken) {
            let _stopped = self.stopped;

            _ = self.started.send(());

            pending::<()>().await;
        }
    }

    /// Panics once started, dropping `ran` as it unwinds.
    struct PanickingWorker {
        ran: oneshot::Sender<()>,
    }

    impl WorkerService for PanickingWorker {
        fn name(&self) -> &'static str {
            "panicking-worker"
        }

        async fn run(self, _shutdown: CancellationToken) {
            let _ran = self.ran;

            panic!("the worker failed");
        }
    }

    async fn start_waiting_worker(host: &WorkerHost) -> oneshot::Receiver<()> {
        let (started, ready) = oneshot::channel();
        let (stopped, done) = oneshot::channel();

        host.register(WaitingWorker { started, stopped });
        ready.await.unwrap();

        done
    }

    #[tokio::test]
    async fn shutdown_stops_a_worker_and_waits_for_it() {
        let host = WorkerHost::new();
        let done = start_waiting_worker(&host).await;

        tokio::time::timeout(Duration::from_secs(1), host.shutdown())
            .await
            .unwrap();

        assert!(done.await.is_err(), "the worker's future was dropped");
    }

    #[tokio::test]
    async fn dropping_the_host_stops_its_workers() {
        let host = WorkerHost::new();
        let done = start_waiting_worker(&host).await;

        drop(host);

        let stopped = tokio::time::timeout(Duration::from_secs(1), done).await;

        assert!(stopped.unwrap().is_err(), "the worker's future was dropped");
    }

    #[tokio::test]
    async fn a_worker_that_panics_leaves_the_others_and_shutdown_alone() {
        let host = WorkerHost::new();
        let (ran, panicked) = oneshot::channel();
        let mut done = start_waiting_worker(&host).await;

        host.register(PanickingWorker { ran });

        assert!(panicked.await.is_err(), "the worker panicked");
        assert!(done.try_recv().is_err(), "the other worker still runs");

        tokio::time::timeout(Duration::from_secs(1), host.shutdown())
            .await
            .unwrap();
        assert!(done.await.is_err());
    }
}
