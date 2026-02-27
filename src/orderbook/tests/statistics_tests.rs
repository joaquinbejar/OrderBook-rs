//! Tests for aggregate statistics and order book analysis

#[cfg(test)]
mod tests {
    use crate::OrderBook;
    use pricelevel::{Id, Side, TimeInForce};

    fn setup_test_book() -> OrderBook<()> {
        let book = OrderBook::<()>::new("TEST");

        // Add buy orders with varying sizes
        let _ = book.add_limit_order(Id::new(), 100, 10, Side::Buy, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 99, 20, Side::Buy, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 98, 30, Side::Buy, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 97, 40, Side::Buy, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 96, 50, Side::Buy, TimeInForce::Gtc, None);

        // Add sell orders with varying sizes
        let _ = book.add_limit_order(Id::new(), 101, 15, Side::Sell, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 102, 25, Side::Sell, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 103, 35, Side::Sell, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 104, 45, Side::Sell, TimeInForce::Gtc, None);

        book
    }

    #[test]
    fn test_depth_statistics_buy_basic() {
        let book = setup_test_book();

        let stats = book.depth_statistics(Side::Buy, 5);

        assert_eq!(stats.total_volume, 150); // 10 + 20 + 30 + 40 + 50
        assert_eq!(stats.levels_count, 5);
        assert_eq!(stats.avg_level_size, 30.0);
        assert_eq!(stats.min_level_size, 10);
        assert_eq!(stats.max_level_size, 50);
    }

    #[test]
    fn test_depth_statistics_sell_basic() {
        let book = setup_test_book();

        let stats = book.depth_statistics(Side::Sell, 4);

        assert_eq!(stats.total_volume, 120); // 15 + 25 + 35 + 45
        assert_eq!(stats.levels_count, 4);
        assert_eq!(stats.avg_level_size, 30.0);
        assert_eq!(stats.min_level_size, 15);
        assert_eq!(stats.max_level_size, 45);
    }

    #[test]
    fn test_depth_statistics_limited_levels() {
        let book = setup_test_book();

        let stats = book.depth_statistics(Side::Buy, 3);

        assert_eq!(stats.total_volume, 60); // 10 + 20 + 30
        assert_eq!(stats.levels_count, 3);
        assert_eq!(stats.avg_level_size, 20.0);
    }

    #[test]
    fn test_depth_statistics_all_levels() {
        let book = setup_test_book();

        let stats = book.depth_statistics(Side::Buy, 0);

        assert_eq!(stats.total_volume, 150);
        assert_eq!(stats.levels_count, 5);
    }

    #[test]
    fn test_depth_statistics_empty_book() {
        let book = OrderBook::<()>::new("TEST");

        let stats = book.depth_statistics(Side::Buy, 10);

        assert!(stats.is_empty());
        assert_eq!(stats.total_volume, 0);
        assert_eq!(stats.levels_count, 0);
    }

    #[test]
    fn test_depth_statistics_weighted_avg_price() {
        let book = setup_test_book();

        let stats = book.depth_statistics(Side::Buy, 3);

        // Weighted avg = (100*10 + 99*20 + 98*30) / (10 + 20 + 30)
        // = (1000 + 1980 + 2940) / 60 = 5920 / 60 = 98.666...
        assert!((stats.weighted_avg_price - 98.666).abs() < 0.01);
    }

    #[test]
    fn test_buy_sell_pressure() {
        let book = setup_test_book();

        let (buy_pressure, sell_pressure) = book.buy_sell_pressure();

        assert_eq!(buy_pressure, 150); // 10 + 20 + 30 + 40 + 50
        assert_eq!(sell_pressure, 120); // 15 + 25 + 35 + 45
    }

    #[test]
    fn test_buy_sell_pressure_empty_book() {
        let book = OrderBook::<()>::new("TEST");

        let (buy_pressure, sell_pressure) = book.buy_sell_pressure();

        assert_eq!(buy_pressure, 0);
        assert_eq!(sell_pressure, 0);
    }

    #[test]
    fn test_buy_sell_pressure_one_sided() {
        let book = OrderBook::<()>::new("TEST");
        let _ = book.add_limit_order(Id::new(), 100, 50, Side::Buy, TimeInForce::Gtc, None);

        let (buy_pressure, sell_pressure) = book.buy_sell_pressure();

        assert_eq!(buy_pressure, 50);
        assert_eq!(sell_pressure, 0);
    }

    #[test]
    fn test_is_thin_book_true() {
        let book = OrderBook::<()>::new("TEST");
        let _ = book.add_limit_order(Id::new(), 100, 5, Side::Buy, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 101, 5, Side::Sell, TimeInForce::Gtc, None);

        assert!(book.is_thin_book(100, 10));
    }

    #[test]
    fn test_is_thin_book_false() {
        let book = setup_test_book();

        assert!(!book.is_thin_book(100, 10));
    }

    #[test]
    fn test_is_thin_book_one_side_thin() {
        let book = OrderBook::<()>::new("TEST");
        let _ = book.add_limit_order(Id::new(), 100, 200, Side::Buy, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 101, 5, Side::Sell, TimeInForce::Gtc, None);

        // Sell side is thin
        assert!(book.is_thin_book(100, 10));
    }

    #[test]
    fn test_is_thin_book_empty() {
        let book = OrderBook::<()>::new("TEST");

        assert!(book.is_thin_book(1, 10));
    }

    #[test]
    fn test_depth_distribution_basic() {
        let book = setup_test_book();

        let distribution = book.depth_distribution(Side::Buy, 5);

        assert_eq!(distribution.len(), 5);

        // Verify total volume matches
        let total: u64 = distribution.iter().map(|bin| bin.volume).sum();
        assert_eq!(total, 150);
    }

    #[test]
    fn test_depth_distribution_bins() {
        let book = OrderBook::<()>::new("TEST");

        // Add orders at specific prices
        for i in 0..10 {
            let price = 100 - i;
            let _ = book.add_limit_order(Id::new(), price, 10, Side::Buy, TimeInForce::Gtc, None);
        }

        let distribution = book.depth_distribution(Side::Buy, 3);

        assert_eq!(distribution.len(), 3);

        // Check that bins cover the full range
        assert_eq!(distribution[0].min_price, 91);
        assert!(distribution.last().unwrap().max_price >= 100);
    }

    #[test]
    fn test_depth_distribution_zero_bins() {
        let book = setup_test_book();

        let distribution = book.depth_distribution(Side::Buy, 0);

        assert!(distribution.is_empty());
    }

    #[test]
    fn test_depth_distribution_empty_book() {
        let book = OrderBook::<()>::new("TEST");

        let distribution = book.depth_distribution(Side::Buy, 5);

        assert!(distribution.is_empty());
    }

    #[test]
    fn test_depth_distribution_single_price() {
        let book = OrderBook::<()>::new("TEST");
        let _ = book.add_limit_order(Id::new(), 100, 50, Side::Buy, TimeInForce::Gtc, None);

        let distribution = book.depth_distribution(Side::Buy, 3);

        assert_eq!(distribution.len(), 3);

        // All volume should be in the bins
        let total: u64 = distribution.iter().map(|bin| bin.volume).sum();
        assert_eq!(total, 50);
    }

    #[test]
    fn test_depth_distribution_level_count() {
        let book = OrderBook::<()>::new("TEST");

        // Add multiple orders at different prices
        let _ = book.add_limit_order(Id::new(), 100, 10, Side::Buy, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 99, 10, Side::Buy, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 98, 10, Side::Buy, TimeInForce::Gtc, None);

        let distribution = book.depth_distribution(Side::Buy, 2);

        // Verify level counts
        let total_levels: usize = distribution.iter().map(|bin| bin.level_count).sum();
        assert_eq!(total_levels, 3);
    }

    #[test]
    fn test_depth_statistics_std_dev() {
        let book = OrderBook::<()>::new("TEST");

        // Add orders with known sizes for std dev calculation
        let _ = book.add_limit_order(Id::new(), 100, 10, Side::Buy, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 99, 20, Side::Buy, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 98, 30, Side::Buy, TimeInForce::Gtc, None);

        let stats = book.depth_statistics(Side::Buy, 3);

        // Mean = 20, variance = ((10-20)^2 + (20-20)^2 + (30-20)^2) / 3 = 200/3
        // Std dev = sqrt(200/3) ≈ 8.165
        assert!((stats.std_dev_level_size - 8.165).abs() < 0.01);
    }

    #[test]
    fn test_order_book_imbalance_balanced() {
        let book = OrderBook::<()>::new("TEST");
        let _ = book.add_limit_order(Id::new(), 100, 50, Side::Buy, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 101, 50, Side::Sell, TimeInForce::Gtc, None);

        let imbalance = book.order_book_imbalance(5);

        assert!((imbalance - 0.0).abs() < 0.01);
    }

    #[test]
    fn test_order_book_imbalance_buy_heavy() {
        let book = OrderBook::<()>::new("TEST");
        let _ = book.add_limit_order(Id::new(), 100, 100, Side::Buy, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 101, 50, Side::Sell, TimeInForce::Gtc, None);

        let imbalance = book.order_book_imbalance(5);

        // More buy volume, expect positive imbalance
        assert!(imbalance > 0.0);
    }

    #[test]
    fn test_order_book_imbalance_sell_heavy() {
        let book = OrderBook::<()>::new("TEST");
        let _ = book.add_limit_order(Id::new(), 100, 30, Side::Buy, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 101, 100, Side::Sell, TimeInForce::Gtc, None);

        let imbalance = book.order_book_imbalance(5);

        // More sell volume, expect negative imbalance
        assert!(imbalance < 0.0);
    }

    #[test]
    fn test_combined_statistics_workflow() {
        let book = setup_test_book();

        // Get statistics
        let bid_stats = book.depth_statistics(Side::Buy, 10);
        let ask_stats = book.depth_statistics(Side::Sell, 10);

        // Check market conditions
        let _imbalance = book.order_book_imbalance(5);
        let (buy_pressure, sell_pressure) = book.buy_sell_pressure();

        // Verify consistency
        assert_eq!(buy_pressure, bid_stats.total_volume);
        assert_eq!(sell_pressure, ask_stats.total_volume);

        // Check thin book
        let is_thin = book.is_thin_book(1000, 5);
        assert!(is_thin); // Total volume is 270, less than 1000
    }

    #[test]
    fn test_depth_distribution_bins_match_range() {
        let book = OrderBook::<()>::new("TEST");

        // Add orders spanning a known range
        let _ = book.add_limit_order(Id::new(), 100, 10, Side::Buy, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 90, 10, Side::Buy, TimeInForce::Gtc, None);

        let distribution = book.depth_distribution(Side::Buy, 2);

        assert_eq!(distribution.len(), 2);

        // Verify bins cover the full range
        assert!(distribution[0].min_price <= 90);
        assert!(distribution.last().unwrap().max_price > 100);
    }
}
