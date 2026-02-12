//! Tests for Self-Trade Prevention (STP) feature.

#[cfg(test)]
mod tests {
    use crate::orderbook::book::OrderBook;
    use crate::orderbook::error::OrderBookError;
    use crate::orderbook::stp::STPMode;
    use pricelevel::{Hash32, OrderId, OrderType, Side, TimeInForce};

    /// Helper: create a non-zero user hash from a single byte value.
    fn user(byte: u8) -> Hash32 {
        Hash32::new([byte; 32])
    }

    /// Helper: add a resting sell order with a specific user_id.
    fn add_sell_order_with_user(
        book: &OrderBook<()>,
        price: u128,
        quantity: u64,
        user_id: Hash32,
    ) -> OrderId {
        let order = OrderType::Standard {
            id: OrderId::new(),
            price,
            quantity,
            side: Side::Sell,
            user_id,
            timestamp: crate::utils::current_time_millis(),
            time_in_force: TimeInForce::Gtc,
            extra_fields: (),
        };
        let id = order.id();
        let result = book.add_order(order);
        assert!(result.is_ok(), "failed to add sell order: {result:?}");
        id
    }

    /// Helper: add a resting buy order with a specific user_id.
    fn add_buy_order_with_user(
        book: &OrderBook<()>,
        price: u128,
        quantity: u64,
        user_id: Hash32,
    ) -> OrderId {
        let order = OrderType::Standard {
            id: OrderId::new(),
            price,
            quantity,
            side: Side::Buy,
            user_id,
            timestamp: crate::utils::current_time_millis(),
            time_in_force: TimeInForce::Gtc,
            extra_fields: (),
        };
        let id = order.id();
        let result = book.add_order(order);
        assert!(result.is_ok(), "failed to add buy order: {result:?}");
        id
    }

    // -----------------------------------------------------------------------
    // STPMode::None — backward compatibility
    // -----------------------------------------------------------------------

    #[test]
    fn test_stp_none_orders_match_normally() {
        let book: OrderBook<()> = OrderBook::new("TEST");
        // default is STPMode::None
        assert_eq!(book.stp_mode(), STPMode::None);

        let same_user = user(1);
        add_sell_order_with_user(&book, 100, 10, same_user);

        // Same user submits a buy market order — should match (no STP)
        let taker_id = OrderId::new();
        let result = book.match_market_order_with_user(taker_id, 10, Side::Buy, same_user);
        assert!(result.is_ok());
        let mr = result.unwrap();
        assert!(mr.is_complete);
        assert_eq!(mr.executed_quantity(), 10);
    }

    // -----------------------------------------------------------------------
    // STPMode::CancelTaker
    // -----------------------------------------------------------------------

    #[test]
    fn test_cancel_taker_prevents_self_trade() {
        let mut book: OrderBook<()> = OrderBook::new("TEST");
        book.set_stp_mode(STPMode::CancelTaker);

        let same_user = user(1);
        let maker_id = add_sell_order_with_user(&book, 100, 10, same_user);

        // Same user tries to buy — STP should block
        let taker_id = OrderId::new();
        let result = book.match_market_order_with_user(taker_id, 10, Side::Buy, same_user);

        match result {
            Err(OrderBookError::SelfTradePrevented { mode, .. }) => {
                assert_eq!(mode, STPMode::CancelTaker);
            }
            other => panic!("expected SelfTradePrevented, got {other:?}"),
        }

        // Maker order should still be in the book
        assert!(book.get_order(maker_id).is_some());
        assert_eq!(book.best_ask(), Some(100));
    }

    #[test]
    fn test_cancel_taker_allows_different_users() {
        let mut book: OrderBook<()> = OrderBook::new("TEST");
        book.set_stp_mode(STPMode::CancelTaker);

        let maker_user = user(1);
        let taker_user = user(2);
        add_sell_order_with_user(&book, 100, 10, maker_user);

        // Different user buys — should match normally
        let taker_id = OrderId::new();
        let result = book.match_market_order_with_user(taker_id, 10, Side::Buy, taker_user);
        assert!(result.is_ok());
        let mr = result.unwrap();
        assert!(mr.is_complete);
        assert_eq!(mr.executed_quantity(), 10);
    }

    #[test]
    fn test_cancel_taker_partial_fill_before_self_trade() {
        let mut book: OrderBook<()> = OrderBook::new("TEST");
        book.set_stp_mode(STPMode::CancelTaker);

        let taker_user = user(1);
        let other_user = user(2);

        // Use different price levels to guarantee execution order:
        // Level 100: other user (qty 5) — matched first (best ask)
        // Level 200: same user (qty 10) — STP triggers here
        add_sell_order_with_user(&book, 100, 5, other_user);
        add_sell_order_with_user(&book, 200, 10, taker_user);

        // Taker tries to buy 20 — should fill 5 at price 100, then STP at 200
        let taker_id = OrderId::new();
        let result = book.match_market_order_with_user(taker_id, 20, Side::Buy, taker_user);

        // Should succeed with partial fill (STP only returns error when zero fills)
        assert!(result.is_ok(), "expected Ok, got: {result:?}");
        let mr = result.unwrap();
        assert_eq!(mr.executed_quantity(), 5);
        assert!(!mr.is_complete);
    }

    #[test]
    fn test_cancel_taker_zero_taker_user_bypasses_stp_during_matching() {
        let mut book: OrderBook<()> = OrderBook::new("TEST");
        book.set_stp_mode(STPMode::CancelTaker);

        let maker_user = user(1);
        add_sell_order_with_user(&book, 100, 10, maker_user);

        // Matching with Hash32::zero() as taker_user_id should bypass STP
        let taker_id = OrderId::new();
        let result = book.match_market_order_with_user(taker_id, 10, Side::Buy, Hash32::zero());
        assert!(result.is_ok());
        let mr = result.unwrap();
        assert!(mr.is_complete);
    }

    // -----------------------------------------------------------------------
    // STPMode::CancelMaker
    // -----------------------------------------------------------------------

    #[test]
    fn test_cancel_maker_removes_same_user_orders() {
        let mut book: OrderBook<()> = OrderBook::new("TEST");
        book.set_stp_mode(STPMode::CancelMaker);

        let same_user = user(1);
        let other_user = user(2);

        // Same user resting sell at 100 (qty 5)
        let maker_id = add_sell_order_with_user(&book, 100, 5, same_user);
        // Other user resting sell at 100 (qty 10)
        add_sell_order_with_user(&book, 100, 10, other_user);

        // Same user buys 10 — maker should be cancelled, match against other
        let taker_id = OrderId::new();
        let result = book.match_market_order_with_user(taker_id, 10, Side::Buy, same_user);
        assert!(result.is_ok());
        let mr = result.unwrap();
        assert_eq!(mr.executed_quantity(), 10);
        assert!(mr.is_complete);

        // Same user's maker order should be gone
        assert!(book.get_order(maker_id).is_none());
    }

    #[test]
    fn test_cancel_maker_all_same_user_orders_cancelled() {
        let mut book: OrderBook<()> = OrderBook::new("TEST");
        book.set_stp_mode(STPMode::CancelMaker);

        let same_user = user(1);

        // Only same-user orders at this level
        let maker_id1 = add_sell_order_with_user(&book, 100, 5, same_user);
        let maker_id2 = add_sell_order_with_user(&book, 100, 3, same_user);

        // Taker tries to buy — all makers cancelled, level empty, no fills
        let taker_id = OrderId::new();
        let result = book.match_market_order_with_user(taker_id, 10, Side::Buy, same_user);

        // Market order with no liquidity after cancellations returns InsufficientLiquidity
        match result {
            Err(OrderBookError::InsufficientLiquidity { .. }) => {}
            other => panic!("expected InsufficientLiquidity, got {other:?}"),
        }

        // Both makers should be gone
        assert!(book.get_order(maker_id1).is_none());
        assert!(book.get_order(maker_id2).is_none());
        assert_eq!(book.best_ask(), None);
    }

    #[test]
    fn test_cancel_maker_across_price_levels() {
        let mut book: OrderBook<()> = OrderBook::new("TEST");
        book.set_stp_mode(STPMode::CancelMaker);

        let same_user = user(1);
        let other_user = user(2);

        // Level 100: same user (qty 5)
        let maker1 = add_sell_order_with_user(&book, 100, 5, same_user);
        // Level 200: other user (qty 10)
        add_sell_order_with_user(&book, 200, 10, other_user);

        // Buy 10 — cancel maker at 100, then match at 200
        let taker_id = OrderId::new();
        let result = book.match_market_order_with_user(taker_id, 10, Side::Buy, same_user);
        assert!(result.is_ok());
        let mr = result.unwrap();
        assert_eq!(mr.executed_quantity(), 10);

        // Maker at 100 should be gone
        assert!(book.get_order(maker1).is_none());
    }

    // -----------------------------------------------------------------------
    // STPMode::CancelBoth
    // -----------------------------------------------------------------------

    #[test]
    fn test_cancel_both_cancels_maker_and_taker() {
        let mut book: OrderBook<()> = OrderBook::new("TEST");
        book.set_stp_mode(STPMode::CancelBoth);

        let same_user = user(1);
        let maker_id = add_sell_order_with_user(&book, 100, 10, same_user);

        // Same user tries to buy — both should be cancelled
        let taker_id = OrderId::new();
        let result = book.match_market_order_with_user(taker_id, 10, Side::Buy, same_user);

        match result {
            Err(OrderBookError::SelfTradePrevented { mode, .. }) => {
                assert_eq!(mode, STPMode::CancelBoth);
            }
            other => panic!("expected SelfTradePrevented, got {other:?}"),
        }

        // Maker should be cancelled too
        assert!(book.get_order(maker_id).is_none());
    }

    #[test]
    fn test_cancel_both_partial_fill_before_self() {
        let mut book: OrderBook<()> = OrderBook::new("TEST");
        book.set_stp_mode(STPMode::CancelBoth);

        let taker_user = user(1);
        let other_user = user(2);

        // Use different price levels to guarantee execution order:
        // Level 100: other user (qty 3) — matched first (best ask)
        // Level 200: same user (qty 10) — CancelBoth triggers here
        add_sell_order_with_user(&book, 100, 3, other_user);
        let maker_id = add_sell_order_with_user(&book, 200, 10, taker_user);

        // Taker buys 20 — fills 3 at 100, then CancelBoth at 200
        let taker_id = OrderId::new();
        let result = book.match_market_order_with_user(taker_id, 20, Side::Buy, taker_user);

        // Partial fill occurred, so result is Ok (not error)
        assert!(result.is_ok());
        let mr = result.unwrap();
        assert_eq!(mr.executed_quantity(), 3);
        assert!(!mr.is_complete);

        // Same-user maker should be cancelled
        assert!(book.get_order(maker_id).is_none());
    }

    // -----------------------------------------------------------------------
    // STP with add_order (limit order crossing)
    // -----------------------------------------------------------------------

    #[test]
    fn test_stp_cancel_taker_via_add_order() {
        let mut book: OrderBook<()> = OrderBook::new("TEST");
        book.set_stp_mode(STPMode::CancelTaker);

        let same_user = user(1);
        add_sell_order_with_user(&book, 100, 10, same_user);

        // Same user adds an aggressive buy that would cross
        let order = OrderType::Standard {
            id: OrderId::new(),
            price: 100,
            quantity: 10,
            side: Side::Buy,
            user_id: same_user,
            timestamp: crate::utils::current_time_millis(),
            time_in_force: TimeInForce::Gtc,
            extra_fields: (),
        };

        let result = book.add_order(order);
        match result {
            Err(OrderBookError::SelfTradePrevented { mode, .. }) => {
                assert_eq!(mode, STPMode::CancelTaker);
            }
            other => panic!("expected SelfTradePrevented, got {other:?}"),
        }

        // Ask side unchanged
        assert_eq!(book.best_ask(), Some(100));
    }

    #[test]
    fn test_stp_cancel_maker_via_add_order() {
        let mut book: OrderBook<()> = OrderBook::new("TEST");
        book.set_stp_mode(STPMode::CancelMaker);

        let same_user = user(1);
        let other_user = user(2);

        // Same user sell at 100 (qty 5)
        let maker_id = add_sell_order_with_user(&book, 100, 5, same_user);
        // Other user sell at 100 (qty 10)
        add_sell_order_with_user(&book, 100, 10, other_user);

        // Same user adds aggressive buy at 100 for qty 8
        let order = OrderType::Standard {
            id: OrderId::new(),
            price: 100,
            quantity: 8,
            side: Side::Buy,
            user_id: same_user,
            timestamp: crate::utils::current_time_millis(),
            time_in_force: TimeInForce::Gtc,
            extra_fields: (),
        };

        let result = book.add_order(order);
        assert!(result.is_ok());

        // Same-user maker should be gone
        assert!(book.get_order(maker_id).is_none());
    }

    // -----------------------------------------------------------------------
    // Sell-side STP (taker selling into bids)
    // -----------------------------------------------------------------------

    #[test]
    fn test_stp_cancel_taker_sell_side() {
        let mut book: OrderBook<()> = OrderBook::new("TEST");
        book.set_stp_mode(STPMode::CancelTaker);

        let same_user = user(1);
        let maker_id = add_buy_order_with_user(&book, 100, 10, same_user);

        // Same user tries to sell — STP should block
        let taker_id = OrderId::new();
        let result = book.match_market_order_with_user(taker_id, 10, Side::Sell, same_user);

        match result {
            Err(OrderBookError::SelfTradePrevented { mode, .. }) => {
                assert_eq!(mode, STPMode::CancelTaker);
            }
            other => panic!("expected SelfTradePrevented, got {other:?}"),
        }

        // Maker should still be in the book
        assert!(book.get_order(maker_id).is_some());
        assert_eq!(book.best_bid(), Some(100));
    }

    #[test]
    fn test_stp_cancel_maker_sell_side() {
        let mut book: OrderBook<()> = OrderBook::new("TEST");
        book.set_stp_mode(STPMode::CancelMaker);

        let same_user = user(1);
        let other_user = user(2);

        // Same user buy at 200 (qty 5)
        let maker_id = add_buy_order_with_user(&book, 200, 5, same_user);
        // Other user buy at 200 (qty 10)
        add_buy_order_with_user(&book, 200, 10, other_user);

        // Same user sells 10 — maker cancelled, match against other
        let taker_id = OrderId::new();
        let result = book.match_market_order_with_user(taker_id, 10, Side::Sell, same_user);
        assert!(result.is_ok());
        let mr = result.unwrap();
        assert_eq!(mr.executed_quantity(), 10);

        // Same user maker gone
        assert!(book.get_order(maker_id).is_none());
    }

    // -----------------------------------------------------------------------
    // Setter / getter / constructor
    // -----------------------------------------------------------------------

    #[test]
    fn test_stp_mode_setter_getter() {
        let mut book: OrderBook<()> = OrderBook::new("TEST");
        assert_eq!(book.stp_mode(), STPMode::None);

        book.set_stp_mode(STPMode::CancelTaker);
        assert_eq!(book.stp_mode(), STPMode::CancelTaker);

        book.set_stp_mode(STPMode::CancelMaker);
        assert_eq!(book.stp_mode(), STPMode::CancelMaker);

        book.set_stp_mode(STPMode::CancelBoth);
        assert_eq!(book.stp_mode(), STPMode::CancelBoth);

        book.set_stp_mode(STPMode::None);
        assert_eq!(book.stp_mode(), STPMode::None);
    }

    #[test]
    fn test_with_stp_mode_constructor() {
        let book: OrderBook<()> = OrderBook::with_stp_mode("TEST", STPMode::CancelBoth);
        assert_eq!(book.stp_mode(), STPMode::CancelBoth);
        assert_eq!(book.symbol(), "TEST");
    }

    // -----------------------------------------------------------------------
    // Edge cases
    // -----------------------------------------------------------------------

    #[test]
    fn test_stp_empty_book_returns_insufficient_liquidity() {
        let mut book: OrderBook<()> = OrderBook::new("TEST");
        book.set_stp_mode(STPMode::CancelTaker);

        let taker_id = OrderId::new();
        let result = book.match_market_order_with_user(taker_id, 10, Side::Buy, user(1));

        match result {
            Err(OrderBookError::InsufficientLiquidity { .. }) => {}
            other => panic!("expected InsufficientLiquidity, got {other:?}"),
        }
    }

    #[test]
    fn test_stp_limit_order_no_cross_adds_to_book() {
        let mut book: OrderBook<()> = OrderBook::new("TEST");
        book.set_stp_mode(STPMode::CancelTaker);

        let same_user = user(1);
        add_sell_order_with_user(&book, 200, 10, same_user);

        // Same user adds buy at 100 (no cross) — should rest in book
        let order = OrderType::Standard {
            id: OrderId::new(),
            price: 100,
            quantity: 5,
            side: Side::Buy,
            user_id: same_user,
            timestamp: crate::utils::current_time_millis(),
            time_in_force: TimeInForce::Gtc,
            extra_fields: (),
        };

        let result = book.add_order(order);
        assert!(result.is_ok());
        assert_eq!(book.best_bid(), Some(100));
    }

    #[test]
    fn test_stp_submit_market_order_with_user() {
        let mut book: OrderBook<()> = OrderBook::new("TEST");
        book.set_stp_mode(STPMode::CancelTaker);

        let same_user = user(1);
        add_sell_order_with_user(&book, 100, 10, same_user);

        // Use the submit_market_order_with_user convenience method
        let taker_id = OrderId::new();
        let result = book.submit_market_order_with_user(taker_id, 10, Side::Buy, same_user);

        match result {
            Err(OrderBookError::SelfTradePrevented { mode, .. }) => {
                assert_eq!(mode, STPMode::CancelTaker);
            }
            other => panic!("expected SelfTradePrevented, got {other:?}"),
        }
    }

    #[test]
    fn test_stp_match_limit_order_with_user() {
        let mut book: OrderBook<()> = OrderBook::new("TEST");
        book.set_stp_mode(STPMode::CancelTaker);

        let same_user = user(1);
        add_sell_order_with_user(&book, 100, 10, same_user);

        // Use match_limit_order_with_user
        let taker_id = OrderId::new();
        let result = book.match_limit_order_with_user(taker_id, 10, Side::Buy, 100, same_user);

        match result {
            Err(OrderBookError::SelfTradePrevented { mode, .. }) => {
                assert_eq!(mode, STPMode::CancelTaker);
            }
            other => panic!("expected SelfTradePrevented, got {other:?}"),
        }
    }

    #[test]
    fn test_stp_backward_compat_no_user_id() {
        let mut book: OrderBook<()> = OrderBook::new("TEST");
        book.set_stp_mode(STPMode::CancelTaker);

        let same_user = user(1);
        add_sell_order_with_user(&book, 100, 10, same_user);

        // Using the old API (no user_id) should bypass STP
        let taker_id = OrderId::new();
        let result = book.match_market_order(taker_id, 10, Side::Buy);
        assert!(result.is_ok());
        let mr = result.unwrap();
        assert!(mr.is_complete);
    }

    #[test]
    fn test_stp_multiple_levels_cancel_taker() {
        let mut book: OrderBook<()> = OrderBook::new("TEST");
        book.set_stp_mode(STPMode::CancelTaker);

        let taker_user = user(1);
        let other_user = user(2);

        // Level 100: other user (qty 5)
        add_sell_order_with_user(&book, 100, 5, other_user);
        // Level 200: same user (qty 10)
        add_sell_order_with_user(&book, 200, 10, taker_user);

        // Buy 20 — fills 5 at 100, then STP at 200
        let taker_id = OrderId::new();
        let result = book.match_market_order_with_user(taker_id, 20, Side::Buy, taker_user);

        assert!(result.is_ok());
        let mr = result.unwrap();
        assert_eq!(mr.executed_quantity(), 5);
        assert!(!mr.is_complete);
    }

    // -----------------------------------------------------------------------
    // MissingUserId enforcement (issue #9)
    // -----------------------------------------------------------------------

    #[test]
    fn test_missing_user_id_limit_order_stp_enabled() {
        let mut book: OrderBook<()> = OrderBook::new("TEST");
        book.set_stp_mode(STPMode::CancelTaker);

        // add_limit_order defaults to Hash32::zero() → should be rejected
        let result =
            book.add_limit_order(OrderId::new(), 100, 10, Side::Buy, TimeInForce::Gtc, None);
        match result {
            Err(OrderBookError::MissingUserId { .. }) => {}
            other => panic!("expected MissingUserId, got {other:?}"),
        }
    }

    #[test]
    fn test_missing_user_id_limit_order_stp_disabled() {
        let book: OrderBook<()> = OrderBook::new("TEST");
        assert_eq!(book.stp_mode(), STPMode::None);

        // STP disabled → Hash32::zero() is fine
        let result =
            book.add_limit_order(OrderId::new(), 100, 10, Side::Buy, TimeInForce::Gtc, None);
        assert!(result.is_ok());
    }

    #[test]
    fn test_limit_order_with_user_stp_enabled() {
        let mut book: OrderBook<()> = OrderBook::new("TEST");
        book.set_stp_mode(STPMode::CancelTaker);

        // Non-zero user_id → accepted
        let result = book.add_limit_order_with_user(
            OrderId::new(),
            100,
            10,
            Side::Buy,
            TimeInForce::Gtc,
            user(1),
            None,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_limit_order_with_zero_user_stp_enabled() {
        let mut book: OrderBook<()> = OrderBook::new("TEST");
        book.set_stp_mode(STPMode::CancelMaker);

        // Explicitly zero user_id via _with_user → should be rejected
        let result = book.add_limit_order_with_user(
            OrderId::new(),
            100,
            10,
            Side::Buy,
            TimeInForce::Gtc,
            Hash32::zero(),
            None,
        );
        match result {
            Err(OrderBookError::MissingUserId { .. }) => {}
            other => panic!("expected MissingUserId, got {other:?}"),
        }
    }

    #[test]
    fn test_missing_user_id_iceberg_order_stp_enabled() {
        let mut book: OrderBook<()> = OrderBook::new("TEST");
        book.set_stp_mode(STPMode::CancelBoth);

        let result = book.add_iceberg_order(
            OrderId::new(),
            100,
            5,
            10,
            Side::Sell,
            TimeInForce::Gtc,
            None,
        );
        match result {
            Err(OrderBookError::MissingUserId { .. }) => {}
            other => panic!("expected MissingUserId, got {other:?}"),
        }
    }

    #[test]
    fn test_iceberg_order_with_user_stp_enabled() {
        let mut book: OrderBook<()> = OrderBook::new("TEST");
        book.set_stp_mode(STPMode::CancelTaker);

        let result = book.add_iceberg_order_with_user(
            OrderId::new(),
            100,
            5,
            10,
            Side::Sell,
            TimeInForce::Gtc,
            user(2),
            None,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_iceberg_order_stp_disabled() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        let result = book.add_iceberg_order(
            OrderId::new(),
            100,
            5,
            10,
            Side::Sell,
            TimeInForce::Gtc,
            None,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_missing_user_id_post_only_order_stp_enabled() {
        let mut book: OrderBook<()> = OrderBook::new("TEST");
        book.set_stp_mode(STPMode::CancelTaker);

        let result =
            book.add_post_only_order(OrderId::new(), 100, 10, Side::Buy, TimeInForce::Gtc, None);
        match result {
            Err(OrderBookError::MissingUserId { .. }) => {}
            other => panic!("expected MissingUserId, got {other:?}"),
        }
    }

    #[test]
    fn test_post_only_order_with_user_stp_enabled() {
        let mut book: OrderBook<()> = OrderBook::new("TEST");
        book.set_stp_mode(STPMode::CancelMaker);

        let result = book.add_post_only_order_with_user(
            OrderId::new(),
            200,
            10,
            Side::Sell,
            TimeInForce::Gtc,
            user(3),
            None,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_post_only_order_stp_disabled() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        let result =
            book.add_post_only_order(OrderId::new(), 100, 10, Side::Buy, TimeInForce::Gtc, None);
        assert!(result.is_ok());
    }

    #[test]
    fn test_add_order_direct_zero_user_stp_enabled() {
        let mut book: OrderBook<()> = OrderBook::new("TEST");
        book.set_stp_mode(STPMode::CancelTaker);

        // Direct add_order with zero user_id → should be rejected
        let order = OrderType::Standard {
            id: OrderId::new(),
            price: 100,
            quantity: 10,
            side: Side::Buy,
            user_id: Hash32::zero(),
            timestamp: crate::utils::current_time_millis(),
            time_in_force: TimeInForce::Gtc,
            extra_fields: (),
        };
        let result = book.add_order(order);
        match result {
            Err(OrderBookError::MissingUserId { .. }) => {}
            other => panic!("expected MissingUserId, got {other:?}"),
        }
    }

    #[test]
    fn test_add_order_direct_nonzero_user_stp_enabled() {
        let mut book: OrderBook<()> = OrderBook::new("TEST");
        book.set_stp_mode(STPMode::CancelBoth);

        // Direct add_order with non-zero user_id → should be accepted
        let order = OrderType::Standard {
            id: OrderId::new(),
            price: 100,
            quantity: 10,
            side: Side::Buy,
            user_id: user(5),
            timestamp: crate::utils::current_time_millis(),
            time_in_force: TimeInForce::Gtc,
            extra_fields: (),
        };
        let result = book.add_order(order);
        assert!(result.is_ok());
    }

    #[test]
    fn test_missing_user_id_all_stp_modes() {
        // Verify enforcement for every non-None STP mode
        for mode in [
            STPMode::CancelTaker,
            STPMode::CancelMaker,
            STPMode::CancelBoth,
        ] {
            let mut book: OrderBook<()> = OrderBook::new("TEST");
            book.set_stp_mode(mode);

            let result =
                book.add_limit_order(OrderId::new(), 100, 10, Side::Buy, TimeInForce::Gtc, None);
            match result {
                Err(OrderBookError::MissingUserId { .. }) => {}
                other => panic!("expected MissingUserId for mode {mode}, got {other:?}"),
            }
        }
    }

    #[test]
    fn test_missing_user_id_error_contains_order_id() {
        let mut book: OrderBook<()> = OrderBook::new("TEST");
        book.set_stp_mode(STPMode::CancelTaker);

        let oid = OrderId::new();
        let order = OrderType::Standard {
            id: oid,
            price: 100,
            quantity: 10,
            side: Side::Buy,
            user_id: Hash32::zero(),
            timestamp: crate::utils::current_time_millis(),
            time_in_force: TimeInForce::Gtc,
            extra_fields: (),
        };
        let result = book.add_order(order);
        match result {
            Err(OrderBookError::MissingUserId { order_id }) => {
                assert_eq!(order_id, oid);
            }
            other => panic!("expected MissingUserId, got {other:?}"),
        }
    }

    #[test]
    fn test_missing_user_id_display() {
        let oid = OrderId::new();
        let err = OrderBookError::MissingUserId { order_id: oid };
        let msg = err.to_string();
        assert!(msg.contains("missing user_id"));
        assert!(msg.contains("STP"));
    }
}
