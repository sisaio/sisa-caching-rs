//! Settings for a hybrid deployment: its own TTLs, its two layers and the backplane worker.

mod backplane_worker_settings;
mod hybrid_cache_settings;
mod local_settings;

pub use backplane_worker_settings::BackplaneWorkerSettings;
pub use hybrid_cache_settings::HybridCacheSettings;
pub use local_settings::LocalSettings;
