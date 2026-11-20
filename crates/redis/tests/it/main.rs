//! The Redis store's tests on a real server, built as one binary so that their files run side by
//! side. The metrics tests and the native-tls tests have binaries of their own.

#![expect(
    clippy::unwrap_used,
    reason = "tests and their helpers unwrap: a failure should panic"
)]

mod cluster;
mod sentinel;
mod store;
mod tls;
