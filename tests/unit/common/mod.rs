//! Shared test helpers for `orderbook-rs` integration tests.
//!
//! Kept intentionally thin — add new sub-modules here as future proptest
//! issues (#57 byte-identical replay widening, #52 engine_seq monotonicity,
//! etc.) need shared machinery.

pub mod strategies;
