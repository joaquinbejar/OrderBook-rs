// This crate root is entirely bench code, not production (issue #242's
// Production Panic Policy gate, `[lints.clippy]` in `Cargo.toml`, is
// package-wide and would otherwise apply here too). Bench fixtures freely
// `.unwrap()` / `.expect()` setup and do raw arithmetic on sample sizes;
// none of that reaches `src/`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]

// best_price_read_hdr: the cost of one `best_bid()` + `best_ask()` pair
// (issue #309).
//
// Two scenarios on a 10 000-order non-crossing book (bids on 900..=999,
// asks on 1 001..=1 100):
//
// - `best_price_read`: a quiet book, nothing mutates it while the reader
//   loops. This is the read-heavy best case for any best-price cache.
// - `best_price_read_1w`: the same loop while one writer thread adds an
//   ask at 1 000 (in front of the best ask, crossing nothing) and cancels
//   it, as fast as it can. Every write moves the best ask, so this is the
//   worst case for a cache that must never serve a stale price.
//
// Methodology (issue #258): one pair runs in the tens of ns or less,
// at or below the host clock tick, so `record_batch` times `BATCH` pairs
// per `Instant` pair and records the per-pair average. Only the public
// API is used, so the bench builds against any ref that has it.

#[path = "hdr_common.rs"]
mod common;

use common::{Rng, SEED_OWNERS, new_histogram, persist, record_batch, report, seed_resting};
use pricelevel::{Id, Side, TimeInForce};
use std::hint::black_box;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier};

const BOOK_ORDERS: u64 = 10_000;
const SAMPLES: u64 = 200_000;
const BATCH: u64 = 32;
const SEED: u64 = 0xA5A5_A5A5_A5A5_A5A5;
const WRITER_PRICE: u128 = 1_000;
const WRITER_FIRST_ID: u64 = 1_000_000_000;

fn seeded_book() -> orderbook_rs::OrderBook<()> {
    let book = common::fresh_book();
    let mut rng = Rng::new(SEED);
    seed_resting(&book, &mut rng, 1, BOOK_ORDERS, SEED_OWNERS);
    book
}

fn read_loop(book: &orderbook_rs::OrderBook<()>) -> hdrhistogram::Histogram<u64> {
    let mut hist = new_histogram();
    for _ in 0..SAMPLES {
        record_batch(&mut hist, BATCH, |_| {
            black_box(book.best_bid());
            black_box(book.best_ask());
        });
    }
    hist
}

fn quiet() {
    let book = seeded_book();
    let hist = read_loop(&book);
    report("best_price_read", &hist);
    persist("best_price_read", &hist).expect("persist hgrm");
}

fn one_writer() {
    let book = Arc::new(seeded_book());
    let stop = Arc::new(AtomicBool::new(false));
    let start = Arc::new(Barrier::new(2));
    let writer = {
        let book = Arc::clone(&book);
        let stop = Arc::clone(&stop);
        let start = Arc::clone(&start);
        std::thread::spawn(move || {
            start.wait();
            let mut id = WRITER_FIRST_ID;
            let mut writes = 0u64;
            while !stop.load(Ordering::Relaxed) {
                let order = Id::from_u64(id);
                id += 1;
                book.add_limit_order(order, WRITER_PRICE, 1, Side::Sell, TimeInForce::Gtc, None)
                    .expect("writer add");
                book.cancel_order(order).expect("writer cancel");
                writes += 2;
            }
            writes
        })
    };
    start.wait();
    let hist = read_loop(&book);
    stop.store(true, Ordering::Relaxed);
    let writes = writer.join().expect("writer thread");
    assert!(writes > 0, "the writer made progress");
    report("best_price_read_1w", &hist);
    eprintln!("writer ops   : {writes}");
    persist("best_price_read_1w", &hist).expect("persist hgrm");
}

fn main() {
    quiet();
    one_writer();
}
