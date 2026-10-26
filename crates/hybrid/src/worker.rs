//! The background job a hybrid deployment runs. Register it on a
//! [`WorkerHost`](sisa_caching_core::WorkerHost) next to the store it serves.

mod backplane_worker;

pub use backplane_worker::BackplaneWorker;
