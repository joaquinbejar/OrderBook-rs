//! Tests for functional-style order book iterators

#[cfg(test)]
mod tests {
    use crate::OrderBook;
    use pricelevel::{Id, Side, TimeInForce};

    fn setup_test_book() -> OrderBook {
        let book = OrderBook::new("TEST");

        // Add buy orders at different prices
        let _ = book.add_limit_order(Id::new(), 100, 10, Side::Buy, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 95, 15, Side::Buy, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 90, 20, Side::Buy, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 85, 25, Side::Buy, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 80, 30, Side::Buy, TimeInForce::Gtc, None);

        // Add sell orders at different prices
        let _ = book.add_limit_order(Id::new(), 105, 12, Side::Sell, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 110, 18, Side::Sell, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 115, 24, Side::Sell, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 120, 30, Side::Sell, TimeInForce::Gtc, None);

        book
    }

    #[test]
    fn test_levels_with_cumulative_depth_buy() {
        let book = setup_test_book();

        let levels: Vec<_> = book.levels_with_cumulative_depth(Side::Buy).collect();

        assert_eq!(levels.len(), 5);

        // First level (best bid)
        assert_eq!(levels[0].price, 100);
        assert_eq!(levels[0].quantity, 10);
        assert_eq!(levels[0].cumulative_depth, 10);

        // Second level
        assert_eq!(levels[1].price, 95);
        assert_eq!(levels[1].quantity, 15);
        assert_eq!(levels[1].cumulative_depth, 25);

        // Third level
        assert_eq!(levels[2].price, 90);
        assert_eq!(levels[2].quantity, 20);
        assert_eq!(levels[2].cumulative_depth, 45);
    }

    #[test]
    fn test_levels_with_cumulative_depth_sell() {
        let book = setup_test_book();

        let levels: Vec<_> = book.levels_with_cumulative_depth(Side::Sell).collect();

        assert_eq!(levels.len(), 4);

        // First level (best ask)
        assert_eq!(levels[0].price, 105);
        assert_eq!(levels[0].quantity, 12);
        assert_eq!(levels[0].cumulative_depth, 12);

        // Second level
        assert_eq!(levels[1].price, 110);
        assert_eq!(levels[1].quantity, 18);
        assert_eq!(levels[1].cumulative_depth, 30);
    }

    #[test]
    fn test_levels_with_cumulative_depth_take() {
        let book = setup_test_book();

        // Take only first 3 levels
        let levels: Vec<_> = book
            .levels_with_cumulative_depth(Side::Buy)
            .take(3)
            .collect();

        assert_eq!(levels.len(), 3);
        assert_eq!(levels[2].price, 90);
    }

    #[test]
    fn test_levels_with_cumulative_depth_empty_book() {
        let book: OrderBook = OrderBook::new("TEST");

        let levels: Vec<_> = book.levels_with_cumulative_depth(Side::Buy).collect();

        assert_eq!(levels.len(), 0);
    }

    #[test]
    fn test_levels_until_depth_exact() {
        let book = setup_test_book();

        // Target depth of 25 should give us 2 levels (10 + 15)
        let levels: Vec<_> = book.levels_until_depth(25, Side::Buy).collect();

        assert_eq!(levels.len(), 2);
        assert_eq!(levels[0].price, 100);
        assert_eq!(levels[1].price, 95);
        assert_eq!(levels[1].cumulative_depth, 25);
    }

    #[test]
    fn test_levels_until_depth_partial() {
        let book = setup_test_book();

        // Target depth of 30 should give us 3 levels (10 + 15 + 20 = 45)
        let levels: Vec<_> = book.levels_until_depth(30, Side::Buy).collect();

        assert_eq!(levels.len(), 3);
        assert_eq!(levels[2].cumulative_depth, 45);
    }

    #[test]
    fn test_levels_until_depth_exceeds_available() {
        let book = setup_test_book();

        // Target depth of 1000 should give us all levels
        let levels: Vec<_> = book.levels_until_depth(1000, Side::Buy).collect();

        assert_eq!(levels.len(), 5);
    }

    #[test]
    fn test_levels_until_depth_zero() {
        let book = setup_test_book();

        // Zero target should still return first level if it has any quantity
        let levels: Vec<_> = book.levels_until_depth(0, Side::Buy).collect();

        assert_eq!(levels.len(), 1);
    }

    #[test]
    fn test_levels_in_range_basic() {
        let book = setup_test_book();

        // Range 85-95 should include 3 levels
        let levels: Vec<_> = book.levels_in_range(85, 95, Side::Buy).collect();

        assert_eq!(levels.len(), 3);
        assert_eq!(levels[0].price, 95);
        assert_eq!(levels[1].price, 90);
        assert_eq!(levels[2].price, 85);
    }

    #[test]
    fn test_levels_in_range_single() {
        let book = setup_test_book();

        // Exact match for one level
        let levels: Vec<_> = book.levels_in_range(100, 100, Side::Buy).collect();

        assert_eq!(levels.len(), 1);
        assert_eq!(levels[0].price, 100);
    }

    #[test]
    fn test_levels_in_range_no_match() {
        let book = setup_test_book();

        // Range with no levels
        let levels: Vec<_> = book.levels_in_range(101, 104, Side::Buy).collect();

        assert_eq!(levels.len(), 0);
    }

    #[test]
    fn test_levels_in_range_partial() {
        let book = setup_test_book();

        // Range that partially overlaps
        let levels: Vec<_> = book.levels_in_range(92, 102, Side::Buy).collect();

        assert_eq!(levels.len(), 2); // 100 and 95
        assert_eq!(levels[0].price, 100);
        assert_eq!(levels[1].price, 95);
    }

    #[test]
    fn test_find_level_by_quantity() {
        let book = setup_test_book();

        // Find first level with quantity > 15
        let level = book.find_level(Side::Buy, |info| info.quantity > 15);

        assert!(level.is_some());
        let level = level.unwrap();
        assert_eq!(level.price, 90); // First level with 20 units
    }

    #[test]
    fn test_find_level_by_cumulative_depth() {
        let book = setup_test_book();

        // Find first level where cumulative depth exceeds 30
        let level = book.find_level(Side::Buy, |info| info.cumulative_depth > 30);

        assert!(level.is_some());
        let level = level.unwrap();
        assert_eq!(level.price, 90);
        assert_eq!(level.cumulative_depth, 45);
    }

    #[test]
    fn test_find_level_not_found() {
        let book = setup_test_book();

        // Find level with impossible condition
        let level = book.find_level(Side::Buy, |info| info.quantity > 1000);

        assert!(level.is_none());
    }

    #[test]
    fn test_find_level_empty_book() {
        let book: OrderBook = OrderBook::new("TEST");

        let level = book.find_level(Side::Buy, |_| true);

        assert!(level.is_none());
    }

    #[test]
    fn test_iterator_composition() {
        let book = setup_test_book();

        // Complex functional pipeline
        let total_qty: u64 = book
            .levels_with_cumulative_depth(Side::Buy)
            .take(3) // Only first 3 levels
            .filter(|level| level.quantity >= 15) // Only levels with 15+ units
            .map(|level| level.quantity)
            .sum();

        // Should sum 15 (second level) + 20 (third level) = 35
        assert_eq!(total_qty, 35);
    }

    #[test]
    fn test_iterator_short_circuit() {
        let book = setup_test_book();

        // Find first level and stop
        let first = book.levels_with_cumulative_depth(Side::Buy).next();

        assert!(first.is_some());
        assert_eq!(first.unwrap().price, 100);
    }

    #[test]
    fn test_levels_in_range_map_sum() {
        let book = setup_test_book();

        // Calculate total quantity in range
        let total: u64 = book
            .levels_in_range(85, 95, Side::Buy)
            .map(|level| level.quantity)
            .sum();

        // Should sum 25 + 20 + 15 = 60
        assert_eq!(total, 60);
    }

    #[test]
    fn test_levels_until_depth_with_filter() {
        let book = setup_test_book();

        // Get levels until depth 50, but only those with even prices
        let levels: Vec<_> = book
            .levels_until_depth(50, Side::Buy)
            .filter(|level| level.price % 2 == 0)
            .collect();

        // Should include 100, 90 (even prices reached before cumulative depth of 50)
        // Cumulative: 100(10), 95(25), 90(45), 85(70) - stops at 85
        // After filter: 100, 90
        assert_eq!(levels.len(), 2);
        assert_eq!(levels[0].price, 100);
        assert_eq!(levels[1].price, 90);
    }

    #[test]
    fn test_functional_style_analysis() {
        let book = setup_test_book();

        // Real-world scenario: Find average order size in top 3 levels
        let levels: Vec<_> = book
            .levels_with_cumulative_depth(Side::Buy)
            .take(3)
            .collect();

        let total_qty: u64 = levels.iter().map(|l| l.quantity).sum();
        let avg_size = total_qty as f64 / levels.len() as f64;

        // (10 + 15 + 20) / 3 = 15.0
        assert!((avg_size - 15.0).abs() < 0.01);
    }

    #[test]
    fn test_find_depth_threshold() {
        let book = setup_test_book();

        // Find price level where we have at least 40 units cumulative
        let level = book.find_level(Side::Buy, |info| info.cumulative_depth >= 40);

        assert!(level.is_some());
        let level = level.unwrap();
        assert_eq!(level.price, 90);
        assert_eq!(level.cumulative_depth, 45);
    }

    #[test]
    fn test_sell_side_iterators() {
        let book = setup_test_book();

        // Test sell side with levels_until_depth
        let levels: Vec<_> = book.levels_until_depth(30, Side::Sell).collect();

        // Should get 105 (12) + 110 (18) = 30 cumulative
        assert_eq!(levels.len(), 2);
        assert_eq!(levels[0].price, 105);
        assert_eq!(levels[1].price, 110);
        assert_eq!(levels[1].cumulative_depth, 30);
    }

    #[test]
    fn test_count_levels() {
        let book = setup_test_book();

        // Count levels with quantity > 20
        let count = book
            .levels_with_cumulative_depth(Side::Buy)
            .filter(|level| level.quantity > 20)
            .count();

        assert_eq!(count, 2); // 25 and 30
    }
}
