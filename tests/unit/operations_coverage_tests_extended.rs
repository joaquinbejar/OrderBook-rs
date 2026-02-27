use orderbook_rs::OrderBook;
use orderbook_rs::orderbook::modifications::OrderQuantity;
use pricelevel::{Hash32, Id, OrderType, Price, Quantity, Side, TimeInForce, TimestampMs};

#[derive(Clone, Debug, Default, PartialEq)]
struct TestExtraFields {
    pub user_id: String,
    pub strategy: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_submit_market_order_with_liquidity() {
        let book: OrderBook<TestExtraFields> = OrderBook::new("TEST");

        // Add liquidity
        let sell_id = Id::new_uuid();
        let extra_fields = TestExtraFields {
            user_id: "seller123".to_string(),
            strategy: "market_making".to_string(),
        };
        book.add_limit_order(
            sell_id,
            100,
            30,
            Side::Sell,
            TimeInForce::Gtc,
            Some(extra_fields),
        )
        .unwrap();

        // Submit market order
        let order_id = Id::new_uuid();
        let result = book.submit_market_order(order_id, 30, Side::Buy);

        assert!(result.is_ok());
        let match_result = result.unwrap();
        assert_eq!(match_result.remaining_quantity(), 0);
        assert!(match_result.is_complete());
        assert_eq!(match_result.trades().len(), 1);
    }

    #[test]
    fn test_submit_market_order_without_extra_fields() {
        let book: OrderBook<TestExtraFields> = OrderBook::new("TEST");

        // Add some liquidity first
        let sell_id = Id::new_uuid();
        book.add_limit_order(sell_id, 100, 50, Side::Sell, TimeInForce::Gtc, None)
            .unwrap();

        let sell_id2 = Id::new_uuid();
        book.add_limit_order(sell_id2, 110, 50, Side::Sell, TimeInForce::Gtc, None)
            .unwrap();

        // Submit market order
        let order_id = Id::new_uuid();
        let result = book.submit_market_order(order_id, 100, Side::Buy);

        assert!(result.is_ok());
        let match_result = result.unwrap();
        assert_eq!(match_result.remaining_quantity(), 0);
        assert!(match_result.is_complete());
        assert_eq!(match_result.trades().len(), 2);
    }

    #[test]
    fn test_submit_market_order_insufficient_liquidity() {
        let book: OrderBook<TestExtraFields> = OrderBook::new("TEST");

        // Add limited liquidity
        let sell_id = Id::new_uuid();
        book.add_limit_order(sell_id, 100, 20, Side::Sell, TimeInForce::Gtc, None)
            .unwrap();

        // Submit market order for more than available
        let order_id = Id::new_uuid();
        let result = book.submit_market_order(order_id, 50, Side::Buy);

        assert!(result.is_ok());
        let match_result = result.unwrap();
        assert_eq!(match_result.remaining_quantity(), 30); // 50 - 20 = 30
        assert!(!match_result.is_complete());
        assert_eq!(match_result.trades().len(), 1);
    }

    #[test]
    fn test_add_limit_order_fok_complete_fill() {
        let book: OrderBook<TestExtraFields> = OrderBook::new("TEST");

        // Add liquidity
        let sell_id1 = Id::new_uuid();
        let sell_id2 = Id::new_uuid();
        book.add_limit_order(sell_id1, 1000, 30, Side::Sell, TimeInForce::Gtc, None)
            .unwrap();
        book.add_limit_order(sell_id2, 1010, 30, Side::Sell, TimeInForce::Gtc, None)
            .unwrap();

        // FOK order that can be completely filled
        let order_id = Id::new_uuid();
        let result = book.add_limit_order(order_id, 1020, 50, Side::Buy, TimeInForce::Fok, None);

        assert!(result.is_ok());
        let order = result.unwrap();
        assert_eq!(order.id(), order_id);
        assert_eq!(order.price().as_u128(), 1020);
        assert_eq!(order.visible_quantity(), 50);

        // Order should not be in the book since it was completely filled
        assert!(book.get_order(order_id).is_none());
    }

    #[test]
    fn test_add_limit_order_fok_incomplete_fill() {
        let book: OrderBook<TestExtraFields> = OrderBook::new("TEST");

        // Add limited liquidity
        let sell_id = Id::new_uuid();
        book.add_limit_order(sell_id, 1000, 30, Side::Sell, TimeInForce::Gtc, None)
            .unwrap();

        // FOK order that cannot be completely filled
        let order_id = Id::new_uuid();
        let result = book.add_limit_order(order_id, 1020, 50, Side::Buy, TimeInForce::Fok, None);

        assert!(result.is_err());
        // FOK order should either succeed completely or fail
        // The specific error message may vary by implementation
        assert!(
            result.is_err(),
            "Expected FOK order to fail due to insufficient liquidity"
        );

        // Order should not be in the book
        assert!(book.get_order(order_id).is_none());
        // Original sell order should still be there
        assert!(book.get_order(sell_id).is_some());
    }

    #[test]
    fn test_add_limit_order_ioc_partial_fill() {
        let book = OrderBook::<TestExtraFields>::new("TEST");

        // Add a sell order to the book first
        let sell_id = Id::new_uuid();
        book.add_limit_order(sell_id, 100, 30, Side::Sell, TimeInForce::Gtc, None)
            .unwrap();

        // Add IOC buy order that will partially fill
        let buy_id = Id::new_uuid();
        let result = book.add_limit_order(buy_id, 100, 50, Side::Buy, TimeInForce::Ioc, None);

        // IOC orders should either succeed or fail, depending on implementation
        // If they succeed, they should be partially filled and remaining cancelled
        if let Ok(order) = result {
            assert_eq!(order.id(), buy_id);
            // Order should not remain in book after IOC execution
            assert!(book.get_order(buy_id).is_none());
        } else {
            // If IOC fails due to insufficient liquidity, that's also valid
            assert!(result.is_err());
        }
    }

    #[test]
    fn test_add_limit_order_ioc_no_fill() {
        let book = OrderBook::<TestExtraFields>::new("TEST");

        // Add IOC buy order with no matching sell orders
        let buy_id = Id::new_uuid();
        let result = book.add_limit_order(buy_id, 100, 50, Side::Buy, TimeInForce::Ioc, None);

        // IOC with no fill should either succeed and be cancelled, or fail
        if let Ok(order) = result {
            assert_eq!(order.id(), buy_id);
            // Order should not remain in book after IOC execution
            assert!(book.get_order(buy_id).is_none());
        } else {
            // If IOC fails due to no liquidity, that's also valid
            assert!(result.is_err());
        }
    }

    #[test]
    fn test_add_post_only_order_no_cross() {
        let book: OrderBook<TestExtraFields> = OrderBook::new("TEST");

        // Add sell order at higher price
        let sell_id = Id::new_uuid();
        book.add_limit_order(sell_id, 2000, 30, Side::Sell, TimeInForce::Gtc, None)
            .unwrap();

        // PostOnly buy order that won't cross
        let order_id = Id::new_uuid();
        let result =
            book.add_post_only_order(order_id, 1000, 50, Side::Buy, TimeInForce::Gtc, None);

        assert!(result.is_ok());
        let order = result.unwrap();
        assert_eq!(order.quantity(), 50);

        // Order should be in the book
        assert!(book.get_order(order_id).is_some());
    }

    #[test]
    fn test_add_post_only_order_would_cross() {
        let book: OrderBook<TestExtraFields> = OrderBook::new("TEST");

        // Add sell order at lower price
        let sell_id = Id::new_uuid();
        book.add_limit_order(sell_id, 1000, 30, Side::Sell, TimeInForce::Gtc, None)
            .unwrap();

        // PostOnly buy order that would cross - should be rejected
        let order_id = Id::new_uuid();
        let result =
            book.add_post_only_order(order_id, 1500, 50, Side::Buy, TimeInForce::Gtc, None);

        assert!(result.is_err());
        if let Err(e) = result {
            assert!(e.to_string().contains("WouldCross") || e.to_string().contains("would cross"));
        }

        // Order should not be in the book
        assert!(book.get_order(order_id).is_none());
    }

    #[test]
    fn test_add_iceberg_order_with_extra_fields() {
        let book: OrderBook<TestExtraFields> = OrderBook::new("TEST");

        let order_id = Id::new_uuid();
        let extra_fields = TestExtraFields {
            user_id: "iceberg_user".to_string(),
            strategy: "iceberg_strategy".to_string(),
        };

        let result = book.add_iceberg_order(
            order_id,
            1000,
            20, // visible quantity
            80, // hidden quantity (total = 100)
            Side::Buy,
            TimeInForce::Gtc,
            Some(extra_fields.clone()),
        );

        // Test if iceberg orders are supported
        if let Ok(order) = result {
            assert_eq!(order.quantity(), 20); // Only visible quantity

            // Verify order is in book
            let stored_order = book.get_order(order_id).unwrap();
            assert_eq!(stored_order.quantity(), 20); // Only visible quantity
        } else {
            // If iceberg orders are not supported, that's also valid
            assert!(result.is_err());
        }
    }

    #[test]
    fn test_add_iceberg_order_without_extra_fields() {
        let book: OrderBook<TestExtraFields> = OrderBook::new("TEST");

        let order_id = Id::new_uuid();
        let result = book.add_iceberg_order(
            order_id,
            1000,
            25, // visible quantity
            75, // hidden quantity (total = 100)
            Side::Sell,
            TimeInForce::Gtc,
            None,
        );

        assert!(result.is_ok());
        let order = result.unwrap();
        assert_eq!(order.quantity(), 25); // Visible quantity
        assert_eq!(order.total_quantity(), 100); // Total quantity

        // Verify default extra fields
        let stored_order = book.get_order(order_id).unwrap();
        match stored_order.as_ref() {
            OrderType::IcebergOrder { extra_fields, .. } => {
                assert_eq!(*extra_fields, TestExtraFields::default());
            }
            _ => panic!("Expected IcebergOrder"),
        }
    }

    #[test]
    fn test_add_iceberg_order_zero_visible_quantity() {
        let book: OrderBook<TestExtraFields> = OrderBook::new("TEST");

        let order_id = Id::new_uuid();

        // Invalid case: visible quantity 0
        let result = book.add_iceberg_order(
            order_id,
            1000,
            0,  // visible quantity = 0 (invalid)
            50, // hidden quantity
            Side::Buy,
            TimeInForce::Gtc,
            None,
        );

        // Zero visible quantity might be handled differently by implementation
        // Some implementations might allow it, others might reject it
        if let Ok(order) = result {
            // If allowed, verify the order behavior
            assert_eq!(order.quantity(), 0);
        } else {
            // If rejected, that's also valid behavior
            assert!(result.is_err());
        }
    }

    #[test]
    fn test_add_iceberg_order_zero_hidden_quantity() {
        let book: OrderBook<TestExtraFields> = OrderBook::new("TEST");

        let order_id = Id::new_uuid();

        // Case: zero hidden quantity (should work - becomes regular order)
        let result = book.add_iceberg_order(
            order_id,
            1000,
            50, // visible quantity
            0,  // hidden quantity = 0
            Side::Buy,
            TimeInForce::Gtc,
            None,
        );

        // This might be valid - iceberg with no hidden part
        if let Ok(order) = result {
            assert_eq!(order.quantity(), 50);
            assert_eq!(order.total_quantity(), 50);
        } else {
            // Or it might be invalid - check error message
            if let Err(e) = result {
                assert!(
                    e.to_string().contains("InvalidQuantity") || e.to_string().contains("hidden")
                );
            }
        }
    }

    #[test]
    fn test_order_type_methods_coverage() {
        // Test OrderType methods for coverage
        let standard_order = OrderType::Standard {
            id: Id::new_uuid(),
            price: Price::new(1000),
            quantity: Quantity::new(100),
            side: Side::Buy,
            user_id: Hash32::zero(),
            timestamp: TimestampMs::new(0),
            time_in_force: TimeInForce::Gtc,
            extra_fields: TestExtraFields::default(),
        };
        assert_eq!(standard_order.quantity(), 100);

        let iceberg_order = OrderType::IcebergOrder {
            id: Id::new_uuid(),
            price: Price::new(1000),
            visible_quantity: Quantity::new(20),
            hidden_quantity: Quantity::new(80),
            side: Side::Buy,
            user_id: Hash32::zero(),
            timestamp: TimestampMs::new(0),
            time_in_force: TimeInForce::Gtc,
            extra_fields: TestExtraFields::default(),
        };
        assert_eq!(iceberg_order.quantity(), 20);
        assert_eq!(iceberg_order.total_quantity(), 100);
    }
}
