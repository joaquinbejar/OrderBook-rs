use orderbook_rs::OrderBook;
use pricelevel::{Hash32, Id, OrderType, Price, Quantity, Side, TimeInForce, TimestampMs};

#[cfg(test)]
mod tests {
    use super::*;

    fn make_standard_order(price: u128, quantity: u64, side: Side) -> OrderType<()> {
        OrderType::Standard {
            id: Id::new_uuid(),
            price: Price::new(price),
            quantity: Quantity::new(quantity),
            side,
            user_id: Hash32::zero(),
            timestamp: TimestampMs::new(0),
            time_in_force: TimeInForce::Gtc,
            extra_fields: (),
        }
    }

    fn make_iceberg_order(price: u128, side: Side) -> OrderType<()> {
        make_iceberg_order_with_qty(price, 10, 90, side)
    }

    fn make_iceberg_order_with_qty(
        price: u128,
        visible: u64,
        hidden: u64,
        side: Side,
    ) -> OrderType<()> {
        OrderType::IcebergOrder {
            id: Id::new_uuid(),
            price: Price::new(price),
            visible_quantity: Quantity::new(visible),
            hidden_quantity: Quantity::new(hidden),
            side,
            user_id: Hash32::zero(),
            timestamp: TimestampMs::new(0),
            time_in_force: TimeInForce::Gtc,
            extra_fields: (),
        }
    }

    fn make_post_only_order(price: u128, side: Side) -> OrderType<()> {
        OrderType::PostOnly {
            id: Id::new_uuid(),
            price: Price::new(price),
            quantity: Quantity::new(100),
            side,
            user_id: Hash32::zero(),
            timestamp: TimestampMs::new(0),
            time_in_force: TimeInForce::Gtc,
            extra_fields: (),
        }
    }

    // --- with_tick_size constructor ---

    #[test]
    fn test_with_tick_size_constructor() {
        let book: OrderBook<()> = OrderBook::with_tick_size("BTC/USD", 100);
        assert_eq!(book.tick_size(), Some(100));
    }

    // --- set_tick_size setter ---

    #[test]
    fn test_set_tick_size() {
        let mut book: OrderBook<()> = OrderBook::new("BTC/USD");
        assert_eq!(book.tick_size(), None);
        book.set_tick_size(50);
        assert_eq!(book.tick_size(), Some(50));
    }

    // --- Backward compatibility: no tick size ---

    #[test]
    fn test_no_tick_size_accepts_any_price() {
        let book: OrderBook<()> = OrderBook::new("BTC/USD");
        let order = make_standard_order(12345, 100, Side::Buy);
        let result = book.add_order(order);
        assert!(result.is_ok());
    }

    // --- Valid tick sizes ---

    #[test]
    fn test_valid_tick_size_exact_multiple() {
        let book: OrderBook<()> = OrderBook::with_tick_size("BTC/USD", 100);
        let order = make_standard_order(1000, 100, Side::Buy);
        let result = book.add_order(order);
        assert!(result.is_ok());
    }

    #[test]
    fn test_valid_tick_size_larger_multiple() {
        let book: OrderBook<()> = OrderBook::with_tick_size("BTC/USD", 100);
        let order = make_standard_order(50000, 100, Side::Buy);
        let result = book.add_order(order);
        assert!(result.is_ok());
    }

    #[test]
    fn test_valid_tick_size_equals_tick() {
        let book: OrderBook<()> = OrderBook::with_tick_size("BTC/USD", 100);
        let order = make_standard_order(100, 100, Side::Buy);
        let result = book.add_order(order);
        assert!(result.is_ok());
    }

    // --- Invalid tick sizes ---

    #[test]
    fn test_invalid_tick_size_not_multiple() {
        let book: OrderBook<()> = OrderBook::with_tick_size("BTC/USD", 100);
        let order = make_standard_order(150, 100, Side::Buy);
        let result = book.add_order(order);
        assert!(result.is_err());
        let err = result.unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("tick size"),
            "Error message should mention tick size: {msg}"
        );
    }

    #[test]
    fn test_invalid_tick_size_off_by_one() {
        let book: OrderBook<()> = OrderBook::with_tick_size("BTC/USD", 100);
        let order = make_standard_order(1001, 100, Side::Buy);
        let result = book.add_order(order);
        assert!(result.is_err());
    }

    #[test]
    fn test_invalid_tick_size_off_by_one_below() {
        let book: OrderBook<()> = OrderBook::with_tick_size("BTC/USD", 100);
        let order = make_standard_order(999, 100, Side::Buy);
        let result = book.add_order(order);
        assert!(result.is_err());
    }

    // --- Tick size = 1 accepts all prices ---

    #[test]
    fn test_tick_size_one_accepts_any_price() {
        let book: OrderBook<()> = OrderBook::with_tick_size("BTC/USD", 1);
        let order = make_standard_order(12345, 100, Side::Buy);
        let result = book.add_order(order);
        assert!(result.is_ok());
    }

    // --- Validation applies to all order types ---

    #[test]
    fn test_tick_size_rejects_iceberg_order() {
        let book: OrderBook<()> = OrderBook::with_tick_size("BTC/USD", 100);
        let order = make_iceberg_order(150, Side::Buy);
        let result = book.add_order(order);
        assert!(result.is_err());
    }

    #[test]
    fn test_tick_size_accepts_iceberg_order_valid() {
        let book: OrderBook<()> = OrderBook::with_tick_size("BTC/USD", 100);
        let order = make_iceberg_order(200, Side::Buy);
        let result = book.add_order(order);
        assert!(result.is_ok());
    }

    #[test]
    fn test_tick_size_rejects_post_only_order() {
        let book: OrderBook<()> = OrderBook::with_tick_size("BTC/USD", 100);
        let order = make_post_only_order(150, Side::Buy);
        let result = book.add_order(order);
        assert!(result.is_err());
    }

    #[test]
    fn test_tick_size_accepts_post_only_order_valid() {
        let book: OrderBook<()> = OrderBook::with_tick_size("BTC/USD", 100);
        let order = make_post_only_order(200, Side::Buy);
        let result = book.add_order(order);
        assert!(result.is_ok());
    }

    // --- Sell side validation ---

    #[test]
    fn test_tick_size_rejects_sell_order() {
        let book: OrderBook<()> = OrderBook::with_tick_size("BTC/USD", 100);
        let order = make_standard_order(150, 100, Side::Sell);
        let result = book.add_order(order);
        assert!(result.is_err());
    }

    #[test]
    fn test_tick_size_accepts_sell_order_valid() {
        let book: OrderBook<()> = OrderBook::with_tick_size("BTC/USD", 100);
        let order = make_standard_order(200, 100, Side::Sell);
        let result = book.add_order(order);
        assert!(result.is_ok());
    }

    // --- Dynamic tick size change ---

    #[test]
    fn test_set_tick_size_changes_validation() {
        let mut book: OrderBook<()> = OrderBook::new("BTC/USD");

        // No tick size — any price accepted
        let order = make_standard_order(150, 100, Side::Buy);
        assert!(book.add_order(order).is_ok());

        // Set tick size — 150 would now fail
        book.set_tick_size(100);
        let order = make_standard_order(150, 100, Side::Sell);
        assert!(book.add_order(order).is_err());

        // But 200 still works
        let order = make_standard_order(200, 100, Side::Sell);
        assert!(book.add_order(order).is_ok());
    }

    // --- add_limit_order convenience method ---

    #[test]
    fn test_add_limit_order_respects_tick_size() {
        let book: OrderBook<()> = OrderBook::with_tick_size("BTC/USD", 100);
        let result =
            book.add_limit_order(Id::new_uuid(), 150, 100, Side::Buy, TimeInForce::Gtc, None);
        assert!(result.is_err());
    }

    #[test]
    fn test_add_limit_order_valid_tick_size() {
        let book: OrderBook<()> = OrderBook::with_tick_size("BTC/USD", 100);
        let result =
            book.add_limit_order(Id::new_uuid(), 200, 100, Side::Buy, TimeInForce::Gtc, None);
        assert!(result.is_ok());
    }

    // --- Error display ---

    #[test]
    fn test_invalid_tick_size_error_display() {
        let book: OrderBook<()> = OrderBook::with_tick_size("BTC/USD", 100);
        let order = make_standard_order(150, 100, Side::Buy);
        let err = book.add_order(order).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("150"), "Should contain price: {msg}");
        assert!(msg.contains("100"), "Should contain tick size: {msg}");
    }

    // =========================================================================
    // Lot Size Validation Tests
    // =========================================================================

    // --- with_lot_size constructor ---

    #[test]
    fn test_with_lot_size_constructor() {
        let book: OrderBook<()> = OrderBook::with_lot_size("BTC/USD", 10);
        assert_eq!(book.lot_size(), Some(10));
    }

    // --- set_lot_size setter ---

    #[test]
    fn test_set_lot_size() {
        let mut book: OrderBook<()> = OrderBook::new("BTC/USD");
        assert_eq!(book.lot_size(), None);
        book.set_lot_size(25);
        assert_eq!(book.lot_size(), Some(25));
    }

    // --- Backward compatibility: no lot size ---

    #[test]
    fn test_no_lot_size_accepts_any_quantity() {
        let book: OrderBook<()> = OrderBook::new("BTC/USD");
        let order = make_standard_order(1000, 7, Side::Buy);
        assert!(book.add_order(order).is_ok());
    }

    // --- Valid lot sizes ---

    #[test]
    fn test_valid_lot_size_exact_multiple() {
        let book: OrderBook<()> = OrderBook::with_lot_size("BTC/USD", 10);
        let order = make_standard_order(1000, 100, Side::Buy);
        assert!(book.add_order(order).is_ok());
    }

    #[test]
    fn test_valid_lot_size_equals_lot() {
        let book: OrderBook<()> = OrderBook::with_lot_size("BTC/USD", 10);
        let order = make_standard_order(1000, 10, Side::Buy);
        assert!(book.add_order(order).is_ok());
    }

    // --- Invalid lot sizes ---

    #[test]
    fn test_invalid_lot_size_not_multiple() {
        let book: OrderBook<()> = OrderBook::with_lot_size("BTC/USD", 10);
        let order = make_standard_order(1000, 15, Side::Buy);
        let result = book.add_order(order);
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(
            msg.contains("lot size"),
            "Error message should mention lot size: {msg}"
        );
    }

    #[test]
    fn test_invalid_lot_size_off_by_one() {
        let book: OrderBook<()> = OrderBook::with_lot_size("BTC/USD", 10);
        let order = make_standard_order(1000, 11, Side::Buy);
        assert!(book.add_order(order).is_err());
    }

    // --- Lot size = 1 accepts all quantities ---

    #[test]
    fn test_lot_size_one_accepts_any_quantity() {
        let book: OrderBook<()> = OrderBook::with_lot_size("BTC/USD", 1);
        let order = make_standard_order(1000, 7, Side::Buy);
        assert!(book.add_order(order).is_ok());
    }

    // --- Iceberg order: individual visible/hidden validation ---

    #[test]
    fn test_lot_size_iceberg_both_valid() {
        let book: OrderBook<()> = OrderBook::with_lot_size("BTC/USD", 10);
        let order = make_iceberg_order_with_qty(1000, 20, 80, Side::Buy);
        assert!(book.add_order(order).is_ok());
    }

    #[test]
    fn test_lot_size_iceberg_visible_invalid() {
        let book: OrderBook<()> = OrderBook::with_lot_size("BTC/USD", 10);
        let order = make_iceberg_order_with_qty(1000, 15, 80, Side::Buy);
        let result = book.add_order(order);
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(
            msg.contains("15"),
            "Should contain invalid visible qty: {msg}"
        );
    }

    #[test]
    fn test_lot_size_iceberg_hidden_invalid() {
        let book: OrderBook<()> = OrderBook::with_lot_size("BTC/USD", 10);
        let order = make_iceberg_order_with_qty(1000, 20, 75, Side::Buy);
        let result = book.add_order(order);
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(
            msg.contains("75"),
            "Should contain invalid hidden qty: {msg}"
        );
    }

    #[test]
    fn test_lot_size_iceberg_both_invalid() {
        let book: OrderBook<()> = OrderBook::with_lot_size("BTC/USD", 10);
        // visible is checked first, so error reports visible quantity
        let order = make_iceberg_order_with_qty(1000, 15, 75, Side::Buy);
        let result = book.add_order(order);
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(msg.contains("15"), "Should report visible qty first: {msg}");
    }

    // --- Post-only order lot validation ---

    #[test]
    fn test_lot_size_rejects_post_only_order() {
        let book: OrderBook<()> = OrderBook::with_lot_size("BTC/USD", 10);
        // make_post_only_order uses quantity=100, which is valid for lot=10
        // Create one with invalid quantity
        let order = OrderType::PostOnly {
            id: Id::new_uuid(),
            price: Price::new(1000),
            quantity: Quantity::new(15),
            side: Side::Buy,
            user_id: Hash32::zero(),
            timestamp: TimestampMs::new(0),
            time_in_force: TimeInForce::Gtc,
            extra_fields: (),
        };
        assert!(book.add_order(order).is_err());
    }

    #[test]
    fn test_lot_size_accepts_post_only_order_valid() {
        let book: OrderBook<()> = OrderBook::with_lot_size("BTC/USD", 10);
        let order = make_post_only_order(1000, Side::Buy);
        assert!(book.add_order(order).is_ok());
    }

    // --- Sell side lot validation ---

    #[test]
    fn test_lot_size_rejects_sell_order() {
        let book: OrderBook<()> = OrderBook::with_lot_size("BTC/USD", 10);
        let order = make_standard_order(1000, 15, Side::Sell);
        assert!(book.add_order(order).is_err());
    }

    #[test]
    fn test_lot_size_accepts_sell_order_valid() {
        let book: OrderBook<()> = OrderBook::with_lot_size("BTC/USD", 10);
        let order = make_standard_order(1000, 50, Side::Sell);
        assert!(book.add_order(order).is_ok());
    }

    // --- Dynamic lot size change ---

    #[test]
    fn test_set_lot_size_changes_validation() {
        let mut book: OrderBook<()> = OrderBook::new("BTC/USD");

        // No lot size — any quantity accepted
        let order = make_standard_order(1000, 7, Side::Buy);
        assert!(book.add_order(order).is_ok());

        // Set lot size — 7 would now fail
        book.set_lot_size(10);
        let order = make_standard_order(2000, 7, Side::Sell);
        assert!(book.add_order(order).is_err());

        // But 20 works
        let order = make_standard_order(2000, 20, Side::Sell);
        assert!(book.add_order(order).is_ok());
    }

    // --- add_limit_order convenience method ---

    #[test]
    fn test_add_limit_order_respects_lot_size() {
        let book: OrderBook<()> = OrderBook::with_lot_size("BTC/USD", 10);
        let result =
            book.add_limit_order(Id::new_uuid(), 1000, 15, Side::Buy, TimeInForce::Gtc, None);
        assert!(result.is_err());
    }

    #[test]
    fn test_add_limit_order_valid_lot_size() {
        let book: OrderBook<()> = OrderBook::with_lot_size("BTC/USD", 10);
        let result =
            book.add_limit_order(Id::new_uuid(), 1000, 20, Side::Buy, TimeInForce::Gtc, None);
        assert!(result.is_ok());
    }

    // --- Lot size error display ---

    #[test]
    fn test_invalid_lot_size_error_display() {
        let book: OrderBook<()> = OrderBook::with_lot_size("BTC/USD", 10);
        let order = make_standard_order(1000, 15, Side::Buy);
        let err = book.add_order(order).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("15"), "Should contain quantity: {msg}");
        assert!(msg.contains("10"), "Should contain lot size: {msg}");
    }

    // --- Combined tick + lot validation ---

    #[test]
    fn test_tick_and_lot_size_both_valid() {
        let mut book: OrderBook<()> = OrderBook::with_tick_size("BTC/USD", 100);
        book.set_lot_size(10);
        let order = make_standard_order(1000, 50, Side::Buy);
        assert!(book.add_order(order).is_ok());
    }

    #[test]
    fn test_tick_valid_lot_invalid() {
        let mut book: OrderBook<()> = OrderBook::with_tick_size("BTC/USD", 100);
        book.set_lot_size(10);
        let order = make_standard_order(1000, 15, Side::Buy);
        let result = book.add_order(order);
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(msg.contains("lot size"), "Should fail on lot size: {msg}");
    }

    #[test]
    fn test_tick_invalid_lot_valid() {
        let mut book: OrderBook<()> = OrderBook::with_tick_size("BTC/USD", 100);
        book.set_lot_size(10);
        let order = make_standard_order(150, 50, Side::Buy);
        let result = book.add_order(order);
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(
            msg.contains("tick size"),
            "Should fail on tick size first: {msg}"
        );
    }

    // =========================================================================
    // Min/Max Order Size Validation Tests
    // =========================================================================

    // --- Setters and getters ---

    #[test]
    fn test_set_min_order_size() {
        let mut book: OrderBook<()> = OrderBook::new("BTC/USD");
        assert_eq!(book.min_order_size(), None);
        book.set_min_order_size(10);
        assert_eq!(book.min_order_size(), Some(10));
    }

    #[test]
    fn test_set_max_order_size() {
        let mut book: OrderBook<()> = OrderBook::new("BTC/USD");
        assert_eq!(book.max_order_size(), None);
        book.set_max_order_size(1000);
        assert_eq!(book.max_order_size(), Some(1000));
    }

    // --- Backward compatibility: no min/max ---

    #[test]
    fn test_no_min_max_accepts_any_quantity() {
        let book: OrderBook<()> = OrderBook::new("BTC/USD");
        let order = make_standard_order(1000, 1, Side::Buy);
        assert!(book.add_order(order).is_ok());
    }

    // --- Min order size ---

    #[test]
    fn test_min_order_size_rejects_below() {
        let mut book: OrderBook<()> = OrderBook::new("BTC/USD");
        book.set_min_order_size(10);
        let order = make_standard_order(1000, 5, Side::Buy);
        let result = book.add_order(order);
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(
            msg.contains("out of range"),
            "Error should mention out of range: {msg}"
        );
    }

    #[test]
    fn test_min_order_size_accepts_equal() {
        let mut book: OrderBook<()> = OrderBook::new("BTC/USD");
        book.set_min_order_size(10);
        let order = make_standard_order(1000, 10, Side::Buy);
        assert!(book.add_order(order).is_ok());
    }

    #[test]
    fn test_min_order_size_accepts_above() {
        let mut book: OrderBook<()> = OrderBook::new("BTC/USD");
        book.set_min_order_size(10);
        let order = make_standard_order(1000, 50, Side::Buy);
        assert!(book.add_order(order).is_ok());
    }

    // --- Max order size ---

    #[test]
    fn test_max_order_size_rejects_above() {
        let mut book: OrderBook<()> = OrderBook::new("BTC/USD");
        book.set_max_order_size(100);
        let order = make_standard_order(1000, 150, Side::Buy);
        let result = book.add_order(order);
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(
            msg.contains("out of range"),
            "Error should mention out of range: {msg}"
        );
    }

    #[test]
    fn test_max_order_size_accepts_equal() {
        let mut book: OrderBook<()> = OrderBook::new("BTC/USD");
        book.set_max_order_size(100);
        let order = make_standard_order(1000, 100, Side::Buy);
        assert!(book.add_order(order).is_ok());
    }

    #[test]
    fn test_max_order_size_accepts_below() {
        let mut book: OrderBook<()> = OrderBook::new("BTC/USD");
        book.set_max_order_size(100);
        let order = make_standard_order(1000, 50, Side::Buy);
        assert!(book.add_order(order).is_ok());
    }

    // --- Only min set, no max ---

    #[test]
    fn test_only_min_set_large_quantity_accepted() {
        let mut book: OrderBook<()> = OrderBook::new("BTC/USD");
        book.set_min_order_size(10);
        let order = make_standard_order(1000, 999_999, Side::Buy);
        assert!(book.add_order(order).is_ok());
    }

    // --- Only max set, no min ---

    #[test]
    fn test_only_max_set_small_quantity_accepted() {
        let mut book: OrderBook<()> = OrderBook::new("BTC/USD");
        book.set_max_order_size(1000);
        let order = make_standard_order(1000, 1, Side::Buy);
        assert!(book.add_order(order).is_ok());
    }

    // --- Both min and max set ---

    #[test]
    fn test_min_and_max_within_range() {
        let mut book: OrderBook<()> = OrderBook::new("BTC/USD");
        book.set_min_order_size(10);
        book.set_max_order_size(100);
        let order = make_standard_order(1000, 50, Side::Buy);
        assert!(book.add_order(order).is_ok());
    }

    #[test]
    fn test_min_and_max_below_min() {
        let mut book: OrderBook<()> = OrderBook::new("BTC/USD");
        book.set_min_order_size(10);
        book.set_max_order_size(100);
        let order = make_standard_order(1000, 5, Side::Buy);
        assert!(book.add_order(order).is_err());
    }

    #[test]
    fn test_min_and_max_above_max() {
        let mut book: OrderBook<()> = OrderBook::new("BTC/USD");
        book.set_min_order_size(10);
        book.set_max_order_size(100);
        let order = make_standard_order(1000, 150, Side::Buy);
        assert!(book.add_order(order).is_err());
    }

    // --- Iceberg total quantity checked ---

    #[test]
    fn test_max_order_size_iceberg_total_above_max() {
        let mut book: OrderBook<()> = OrderBook::new("BTC/USD");
        book.set_max_order_size(50);
        // default iceberg: visible=10, hidden=90 → total=100 > 50
        let order = make_iceberg_order(1000, Side::Buy);
        assert!(book.add_order(order).is_err());
    }

    #[test]
    fn test_min_order_size_iceberg_total_accepted() {
        let mut book: OrderBook<()> = OrderBook::new("BTC/USD");
        book.set_min_order_size(50);
        // default iceberg: visible=10, hidden=90 → total=100 >= 50
        let order = make_iceberg_order(1000, Side::Buy);
        assert!(book.add_order(order).is_ok());
    }

    // --- Post-only order ---

    #[test]
    fn test_min_order_size_rejects_post_only() {
        let mut book: OrderBook<()> = OrderBook::new("BTC/USD");
        book.set_min_order_size(200);
        // make_post_only_order uses quantity=100
        let order = make_post_only_order(1000, Side::Buy);
        assert!(book.add_order(order).is_err());
    }

    // --- Sell side ---

    #[test]
    fn test_min_order_size_rejects_sell_below() {
        let mut book: OrderBook<()> = OrderBook::new("BTC/USD");
        book.set_min_order_size(10);
        let order = make_standard_order(1000, 5, Side::Sell);
        assert!(book.add_order(order).is_err());
    }

    // --- Dynamic changes ---

    #[test]
    fn test_set_min_max_changes_validation() {
        let mut book: OrderBook<()> = OrderBook::new("BTC/USD");

        // No limits — any quantity accepted
        let order = make_standard_order(1000, 1, Side::Buy);
        assert!(book.add_order(order).is_ok());

        // Set min — 1 would now fail
        book.set_min_order_size(10);
        let order = make_standard_order(2000, 1, Side::Sell);
        assert!(book.add_order(order).is_err());

        // Set max — large orders fail
        book.set_max_order_size(100);
        let order = make_standard_order(3000, 200, Side::Buy);
        assert!(book.add_order(order).is_err());

        // Within range works
        let order = make_standard_order(3000, 50, Side::Buy);
        assert!(book.add_order(order).is_ok());
    }

    // --- add_limit_order convenience method ---

    #[test]
    fn test_add_limit_order_respects_min_order_size() {
        let mut book: OrderBook<()> = OrderBook::new("BTC/USD");
        book.set_min_order_size(10);
        let result =
            book.add_limit_order(Id::new_uuid(), 1000, 5, Side::Buy, TimeInForce::Gtc, None);
        assert!(result.is_err());
    }

    #[test]
    fn test_add_limit_order_respects_max_order_size() {
        let mut book: OrderBook<()> = OrderBook::new("BTC/USD");
        book.set_max_order_size(100);
        let result =
            book.add_limit_order(Id::new_uuid(), 1000, 150, Side::Buy, TimeInForce::Gtc, None);
        assert!(result.is_err());
    }

    // --- Error display ---

    #[test]
    fn test_order_size_out_of_range_error_display() {
        let mut book: OrderBook<()> = OrderBook::new("BTC/USD");
        book.set_min_order_size(10);
        book.set_max_order_size(100);
        let order = make_standard_order(1000, 5, Side::Buy);
        let err = book.add_order(order).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("5"), "Should contain quantity: {msg}");
        assert!(msg.contains("10"), "Should contain min: {msg}");
        assert!(msg.contains("100"), "Should contain max: {msg}");
    }

    // --- Combined tick + lot + min/max ---

    #[test]
    fn test_all_validations_pass() {
        let mut book: OrderBook<()> = OrderBook::with_tick_size("BTC/USD", 100);
        book.set_lot_size(10);
        book.set_min_order_size(10);
        book.set_max_order_size(1000);
        let order = make_standard_order(1000, 100, Side::Buy);
        assert!(book.add_order(order).is_ok());
    }

    #[test]
    fn test_tick_fails_before_min_max() {
        let mut book: OrderBook<()> = OrderBook::with_tick_size("BTC/USD", 100);
        book.set_min_order_size(10);
        let order = make_standard_order(150, 50, Side::Buy);
        let result = book.add_order(order);
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(msg.contains("tick size"), "Should fail on tick: {msg}");
    }

    #[test]
    fn test_lot_fails_before_min_max() {
        let mut book: OrderBook<()> = OrderBook::new("BTC/USD");
        book.set_lot_size(10);
        book.set_min_order_size(5);
        // quantity 7 fails lot (not multiple of 10), but is above min 5
        let order = make_standard_order(1000, 7, Side::Buy);
        let result = book.add_order(order);
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(msg.contains("lot size"), "Should fail on lot: {msg}");
    }
}
