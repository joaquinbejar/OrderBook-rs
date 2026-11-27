// This crate root is entirely bench code, not production (issue #242's
// Production Panic Policy gate, `[lints.clippy]` in `Cargo.toml`, is
// package-wide and would otherwise apply here too). Bench fixtures freely
// `.unwrap()` / `.expect()` setup, index fixed-size scratch buffers and do
// raw arithmetic on sample sizes; none of that reaches `src/`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap
)]

use criterion::{criterion_group, criterion_main};

mod concurrent;
mod order_book;
mod serialization;
mod simple;

use concurrent::register_benchmarks as register_concurrent_benchmarks;
use order_book::register_benchmarks as register_order_book_benchmarks;
use serialization::register_benchmarks as register_serialization_benchmarks;
use simple::basic::benchmark_data;

// Define the benchmark groups
criterion_group!(
    benches,
    benchmark_data,
    register_order_book_benchmarks,
    register_concurrent_benchmarks,
    register_serialization_benchmarks,
);

// #286: pending-stop workloads run last, after every shared row.
#[cfg(feature = "special_orders")]
criterion_group!(
    pending_stop_benches,
    order_book::pending_stops::register_benchmarks
);

// #328: pegged re-price sweeps, after the pending-stop rows.
#[cfg(feature = "special_orders")]
criterion_group!(
    pegged_reprice_benches,
    order_book::pegged_reprice::register_benchmarks
);

#[cfg(not(feature = "special_orders"))]
criterion_main!(benches);
#[cfg(feature = "special_orders")]
criterion_main!(benches, pending_stop_benches, pegged_reprice_benches);
