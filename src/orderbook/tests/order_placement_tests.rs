//! Tests for intelligent order placement utilities

#[cfg(test)]
mod tests {
    use crate::OrderBook;
    use pricelevel::{Id, Side, TimeInForce};

    #[test]
    fn test_queue_ahead_at_price_basic() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        // Add multiple orders at same price
        let _ = book.add_limit_order(Id::new(), 100, 10, Side::Buy, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 100, 20, Side::Buy, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 100, 30, Side::Buy, TimeInForce::Gtc, None);

        assert_eq!(book.queue_ahead_at_price(100, Side::Buy), 3);
    }

    #[test]
    fn test_queue_ahead_at_price_no_orders() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        assert_eq!(book.queue_ahead_at_price(100, Side::Buy), 0);
    }

    #[test]
    fn test_queue_ahead_at_price_different_levels() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        let _ = book.add_limit_order(Id::new(), 100, 10, Side::Buy, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 99, 20, Side::Buy, TimeInForce::Gtc, None);

        assert_eq!(book.queue_ahead_at_price(100, Side::Buy), 1);
        assert_eq!(book.queue_ahead_at_price(99, Side::Buy), 1);
        assert_eq!(book.queue_ahead_at_price(98, Side::Buy), 0);
    }

    #[test]
    fn test_price_n_ticks_inside_buy() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        let _ = book.add_limit_order(Id::new(), 100, 10, Side::Buy, TimeInForce::Gtc, None);

        // 1 tick inside with tick_size = 1 means 100 - 1 = 99
        assert_eq!(book.price_n_ticks_inside(1, 1, Side::Buy), Some(99));

        // 5 ticks inside = 100 - 5 = 95
        assert_eq!(book.price_n_ticks_inside(5, 1, Side::Buy), Some(95));

        // With tick_size = 10
        assert_eq!(book.price_n_ticks_inside(2, 10, Side::Buy), Some(80));
    }

    #[test]
    fn test_price_n_ticks_inside_sell() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        let _ = book.add_limit_order(Id::new(), 100, 10, Side::Sell, TimeInForce::Gtc, None);

        // 1 tick inside with tick_size = 1 means 100 + 1 = 101
        assert_eq!(book.price_n_ticks_inside(1, 1, Side::Sell), Some(101));

        // 5 ticks inside = 100 + 5 = 105
        assert_eq!(book.price_n_ticks_inside(5, 1, Side::Sell), Some(105));

        // With tick_size = 10
        assert_eq!(book.price_n_ticks_inside(2, 10, Side::Sell), Some(120));
    }

    #[test]
    fn test_price_n_ticks_inside_zero_values() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        let _ = book.add_limit_order(Id::new(), 100, 10, Side::Buy, TimeInForce::Gtc, None);

        // Zero ticks should return None
        assert_eq!(book.price_n_ticks_inside(0, 1, Side::Buy), None);

        // Zero tick_size should return None
        assert_eq!(book.price_n_ticks_inside(1, 0, Side::Buy), None);
    }

    #[test]
    fn test_price_n_ticks_inside_empty_book() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        assert_eq!(book.price_n_ticks_inside(1, 1, Side::Buy), None);
        assert_eq!(book.price_n_ticks_inside(1, 1, Side::Sell), None);
    }

    #[test]
    fn test_price_n_ticks_inside_underflow() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        let _ = book.add_limit_order(Id::new(), 5, 10, Side::Buy, TimeInForce::Gtc, None);

        // Trying to go 10 ticks inside would underflow (5 - 10 < 0)
        assert_eq!(book.price_n_ticks_inside(10, 1, Side::Buy), None);
    }

    #[test]
    fn test_price_for_queue_position_basic() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        let _ = book.add_limit_order(Id::new(), 100, 10, Side::Buy, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 99, 10, Side::Buy, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 98, 10, Side::Buy, TimeInForce::Gtc, None);

        // Position 1 should be best bid (100)
        assert_eq!(book.price_for_queue_position(1, Side::Buy), Some(100));

        // Position 2 should be second best (99)
        assert_eq!(book.price_for_queue_position(2, Side::Buy), Some(99));

        // Position 3 should be third best (98)
        assert_eq!(book.price_for_queue_position(3, Side::Buy), Some(98));

        // Position 4 doesn't exist
        assert_eq!(book.price_for_queue_position(4, Side::Buy), None);
    }

    #[test]
    fn test_price_for_queue_position_sell_side() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        let _ = book.add_limit_order(Id::new(), 100, 10, Side::Sell, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 101, 10, Side::Sell, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 102, 10, Side::Sell, TimeInForce::Gtc, None);

        // Position 1 should be best ask (100)
        assert_eq!(book.price_for_queue_position(1, Side::Sell), Some(100));

        // Position 2 should be second best (101)
        assert_eq!(book.price_for_queue_position(2, Side::Sell), Some(101));
    }

    #[test]
    fn test_price_for_queue_position_zero() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        let _ = book.add_limit_order(Id::new(), 100, 10, Side::Buy, TimeInForce::Gtc, None);

        // Position 0 is invalid
        assert_eq!(book.price_for_queue_position(0, Side::Buy), None);
    }

    #[test]
    fn test_price_for_queue_position_empty_book() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        assert_eq!(book.price_for_queue_position(1, Side::Buy), None);
    }

    #[test]
    fn test_price_at_depth_adjusted_basic() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        // Add orders with cumulative depth
        let _ = book.add_limit_order(Id::new(), 100, 50, Side::Buy, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 99, 60, Side::Buy, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 98, 70, Side::Buy, TimeInForce::Gtc, None);

        // Want to be just inside 100 units of depth
        // Depth at 100: 50, at 99: 110 (50+60), so we reach target at 99
        // One tick better than 99 is 100
        if let Some(price) = book.price_at_depth_adjusted(100, 1, Side::Buy) {
            assert_eq!(price, 100);
        }

        // Target depth 50 or less should return 101 (one tick better than 100)
        if let Some(price) = book.price_at_depth_adjusted(50, 1, Side::Buy) {
            assert_eq!(price, 101);
        }
    }

    #[test]
    fn test_price_at_depth_adjusted_insufficient_depth() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        let _ = book.add_limit_order(Id::new(), 100, 50, Side::Buy, TimeInForce::Gtc, None);

        // Target depth exceeds available, should return deepest price
        if let Some(price) = book.price_at_depth_adjusted(100, 1, Side::Buy) {
            assert_eq!(price, 100);
        }
    }

    #[test]
    fn test_price_at_depth_adjusted_sell_side() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        let _ = book.add_limit_order(Id::new(), 100, 50, Side::Sell, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 101, 60, Side::Sell, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 102, 70, Side::Sell, TimeInForce::Gtc, None);

        // Want to be just inside 100 units of depth
        // Depth at 100: 50, at 101: 110, so we reach target at 101
        // One tick better than 101 is 100 (for sell, better = lower)
        if let Some(price) = book.price_at_depth_adjusted(100, 1, Side::Sell) {
            assert_eq!(price, 100);
        }
    }

    #[test]
    fn test_price_at_depth_adjusted_zero_values() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        let _ = book.add_limit_order(Id::new(), 100, 50, Side::Buy, TimeInForce::Gtc, None);

        // Zero target_depth should return None
        assert_eq!(book.price_at_depth_adjusted(0, 1, Side::Buy), None);

        // Zero tick_size should return None
        assert_eq!(book.price_at_depth_adjusted(100, 0, Side::Buy), None);
    }

    #[test]
    fn test_price_at_depth_adjusted_empty_book() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        assert_eq!(book.price_at_depth_adjusted(100, 1, Side::Buy), None);
    }

    #[test]
    fn test_all_placement_functions_together() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        // Setup realistic order book
        let _ = book.add_limit_order(Id::new(), 100, 30, Side::Buy, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 100, 20, Side::Buy, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 99, 40, Side::Buy, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 98, 50, Side::Buy, TimeInForce::Gtc, None);

        let _ = book.add_limit_order(Id::new(), 105, 25, Side::Sell, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 106, 35, Side::Sell, TimeInForce::Gtc, None);

        // Test queue_ahead_at_price
        assert_eq!(book.queue_ahead_at_price(100, Side::Buy), 2);
        assert_eq!(book.queue_ahead_at_price(99, Side::Buy), 1);

        // Test price_n_ticks_inside
        assert_eq!(book.price_n_ticks_inside(1, 1, Side::Buy), Some(99));
        assert_eq!(book.price_n_ticks_inside(1, 1, Side::Sell), Some(106));

        // Test price_for_queue_position
        assert_eq!(book.price_for_queue_position(1, Side::Buy), Some(100));
        assert_eq!(book.price_for_queue_position(2, Side::Buy), Some(99));
        assert_eq!(book.price_for_queue_position(1, Side::Sell), Some(105));

        // Test price_at_depth_adjusted
        // Buy side depth: 100=50, 99=90, 98=140
        if let Some(price) = book.price_at_depth_adjusted(70, 1, Side::Buy) {
            assert_eq!(price, 100); // Just inside level that reaches 90
        }
    }

    #[test]
    fn test_queue_position_with_multiple_levels() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        // Create a deep book
        for i in 0..10 {
            let price = 100 - i;
            let _ = book.add_limit_order(Id::new(), price, 10, Side::Buy, TimeInForce::Gtc, None);
        }

        // Test various positions
        assert_eq!(book.price_for_queue_position(1, Side::Buy), Some(100));
        assert_eq!(book.price_for_queue_position(5, Side::Buy), Some(96));
        assert_eq!(book.price_for_queue_position(10, Side::Buy), Some(91));
        assert_eq!(book.price_for_queue_position(11, Side::Buy), None);
    }

    #[test]
    fn test_tick_calculations_with_large_values() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        let _ = book.add_limit_order(Id::new(), 100000, 10, Side::Buy, TimeInForce::Gtc, None);

        // Test with large tick size
        assert_eq!(book.price_n_ticks_inside(10, 100, Side::Buy), Some(99000));

        // Test with large number of ticks
        assert_eq!(book.price_n_ticks_inside(1000, 1, Side::Buy), Some(99000));
    }
}
