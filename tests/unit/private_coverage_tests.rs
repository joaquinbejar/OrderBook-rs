//! Additional unit tests to improve test coverage for private.rs
//! These tests target specific uncovered lines and edge cases

use pricelevel::{
    Hash32, Id, OrderType, PegReferenceType, Price, Quantity, Side, TimeInForce, TimestampMs,
};
use std::sync::Arc;

#[derive(Debug, Clone, Default, PartialEq)]
struct TestExtraFields {
    pub metadata: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    use orderbook_rs::{OrderBook, current_time_millis};

    fn create_order_id() -> Id {
        Id::new_uuid()
    }

    #[test]
    fn test_has_expired_with_market_close_set() {
        // Test has_expired with market close timestamp set (lines 43, 65)
        let book = OrderBook::<TestExtraFields>::new("TEST");
        let current_time = current_time_millis();

        // Set market close timestamp in the past
        book.set_market_close_timestamp(current_time - 1000);

        let day_order = OrderType::Standard {
            id: create_order_id(),
            price: Price::new(100),
            quantity: Quantity::new(10),
            side: Side::Buy,
            user_id: Hash32::zero(),
            timestamp: TimestampMs::new(current_time),
            time_in_force: TimeInForce::Day,
            extra_fields: TestExtraFields::default(),
        };

        // Day order should expire when market close is in the past
        assert!(book.has_expired(&day_order));
    }

    #[test]
    fn test_has_expired_without_market_close() {
        // Test has_expired without market close timestamp (lines 67-72)
        let book = OrderBook::<TestExtraFields>::new("TEST");
        let current_time = current_time_millis();

        let gtc_order = OrderType::Standard {
            id: create_order_id(),
            price: Price::new(100),
            quantity: Quantity::new(10),
            side: Side::Buy,
            user_id: Hash32::zero(),
            timestamp: TimestampMs::new(current_time),
            time_in_force: TimeInForce::Gtc,
            extra_fields: TestExtraFields::default(),
        };

        // GTC order should not expire without market close
        assert!(!book.has_expired(&gtc_order));
    }

    #[test]
    fn test_will_cross_market_buy_side() {
        // Test will_cross_market for buy side (lines 74-80)
        let book = OrderBook::<TestExtraFields>::new("TEST");

        // Add a sell order at price 100
        let sell_id = create_order_id();
        let _ = book.add_limit_order(sell_id, 100, 10, Side::Sell, TimeInForce::Gtc, None);

        // Buy at 100 should cross (equal price)
        assert!(book.will_cross_market(100, Side::Buy));

        // Buy at 101 should cross (higher price)
        assert!(book.will_cross_market(101, Side::Buy));

        // Buy at 99 should not cross (lower price)
        assert!(!book.will_cross_market(99, Side::Buy));
    }

    #[test]
    fn test_will_cross_market_sell_side() {
        // Test will_cross_market for sell side (lines 82)
        let book = OrderBook::<TestExtraFields>::new("TEST");

        // Add a buy order at price 100
        let buy_id = create_order_id();
        let _ = book.add_limit_order(buy_id, 100, 10, Side::Buy, TimeInForce::Gtc, None);

        // Sell at 100 should cross (equal price)
        assert!(book.will_cross_market(100, Side::Sell));

        // Sell at 99 should cross (lower price)
        assert!(book.will_cross_market(99, Side::Sell));

        // Sell at 101 should not cross (higher price)
        assert!(!book.will_cross_market(101, Side::Sell));
    }

    #[test]
    fn test_place_order_in_book_buy_side() {
        // Test place_order_in_book for buy side (lines 90)
        let book = OrderBook::<TestExtraFields>::new("TEST");
        let order_id = create_order_id();

        let order = Arc::new(OrderType::Standard {
            id: order_id,
            price: Price::new(100),
            quantity: Quantity::new(10),
            side: Side::Buy,
            user_id: Hash32::zero(),
            timestamp: TimestampMs::new(current_time_millis()),
            time_in_force: TimeInForce::Gtc,
            extra_fields: TestExtraFields::default(),
        });

        let result = book.place_order_in_book(order.clone());
        assert!(result.is_ok());

        // Verify order was added by checking if we can retrieve it
        let retrieved_order = book.get_order(order_id);
        assert!(retrieved_order.is_some());

        // Verify order properties
        let order = retrieved_order.unwrap();
        assert_eq!(order.price().as_u128(), 100);
        assert_eq!(order.visible_quantity(), 10);
        assert_eq!(order.side(), Side::Buy);
    }

    #[test]
    fn test_place_order_in_book_sell_side() {
        // Test place_order_in_book for sell side
        let book = OrderBook::<TestExtraFields>::new("TEST");
        let order_id = create_order_id();

        let order = Arc::new(OrderType::Standard {
            id: order_id,
            price: Price::new(100),
            quantity: Quantity::new(10),
            side: Side::Sell,
            user_id: Hash32::zero(),
            timestamp: TimestampMs::new(current_time_millis()),
            time_in_force: TimeInForce::Gtc,
            extra_fields: TestExtraFields::default(),
        });

        let result = book.place_order_in_book(order.clone());
        assert!(result.is_ok());

        // Verify order was added by checking if we can retrieve it
        let retrieved_order = book.get_order(order_id);
        assert!(retrieved_order.is_some());

        // Verify order properties
        let order = retrieved_order.unwrap();
        assert_eq!(order.price().as_u128(), 100);
        assert_eq!(order.visible_quantity(), 10);
        assert_eq!(order.side(), Side::Sell);
    }

    #[test]
    fn test_convert_to_unit_type_standard_order() {
        // Test convert_to_unit_type for Standard order (lines 105-115)
        let book = OrderBook::<TestExtraFields>::new("TEST");
        let order_id = create_order_id();
        let timestamp = current_time_millis();

        let order = OrderType::Standard {
            id: order_id,
            price: Price::new(100),
            quantity: Quantity::new(10),
            side: Side::Buy,
            user_id: Hash32::zero(),
            timestamp: TimestampMs::new(timestamp),
            time_in_force: TimeInForce::Gtc,
            extra_fields: TestExtraFields {
                metadata: "test".to_string(),
            },
        };

        let unit_order = book.convert_to_unit_type(&order);

        match unit_order {
            OrderType::Standard {
                id: converted_id,
                price: converted_price,
                quantity: converted_quantity,
                side: converted_side,
                user_id: _,
                timestamp: converted_timestamp,
                time_in_force: converted_tif,
                extra_fields: _,
            } => {
                assert_eq!(converted_id, order_id);
                assert_eq!(converted_price, Price::new(100));
                assert_eq!(converted_quantity, Quantity::new(10));
                assert_eq!(converted_side, Side::Buy);
                assert_eq!(converted_timestamp, TimestampMs::new(timestamp));
                assert_eq!(converted_tif, TimeInForce::Gtc);
                // extra_fields is unit type as expected
            }
            _ => panic!("Expected Standard order type"),
        }
    }

    #[test]
    fn test_convert_to_unit_type_iceberg_order() {
        // Test convert_to_unit_type for IcebergOrder (lines 116-128)
        let book = OrderBook::<TestExtraFields>::new("TEST");
        let order_id = create_order_id();
        let timestamp = current_time_millis();

        let order = OrderType::IcebergOrder {
            id: order_id,
            price: Price::new(100),
            visible_quantity: Quantity::new(5),
            hidden_quantity: Quantity::new(15),
            side: Side::Sell,
            user_id: Hash32::zero(),
            timestamp: TimestampMs::new(timestamp),
            time_in_force: TimeInForce::Ioc,
            extra_fields: TestExtraFields {
                metadata: "iceberg".to_string(),
            },
        };

        let unit_order = book.convert_to_unit_type(&order);

        match unit_order {
            OrderType::IcebergOrder {
                id: converted_id,
                price: converted_price,
                visible_quantity: converted_visible,
                hidden_quantity: converted_hidden,
                side: converted_side,
                user_id: _,
                timestamp: converted_timestamp,
                time_in_force: converted_tif,
                extra_fields: _,
            } => {
                assert_eq!(converted_id, order_id);
                assert_eq!(converted_price, Price::new(100));
                assert_eq!(converted_visible, Quantity::new(5));
                assert_eq!(converted_hidden, Quantity::new(15));
                assert_eq!(converted_side, Side::Sell);
                assert_eq!(converted_timestamp, TimestampMs::new(timestamp));
                assert_eq!(converted_tif, TimeInForce::Ioc);
                // extra_fields is unit type as expected
            }
            _ => panic!("Expected IcebergOrder type"),
        }
    }

    #[test]
    fn test_convert_to_unit_type_post_only_order() {
        // Test convert_to_unit_type for PostOnly order (lines 129-139)
        let book = OrderBook::<TestExtraFields>::new("TEST");
        let order_id = create_order_id();
        let timestamp = current_time_millis();

        let order = OrderType::PostOnly {
            id: order_id,
            price: Price::new(100),
            quantity: Quantity::new(20),
            side: Side::Buy,
            user_id: Hash32::zero(),
            timestamp: TimestampMs::new(timestamp),
            time_in_force: TimeInForce::Fok,
            extra_fields: TestExtraFields {
                metadata: "post_only".to_string(),
            },
        };

        let unit_order = book.convert_to_unit_type(&order);

        match unit_order {
            OrderType::PostOnly {
                id: converted_id,
                price: converted_price,
                quantity: converted_quantity,
                side: converted_side,
                user_id: _,
                timestamp: converted_timestamp,
                time_in_force: converted_tif,
                extra_fields: _,
            } => {
                assert_eq!(converted_id, order_id);
                assert_eq!(converted_price, Price::new(100));
                assert_eq!(converted_quantity, Quantity::new(20));
                assert_eq!(converted_side, Side::Buy);
                assert_eq!(converted_timestamp, TimestampMs::new(timestamp));
                assert_eq!(converted_tif, TimeInForce::Fok);
                // extra_fields is unit type as expected
            }
            _ => panic!("Expected PostOnly order type"),
        }
    }

    #[test]
    fn test_convert_to_unit_type_trailing_stop_order() {
        // Test convert_to_unit_type for TrailingStop order (lines 140-153)
        let book = OrderBook::<TestExtraFields>::new("TEST");
        let order_id = create_order_id();
        let timestamp = current_time_millis();

        let order = OrderType::TrailingStop {
            id: order_id,
            price: Price::new(100),
            quantity: Quantity::new(25),
            side: Side::Sell,
            user_id: Hash32::zero(),
            timestamp: TimestampMs::new(timestamp),
            time_in_force: TimeInForce::Day,
            trail_amount: Quantity::new(5),
            last_reference_price: Price::new(105),
            extra_fields: TestExtraFields {
                metadata: "trailing_stop".to_string(),
            },
        };

        let unit_order = book.convert_to_unit_type(&order);

        match unit_order {
            OrderType::TrailingStop {
                id: converted_id,
                price: converted_price,
                quantity: converted_quantity,
                side: converted_side,
                user_id: _,
                timestamp: converted_timestamp,
                time_in_force: converted_tif,
                trail_amount: converted_trail,
                last_reference_price: converted_ref_price,
                extra_fields: _,
            } => {
                assert_eq!(converted_id, order_id);
                assert_eq!(converted_price, Price::new(100));
                assert_eq!(converted_quantity, Quantity::new(25));
                assert_eq!(converted_side, Side::Sell);
                assert_eq!(converted_timestamp, TimestampMs::new(timestamp));
                assert_eq!(converted_tif, TimeInForce::Day);
                assert_eq!(converted_trail, Quantity::new(5));
                assert_eq!(converted_ref_price, Price::new(105));
                // extra_fields is unit type as expected
            }
            _ => panic!("Expected TrailingStop order type"),
        }
    }

    #[test]
    fn test_convert_to_unit_type_pegged_order() {
        // Test convert_to_unit_type for PeggedOrder (lines 154-167)
        let book = OrderBook::<TestExtraFields>::new("TEST");
        let order_id = create_order_id();
        let timestamp = current_time_millis();

        let order = OrderType::PeggedOrder {
            id: order_id,
            price: Price::new(100),
            quantity: Quantity::new(30),
            side: Side::Buy,
            user_id: Hash32::zero(),
            timestamp: TimestampMs::new(timestamp),
            time_in_force: TimeInForce::Gtc,
            reference_price_offset: 2,
            reference_price_type: PegReferenceType::BestBid,
            extra_fields: TestExtraFields {
                metadata: "pegged".to_string(),
            },
        };

        let unit_order = book.convert_to_unit_type(&order);

        match unit_order {
            OrderType::PeggedOrder {
                id: converted_id,
                price: converted_price,
                quantity: converted_quantity,
                side: converted_side,
                user_id: _,
                timestamp: converted_timestamp,
                time_in_force: converted_tif,
                reference_price_offset: converted_offset,
                reference_price_type: converted_ref_type,
                extra_fields: _,
            } => {
                assert_eq!(converted_id, order_id);
                assert_eq!(converted_price, Price::new(100));
                assert_eq!(converted_quantity, Quantity::new(30));
                assert_eq!(converted_side, Side::Buy);
                assert_eq!(converted_timestamp, TimestampMs::new(timestamp));
                assert_eq!(converted_tif, TimeInForce::Gtc);
                assert_eq!(converted_offset, 2);
                assert_eq!(converted_ref_type, PegReferenceType::BestBid);
                // extra_fields is unit type as expected
            }
            _ => panic!("Expected PeggedOrder order type"),
        }
    }

    #[test]
    fn test_convert_to_unit_type_market_to_limit_order() {
        // Test convert_to_unit_type for MarketToLimit order (lines 168-178)
        let book = OrderBook::<TestExtraFields>::new("TEST");
        let order_id = create_order_id();
        let timestamp = current_time_millis();

        let order = OrderType::MarketToLimit {
            id: order_id,
            price: Price::new(100),
            quantity: Quantity::new(35),
            side: Side::Sell,
            user_id: Hash32::zero(),
            timestamp: TimestampMs::new(timestamp),
            time_in_force: TimeInForce::Ioc,
            extra_fields: TestExtraFields {
                metadata: "market_to_limit".to_string(),
            },
        };

        let unit_order = book.convert_to_unit_type(&order);

        match unit_order {
            OrderType::MarketToLimit {
                id: converted_id,
                price: converted_price,
                quantity: converted_quantity,
                side: converted_side,
                user_id: _,
                timestamp: converted_timestamp,
                time_in_force: converted_tif,
                extra_fields: _,
            } => {
                assert_eq!(converted_id, order_id);
                assert_eq!(converted_price, Price::new(100));
                assert_eq!(converted_quantity, Quantity::new(35));
                assert_eq!(converted_side, Side::Sell);
                assert_eq!(converted_timestamp, TimestampMs::new(timestamp));
                assert_eq!(converted_tif, TimeInForce::Ioc);
                // extra_fields is unit type as expected
            }
            _ => panic!("Expected MarketToLimit order type"),
        }
    }

    #[test]
    fn test_convert_to_unit_type_reserve_order() {
        // Test convert_to_unit_type for ReserveOrder (lines 179-195)
        let book = OrderBook::<TestExtraFields>::new("TEST");
        let order_id = create_order_id();
        let timestamp = current_time_millis();

        let order = OrderType::ReserveOrder {
            id: order_id,
            price: Price::new(100),
            visible_quantity: Quantity::new(10),
            hidden_quantity: Quantity::new(40),
            side: Side::Buy,
            user_id: Hash32::zero(),
            timestamp: TimestampMs::new(timestamp),
            time_in_force: TimeInForce::Fok,
            replenish_threshold: Quantity::new(5),
            replenish_amount: Some(Quantity::new(15)),
            auto_replenish: true,
            extra_fields: TestExtraFields {
                metadata: "reserve".to_string(),
            },
        };

        let unit_order = book.convert_to_unit_type(&order);

        match unit_order {
            OrderType::ReserveOrder {
                id: converted_id,
                price: converted_price,
                visible_quantity: converted_visible,
                hidden_quantity: converted_hidden,
                side: converted_side,
                user_id: _,
                timestamp: converted_timestamp,
                time_in_force: converted_tif,
                replenish_threshold: converted_threshold,
                replenish_amount: converted_amount,
                auto_replenish: converted_auto,
                extra_fields: _,
            } => {
                assert_eq!(converted_id, order_id);
                assert_eq!(converted_price, Price::new(100));
                assert_eq!(converted_visible, Quantity::new(10));
                assert_eq!(converted_hidden, Quantity::new(40));
                assert_eq!(converted_side, Side::Buy);
                assert_eq!(converted_timestamp, TimestampMs::new(timestamp));
                assert_eq!(converted_tif, TimeInForce::Fok);
                assert_eq!(converted_threshold, Quantity::new(5));
                assert_eq!(converted_amount, Some(Quantity::new(15)));
                assert!(converted_auto);
                // extra_fields is unit type as expected
            }
            _ => panic!("Expected ReserveOrder order type"),
        }
    }

    #[test]
    fn test_has_expired_gtd_order_not_expired() {
        // Test has_expired for GTD order that hasn't expired
        let book = OrderBook::<TestExtraFields>::new("TEST");
        let current_time = current_time_millis();

        let gtd_order = OrderType::Standard {
            id: create_order_id(),
            price: Price::new(100),
            quantity: Quantity::new(10),
            side: Side::Buy,
            user_id: Hash32::zero(),
            timestamp: TimestampMs::new(current_time),
            time_in_force: TimeInForce::Gtd(current_time + 10000), // Expires in future
            extra_fields: TestExtraFields::default(),
        };

        // GTD order should not expire if expiry is in future
        assert!(!book.has_expired(&gtd_order));
    }

    #[test]
    fn test_has_expired_gtd_order_expired() {
        // Test has_expired for GTD order that has expired
        let book = OrderBook::<TestExtraFields>::new("TEST");
        let current_time = current_time_millis();

        let gtd_order = OrderType::Standard {
            id: create_order_id(),
            price: Price::new(100),
            quantity: Quantity::new(10),
            side: Side::Buy,
            user_id: Hash32::zero(),
            timestamp: TimestampMs::new(current_time),
            time_in_force: TimeInForce::Gtd(current_time - 1000), // Expired
            extra_fields: TestExtraFields::default(),
        };

        // GTD order should expire if expiry is in past
        assert!(book.has_expired(&gtd_order));
    }

    #[test]
    fn test_will_cross_market_no_opposite_side() {
        // Test will_cross_market when there's no opposite side
        let book = OrderBook::<TestExtraFields>::new("TEST");

        // No orders on either side - should not cross
        assert!(!book.will_cross_market(100, Side::Buy));
        assert!(!book.will_cross_market(100, Side::Sell));
    }

    #[test]
    fn test_place_order_in_book_existing_price_level() {
        // Test place_order_in_book when price level already exists
        let book = OrderBook::<TestExtraFields>::new("TEST");

        // Add first order
        let order_id1 = create_order_id();
        let order1 = Arc::new(OrderType::Standard {
            id: order_id1,
            price: Price::new(100),
            quantity: Quantity::new(10),
            side: Side::Buy,
            user_id: Hash32::zero(),
            timestamp: TimestampMs::new(current_time_millis()),
            time_in_force: TimeInForce::Gtc,
            extra_fields: TestExtraFields::default(),
        });

        let _ = book.place_order_in_book(order1);

        // Add second order at same price
        let order_id2 = create_order_id();
        let order2 = Arc::new(OrderType::Standard {
            id: order_id2,
            price: Price::new(100),
            quantity: Quantity::new(15),
            side: Side::Buy,
            user_id: Hash32::zero(),
            timestamp: TimestampMs::new(current_time_millis()),
            time_in_force: TimeInForce::Gtc,
            extra_fields: TestExtraFields::default(),
        });

        let result = book.place_order_in_book(order2);
        assert!(result.is_ok());

        // Verify both orders were added by checking if we can retrieve them
        let retrieved_order1 = book.get_order(order_id1);
        let retrieved_order2 = book.get_order(order_id2);
        assert!(retrieved_order1.is_some());
        assert!(retrieved_order2.is_some());

        // Verify we have orders at this price level
        let orders_at_price = book.get_orders_at_price(100, Side::Buy);
        assert_eq!(orders_at_price.len(), 2);
    }
}
