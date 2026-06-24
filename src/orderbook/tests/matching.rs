//! Unit tests for the `match_order` function in the order book.

#[cfg(test)]
mod tests {
    use crate::orderbook::OrderBookError;
    use crate::orderbook::book::OrderBook;
    use pricelevel::{Hash32, Id, OrderType, Price, Quantity, Side, TimeInForce, TimestampMs};

    // Helper function to create a new order book for testing.
    fn setup_book() -> OrderBook<()> {
        OrderBook::new("TEST_SYMBOL")
    }

    // Helper to add a standard limit order to the book.
    fn add_limit_order(book: &OrderBook, side: Side, price: u128, quantity: u64) -> Id {
        let order = OrderType::Standard {
            id: Id::new(),
            side,
            price: Price::new(price),
            quantity: Quantity::new(quantity),
            user_id: Hash32::zero(),
            time_in_force: TimeInForce::Gtc, // Good-Til-Canceled
            timestamp: TimestampMs::new(0),  // Not relevant for these tests
            extra_fields: (),
        };
        let order_id = order.id();
        book.add_order(order).unwrap();
        order_id
    }

    /// Regression test for #88 (fixed upstream in pricelevel#39 / 0.8.0): a
    /// partially-filled resting maker MUST keep its place at the front of the
    /// price-level queue. Before pricelevel 0.8 a partial fill demoted the
    /// maker behind later same-price arrivals, so the next aggressor matched
    /// the wrong `maker_order_id` — a price-time-priority violation.
    #[test]
    fn test_partial_fill_preserves_price_time_priority_issue_88() {
        let book = setup_book();
        // A arrives first, B second — identical price, so price-time priority
        // means A (and its remainder) is consumed entirely before B.
        let maker_a = add_limit_order(&book, Side::Sell, 100, 10);
        let maker_b = add_limit_order(&book, Side::Sell, 100, 10);

        // First aggressor partially fills A (A: 10 -> 6).
        let r1 = book
            .match_order(Id::new(), Side::Buy, 4, Some(100))
            .unwrap();
        let t1 = r1.trades().as_vec();
        assert_eq!(t1.len(), 1);
        assert_eq!(t1[0].maker_order_id(), maker_a);
        assert_eq!(t1[0].quantity(), Quantity::new(4));

        // Second, separate aggressor must continue consuming A's remainder,
        // NOT jump to the later arrival B (this is the exact #88 failure).
        let r2 = book
            .match_order(Id::new(), Side::Buy, 4, Some(100))
            .unwrap();
        let t2 = r2.trades().as_vec();
        assert_eq!(t2.len(), 1);
        assert_eq!(
            t2[0].maker_order_id(),
            maker_a,
            "partial fill must not demote the resting maker behind later arrivals"
        );

        // Third aggressor exhausts A's last 2 then spills into B: the trade
        // order proves A is fully consumed before B is ever touched.
        let r3 = book
            .match_order(Id::new(), Side::Buy, 5, Some(100))
            .unwrap();
        let t3 = r3.trades().as_vec();
        assert_eq!(t3.len(), 2);
        assert_eq!(t3[0].maker_order_id(), maker_a);
        assert_eq!(t3[0].quantity(), Quantity::new(2));
        assert_eq!(t3[1].maker_order_id(), maker_b);
        assert_eq!(t3[1].quantity(), Quantity::new(3));
    }

    #[test]
    fn test_market_buy_full_match() {
        let book = setup_book();
        add_limit_order(&book, Side::Sell, 100, 50); // Add a sell order

        let taker_order_id = Id::new();
        let result = book
            .match_order(taker_order_id, Side::Buy, 50, None)
            .unwrap();

        assert_eq!(result.remaining_quantity(), Quantity::new(0));
        assert!(result.is_complete());
        assert_eq!(result.trades().as_vec().len(), 1);
        assert_eq!(book.asks.len(), 0); // The ask side should be empty now
        assert!(book.has_traded.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(book.last_trade_price.load(), 100);
    }

    #[test]
    fn test_market_sell_partial_match() {
        let book = setup_book();
        add_limit_order(&book, Side::Buy, 90, 30);

        let taker_order_id = Id::new();
        let result = book
            .match_order(taker_order_id, Side::Sell, 50, None)
            .unwrap();

        assert_eq!(result.remaining_quantity(), Quantity::new(20));
        assert!(!result.is_complete());
        assert_eq!(result.trades().as_vec().len(), 1);
        assert_eq!(book.bids.len(), 0); // The bid side should be empty
    }

    #[test]
    fn test_limit_buy_favorable_price_match() {
        let book = setup_book();
        add_limit_order(&book, Side::Sell, 100, 50);

        // This limit buy order has a favorable price (higher than the ask)
        let taker_order_id = Id::new();
        let result = book
            .match_order(taker_order_id, Side::Buy, 50, Some(105))
            .unwrap();

        assert_eq!(result.remaining_quantity(), Quantity::new(0));
        assert!(result.is_complete());
        assert_eq!(book.asks.len(), 0);
    }

    #[test]
    fn test_limit_sell_unfavorable_price_no_match() {
        let book = setup_book();
        add_limit_order(&book, Side::Buy, 90, 50);

        // This limit sell order has an unfavorable price (higher than the bid)
        let taker_order_id = Id::new();
        let result = book
            .match_order(taker_order_id, Side::Sell, 50, Some(95))
            .unwrap();

        assert_eq!(result.remaining_quantity(), Quantity::new(50));
        assert!(!result.is_complete());
        assert!(result.trades().as_vec().is_empty());
        assert_eq!(book.bids.len(), 1); // The bid side should be unchanged
    }

    #[test]
    fn test_market_order_no_liquidity_error() {
        let book = setup_book();
        let taker_order_id = Id::new();
        let result = book.match_order(taker_order_id, Side::Buy, 50, None);

        assert!(matches!(
            result,
            Err(OrderBookError::InsufficientLiquidity { .. })
        ));
    }

    #[test]
    fn test_match_across_multiple_price_levels() {
        let book = setup_book();
        add_limit_order(&book, Side::Sell, 100, 20);
        add_limit_order(&book, Side::Sell, 101, 30);
        add_limit_order(&book, Side::Sell, 102, 40);

        let taker_order_id = Id::new();
        // Market order to buy 70 shares, should consume the first two levels and part of the third
        let result = book
            .match_order(taker_order_id, Side::Buy, 70, None)
            .unwrap();

        assert_eq!(result.remaining_quantity(), Quantity::new(0));
        assert!(result.is_complete());
        assert_eq!(result.trades().as_vec().len(), 3);
        assert_eq!(book.asks.len(), 1); // One price level should remain

        let remaining_level = book.asks.get(&102).unwrap();
        assert_eq!(remaining_level.value().total_quantity().unwrap_or(0), 20); // 40 - 20 = 20 remaining
        assert_eq!(book.last_trade_price.load(), 102);
    }

    #[test]
    fn test_peek_match_buy_side_full_match() {
        let book: OrderBook<()> = OrderBook::new("TEST");
        book.add_limit_order(Id::new(), 101, 10, Side::Sell, TimeInForce::Gtc, None)
            .unwrap();
        book.add_limit_order(Id::new(), 102, 5, Side::Sell, TimeInForce::Gtc, None)
            .unwrap();

        // Request 15, which is fully available (10 at 101, 5 at 102)
        let matched_quantity = book.peek_match(Side::Buy, 15, None);
        assert_eq!(matched_quantity, 15);
    }

    #[test]
    fn test_peek_match_buy_side_partial_match() {
        let book: OrderBook<()> = OrderBook::new("TEST");
        book.add_limit_order(Id::new(), 101, 10, Side::Sell, TimeInForce::Gtc, None)
            .unwrap();

        // Request 20, but only 10 is available
        let matched_quantity = book.peek_match(Side::Buy, 20, None);
        assert_eq!(matched_quantity, 10);
    }

    #[test]
    fn test_peek_match_sell_side_with_price_limit() {
        let book: OrderBook<()> = OrderBook::new("TEST");
        book.add_limit_order(Id::new(), 98, 10, Side::Buy, TimeInForce::Gtc, None)
            .unwrap();
        book.add_limit_order(Id::new(), 99, 5, Side::Buy, TimeInForce::Gtc, None)
            .unwrap();
        book.add_limit_order(Id::new(), 100, 20, Side::Buy, TimeInForce::Gtc, None)
            .unwrap();

        // Request to sell with a limit of 99. Should only match with bids at 99 and 100.
        let matched_quantity = book.peek_match(Side::Sell, 50, Some(99));
        assert_eq!(matched_quantity, 25); // 5 at 99 + 20 at 100
    }

    #[test]
    fn test_peek_match_no_liquidity() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        // No orders in the book
        let matched_quantity = book.peek_match(Side::Buy, 10, None);
        assert_eq!(matched_quantity, 0);
    }
}
