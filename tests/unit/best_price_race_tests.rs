//! `best_bid()` / `best_ask()` against a concurrent reader (#309).
//!
//! Before #309 a `best_ask()` that raced a write could leave the
//! best-price cache holding the price from before the write, marked
//! valid, until the next mutation of the book; after a cancel that price
//! could belong to a level that no longer existed. The post-only and
//! headroom prechecks read the same value.
//!
//! One writer moves the best ask between two prices (add a better ask,
//! then cancel it) while a reader thread loops `best_ask()`. After each
//! write the writer compares `best_ask()` with `create_snapshot(1)`, which
//! walks the levels. Only the writer mutates the book, so the two can
//! differ only when `best_ask()` serves a stale value.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

use orderbook_rs::{Id, OrderBook, OrderBookError, Side, TimeInForce};

const RESTING_ASK: u128 = 9882;
const BETTER_ASK: u128 = 9880;
/// Below the resting ask, above the better ask: crosses only a stale one.
const POST_ONLY_BID: u128 = 9881;
const RUN_FOR: Duration = Duration::from_millis(300);

fn snapshot_best_ask(book: &OrderBook<()>) -> Option<u128> {
    book.create_snapshot(1)
        .ok()
        .and_then(|snapshot| snapshot.best_ask())
        .map(|(price, _)| price)
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

/// Spawns a reader that loops `best_ask()` until `stop`; it starts on
/// `start` together with the writer and returns how many reads it made.
fn spawn_reader(
    book: &Arc<OrderBook<()>>,
    stop: &Arc<AtomicBool>,
    start: &Arc<Barrier>,
) -> thread::JoinHandle<u64> {
    let (book, stop, start) = (Arc::clone(book), Arc::clone(stop), Arc::clone(start));
    thread::spawn(move || {
        start.wait();
        let mut reads: u64 = 0;
        while !stop.load(Ordering::Relaxed) {
            std::hint::black_box(book.best_ask());
            reads += 1;
        }
        reads
    })
}

#[test]
fn best_ask_matches_the_levels_after_every_write() {
    let book = book_with_resting_ask();
    let stop = Arc::new(AtomicBool::new(false));
    let start = Arc::new(Barrier::new(2));
    let reader = spawn_reader(&book, &stop, &start);

    let mut writes: u64 = 0;
    let mut stale_after_add: u64 = 0;
    let mut stale_after_cancel: u64 = 0;
    let mut first_stale: Option<(&str, Option<u128>, Option<u128>)> = None;
    let mut next_id: u64 = 2;
    start.wait();
    let started = Instant::now();
    while started.elapsed() < RUN_FOR {
        let id = Id::from_u64(next_id);
        next_id += 1;

        book.add_limit_order(id, BETTER_ASK, 1, Side::Sell, TimeInForce::Gtc, None)
            .expect("better ask");
        writes += 1;
        let (served, levels) = (book.best_ask(), snapshot_best_ask(&book));
        if served != levels {
            stale_after_add += 1;
            first_stale.get_or_insert(("add", served, levels));
        }

        book.cancel_order(id).expect("cancel better ask");
        writes += 1;
        let (served, levels) = (book.best_ask(), snapshot_best_ask(&book));
        if served != levels {
            stale_after_cancel += 1;
            first_stale.get_or_insert(("cancel", served, levels));
        }
    }

    stop.store(true, Ordering::Relaxed);
    let reads = reader.join().expect("reader");
    let (served, levels) = (book.best_ask(), snapshot_best_ask(&book));

    assert!(reads > 0, "the reader must race the writer at least once");
    assert_eq!(
        stale_after_add, 0,
        "best_ask() was stale after {stale_after_add} add(s) over {writes} writes and \
         {reads} reads; first: {first_stale:?}"
    );
    assert_eq!(
        stale_after_cancel, 0,
        "best_ask() was stale after {stale_after_cancel} cancel(s) over {writes} writes and \
         {reads} reads; first: {first_stale:?}"
    );
    assert_eq!(served, levels, "best_ask() disagrees with the levels at rest");
    assert_eq!(served, Some(RESTING_ASK));
}

#[test]
fn post_only_below_the_real_best_ask_is_never_refused_under_a_racing_reader() {
    let book = book_with_resting_ask();
    let stop = Arc::new(AtomicBool::new(false));
    let start = Arc::new(Barrier::new(2));
    let reader = spawn_reader(&book, &stop, &start);

    let mut attempts: u64 = 0;
    let mut refused: u64 = 0;
    let mut next_id: u64 = 2;
    start.wait();
    let started = Instant::now();
    while started.elapsed() < RUN_FOR {
        let better = Id::from_u64(next_id);
        let bid = Id::from_u64(next_id + 1);
        next_id += 2;

        book.add_limit_order(better, BETTER_ASK, 1, Side::Sell, TimeInForce::Gtc, None)
            .expect("better ask");
        book.cancel_order(better).expect("cancel better ask");

        // The real best ask is RESTING_ASK again, so a post-only bid at
        // POST_ONLY_BID crosses nothing and must rest.
        attempts += 1;
        match book.add_post_only_order(bid, POST_ONLY_BID, 1, Side::Buy, TimeInForce::Gtc, None) {
            Ok(_) => {
                book.cancel_order(bid).expect("cancel post-only bid");
            }
            Err(OrderBookError::PriceCrossing { .. }) => refused += 1,
            Err(other) => panic!("unexpected post-only error: {other}"),
        }
    }

    stop.store(true, Ordering::Relaxed);
    let reads = reader.join().expect("reader");
    assert!(reads > 0, "the reader must race the writer at least once");
    assert!(attempts > 0, "the writer must submit at least once");
    assert_eq!(
        refused, 0,
        "{refused} of {attempts} non-crossing post-only bids were refused as crossing \
         a stale best ask ({reads} concurrent reads)"
    );
}
