//! The tests that put a hybrid store's parts together, built as one binary so that their files run
//! side by side. The tests that install a meter provider, the native-tls tests and the chaos test
//! have binaries of their own.

#![expect(
    clippy::unwrap_used,
    reason = "tests and their helpers unwrap: a failure should panic"
)]

#[path = "../common/mod.rs"]
mod common;

mod cluster;
mod failures;
mod invalidation;
mod reads_and_writes;
mod redis;
mod sentinel;
mod tls;
