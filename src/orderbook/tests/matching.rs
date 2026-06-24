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

    /// Regression for #104: a fully-consumed resting maker records its TRUE
    /// filled quantity in the order-state tracker, not the old `0` placeholder.
    #[test]
    fn test_fully_consumed_maker_records_true_filled_quantity_issue_104() {
        use crate::orderbook::order_state::{OrderStateTracker, OrderStatus};

        let mut book = setup_book();
        book.set_order_state_tracker(OrderStateTracker::new());

        let maker = add_limit_order(&book, Side::Sell, 100, 10);
        let taker = Id::new();
        let result = book.match_order(taker, Side::Buy, 10, None).unwrap();
        assert!(result.is_complete());

        match book.order_status(maker) {
            Some(OrderStatus::Filled { filled_quantity }) => {
                assert_eq!(
                    filled_quantity, 10,
                    "fully-consumed maker must record its true 10-unit fill, not 0"
                );
            }
            other => panic!("expected Filled {{ filled_quantity: 10 }}, got {other:?}"),
        }
    }

    /// #96: the FOK feasibility check is now `lot_size`-aware. Admission already
    /// rejects non-lot-multiple orders, so a lot-rounding *divergence* from raw
    /// depth is not reachable through the public API — but the lot branch in the
    /// faithful feasibility check must still admit a legitimately fillable,
    /// lot-aligned FOK (it must not spuriously kill it).
    #[test]
    fn test_fok_with_lot_size_fills_when_depth_suffices_issue_96() {
        let book: OrderBook<()> = OrderBook::with_lot_size("TEST", 5);
        add_limit_order(&book, Side::Sell, 100, 5);
        add_limit_order(&book, Side::Sell, 101, 5);

        // FOK buy 10 (lot-aligned) at limit 101 — full reachable depth is 10.
        let fok = OrderType::Standard {
            id: Id::new(),
            price: Price::new(101),
            quantity: Quantity::new(10),
            side: Side::Buy,
            user_id: Hash32::zero(),
            time_in_force: TimeInForce::Fok,
            timestamp: TimestampMs::new(0),
            extra_fields: (),
        };
        let result = book.add_order(fok);
        assert!(
            result.is_ok(),
            "lot-aligned FOK with sufficient depth must fill, got {result:?}"
        );
        assert!(book.has_traded.load(std::sync::atomic::Ordering::SeqCst));
        assert!(book.asks.is_empty(), "both ask levels should be consumed");
    }

    /// #96: FOK feasibility must count only *drawable* depth. A non-auto-replenish
    /// reserve's hidden tranche is dropped unfilled by the sweep, so it is not
    /// reachable — a FOK that would need it must be killed before any partial fill.
    #[test]
    fn test_fok_excludes_non_replenish_reserve_hidden_issue_96() {
        use std::num::NonZeroU64;

        let book: OrderBook<()> = OrderBook::new("TEST");
        let reserve_id = Id::new();
        book.add_order(OrderType::ReserveOrder {
            id: reserve_id,
            price: Price::new(100),
            visible_quantity: Quantity::new(5),
            hidden_quantity: Quantity::new(5),
            side: Side::Sell,
            user_id: Hash32::zero(),
            timestamp: TimestampMs::new(0),
            time_in_force: TimeInForce::Gtc,
            replenish_threshold: Quantity::new(0),
            replenish_amount: Some(NonZeroU64::new(5).expect("nonzero")),
            auto_replenish: false,
            extra_fields: (),
        })
        .expect("reserve admitted");

        // FOK buy 10 at price 100. Raw level total is 10 (5 visible + 5 hidden),
        // but only the 5 visible is drawable, so the FOK must be killed with no
        // trade and the reserve untouched. The old raw-depth check let it proceed,
        // fill 5, drop the hidden, and error with the book already mutated.
        let fok = OrderType::Standard {
            id: Id::new(),
            price: Price::new(100),
            quantity: Quantity::new(10),
            side: Side::Buy,
            user_id: Hash32::zero(),
            time_in_force: TimeInForce::Fok,
            timestamp: TimestampMs::new(0),
            extra_fields: (),
        };
        let result = book.add_order(fok);
        assert!(
            matches!(result, Err(OrderBookError::InsufficientLiquidity { .. })),
            "FOK must be killed: a non-replenish reserve's hidden is not drawable, got {result:?}"
        );
        assert!(
            !book.has_traded.load(std::sync::atomic::Ordering::SeqCst),
            "FOK kill must emit no trades"
        );
        assert!(
            book.get_order(reserve_id).is_some(),
            "the reserve must be untouched by a killed FOK"
        );
    }

    /// #136: the non-STP FOK feasibility path now delegates to the upstream
    /// `PriceLevel::matchable_quantity`, which counts an iceberg's replenishable
    /// hidden depth as drawable. A FOK whose size is covered only by visible +
    /// replenished hidden must fill (not be killed) — the positive complement to
    /// the non-replenish-reserve case above.
    #[test]
    fn test_fok_fills_against_iceberg_replenishable_hidden_issue_136() {
        let book: OrderBook<()> = OrderBook::new("TEST");
        book.add_order(OrderType::IcebergOrder {
            id: Id::new(),
            price: Price::new(100),
            visible_quantity: Quantity::new(2),
            hidden_quantity: Quantity::new(8),
            side: Side::Sell,
            user_id: Hash32::zero(),
            timestamp: TimestampMs::new(0),
            time_in_force: TimeInForce::Gtc,
            extra_fields: (),
        })
        .expect("iceberg admitted");

        // FOK buy 10 at price 100. Visible is only 2, but the iceberg replenishes
        // its 8 hidden, so `matchable_quantity` reports 10 drawable → the FOK
        // fills fully and consumes the level.
        let fok = OrderType::Standard {
            id: Id::new(),
            price: Price::new(100),
            quantity: Quantity::new(10),
            side: Side::Buy,
            user_id: Hash32::zero(),
            time_in_force: TimeInForce::Fok,
            timestamp: TimestampMs::new(0),
            extra_fields: (),
        };
        let result = book.add_order(fok);
        assert!(
            result.is_ok(),
            "FOK must fill against an iceberg's replenishable depth, got {result:?}"
        );
        assert!(book.has_traded.load(std::sync::atomic::Ordering::SeqCst));
        assert!(book.asks.is_empty(), "the iceberg is fully consumed");
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
