//! The background task each store runs for itself.

use std::{
    future::Future,
    sync::{Arc, OnceLock},
};

use tokio::task::AbortHandle;

use crate::cache::KeyPrefix;

/// The background task a store runs for itself, such as its purge, shared by the store's clones.
/// It starts once, on the store's first call inside a tokio runtime, and runs on that runtime
/// until the store's last clone drops, or for good on [`stop`](Self::stop). So that runtime must
/// outlive the store: a task whose runtime shut down is gone, and doesn't start again.
///
/// A task that holds a copy of its store gives that copy [`none`](Self::none), so the task
/// doesn't keep itself running.
#[derive(Clone, Debug)]
pub struct Worker(Option<Arc<OnceLock<Option<Running>>>>);

/// The running task, which is aborted when the store's last clone drops this.
#[derive(Debug)]
struct Running(AbortHandle);

impl Drop for Running {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl Worker {
    pub fn new() -> Self {
        Self(Some(Arc::default()))
    }

    /// Never starts anything, for the copy of a store that its own task holds.
    pub fn none() -> Self {
        Self(None)
    }

    /// Spawns the future `task` returns on the current tokio runtime, the first time this runs
    /// inside one. Later calls, and calls outside a runtime, do nothing. The task's start, stop
    /// and panic are logged under `name` and the name of `store`.
    pub fn start<F>(&self, name: &'static str, store: &KeyPrefix, task: impl FnOnce() -> F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let Some(running) = &self.0 else {
            return;
        };

        // Once started, this is one load. `Handle::try_current` clones an `Arc` that every thread
        // of the runtime shares, so it comes second.
        if running.get().is_some() || tokio::runtime::Handle::try_current().is_err() {
            return;
        }

        running.get_or_init(|| {
            Some(Running(
                tokio::spawn(logged(name, store.clone(), task())).abort_handle(),
            ))
        });
    }

    /// Aborts the task if it runs, and keeps it from starting from then on, for every clone.
    pub fn stop(&self) {
        let Some(running) = &self.0 else {
            return;
        };

        _ = running.set(None);

        if let Some(Some(Running(task))) = running.get() {
            task.abort();
        }
    }
}

impl Default for Worker {
    fn default() -> Self {
        Self::new()
    }
}

/// Runs `task`, and logs its start, its stop and a panic under `name` and `store`.
async fn logged(name: &'static str, store: KeyPrefix, task: impl Future<Output = ()>) {
    tracing::info!(worker = name, %store, "worker started");

    // Logs the stop on every exit: the task ending, the store's last clone dropping, or a panic.
    let _stopped = Stopped(name, store);

    task.await;
}

struct Stopped(&'static str, KeyPrefix);

impl Drop for Stopped {
    fn drop(&mut self) {
        let Self(name, store) = self;

        if std::thread::panicking() {
            tracing::error!(worker = *name, %store, "worker panicked");
        } else {
            tracing::info!(worker = *name, %store, "worker stopped");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        future::pending,
        sync::atomic::{AtomicUsize, Ordering},
        time::Duration,
    };

    use tokio::sync::oneshot;

    use super::*;

    fn store() -> KeyPrefix {
        KeyPrefix::new("app").unwrap()
    }

    #[tokio::test]
    async fn the_task_starts_once_and_stops_when_the_last_clone_drops() {
        let worker = Worker::new();
        let clone = worker.clone();
        let starts = AtomicUsize::new(0);
        let (stopped, done) = oneshot::channel::<()>();
        let mut stopped = Some(stopped);

        for worker in [&worker, &clone, &worker] {
            worker.start("waiting", &store(), || {
                starts.fetch_add(1, Ordering::SeqCst);

                let stopped = stopped.take().unwrap();

                // Waits forever, and closes `stopped` when its future is dropped.
                async move {
                    let _stopped = stopped;

                    pending::<()>().await;
                }
            });
        }

        assert_eq!(starts.load(Ordering::SeqCst), 1);

        drop(worker);
        drop(clone);

        let stopped = tokio::time::timeout(Duration::from_secs(1), done).await;

        assert!(stopped.unwrap().is_err(), "the task's future was dropped");
    }

    #[tokio::test]
    async fn a_stopped_task_ends_and_never_starts_again() {
        let worker = Worker::new();
        let clone = worker.clone();
        let (stopped, done) = oneshot::channel::<()>();

        worker.start("waiting", &store(), || async move {
            let _stopped = stopped;

            pending::<()>().await;
        });
        clone.stop();

        let stopped = tokio::time::timeout(Duration::from_secs(1), done).await;

        assert!(stopped.unwrap().is_err(), "the task's future was dropped");
        worker.start("waiting", &store(), || async {
            unreachable!("it never starts again")
        });
    }

    #[tokio::test]
    async fn a_task_stopped_before_it_starts_never_does() {
        let worker = Worker::new();

        worker.stop();
        worker.start("waiting", &store(), || async {
            unreachable!("it never starts")
        });
    }

    #[test]
    fn outside_a_runtime_nothing_starts_until_a_call_inside_one() {
        let worker = Worker::new();
        let starts = AtomicUsize::new(0);
        let task = || {
            starts.fetch_add(1, Ordering::SeqCst);
            async {}
        };

        worker.start("counting", &store(), task);
        assert_eq!(starts.load(Ordering::SeqCst), 0);

        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(async { worker.start("counting", &store(), task) });
        assert_eq!(starts.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn none_never_starts() {
        let starts = AtomicUsize::new(0);

        Worker::none().start("counting", &store(), || {
            starts.fetch_add(1, Ordering::SeqCst);
            async {}
        });

        assert_eq!(starts.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn a_task_that_panics_leaves_the_store_alone() {
        let worker = Worker::new();
        let (ran, panicked) = oneshot::channel::<()>();

        worker.start("panicking", &store(), || async move {
            let _ran = ran;

            panic!("the task failed");
        });

        assert!(panicked.await.is_err(), "the task panicked");
        worker.start("panicking", &store(), || async {
            unreachable!("it started once")
        });
    }
}
