//! Tests for market impact simulation and liquidity analysis

#[cfg(test)]
mod tests {
    use crate::OrderBook;
    use pricelevel::{Id, Side, TimeInForce};

    #[test]
    fn test_market_impact_basic() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        // Add ask orders
        let _ = book.add_limit_order(Id::new(), 100, 10, Side::Sell, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 105, 15, Side::Sell, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 110, 20, Side::Sell, TimeInForce::Gtc, None);

        // Buy 20 units (will consume 2 levels)
        let impact = book.market_impact(20, Side::Buy);

        assert_eq!(impact.total_quantity_available, 20);
        assert_eq!(impact.levels_consumed, 2);
        assert_eq!(impact.worst_price, 105);
        assert_eq!(impact.slippage, 5);
        // avg_price = (100*10 + 105*10) / 20 = 102.5
        assert_eq!(impact.avg_price, 102.5);
    }

    #[test]
    fn test_market_impact_insufficient_liquidity() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        // Add limited ask orders
        let _ = book.add_limit_order(Id::new(), 100, 10, Side::Sell, TimeInForce::Gtc, None);

        // Request more than available
        let impact = book.market_impact(50, Side::Buy);

        assert_eq!(impact.total_quantity_available, 10);
        assert_eq!(impact.levels_consumed, 1);
        assert!(!impact.can_fill(50));
        assert_eq!(impact.fill_ratio(50), 0.2);
    }

    #[test]
    fn test_market_impact_empty_book() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        let impact = book.market_impact(100, Side::Buy);

        assert_eq!(impact.avg_price, 0.0);
        assert_eq!(impact.worst_price, 0);
        assert_eq!(impact.levels_consumed, 0);
        assert_eq!(impact.total_quantity_available, 0);
    }

    #[test]
    fn test_market_impact_zero_quantity() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        let _ = book.add_limit_order(Id::new(), 100, 10, Side::Sell, TimeInForce::Gtc, None);

        let impact = book.market_impact(0, Side::Buy);

        assert_eq!(impact.total_quantity_available, 0);
        assert_eq!(impact.levels_consumed, 0);
    }

    #[test]
    fn test_market_impact_sell_side() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        // Add bid orders
        let _ = book.add_limit_order(Id::new(), 100, 10, Side::Buy, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 95, 15, Side::Buy, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 90, 20, Side::Buy, TimeInForce::Gtc, None);

        // Sell 20 units (will consume 2 levels)
        let impact = book.market_impact(20, Side::Sell);

        assert_eq!(impact.total_quantity_available, 20);
        assert_eq!(impact.levels_consumed, 2);
        assert_eq!(impact.worst_price, 95);
        assert_eq!(impact.slippage, 5); // 100 - 95
        // avg_price = (100*10 + 95*10) / 20 = 97.5
        assert_eq!(impact.avg_price, 97.5);
    }

    #[test]
    fn test_market_impact_slippage_bps() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        // Add ask orders at 10000 and 10100
        let _ = book.add_limit_order(Id::new(), 10000, 10, Side::Sell, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 10100, 10, Side::Sell, TimeInForce::Gtc, None);

        // Buy 15 units (will go into second level)
        let impact = book.market_impact(15, Side::Buy);

        // Slippage = 100, best_price = 10000, bps = (100/10000) * 10000 = 100 bps
        assert_eq!(impact.slippage, 100);
        assert_eq!(impact.slippage_bps, 100.0);
    }

    #[test]
    fn test_simulate_market_order_basic() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        // Add ask orders
        let _ = book.add_limit_order(Id::new(), 100, 10, Side::Sell, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 105, 15, Side::Sell, TimeInForce::Gtc, None);

        // Buy 20 units
        let simulation = book.simulate_market_order(20, Side::Buy);

        assert_eq!(simulation.fills.len(), 2);
        assert_eq!(simulation.fills[0], (100, 10));
        assert_eq!(simulation.fills[1], (105, 10));
        assert_eq!(simulation.total_filled, 20);
        assert_eq!(simulation.remaining_quantity, 0);
        assert!(simulation.is_fully_filled());
        // avg_price = (100*10 + 105*10) / 20 = 102.5
        assert_eq!(simulation.avg_price, 102.5);
    }

    #[test]
    fn test_simulate_market_order_partial_fill() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        // Add limited ask orders
        let _ = book.add_limit_order(Id::new(), 100, 10, Side::Sell, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 105, 15, Side::Sell, TimeInForce::Gtc, None);

        // Request more than available
        let simulation = book.simulate_market_order(50, Side::Buy);

        assert_eq!(simulation.total_filled, 25);
        assert_eq!(simulation.remaining_quantity, 25);
        assert!(!simulation.is_fully_filled());
        assert_eq!(simulation.levels_count(), 2);
    }

    #[test]
    fn test_simulate_market_order_empty_book() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        let simulation = book.simulate_market_order(100, Side::Buy);

        assert_eq!(simulation.fills.len(), 0);
        assert_eq!(simulation.total_filled, 0);
        assert_eq!(simulation.remaining_quantity, 100);
    }

    #[test]
    fn test_simulate_market_order_total_cost() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        let _ = book.add_limit_order(Id::new(), 100, 10, Side::Sell, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 105, 10, Side::Sell, TimeInForce::Gtc, None);

        let simulation = book.simulate_market_order(20, Side::Buy);

        // Total cost = (100*10) + (105*10) = 2050
        assert_eq!(simulation.total_cost(), 2050);
    }

    #[test]
    fn test_liquidity_in_range_basic() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        // Add buy orders at different prices
        let _ = book.add_limit_order(Id::new(), 100, 10, Side::Buy, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 105, 15, Side::Buy, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 110, 20, Side::Buy, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 115, 25, Side::Buy, TimeInForce::Gtc, None);

        // Get liquidity between 105 and 110 (inclusive)
        let liquidity = book.liquidity_in_range(105, 110, Side::Buy);

        assert_eq!(liquidity, 35); // 15 + 20
    }

    #[test]
    fn test_liquidity_in_range_full_range() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        let _ = book.add_limit_order(Id::new(), 100, 10, Side::Buy, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 105, 15, Side::Buy, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 110, 20, Side::Buy, TimeInForce::Gtc, None);

        // Get all liquidity
        let liquidity = book.liquidity_in_range(0, u128::MAX, Side::Buy);

        assert_eq!(liquidity, 45); // 10 + 15 + 20
    }

    #[test]
    fn test_liquidity_in_range_no_overlap() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        let _ = book.add_limit_order(Id::new(), 100, 10, Side::Buy, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 105, 15, Side::Buy, TimeInForce::Gtc, None);

        // Range outside of available prices
        let liquidity = book.liquidity_in_range(200, 300, Side::Buy);

        assert_eq!(liquidity, 0);
    }

    #[test]
    fn test_liquidity_in_range_invalid_range() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        let _ = book.add_limit_order(Id::new(), 100, 10, Side::Buy, TimeInForce::Gtc, None);

        // min_price > max_price
        let liquidity = book.liquidity_in_range(200, 100, Side::Buy);

        assert_eq!(liquidity, 0);
    }

    #[test]
    fn test_liquidity_in_range_empty_book() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        let liquidity = book.liquidity_in_range(100, 200, Side::Buy);

        assert_eq!(liquidity, 0);
    }

    #[test]
    fn test_liquidity_in_range_asks() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        // Add sell orders
        let _ = book.add_limit_order(Id::new(), 100, 10, Side::Sell, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 105, 15, Side::Sell, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 110, 20, Side::Sell, TimeInForce::Gtc, None);

        let liquidity = book.liquidity_in_range(100, 105, Side::Sell);

        assert_eq!(liquidity, 25); // 10 + 15
    }

    #[test]
    fn test_all_functions_together() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        // Setup order book
        let _ = book.add_limit_order(Id::new(), 99, 20, Side::Buy, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 100, 30, Side::Buy, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 101, 25, Side::Sell, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 102, 35, Side::Sell, TimeInForce::Gtc, None);

        // Test market impact
        let impact = book.market_impact(50, Side::Buy);
        assert_eq!(impact.total_quantity_available, 50);
        assert!(impact.can_fill(50));

        // Test simulation
        let simulation = book.simulate_market_order(50, Side::Buy);
        assert!(simulation.is_fully_filled());
        assert_eq!(simulation.levels_count(), 2);

        // Test liquidity
        let liquidity = book.liquidity_in_range(101, 102, Side::Sell);
        assert_eq!(liquidity, 60); // 25 + 35
    }
}
