//! The transport between nodes. A write publishes one invalidation over Redis pub/sub; the
//! backplane worker on every other node receives it and removes the named local copies.

mod backplane_message;
mod node_id;
mod redis_backplane;

pub(crate) use backplane_message::{BackplaneMessage, Invalidation};
pub use node_id::NodeId;
pub use redis_backplane::RedisBackplane;
