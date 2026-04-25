// examples/src/bin/risk_limits.rs
//
// Operator-style demo of the pre-trade risk layer (issue #54).
//
// Configures all three guard-rails on a fresh book and submits orders
// that breach each in sequence, then submits a clean order to confirm
// post-rejection state is consistent.

use orderbook_rs::orderbook::risk::{ReferencePriceSource, RiskConfig};
use orderbook_rs::{OrderBook, OrderBookError};
use pricelevel::{Hash32, Id, Side, TimeInForce, setup_logger};
use tracing::{info, warn};

fn account(byte: u8) -> Hash32 {
    let mut bytes = [0u8; 32];
    bytes[0] = byte;
    Hash32::new(bytes)
}

fn main() {
    let _ = setup_logger();
    info!("Risk limits demo");

    let mut book = OrderBook::<()>::new("BTC/USD");
    let acct_a = account(1);

    // Reference price for the band check needs at least one trade or a
    // two-sided book. Plant a fresh trade so `LastTrade` resolves.
    seed_reference_trade(&book);

    book.set_risk_config(
        RiskConfig::new()
            .with_max_open_orders_per_account(2)
            .with_max_notional_per_account(5_000)
            .with_price_band_bps(1_000, ReferencePriceSource::LastTrade),
    );

    info!(
        "Configured: max_open=2, max_notional=5_000, band=1000 bps vs LastTrade ({})",
        book.last_trade_price().unwrap_or(0)
    );

    demo_max_open_breach(&book, acct_a);
    demo_max_notional_breach(&book);
    demo_price_band_breach(&book, acct_a);

    let acct_b = account(2);
    submit_clean_order(&book, acct_b);

    info!("Risk limits demo complete");
}

fn seed_reference_trade(book: &OrderBook<()>) {
    book.add_limit_order_with_user(
        Id::from_u64(900),
        100,
        1,
        Side::Sell,
        TimeInForce::Gtc,
        account(99),
        None,
    )
    .expect("seed ask");
    book.submit_market_order_with_user(Id::from_u64(901), 1, Side::Buy, account(98))
        .expect("trade against seed ask");
}

fn demo_max_open_breach(book: &OrderBook<()>, acct: Hash32) {
    info!("--- max_open_orders breach ---");
    for i in 0..2 {
        let id = Id::from_u64(100 + i);
        match book.add_limit_order_with_user(id, 100, 1, Side::Buy, TimeInForce::Gtc, acct, None) {
            Ok(_) => info!("admitted bid #{i}"),
            Err(err) => warn!("unexpected reject: {err}"),
        }
    }
    let third = Id::from_u64(102);
    match book.add_limit_order_with_user(third, 100, 1, Side::Buy, TimeInForce::Gtc, acct, None) {
        Err(OrderBookError::RiskMaxOpenOrders {
            current,
            limit,
            account: a,
        }) => {
            info!(
                "third bid correctly rejected: account={:?} current={current} limit={limit}",
                a.as_bytes().first().copied().unwrap_or_default()
            )
        }
        other => warn!("expected RiskMaxOpenOrders, got {other:?}"),
    }

    // Drain so the next demo starts clean.
    let _ = book.cancel_order(Id::from_u64(100));
    let _ = book.cancel_order(Id::from_u64(101));
}

fn demo_max_notional_breach(book: &OrderBook<()>) {
    info!("--- max_notional breach ---");
    let acct = account(3);
    // Notional limit is 5_000. price * qty = 100 * 40 = 4_000 (within),
    // then 100 * 20 = 2_000 (would push to 6_000 → reject).
    book.add_limit_order_with_user(
        Id::from_u64(200),
        100,
        40,
        Side::Buy,
        TimeInForce::Gtc,
        acct,
        None,
    )
    .expect("first bid within notional");
    let next = Id::from_u64(201);
    match book.add_limit_order_with_user(next, 100, 20, Side::Buy, TimeInForce::Gtc, acct, None) {
        Err(OrderBookError::RiskMaxNotional {
            current,
            attempted,
            limit,
            ..
        }) => info!(
            "second bid correctly rejected: current={current} attempted={attempted} limit={limit}"
        ),
        other => warn!("expected RiskMaxNotional, got {other:?}"),
    }
    let _ = book.cancel_order(Id::from_u64(200));
}

fn demo_price_band_breach(book: &OrderBook<()>, acct: Hash32) {
    info!("--- price_band breach ---");
    // Band is 1000 bps (10%) of last_trade_price = 100, so any bid
    // below 90 or above 110 breaches the band.
    let off_band = Id::from_u64(300);
    match book.add_limit_order_with_user(off_band, 50, 1, Side::Buy, TimeInForce::Gtc, acct, None) {
        Err(OrderBookError::RiskPriceBand {
            submitted,
            reference,
            deviation_bps,
            limit_bps,
        }) => info!(
            "off-band bid correctly rejected: submitted={submitted} reference={reference} deviation={deviation_bps}bps limit={limit_bps}bps"
        ),
        other => warn!("expected RiskPriceBand, got {other:?}"),
    }
}

fn submit_clean_order(book: &OrderBook<()>, acct: Hash32) {
    info!("--- clean order on a fresh account ---");
    match book.add_limit_order_with_user(
        Id::from_u64(400),
        100,
        1,
        Side::Buy,
        TimeInForce::Gtc,
        acct,
        None,
    ) {
        Ok(_) => info!("clean order admitted"),
        Err(err) => warn!("clean order failed: {err}"),
    }
}
