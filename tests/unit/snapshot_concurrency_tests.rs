//! `create_snapshot` against concurrent cancels and sweeps (#313).
//!
//! `create_snapshot` runs on the shared side of the submit gate, as cancels
//! and non-FOK sweeps do, so it can read the book halfway through one of
//! them. Before #313 two windows showed up in the snapshot:
//!
//! - A level whose last order had left but which was still in the skip
//!   list was returned at zero quantity.
//! - A level removed between the key collection and the per-level reads
//!   was missing, and with `depth` 1 nothing took its place.
//!
//! The ask at `RESTING_ASK` rests untouched for the whole run, so a
//! snapshot whose best ask is not a level with quantity above zero is not
//! a state the book passed through.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use orderbook_rs::{Id, OrderBook, Side, TimeInForce};

const RESTING_ASK: u128 = 9882;
const BETTER_ASK: u128 = 9880;
const NEXT_ASK: u128 = 9881;
const RUN_FOR: Duration = Duration::from_millis(300);

/// What one reader saw.
#[derive(Debug, Default)]
struct Seen {
    reads: u64,
    /// `best_ask()` was `Some((price, 0))`.
    zero_size_best_ask: u64,
    /// `best_ask()` was `None`.
    no_best_ask: u64,
    /// Every level in the snapshot was at zero quantity.
    all_levels_zero: u64,
}

/// Loops `create_snapshot(depth)` on another thread while `write` runs for
/// `RUN_FOR`, and returns what the reader saw and how many writes ran.
fn race(
    book: Arc<OrderBook<()>>,
    depth: usize,
    mut write: impl FnMut(&OrderBook<()>),
) -> (Seen, u64) {
    let stop = Arc::new(AtomicBool::new(false));
    let reader = {
        let book = Arc::clone(&book);
        let stop = Arc::clone(&stop);
        thread::spawn(move || {
            let mut seen = Seen::default();
            while !stop.load(Ordering::Relaxed) {
                let snapshot = book.create_snapshot(depth).expect("snapshot");
                seen.reads += 1;
                match snapshot.best_ask() {
                    Some((_, 0)) => seen.zero_size_best_ask += 1,
                    None => seen.no_best_ask += 1,
                    Some(_) => {}
                }
                if !snapshot.asks.is_empty()
                    && snapshot
                        .asks
                        .iter()
                        .all(|level| level.visible_quantity().as_u64() == 0)
                {
                    seen.all_levels_zero += 1;
                }
            }
            seen
        })
    };

    let started = Instant::now();
    let mut writes: u64 = 0;
    while started.elapsed() < RUN_FOR {
        write(&book);
        writes += 1;
    }
    stop.store(true, Ordering::Relaxed);
    (reader.join().expect("reader"), writes)
}

fn book_with_resting_ask() -> Arc<OrderBook<()>> {
    let book = Arc::new(OrderBook::<()>::new("TEST"));
    book.add_limit_order(
        Id::from_u64(1),
        RESTING_ASK,
        10,
        Side::Sell,
        TimeInForce::Gtc,
        None,
    )
    .expect("resting ask");
    book
}

#[test]
fn snapshot_concurrency_best_ask_never_empty_or_zero_while_better_ask_cancelled() {
    let next_id = AtomicU64::new(2);
    let (seen, writes) = race(book_with_resting_ask(), 1, |book| {
        let id = Id::from_u64(next_id.fetch_add(1, Ordering::Relaxed));
        book.add_limit_order(id, BETTER_ASK, 1, Side::Sell, TimeInForce::Gtc, None)
            .expect("better ask");
        book.cancel_order(id).expect("cancel better ask");
    });

    assert!(
        seen.zero_size_best_ask == 0 && seen.no_best_ask == 0,
        "over {writes} add+cancel pairs, create_snapshot(1).best_ask() was Some((price, 0)) \
         {} time(s) and None {} time(s) in {} reads, while the ask at {RESTING_ASK} rested \
         throughout",
        seen.zero_size_best_ask,
        seen.no_best_ask,
        seen.reads,
    );
}

#[test]
fn snapshot_concurrency_never_lists_only_emptied_levels_while_sweep_runs() {
    let next_id = AtomicU64::new(2);
    let (seen, writes) = race(book_with_resting_ask(), 2, |book| {
        for price in [BETTER_ASK, NEXT_ASK] {
            let id = Id::from_u64(next_id.fetch_add(1, Ordering::Relaxed));
            book.add_limit_order(id, price, 1, Side::Sell, TimeInForce::Gtc, None)
                .expect("ask in front of the resting one");
        }
        let id = Id::from_u64(next_id.fetch_add(1, Ordering::Relaxed));
        book.submit_market_order(id, 2, Side::Buy)
            .expect("sweep the two front asks");
    });

    assert!(
        seen.all_levels_zero == 0 && seen.zero_size_best_ask == 0 && seen.no_best_ask == 0,
        "over {writes} sweeps of the two front asks, every level of create_snapshot(2) was \
         at zero quantity {} time(s) in {} reads (best_ask() Some((price, 0)) {} time(s), \
         None {} time(s)), while the ask at {RESTING_ASK} rested throughout",
        seen.all_levels_zero,
        seen.reads,
        seen.zero_size_best_ask,
        seen.no_best_ask,
    );
}
