use crate::orderbook::book::OrderBook;
use crate::orderbook::book_change_event::PriceLevelChangedEvent;
use crate::orderbook::error::OrderBookError;
use crate::orderbook::trade::TradeResult;
use pricelevel::{OrderId, OrderType, OrderUpdate, PriceLevel, Side};
use std::sync::Arc;
use tracing::trace;

/// A trait to abstract quantity access and modification for different order types.
pub trait OrderQuantity<T = ()> {
    /// Returns the primary quantity used for display or simple matching.
    /// For iceberg orders, this is the visible quantity.
    fn quantity(&self) -> u64;

    /// Returns the total quantity of the order (e.g., visible + hidden).
    fn total_quantity(&self) -> u64;

    /// Sets the new quantity for an order, handling the logic for different types.
    /// For iceberg orders, it adjusts the visible and hidden parts correctly.
    fn set_quantity(&mut self, new_total_quantity: u64);
}

impl<T> OrderQuantity<T> for OrderType<T> {
    fn quantity(&self) -> u64 {
        match self {
            OrderType::Standard { quantity, .. } => *quantity,
            OrderType::IcebergOrder {
                visible_quantity, ..
            } => *visible_quantity,
            OrderType::PostOnly { quantity, .. } => *quantity,
            OrderType::TrailingStop { quantity, .. } => *quantity,
            OrderType::PeggedOrder { quantity, .. } => *quantity,
            OrderType::MarketToLimit { quantity, .. } => *quantity,
            OrderType::ReserveOrder {
                visible_quantity, ..
            } => *visible_quantity,
        }
    }

    fn total_quantity(&self) -> u64 {
        match self {
            OrderType::Standard { quantity, .. } => *quantity,
            OrderType::IcebergOrder {
                visible_quantity,
                hidden_quantity,
                ..
            } => *visible_quantity + *hidden_quantity,
            OrderType::PostOnly { quantity, .. } => *quantity,
            OrderType::TrailingStop { quantity, .. } => *quantity,
            OrderType::PeggedOrder { quantity, .. } => *quantity,
            OrderType::MarketToLimit { quantity, .. } => *quantity,
            OrderType::ReserveOrder {
                visible_quantity,
                hidden_quantity,
                ..
            } => *visible_quantity + *hidden_quantity,
        }
    }

    fn set_quantity(&mut self, new_total_quantity: u64) {
        match self {
            OrderType::Standard { quantity, .. }
            | OrderType::PostOnly { quantity, .. }
            | OrderType::TrailingStop { quantity, .. }
            | OrderType::PeggedOrder { quantity, .. }
            | OrderType::MarketToLimit { quantity, .. } => *quantity = new_total_quantity,

            OrderType::IcebergOrder {
                visible_quantity, ..
            } => {
                // For iceberg orders, treat new_total_quantity as the new visible quantity
                // This matches the expected behavior where quantity() returns visible_quantity
                *visible_quantity = new_total_quantity;
                // Hidden quantity remains unchanged
            }
            OrderType::ReserveOrder {
                visible_quantity,
                hidden_quantity,
                replenish_amount,
                ..
            } => {
                let original_total = *visible_quantity + *hidden_quantity;
                let amount_to_reduce = original_total.saturating_sub(new_total_quantity);

                let filled_from_visible = amount_to_reduce.min(*visible_quantity);
                *visible_quantity -= filled_from_visible;

                let remaining_to_reduce = amount_to_reduce - filled_from_visible;
                *hidden_quantity = hidden_quantity.saturating_sub(remaining_to_reduce);

                if *visible_quantity == 0 && *hidden_quantity > 0 {
                    let refresh = replenish_amount.unwrap_or(0).min(*hidden_quantity);
                    *visible_quantity = refresh;
                    *hidden_quantity -= refresh;
                }
            }
        }
    }
}

impl<T> OrderBook<T>
where
    T: Clone + Send + Sync + Default + 'static,
{
    /// Update an order's price and/or quantity
    pub fn update_order(
        &self,
        update: OrderUpdate,
    ) -> Result<Option<Arc<OrderType<T>>>, OrderBookError> {
        self.cache.invalidate();
        trace!("Order book {}: Updating order {:?}", self.symbol, update);
        match update {
            OrderUpdate::UpdatePrice {
                order_id,
                new_price,
            } => {
                // Get the order location without locking
                let location = self.order_locations.get(&order_id).map(|val| *val);

                if let Some((old_price, _)) = location {
                    // If price doesn't change, do nothing
                    if old_price == new_price {
                        return Err(OrderBookError::InvalidOperation {
                            message: "Cannot update price to the same value".to_string(),
                        });
                    }

                    // Get the original order without holding locks
                    let original_order = if let Some(order) = self.get_order(order_id) {
                        // Create a copy of the order
                        Arc::try_unwrap(order.clone()).unwrap_or_else(|arc| (*arc).clone())
                    } else {
                        return Ok(None); // Order not found
                    };

                    // Cancel the original order
                    self.cancel_order(order_id)?;

                    // Create a new order with the updated price
                    let mut new_order = original_order;

                    // Update the price based on order type
                    match &mut new_order {
                        OrderType::Standard { price, .. } => *price = new_price,
                        OrderType::IcebergOrder { price, .. } => *price = new_price,
                        OrderType::PostOnly { price, .. } => *price = new_price,
                        OrderType::TrailingStop { price, .. } => *price = new_price,
                        OrderType::PeggedOrder { price, .. } => *price = new_price,
                        OrderType::MarketToLimit { price, .. } => *price = new_price,
                        OrderType::ReserveOrder { price, .. } => *price = new_price,
                    }

                    // Add the updated order
                    let result = self.add_order(new_order)?;
                    Ok(Some(result))
                } else {
                    Ok(None) // Order not found
                }
            }

            OrderUpdate::UpdateQuantity {
                order_id,
                new_quantity,
            } => {
                // Get order location without locking
                let location = self.order_locations.get(&order_id).map(|val| *val);

                if let Some((price, side)) = location {
                    // Get the appropriate price levels map
                    let price_levels = match side {
                        Side::Buy => &self.bids,
                        Side::Sell => &self.asks,
                    };

                    // Attempt to update the order within the price level
                    let mut result = None;
                    let mut is_empty = false;

                    // Get the price level and update it
                    if let Some(entry) = price_levels.get(&price) {
                        let price_level = entry.value();
                        let update = OrderUpdate::UpdateQuantity {
                            order_id,
                            new_quantity,
                        };

                        if let Ok(updated_order) = price_level.update_order(update)
                            && let Some(order) = updated_order
                        {
                            // notify price level changes
                            if let Some(ref listener) = self.price_level_changed_listener {
                                listener(PriceLevelChangedEvent {
                                    side,
                                    price: price_level.price(),
                                    quantity: price_level.visible_quantity(),
                                })
                            }
                            result = Some(Arc::new(self.convert_from_unit_type(&order)));
                        }

                        is_empty = price_level.order_count() == 0;
                    }

                    // If the price level is now empty, remove it
                    if is_empty {
                        price_levels.remove(&price);
                        self.order_locations.remove(&order_id);
                    }

                    self.cache.invalidate();
                    Ok(result)
                } else {
                    Ok(None) // Order not found
                }
            }

            OrderUpdate::UpdatePriceAndQuantity {
                order_id,
                new_price,
                new_quantity,
            } => {
                // Get order location without locking
                let location = self.order_locations.get(&order_id).map(|val| *val);

                if let Some((_, _)) = location {
                    // Get the original order without holding locks
                    let original_order = if let Some(order) = self.get_order(order_id) {
                        // Create a copy of the order
                        Arc::try_unwrap(order.clone()).unwrap_or_else(|arc| (*arc).clone())
                    } else {
                        return Ok(None); // Order not found
                    };

                    // Cancel the original order
                    self.cancel_order(order_id)?;

                    // Create a new order with the updated price and quantity
                    let mut new_order = original_order;

                    // Update the price based on order type
                    match &mut new_order {
                        OrderType::Standard { price, .. } => *price = new_price,
                        OrderType::IcebergOrder { price, .. } => *price = new_price,
                        OrderType::PostOnly { price, .. } => *price = new_price,
                        OrderType::TrailingStop { price, .. } => *price = new_price,
                        OrderType::PeggedOrder { price, .. } => *price = new_price,
                        OrderType::MarketToLimit { price, .. } => *price = new_price,
                        OrderType::ReserveOrder { price, .. } => *price = new_price,
                    }

                    // Update the quantity using the trait method
                    new_order.set_quantity(new_quantity);

                    // Add the updated order
                    let result = self.add_order(new_order)?;
                    Ok(Some(result))
                } else {
                    Ok(None) // Order not found
                }
            }

            OrderUpdate::Cancel { order_id } => {
                // Get order location without locking
                let location = self.order_locations.get(&order_id).map(|val| *val);

                if let Some((price, side)) = location {
                    // Get the appropriate price levels map
                    let price_levels = match side {
                        Side::Buy => &self.bids,
                        Side::Sell => &self.asks,
                    };

                    // Attempt to cancel the order
                    let mut result = None;
                    let mut is_empty = false;

                    // Get the current order first
                    if let Some(current_order) = self.get_order(order_id) {
                        result = Some(current_order);

                        // Remove the order directly from the price level
                        if let Some(entry) = price_levels.get(&price) {
                            let price_level = entry.value();
                            let cancel_update = OrderUpdate::Cancel { order_id };
                            let result = price_level.update_order(cancel_update);
                            // notify price level changes
                            if let Some(ref listener) = self.price_level_changed_listener
                                && let Ok(updated_order) = result
                                && updated_order.is_some()
                            {
                                listener(PriceLevelChangedEvent {
                                    side,
                                    price: price_level.price(),
                                    quantity: price_level.visible_quantity(),
                                })
                            }
                            is_empty = price_level.order_count() == 0;
                        }

                        // Remove from order locations tracking
                        self.order_locations.remove(&order_id);
                    }

                    // If price level is empty, remove it
                    if is_empty {
                        price_levels.remove(&price);
                    }

                    Ok(result)
                } else {
                    Ok(None) // Order not found
                }
            }

            OrderUpdate::Replace {
                order_id,
                price,
                quantity,
                side,
            } => {
                // Get the original order without holding locks
                let original_opt = self.get_order(order_id);

                if let Some(original) = original_opt {
                    // Create a new order by cloning and updating the original
                    let mut new_order = (*original).clone();

                    // Update the order fields based on order type
                    match &mut new_order {
                        OrderType::Standard {
                            id,
                            price: p,
                            quantity: q,
                            side: s,
                            ..
                        } => {
                            *id = order_id;
                            *p = price;
                            *q = quantity;
                            *s = side;
                        }
                        OrderType::IcebergOrder {
                            id,
                            price: p,
                            visible_quantity,
                            side: s,
                            ..
                        } => {
                            *id = order_id;
                            *p = price;
                            *visible_quantity = quantity;
                            *s = side;
                        }
                        OrderType::PostOnly {
                            id,
                            price: p,
                            quantity: q,
                            side: s,
                            ..
                        } => {
                            *id = order_id;
                            *p = price;
                            *q = quantity;
                            *s = side;
                        }
                        OrderType::TrailingStop {
                            id,
                            price: p,
                            quantity: q,
                            side: s,
                            ..
                        } => {
                            *id = order_id;
                            *p = price;
                            *q = quantity;
                            *s = side;
                        }
                        OrderType::PeggedOrder {
                            id,
                            price: p,
                            quantity: q,
                            side: s,
                            ..
                        } => {
                            *id = order_id;
                            *p = price;
                            *q = quantity;
                            *s = side;
                        }
                        OrderType::MarketToLimit {
                            id,
                            price: p,
                            quantity: q,
                            side: s,
                            ..
                        } => {
                            *id = order_id;
                            *p = price;
                            *q = quantity;
                            *s = side;
                        }
                        OrderType::ReserveOrder {
                            id,
                            price: p,
                            visible_quantity,
                            side: s,
                            ..
                        } => {
                            *id = order_id;
                            *p = price;
                            *visible_quantity = quantity;
                            *s = side;
                        }
                    }

                    // Cancel the original order
                    self.cancel_order(order_id)?;

                    // Add the new order
                    let result = self.add_order(new_order)?;
                    Ok(Some(result))
                } else {
                    Ok(None) // Original order not found
                }
            }
        }
    }

    /// Cancel an order by ID
    pub fn cancel_order(
        &self,
        order_id: OrderId,
    ) -> Result<Option<Arc<OrderType<T>>>, OrderBookError> {
        self.cache.invalidate();
        // First, we find the order's location (price and side) without locking
        let location = self.order_locations.get(&order_id).map(|val| *val);

        if let Some((price, side)) = location {
            // Obtener el mapa de niveles de precio apropiado
            let price_levels = match side {
                Side::Buy => &self.bids,
                Side::Sell => &self.asks,
            };

            // Create the update to cancel
            let update = OrderUpdate::Cancel { order_id };

            // Attempt to cancel the order from the price level
            let mut result = None;
            let mut empty_level = false;

            if let Some(entry) = price_levels.get(&price) {
                let price_level = entry.value();
                // Try to cancel the order
                if let Ok(cancelled) = price_level.update_order(update) {
                    result = cancelled;

                    // notify price level changes
                    if result.is_some()
                        && let Some(ref listener) = self.price_level_changed_listener
                    {
                        listener(PriceLevelChangedEvent {
                            side,
                            price: price_level.price(),
                            quantity: price_level.visible_quantity(),
                        })
                    }

                    // Check if the level became empty
                    empty_level = price_level.order_count() == 0;
                }
            }

            self.cache.invalidate();
            // If we got a result and the order was canceled
            if result.is_some() {
                // Remove the order from the locations map
                self.order_locations.remove(&order_id);

                // Unregister special orders from re-pricing tracking
                #[cfg(feature = "special_orders")]
                {
                    self.special_order_tracker
                        .unregister_pegged_order(&order_id);
                    self.special_order_tracker
                        .unregister_trailing_stop(&order_id);
                }

                // If the level became empty, remove it
                if empty_level {
                    price_levels.remove(&price);
                }
            }

            Ok(result.map(|order| Arc::new(self.convert_from_unit_type(&order))))
        } else {
            Ok(None)
        }
    }

    /// Add a new order to the book, automatically matching it if it's aggressive.
    pub fn add_order(&self, mut order: OrderType<T>) -> Result<Arc<OrderType<T>>, OrderBookError> {
        self.cache.invalidate();

        trace!(
            "Order book {}: Adding order {} at price {}",
            self.symbol,
            order.id(),
            order.price()
        );

        if self.has_expired(&order) {
            return Err(OrderBookError::InvalidOperation {
                message: "Order has already expired".to_string(),
            });
        }

        if order.is_post_only() && self.will_cross_market(order.price(), order.side()) {
            return Err(OrderBookError::PriceCrossing {
                price: order.price(),
                side: order.side(),
                opposite_price: if order.side() == Side::Buy {
                    self.best_ask().unwrap_or(0)
                } else {
                    self.best_bid().unwrap_or(0)
                },
            });
        }

        // For FOK orders, first check if the entire quantity can be matched without altering the book.
        if order.is_fill_or_kill() {
            let potential_match =
                self.peek_match(order.side(), order.total_quantity(), Some(order.price()));
            if potential_match < order.total_quantity() {
                return Err(OrderBookError::InsufficientLiquidity {
                    side: order.side(),
                    requested: order.total_quantity(),
                    available: potential_match,
                });
            }
        }

        self.cache.invalidate();
        // Attempt to match the order immediately
        let match_result = self.match_order(
            order.id(),
            order.side(),
            order.total_quantity(), // Use total quantity for matching
            Some(order.price()),
        )?;

        if !match_result.transactions.transactions.is_empty()
            && let Some(ref listener) = self.trade_listener
        {
            let trade_result = TradeResult::new(self.symbol.clone(), match_result.clone());
            listener(&trade_result) // emit trade events to listener
        }

        // If the order was not fully filled, add the remainder to the book
        if match_result.remaining_quantity > 0 {
            if order.is_immediate() {
                // IOC/FOK orders should not have a resting part.
                // If FOK, it should have been fully filled or cancelled before this point.
                // If IOC, this is the remaining part that couldn't be filled, so we just drop it.
                return Err(OrderBookError::InsufficientLiquidity {
                    side: order.side(),
                    requested: order.quantity(), // Now uses the trait method
                    available: order
                        .quantity()
                        .saturating_sub(match_result.remaining_quantity),
                });
            }

            // Update the order with the remaining quantity
            // For iceberg orders, only update if there was actual matching (remaining < total)
            if match_result.remaining_quantity < order.total_quantity() {
                order.set_quantity(match_result.remaining_quantity); // Now uses the trait method
            }

            let price = order.price();
            let side = order.side();

            let price_levels = match side {
                Side::Buy => &self.bids,
                Side::Sell => &self.asks,
            };

            let price_level = price_levels.get_or_insert(price, Arc::new(PriceLevel::new(price)));
            let level = price_level.value();

            // Convert to unit type for PriceLevel compatibility
            let unit_order = self.convert_to_unit_type(&order);
            let unit_order_arc = price_level.value().add_order(unit_order);
            // notify price level changes
            if let Some(ref listener) = self.price_level_changed_listener {
                listener(PriceLevelChangedEvent {
                    side,
                    price: level.price(),
                    quantity: level.visible_quantity(),
                })
            }
            self.order_locations
                .insert(unit_order_arc.id(), (price, side));

            // Register special orders for re-pricing tracking
            #[cfg(feature = "special_orders")]
            match &order {
                OrderType::PeggedOrder { id, .. } => {
                    self.special_order_tracker.register_pegged_order(*id);
                }
                OrderType::TrailingStop { id, .. } => {
                    self.special_order_tracker.register_trailing_stop(*id);
                }
                _ => {}
            }

            // Convert back to generic type for return
            let generic_order = self.convert_from_unit_type(&unit_order_arc);
            Ok(Arc::new(generic_order))
        } else {
            // The order was fully matched, create an Arc from the matched result
            // Note: The original order object is consumed, but we can reconstruct its essence if needed.
            // For now, we return a representation of the completed order.
            Ok(Arc::new(order))
        }
    }
}
