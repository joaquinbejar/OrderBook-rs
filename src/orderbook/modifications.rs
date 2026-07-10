use crate::orderbook::book::OrderBook;
use crate::orderbook::book_change_event::PriceLevelChangedEvent;
use crate::orderbook::error::OrderBookError;
use crate::orderbook::matching::MatchOutcome;
use crate::orderbook::order_state::{CancelReason, OrderStatus};
use crate::orderbook::reject_reason::RejectReason;
use crate::orderbook::trade::TradeResult;
use either::Either;
use pricelevel::{Id, OrderType, OrderUpdate, PriceLevel, Quantity, Side};
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
            OrderType::Standard { quantity, .. } => quantity.as_u64(),
            OrderType::IcebergOrder {
                visible_quantity, ..
            } => visible_quantity.as_u64(),
            OrderType::PostOnly { quantity, .. } => quantity.as_u64(),
            OrderType::TrailingStop { quantity, .. } => quantity.as_u64(),
            OrderType::PeggedOrder { quantity, .. } => quantity.as_u64(),
            OrderType::MarketToLimit { quantity, .. } => quantity.as_u64(),
            OrderType::ReserveOrder {
                visible_quantity, ..
            } => visible_quantity.as_u64(),
        }
    }

    fn total_quantity(&self) -> u64 {
        match self {
            OrderType::Standard { quantity, .. } => quantity.as_u64(),
            OrderType::IcebergOrder {
                visible_quantity,
                hidden_quantity,
                ..
            } => visible_quantity
                .as_u64()
                .saturating_add(hidden_quantity.as_u64()),
            OrderType::PostOnly { quantity, .. } => quantity.as_u64(),
            OrderType::TrailingStop { quantity, .. } => quantity.as_u64(),
            OrderType::PeggedOrder { quantity, .. } => quantity.as_u64(),
            OrderType::MarketToLimit { quantity, .. } => quantity.as_u64(),
            OrderType::ReserveOrder {
                visible_quantity,
                hidden_quantity,
                ..
            } => visible_quantity
                .as_u64()
                .saturating_add(hidden_quantity.as_u64()),
        }
    }

    fn set_quantity(&mut self, new_total_quantity: u64) {
        match self {
            OrderType::Standard { quantity, .. }
            | OrderType::PostOnly { quantity, .. }
            | OrderType::TrailingStop { quantity, .. }
            | OrderType::PeggedOrder { quantity, .. }
            | OrderType::MarketToLimit { quantity, .. } => {
                *quantity = Quantity::new(new_total_quantity)
            }

            OrderType::IcebergOrder {
                visible_quantity, ..
            } => {
                // For iceberg orders, treat new_total_quantity as the new visible quantity
                // This matches the expected behavior where quantity() returns visible_quantity
                *visible_quantity = Quantity::new(new_total_quantity);
                // Hidden quantity remains unchanged
            }
            OrderType::ReserveOrder {
                visible_quantity,
                hidden_quantity,
                replenish_amount,
                ..
            } => {
                let original_total = visible_quantity
                    .as_u64()
                    .saturating_add(hidden_quantity.as_u64());
                let amount_to_reduce = original_total.saturating_sub(new_total_quantity);

                let vis = visible_quantity.as_u64();
                let filled_from_visible = amount_to_reduce.min(vis);
                *visible_quantity = Quantity::new(vis.saturating_sub(filled_from_visible));

                let remaining_to_reduce = amount_to_reduce - filled_from_visible;
                *hidden_quantity =
                    Quantity::new(hidden_quantity.as_u64().saturating_sub(remaining_to_reduce));

                if visible_quantity.as_u64() == 0 && hidden_quantity.as_u64() > 0 {
                    let refresh = replenish_amount
                        .map(|q| q.get())
                        .unwrap_or(0)
                        .min(hidden_quantity.as_u64());
                    *visible_quantity = Quantity::new(refresh);
                    *hidden_quantity =
                        Quantity::new(hidden_quantity.as_u64().saturating_sub(refresh));
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
    ///
    /// # Errors
    /// Returns [`OrderBookError::KillSwitchActive`] when the kill switch
    /// is engaged and the update is anything other than
    /// [`OrderUpdate::Cancel`]. Cancels are explicitly allowed so that
    /// operators can drain resting orders while new flow is halted.
    pub fn update_order(
        &self,
        update: OrderUpdate,
    ) -> Result<Option<Arc<OrderType<T>>>, OrderBookError> {
        // Gate non-cancel variants on the kill switch. Cancel passes
        // through unchanged so operators can drain the book. The
        // existing order stays live — only the modification is
        // rejected — so we use `check_kill_switch` (no tracker
        // recording) rather than `check_kill_switch_or_reject` (which
        // would mark a live order as terminal-Rejected).
        let is_modify = matches!(
            &update,
            OrderUpdate::UpdatePrice { .. }
                | OrderUpdate::UpdateQuantity { .. }
                | OrderUpdate::UpdatePriceAndQuantity { .. }
                | OrderUpdate::Replace { .. }
        );
        if is_modify {
            self.check_kill_switch()?;
        }

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
                    if old_price == new_price.as_u128() {
                        return Err(OrderBookError::InvalidOperation {
                            message: "Cannot update price to the same value".to_string(),
                        });
                    }

                    // Get the original order without holding locks
                    let original_order = if let Some(order) = self.get_order(order_id) {
                        // Create a copy of the order
                        (*order).clone()
                    } else {
                        return Ok(None); // Order not found
                    };

                    // Create a new order with the updated price
                    let mut new_order = original_order.clone();

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

                    // Validate-first atomic modify (#98): validate the new
                    // order's shape and run the modify-aware risk check
                    // *before* removing the original. On any rejection we
                    // return the typed error and the original order is
                    // never cancelled — no book mutation, no events, no
                    // trades. These checks are pure functions of the new
                    // order + the opposite book side, so evaluating them
                    // while the same-side original still rests yields the
                    // same verdict as after cancel.
                    self.validate_order_shape(&new_order)?;
                    self.check_risk_modify_admission(
                        order_id,
                        new_order.user_id(),
                        new_order.price().as_u128(),
                        new_order.total_quantity(),
                    )?;

                    // #168: reject a re-price that would self-cross the same
                    // user's opposite-side liquidity under CancelTaker/CancelBoth
                    // BEFORE cancelling the original, so the original survives.
                    self.check_modify_stp_self_cross(&new_order)?;

                    // Both checks passed: cancel the original and add the
                    // updated order. `add_order` re-runs its own checks;
                    // post-cancel the account count is restored so its risk
                    // check passes — consistent with the pre-guard.
                    self.cancel_order(order_id)?;
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
                                let engine_seq = self.next_engine_seq();
                                listener(PriceLevelChangedEvent {
                                    side,
                                    price: price_level.price(),
                                    quantity: price_level.visible_quantity(),
                                    engine_seq,
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
                        self.untrack_order_by_id(&order_id);
                    }

                    self.cache.invalidate();
                    if is_empty {
                        // Refresh depth gauges now that a level was
                        // removed during the modification path.
                        self.record_depth_metric();
                    }
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

                if location.is_some() {
                    // Get the original order without holding locks
                    let original_order = if let Some(order) = self.get_order(order_id) {
                        // Create a copy of the order
                        (*order).clone()
                    } else {
                        return Ok(None); // Order not found
                    };

                    // Create a new order with the updated price and quantity
                    let mut new_order = original_order.clone();

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
                    new_order.set_quantity(new_quantity.as_u64());

                    // Validate-first atomic modify (#98): validate the new
                    // order's shape and run the modify-aware risk check
                    // *before* removing the original. On any rejection the
                    // original order is never cancelled.
                    self.validate_order_shape(&new_order)?;
                    self.check_risk_modify_admission(
                        order_id,
                        new_order.user_id(),
                        new_order.price().as_u128(),
                        new_order.total_quantity(),
                    )?;

                    // #168: reject a re-price that would self-cross the same
                    // user's opposite-side liquidity under CancelTaker/CancelBoth
                    // BEFORE cancelling the original, so the original survives.
                    self.check_modify_stp_self_cross(&new_order)?;

                    // Both checks passed: cancel the original and add the
                    // updated order.
                    self.cancel_order(order_id)?;
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
                                let engine_seq = self.next_engine_seq();
                                listener(PriceLevelChangedEvent {
                                    side,
                                    price: price_level.price(),
                                    quantity: price_level.visible_quantity(),
                                    engine_seq,
                                })
                            }
                            is_empty = price_level.order_count() == 0;
                        }

                        // Remove from order locations tracking
                        self.order_locations.remove(&order_id);
                        // Remove from user_orders index
                        self.untrack_order_by_id(&order_id);
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

                    // Validate-first atomic modify (#98): validate the new
                    // order's shape and run the modify-aware risk check
                    // *before* removing the original. On any rejection the
                    // original order is never cancelled — no book mutation,
                    // no events, no trades.
                    self.validate_order_shape(&new_order)?;
                    self.check_risk_modify_admission(
                        order_id,
                        new_order.user_id(),
                        new_order.price().as_u128(),
                        new_order.total_quantity(),
                    )?;

                    // #168: reject a re-price that would self-cross the same
                    // user's opposite-side liquidity under CancelTaker/CancelBoth
                    // BEFORE cancelling the original, so the original survives.
                    self.check_modify_stp_self_cross(&new_order)?;

                    // Both checks passed: cancel the original and add the
                    // new order.
                    self.cancel_order(order_id)?;
                    let result = self.add_order(new_order)?;
                    Ok(Some(result))
                } else {
                    Ok(None) // Original order not found
                }
            }
        }
    }

    /// Cancel an order by ID.
    ///
    /// Tracks the cancellation as `CancelReason::UserRequested` in the
    /// order state tracker (if configured).
    pub fn cancel_order(&self, order_id: Id) -> Result<Option<Arc<OrderType<T>>>, OrderBookError> {
        self.cancel_order_with_reason(order_id, CancelReason::UserRequested)
    }

    /// Cancel an order by ID with an explicit cancellation reason.
    ///
    /// This is the internal implementation used by both `cancel_order`
    /// and mass cancel operations to track the correct
    /// [`CancelReason`] in the order state tracker.
    pub(super) fn cancel_order_with_reason(
        &self,
        order_id: Id,
        reason: CancelReason,
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
                        let engine_seq = self.next_engine_seq();
                        listener(PriceLevelChangedEvent {
                            side,
                            price: price_level.price(),
                            quantity: price_level.visible_quantity(),
                            engine_seq,
                        })
                    }

                    // Check if the level became empty
                    empty_level = price_level.order_count() == 0;
                }
            }

            self.cache.invalidate();
            // If we got a result and the order was canceled
            if let Some(ref cancelled_order) = result {
                // Track the cancellation in the order state tracker
                let prev_filled = self
                    .order_state_tracker
                    .as_ref()
                    .and_then(|t| t.get(order_id))
                    .map(|s| s.filled_quantity())
                    .unwrap_or(0);
                self.track_state(
                    order_id,
                    OrderStatus::Cancelled {
                        filled_quantity: prev_filled,
                        reason,
                    },
                );

                // Remove the order from the locations map
                self.order_locations.remove(&order_id);

                // Pre-trade risk hook: drop the per-account counter
                // contribution before the order leaves the index. Does
                // not depend on `cancelled_order` because the risk
                // state already stores `account` and `remaining_qty`.
                // No-op when no `RiskConfig` is installed.
                self.risk_state.on_cancel(order_id);

                // Remove the order from the user_orders index
                self.untrack_user_order(cancelled_order.user_id(), &order_id);

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
                    // Refresh the depth gauges now that a level was
                    // removed. No-op when the `metrics` feature is
                    // disabled.
                    self.record_depth_metric();
                }
            }

            Ok(result.map(|order| Arc::new(self.convert_from_unit_type(&order))))
        } else {
            Ok(None)
        }
    }

    /// Apply the side-effects of cancelling a single resting `order_id` that is
    /// known to live on the already-held `price_level` (resting on `side`),
    /// **without** removing the level from the bid/ask map.
    ///
    /// This mirrors the per-order effects of [`Self::cancel_order_with_reason`]
    /// — level-change event, `Cancelled { reason }` state transition, per-account
    /// risk release, `user_orders` / `order_locations` untrack, and special-order
    /// deregistration — but it deliberately does **not** touch the bid/ask
    /// `SkipMap`. The caller owns level removal (the matching loop drains
    /// `empty_price_levels` after the walk), so this is safe to invoke mid-walk:
    /// it never removes a level the iterator still references and never
    /// re-resolves `order_locations`, so a sequence of cancels on the same held
    /// level cannot skip a later order. Used by the STP `CancelMaker` /
    /// `CancelBoth` arms (#95). No-op if `order_id` is not resting on the level.
    pub(super) fn cancel_resting_maker_on_level(
        &self,
        price_level: &PriceLevel,
        side: Side,
        order_id: Id,
        reason: CancelReason,
    ) {
        let Ok(Some(cancelled)) = price_level.update_order(OrderUpdate::Cancel { order_id }) else {
            return;
        };
        self.cache.invalidate();

        // 1. Notify the level change (same shape as cancel_order_with_reason).
        if let Some(ref listener) = self.price_level_changed_listener {
            let engine_seq = self.next_engine_seq();
            listener(PriceLevelChangedEvent {
                side,
                price: price_level.price(),
                quantity: price_level.visible_quantity(),
                engine_seq,
            });
        }

        // 2. Record the terminal cancellation, preserving any prior fill.
        let prev_filled = self
            .order_state_tracker
            .as_ref()
            .and_then(|t| t.get(order_id))
            .map(|s| s.filled_quantity())
            .unwrap_or(0);
        self.track_state(
            order_id,
            OrderStatus::Cancelled {
                filled_quantity: prev_filled,
                reason,
            },
        );

        // 3. Drop the per-account risk contribution, then untrack the order.
        self.order_locations.remove(&order_id);
        self.risk_state.on_cancel(order_id);
        self.untrack_user_order(cancelled.user_id(), &order_id);

        #[cfg(feature = "special_orders")]
        {
            self.special_order_tracker
                .unregister_pegged_order(&order_id);
            self.special_order_tracker
                .unregister_trailing_stop(&order_id);
        }
    }

    /// Validate the *shape* of an order against this book's admission
    /// rules **without** mutating any book state.
    ///
    /// This is the single source of truth for the non-risk admission
    /// checks that [`Self::add_order`] performs, in the same order and
    /// returning the same typed [`OrderBookError`] variants. Unlike
    /// `add_order` it is pure: it never calls
    /// [`track_state`](Self::track_state), [`reject_with_risk`](Self::reject_with_risk),
    /// emits metrics, or invalidates the cache. Every check here is a
    /// function of the new order plus the *opposite* book side, so it
    /// yields the same verdict whether evaluated before or after the
    /// original (same-side) order has been cancelled — which is what
    /// makes the validate-first atomic modify (#98) safe.
    ///
    /// Checks, in order:
    /// 1. STP `MissingUserId` (when STP is enabled and `user_id` is zero).
    /// 2. Tick size (`InvalidTickSize`).
    /// 3. Lot size (`InvalidLotSize`, iceberg visible/hidden split).
    /// 4. Min/max order size (`OrderSizeOutOfRange`).
    /// 5. Expiry (`InvalidOperation` — already expired).
    /// 6. Post-only would cross (`PriceCrossing`).
    /// 7. FOK feasibility (`InsufficientLiquidity`).
    ///
    /// # Errors
    /// Returns the first failing check's typed [`OrderBookError`].
    pub(super) fn validate_order_shape(&self, order: &OrderType<T>) -> Result<(), OrderBookError> {
        // STP user_id enforcement: when STP is enabled, all orders must carry
        // a non-zero user_id so that self-trade checks can identify the owner.
        if self.stp_mode != crate::orderbook::stp::STPMode::None
            && order.user_id() == pricelevel::Hash32::zero()
        {
            return Err(OrderBookError::MissingUserId {
                order_id: order.id(),
            });
        }

        // Tick size validation: reject orders whose price is not a multiple of tick_size
        if let Some(tick) = self.tick_size
            && tick > 0
            && !order.price().as_u128().is_multiple_of(tick)
        {
            return Err(OrderBookError::InvalidTickSize {
                price: order.price().as_u128(),
                tick_size: tick,
            });
        }

        // Lot size validation: reject orders whose quantity is not a multiple of lot_size.
        // For iceberg orders, validate visible and hidden quantities individually.
        if let Some(lot) = self.lot_size
            && lot > 0
        {
            match order {
                OrderType::IcebergOrder {
                    visible_quantity,
                    hidden_quantity,
                    ..
                } => {
                    if visible_quantity.as_u64() % lot != 0 {
                        return Err(OrderBookError::InvalidLotSize {
                            quantity: visible_quantity.as_u64(),
                            lot_size: lot,
                        });
                    }
                    if hidden_quantity.as_u64() % lot != 0 {
                        return Err(OrderBookError::InvalidLotSize {
                            quantity: hidden_quantity.as_u64(),
                            lot_size: lot,
                        });
                    }
                }
                _ => {
                    if order.total_quantity() % lot != 0 {
                        return Err(OrderBookError::InvalidLotSize {
                            quantity: order.total_quantity(),
                            lot_size: lot,
                        });
                    }
                }
            }
        }

        // Min/max order size validation
        let qty = order.total_quantity();
        if let Some(min) = self.min_order_size
            && qty < min
        {
            return Err(OrderBookError::OrderSizeOutOfRange {
                quantity: qty,
                min: Some(min),
                max: self.max_order_size,
            });
        }
        if let Some(max) = self.max_order_size
            && qty > max
        {
            return Err(OrderBookError::OrderSizeOutOfRange {
                quantity: qty,
                min: self.min_order_size,
                max: Some(max),
            });
        }

        if self.has_expired(order) {
            return Err(OrderBookError::InvalidOperation {
                message: "Order has already expired".to_string(),
            });
        }

        if order.is_post_only() && self.will_cross_market(order.price().as_u128(), order.side()) {
            return Err(OrderBookError::PriceCrossing {
                price: order.price().as_u128(),
                side: order.side(),
                opposite_price: if order.side() == Side::Buy {
                    self.best_ask().unwrap_or(0)
                } else {
                    self.best_bid().unwrap_or(0)
                },
            });
        }

        // For FOK orders, first check if the entire quantity can be matched
        // without altering the book. Use the faithful feasibility check (lot_size
        // + STP aware), not the raw-depth `peek_match`, so fill-or-kill stays
        // all-or-nothing and never emits a partial fill it then reports as killed (#96).
        if order.is_fill_or_kill() {
            let potential_match = self.fok_fillable_quantity(
                order.side(),
                order.total_quantity(),
                Some(order.price().as_u128()),
                order.user_id(),
            );
            if potential_match < order.total_quantity() {
                return Err(OrderBookError::InsufficientLiquidity {
                    side: order.side(),
                    requested: order.total_quantity(),
                    available: potential_match,
                });
            }
        }

        Ok(())
    }

    /// STP self-cross pre-check for the validate-first atomic modify (#168).
    ///
    /// Closes the one post-match modify-atomicity gap #98 left open. Under
    /// [`STPMode::CancelTaker`](crate::orderbook::stp::STPMode::CancelTaker) /
    /// [`CancelBoth`](crate::orderbook::stp::STPMode::CancelBoth), if a
    /// re-priced order would cross into the **same user's** resting liquidity on
    /// the opposite side, `add_order` matches post-cancel and cancels the taker
    /// (the re-added order) — *after* the original was already removed,
    /// destroying it. This dry-runs the crossable opposite side and, if the
    /// sweep would reach a same-user maker while the taker still has unfilled
    /// quantity (the exact condition under which the engine sets
    /// `stp_taker_cancelled`), returns [`OrderBookError::SelfTradePrevented`]
    /// **before** the original is cancelled, so it survives unchanged.
    ///
    /// No-op when STP is off, the taker is anonymous, or the mode is
    /// [`CancelMaker`](crate::orderbook::stp::STPMode::CancelMaker) (which
    /// cancels the maker and rests the taker — it never destroys the re-added
    /// order). Like the other validate-first checks (#98) it is a pure function
    /// of the new order plus the *opposite* book side, so evaluating it while
    /// the same-side original still rests yields the same verdict as after
    /// cancel.
    pub(super) fn check_modify_stp_self_cross(
        &self,
        new_order: &OrderType<T>,
    ) -> Result<(), OrderBookError> {
        use crate::orderbook::stp::STPMode;

        let taker_user_id = new_order.user_id();
        // Only CancelTaker / CancelBoth cancel the taker; None / CancelMaker
        // rest it, so the re-added order is never destroyed.
        match self.stp_mode {
            STPMode::CancelTaker | STPMode::CancelBoth => {}
            _ => return Ok(()),
        }
        if taker_user_id == pricelevel::Hash32::zero() {
            return Ok(());
        }

        let side = new_order.side();
        let new_price = new_order.price().as_u128();
        let opposite = match side {
            Side::Buy => &self.asks,
            Side::Sell => &self.bids,
        };
        // Walk the crossable opposite side in price-time priority — asks
        // ascending for a Buy, bids descending for a Sell — exactly the sweep's
        // visit order.
        let iter = match side {
            Side::Buy => Either::Left(opposite.iter()),
            Side::Sell => Either::Right(opposite.iter().rev()),
        };

        let mut remaining = new_order.total_quantity();
        for entry in iter {
            if remaining == 0 {
                // The taker fully fills against non-self depth before reaching
                // any same-user maker → the engine never cancels it.
                return Ok(());
            }
            let price = *entry.key();
            let crosses = match side {
                Side::Buy => new_price >= price,
                Side::Sell => new_price <= price,
            };
            if !crosses {
                // Price-sorted levels: no further level can cross.
                break;
            }
            let level = entry.value();
            if level.iter_orders().any(|o| o.user_id() == taker_user_id) {
                // The sweep reaches a level holding a same-user maker while the
                // taker still has unfilled quantity: the engine would cancel the
                // taker here. Reject the modify before the original is cancelled.
                return Err(OrderBookError::SelfTradePrevented {
                    mode: self.stp_mode,
                    taker_order_id: new_order.id(),
                    user_id: taker_user_id,
                });
            }
            // No same-user maker at this level: the taker consumes its full
            // matchable depth (the authoritative upstream dry run), then walks on.
            remaining = remaining.saturating_sub(level.matchable_quantity(remaining));
        }
        Ok(())
    }

    /// Record the terminal state transition (and metric) that the direct
    /// [`Self::add_order`] path historically emitted for each shape
    /// rejection returned by [`Self::validate_order_shape`].
    ///
    /// Keeping this mapping next to the validator preserves the exact
    /// pre-#98 reject side-effects of `add_order` while letting the
    /// validate-first modify path reuse the same pure validator without
    /// recording any state. Errors that previously had no side-effect
    /// (e.g. the already-expired `InvalidOperation`) are intentionally
    /// no-ops here.
    fn record_shape_rejection(&self, order: &OrderType<T>, err: &OrderBookError) {
        match err {
            OrderBookError::MissingUserId { .. } => {
                self.track_state(
                    order.id(),
                    OrderStatus::Rejected {
                        reason: RejectReason::MissingUserId,
                    },
                );
            }
            OrderBookError::InvalidTickSize { .. } => {
                self.track_state(
                    order.id(),
                    OrderStatus::Rejected {
                        reason: RejectReason::InvalidPrice,
                    },
                );
            }
            OrderBookError::InvalidLotSize { .. } => {
                self.track_state(
                    order.id(),
                    OrderStatus::Rejected {
                        reason: RejectReason::InvalidQuantity,
                    },
                );
            }
            OrderBookError::OrderSizeOutOfRange { .. } => {
                self.track_state(
                    order.id(),
                    OrderStatus::Rejected {
                        reason: RejectReason::OrderSizeOutOfRange,
                    },
                );
            }
            OrderBookError::PriceCrossing { .. } => {
                self.track_state(
                    order.id(),
                    OrderStatus::Rejected {
                        reason: RejectReason::PostOnlyWouldCross,
                    },
                );
            }
            OrderBookError::InsufficientLiquidity { .. } => {
                self.track_state(
                    order.id(),
                    OrderStatus::Cancelled {
                        filled_quantity: 0,
                        reason: CancelReason::InsufficientLiquidity,
                    },
                );
                crate::orderbook::metrics::record_reject(RejectReason::InsufficientLiquidity);
            }
            // The already-expired `InvalidOperation` path historically
            // recorded no terminal transition; preserve that.
            _ => {}
        }
    }

    /// Add a new order to the book, automatically matching it if it's aggressive.
    ///
    /// This convenience method calls the same implementation as
    /// [`Self::add_order_with_result`] but discards the trade result. When no
    /// trade listener is installed, the `TradeResult` is never constructed, so
    /// this path stays free of the extra `MatchResult` clone.
    ///
    /// # Errors
    /// Returns [`OrderBookError::KillSwitchActive`] when the kill switch
    /// is engaged. The check runs before any cache invalidation, STP
    /// validation, tick/lot validation, or matching work.
    #[inline]
    pub fn add_order(&self, order: OrderType<T>) -> Result<Arc<OrderType<T>>, OrderBookError> {
        self.add_order_inner(order, false).map(|(order, _)| order)
    }

    /// Add a new order to the book, automatically matching it if it's
    /// aggressive, and additionally return the [`TradeResult`] produced by the
    /// match directly to the caller.
    ///
    /// The trade result is `None` when the order produced no fills (it rested
    /// on the book, or was admitted without matching). When a trade listener
    /// is installed, the listener is invoked with the exact same `TradeResult`
    /// that is returned here — same fills, same fees, same `engine_seq`.
    ///
    /// Per-call attribution: concurrent submits on the same book each receive
    /// exactly their own fills; the result is built from this call's private
    /// match outcome, never from shared capture state. The engine holds no
    /// cross-call trade accumulator — each returned `TradeResult` is
    /// constructed from the `MatchResult` produced by this invocation alone —
    /// so two threads submitting crossing orders concurrently cannot observe
    /// each other's fills in their own returned result.
    ///
    /// On error paths that follow real fills (an unfillable IOC remainder, or
    /// a self-trade-prevention cancellation after earlier non-self fills) the
    /// typed error is returned instead, so those fills reach the trade
    /// listener only.
    ///
    /// Every trade-producing call consumes one `engine_seq` tick, even when no
    /// trade listener is installed (plain [`Self::add_order`] only consumes one
    /// when a listener is present). `engine_seq` is per-instance and not
    /// replay-reproducible; consumers that need a stable ordering key should
    /// use the journal's `sequence_num` / `timestamp_ns` instead.
    ///
    /// # Errors
    /// Returns [`OrderBookError::KillSwitchActive`] when the kill switch
    /// is engaged. The check runs before any cache invalidation, STP
    /// validation, tick/lot validation, or matching work.
    pub fn add_order_with_result(
        &self,
        order: OrderType<T>,
    ) -> Result<(Arc<OrderType<T>>, Option<TradeResult>), OrderBookError> {
        self.add_order_inner(order, true)
    }

    /// Shared implementation behind [`Self::add_order`] and
    /// [`Self::add_order_with_result`]. `want_result` gates `TradeResult`
    /// construction so the plain `add_order` path only pays for it when an
    /// installed trade listener needs it anyway.
    fn add_order_inner(
        &self,
        mut order: OrderType<T>,
        want_result: bool,
    ) -> Result<(Arc<OrderType<T>>, Option<TradeResult>), OrderBookError> {
        self.check_kill_switch_or_reject(order.id())?;
        // Pre-trade risk gate: per-account open-orders / notional /
        // price band. No-op when no `RiskConfig` is installed.
        // Documented order: kill_switch → risk → STP → fees → match.
        // On the cold reject path, record an `OrderStatus::Rejected`
        // transition with the closed `RejectReason` taxonomy before
        // propagating the typed error.
        if let Err(err) = self.check_risk_limit_admission(
            order.user_id(),
            order.price().as_u128(),
            order.total_quantity(),
        ) {
            self.reject_with_risk(order.id(), &err);
            return Err(err);
        }

        // Reject a duplicate order id: an order with this id is already
        // resting on the book. Admitting it would overwrite the existing
        // order's entry in `order_locations` and orphan the live order (it
        // could no longer be cancelled or modified by id). This is an
        // `add_order`-specific structural check and deliberately does NOT
        // live in `validate_order_shape`: the validate-first atomic modify
        // (#98) runs that shared validator while the original, same-id
        // order is still resting, so a check there would false-reject every
        // modify. We also do NOT record an `OrderStatus::Rejected`
        // transition — the id belongs to a different, still-live order
        // whose tracked state must not be clobbered. The metric plus the
        // typed error (which the wire layer maps to
        // `RejectReason::DuplicateOrderId`) are sufficient.
        //
        // This is a sequential guard, not a concurrency guard: the check
        // and the eventual `order_locations.insert` straddle the match
        // walk, so two concurrent `add_order` calls with the same *fresh*
        // id can both pass here and both rest (last-writer-wins on insert).
        // Serializing order ids is the ingress / sequencing layer's job.
        if self.order_locations.contains_key(&order.id()) {
            crate::orderbook::metrics::record_reject(RejectReason::DuplicateOrderId);
            return Err(OrderBookError::DuplicateOrderId {
                order_id: order.id(),
            });
        }

        trace!(
            "Order book {}: Adding order {} at price {}",
            self.symbol,
            order.id(),
            order.price()
        );

        // Non-risk admission checks are owned by `validate_order_shape`
        // (the single source of truth shared with the validate-first
        // atomic modify path, #98). On the cold reject path we still
        // record the matching terminal state transition / metric here so
        // the direct (non-modify) `add_order` behavior is preserved
        // exactly.
        if let Err(err) = self.validate_order_shape(&order) {
            self.record_shape_rejection(&order, &err);
            return Err(err);
        }

        self.cache.invalidate();
        // Attempt to match the order immediately (with STP user_id propagation).
        // The outcome also carries whether STP cancelled the taker (#97).
        let MatchOutcome {
            result: match_result,
            taker_stp_cancelled,
        } = self.match_order_with_user_outcome(
            order.id(),
            order.side(),
            order.total_quantity(), // Use total quantity for matching
            Some(order.price().as_u128()),
            order.user_id(),
        )?;

        // Emit trades BEFORE any early return below: the STP taker-cancel and
        // unfillable-IOC paths return `Err` after real (non-self) fills already
        // executed, and those fills must still reach the metrics and the trade
        // listener. The `TradeResult` is only constructed when someone consumes
        // it — the installed listener and/or an `add_order_with_result` caller —
        // so the plain `add_order` hot path skips the `MatchResult` clone.
        let trades_emitted = match_result.trades().len() as u64;
        let trade_result = if trades_emitted > 0 {
            crate::orderbook::metrics::record_trades(trades_emitted);
            let listener = self.trade_listener.as_ref();
            if want_result || listener.is_some() {
                let mut trade_result = TradeResult::with_fees(
                    self.symbol.clone(),
                    match_result.clone(),
                    self.fee_schedule,
                );
                trade_result.engine_seq = self.next_engine_seq();
                if let Some(listener) = listener {
                    listener(&trade_result) // emit trade events to listener
                }
                Some(trade_result)
            } else {
                None
            }
        } else {
            None
        };

        // True (non-self) executed quantity. `remaining_quantity` only decrements on
        // real trades, so STP-prevented self-fills never count toward it.
        let original_qty = order.total_quantity();
        let filled_qty = original_qty.saturating_sub(match_result.remaining_quantity().as_u64());

        // If STP cancelled the taker, the residual must NOT rest — even though some
        // non-self fills already occurred at earlier levels. Record the terminal
        // SelfTradePrevention state with the true filled quantity and surface the STP
        // error (#97). The zero-fills case already returned this error from the match.
        if taker_stp_cancelled {
            self.track_state(
                order.id(),
                OrderStatus::Cancelled {
                    filled_quantity: filled_qty,
                    reason: CancelReason::SelfTradePrevention,
                },
            );
            crate::orderbook::metrics::record_reject(RejectReason::SelfTradePrevention);
            return Err(OrderBookError::SelfTradePrevented {
                mode: self.stp_mode,
                taker_order_id: order.id(),
                user_id: order.user_id(),
            });
        }

        // If the order was not fully filled, add the remainder to the book
        if match_result.remaining_quantity().as_u64() > 0 {
            if order.is_immediate() {
                // IOC/FOK orders should not have a resting part.
                // If FOK, it should have been fully filled or cancelled before this point.
                // If IOC, this is the remaining part that couldn't be filled, so we just drop it.
                self.track_state(
                    order.id(),
                    OrderStatus::Cancelled {
                        filled_quantity: filled_qty,
                        reason: CancelReason::InsufficientLiquidity,
                    },
                );
                crate::orderbook::metrics::record_reject(RejectReason::InsufficientLiquidity);
                return Err(OrderBookError::InsufficientLiquidity {
                    side: order.side(),
                    requested: order.quantity(), // Now uses the trait method
                    available: order
                        .quantity()
                        .saturating_sub(match_result.remaining_quantity().as_u64()),
                });
            }

            // Update the order with the remaining quantity
            // For iceberg orders, only update if there was actual matching (remaining < total)
            if match_result.remaining_quantity().as_u64() < order.total_quantity() {
                order.set_quantity(match_result.remaining_quantity().as_u64()); // Now uses the trait method
            }

            let price = order.price().as_u128();
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
                let engine_seq = self.next_engine_seq();
                listener(PriceLevelChangedEvent {
                    side,
                    price: level.price(),
                    quantity: level.visible_quantity(),
                    engine_seq,
                })
            }
            self.order_locations
                .insert(unit_order_arc.id(), (price, side));

            // Refresh the depth gauges. The level may be brand-new
            // (`get_or_insert` created it) or pre-existing — either
            // way the gauge reflects current state. No-op when the
            // `metrics` feature is disabled.
            self.record_depth_metric();

            // Pre-trade risk hook: register the resting order with
            // the risk state so per-account counters are updated and
            // future checks see the new contribution. No-op when no
            // `RiskConfig` is installed.
            self.risk_state.on_admission(
                unit_order_arc.id(),
                order.user_id(),
                price,
                match_result.remaining_quantity().as_u64(),
            );

            // Track the order in the user_orders index
            self.track_user_order(order.user_id(), unit_order_arc.id());

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

            // Track state: Open (no fills) or PartiallyFilled (some fills, resting)
            if filled_qty > 0 {
                self.track_state(
                    order.id(),
                    OrderStatus::PartiallyFilled {
                        original_quantity: original_qty,
                        filled_quantity: filled_qty,
                    },
                );
            } else {
                self.track_state(order.id(), OrderStatus::Open);
            }

            // Convert back to generic type for return
            let generic_order = self.convert_from_unit_type(&unit_order_arc);
            Ok((Arc::new(generic_order), trade_result))
        } else {
            // The order was fully matched
            self.track_state(
                order.id(),
                OrderStatus::Filled {
                    filled_quantity: original_qty,
                },
            );
            Ok((Arc::new(order), trade_result))
        }
    }
}
