use std::future::Future;

use crate::worker::CancellationToken;

/// A background job that a [`WorkerHost`](crate::WorkerHost) runs until told to stop.
pub trait WorkerService: Send + 'static {
    /// The name the host logs when the worker starts, stops or panics.
    fn name(&self) -> &'static str;

    /// The host drops this future on shutdown, including while it is waiting on I/O. Use `Drop`
    /// for cleanup; `shutdown` distinguishes an intentional stop from an unexpected exit.
    fn run(self, shutdown: CancellationToken) -> impl Future<Output = ()> + Send;
}
