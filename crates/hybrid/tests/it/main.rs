//! The tests that put a hybrid store's parts together, built as one binary so that their files run
//! side by side. The tests that install a meter provider, the native-tls tests and the chaos test
//! have binaries of their own.

#[path = "../common/mod.rs"]
mod common;

mod failures;
mod invalidation;
mod reads_and_writes;
mod redis;
mod sentinel;
mod tls;
