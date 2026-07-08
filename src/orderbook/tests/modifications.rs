#[cfg(test)]
mod test_order_modifications {

    use crate::{OrderBook, OrderBookError};
    use pricelevel::{Id, OrderType, OrderUpdate, Price, Quantity, Side, TimeInForce};

    // Helper function to create a unique order ID
    fn create_order_id() -> Id {
        Id::new_uuid()
    }

    #[test]
    fn test_update_price_same_value() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        // Add a limit order
        let id = create_order_id();
        let price = 1000;
        let quantity = 10;
        let side = Side::Buy;

        let result = book.add_limit_order(id, price, quantity, side, TimeInForce::Gtc, None);
        assert!(result.is_ok());

        // Try to update to the same price
        let update = OrderUpdate::UpdatePrice {
            order_id: id,
            new_price: Price::new(price),
        };

        let result = book.update_order(update);
        assert!(result.is_err());
        match result {
            Err(OrderBookError::InvalidOperation { message }) => {
                assert!(message.contains("Cannot update price to the same value"));
            }
            _ => panic!("Expected InvalidOperation error"),
        }
    }

    #[test]
    fn test_update_price_and_quantity() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        // Add a limit order
        let id = create_order_id();
        let price = 1000;
        let quantity = 10;
        let side = Side::Buy;

        let result = book.add_limit_order(id, price, quantity, side, TimeInForce::Gtc, None);
        assert!(result.is_ok());

        // Update price and quantity
        let new_price = 1100;
        let new_quantity = 15;
        let update = OrderUpdate::UpdatePriceAndQuantity {
            order_id: id,
            new_price: Price::new(new_price),
            new_quantity: Quantity::new(new_quantity),
        };

        let result = book.update_order(update);
        assert!(result.is_ok());

        // Verify the order was updated
        let updated_order = book.get_order(id);
        assert!(updated_order.is_some());
        let updated_order = updated_order.unwrap();
        assert_eq!(updated_order.price().as_u128(), new_price);
        assert_eq!(
            updated_order.visible_quantity(),
            Quantity::new(new_quantity)
        );
    }

    #[test]
    fn test_cancel_nonexistent_order() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        // Try to cancel a non-existent order
        let id = create_order_id();
        let result = book.cancel_order(id);

        // Should return Ok(None)
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[test]
    fn test_update_order_cancel() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        // Add a limit order
        let id = create_order_id();
        let price = 1000;
        let quantity = 10;
        let side = Side::Buy;

        let result = book.add_limit_order(id, price, quantity, side, TimeInForce::Gtc, None);
        assert!(result.is_ok());

        // Cancel using the OrderUpdate enum
        let update = OrderUpdate::Cancel { order_id: id };

        let result = book.update_order(update);
        assert!(result.is_ok());

        // Order should be removed
        let order = book.get_order(id);
        assert!(order.is_none());
    }

    #[test]
    fn test_update_order_replace() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        // Add a limit order
        let id = create_order_id();
        let price = 1000;
        let quantity = 10;
        let side = Side::Buy;

        let result = book.add_limit_order(id, price, quantity, side, TimeInForce::Gtc, None);
        assert!(result.is_ok());

        // Replace the order
        let new_price = 1100;
        let new_quantity = 15;
        let update = OrderUpdate::Replace {
            order_id: id,
            price: Price::new(new_price),
            quantity: Quantity::new(new_quantity),
            side: Side::Buy,
        };

        let result = book.update_order(update);
        assert!(result.is_ok());

        // Verify the order was replaced
        let replaced_order = book.get_order(id);
        assert!(replaced_order.is_some());
        let replaced_order = replaced_order.unwrap();
        assert_eq!(replaced_order.price().as_u128(), new_price);
        assert_eq!(
            replaced_order.visible_quantity(),
            Quantity::new(new_quantity)
        );
    }

    #[test]
    fn test_replace_with_different_side() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        // Add a limit order
        let id = create_order_id();
        let price = 1000;
        let quantity = 10;
        let side = Side::Buy;

        let result = book.add_limit_order(id, price, quantity, side, TimeInForce::Gtc, None);
        assert!(result.is_ok());

        // Replace with different side
        let update = OrderUpdate::Replace {
            order_id: id,
            price: Price::new(1100),
            quantity: Quantity::new(15),
            side: Side::Sell, // Different side
        };

        let result = book.update_order(update);
        assert!(result.is_ok());

        // Verify the order side was changed
        let replaced_order = book.get_order(id);
        assert!(replaced_order.is_some());
        assert_eq!(replaced_order.unwrap().side(), Side::Sell);
    }

    #[test]
    fn test_iceberg_order_update_quantity() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        // Add an iceberg order
        let id = create_order_id();
        let price = 1000;
        let visible = 10;
        let hidden = 90;
        let side = Side::Buy;

        let result =
            book.add_iceberg_order(id, price, visible, hidden, side, TimeInForce::Gtc, None);
        assert!(result.is_ok());

        // Update visible quantity
        let new_quantity = 15;
        let update = OrderUpdate::UpdateQuantity {
            order_id: id,
            new_quantity: Quantity::new(new_quantity),
        };

        let result = book.update_order(update);
        assert!(result.is_ok());

        // Verify the order's visible quantity was updated
        let updated_order = book.get_order(id);
        assert!(updated_order.is_some());
        let updated_order = updated_order.unwrap();
        assert_eq!(
            updated_order.visible_quantity(),
            Quantity::new(new_quantity)
        );

        // Hidden quantity should remain the same
        match &*updated_order {
            OrderType::IcebergOrder {
                hidden_quantity, ..
            } => {
                assert_eq!(hidden_quantity.as_u64(), hidden);
            }
            _ => panic!("Expected IcebergOrder"),
        }
    }
}

#[cfg(test)]
mod test_modifications_remaining {
    use crate::OrderBook;

    use pricelevel::{
        Hash32, Id, OrderType, OrderUpdate, PegReferenceType, Price, Quantity, Side, TimeInForce,
        TimestampMs,
    };

    fn create_order_id() -> Id {
        Id::new_uuid()
    }

    #[test]
    fn test_update_price_error_cases() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        // Update a non-existent order
        let id = create_order_id();
        let update = OrderUpdate::UpdatePrice {
            order_id: id,
            new_price: Price::new(1000),
        };

        let result = book.update_order(update);
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[test]
    fn test_update_price_and_quantity_nonexistent() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        // Update a non-existent order
        let id = create_order_id();
        let update = OrderUpdate::UpdatePriceAndQuantity {
            order_id: id,
            new_price: Price::new(1000),
            new_quantity: Quantity::new(10),
        };

        let result = book.update_order(update);
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[test]
    fn test_update_order_with_all_types() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        // Add different order types

        // 1. Add a trailing stop order
        let id1 = create_order_id();
        let timestamp = crate::utils::current_time_millis();
        let trail_order = OrderType::TrailingStop {
            id: id1,
            price: Price::new(1000),
            quantity: Quantity::new(10),
            side: Side::Buy,
            user_id: Hash32::zero(),
            timestamp: TimestampMs::new(timestamp),
            time_in_force: TimeInForce::Gtc,
            trail_amount: Quantity::new(5),
            last_reference_price: Price::new(995),
            extra_fields: (),
        };

        // 2. Add a pegged order
        let id2 = create_order_id();
        let peg_order = OrderType::PeggedOrder {
            id: id2,
            price: Price::new(1000),
            quantity: Quantity::new(10),
            side: Side::Buy,
            user_id: Hash32::zero(),
            timestamp: TimestampMs::new(timestamp),
            time_in_force: TimeInForce::Gtc,
            reference_price_offset: 5,
            reference_price_type: PegReferenceType::BestBid,
            extra_fields: (),
        };

        // 3. Add a market to limit order
        let id3 = create_order_id();
        let mtl_order = OrderType::MarketToLimit {
            id: id3,
            price: Price::new(1000),
            quantity: Quantity::new(10),
            side: Side::Buy,
            user_id: Hash32::zero(),
            timestamp: TimestampMs::new(timestamp),
            time_in_force: TimeInForce::Gtc,
            extra_fields: (),
        };

        // 4. Add a reserve order
        let id4 = create_order_id();
        let reserve_order = OrderType::ReserveOrder {
            id: id4,
            price: Price::new(1000),
            visible_quantity: Quantity::new(5),
            hidden_quantity: Quantity::new(5),
            side: Side::Buy,
            user_id: Hash32::zero(),
            timestamp: TimestampMs::new(timestamp),
            time_in_force: TimeInForce::Gtc,
            replenish_threshold: Quantity::new(2),
            replenish_amount: Some(std::num::NonZeroU64::new(3).expect("nonzero")),
            auto_replenish: true,
            extra_fields: (),
        };

        // Add all orders to the book
        let _ = book.add_order(trail_order);
        let _ = book.add_order(peg_order);
        let _ = book.add_order(mtl_order);
        let _ = book.add_order(reserve_order);

        // Test updating all order types

        // 1. Update trailing stop
        let update1 = OrderUpdate::UpdatePriceAndQuantity {
            order_id: id1,
            new_price: Price::new(1010),
            new_quantity: Quantity::new(15),
        };

        // 2. Update pegged order
        let update2 = OrderUpdate::UpdatePriceAndQuantity {
            order_id: id2,
            new_price: Price::new(1010),
            new_quantity: Quantity::new(15),
        };

        // 3. Update market to limit
        let update3 = OrderUpdate::UpdatePriceAndQuantity {
            order_id: id3,
            new_price: Price::new(1010),
            new_quantity: Quantity::new(15),
        };

        // 4. Update reserve order
        let update4 = OrderUpdate::UpdatePriceAndQuantity {
            order_id: id4,
            new_price: Price::new(1010),
            new_quantity: Quantity::new(15),
        };

        // Execute all updates
        let result1 = book.update_order(update1);
        let result2 = book.update_order(update2);
        let result3 = book.update_order(update3);
        let result4 = book.update_order(update4);

        assert!(result1.is_ok());
        assert!(result2.is_ok());
        assert!(result3.is_ok());
        assert!(result4.is_ok());

        // Verify the orders were updated
        let order1 = book.get_order(id1);
        let order2 = book.get_order(id2);
        let order3 = book.get_order(id3);
        let order4 = book.get_order(id4);

        assert!(order1.is_some());
        assert!(order2.is_some());
        assert!(order3.is_some());
        assert!(order4.is_some());

        assert_eq!(order1.unwrap().price().as_u128(), 1010);
        assert_eq!(order2.unwrap().price().as_u128(), 1010);
        assert_eq!(order3.unwrap().price().as_u128(), 1010);
        assert_eq!(order4.unwrap().price().as_u128(), 1010);
    }

    #[test]
    fn test_replace_with_special_order_types() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        // Add a reserve order
        let id = create_order_id();
        let timestamp = crate::utils::current_time_millis();
        let reserve_order = OrderType::ReserveOrder {
            id,
            price: Price::new(1000),
            visible_quantity: Quantity::new(5),
            hidden_quantity: Quantity::new(5),
            side: Side::Buy,
            user_id: Hash32::zero(),
            timestamp: TimestampMs::new(timestamp),
            time_in_force: TimeInForce::Gtc,
            replenish_threshold: Quantity::new(2),
            replenish_amount: Some(std::num::NonZeroU64::new(3).expect("nonzero")),
            auto_replenish: true,
            extra_fields: (),
        };

        let _ = book.add_order(reserve_order);

        // Try to replace with an unsupported type via Replace operation
        let update = OrderUpdate::Replace {
            order_id: id,
            price: Price::new(1010),
            quantity: Quantity::new(15),
            side: Side::Buy,
        };

        let result = book.update_order(update);

        // Should succeed since we're replacing with a standard order
        assert!(result.is_ok());

        // Verify the order was updated
        let updated_order = book.get_order(id);
        assert!(updated_order.is_some());
        assert_eq!(updated_order.clone().unwrap().price().as_u128(), 1010);
        assert_eq!(updated_order.unwrap().visible_quantity(), Quantity::new(15));
    }

    #[test]
    fn test_cancel_order_removes_price_level() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        // Add a limit order
        let id = create_order_id();
        let _ = book.add_limit_order(id, 1000, 10, Side::Buy, TimeInForce::Gtc, None);

        // Cancel the order
        let update = OrderUpdate::Cancel { order_id: id };

        let result = book.update_order(update);
        assert!(result.is_ok());

        // Price level should be removed
        assert_eq!(book.best_bid(), None);

        // Order should be removed from tracking
        assert!(book.get_order(id).is_none());
    }
}

#[cfg(test)]
mod test_modifications_specific {
    use crate::{OrderBook, OrderBookError};
    use pricelevel::{
        Hash32, Id, OrderType, OrderUpdate, PegReferenceType, Price, Quantity, Side, TimeInForce,
        TimestampMs,
    };

    fn create_order_id() -> Id {
        Id::new_uuid()
    }

    #[test]
    fn test_update_price_edge_cases() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        // Update a non-existent order
        let id = create_order_id();
        let update = OrderUpdate::UpdatePrice {
            order_id: id,
            new_price: Price::new(1000),
        };

        // Should return Ok(None) for non-existent order
        let result = book.update_order(update);
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[test]
    fn test_cancel_non_existent_order() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        // Create an ID for an order that doesn't exist
        let id = create_order_id();

        // Cancel via OrderUpdate
        let update = OrderUpdate::Cancel { order_id: id };
        let result = book.update_order(update);

        // Should return Ok(None)
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[test]
    fn test_update_order_when_order_is_not_found() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        // Create a reserve order type
        let id = create_order_id();
        let timestamp = crate::utils::current_time_millis();
        let reserve_order = OrderType::ReserveOrder {
            id,
            price: Price::new(1000),
            visible_quantity: Quantity::new(5),
            hidden_quantity: Quantity::new(5),
            side: Side::Buy,
            user_id: Hash32::zero(),
            timestamp: TimestampMs::new(timestamp),
            time_in_force: TimeInForce::Gtc,
            replenish_threshold: Quantity::new(2),
            replenish_amount: Some(std::num::NonZeroU64::new(3).expect("nonzero")),
            auto_replenish: true,
            extra_fields: (),
        };

        // Add it to the book
        let _ = book.add_order(reserve_order);

        // First, test with an order that doesn't exist
        let nonexistent_id = create_order_id();

        // Test UpdatePrice
        let update = OrderUpdate::UpdatePrice {
            order_id: nonexistent_id,
            new_price: Price::new(1100),
        };
        let result = book.update_order(update);
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());

        // Test UpdateQuantity
        let update = OrderUpdate::UpdateQuantity {
            order_id: nonexistent_id,
            new_quantity: Quantity::new(20),
        };
        let result = book.update_order(update);
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());

        // Test PriceAndQuantity
        let update = OrderUpdate::UpdatePriceAndQuantity {
            order_id: nonexistent_id,
            new_price: Price::new(1100),
            new_quantity: Quantity::new(20),
        };
        let result = book.update_order(update);
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());

        // Test Replace
        let update = OrderUpdate::Replace {
            order_id: nonexistent_id,
            price: Price::new(1100),
            quantity: Quantity::new(20),
            side: Side::Buy,
        };
        let result = book.update_order(update);
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[test]
    fn test_replace_unsupported_order_type() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        // Add an unsupported order type
        let id = create_order_id();
        let timestamp = crate::utils::current_time_millis();

        // Use a PeggedOrder as an example
        let peg_order = OrderType::PeggedOrder {
            id,
            price: Price::new(1000),
            quantity: Quantity::new(10),
            side: Side::Buy,
            user_id: Hash32::zero(),
            timestamp: TimestampMs::new(timestamp),
            time_in_force: TimeInForce::Gtc,
            reference_price_offset: 5,
            reference_price_type: PegReferenceType::BestBid,
            extra_fields: (),
        };

        let _ = book.add_order(peg_order);

        // Try to replace it
        let update = OrderUpdate::Replace {
            order_id: id,
            price: Price::new(1100),
            quantity: Quantity::new(20),
            side: Side::Buy,
        };

        let result = book.update_order(update);

        // Check if we get the expected error
        match result {
            Err(OrderBookError::InvalidOperation { message }) => {
                assert!(message.contains("Replace operation not supported"));
            }
            Ok(_) => {
                // If it doesn't error, just check the order was updated
                let updated_order = book.get_order(id);
                assert!(updated_order.is_some());
            }
            _ => panic!("Unexpected result"),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::orderbook::OrderBookError;
    use crate::orderbook::book::OrderBook;
    use crate::orderbook::modifications::OrderQuantity;
    use pricelevel::{
        Hash32, Id, OrderType, OrderUpdate, Price, Quantity, Side, TimeInForce, TimestampMs,
    };

    fn setup_book_with_orders() -> OrderBook<()> {
        let book: OrderBook<()> = OrderBook::new("TEST");
        let sell_order = OrderType::Standard {
            id: Id::new(),
            side: Side::Sell,
            price: Price::new(100),
            quantity: Quantity::new(10),
            user_id: Hash32::zero(),
            time_in_force: TimeInForce::Gtc,
            timestamp: TimestampMs::new(0),
            extra_fields: (),
        };
        book.add_order(sell_order).unwrap();

        let buy_order = OrderType::Standard {
            id: Id::new(),
            side: Side::Buy,
            price: Price::new(90),
            quantity: Quantity::new(10),
            user_id: Hash32::zero(),
            time_in_force: TimeInForce::Gtc,
            timestamp: TimestampMs::new(0),
            extra_fields: (),
        };
        book.add_order(buy_order).unwrap();
        book
    }

    #[test]
    fn test_add_post_only_order_crossing_market() {
        let book = setup_book_with_orders();
        let post_only_order = OrderType::PostOnly {
            id: Id::new(),
            side: Side::Buy,
            price: Price::new(100), // This price crosses the best ask (100)
            quantity: Quantity::new(5),
            user_id: Hash32::zero(),
            time_in_force: TimeInForce::Gtc,
            timestamp: TimestampMs::new(0),
            extra_fields: (),
        };

        let result = book.add_order(post_only_order);
        assert!(matches!(result, Err(OrderBookError::PriceCrossing { .. })));
    }

    #[test]
    fn test_add_expired_order() {
        let book: OrderBook<()> = OrderBook::new("TEST");
        book.set_market_close_timestamp(100); // Market closed at timestamp 100

        let expired_order = OrderType::Standard {
            id: Id::new(),
            side: Side::Buy,
            price: Price::new(95),
            quantity: Quantity::new(10),
            user_id: Hash32::zero(),
            time_in_force: TimeInForce::Day,  // Day order
            timestamp: TimestampMs::new(101), // Submitted after market close
            extra_fields: (),
        };

        let result = book.add_order(expired_order);
        assert!(matches!(
            result,
            Err(OrderBookError::InvalidOperation { .. })
        ));
    }

    #[test]
    fn test_successful_cancel_order_removes_level() {
        let book: OrderBook<()> = OrderBook::new("TEST");
        let order_id = Id::new();
        let order = OrderType::Standard {
            id: order_id,
            side: Side::Sell,
            price: Price::new(100),
            quantity: Quantity::new(10),
            user_id: Hash32::zero(),
            time_in_force: TimeInForce::Gtc,
            timestamp: TimestampMs::new(0),
            extra_fields: (),
        };
        book.add_order(order).unwrap();

        assert!(book.asks.contains_key(&100));
        book.cancel_order(order_id).unwrap();
        assert!(!book.asks.contains_key(&100)); // Price level should be gone
        assert!(book.order_locations.get(&order_id).is_none());
    }

    #[test]
    fn test_update_order_not_found() {
        let book: OrderBook<()> = OrderBook::new("TEST");
        let non_existent_id = Id::new();
        let result = book.update_order(OrderUpdate::Cancel {
            order_id: non_existent_id,
        });
        assert!(result.is_ok() && result.unwrap().is_none());
    }

    #[test]
    fn test_update_price_and_quantity() {
        let book = setup_book_with_orders();
        let orders: Vec<_> = book.bids.get(&90).unwrap().value().iter_orders().collect();
        let original_order_id = orders[0].id();

        let result = book.update_order(OrderUpdate::UpdatePriceAndQuantity {
            order_id: original_order_id,
            new_price: Price::new(92),
            new_quantity: Quantity::new(12),
        });

        assert!(result.is_ok());
        let updated_order = book.get_order(original_order_id).unwrap();
        assert_eq!(updated_order.price().as_u128(), 92);
        assert_eq!(updated_order.visible_quantity(), Quantity::new(12));
        assert!(book.bids.contains_key(&92));
        assert!(!book.bids.contains_key(&90));
    }

    #[test]
    fn test_set_quantity_for_reserve_order() {
        let mut order = OrderType::ReserveOrder {
            id: Id::new(),
            side: Side::Buy,
            price: Price::new(100),
            visible_quantity: Quantity::new(10),
            hidden_quantity: Quantity::new(90),
            replenish_amount: Some(std::num::NonZeroU64::new(10).expect("nonzero")),
            auto_replenish: true,
            replenish_threshold: Quantity::new(0),
            user_id: Hash32::zero(),
            time_in_force: TimeInForce::Gtc,
            timestamp: TimestampMs::new(0),
            extra_fields: (),
        };

        // Simulate a partial fill of 15 units
        order.set_quantity(85); // 100 - 15 = 85

        // After the fill, the visible part is consumed and then immediately replenished.
        assert_eq!(order.quantity(), 10); // The visible quantity is replenished to 10.
        assert_eq!(order.total_quantity(), 85); // The total remaining quantity is correct.

        // Verify the internal state of the order
        if let OrderType::ReserveOrder {
            visible_quantity,
            hidden_quantity,
            ..
        } = order
        {
            assert_eq!(visible_quantity, Quantity::new(10));
            assert_eq!(hidden_quantity, Quantity::new(75));
        }
    }
}

#[cfg(test)]
mod test_add_order_with_result {
    use crate::orderbook::modifications::OrderQuantity;
    use crate::orderbook::stp::STPMode;
    use crate::{OrderBook, OrderBookError, TradeListener, TradeResult};
    use pricelevel::{Hash32, Id, OrderType, Price, Quantity, Side, TimeInForce, TimestampMs};
    use std::sync::{Arc, Mutex};

    /// Helper: create a non-zero user hash from a single byte value.
    fn user(byte: u8) -> Hash32 {
        Hash32::new([byte; 32])
    }

    /// Helper: build a standard GTC order.
    fn standard_order(price: u128, quantity: u64, side: Side, user_id: Hash32) -> OrderType<()> {
        OrderType::Standard {
            id: Id::new(),
            price: Price::new(price),
            quantity: Quantity::new(quantity),
            side,
            user_id,
            timestamp: TimestampMs::new(crate::utils::current_time_millis()),
            time_in_force: TimeInForce::Gtc,
            extra_fields: (),
        }
    }

    /// Helper: book whose trade listener captures every emitted `TradeResult`.
    fn book_with_capturing_listener() -> (OrderBook<()>, Arc<Mutex<Vec<TradeResult>>>) {
        let captured: Arc<Mutex<Vec<TradeResult>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&captured);
        let listener: TradeListener = Arc::new(move |tr: &TradeResult| {
            if let Ok(mut guard) = sink.lock() {
                guard.push(tr.clone());
            }
        });
        (OrderBook::with_trade_listener("TEST", listener), captured)
    }

    #[test]
    fn test_add_order_with_result_full_fill_returns_trade_result() {
        let book: OrderBook<()> = OrderBook::new("TEST");
        let maker = user(1);
        let taker = user(2);

        let result = book.add_order(standard_order(100, 5, Side::Sell, maker));
        assert!(result.is_ok(), "failed to rest maker: {result:?}");

        let result = book.add_order_with_result(standard_order(100, 5, Side::Buy, taker));
        let Ok((_, Some(trade_result))) = result else {
            panic!("expected Ok with a trade result, got {result:?}");
        };
        assert_eq!(trade_result.symbol, "TEST");
        assert_eq!(
            trade_result.match_result.executed_quantity().ok(),
            Some(Quantity::new(5)),
            "full fill must execute the entire quantity"
        );
        assert!(trade_result.match_result.is_complete());
    }

    #[test]
    fn test_add_order_with_result_resting_order_returns_none() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        let order = standard_order(100, 10, Side::Buy, user(1));
        let order_id = order.id();
        let result = book.add_order_with_result(order);
        let Ok((added, trade_result)) = result else {
            panic!("expected Ok, got {result:?}");
        };
        assert!(
            trade_result.is_none(),
            "an order that matched nothing must return no trade result"
        );
        assert_eq!(added.id(), order_id);
        assert!(book.get_order(order_id).is_some(), "order must rest");
    }

    #[test]
    fn test_add_order_with_result_partial_fill_rests_remainder() {
        let book: OrderBook<()> = OrderBook::new("TEST");
        let maker = user(1);
        let taker = user(2);

        let result = book.add_order(standard_order(100, 5, Side::Sell, maker));
        assert!(result.is_ok(), "failed to rest maker: {result:?}");

        let result = book.add_order_with_result(standard_order(100, 20, Side::Buy, taker));
        let Ok((added, Some(trade_result))) = result else {
            panic!("expected Ok with a trade result, got {result:?}");
        };
        assert_eq!(
            trade_result.match_result.executed_quantity().ok(),
            Some(Quantity::new(5)),
            "only the resting 5 units can fill"
        );
        assert_eq!(added.quantity(), 15, "the 15-unit remainder must rest");
        assert!(book.get_order(added.id()).is_some());
    }

    #[test]
    fn test_add_order_with_result_listener_receives_same_trade_result() {
        let (book, captured) = book_with_capturing_listener();
        let maker = user(1);
        let taker = user(2);

        let result = book.add_order(standard_order(100, 5, Side::Sell, maker));
        assert!(result.is_ok(), "failed to rest maker: {result:?}");

        let result = book.add_order_with_result(standard_order(100, 5, Side::Buy, taker));
        let Ok((_, Some(trade_result))) = result else {
            panic!("expected Ok with a trade result, got {result:?}");
        };

        let captured = captured.lock().expect("listener mutex poisoned");
        assert_eq!(captured.len(), 1, "listener must fire exactly once");
        assert_eq!(
            captured[0].engine_seq, trade_result.engine_seq,
            "listener and caller must see the same engine_seq"
        );
        assert_eq!(
            captured[0].match_result.executed_quantity().ok(),
            trade_result.match_result.executed_quantity().ok(),
            "listener and caller must see the same fills"
        );
    }

    /// Regression: the trade emission block must run BEFORE the STP
    /// taker-cancel early return. A taker that partially fills against
    /// another user and is then STP-cancelled surfaces the typed error,
    /// but the real (non-self) fills must still reach the trade listener.
    #[test]
    fn test_add_order_stp_cancel_taker_partial_fill_still_reaches_listener() {
        let (mut book, captured) = book_with_capturing_listener();
        book.set_stp_mode(STPMode::CancelTaker);

        let taker_user = user(7);
        let other = user(1);

        // Non-self liquidity at the better price; the taker's own order behind it.
        let result = book.add_order(standard_order(100, 5, Side::Sell, other));
        assert!(result.is_ok(), "failed to rest other maker: {result:?}");
        let result = book.add_order(standard_order(200, 10, Side::Sell, taker_user));
        assert!(result.is_ok(), "failed to rest self maker: {result:?}");

        // GTC buy 20 at limit 200: fills 5 vs `other` at 100, then hits its own
        // order at 200 -> CancelTaker cancels the taker.
        let result = book.add_order(standard_order(200, 20, Side::Buy, taker_user));
        assert!(
            matches!(result, Err(OrderBookError::SelfTradePrevented { .. })),
            "partial self-cross under CancelTaker must surface the STP error, got {result:?}"
        );

        let captured = captured.lock().expect("listener mutex poisoned");
        assert_eq!(
            captured.len(),
            1,
            "the 5-unit non-self fill must reach the trade listener"
        );
        assert_eq!(
            captured[0].match_result.executed_quantity().ok(),
            Some(Quantity::new(5))
        );
    }

    /// Same STP partial-fill scenario through `add_order_with_result`: the
    /// typed error is returned (no trade result), the listener still fires.
    #[test]
    fn test_add_order_with_result_stp_cancel_taker_partial_fill_errors_and_emits() {
        let (mut book, captured) = book_with_capturing_listener();
        book.set_stp_mode(STPMode::CancelTaker);

        let taker_user = user(7);
        let other = user(1);

        let result = book.add_order(standard_order(100, 5, Side::Sell, other));
        assert!(result.is_ok(), "failed to rest other maker: {result:?}");
        let result = book.add_order(standard_order(200, 10, Side::Sell, taker_user));
        assert!(result.is_ok(), "failed to rest self maker: {result:?}");

        let result = book.add_order_with_result(standard_order(200, 20, Side::Buy, taker_user));
        assert!(
            matches!(result, Err(OrderBookError::SelfTradePrevented { .. })),
            "partial self-cross under CancelTaker must surface the STP error, got {result:?}"
        );

        let captured = captured.lock().expect("listener mutex poisoned");
        assert_eq!(
            captured.len(),
            1,
            "the 5-unit non-self fill must reach the trade listener"
        );
    }

    /// An IOC that partially fills and cannot complete returns
    /// `InsufficientLiquidity`; the partial fill still reaches the listener.
    #[test]
    fn test_add_order_with_result_ioc_partial_fill_errors_and_emits() {
        let (book, captured) = book_with_capturing_listener();
        let maker = user(1);
        let taker = user(2);

        let result = book.add_order(standard_order(100, 5, Side::Sell, maker));
        assert!(result.is_ok(), "failed to rest maker: {result:?}");

        let ioc = OrderType::Standard {
            id: Id::new(),
            price: Price::new(100),
            quantity: Quantity::new(20),
            side: Side::Buy,
            user_id: taker,
            timestamp: TimestampMs::new(crate::utils::current_time_millis()),
            time_in_force: TimeInForce::Ioc,
            extra_fields: (),
        };
        let result = book.add_order_with_result(ioc);
        assert!(
            matches!(result, Err(OrderBookError::InsufficientLiquidity { .. })),
            "unfillable IOC remainder must error, got {result:?}"
        );

        let captured = captured.lock().expect("listener mutex poisoned");
        assert_eq!(
            captured.len(),
            1,
            "the 5-unit partial fill must reach the trade listener"
        );
        assert_eq!(
            captured[0].match_result.executed_quantity().ok(),
            Some(Quantity::new(5))
        );
    }
}
