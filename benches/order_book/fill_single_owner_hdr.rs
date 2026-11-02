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

// fill_single_owner_hdr — pre-loaded ask side, every maker owned by ONE
// user, consumed one maker per market order.
//
// Each one-lot market buy fully fills the best one-lot ask, so every
// timed op runs the filled-maker cleanup, including the maker's removal
// from its owner's `user_orders` entry. With a single owner that entry
// holds every resting maker. Ask prices are random, so the filled maker
// sits at a random position in the owner's admission-ordered list.
//
// Methodology: one `Instant` pair per market order (`record`); a fill is
// well above the host's clock tick, so the tail stays op-level.

#[path = "hdr_common.rs"]
mod common;

use common::{Rng, new_histogram, owner, persist, record, report};
use pricelevel::{Id, Side, TimeInForce};

const SCENARIO: &str = "fill_single_owner";
const PRELOAD_OPS: u64 = 100_000;
const SEED: u64 = 0xA5A5_A5A5_A5A5_A5A5;

fn main() {
    let book = common::fresh_book();
    let mut rng = Rng::new(SEED);
    let mut hist = new_histogram();
    let maker = owner(0xAA);
    let taker = owner(0xBB);

    for id in 1..=PRELOAD_OPS {
        book.add_limit_order_with_user(
            Id::from_u64(id),
            1_001 + rng.range(0, 999) as u128,
            1,
            Side::Sell,
            TimeInForce::Gtc,
            maker,
            None,
        )
        .expect("seed ask rests");
    }

    for id in PRELOAD_OPS + 1..=2 * PRELOAD_OPS {
        let result = record(&mut hist, || {
            book.submit_market_order_with_user(Id::from_u64(id), 1, Side::Buy, taker)
        });
        assert!(result.is_ok(), "every market order fills one maker");
    }
    assert!(book.get_all_orders().is_empty(), "every maker was filled");

    report(SCENARIO, &hist);
    persist(SCENARIO, &hist).expect("persist hgrm");
}
