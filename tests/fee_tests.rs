//! Tests for fee schedule functionality

use orderbook_rs::{FeeSchedule, OrderBook, TradeResult};
use pricelevel::{Id, Side, TimeInForce};
use std::sync::Arc;

#[test]
fn test_fee_schedule_creation() {
    let schedule = FeeSchedule::new(-2, 5);
    assert_eq!(schedule.maker_fee_bps, -2);
    assert_eq!(schedule.taker_fee_bps, 5);
    assert!(schedule.has_maker_rebate());
    assert!(!schedule.is_zero_fee());
}

#[test]
fn test_zero_fee_schedule() {
    let schedule = FeeSchedule::zero_fee();
    assert_eq!(schedule.maker_fee_bps, 0);
    assert_eq!(schedule.taker_fee_bps, 0);
    assert!(!schedule.has_maker_rebate());
    assert!(schedule.is_zero_fee());
}

#[test]
fn test_taker_only_schedule() {
    let schedule = FeeSchedule::taker_only(10);
    assert_eq!(schedule.maker_fee_bps, 0);
    assert_eq!(schedule.taker_fee_bps, 10);
    assert!(!schedule.has_maker_rebate());
    assert!(!schedule.is_zero_fee());
}

#[test]
fn test_maker_rebate_schedule() {
    let schedule = FeeSchedule::with_maker_rebate(3, 7);
    assert_eq!(schedule.maker_fee_bps, -3);
    assert_eq!(schedule.taker_fee_bps, 7);
    assert!(schedule.has_maker_rebate());
    assert!(!schedule.is_zero_fee());
}

#[test]
fn test_fee_calculation_taker() {
    let schedule = FeeSchedule::new(-2, 5);
    let notional = 100_000_000; // $1,000 in cents

    // 5 bps of $1,000 = $0.50 = 50 cents
    let fee = schedule.calculate_fee(notional, false);
    assert_eq!(fee, 50_000);
}

#[test]
fn test_fee_calculation_maker_rebate() {
    let schedule = FeeSchedule::new(-2, 5);
    let notional = 100_000_000; // $1,000 in cents

    // -2 bps of $1,000 = -$0.20 = -20 cents
    let rebate = schedule.calculate_fee(notional, true);
    assert_eq!(rebate, -20_000);
}

#[test]
fn test_fee_calculation_zero_fee() {
    let schedule = FeeSchedule::zero_fee();
    let notional = 100_000_000;

    assert_eq!(schedule.calculate_fee(notional, true), 0);
    assert_eq!(schedule.calculate_fee(notional, false), 0);
}

#[test]
fn test_fee_calculation_edge_cases() {
    let schedule = FeeSchedule::new(1, 1);
    let notional = 0;

    assert_eq!(schedule.calculate_fee(notional, true), 0);
    assert_eq!(schedule.calculate_fee(notional, false), 0);
}

#[test]
fn test_fee_calculation_large_values() {
    let schedule = FeeSchedule::new(1, 1);
    let notional = u128::MAX / 10_000 - 1; // Safe large value

    let fee = schedule.calculate_fee(notional, false);
    assert!(fee > 0);
    assert!(fee < i128::MAX);
}

#[test]
fn test_orderbook_fee_schedule_default() {
    let book = OrderBook::<()>::new("BTC/USD");
    assert_eq!(book.fee_schedule(), None);
}

#[test]
fn test_orderbook_set_fee_schedule() {
    let mut book = OrderBook::<()>::new("BTC/USD");
    let schedule = FeeSchedule::new(-2, 5);

    book.set_fee_schedule(Some(schedule));
    assert_eq!(book.fee_schedule(), Some(schedule));
}

#[test]
fn test_orderbook_update_fee_schedule() {
    let mut book = OrderBook::<()>::new("BTC/USD");

    // Set initial schedule
    let initial_schedule = FeeSchedule::new(-1, 3);
    book.set_fee_schedule(Some(initial_schedule));
    assert_eq!(book.fee_schedule(), Some(initial_schedule));

    // Update to different schedule
    let new_schedule = FeeSchedule::new(-2, 5);
    book.set_fee_schedule(Some(new_schedule));
    assert_eq!(book.fee_schedule(), Some(new_schedule));

    // Disable fees
    book.set_fee_schedule(None);
    assert_eq!(book.fee_schedule(), None);
}

#[test]
fn test_orderbook_fee_schedule_persistence() {
    let mut book = OrderBook::<()>::new("BTC/USD");
    let schedule = FeeSchedule::with_maker_rebate(2, 6);

    book.set_fee_schedule(Some(schedule));

    // Add an order (maker)
    let order_id = Id::new_uuid();
    let result = book.add_limit_order(order_id, 100_000, 10, Side::Buy, TimeInForce::Gtc, None);
    assert!(result.is_ok());

    // Fee schedule should still be set
    assert_eq!(book.fee_schedule(), Some(schedule));
}

#[test]
fn test_orderbook_constructors_with_fee_schedule() {
    let schedule = FeeSchedule::new(-2, 5);

    // Test basic constructor
    let mut book1 = OrderBook::<()>::new("BTC/USD");
    book1.set_fee_schedule(Some(schedule));
    assert_eq!(book1.fee_schedule(), Some(schedule));

    // Test constructor with trade listener
    let listener: Arc<dyn Fn(&TradeResult) + Send + Sync> =
        Arc::new(|_trade_result: &TradeResult| {
            // Empty listener for testing
        });
    let mut book2 = OrderBook::<()>::with_trade_listener("BTC/USD", listener);
    book2.set_fee_schedule(Some(schedule));
    assert_eq!(book2.fee_schedule(), Some(schedule));
}

#[test]
fn test_fee_schedule_serialization() {
    let schedule = FeeSchedule::new(-2, 5);

    // Test JSON serialization
    let json = serde_json::to_string(&schedule).unwrap();
    let deserialized: FeeSchedule = serde_json::from_str(&json).unwrap();

    assert_eq!(schedule, deserialized);

    // Test with zero fees
    let zero_schedule = FeeSchedule::zero_fee();
    let json = serde_json::to_string(&zero_schedule).unwrap();
    let deserialized: FeeSchedule = serde_json::from_str(&json).unwrap();

    assert_eq!(zero_schedule, deserialized);
}

#[test]
fn test_orderbook_serialization_with_fee_schedule() {
    let mut book = OrderBook::<()>::new("BTC/USD");
    let schedule = FeeSchedule::with_maker_rebate(3, 7);
    book.set_fee_schedule(Some(schedule));

    // Test serialization
    let json = serde_json::to_string(&book).unwrap();
    assert!(json.contains("fee_schedule"));
}

#[test]
fn test_orderbook_serialization_without_fee_schedule() {
    let book = OrderBook::<()>::new("BTC/USD");

    // Test serialization with no fee schedule
    let json = serde_json::to_string(&book).unwrap();
    assert!(json.contains("fee_schedule"));
}

#[test]
fn test_fee_schedule_mathematical_properties() {
    let schedule = FeeSchedule::new(10, 20);
    let notional = 100_000; // $1,000

    // Test linearity: double the notional should double the fee
    let fee1 = schedule.calculate_fee(notional, false);
    let fee2 = schedule.calculate_fee(notional * 2, false);
    assert_eq!(fee2, fee1 * 2);

    // Test sign consistency
    assert!(schedule.calculate_fee(notional, false) > 0); // Taker fee positive
    assert!(schedule.calculate_fee(notional, true) > 0); // Maker fee positive

    // Test with rebates
    let rebate_schedule = FeeSchedule::new(-5, 10);
    assert!(rebate_schedule.calculate_fee(notional, true) < 0); // Maker rebate negative
    assert!(rebate_schedule.calculate_fee(notional, false) > 0); // Taker fee positive
}

#[test]
fn test_fee_schedule_precision() {
    let schedule = FeeSchedule::new(1, 1);

    // Test with small notional values
    let small_notional = 1;
    let fee = schedule.calculate_fee(small_notional, false);
    assert_eq!(fee, 0); // Should be 0 due to integer division (1 * 1 / 10000)

    // Test with exact division
    let exact_notional = 10_000;
    let fee = schedule.calculate_fee(exact_notional, false);
    assert_eq!(fee, 1); // Should be exactly 1 (10000 * 1 / 10000)
}

#[test]
fn test_fee_schedule_default_trait() {
    let default_schedule = FeeSchedule::default();
    assert!(default_schedule.is_zero_fee());
    assert_eq!(default_schedule.maker_fee_bps, 0);
    assert_eq!(default_schedule.taker_fee_bps, 0);
}

#[cfg(test)]
mod integration_tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn test_fee_schedule_with_matching() {
        let mut book = OrderBook::<()>::new("BTC/USD");
        let schedule = FeeSchedule::new(-2, 5);
        book.set_fee_schedule(Some(schedule));

        // Add a bid order (maker)
        let bid_order_id = Id::new_uuid();
        let bid_result =
            book.add_limit_order(bid_order_id, 100_000, 10, Side::Buy, TimeInForce::Gtc, None);
        assert!(bid_result.is_ok());

        // Add an ask order that will match (taker)
        let ask_order_id = Id::new_uuid();
        let ask_result =
            book.add_limit_order(ask_order_id, 100_000, 5, Side::Sell, TimeInForce::Ioc, None);
        assert!(ask_result.is_ok());

        // The fee schedule should still be intact after matching
        assert_eq!(book.fee_schedule(), Some(schedule));

        // Verify the book state
        let snapshot = book.create_snapshot(10);
        assert_eq!(snapshot.bids.len(), 1); // Remaining bid quantity
        assert_eq!(snapshot.asks.len(), 0); // Ask fully consumed
    }

    #[test]
    fn test_fee_schedule_with_multiple_operations() {
        let mut book = OrderBook::<()>::new("BTC/USD");

        // Start with no fees
        assert_eq!(book.fee_schedule(), None);

        // Add orders with no fees
        let order1_id = Id::new_uuid();
        book.add_limit_order(order1_id, 100_000, 10, Side::Buy, TimeInForce::Gtc, None)
            .unwrap();
        assert_eq!(book.fee_schedule(), None);

        // Set fee schedule
        let schedule = FeeSchedule::taker_only(10);
        book.set_fee_schedule(Some(schedule));
        assert_eq!(book.fee_schedule(), Some(schedule));

        // Add more orders with fees active
        let order2_id = Id::new_uuid();
        book.add_limit_order(order2_id, 100_000, 5, Side::Sell, TimeInForce::Gtc, None)
            .unwrap();
        assert_eq!(book.fee_schedule(), Some(schedule));

        // Fee schedule should persist through operations
        let snapshot = book.create_snapshot(10);
        assert!(!snapshot.bids.is_empty() || !snapshot.asks.is_empty());
        assert_eq!(book.fee_schedule(), Some(schedule));
    }

    #[test]
    fn test_trade_listener_receives_fees() {
        // Capture trades via listener
        let captured_trades = Arc::new(Mutex::new(Vec::<TradeResult>::new()));
        let captured_clone = captured_trades.clone();

        let listener: Arc<dyn Fn(&TradeResult) + Send + Sync> =
            Arc::new(move |trade_result: &TradeResult| {
                let mut trades = captured_clone.lock().unwrap();
                trades.push(trade_result.clone());
            });

        let mut book = OrderBook::<()>::with_trade_listener("BTC/USD", listener);

        // Set fee schedule: -2 bps maker rebate, 5 bps taker fee
        let schedule = FeeSchedule::new(-2, 5);
        book.set_fee_schedule(Some(schedule));

        // Add a resting sell order (will become maker)
        let sell_id = Id::new_uuid();
        book.add_limit_order(sell_id, 10_000, 100, Side::Sell, TimeInForce::Gtc, None)
            .unwrap();

        // Submit market buy (taker) — will match and trigger listener
        let buy_id = Id::new_uuid();
        book.submit_market_order(buy_id, 50, Side::Buy).unwrap();

        // Verify the captured trade has correct fees
        let trades = captured_trades.lock().unwrap();
        assert_eq!(trades.len(), 1);

        let tr = &trades[0];
        assert_eq!(tr.symbol, "BTC/USD");

        // notional = 10_000 * 50 = 500_000
        // maker fee: 500_000 * -2 / 10_000 = -100
        assert_eq!(tr.total_maker_fees, -100);
        // taker fee: 500_000 * 5 / 10_000 = 250
        assert_eq!(tr.total_taker_fees, 250);
        // total: -100 + 250 = 150
        assert_eq!(tr.total_fees(), 150);
    }

    #[test]
    fn test_trade_listener_receives_zero_fees_when_no_schedule() {
        let captured_trades = Arc::new(Mutex::new(Vec::<TradeResult>::new()));
        let captured_clone = captured_trades.clone();

        let listener: Arc<dyn Fn(&TradeResult) + Send + Sync> =
            Arc::new(move |trade_result: &TradeResult| {
                let mut trades = captured_clone.lock().unwrap();
                trades.push(trade_result.clone());
            });

        let book = OrderBook::<()>::with_trade_listener("BTC/USD", listener);
        // No fee schedule configured (None by default)

        let sell_id = Id::new_uuid();
        book.add_limit_order(sell_id, 10_000, 100, Side::Sell, TimeInForce::Gtc, None)
            .unwrap();

        let buy_id = Id::new_uuid();
        book.submit_market_order(buy_id, 50, Side::Buy).unwrap();

        let trades = captured_trades.lock().unwrap();
        assert_eq!(trades.len(), 1);
        assert_eq!(trades[0].total_maker_fees, 0);
        assert_eq!(trades[0].total_taker_fees, 0);
        assert_eq!(trades[0].total_fees(), 0);
    }

    #[test]
    fn test_trade_listener_fees_across_multiple_price_levels() {
        let captured_trades = Arc::new(Mutex::new(Vec::<TradeResult>::new()));
        let captured_clone = captured_trades.clone();

        let listener: Arc<dyn Fn(&TradeResult) + Send + Sync> =
            Arc::new(move |trade_result: &TradeResult| {
                let mut trades = captured_clone.lock().unwrap();
                trades.push(trade_result.clone());
            });

        let mut book = OrderBook::<()>::with_trade_listener("BTC/USD", listener);

        // 10 bps taker, -3 bps maker rebate
        let schedule = FeeSchedule::new(-3, 10);
        book.set_fee_schedule(Some(schedule));

        // Add two sell orders at different prices
        let sell_id1 = Id::new_uuid();
        book.add_limit_order(sell_id1, 1000, 10, Side::Sell, TimeInForce::Gtc, None)
            .unwrap();

        let sell_id2 = Id::new_uuid();
        book.add_limit_order(sell_id2, 2000, 10, Side::Sell, TimeInForce::Gtc, None)
            .unwrap();

        // Market buy that sweeps both levels
        let buy_id = Id::new_uuid();
        book.submit_market_order(buy_id, 20, Side::Buy).unwrap();

        let trades = captured_trades.lock().unwrap();
        assert_eq!(trades.len(), 1);

        let tr = &trades[0];
        // tx1: notional = 1000 * 10 = 10_000
        //   maker: -3 * 10_000 / 10_000 = -3
        //   taker: 10 * 10_000 / 10_000 = 10
        // tx2: notional = 2000 * 10 = 20_000
        //   maker: -3 * 20_000 / 10_000 = -6
        //   taker: 10 * 20_000 / 10_000 = 20
        assert_eq!(tr.total_maker_fees, -9);
        assert_eq!(tr.total_taker_fees, 30);
        assert_eq!(tr.total_fees(), 21);
    }
}
