use std::{
    collections::hash_map::RandomState,
    fmt,
    hash::{BuildHasher, Hasher},
    sync::Arc,
};

use serde::{Deserialize, Serialize};

/// Names one node, so a node can skip the invalidations that it sent itself.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub struct NodeId(Arc<str>);

impl NodeId {
    /// An id from any string, such as a pod name. It must be unique to the process: a node skips
    /// every message that carries its own id.
    pub fn new(id: impl Into<Arc<str>>) -> Self {
        Self(id.into())
    }

    /// A fresh id from the process id and a per-call random number.
    pub fn generate() -> Self {
        let random = RandomState::new().build_hasher().finish();

        Self::new(format!("{}-{random:016x}", std::process::id()))
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl From<String> for NodeId {
    fn from(id: String) -> Self {
        Self::new(id)
    }
}

impl From<NodeId> for String {
    fn from(id: NodeId) -> Self {
        id.to_string()
    }
}
