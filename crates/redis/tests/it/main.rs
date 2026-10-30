//! The Redis store's tests on a real server, built as one binary so that their files run side by
//! side. The metrics tests and the native-tls tests have binaries of their own.

mod cluster;
mod sentinel;
mod store;
mod tls;
