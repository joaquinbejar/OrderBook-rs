// examples/src/bin/market_order_by_amount.rs
//
// Demonstrates the quote-notional market-order path
// (`match_market_order_by_amount`).
//
// Run with:
//
//     cargo run --example market_order_by_amount
//
// (See `examples/Cargo.toml` for the binary registration. Locally:
// `cargo run -p examples --bin market_order_by_amount`.)

use orderbook_rs::OrderBook;
use pricelevel::{Id, Side, TimeInForce, setup_logger};
use tracing::info;

fn main() {
    let _ = setup_logger();
    info!("Quote-notional market order demo");

    // Lot size = 10 base units. Resting orders must be multiples of 10.
    let book: OrderBook<()> = OrderBook::with_lot_size("BTC/USDT", 10);

    seed_ask_wall(&book);
    info!("Best ask: {:?}", book.best_ask());

    notional_buy_exact_fit(&book);
    notional_buy_with_dust(&book);
    notional_buy_walks_levels(&book);
    notional_buy_below_one_lot_errors(&book);
}

fn seed_ask_wall(book: &OrderBook<()>) {
    // Three ask levels: 100 @ 50, 101 @ 50, 102 @ 50 (all multiples of lot=10).
    for &(price, qty) in &[(100u128, 50u64), (101, 50), (102, 50)] {
        book.add_limit_order(
            Id::new_uuid(),
            price,
            qty,
            Side::Sell,
            TimeInForce::Gtc,
            None,
        )
        .expect("seed ask");
    }
    info!("Seeded asks: 100x50, 101x50, 102x50 (lot=10)");
}

fn notional_buy_exact_fit(book: &OrderBook<()>) {
    info!("\n--- Notional buy: $5_000 (exact fit at best ask) ---");
    let result = book
        .match_market_order_by_amount(Id::new_uuid(), 5_000, Side::Buy)
        .expect("notional buy");
    info!(
        "  trades: {}, executed_qty: {:?}, executed_value: {:?}",
        result.trades().len(),
        result.executed_quantity(),
        result.executed_value(),
    );
}

fn notional_buy_with_dust(book: &OrderBook<()>) {
    info!("\n--- Notional buy: $1_405 (lot=10 ⇒ rounds down to 10 units = $1_010) ---");
    let result = book
        .match_market_order_by_amount(Id::new_uuid(), 1_405, Side::Buy)
        .expect("notional buy");
    info!(
        "  trades: {}, executed_value: {:?}, residual dust = $1_405 - executed_value",
        result.trades().len(),
        result.executed_value(),
    );
}

fn notional_buy_walks_levels(book: &OrderBook<()>) {
    info!("\n--- Notional buy: $4_040 (walks two levels) ---");
    let result = book
        .match_market_order_by_amount(Id::new_uuid(), 4_040, Side::Buy)
        .expect("notional buy");
    info!(
        "  trades: {}, executed_qty: {:?}, executed_value: {:?}",
        result.trades().len(),
        result.executed_quantity(),
        result.executed_value(),
    );
    for trade in result.trades().as_vec() {
        info!(
            "    fill price={} qty={}",
            trade.price().as_u128(),
            trade.quantity().as_u64()
        );
    }
}

fn notional_buy_below_one_lot_errors(book: &OrderBook<()>) {
    info!("\n--- Notional buy: $50 (below 1 lot * best price ⇒ insufficient) ---");
    match book.match_market_order_by_amount(Id::new_uuid(), 50, Side::Buy) {
        Ok(_) => info!("  unexpectedly succeeded"),
        Err(e) => info!("  expected error: {e}"),
    }
}
