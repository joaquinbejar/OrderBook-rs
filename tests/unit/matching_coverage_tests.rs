//! Additional unit tests to improve test coverage for matching.rs
//! These tests target specific uncovered lines and edge cases

use pricelevel::{Id, Side, TimeInForce};

#[derive(Debug, Clone, Default, PartialEq)]
struct TestExtraFields {
    pub metadata: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use orderbook_rs::{OrderBook, OrderBookError};

    #[test]
    fn test_match_order_empty_opposite_side_market_order() {
        // Test match_order with empty opposite side and market order (lines 39-40, 44-45)
        let book = OrderBook::<TestExtraFields>::new("TEST");

        // Try to match a market buy order when there are no asks
        let result = book.match_order(
            Id::from_u64(1),
            Side::Buy,
            10,
            None, // Market order (no limit price)
        );

        assert!(result.is_err());
        match result.unwrap_err() {
            OrderBookError::InsufficientLiquidity {
                side,
                requested,
                available,
            } => {
                assert_eq!(side, Side::Buy);
                assert_eq!(requested, 10);
                assert_eq!(available, 0);
            }
            _ => panic!("Expected InsufficientLiquidity error"),
        }
    }

    #[test]
    fn test_match_order_empty_opposite_side_limit_order() {
        // Test match_order with empty opposite side and limit order (lines 49)
        let book = OrderBook::<TestExtraFields>::new("TEST");

        // Try to match a limit buy order when there are no asks
        let result = book.match_order(
            Id::from_u64(1),
            Side::Buy,
            10,
            Some(100), // Limit order
        );

        assert!(result.is_ok());
        let match_result = result.unwrap();
        assert_eq!(match_result.remaining_quantity(), 10); // No match occurred
        assert!(!match_result.is_complete());
        assert!(match_result.trades().as_vec().is_empty());
    }

    #[test]
    fn test_match_order_price_limit_buy_exceeds() {
        // Test match_order with buy order exceeding price limit (lines 80)
        let book = OrderBook::<TestExtraFields>::new("TEST");

        // Add a sell order at high price
        let sell_id = Id::from_u64(1);
        let _ = book.add_limit_order(sell_id, 200, 10, Side::Sell, TimeInForce::Gtc, None);

        // Try to match with a buy order with lower limit
        let result = book.match_order(
            Id::from_u64(2),
            Side::Buy,
            5,
            Some(150), // Limit below ask price
        );

        assert!(result.is_ok());
        let match_result = result.unwrap();
        assert_eq!(match_result.remaining_quantity(), 5); // No match due to price limit
        assert!(!match_result.is_complete());
        assert!(match_result.trades().as_vec().is_empty());
    }

    #[test]
    fn test_match_order_price_limit_sell_below() {
        // Test match_order with sell order below price limit (lines 97, 101)
        let book = OrderBook::<TestExtraFields>::new("TEST");

        // Add a buy order at low price
        let buy_id = Id::from_u64(1);
        let _ = book.add_limit_order(buy_id, 50, 10, Side::Buy, TimeInForce::Gtc, None);

        // Try to match with a sell order with higher limit
        let result = book.match_order(
            Id::from_u64(2),
            Side::Sell,
            5,
            Some(100), // Limit above bid price
        );

        assert!(result.is_ok());
        let match_result = result.unwrap();
        assert_eq!(match_result.remaining_quantity(), 5); // No match due to price limit
        assert!(!match_result.is_complete());
        assert!(match_result.trades().as_vec().is_empty());
    }

    #[test]
    fn test_match_order_price_level_removed_by_thread() {
        // Test match_order when price level is removed by another thread (lines 107-108)
        let book = OrderBook::<TestExtraFields>::new("TEST");

        // Add and immediately remove an order to simulate concurrent removal
        let sell_id = Id::from_u64(1);
        let _ = book.add_limit_order(sell_id, 100, 10, Side::Sell, TimeInForce::Gtc, None);
        let _ = book.cancel_order(sell_id); // Remove the order

        // Try to match - the price level should be gone
        let result = book.match_order(Id::from_u64(2), Side::Buy, 5, Some(100));

        assert!(result.is_ok());
        let match_result = result.unwrap();
        assert_eq!(match_result.remaining_quantity(), 5); // No match since level was removed
        assert!(match_result.trades().as_vec().is_empty());
    }

    #[test]
    fn test_match_order_with_transactions() {
        // Test match_order with successful transactions (lines 130, 135)
        let book = OrderBook::<TestExtraFields>::new("TEST");

        // Add a sell order
        let sell_id = Id::from_u64(1);
        let _ = book.add_limit_order(sell_id, 100, 10, Side::Sell, TimeInForce::Gtc, None);

        // Match with a buy order
        let result = book.match_order(Id::from_u64(2), Side::Buy, 5, Some(100));

        assert!(result.is_ok());
        let match_result = result.unwrap();
        assert_eq!(match_result.remaining_quantity(), 0); // Fully matched
        assert!(match_result.is_complete());
        assert!(!match_result.trades().as_vec().is_empty());

        // Verify last trade price was updated
        assert_eq!(book.last_trade_price(), Some(100));
    }

    #[test]
    fn test_match_order_filled_orders_tracking() {
        // Test match_order with filled orders tracking (lines 147-150)
        let book = OrderBook::<TestExtraFields>::new("TEST");

        // Add multiple small sell orders at same price
        let sell_id1 = Id::from_u64(1);
        let sell_id2 = Id::from_u64(2);
        let _ = book.add_limit_order(sell_id1, 100, 3, Side::Sell, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(sell_id2, 100, 3, Side::Sell, TimeInForce::Gtc, None);

        // Match with a large buy order that will fill both
        let result = book.match_order(
            Id::from_u64(3),
            Side::Buy,
            6, // Will fill both sell orders
            Some(100),
        );

        assert!(result.is_ok());
        let match_result = result.unwrap();
        assert_eq!(match_result.remaining_quantity(), 0); // Fully matched
        assert!(match_result.is_complete());

        // Verify filled orders are tracked
        assert!(!match_result.filled_order_ids().is_empty());

        // Verify orders are removed from book
        assert!(book.get_order(sell_id1).is_none());
        assert!(book.get_order(sell_id2).is_none());
    }

    #[test]
    fn test_match_order_empty_price_level_removal() {
        // Test match_order with empty price level removal (lines 155-156, 158)
        let book = OrderBook::<TestExtraFields>::new("TEST");

        // Add a single sell order
        let sell_id = Id::from_u64(1);
        let _ = book.add_limit_order(sell_id, 100, 10, Side::Sell, TimeInForce::Gtc, None);

        // Match with a buy order that will completely fill the sell order
        let result = book.match_order(
            Id::from_u64(2),
            Side::Buy,
            10, // Exactly matches the sell order
            Some(100),
        );

        assert!(result.is_ok());
        let match_result = result.unwrap();
        assert_eq!(match_result.remaining_quantity(), 0);
        assert!(match_result.is_complete());

        // Verify the price level is completely removed
        assert!(book.get_orders_at_price(100, Side::Sell).is_empty());
    }

    #[test]
    fn test_match_order_early_exit_full_match() {
        // Test match_order early exit when fully matched (lines 172)
        let book = OrderBook::<TestExtraFields>::new("TEST");

        // Add multiple sell orders at different prices
        let sell_id1 = Id::from_u64(1);
        let sell_id2 = Id::from_u64(2);
        let _ = book.add_limit_order(sell_id1, 100, 5, Side::Sell, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(sell_id2, 110, 10, Side::Sell, TimeInForce::Gtc, None);

        // Match with a buy order that will be fully satisfied by first price level
        let result = book.match_order(
            Id::from_u64(3),
            Side::Buy,
            5,         // Exactly matches first sell order
            Some(120), // High enough limit to match both levels
        );

        assert!(result.is_ok());
        let match_result = result.unwrap();
        assert_eq!(match_result.remaining_quantity(), 0);
        assert!(match_result.is_complete());

        // Verify second price level was not touched (early exit)
        assert!(!book.get_orders_at_price(110, Side::Sell).is_empty());
    }

    #[test]
    fn test_match_order_batch_removal_operations() {
        // Test match_order batch removal operations (lines 175-176)
        let book = OrderBook::<TestExtraFields>::new("TEST");

        // Add multiple sell orders that will all be filled
        let sell_id1 = Id::from_u64(1);
        let sell_id2 = Id::from_u64(2);
        let sell_id3 = Id::from_u64(3);
        let _ = book.add_limit_order(sell_id1, 100, 2, Side::Sell, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(sell_id2, 101, 3, Side::Sell, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(sell_id3, 102, 4, Side::Sell, TimeInForce::Gtc, None);

        // Match with a large buy order
        let result = book.match_order(
            Id::from_u64(4),
            Side::Buy,
            9, // Will fill all three orders
            Some(105),
        );

        assert!(result.is_ok());
        let match_result = result.unwrap();
        assert_eq!(match_result.remaining_quantity(), 0);

        // Verify all price levels are removed (batch removal)
        assert!(book.get_orders_at_price(100, Side::Sell).is_empty());
        assert!(book.get_orders_at_price(101, Side::Sell).is_empty());
        assert!(book.get_orders_at_price(102, Side::Sell).is_empty());

        // Verify all orders are removed from tracking (batch removal)
        assert!(book.get_order(sell_id1).is_none());
        assert!(book.get_order(sell_id2).is_none());
        assert!(book.get_order(sell_id3).is_none());
    }

    #[test]
    fn test_match_order_market_order_insufficient_liquidity() {
        // Test match_order market order with insufficient liquidity (lines 194)
        let book = OrderBook::<TestExtraFields>::new("TEST");

        // Add limited liquidity
        let sell_id = Id::from_u64(1);
        let _ = book.add_limit_order(sell_id, 100, 5, Side::Sell, TimeInForce::Gtc, None);

        // Try to match a market order for more than available
        let result = book.match_order(
            Id::from_u64(2),
            Side::Buy,
            10,   // More than available (5)
            None, // Market order
        );

        // Market orders should partially fill and return remaining quantity
        assert!(result.is_ok());
        let match_result = result.unwrap();
        assert_eq!(match_result.remaining_quantity(), 5); // 10 requested - 5 available = 5 remaining
        assert!(!match_result.is_complete());

        // Verify the available order was consumed
        assert!(book.get_order(sell_id).is_none());
    }

    #[test]
    fn test_peek_match_empty_price_levels() {
        // Test peek_match with empty price levels (lines 208-211)
        let book = OrderBook::<TestExtraFields>::new("TEST");

        // Test peek match when there are no orders
        let matched_quantity = book.peek_match(Side::Buy, 10, Some(100));
        assert_eq!(matched_quantity, 0);

        let matched_quantity = book.peek_match(Side::Sell, 10, None);
        assert_eq!(matched_quantity, 0);
    }

    #[test]
    fn test_peek_match_early_termination() {
        // Test peek_match early termination when enough quantity is found (lines 218)
        let book = OrderBook::<TestExtraFields>::new("TEST");

        // Add multiple sell orders
        let sell_id1 = Id::from_u64(1);
        let sell_id2 = Id::from_u64(2);
        let _ = book.add_limit_order(sell_id1, 100, 10, Side::Sell, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(sell_id2, 110, 20, Side::Sell, TimeInForce::Gtc, None);

        // Peek match for less than first level
        let matched_quantity = book.peek_match(Side::Buy, 5, Some(120));
        assert_eq!(matched_quantity, 5); // Should match from first level only

        // Peek match for exactly first level
        let matched_quantity = book.peek_match(Side::Buy, 10, Some(120));
        assert_eq!(matched_quantity, 10); // Should match exactly first level
    }

    #[test]
    fn test_peek_match_price_limit_buy_continue() {
        // Test peek_match with buy price limit continue (lines 222)
        let book = OrderBook::<TestExtraFields>::new("TEST");

        // Add sell orders at different prices
        let sell_id1 = Id::from_u64(1);
        let sell_id2 = Id::from_u64(2);
        let _ = book.add_limit_order(sell_id1, 150, 5, Side::Sell, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(sell_id2, 100, 10, Side::Sell, TimeInForce::Gtc, None);

        // Peek match with price limit that excludes higher price
        let matched_quantity = book.peek_match(Side::Buy, 15, Some(120));
        assert_eq!(matched_quantity, 10); // Should only match the 100-price level
    }

    #[test]
    fn test_peek_match_price_limit_sell_continue() {
        // Test peek_match with sell price limit continue (lines 226)
        let book = OrderBook::<TestExtraFields>::new("TEST");

        // Add buy orders at different prices
        let buy_id1 = Id::from_u64(1);
        let buy_id2 = Id::from_u64(2);
        let _ = book.add_limit_order(buy_id1, 50, 5, Side::Buy, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(buy_id2, 100, 10, Side::Buy, TimeInForce::Gtc, None);

        // Peek match with price limit that excludes lower price
        let matched_quantity = book.peek_match(Side::Sell, 15, Some(80));
        assert_eq!(matched_quantity, 10); // Should only match the 100-price level
    }

    #[test]
    fn test_peek_match_quantity_calculation() {
        // Test peek_match quantity calculation logic (lines 228-230, 233)
        let book = OrderBook::<TestExtraFields>::new("TEST");

        // Add sell orders with specific quantities
        let sell_id1 = Id::from_u64(1);
        let sell_id2 = Id::from_u64(2);
        let sell_id3 = Id::from_u64(3);
        let _ = book.add_limit_order(sell_id1, 100, 3, Side::Sell, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(sell_id2, 101, 7, Side::Sell, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(sell_id3, 102, 15, Side::Sell, TimeInForce::Gtc, None);

        // Test various peek quantities
        let matched_quantity = book.peek_match(Side::Buy, 2, Some(105));
        assert_eq!(matched_quantity, 2); // Partial from first level

        let matched_quantity = book.peek_match(Side::Buy, 5, Some(105));
        assert_eq!(matched_quantity, 5); // Full first level + partial second

        let matched_quantity = book.peek_match(Side::Buy, 12, Some(105));
        assert_eq!(matched_quantity, 12); // First two levels + partial third

        let matched_quantity = book.peek_match(Side::Buy, 30, Some(105));
        assert_eq!(matched_quantity, 25); // All available (3+7+15)
    }
}
