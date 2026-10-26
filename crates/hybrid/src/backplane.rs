//! The transport contract. A write publishes one [`BackplaneMessage`]; the backplane worker on
//! every other node receives it and removes the named local copies.

use std::future::Future;

mod backplane_message;
mod node_id;
mod redis_backplane;

pub use backplane_message::{BackplaneMessage, Invalidation};
pub use node_id::NodeId;
pub use redis_backplane::{RedisBackplane, RedisSubscription};

/// Carries invalidations between nodes sharing a distributed store. It is only the transport: the
/// backplane worker decides when to subscribe again and what to do with each message.
pub trait Backplane: Send + Sync + 'static {
    /// What publishing and subscribing fail with.
    type Error: std::error::Error + Send + Sync + 'static;
    /// What `subscribe` returns.
    type Subscription: BackplaneSubscription<Error = Self::Error>;

    /// Sends the message to all subscribers, including this node's own.
    fn publish(
        &self,
        message: &BackplaneMessage,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;

    /// Opens a subscription that receives the messages published from now on.
    fn subscribe(&self) -> impl Future<Output = Result<Self::Subscription, Self::Error>> + Send;
}

/// Messages from one subscription, in order, with none missing.
pub trait BackplaneSubscription: Send + 'static {
    /// What `next` fails with.
    type Error: std::error::Error + Send + Sync + 'static;

    /// The next message. An error means messages may have been lost and this subscription is
    /// done; open a new one.
    fn next(&mut self) -> impl Future<Output = Result<BackplaneMessage, Self::Error>> + Send;
}
