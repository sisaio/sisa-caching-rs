//! What the benchmarks store: a small user record under `user:{id}`.

use serde::{Deserialize, Serialize};
use sisa_caching_core::cache::CacheKey;

#[derive(Clone, Serialize, Deserialize)]
pub struct User {
    pub id: u64,
    pub name: String,
    pub email: String,
}

pub fn user(id: u64) -> User {
    User {
        id,
        name: "Ada Lovelace".to_owned(),
        email: "ada@example.com".to_owned(),
    }
}

pub fn key(id: u64) -> CacheKey {
    CacheKey::new("user", id).unwrap()
}
