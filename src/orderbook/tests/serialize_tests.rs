//! Tests for OrderBook serialization functionality

#[cfg(test)]
mod tests {
    use crate::orderbook::OrderBook;
    use pricelevel::{Id, Side, TimeInForce};
    use serde_json;

    #[test]
    fn test_orderbook_serialize_empty() {
        let orderbook: OrderBook<String> = OrderBook::new("BTCUSD");

        // Test that empty orderbook can be serialized
        let json_result = serde_json::to_string(&orderbook);
        assert!(json_result.is_ok(), "Failed to serialize empty orderbook");

        let json = json_result.unwrap();
        assert!(
            json.contains("BTCUSD"),
            "Serialized JSON should contain symbol"
        );
        assert!(
            json.contains("bids"),
            "Serialized JSON should contain bids field"
        );
        assert!(
            json.contains("asks"),
            "Serialized JSON should contain asks field"
        );
    }

    #[test]
    fn test_orderbook_serialize_with_orders() {
        let orderbook: OrderBook<String> = OrderBook::new("ETHUSD");

        // Add some orders using the add_limit_order method
        let buy_order_id = Id::new_uuid();
        let sell_order_id = Id::new_uuid();

        let _ = orderbook.add_limit_order(
            buy_order_id,
            50000,
            100,
            Side::Buy,
            TimeInForce::Gtc,
            Some("buy_order_1".to_string()),
        );

        let _ = orderbook.add_limit_order(
            sell_order_id,
            51000,
            200,
            Side::Sell,
            TimeInForce::Gtc,
            Some("sell_order_1".to_string()),
        );

        // Test serialization
        let json_result = serde_json::to_string_pretty(&orderbook);

        assert!(
            json_result.is_ok(),
            "Failed to serialize orderbook with orders"
        );

        let json = json_result.unwrap();
        assert!(
            json.contains("ETHUSD"),
            "Serialized JSON should contain symbol"
        );
        assert!(
            json.contains("bids"),
            "Serialized JSON should contain bids field"
        );
        assert!(
            json.contains("asks"),
            "Serialized JSON should contain asks field"
        );
        assert!(
            json.contains("order_locations"),
            "Serialized JSON should contain order_locations field"
        );
        assert!(
            json.contains("cache"),
            "Serialized JSON should contain cache field"
        );

        // Verify the JSON is valid by parsing it back
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["symbol"], "ETHUSD");
    }

    #[test]
    fn test_orderbook_serialize_pretty() {
        let orderbook: OrderBook<()> = OrderBook::new("ADAUSD");

        // Test pretty printing
        let json_result = serde_json::to_string_pretty(&orderbook);
        assert!(json_result.is_ok(), "Failed to pretty serialize orderbook");

        let json = json_result.unwrap();
        assert!(
            json.contains("ADAUSD"),
            "Pretty serialized JSON should contain symbol"
        );
        // Pretty JSON should have newlines
        assert!(json.contains('\n'), "Pretty JSON should contain newlines");
    }

    #[test]
    fn test_orderbook_serialize_with_market_close() {
        let orderbook: OrderBook<i32> = OrderBook::new("DOGUSD");

        // Set market close timestamp
        orderbook.set_market_close_timestamp(9999999999);

        let json_result = serde_json::to_string(&orderbook);
        assert!(
            json_result.is_ok(),
            "Failed to serialize orderbook with market close"
        );

        let json = json_result.unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed["symbol"], "DOGUSD");
        assert_eq!(parsed["market_close_timestamp"], 9999999999u64);
        assert_eq!(parsed["has_market_close"], true);
    }
}
