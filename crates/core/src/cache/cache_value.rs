use std::any::Any;

use serde::{Serialize, de::DeserializeOwned};

/// Any value a store can hold. One store holds values of many types; the type is named on each
/// call (`get::<User>`, `set(&key, user, None)`), not on the store.
///
/// `Any` lets an in-memory layer hand a value back as the type it was stored as, and the serde
/// bounds let a distributed layer carry it as bytes. Every read clones the value, so keep large
/// values cheap to clone. A value's `Drop` must not wait on a hybrid store call: a hybrid store can
/// free a value while an invalidation waits behind it.
pub trait CacheValue: Any + Serialize + DeserializeOwned + Clone + Send + Sync + 'static {}

impl<T> CacheValue for T where T: Any + Serialize + DeserializeOwned + Clone + Send + Sync + 'static {}
