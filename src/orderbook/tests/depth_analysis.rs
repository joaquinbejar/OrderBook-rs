//! Tests for depth analysis methods

#[cfg(test)]
mod tests {
    use crate::OrderBook;
    use pricelevel::{Id, Side, TimeInForce};

    #[test]
    fn test_price_at_depth_buy_side() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        // Add bid orders at different price levels
        let _ = book.add_limit_order(Id::new(), 100, 10, Side::Buy, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 99, 20, Side::Buy, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 98, 30, Side::Buy, TimeInForce::Gtc, None);

        // Target depth of 10 should be at price 100
        assert_eq!(book.price_at_depth(10, Side::Buy), Some(100));

        // Target depth of 25 should be at price 99 (10 + 20 >= 25)
        assert_eq!(book.price_at_depth(25, Side::Buy), Some(99));

        // Target depth of 60 should be at price 98 (10 + 20 + 30 = 60)
        assert_eq!(book.price_at_depth(60, Side::Buy), Some(98));

        // Target depth of 100 exceeds available liquidity
        assert_eq!(book.price_at_depth(100, Side::Buy), None);
    }

    #[test]
    fn test_price_at_depth_sell_side() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        // Add ask orders at different price levels
        let _ = book.add_limit_order(Id::new(), 101, 15, Side::Sell, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 102, 25, Side::Sell, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 103, 35, Side::Sell, TimeInForce::Gtc, None);

        // Target depth of 15 should be at price 101
        assert_eq!(book.price_at_depth(15, Side::Sell), Some(101));

        // Target depth of 30 should be at price 102 (15 + 25 >= 30)
        assert_eq!(book.price_at_depth(30, Side::Sell), Some(102));

        // Target depth of 75 should be at price 103 (15 + 25 + 35 = 75)
        assert_eq!(book.price_at_depth(75, Side::Sell), Some(103));

        // Target depth of 100 exceeds available liquidity
        assert_eq!(book.price_at_depth(100, Side::Sell), None);
    }

    #[test]
    fn test_price_at_depth_empty_book() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        // Empty book should return None
        assert_eq!(book.price_at_depth(10, Side::Buy), None);
        assert_eq!(book.price_at_depth(10, Side::Sell), None);
    }

    #[test]
    fn test_cumulative_depth_to_target_buy_side() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        // Add bid orders at different price levels
        let _ = book.add_limit_order(Id::new(), 100, 10, Side::Buy, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 99, 20, Side::Buy, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 98, 30, Side::Buy, TimeInForce::Gtc, None);

        // Target depth of 10 should return (100, 10)
        assert_eq!(
            book.cumulative_depth_to_target(10, Side::Buy),
            Some((100, 10))
        );

        // Target depth of 25 should return (99, 30) - cumulative is 10 + 20 = 30
        assert_eq!(
            book.cumulative_depth_to_target(25, Side::Buy),
            Some((99, 30))
        );

        // Target depth of 60 should return (98, 60) - cumulative is 10 + 20 + 30 = 60
        assert_eq!(
            book.cumulative_depth_to_target(60, Side::Buy),
            Some((98, 60))
        );

        // Target depth of 100 exceeds available liquidity
        assert_eq!(book.cumulative_depth_to_target(100, Side::Buy), None);
    }

    #[test]
    fn test_cumulative_depth_to_target_sell_side() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        // Add ask orders at different price levels
        let _ = book.add_limit_order(Id::new(), 101, 15, Side::Sell, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 102, 25, Side::Sell, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 103, 35, Side::Sell, TimeInForce::Gtc, None);

        // Target depth of 15 should return (101, 15)
        assert_eq!(
            book.cumulative_depth_to_target(15, Side::Sell),
            Some((101, 15))
        );

        // Target depth of 30 should return (102, 40) - cumulative is 15 + 25 = 40
        assert_eq!(
            book.cumulative_depth_to_target(30, Side::Sell),
            Some((102, 40))
        );

        // Target depth of 75 should return (103, 75) - cumulative is 15 + 25 + 35 = 75
        assert_eq!(
            book.cumulative_depth_to_target(75, Side::Sell),
            Some((103, 75))
        );

        // Target depth of 100 exceeds available liquidity
        assert_eq!(book.cumulative_depth_to_target(100, Side::Sell), None);
    }

    #[test]
    fn test_cumulative_depth_to_target_empty_book() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        // Empty book should return None
        assert_eq!(book.cumulative_depth_to_target(10, Side::Buy), None);
        assert_eq!(book.cumulative_depth_to_target(10, Side::Sell), None);
    }

    #[test]
    fn test_total_depth_at_levels_buy_side() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        // Add bid orders at different price levels
        let _ = book.add_limit_order(Id::new(), 100, 10, Side::Buy, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 99, 20, Side::Buy, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 98, 30, Side::Buy, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 97, 40, Side::Buy, TimeInForce::Gtc, None);

        // Top 1 level should have depth of 10
        assert_eq!(book.total_depth_at_levels(1, Side::Buy), 10);

        // Top 2 levels should have depth of 30 (10 + 20)
        assert_eq!(book.total_depth_at_levels(2, Side::Buy), 30);

        // Top 3 levels should have depth of 60 (10 + 20 + 30)
        assert_eq!(book.total_depth_at_levels(3, Side::Buy), 60);

        // Top 4 levels should have depth of 100 (10 + 20 + 30 + 40)
        assert_eq!(book.total_depth_at_levels(4, Side::Buy), 100);

        // Requesting more levels than available should return all available
        assert_eq!(book.total_depth_at_levels(10, Side::Buy), 100);
    }

    #[test]
    fn test_total_depth_at_levels_sell_side() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        // Add ask orders at different price levels
        let _ = book.add_limit_order(Id::new(), 101, 15, Side::Sell, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 102, 25, Side::Sell, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 103, 35, Side::Sell, TimeInForce::Gtc, None);

        // Top 1 level should have depth of 15
        assert_eq!(book.total_depth_at_levels(1, Side::Sell), 15);

        // Top 2 levels should have depth of 40 (15 + 25)
        assert_eq!(book.total_depth_at_levels(2, Side::Sell), 40);

        // Top 3 levels should have depth of 75 (15 + 25 + 35)
        assert_eq!(book.total_depth_at_levels(3, Side::Sell), 75);

        // Requesting more levels than available should return all available
        assert_eq!(book.total_depth_at_levels(10, Side::Sell), 75);
    }

    #[test]
    fn test_total_depth_at_levels_empty_book() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        // Empty book should return 0
        assert_eq!(book.total_depth_at_levels(5, Side::Buy), 0);
        assert_eq!(book.total_depth_at_levels(5, Side::Sell), 0);
    }

    #[test]
    fn test_total_depth_at_levels_zero_levels() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        // Add some orders
        let _ = book.add_limit_order(Id::new(), 100, 10, Side::Buy, TimeInForce::Gtc, None);

        // Zero levels should return 0
        assert_eq!(book.total_depth_at_levels(0, Side::Buy), 0);
        assert_eq!(book.total_depth_at_levels(0, Side::Sell), 0);
    }

    #[test]
    fn test_depth_methods_with_multiple_orders_per_level() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        // Add multiple orders at the same price level
        let _ = book.add_limit_order(Id::new(), 100, 10, Side::Buy, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 100, 15, Side::Buy, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 99, 20, Side::Buy, TimeInForce::Gtc, None);

        // Total at price 100 should be 25 (10 + 15)
        assert_eq!(book.price_at_depth(25, Side::Buy), Some(100));
        assert_eq!(book.price_at_depth(30, Side::Buy), Some(99));

        // Cumulative depth
        assert_eq!(
            book.cumulative_depth_to_target(25, Side::Buy),
            Some((100, 25))
        );
        assert_eq!(
            book.cumulative_depth_to_target(30, Side::Buy),
            Some((99, 45))
        );

        // Total depth at levels
        assert_eq!(book.total_depth_at_levels(1, Side::Buy), 25);
        assert_eq!(book.total_depth_at_levels(2, Side::Buy), 45);
    }

    #[test]
    fn test_depth_methods_priority_order_buy_side() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        // Add bids in non-sequential order to ensure priority is correct
        let _ = book.add_limit_order(Id::new(), 98, 30, Side::Buy, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 100, 10, Side::Buy, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 99, 20, Side::Buy, TimeInForce::Gtc, None);

        // Should iterate from highest to lowest (100, 99, 98)
        assert_eq!(book.total_depth_at_levels(1, Side::Buy), 10); // 100
        assert_eq!(book.total_depth_at_levels(2, Side::Buy), 30); // 100, 99
        assert_eq!(book.total_depth_at_levels(3, Side::Buy), 60); // 100, 99, 98
    }

    #[test]
    fn test_depth_methods_priority_order_sell_side() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        // Add asks in non-sequential order to ensure priority is correct
        let _ = book.add_limit_order(Id::new(), 103, 35, Side::Sell, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 101, 15, Side::Sell, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 102, 25, Side::Sell, TimeInForce::Gtc, None);

        // Should iterate from lowest to highest (101, 102, 103)
        assert_eq!(book.total_depth_at_levels(1, Side::Sell), 15); // 101
        assert_eq!(book.total_depth_at_levels(2, Side::Sell), 40); // 101, 102
        assert_eq!(book.total_depth_at_levels(3, Side::Sell), 75); // 101, 102, 103
    }
}
