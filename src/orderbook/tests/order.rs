#[cfg(test)]
mod tests {
    use pricelevel::{Hash32, Id, OrderType, Price, Quantity, Side, TimeInForce, TimestampMs};

    fn create_sample_order_id() -> Id {
        Id::new_uuid()
    }

    #[test]
    fn test_standard_order_properties() {
        let id = create_sample_order_id();
        let price = 1000;
        let quantity = 10;
        let side = Side::Buy;
        let timestamp = 12345678;
        let time_in_force = TimeInForce::Gtc;

        let order = OrderType::Standard {
            id,
            price: Price::new(price),
            quantity: Quantity::new(quantity),
            side,
            user_id: Hash32::zero(),
            timestamp: TimestampMs::new(timestamp),
            time_in_force,
            extra_fields: (),
        };

        // Test property getters
        assert_eq!(order.id(), id);
        assert_eq!(order.price().as_u128(), price);
        assert_eq!(order.visible_quantity(), quantity);
        assert_eq!(order.hidden_quantity(), 0); // Standard orders have no hidden quantity
        assert_eq!(order.side(), side);
        assert_eq!(order.timestamp(), timestamp);
        assert_eq!(order.time_in_force(), time_in_force);

        // Test other properties
        assert!(!order.is_immediate()); // GTC is not immediate
        assert!(!order.is_fill_or_kill()); // Not FOK
        assert!(!order.is_post_only()); // Not post only
    }

    #[test]
    fn test_iceberg_order_properties() {
        let id = create_sample_order_id();
        let price = 1000;
        let visible_quantity = 10;
        let hidden_quantity = 90;
        let side = Side::Sell;
        let timestamp = 12345678;
        let time_in_force = TimeInForce::Gtc;

        let order = OrderType::IcebergOrder {
            id,
            price: Price::new(price),
            visible_quantity: Quantity::new(visible_quantity),
            hidden_quantity: Quantity::new(hidden_quantity),
            side,
            user_id: Hash32::zero(),
            timestamp: TimestampMs::new(timestamp),
            time_in_force,
            extra_fields: (),
        };

        // Test property getters
        assert_eq!(order.id(), id);
        assert_eq!(order.price().as_u128(), price);
        assert_eq!(order.visible_quantity(), visible_quantity);
        assert_eq!(order.hidden_quantity(), hidden_quantity);
        assert_eq!(order.side(), side);
        assert_eq!(order.timestamp(), timestamp);
        assert_eq!(order.time_in_force(), time_in_force);
    }

    #[test]
    fn test_post_only_order_properties() {
        let id = create_sample_order_id();
        let price = 1000;
        let quantity = 10;
        let side = Side::Buy;
        let timestamp = 12345678;
        let time_in_force = TimeInForce::Gtc;

        let order = OrderType::PostOnly {
            id,
            price: Price::new(price),
            quantity: Quantity::new(quantity),
            side,
            user_id: Hash32::zero(),
            timestamp: TimestampMs::new(timestamp),
            time_in_force,
            extra_fields: (),
        };

        // Test property getters
        assert_eq!(order.id(), id);
        assert_eq!(order.price().as_u128(), price);
        assert_eq!(order.visible_quantity(), quantity);
        assert_eq!(order.hidden_quantity(), 0);
        assert_eq!(order.side(), side);
        assert_eq!(order.timestamp(), timestamp);
        assert_eq!(order.time_in_force(), time_in_force);

        // Test post only property
        assert!(order.is_post_only());
    }

    #[test]
    fn test_immediate_or_cancel_property() {
        let id = create_sample_order_id();

        let ioc_order = OrderType::Standard {
            id,
            price: Price::new(1000),
            quantity: Quantity::new(10),
            side: Side::Buy,
            user_id: Hash32::zero(),
            timestamp: TimestampMs::new(12345678),
            time_in_force: TimeInForce::Ioc,
            extra_fields: (),
        };

        assert!(ioc_order.is_immediate(), "IOC orders should be immediate");
        assert!(
            !ioc_order.is_fill_or_kill(),
            "IOC orders should not be fill-or-kill"
        );
    }

    #[test]
    fn test_fill_or_kill_property() {
        let id = create_sample_order_id();

        let fok_order = OrderType::Standard {
            id,
            price: Price::new(1000),
            quantity: Quantity::new(10),
            side: Side::Buy,
            user_id: Hash32::zero(),
            timestamp: TimestampMs::new(12345678),
            time_in_force: TimeInForce::Fok,
            extra_fields: (),
        };

        assert!(fok_order.is_immediate(), "FOK orders should be immediate");
        assert!(
            fok_order.is_fill_or_kill(),
            "FOK orders should be fill-or-kill"
        );
    }

    #[test]
    fn test_with_reduced_quantity() {
        let id = create_sample_order_id();
        let original_quantity = Quantity::new(100);
        let new_quantity = 50;

        // Test standard order
        let standard_order = OrderType::Standard {
            id,
            price: Price::new(1000),
            quantity: original_quantity,
            side: Side::Buy,
            user_id: Hash32::zero(),
            timestamp: TimestampMs::new(12345678),
            time_in_force: TimeInForce::Gtc,
            extra_fields: (),
        };

        let reduced_standard = standard_order.with_reduced_quantity(new_quantity);

        match reduced_standard {
            OrderType::Standard { quantity, .. } => {
                assert_eq!(
                    quantity,
                    Quantity::new(new_quantity),
                    "Quantity should be reduced"
                );
            }
            _ => panic!("Expected Standard order"),
        }

        // Test iceberg order
        let iceberg_order = OrderType::IcebergOrder {
            id,
            price: Price::new(1000),
            visible_quantity: original_quantity,
            hidden_quantity: Quantity::new(200),
            side: Side::Buy,
            user_id: Hash32::zero(),
            timestamp: TimestampMs::new(12345678),
            time_in_force: TimeInForce::Gtc,
            extra_fields: (),
        };

        let reduced_iceberg = iceberg_order.with_reduced_quantity(new_quantity);

        match reduced_iceberg {
            OrderType::IcebergOrder {
                visible_quantity,
                hidden_quantity,
                ..
            } => {
                assert_eq!(
                    visible_quantity,
                    Quantity::new(new_quantity),
                    "Visible quantity should be reduced"
                );
                assert_eq!(
                    hidden_quantity,
                    Quantity::new(200),
                    "Hidden quantity should remain unchanged"
                );
            }
            _ => panic!("Expected IcebergOrder"),
        }
    }

    #[test]
    fn test_refresh_iceberg() {
        let id = create_sample_order_id();
        let visible_quantity = 10;
        let hidden_quantity = 90;
        let refresh_amount = 20;

        let iceberg_order = OrderType::IcebergOrder {
            id,
            price: Price::new(1000),
            visible_quantity: Quantity::new(visible_quantity),
            hidden_quantity: Quantity::new(hidden_quantity),
            side: Side::Buy,
            user_id: Hash32::zero(),
            timestamp: TimestampMs::new(12345678),
            time_in_force: TimeInForce::Gtc,
            extra_fields: (),
        };

        let (refreshed_order, used_hidden) = iceberg_order.refresh_iceberg(refresh_amount);

        match refreshed_order {
            OrderType::IcebergOrder {
                visible_quantity: new_visible,
                hidden_quantity: new_hidden,
                ..
            } => {
                assert_eq!(
                    new_visible,
                    Quantity::new(refresh_amount),
                    "Visible quantity should be refreshed to requested amount"
                );
                assert_eq!(
                    new_hidden,
                    Quantity::new(hidden_quantity - refresh_amount),
                    "Hidden quantity should be reduced by refresh amount"
                );
                assert_eq!(
                    used_hidden, refresh_amount,
                    "Used hidden should equal refresh amount"
                );
            }
            _ => panic!("Expected IcebergOrder"),
        }
    }

    #[test]
    fn test_match_against_standard_full_match() {
        let id = create_sample_order_id();
        let order_quantity = Quantity::new(10);
        let incoming_quantity = 10; // Equal to order quantity, so full match

        let order = OrderType::Standard {
            id,
            price: Price::new(1000),
            quantity: order_quantity,
            side: Side::Buy,
            user_id: Hash32::zero(),
            timestamp: TimestampMs::new(12345678),
            time_in_force: TimeInForce::Gtc,
            extra_fields: (),
        };

        let (consumed, updated_order, hidden_reduced, remaining) =
            order.match_against(incoming_quantity);

        assert_eq!(
            Quantity::new(consumed),
            order_quantity,
            "All of order quantity should be consumed"
        );
        assert!(
            updated_order.is_none(),
            "Order should be fully matched, so no updated order"
        );
        assert_eq!(hidden_reduced, 0, "No hidden quantity should be reduced");
        assert_eq!(remaining, 0, "No remaining incoming quantity");
    }

    #[test]
    fn test_match_against_standard_partial_match() {
        let id = create_sample_order_id();
        let order_quantity = Quantity::new(20);
        let incoming_quantity = 10; // Less than order quantity, so partial match

        let order = OrderType::Standard {
            id,
            price: Price::new(1000),
            quantity: order_quantity,
            side: Side::Buy,
            user_id: Hash32::zero(),
            timestamp: TimestampMs::new(12345678),
            time_in_force: TimeInForce::Gtc,
            extra_fields: (),
        };

        let (consumed, updated_order, hidden_reduced, remaining) =
            order.match_against(incoming_quantity);

        assert_eq!(
            consumed, incoming_quantity,
            "Incoming quantity should be fully consumed"
        );

        match updated_order {
            Some(OrderType::Standard { quantity, .. }) => {
                assert_eq!(
                    quantity,
                    Quantity::new(order_quantity.as_u64() - incoming_quantity),
                    "Remaining quantity should be reduced"
                );
            }
            _ => panic!("Expected Some(Standard) with reduced quantity"),
        }

        assert_eq!(hidden_reduced, 0, "No hidden quantity should be reduced");
        assert_eq!(remaining, 0, "No remaining incoming quantity");
    }

    #[test]
    fn test_match_against_iceberg_full_visible_with_refresh() {
        let id = create_sample_order_id();
        let visible_quantity = Quantity::new(10);
        let hidden_quantity = 20;
        let incoming_quantity = 10; // Equal to visible, so full visible match with refresh

        let order = OrderType::IcebergOrder {
            id,
            price: Price::new(1000),
            visible_quantity,
            hidden_quantity: Quantity::new(hidden_quantity),
            side: Side::Buy,
            user_id: Hash32::zero(),
            timestamp: TimestampMs::new(12345678),
            time_in_force: TimeInForce::Gtc,
            extra_fields: (),
        };

        let (consumed, updated_order, hidden_reduced, remaining) =
            order.match_against(incoming_quantity);

        assert_eq!(
            Quantity::new(consumed),
            visible_quantity,
            "All of visible quantity should be consumed"
        );

        match updated_order {
            Some(OrderType::IcebergOrder {
                visible_quantity: new_visible,
                hidden_quantity: new_hidden,
                ..
            }) => {
                assert_eq!(
                    new_visible, visible_quantity,
                    "Visible quantity should be refreshed to original amount"
                );
                assert_eq!(
                    new_hidden,
                    Quantity::new(hidden_quantity - visible_quantity.as_u64()),
                    "Hidden quantity should be reduced by refresh"
                );
                assert_eq!(
                    Quantity::new(hidden_reduced),
                    visible_quantity,
                    "Hidden reduced should equal visible quantity"
                );
            }
            _ => panic!("Expected Some(IcebergOrder) with refreshed quantities"),
        }

        assert_eq!(remaining, 0, "No remaining incoming quantity");
    }
}
