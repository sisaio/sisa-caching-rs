//! Background jobs, and the host that runs and stops them.

mod worker_host;
mod worker_service;

pub use worker_host::*;
pub use worker_service::*;

/// The signal a [`WorkerHost`] gives its workers to stop.
pub use tokio_util::sync::CancellationToken;
