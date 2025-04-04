#[cfg(test)]
mod tests {
    use crate::{OrderBook, OrderBookError};
    use pricelevel::{OrderId, Side, TimeInForce};
    use uuid::Uuid;

    // Helper function to create a random OrderId
    fn new_order_id() -> OrderId {
        OrderId(Uuid::new_v4())
    }

    // Helper function to create an order book for testing
    fn create_test_order_book() -> OrderBook {
        OrderBook::new("TEST-SYMBOL")
    }

    #[test]
    fn test_add_limit_order() {
        let order_book = create_test_order_book();
        let id = new_order_id();
        let price = 1000;
        let quantity = 10;
        let side = Side::Buy;
        let time_in_force = TimeInForce::Gtc;

        let result = order_book.add_limit_order(id, price, quantity, side, time_in_force);
        assert!(result.is_ok(), "Adding a limit order should succeed");

        let order = result.unwrap();
        assert_eq!(order.id(), id, "Order ID should match");
        assert_eq!(order.price(), price, "Price should match");
        assert_eq!(order.visible_quantity(), quantity, "Quantity should match");
        assert_eq!(order.side(), side, "Side should match");
        assert_eq!(
            order.time_in_force(),
            time_in_force,
            "Time in force should match"
        );

        // Verify the order is in the book
        let book_order = order_book.get_order(id);
        assert!(book_order.is_some(), "Order should be in the book");
    }

    #[test]
    fn test_add_iceberg_order() {
        let order_book = create_test_order_book();
        let id = new_order_id();
        let price = 1000;
        let visible_quantity = 10;
        let hidden_quantity = 90;
        let side = Side::Sell;
        let time_in_force = TimeInForce::Gtc;

        let result = order_book.add_iceberg_order(
            id,
            price,
            visible_quantity,
            hidden_quantity,
            side,
            time_in_force,
        );
        assert!(result.is_ok(), "Adding an iceberg order should succeed");

        let order = result.unwrap();
        assert_eq!(order.id(), id, "Order ID should match");
        assert_eq!(order.price(), price, "Price should match");
        assert_eq!(
            order.visible_quantity(),
            visible_quantity,
            "Visible quantity should match"
        );
        assert_eq!(
            order.hidden_quantity(),
            hidden_quantity,
            "Hidden quantity should match"
        );
        assert_eq!(order.side(), side, "Side should match");
        assert_eq!(
            order.time_in_force(),
            time_in_force,
            "Time in force should match"
        );

        // Verify the order is in the book
        let book_order = order_book.get_order(id);
        assert!(book_order.is_some(), "Order should be in the book");
    }

    #[test]
    fn test_add_post_only_order() {
        let order_book = create_test_order_book();
        let id = new_order_id();
        let price = 1000;
        let quantity = 10;
        let side = Side::Buy;
        let time_in_force = TimeInForce::Gtc;

        let result = order_book.add_post_only_order(id, price, quantity, side, time_in_force);
        assert!(result.is_ok(), "Adding a post-only order should succeed");

        let order = result.unwrap();
        assert_eq!(order.id(), id, "Order ID should match");
        assert_eq!(order.price(), price, "Price should match");
        assert_eq!(order.visible_quantity(), quantity, "Quantity should match");
        assert_eq!(order.side(), side, "Side should match");
        assert_eq!(
            order.time_in_force(),
            time_in_force,
            "Time in force should match"
        );
        assert!(order.is_post_only(), "Order should be post-only");

        // Verify the order is in the book
        let book_order = order_book.get_order(id);
        assert!(book_order.is_some(), "Order should be in the book");
    }

    #[test]
    fn test_post_only_order_price_crossing() {
        let order_book = create_test_order_book();

        // First add a sell order at price 1000
        let sell_id = new_order_id();
        let sell_result =
            order_book.add_limit_order(sell_id, 1000, 10, Side::Sell, TimeInForce::Gtc);
        assert!(sell_result.is_ok(), "Adding sell order should succeed");

        // Now try to add a post-only buy order at price 1000 (would cross)
        let buy_id = new_order_id();
        let buy_result =
            order_book.add_post_only_order(buy_id, 1000, 10, Side::Buy, TimeInForce::Gtc);

        assert!(
            buy_result.is_err(),
            "Post-only order that would cross should fail"
        );
        match buy_result {
            Err(OrderBookError::PriceCrossing {
                price,
                side,
                opposite_price,
            }) => {
                assert_eq!(price, 1000, "Price should match");
                assert_eq!(side, Side::Buy, "Side should be buy");
                assert_eq!(opposite_price, 1000, "Opposite price should match");
            }
            _ => panic!("Expected PriceCrossing error"),
        }
    }

    #[test]
    fn test_submit_market_order() {
        let order_book = create_test_order_book();

        // First add a limit sell order
        let sell_id = new_order_id();
        let sell_result =
            order_book.add_limit_order(sell_id, 1000, 10, Side::Sell, TimeInForce::Gtc);
        assert!(sell_result.is_ok(), "Adding sell order should succeed");

        // Now submit a market buy order
        let buy_id = new_order_id();
        let market_result = order_book.submit_market_order(
            buy_id,
            5, // Half of the available quantity
            Side::Buy,
        );

        assert!(market_result.is_ok(), "Market order should succeed");
        let match_result = market_result.unwrap();

        // Check match result
        assert_eq!(match_result.order_id, buy_id, "Order ID should match");
        assert_eq!(
            match_result.executed_quantity(),
            5,
            "Should execute requested quantity"
        );
        assert_eq!(match_result.remaining_quantity, 0, "No remaining quantity");
        assert!(match_result.is_complete, "Order should be complete");
        assert_eq!(
            match_result.transactions.len(),
            1,
            "Should have one transaction"
        );

        // Check transaction details
        let transaction = &match_result.transactions.as_vec()[0];
        assert_eq!(
            transaction.taker_order_id, buy_id,
            "Taker should be market order"
        );
        assert_eq!(
            transaction.maker_order_id, sell_id,
            "Maker should be limit order"
        );
        assert_eq!(
            transaction.price, 1000,
            "Price should match limit order price"
        );
        assert_eq!(
            transaction.quantity, 5,
            "Quantity should match market order size"
        );
        assert_eq!(
            transaction.taker_side,
            Side::Buy,
            "Taker side should be buy"
        );

        // Verify the sell order is still in the book with reduced quantity
        let updated_sell = order_book.get_order(sell_id);
        assert!(
            updated_sell.is_some(),
            "Sell order should still be in the book"
        );
        assert_eq!(
            updated_sell.unwrap().visible_quantity(),
            5,
            "Sell order should have reduced quantity"
        );
    }

    #[test]
    fn test_submit_market_order_full_fill() {
        let order_book = create_test_order_book();

        // First add a limit sell order
        let sell_id = new_order_id();
        let sell_result =
            order_book.add_limit_order(sell_id, 1000, 10, Side::Sell, TimeInForce::Gtc);
        assert!(sell_result.is_ok(), "Adding sell order should succeed");

        // Now submit a market buy order for the full amount
        let buy_id = new_order_id();
        let market_result = order_book.submit_market_order(buy_id, 10, Side::Buy);

        assert!(market_result.is_ok(), "Market order should succeed");
        let match_result = market_result.unwrap();

        assert_eq!(
            match_result.executed_quantity(),
            10,
            "Should execute full quantity"
        );
        assert!(
            match_result.filled_order_ids.contains(&sell_id),
            "Sell order should be marked as filled"
        );

        // Verify the sell order is no longer in the book
        let updated_sell = order_book.get_order(sell_id);
        assert!(
            updated_sell.is_none(),
            "Sell order should be removed from the book"
        );
    }

    #[test]
    fn test_submit_market_order_insufficient_liquidity() {
        let order_book = create_test_order_book();

        // First add a limit sell order
        let sell_id = new_order_id();
        let sell_result =
            order_book.add_limit_order(sell_id, 1000, 10, Side::Sell, TimeInForce::Gtc);
        assert!(sell_result.is_ok(), "Adding sell order should succeed");

        // Now submit a market buy order for more than available
        let buy_id = new_order_id();
        let market_result = order_book.submit_market_order(buy_id, 20, Side::Buy);

        // The market order should execute partially
        assert!(
            market_result.is_ok(),
            "Market order should succeed with partial fill"
        );
        let match_result = market_result.unwrap();

        assert_eq!(
            match_result.executed_quantity(),
            10,
            "Should execute available quantity"
        );
        assert_eq!(
            match_result.remaining_quantity, 10,
            "Should have remaining quantity"
        );
        assert!(!match_result.is_complete, "Order should not be complete");
    }

    #[test]
    fn test_market_order_no_liquidity() {
        let order_book = create_test_order_book();

        // Submit a market buy order with no matching orders
        let buy_id = new_order_id();
        let market_result = order_book.submit_market_order(buy_id, 10, Side::Buy);

        assert!(
            market_result.is_err(),
            "Market order with no liquidity should fail"
        );
        match market_result {
            Err(OrderBookError::InsufficientLiquidity {
                side,
                requested,
                available,
            }) => {
                assert_eq!(side, Side::Buy, "Side should be buy");
                assert_eq!(requested, 10, "Requested quantity should match");
                assert_eq!(available, 0, "Available should be zero");
            }
            _ => panic!("Expected InsufficientLiquidity error"),
        }
    }

    #[test]
    fn test_limit_order_immediate_or_cancel() {
        let order_book = create_test_order_book();

        // Add a sell order
        let sell_id = new_order_id();
        let sell_result =
            order_book.add_limit_order(sell_id, 1000, 10, Side::Sell, TimeInForce::Gtc);
        assert!(sell_result.is_ok(), "Adding sell order should succeed");

        // Add a buy IOC order
        let buy_id = new_order_id();
        let buy_result = order_book.add_limit_order(
            buy_id,
            1000, // Same price so will match
            5,    // Half of the available quantity
            Side::Buy,
            TimeInForce::Ioc,
        );

        assert!(buy_result.is_ok(), "Adding IOC buy order should succeed");

        // IOC order should not be in the book
        let buy_order = order_book.get_order(buy_id);
        assert!(
            buy_order.is_none(),
            "IOC order should not be in the book after execution"
        );

        // Sell order should be partially filled
        let sell_order = order_book.get_order(sell_id);
        assert!(
            sell_order.is_some(),
            "Sell order should still be in the book"
        );
        assert_eq!(
            sell_order.unwrap().visible_quantity(),
            5,
            "Sell order should have reduced quantity"
        );
    }

    #[test]
    fn test_limit_order_fill_or_kill_success() {
        let order_book = create_test_order_book();

        // Add a sell order
        let sell_id = new_order_id();
        let sell_result =
            order_book.add_limit_order(sell_id, 1000, 10, Side::Sell, TimeInForce::Gtc);
        assert!(sell_result.is_ok(), "Adding sell order should succeed");

        // Add a buy FOK order that can be fully filled
        let buy_id = new_order_id();
        let buy_result = order_book.add_limit_order(
            buy_id,
            1000,
            10, // Full quantity available
            Side::Buy,
            TimeInForce::Fok,
        );

        assert!(
            buy_result.is_ok(),
            "Adding FOK buy order should succeed when fully fillable"
        );

        // FOK order should not be in the book after execution
        let buy_order = order_book.get_order(buy_id);
        assert!(
            buy_order.is_none(),
            "FOK order should not be in the book after execution"
        );

        // Sell order should be fully filled and removed
        let sell_order = order_book.get_order(sell_id);
        assert!(
            sell_order.is_none(),
            "Sell order should be removed from the book"
        );
    }

    #[test]
    fn test_limit_order_fill_or_kill_failure() {
        let order_book = create_test_order_book();

        // Add a sell order
        let sell_id = new_order_id();
        let sell_result = order_book.add_limit_order(
            sell_id,
            1000,
            5, // Only 5 units available
            Side::Sell,
            TimeInForce::Gtc,
        );
        assert!(sell_result.is_ok(), "Adding sell order should succeed");

        // Add a buy FOK order that cannot be fully filled
        let buy_id = new_order_id();
        let buy_result = order_book.add_limit_order(
            buy_id,
            1000,
            10, // Requires more than available
            Side::Buy,
            TimeInForce::Fok,
        );

        assert!(
            buy_result.is_err(),
            "Adding FOK buy order should fail when not fully fillable"
        );
        match buy_result {
            Err(OrderBookError::InsufficientLiquidity {
                side,
                requested,
                available,
            }) => {
                assert_eq!(side, Side::Buy, "Side should be buy");
                assert_eq!(requested, 10, "Requested quantity should match");
                assert_eq!(available, 5, "Available quantity should match");
            }
            _ => panic!("Expected InsufficientLiquidity error"),
        }

        // Sell order should remain unchanged
        let sell_order = order_book.get_order(sell_id);
        assert!(
            sell_order.is_some(),
            "Sell order should still be in the book"
        );
        assert_eq!(
            sell_order.unwrap().visible_quantity(),
            5,
            "Sell order quantity should be unchanged"
        );
    }
}

#[cfg(test)]
mod test_operations_remaining {
    use crate::OrderBook;
    use pricelevel::{OrderId, Side, TimeInForce};
    use uuid::Uuid;

    fn create_order_id() -> OrderId {
        OrderId(Uuid::new_v4())
    }

    #[test]
    fn test_add_limit_order_with_trace() {
        let book = OrderBook::new("TEST");

        // Add a limit order with detailed tracing
        let id = create_order_id();
        let price = 1000;
        let quantity = 10;
        let side = Side::Buy;
        let time_in_force = TimeInForce::Gtc;

        let result = book.add_limit_order(id, price, quantity, side, time_in_force);
        assert!(result.is_ok());

        // Verify order was added correctly
        let order = result.unwrap();
        assert_eq!(order.id(), id);
        assert_eq!(order.price(), price);
        assert_eq!(order.visible_quantity(), quantity);
    }

    #[test]
    fn test_add_iceberg_order_with_trace() {
        let book = OrderBook::new("TEST");

        // Add an iceberg order with detailed tracing
        let id = create_order_id();
        let price = 1000;
        let visible_quantity = 10;
        let hidden_quantity = 90;
        let side = Side::Sell;
        let time_in_force = TimeInForce::Gtc;

        let result = book.add_iceberg_order(
            id,
            price,
            visible_quantity,
            hidden_quantity,
            side,
            time_in_force,
        );
        assert!(result.is_ok());

        // Verify order was added correctly
        let order = result.unwrap();
        assert_eq!(order.id(), id);
        assert_eq!(order.price(), price);
        assert_eq!(order.visible_quantity(), visible_quantity);
        assert_eq!(order.hidden_quantity(), hidden_quantity);
    }

    #[test]
    fn test_add_post_only_order_with_trace() {
        let book = OrderBook::new("TEST");

        // Add a post-only order with detailed tracing
        let id = create_order_id();
        let price = 1000;
        let quantity = 10;
        let side = Side::Buy;
        let time_in_force = TimeInForce::Gtc;

        let result = book.add_post_only_order(id, price, quantity, side, time_in_force);
        assert!(result.is_ok());

        // Verify order was added correctly
        let order = result.unwrap();
        assert_eq!(order.id(), id);
        assert_eq!(order.price(), price);
        assert_eq!(order.visible_quantity(), quantity);
        assert!(order.is_post_only());
    }
}

#[cfg(test)]
mod test_operations_specific {
    use crate::OrderBook;
    use pricelevel::{OrderId, Side, TimeInForce};
    use uuid::Uuid;

    use tracing::trace;

    fn create_order_id() -> OrderId {
        OrderId(Uuid::new_v4())
    }

    #[test]
    fn test_submit_market_order_with_tracing() {
        let book = OrderBook::new("TEST");

        // Add a sell order
        let sell_id = create_order_id();
        let _ = book.add_limit_order(sell_id, 1000, 10, Side::Sell, TimeInForce::Gtc);

        // Submit a market buy order with tracing enabled
        let buy_id = create_order_id();

        // Use trace macro to generate some trace output
        trace!(
            "About to submit market order {} for {} at side {:?}",
            buy_id,
            5,
            Side::Buy
        );

        let result = book.submit_market_order(buy_id, 5, Side::Buy);
        assert!(result.is_ok());

        // Verify order was matched correctly
        let remaining_sell = book.get_order(sell_id);
        assert!(remaining_sell.is_some());
        assert_eq!(remaining_sell.unwrap().visible_quantity(), 5);
    }
}
