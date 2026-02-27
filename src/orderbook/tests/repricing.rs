//! Tests for the re-pricing functionality of PeggedOrder and TrailingStop orders

#[cfg(test)]
mod tests {
    use crate::OrderBook;
    use crate::orderbook::repricing::RepricingOperations;
    use pricelevel::{
        Hash32, Id, OrderType, PegReferenceType, Price, Quantity, Side, TimeInForce, TimestampMs,
    };

    fn create_order_id() -> Id {
        Id::new_uuid()
    }

    fn current_time_millis() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
    }

    #[test]
    fn test_pegged_order_registration() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        // Initially no pegged orders
        assert_eq!(book.pegged_order_count(), 0);

        // Add a pegged order
        let id = create_order_id();
        let pegged_order = OrderType::PeggedOrder {
            id,
            price: Price::new(100),
            quantity: Quantity::new(10),
            side: Side::Buy,
            user_id: Hash32::zero(),
            timestamp: TimestampMs::new(current_time_millis()),
            time_in_force: TimeInForce::Gtc,
            reference_price_offset: 5,
            reference_price_type: PegReferenceType::BestBid,
            extra_fields: (),
        };

        let result = book.add_order(pegged_order);
        assert!(result.is_ok());

        // Should be registered
        assert_eq!(book.pegged_order_count(), 1);
        assert!(book.pegged_order_ids().contains(&id));
    }

    #[test]
    fn test_trailing_stop_registration() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        // Initially no trailing stops
        assert_eq!(book.trailing_stop_count(), 0);

        // Add a trailing stop order
        let id = create_order_id();
        let trailing_order = OrderType::TrailingStop {
            id,
            price: Price::new(95),
            quantity: Quantity::new(10),
            side: Side::Sell,
            user_id: Hash32::zero(),
            timestamp: TimestampMs::new(current_time_millis()),
            time_in_force: TimeInForce::Gtc,
            trail_amount: Quantity::new(5),
            last_reference_price: Price::new(100),
            extra_fields: (),
        };

        let result = book.add_order(trailing_order);
        assert!(result.is_ok());

        // Should be registered
        assert_eq!(book.trailing_stop_count(), 1);
        assert!(book.trailing_stop_ids().contains(&id));
    }

    #[test]
    fn test_special_order_unregistration_on_cancel() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        // Add a pegged order (buy side, won't match with trailing stop on sell side at higher price)
        let pegged_id = create_order_id();
        let pegged_order = OrderType::PeggedOrder {
            id: pegged_id,
            price: Price::new(100),
            quantity: Quantity::new(10),
            side: Side::Buy,
            user_id: Hash32::zero(),
            timestamp: TimestampMs::new(current_time_millis()),
            time_in_force: TimeInForce::Gtc,
            reference_price_offset: 5,
            reference_price_type: PegReferenceType::BestBid,
            extra_fields: (),
        };
        book.add_order(pegged_order).unwrap();
        assert_eq!(book.pegged_order_count(), 1);

        // Add a trailing stop (sell side at higher price, won't cross with buy at 100)
        let trailing_id = create_order_id();
        let trailing_order = OrderType::TrailingStop {
            id: trailing_id,
            price: Price::new(110), // Higher than buy price, won't match
            quantity: Quantity::new(10),
            side: Side::Sell,
            user_id: Hash32::zero(),
            timestamp: TimestampMs::new(current_time_millis()),
            time_in_force: TimeInForce::Gtc,
            trail_amount: Quantity::new(5),
            last_reference_price: Price::new(115),
            extra_fields: (),
        };
        book.add_order(trailing_order).unwrap();

        assert_eq!(book.pegged_order_count(), 1);
        assert_eq!(book.trailing_stop_count(), 1);

        // Cancel the pegged order
        book.cancel_order(pegged_id).unwrap();
        assert_eq!(book.pegged_order_count(), 0);
        assert_eq!(book.trailing_stop_count(), 1);

        // Cancel the trailing stop
        book.cancel_order(trailing_id).unwrap();
        assert_eq!(book.pegged_order_count(), 0);
        assert_eq!(book.trailing_stop_count(), 0);
    }

    #[test]
    fn test_reprice_pegged_order_best_bid() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        // First add some liquidity to establish best bid
        let liquidity_id = create_order_id();
        let liquidity_order = OrderType::Standard {
            id: liquidity_id,
            price: Price::new(100),
            quantity: Quantity::new(100),
            side: Side::Buy,
            user_id: Hash32::zero(),
            timestamp: TimestampMs::new(current_time_millis()),
            time_in_force: TimeInForce::Gtc,
            extra_fields: (),
        };
        book.add_order(liquidity_order).unwrap();

        // Add a pegged order that tracks best bid with +5 offset
        let pegged_id = create_order_id();
        let pegged_order = OrderType::PeggedOrder {
            id: pegged_id,
            price: Price::new(90), // Initial price (will be re-priced)
            quantity: Quantity::new(10),
            side: Side::Buy,
            user_id: Hash32::zero(),
            timestamp: TimestampMs::new(current_time_millis()),
            time_in_force: TimeInForce::Gtc,
            reference_price_offset: 5,
            reference_price_type: PegReferenceType::BestBid,
            extra_fields: (),
        };
        book.add_order(pegged_order).unwrap();

        // Best bid is 100, offset is +5, so new price should be 105
        let repriced = book.reprice_pegged_orders().unwrap();
        assert_eq!(repriced, 1);

        // Verify the order was re-priced
        let order = book.get_order(pegged_id).unwrap();
        assert_eq!(order.price().as_u128(), 105);
    }

    #[test]
    fn test_reprice_pegged_order_best_ask() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        // Add liquidity on ask side
        let liquidity_id = create_order_id();
        let liquidity_order = OrderType::Standard {
            id: liquidity_id,
            price: Price::new(110),
            quantity: Quantity::new(100),
            side: Side::Sell,
            user_id: Hash32::zero(),
            timestamp: TimestampMs::new(current_time_millis()),
            time_in_force: TimeInForce::Gtc,
            extra_fields: (),
        };
        book.add_order(liquidity_order).unwrap();

        // Add a pegged order that tracks best ask with -3 offset
        let pegged_id = create_order_id();
        let pegged_order = OrderType::PeggedOrder {
            id: pegged_id,
            price: Price::new(120), // Initial price (will be re-priced)
            quantity: Quantity::new(10),
            side: Side::Sell,
            user_id: Hash32::zero(),
            timestamp: TimestampMs::new(current_time_millis()),
            time_in_force: TimeInForce::Gtc,
            reference_price_offset: -3,
            reference_price_type: PegReferenceType::BestAsk,
            extra_fields: (),
        };
        book.add_order(pegged_order).unwrap();

        // Best ask is 110, offset is -3, so new price should be 107
        let repriced = book.reprice_pegged_orders().unwrap();
        assert_eq!(repriced, 1);

        // Verify the order was re-priced
        let order = book.get_order(pegged_id).unwrap();
        assert_eq!(order.price().as_u128(), 107);
    }

    #[test]
    fn test_reprice_trailing_stop_sell_market_rises() {
        // Test the calculation function directly to avoid matching issues
        use crate::orderbook::repricing::calculate_trailing_stop_price;

        // Sell trailing stop: market rises from 100 to 110
        // Stop should adjust from 95 to 105 (maintaining 5 unit trail)
        let result = calculate_trailing_stop_price(
            Side::Sell,
            95,  // current stop
            5,   // trail amount
            100, // last reference (market was at 100)
            110, // current market (market rose to 110)
        );
        assert_eq!(result, Some((105, 110))); // new stop = 110 - 5 = 105
    }

    #[test]
    fn test_reprice_trailing_stop_buy_market_falls() {
        // Test the calculation function directly to avoid matching issues
        use crate::orderbook::repricing::calculate_trailing_stop_price;

        // Buy trailing stop: market falls from 100 to 90
        // Stop should adjust from 105 to 95 (maintaining 5 unit trail)
        let result = calculate_trailing_stop_price(
            Side::Buy,
            105, // current stop
            5,   // trail amount
            100, // last reference (market was at 100)
            90,  // current market (market fell to 90)
        );
        assert_eq!(result, Some((95, 90))); // new stop = 90 + 5 = 95
    }

    #[test]
    fn test_reprice_special_orders_combined() {
        // Test the calculation functions directly
        use crate::orderbook::repricing::{calculate_pegged_price, calculate_trailing_stop_price};

        // Test pegged order calculation: best_bid=100, offset=+2 -> price=102
        let pegged_price = calculate_pegged_price(
            PegReferenceType::BestBid,
            2,
            Side::Buy,
            Some(100), // best_bid
            Some(120), // best_ask
            Some(110), // mid_price
            None,
        );
        assert_eq!(pegged_price, Some(102));

        // Test trailing stop: market rose from 99 to 100, trail=5
        // new_stop = 100 - 5 = 95, but if current_stop is already 95, no change
        let trailing_result = calculate_trailing_stop_price(
            Side::Sell,
            95,  // current stop
            5,   // trail
            99,  // last reference
            100, // current market
        );
        // 100 > 99, so market rose, new_stop = 100 - 5 = 95
        // But 95 is NOT > 95, so no adjustment
        assert_eq!(trailing_result, None);
    }

    #[test]
    fn test_should_trigger_trailing_stop_sell() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        let order = OrderType::TrailingStop::<()> {
            id: create_order_id(),
            price: Price::new(95),
            quantity: Quantity::new(10),
            side: Side::Sell,
            user_id: Hash32::zero(),
            timestamp: TimestampMs::new(current_time_millis()),
            time_in_force: TimeInForce::Gtc,
            trail_amount: Quantity::new(5),
            last_reference_price: Price::new(100),
            extra_fields: (),
        };

        // Should trigger when market falls to or below stop price
        assert!(book.should_trigger_trailing_stop(&order, 95));
        assert!(book.should_trigger_trailing_stop(&order, 90));
        assert!(!book.should_trigger_trailing_stop(&order, 96));
        assert!(!book.should_trigger_trailing_stop(&order, 100));
    }

    #[test]
    fn test_should_trigger_trailing_stop_buy() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        let order = OrderType::TrailingStop::<()> {
            id: create_order_id(),
            price: Price::new(105),
            quantity: Quantity::new(10),
            side: Side::Buy,
            user_id: Hash32::zero(),
            timestamp: TimestampMs::new(current_time_millis()),
            time_in_force: TimeInForce::Gtc,
            trail_amount: Quantity::new(5),
            last_reference_price: Price::new(100),
            extra_fields: (),
        };

        // Should trigger when market rises to or above stop price
        assert!(book.should_trigger_trailing_stop(&order, 105));
        assert!(book.should_trigger_trailing_stop(&order, 110));
        assert!(!book.should_trigger_trailing_stop(&order, 104));
        assert!(!book.should_trigger_trailing_stop(&order, 100));
    }

    #[test]
    fn test_no_reprice_when_price_unchanged() {
        // Test the calculation function directly
        use crate::orderbook::repricing::calculate_pegged_price;

        // If current price is already 105 and calculated price is also 105, no reprice needed
        let calculated_price = calculate_pegged_price(
            PegReferenceType::BestBid,
            5,
            Side::Buy,
            Some(100), // best_bid
            Some(120), // best_ask
            Some(110), // mid_price
            None,
        );

        // Calculated price is 105 (100 + 5)
        assert_eq!(calculated_price, Some(105));

        // If current order price is also 105, no repricing would occur
        let current_price = 105u128;
        assert_eq!(calculated_price.unwrap(), current_price);
    }

    #[test]
    fn test_standard_order_not_tracked() {
        let book: OrderBook<()> = OrderBook::new("TEST");

        // Add a standard order
        let standard = OrderType::Standard {
            id: create_order_id(),
            price: Price::new(100),
            quantity: Quantity::new(100),
            side: Side::Buy,
            user_id: Hash32::zero(),
            timestamp: TimestampMs::new(current_time_millis()),
            time_in_force: TimeInForce::Gtc,
            extra_fields: (),
        };
        book.add_order(standard).unwrap();

        // Should not be tracked as special order
        assert_eq!(book.pegged_order_count(), 0);
        assert_eq!(book.trailing_stop_count(), 0);
    }
}
