//! The worker that drops expired entries, which each store starts for itself.

mod purge_worker;

pub(crate) use purge_worker::PurgeWorker;
