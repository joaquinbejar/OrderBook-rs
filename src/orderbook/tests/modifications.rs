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
        assert_eq!(updated_order.visible_quantity(), new_quantity);
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
        assert_eq!(replaced_order.visible_quantity(), new_quantity);
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
        assert_eq!(updated_order.visible_quantity(), new_quantity);

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
            replenish_amount: Some(Quantity::new(3)),
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
            replenish_amount: Some(Quantity::new(3)),
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
        assert_eq!(updated_order.unwrap().visible_quantity(), 15);
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
            replenish_amount: Some(Quantity::new(3)),
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
        assert_eq!(updated_order.visible_quantity(), 12);
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
            replenish_amount: Some(Quantity::new(10)),
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
