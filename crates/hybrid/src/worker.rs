//! The background job each hybrid store runs for itself.

mod backplane_worker;

pub(crate) use backplane_worker::BackplaneWorker;
