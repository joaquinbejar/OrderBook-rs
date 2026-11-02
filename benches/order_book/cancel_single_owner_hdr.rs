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

// cancel_single_owner_hdr — pre-loaded book, every order owned by ONE
// user, cancelled in random order.
//
// `cancel_only_hdr` spreads its orders over `SEED_OWNERS` owners so each
// owner's `user_orders` list stays short. This scenario is the opposite
// end: a single market maker (or anonymous flow, which shares
// `Hash32::zero()`) with `PRELOAD_OPS` resting orders, so each cancel
// pays for the `user_orders` removal at that list length.
//
// Methodology: same as `cancel_only_hdr` (issue #258), `BATCH` cancels
// per `Instant` pair, recorded as the per-op average. The cancel order is
// a seeded shuffle drawn before the clock starts.

#[path = "hdr_common.rs"]
mod common;

use common::{Rng, new_histogram, persist, record_batch, report, seed_resting};
use pricelevel::Id;

const SCENARIO: &str = "cancel_single_owner";
const PRELOAD_OPS: u64 = 100_000;
const BATCH: u64 = 32;
const SEED: u64 = 0xA5A5_A5A5_A5A5_A5A5;

fn main() {
    let book = common::fresh_book();
    let mut rng = Rng::new(SEED);
    let mut hist = new_histogram();

    // One owner: every resting order lands in the same `user_orders` entry.
    seed_resting(&book, &mut rng, 1, PRELOAD_OPS, 1);

    // Fisher-Yates shuffle of ids 1..=PRELOAD_OPS, outside the clock.
    let mut order: Vec<u64> = (1..=PRELOAD_OPS).collect();
    for i in (1..order.len()).rev() {
        let j = rng.range(0, i as u64) as usize;
        order.swap(i, j);
    }

    let mut results = Vec::with_capacity(BATCH as usize);
    for chunk in order.chunks(BATCH as usize) {
        results.clear();
        record_batch(&mut hist, chunk.len() as u64, |j| {
            results.push(book.cancel_order(Id::from_u64(chunk[j as usize])));
        });
        assert!(
            results.iter().all(|r| matches!(r, Ok(Some(_)))),
            "every cancel hits a resting order"
        );
    }
    assert!(
        book.get_all_orders().is_empty(),
        "every order was cancelled"
    );

    report(SCENARIO, &hist);
    persist(SCENARIO, &hist).expect("persist hgrm");
}
