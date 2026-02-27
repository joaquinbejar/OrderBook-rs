//! Tests for market metrics methods

#[cfg(test)]
mod tests {
    use crate::OrderBook;
    use pricelevel::{Id, Side, TimeInForce};

    #[test]
    fn test_spread_absolute() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        // Empty book
        assert_eq!(book.spread_absolute(), None);

        // Add bid and ask
        let _ = book.add_limit_order(Id::new(), 100, 10, Side::Buy, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 105, 10, Side::Sell, TimeInForce::Gtc, None);

        assert_eq!(book.spread_absolute(), Some(5));
    }

    #[test]
    fn test_spread_bps() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        // Empty book
        assert_eq!(book.spread_bps(None), None);

        // Add orders at 10000 and 10010
        let _ = book.add_limit_order(Id::new(), 10000, 10, Side::Buy, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 10010, 10, Side::Sell, TimeInForce::Gtc, None);

        // Spread = 10, mid = 10005, bps = (10/10005) * 10000 = ~9.995 bps
        let spread_bps = book.spread_bps(None).unwrap();
        assert!((spread_bps - 9.995).abs() < 0.01);
    }

    #[test]
    fn test_spread_bps_custom_multiplier() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        // Add orders at 10000 and 10010
        let _ = book.add_limit_order(Id::new(), 10000, 10, Side::Buy, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 10010, 10, Side::Sell, TimeInForce::Gtc, None);

        // Test with custom multiplier of 100 (for percentage)
        // Spread = 10, mid = 10005, pct = (10/10005) * 100 = ~0.0999%
        let spread_pct = book.spread_bps(Some(100.0)).unwrap();
        assert!((spread_pct - 0.0999).abs() < 0.001);

        // Test with default multiplier (10,000)
        let spread_bps_default = book.spread_bps(None).unwrap();
        let spread_bps_explicit = book.spread_bps(Some(10_000.0)).unwrap();
        assert_eq!(spread_bps_default, spread_bps_explicit);
    }

    #[test]
    fn test_vwap_buy_side() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        // Add ask orders at different price levels
        let _ = book.add_limit_order(Id::new(), 100, 10, Side::Sell, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 105, 15, Side::Sell, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 110, 20, Side::Sell, TimeInForce::Gtc, None);

        // VWAP for buying 10 units should be 100.0
        let vwap = book.vwap(10, Side::Buy).unwrap();
        assert_eq!(vwap, 100.0);

        // VWAP for buying 20 units (10@100 + 10@105) = (1000 + 1050) / 20 = 102.5
        let vwap = book.vwap(20, Side::Buy).unwrap();
        assert_eq!(vwap, 102.5);

        // VWAP for buying 25 units (10@100 + 15@105) = (1000 + 1575) / 25 = 103.0
        let vwap = book.vwap(25, Side::Buy).unwrap();
        assert_eq!(vwap, 103.0);

        // Insufficient liquidity
        assert_eq!(book.vwap(50, Side::Buy), None);
    }

    #[test]
    fn test_vwap_sell_side() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        // Add bid orders at different price levels
        let _ = book.add_limit_order(Id::new(), 100, 10, Side::Buy, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 95, 15, Side::Buy, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 90, 20, Side::Buy, TimeInForce::Gtc, None);

        // VWAP for selling 10 units should be 100.0
        let vwap = book.vwap(10, Side::Sell).unwrap();
        assert_eq!(vwap, 100.0);

        // VWAP for selling 20 units (10@100 + 10@95) = (1000 + 950) / 20 = 97.5
        let vwap = book.vwap(20, Side::Sell).unwrap();
        assert_eq!(vwap, 97.5);

        // VWAP for selling 25 units (10@100 + 15@95) = (1000 + 1425) / 25 = 97.0
        let vwap = book.vwap(25, Side::Sell).unwrap();
        assert_eq!(vwap, 97.0);

        // Insufficient liquidity
        assert_eq!(book.vwap(50, Side::Sell), None);
    }

    #[test]
    fn test_vwap_empty_book() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        assert_eq!(book.vwap(10, Side::Buy), None);
        assert_eq!(book.vwap(10, Side::Sell), None);
    }

    #[test]
    fn test_vwap_zero_quantity() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        let _ = book.add_limit_order(Id::new(), 100, 10, Side::Sell, TimeInForce::Gtc, None);

        assert_eq!(book.vwap(0, Side::Buy), None);
    }

    #[test]
    fn test_micro_price() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        // Empty book
        assert_eq!(book.micro_price(), None);

        // Add orders with equal volumes
        let _ = book.add_limit_order(Id::new(), 100, 50, Side::Buy, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 105, 50, Side::Sell, TimeInForce::Gtc, None);

        // With equal volumes, micro price equals mid price
        // micro = (105 * 50 + 100 * 50) / 100 = 10250 / 100 = 102.5
        let micro = book.micro_price().unwrap();
        assert_eq!(micro, 102.5);

        // Mid price should also be 102.5
        let mid = book.mid_price().unwrap();
        assert_eq!(mid, 102.5);
    }

    #[test]
    fn test_micro_price_imbalanced() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        // Add orders with imbalanced volumes (more bid volume)
        let _ = book.add_limit_order(Id::new(), 100, 70, Side::Buy, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 105, 30, Side::Sell, TimeInForce::Gtc, None);

        // micro = (105 * 70 + 100 * 30) / 100 = (7350 + 3000) / 100 = 103.5
        let micro = book.micro_price().unwrap();
        assert_eq!(micro, 103.5);

        // Mid price is 102.5, but micro price is higher due to more bid volume
        let mid = book.mid_price().unwrap();
        assert_eq!(mid, 102.5);
        assert!(micro > mid);
    }

    #[test]
    fn test_micro_price_zero_volumes() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        // This scenario shouldn't happen in practice, but test for robustness
        // If there are price levels but no volumes, micro_price should return None
        // Since we can't create this state easily, we just test empty book
        assert_eq!(book.micro_price(), None);
    }

    #[test]
    fn test_order_book_imbalance_balanced() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        // Add balanced orders
        let _ = book.add_limit_order(Id::new(), 100, 50, Side::Buy, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 105, 50, Side::Sell, TimeInForce::Gtc, None);

        // Imbalance should be 0 for balanced book
        let imbalance = book.order_book_imbalance(5);
        assert_eq!(imbalance, 0.0);
    }

    #[test]
    fn test_order_book_imbalance_buy_pressure() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        // More bid volume (buy pressure)
        let _ = book.add_limit_order(Id::new(), 100, 60, Side::Buy, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 105, 40, Side::Sell, TimeInForce::Gtc, None);

        // Imbalance = (60 - 40) / (60 + 40) = 20 / 100 = 0.2
        let imbalance = book.order_book_imbalance(5);
        assert_eq!(imbalance, 0.2);
    }

    #[test]
    fn test_order_book_imbalance_sell_pressure() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        // More ask volume (sell pressure)
        let _ = book.add_limit_order(Id::new(), 100, 30, Side::Buy, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 105, 70, Side::Sell, TimeInForce::Gtc, None);

        // Imbalance = (30 - 70) / (30 + 70) = -40 / 100 = -0.4
        let imbalance = book.order_book_imbalance(5);
        assert_eq!(imbalance, -0.4);
    }

    #[test]
    fn test_order_book_imbalance_multiple_levels() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        // Add multiple levels
        let _ = book.add_limit_order(Id::new(), 100, 10, Side::Buy, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 99, 20, Side::Buy, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 98, 30, Side::Buy, TimeInForce::Gtc, None);

        let _ = book.add_limit_order(Id::new(), 101, 15, Side::Sell, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 102, 25, Side::Sell, TimeInForce::Gtc, None);

        // Top 2 levels: bid=10+20=30, ask=15+25=40
        // Imbalance = (30 - 40) / (30 + 40) = -10 / 70 = -0.142857...
        let imbalance = book.order_book_imbalance(2);
        assert!((imbalance - (-10.0 / 70.0)).abs() < 0.0001);

        // Top 3 levels: bid=10+20+30=60, ask=15+25=40
        // Imbalance = (60 - 40) / (60 + 40) = 20 / 100 = 0.2
        let imbalance = book.order_book_imbalance(3);
        assert_eq!(imbalance, 0.2);
    }

    #[test]
    fn test_order_book_imbalance_empty_book() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        assert_eq!(book.order_book_imbalance(5), 0.0);
    }

    #[test]
    fn test_order_book_imbalance_zero_levels() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        let _ = book.add_limit_order(Id::new(), 100, 50, Side::Buy, TimeInForce::Gtc, None);

        assert_eq!(book.order_book_imbalance(0), 0.0);
    }

    #[test]
    fn test_all_metrics_together() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        // Setup a realistic order book
        let _ = book.add_limit_order(Id::new(), 99, 20, Side::Buy, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 100, 50, Side::Buy, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 101, 30, Side::Sell, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 102, 40, Side::Sell, TimeInForce::Gtc, None);

        // Verify all metrics work together
        assert_eq!(book.mid_price(), Some(100.5));
        assert_eq!(book.spread_absolute(), Some(1));
        assert!(book.spread_bps(None).is_some());
        assert!(book.vwap(50, Side::Buy).is_some());
        assert!(book.micro_price().is_some());

        // Top 1 level: bid=50, ask=30, imbalance = (50-30)/(50+30) = 20/80 = 0.25
        let imbalance = book.order_book_imbalance(1);
        assert_eq!(imbalance, 0.25);
    }
}
