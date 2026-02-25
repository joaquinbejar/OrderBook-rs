use orderbook_rs::OrderBook;
use orderbook_rs::orderbook::modifications::OrderQuantity;
use pricelevel::{
    Hash32, Id, OrderType, OrderUpdate, PegReferenceType, Price, Quantity, Side, TimeInForce,
    TimestampMs,
};

#[derive(Clone, Debug, Default, PartialEq)]
struct TestExtraFields {
    pub user_id: String,
    pub strategy: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_order_type_quantity_methods() {
        // Test quantity() method for different order types
        let standard_order = OrderType::Standard {
            id: Id::new_uuid(),
            price: Price::new(1000),
            quantity: Quantity::new(100),
            side: Side::Buy,
            user_id: Hash32::zero(),
            time_in_force: TimeInForce::Gtc,
            timestamp: TimestampMs::new(0),
            extra_fields: TestExtraFields::default(),
        };
        assert_eq!(standard_order.quantity(), 100);

        let reserve_order = OrderType::ReserveOrder {
            id: Id::new_uuid(),
            price: Price::new(1000),
            visible_quantity: Quantity::new(30),
            hidden_quantity: Quantity::new(70),
            side: Side::Buy,
            user_id: Hash32::zero(),
            time_in_force: TimeInForce::Gtc,
            timestamp: TimestampMs::new(0),
            replenish_threshold: Quantity::new(10),
            replenish_amount: Some(Quantity::new(20)),
            auto_replenish: true,
            extra_fields: TestExtraFields::default(),
        };
        assert_eq!(reserve_order.quantity(), 30); // Returns visible quantity

        let post_only_order = OrderType::PostOnly {
            id: Id::new_uuid(),
            price: Price::new(1000),
            quantity: Quantity::new(75),
            side: Side::Buy,
            user_id: Hash32::zero(),
            time_in_force: TimeInForce::Gtc,
            timestamp: TimestampMs::new(0),
            extra_fields: TestExtraFields::default(),
        };
        assert_eq!(post_only_order.quantity(), 75);

        let trailing_stop_order = OrderType::TrailingStop {
            id: Id::new_uuid(),
            price: Price::new(1000),
            quantity: Quantity::new(25),
            side: Side::Buy,
            user_id: Hash32::zero(),
            time_in_force: TimeInForce::Gtc,
            timestamp: TimestampMs::new(0),
            trail_amount: Quantity::new(5),
            last_reference_price: Price::new(995),
            extra_fields: TestExtraFields::default(),
        };
        assert_eq!(trailing_stop_order.quantity(), 25);

        let pegged_order = OrderType::PeggedOrder {
            id: Id::new_uuid(),
            price: Price::new(1000),
            quantity: Quantity::new(80),
            side: Side::Buy,
            user_id: Hash32::zero(),
            time_in_force: TimeInForce::Gtc,
            timestamp: TimestampMs::new(0),
            reference_price_offset: 5,
            reference_price_type: PegReferenceType::BestBid,
            extra_fields: TestExtraFields::default(),
        };
        assert_eq!(pegged_order.quantity(), 80);

        let market_to_limit_order = OrderType::MarketToLimit {
            id: Id::new_uuid(),
            price: Price::new(1000),
            quantity: Quantity::new(60),
            side: Side::Sell,
            user_id: Hash32::zero(),
            time_in_force: TimeInForce::Ioc,
            timestamp: TimestampMs::new(0),
            extra_fields: TestExtraFields::default(),
        };
        assert_eq!(market_to_limit_order.quantity(), 60);

        let reserve_order = OrderType::ReserveOrder {
            id: Id::new_uuid(),
            price: Price::new(1000),
            visible_quantity: Quantity::new(30),
            hidden_quantity: Quantity::new(70),
            replenish_threshold: Quantity::new(10),
            replenish_amount: Some(Quantity::new(20)),
            auto_replenish: true,
            side: Side::Buy,
            user_id: Hash32::zero(),
            time_in_force: TimeInForce::Gtc,
            timestamp: TimestampMs::new(0),
            extra_fields: TestExtraFields::default(),
        };
        assert_eq!(reserve_order.quantity(), 30); // Returns visible quantity
    }

    #[test]
    fn test_order_type_total_quantity_methods() {
        // Test total_quantity() method for different order types
        let iceberg_order = OrderType::IcebergOrder {
            id: Id::new_uuid(),
            price: Price::new(1000),
            visible_quantity: Quantity::new(50),
            hidden_quantity: Quantity::new(150),
            side: Side::Buy,
            user_id: Hash32::zero(),
            time_in_force: TimeInForce::Gtc,
            timestamp: TimestampMs::new(0),
            extra_fields: TestExtraFields::default(),
        };
        assert_eq!(iceberg_order.total_quantity(), 200); // visible + hidden

        let reserve_order = OrderType::ReserveOrder {
            id: Id::new_uuid(),
            price: Price::new(1000),
            visible_quantity: Quantity::new(30),
            hidden_quantity: Quantity::new(70),
            replenish_threshold: Quantity::new(10),
            replenish_amount: Some(Quantity::new(20)),
            auto_replenish: true,
            side: Side::Buy,
            user_id: Hash32::zero(),
            time_in_force: TimeInForce::Gtc,
            timestamp: TimestampMs::new(0),
            extra_fields: TestExtraFields::default(),
        };
        assert_eq!(reserve_order.total_quantity(), 100); // visible + hidden

        let standard_order = OrderType::Standard {
            id: Id::new_uuid(),
            price: Price::new(1000),
            quantity: Quantity::new(100),
            side: Side::Buy,
            user_id: Hash32::zero(),
            time_in_force: TimeInForce::Gtc,
            timestamp: TimestampMs::new(0),
            extra_fields: TestExtraFields::default(),
        };
        assert_eq!(standard_order.total_quantity(), 100);
    }

    #[test]
    fn test_order_type_set_quantity_methods() {
        // Test set_quantity() method for different order types
        let mut standard_order = OrderType::Standard {
            id: Id::new_uuid(),
            price: Price::new(1000),
            quantity: Quantity::new(100),
            side: Side::Buy,
            user_id: Hash32::zero(),
            time_in_force: TimeInForce::Gtc,
            timestamp: TimestampMs::new(0),
            extra_fields: TestExtraFields::default(),
        };
        standard_order.set_quantity(80);
        assert_eq!(standard_order.quantity(), 80);

        let mut iceberg_order = OrderType::IcebergOrder {
            id: Id::new_uuid(),
            price: Price::new(1000),
            visible_quantity: Quantity::new(50),
            hidden_quantity: Quantity::new(150),
            side: Side::Buy,
            user_id: Hash32::zero(),
            time_in_force: TimeInForce::Gtc,
            timestamp: TimestampMs::new(0),
            extra_fields: TestExtraFields::default(),
        };
        iceberg_order.set_quantity(40);
        assert_eq!(iceberg_order.quantity(), 40); // visible quantity updated

        let mut reserve_order = OrderType::ReserveOrder {
            id: Id::new_uuid(),
            price: Price::new(1000),
            visible_quantity: Quantity::new(30),
            hidden_quantity: Quantity::new(70),
            replenish_threshold: Quantity::new(10),
            replenish_amount: Some(Quantity::new(20)),
            auto_replenish: true,
            side: Side::Buy,
            user_id: Hash32::zero(),
            time_in_force: TimeInForce::Gtc,
            timestamp: TimestampMs::new(0),
            extra_fields: TestExtraFields::default(),
        };
        reserve_order.set_quantity(80); // Reduce from 100 to 80
        assert_eq!(reserve_order.quantity(), 10); // visible reduced by 20
        assert_eq!(reserve_order.total_quantity(), 80); // total is now 80
    }

    #[test]
    fn test_update_order_price_same_value() {
        let book: OrderBook<TestExtraFields> = OrderBook::new("TEST");
        let order_id = Id::new_uuid();

        // Add an order
        book.add_limit_order(order_id, 1000, 100, Side::Buy, TimeInForce::Gtc, None)
            .unwrap();

        // Try to update to the same price
        let update = OrderUpdate::UpdatePrice {
            order_id,
            new_price: Price::new(1000),
        };

        let result = book.update_order(update);
        assert!(result.is_err());
        if let Err(e) = result {
            assert!(
                e.to_string()
                    .contains("Cannot update price to the same value")
            );
        }
    }

    #[test]
    fn test_update_order_price_success() {
        let book: OrderBook<TestExtraFields> = OrderBook::new("TEST");
        let order_id = Id::new_uuid();

        // Add an order
        book.add_limit_order(order_id, 1000, 100, Side::Buy, TimeInForce::Gtc, None)
            .unwrap();

        // Update the price
        let update = OrderUpdate::UpdatePrice {
            order_id,
            new_price: Price::new(1010),
        };

        let result = book.update_order(update);
        assert!(result.is_ok());
        let updated_order = result.unwrap();
        assert!(updated_order.is_some());

        // Verify the order was updated
        let order = updated_order.unwrap();
        assert_eq!(order.price().as_u128(), 1010);
        assert_eq!(order.visible_quantity(), 100);
    }

    #[test]
    fn test_update_order_quantity_success() {
        let book: OrderBook<TestExtraFields> = OrderBook::new("TEST");
        let order_id = Id::new_uuid();

        // Add an order
        book.add_limit_order(order_id, 1000, 100, Side::Buy, TimeInForce::Gtc, None)
            .unwrap();

        // Update the quantity
        let update = OrderUpdate::UpdateQuantity {
            order_id,
            new_quantity: Quantity::new(80),
        };

        let result = book.update_order(update);
        assert!(result.is_ok());
        let updated_order = result.unwrap();
        assert!(updated_order.is_some());

        // Verify the order was updated
        let order = updated_order.unwrap();
        assert_eq!(order.price().as_u128(), 1000);
        assert_eq!(order.visible_quantity(), 80);
    }

    #[test]
    fn test_update_order_price_and_quantity_success() {
        let book: OrderBook<TestExtraFields> = OrderBook::new("TEST");
        let order_id = Id::new_uuid();

        // Add an order
        book.add_limit_order(order_id, 1000, 100, Side::Buy, TimeInForce::Gtc, None)
            .unwrap();

        // Update both price and quantity
        let update = OrderUpdate::UpdatePriceAndQuantity {
            order_id,
            new_price: Price::new(1020),
            new_quantity: Quantity::new(75),
        };

        let result = book.update_order(update);
        assert!(result.is_ok());
        let updated_order = result.unwrap();
        assert!(updated_order.is_some());

        // Verify the order was updated
        let order = updated_order.unwrap();
        assert_eq!(order.price().as_u128(), 1020);
        assert_eq!(order.visible_quantity(), 75);
    }

    #[test]
    fn test_update_nonexistent_order() {
        let book: OrderBook<TestExtraFields> = OrderBook::new("TEST");
        let nonexistent_id = Id::new_uuid();

        // Try to update a nonexistent order
        let update = OrderUpdate::UpdatePrice {
            order_id: nonexistent_id,
            new_price: Price::new(1000),
        };

        let result = book.update_order(update);
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }
}
