// examples/src/bin/kill_switch_drain.rs
//
// Operator-style demo of the kill switch (issue #53).
//
// Walks through the canonical halt-and-drain sequence:
//   1. Build a book and seed it with resting limit orders.
//   2. Engage the kill switch.
//   3. Demonstrate that submit / add_order are rejected while
//      cancel and mass-cancel keep working.
//   4. Drain the resting book via `cancel_all_orders`.
//   5. Release the kill switch and submit a fresh order to confirm
//      normal flow has resumed.

use orderbook_rs::{OrderBook, OrderBookError};
use pricelevel::{
    Hash32, Id, OrderType, Price, Quantity, Side, TimeInForce, TimestampMs, setup_logger,
};
use tracing::{info, warn};

fn main() {
    let _ = setup_logger();
    info!("Kill switch drain example");

    let book = OrderBook::<()>::new("BTC/USD");

    seed_resting_orders(&book);
    info!(
        "Seeded book with {} resting orders",
        book.get_all_orders().len()
    );

    info!("Engaging kill switch — new flow should be halted");
    book.engage_kill_switch();
    assert!(book.is_kill_switch_engaged());

    demo_submit_rejected_while_engaged(&book);
    demo_cancel_still_works_while_engaged(&book);
    drain_remaining_book(&book);

    info!("Releasing kill switch — book should accept new flow again");
    book.release_kill_switch();
    assert!(!book.is_kill_switch_engaged());

    demo_submit_succeeds_after_release(&book);

    info!("Kill switch drain example complete");
}

fn seed_resting_orders(book: &OrderBook<()>) {
    let user = Hash32::zero();
    let timestamp = TimestampMs::new(0);

    let orders: [(u64, u128, u64, Side); 4] = [
        (1, 100, 10, Side::Buy),
        (2, 99, 5, Side::Buy),
        (3, 101, 7, Side::Sell),
        (4, 102, 4, Side::Sell),
    ];

    for (id, price, qty, side) in orders {
        let order = OrderType::Standard {
            id: Id::from_u64(id),
            price: Price::new(price),
            quantity: Quantity::new(qty),
            side,
            time_in_force: TimeInForce::Gtc,
            user_id: user,
            timestamp,
            extra_fields: (),
        };
        if let Err(err) = book.add_order(order) {
            warn!("seed add_order failed: {err}");
        }
    }
}

fn demo_submit_rejected_while_engaged(book: &OrderBook<()>) {
    let order = OrderType::Standard {
        id: Id::from_u64(99),
        price: Price::new(100),
        quantity: Quantity::new(1),
        side: Side::Buy,
        time_in_force: TimeInForce::Gtc,
        user_id: Hash32::zero(),
        timestamp: TimestampMs::new(0),
        extra_fields: (),
    };

    match book.add_order(order) {
        Err(OrderBookError::KillSwitchActive) => {
            info!("add_order correctly rejected with KillSwitchActive");
        }
        other => warn!("expected KillSwitchActive, got {other:?}"),
    }
}

fn demo_cancel_still_works_while_engaged(book: &OrderBook<()>) {
    match book.cancel_order(Id::from_u64(2)) {
        Ok(_) => info!("cancel_order(id=2) succeeded while kill switch engaged"),
        Err(err) => warn!("cancel_order failed unexpectedly: {err}"),
    }
}

fn drain_remaining_book(book: &OrderBook<()>) {
    let result = book.cancel_all_orders();
    info!(
        "cancel_all_orders drained {} remaining order(s) while kill switch engaged",
        result.cancelled_count()
    );
}

fn demo_submit_succeeds_after_release(book: &OrderBook<()>) {
    let order = OrderType::Standard {
        id: Id::from_u64(100),
        price: Price::new(100),
        quantity: Quantity::new(3),
        side: Side::Buy,
        time_in_force: TimeInForce::Gtc,
        user_id: Hash32::zero(),
        timestamp: TimestampMs::new(0),
        extra_fields: (),
    };

    match book.add_order(order) {
        Ok(_) => info!("post-release add_order succeeded as expected"),
        Err(err) => warn!("post-release add_order failed: {err}"),
    }
}
