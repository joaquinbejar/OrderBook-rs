// examples/src/bin/gtd_expiry_sweep.rs
//
// Demonstrates the host-driven GTD / DAY expiry sweep
// (`OrderBook::evict_expired_orders`).
//
// The sweep never reads the book's own clock: the caller passes an explicit
// `TimestampMs` (Unix milliseconds), so a scheduler drives the cadence and the
// sequencer can journal the exact cutoff. Eviction order is deterministic —
// bids before asks, ascending price within a side, ascending insertion
// sequence within a level — which is what keeps replay byte-identical.
//
// Run with:
//
//     cargo run -p examples --bin gtd_expiry_sweep

use std::sync::Arc;

use orderbook_rs::{Clock, OrderBook, StubClock, TimestampMs};
use pricelevel::{Id, Side, TimeInForce, setup_logger};
use tracing::info;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _ = setup_logger();
    info!("GTD / DAY expiry sweep demo");

    // A StubClock starting at logical t=0 so the small GTD deadlines below are
    // admitted. Under the default wall-clock (`MonotonicClock`), a deadline of
    // 1_000 ms since the Unix epoch would be treated as already expired at
    // admission and rejected.
    let book: OrderBook<()> = OrderBook::with_clock(
        "BTC/USDT",
        Arc::new(StubClock::starting_at(0)) as Arc<dyn Clock>,
    );

    // A DAY order expires at the configured market close.
    book.set_market_close_timestamp(3_000);

    seed(&book)?;
    info!(
        "Seeded book: best_bid={:?} best_ask={:?}",
        book.best_bid(),
        book.best_ask()
    );

    // t = 999: nothing has expired yet (expiry boundary is `now >= deadline`).
    sweep(&book, 999);

    // t = 1_500: the two GTD orders with deadline 1_000 expire; the DAY order
    // (market close 3_000) and the GTD at 5_000 and the GTC survive.
    sweep(&book, 1_500);

    // t = 3_000: the DAY order expires at market close.
    sweep(&book, 3_000);

    // t = 5_000: the last GTD expires; only the GTC remains.
    sweep(&book, 5_000);

    info!(
        "Final book: best_bid={:?} best_ask={:?} (the GTC order remains)",
        book.best_bid(),
        book.best_ask()
    );
    Ok(())
}

fn seed(book: &OrderBook<()>) -> Result<(), Box<dyn std::error::Error>> {
    // Two GTD bids expiring at t=1_000, at different prices so the sweep must
    // visit them in ascending-price order.
    book.add_limit_order(
        Id::new_uuid(),
        95,
        5,
        Side::Buy,
        TimeInForce::Gtd(1_000),
        None,
    )?;
    book.add_limit_order(
        Id::new_uuid(),
        96,
        5,
        Side::Buy,
        TimeInForce::Gtd(1_000),
        None,
    )?;
    // A GTC bid that never expires.
    book.add_limit_order(Id::new_uuid(), 97, 5, Side::Buy, TimeInForce::Gtc, None)?;
    // A DAY ask expiring at the market close (3_000).
    book.add_limit_order(Id::new_uuid(), 101, 5, Side::Sell, TimeInForce::Day, None)?;
    // A GTD ask expiring at t=5_000.
    book.add_limit_order(
        Id::new_uuid(),
        102,
        5,
        Side::Sell,
        TimeInForce::Gtd(5_000),
        None,
    )?;
    Ok(())
}

fn sweep(book: &OrderBook<()>, now_ms: u64) {
    let evicted = book.evict_expired_orders(TimestampMs::new(now_ms));
    info!("--- sweep at t={now_ms} ms: {} evicted ---", evicted.len());
    // Evicted orders are returned in the deterministic contract order.
    for order in &evicted {
        info!(
            "    evicted id={} side={:?} price={} tif={:?}",
            order.id(),
            order.side(),
            order.price().as_u128(),
            order.time_in_force(),
        );
    }
}
