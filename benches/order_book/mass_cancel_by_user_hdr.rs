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

// mass_cancel_by_user_hdr — dense book owned by ONE user, then a
// `cancel_orders_by_user` burst for that user. One observation per burst
// ("ns to drain N orders"), like `mass_cancel_burst_hdr`.
//
// The by-user cancel walks the owner's `user_orders` entry and cancels
// each id through the single-order path, so every cancelled order pays a
// `user_orders` removal at that entry's length.

#[path = "hdr_common.rs"]
mod common;

use common::{Rng, new_histogram, persist, record, report, seed_resting};
use pricelevel::Hash32;

const SCENARIO: &str = "mass_cancel_by_user";
const ORDERS_PER_BURST: u64 = 10_000;
const MEASURED_BURSTS: u64 = 100;
const SEED: u64 = 0xA5A5_A5A5_A5A5_A5A5;

fn main() {
    let book = common::fresh_book();
    let mut rng = Rng::new(SEED);
    let mut hist = new_histogram();
    let mut next_id: u64 = 1;
    // `seed_resting` with one owner draws this owner for every order.
    let mut bytes = [0u8; 32];
    bytes[2] = 0x5A;
    let user = Hash32::new(bytes);

    for _ in 0..MEASURED_BURSTS {
        // Re-load the book with ORDERS_PER_BURST resting orders, all
        // owned by `user`. Not measured.
        seed_resting(&book, &mut rng, next_id, ORDERS_PER_BURST, 1);
        next_id += ORDERS_PER_BURST;

        let result = record(&mut hist, || book.cancel_orders_by_user(user));
        assert_eq!(
            result.cancelled_count(),
            ORDERS_PER_BURST as usize,
            "the burst cancels every order"
        );
        drop(result);
        assert!(
            book.get_all_orders().is_empty(),
            "the burst empties the book"
        );
    }

    report(SCENARIO, &hist);
    persist(SCENARIO, &hist).expect("persist hgrm");
}
