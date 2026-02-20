//! Contains the core matching engine logic for the order book.
//!
//! The matching engine supports Self-Trade Prevention (STP) when configured
//! via [`crate::STPMode`]. When STP is disabled (`STPMode::None`, the default),
//! the matching hot path is unchanged with zero overhead.

use crate::orderbook::book_change_event::PriceLevelChangedEvent;
use crate::orderbook::pool::MatchingPool;
use crate::orderbook::stp::{STPAction, check_stp_at_level};
use crate::{OrderBook, OrderBookError};
use pricelevel::{Hash32, MatchResult, OrderId, OrderUpdate, Side};
use std::sync::atomic::Ordering;

impl<T> OrderBook<T>
where
    T: Clone + Send + Sync + Default + 'static,
{
    /// Highly optimized internal matching function.
    ///
    /// This is the backward-compatible entry point that delegates to
    /// [`Self::match_order_with_user`] with `Hash32::zero()` (bypasses STP).
    ///
    /// # Performance Optimization
    /// Uses SkipMap which maintains prices in sorted order automatically.
    /// This eliminates O(N log N) sorting overhead, reducing time complexity
    /// from O(N log N) to O(M log N), where:
    /// - N = total number of price levels
    /// - M = number of price levels actually matched (typically << N)
    ///
    /// In the happy case (single price level fill), complexity is O(log N).
    pub fn match_order(
        &self,
        order_id: OrderId,
        side: Side,
        quantity: u64,
        limit_price: Option<u128>,
    ) -> Result<MatchResult, OrderBookError> {
        self.match_order_with_user(order_id, side, quantity, limit_price, Hash32::zero())
    }

    /// Internal matching function with Self-Trade Prevention support.
    ///
    /// When `taker_user_id` is `Hash32::zero()` or `stp_mode` is `None`,
    /// the STP check is skipped entirely (zero overhead fast path).
    ///
    /// # Arguments
    /// * `order_id` — The taker (incoming) order's unique identifier.
    /// * `side` — The side of the incoming order (`Buy` or `Sell`).
    /// * `quantity` — The quantity to match.
    /// * `limit_price` — Optional price limit (`None` for market orders).
    /// * `taker_user_id` — The user ID of the incoming order for STP checks.
    ///
    /// # Errors
    /// Returns [`OrderBookError::InsufficientLiquidity`] for market orders
    /// when no liquidity is available, or [`OrderBookError::SelfTradePrevented`]
    /// when STP in `CancelTaker` or `CancelBoth` mode cancels the entire taker.
    pub fn match_order_with_user(
        &self,
        order_id: OrderId,
        side: Side,
        quantity: u64,
        limit_price: Option<u128>,
        taker_user_id: Hash32,
    ) -> Result<MatchResult, OrderBookError> {
        self.cache.invalidate();
        let mut match_result = MatchResult::new(order_id, quantity);
        let mut remaining_quantity = quantity;

        // Determine if STP checks are needed for this match
        let stp_active = self.stp_mode.is_enabled() && taker_user_id != Hash32::zero();

        // Choose the appropriate side for matching
        let match_side = match side {
            Side::Buy => &self.asks,
            Side::Sell => &self.bids,
        };

        // Early exit if the opposite side is empty
        if match_side.is_empty() {
            if limit_price.is_none() {
                return Err(OrderBookError::InsufficientLiquidity {
                    side,
                    requested: quantity,
                    available: 0,
                });
            }
            match_result.remaining_quantity = remaining_quantity;
            return Ok(match_result);
        }

        // Use static memory pool for better performance
        thread_local! {
            static MATCHING_POOL: MatchingPool = MatchingPool::new();
        }

        // Get reusable vectors from pool
        let (mut filled_orders, mut empty_price_levels) = MATCHING_POOL.with(|pool| {
            let filled = pool.get_filled_orders_vec();
            let empty = pool.get_price_vec();
            (filled, empty)
        });

        // Track whether STP cancelled the taker
        let mut stp_taker_cancelled = false;

        // Iterate through prices in optimal order (already sorted by SkipMap)
        // For buy orders: iterate asks in ascending order (best ask first)
        // For sell orders: iterate bids in descending order (best bid first)
        let price_iter: Box<dyn Iterator<Item = _>> = match side {
            Side::Buy => Box::new(match_side.iter()),
            Side::Sell => Box::new(match_side.iter().rev()),
        };

        // Process each price level
        for entry in price_iter {
            let price = *entry.key();
            // Check price limit constraint early
            if let Some(limit) = limit_price {
                match side {
                    Side::Buy if price > limit => break,
                    Side::Sell if price < limit => break,
                    _ => {}
                }
            }

            // Get price level value from the entry
            let price_level = entry.value();

            // --- STP pre-processing ---
            // When STP is active, check for self-trade conflicts before matching.
            // This is done per-price-level to handle partial fills correctly.
            if stp_active {
                let orders = price_level.iter_orders();
                let action = check_stp_at_level(&orders, taker_user_id, self.stp_mode);

                match action {
                    STPAction::NoConflict => {
                        // No self-trade at this level; match normally below
                    }

                    STPAction::CancelTaker { safe_quantity } => {
                        // Match up to safe_quantity, then cancel the taker
                        if safe_quantity > 0 {
                            let match_qty = remaining_quantity.min(safe_quantity);
                            let saved_remaining = remaining_quantity;
                            let price_level_match = price_level.match_order(
                                match_qty,
                                order_id,
                                &self.transaction_id_generator,
                            );
                            // Compute actual executed from the sub-match
                            let executed =
                                match_qty.saturating_sub(price_level_match.remaining_quantity);
                            Self::process_level_match(
                                &mut match_result,
                                &price_level_match,
                                &mut filled_orders,
                                &mut remaining_quantity,
                                price,
                                price_level,
                                side,
                                &self.last_trade_price,
                                &self.has_traded,
                                &self.price_level_changed_listener,
                                &mut empty_price_levels,
                            );
                            // Correct remaining: process_level_match set it to
                            // sub-match remaining, but we need overall remaining.
                            remaining_quantity = saved_remaining.saturating_sub(executed);
                        }
                        stp_taker_cancelled = true;
                        break;
                    }

                    STPAction::CancelMaker { maker_order_ids } => {
                        // Cancel same-user resting orders, then match normally
                        for maker_id in &maker_order_ids {
                            let _ = price_level.update_order(OrderUpdate::Cancel {
                                order_id: *maker_id,
                            });
                            self.order_locations.remove(maker_id);
                            self.untrack_user_order(taker_user_id, maker_id);
                        }
                        // If the level is now empty, mark for removal and continue
                        if price_level.order_count() == 0 {
                            empty_price_levels.push(price);
                            continue;
                        }
                        // Fall through to normal matching below
                    }

                    STPAction::CancelBoth {
                        safe_quantity,
                        maker_order_id,
                    } => {
                        // Match up to safe_quantity, cancel the maker, then cancel taker
                        if safe_quantity > 0 {
                            let match_qty = remaining_quantity.min(safe_quantity);
                            let saved_remaining = remaining_quantity;
                            let price_level_match = price_level.match_order(
                                match_qty,
                                order_id,
                                &self.transaction_id_generator,
                            );
                            let executed =
                                match_qty.saturating_sub(price_level_match.remaining_quantity);
                            Self::process_level_match(
                                &mut match_result,
                                &price_level_match,
                                &mut filled_orders,
                                &mut remaining_quantity,
                                price,
                                price_level,
                                side,
                                &self.last_trade_price,
                                &self.has_traded,
                                &self.price_level_changed_listener,
                                &mut empty_price_levels,
                            );
                            // Correct remaining: process_level_match set it to
                            // sub-match remaining, but we need overall remaining.
                            remaining_quantity = saved_remaining.saturating_sub(executed);
                        }
                        // Cancel the maker order
                        let _ = price_level.update_order(OrderUpdate::Cancel {
                            order_id: maker_order_id,
                        });
                        self.order_locations.remove(&maker_order_id);
                        self.untrack_user_order(taker_user_id, &maker_order_id);
                        if price_level.order_count() == 0 {
                            empty_price_levels.push(price);
                        }
                        stp_taker_cancelled = true;
                        break;
                    }
                }
            }

            // --- Normal matching (no STP conflict or after CancelMaker cleanup) ---
            let price_level_match = price_level.match_order(
                remaining_quantity,
                order_id,
                &self.transaction_id_generator,
            );

            Self::process_level_match(
                &mut match_result,
                &price_level_match,
                &mut filled_orders,
                &mut remaining_quantity,
                price,
                price_level,
                side,
                &self.last_trade_price,
                &self.has_traded,
                &self.price_level_changed_listener,
                &mut empty_price_levels,
            );

            // Early exit if order is fully matched
            if remaining_quantity == 0 {
                break;
            }
        }

        // Batch remove empty price levels
        for price in &empty_price_levels {
            match_side.remove(price);
        }

        // Batch remove filled orders from tracking
        for filled_id in &filled_orders {
            self.order_locations.remove(filled_id);
            self.untrack_order_by_id(filled_id);
        }

        // Return vectors to pool for reuse
        MATCHING_POOL.with(|pool| {
            pool.return_filled_orders_vec(filled_orders);
            pool.return_price_vec(empty_price_levels);
        });

        // If STP cancelled the taker and no fills occurred at all, return STP error.
        // When partial fills happened (remaining < original quantity), return Ok
        // with the partial result so the caller can see what was executed.
        if stp_taker_cancelled && remaining_quantity == quantity {
            return Err(OrderBookError::SelfTradePrevented {
                mode: self.stp_mode,
                taker_order_id: order_id,
                user_id: taker_user_id,
            });
        }

        // Check for insufficient liquidity in market orders
        if limit_price.is_none() && remaining_quantity == quantity {
            return Err(OrderBookError::InsufficientLiquidity {
                side,
                requested: quantity,
                available: 0,
            });
        }

        // Set final result properties
        match_result.remaining_quantity = remaining_quantity;
        match_result.is_complete = remaining_quantity == 0;

        Ok(match_result)
    }

    /// Processes match results from a single price level, updating the
    /// aggregate match result and bookkeeping vectors.
    ///
    /// Extracted to avoid code duplication between the normal path and
    /// the STP safe-quantity pre-match path.
    #[allow(clippy::too_many_arguments)]
    fn process_level_match(
        match_result: &mut MatchResult,
        price_level_match: &MatchResult,
        filled_orders: &mut Vec<OrderId>,
        remaining_quantity: &mut u64,
        price: u128,
        price_level: &std::sync::Arc<pricelevel::PriceLevel>,
        side: Side,
        last_trade_price: &crossbeam::atomic::AtomicCell<u128>,
        has_traded: &std::sync::atomic::AtomicBool,
        price_level_changed_listener: &Option<
            crate::orderbook::book_change_event::PriceLevelChangedListener,
        >,
        empty_price_levels: &mut Vec<u128>,
    ) {
        // Process transactions if any occurred
        if !price_level_match.transactions.as_vec().is_empty() {
            // Update last trade price atomically
            last_trade_price.store(price);
            has_traded.store(true, Ordering::Relaxed);

            // Add transactions to result
            for transaction in price_level_match.transactions.as_vec() {
                match_result.add_transaction(*transaction);
            }

            // Notify price level changes
            if let Some(listener) = price_level_changed_listener {
                listener(PriceLevelChangedEvent {
                    side: side.opposite(),
                    price: price_level.price(),
                    quantity: price_level.visible_quantity(),
                });
            }
        }

        // Collect filled orders for batch removal
        for &filled_order_id in &price_level_match.filled_order_ids {
            match_result.add_filled_order_id(filled_order_id);
            filled_orders.push(filled_order_id);
        }

        // Update remaining quantity
        *remaining_quantity = price_level_match.remaining_quantity;

        // Check if price level is empty and mark for removal
        if price_level.order_count() == 0 {
            empty_price_levels.push(price);
        }
    }

    /// Optimized peek match without memory pooling or sorting
    ///
    /// # Performance Optimization
    /// Uses SkipMap's natural ordering to eliminate sorting overhead.
    /// Time complexity: O(M log N) where M = price levels inspected.
    pub fn peek_match(&self, side: Side, quantity: u64, price_limit: Option<u128>) -> u64 {
        let price_levels = match side {
            Side::Buy => &self.asks,
            Side::Sell => &self.bids,
        };

        if price_levels.is_empty() {
            return 0;
        }

        let mut matched_quantity = 0u64;

        // Iterate through prices in optimal order (already sorted by SkipMap)
        let price_iter: Box<dyn Iterator<Item = _>> = match side {
            Side::Buy => Box::new(price_levels.iter()),
            Side::Sell => Box::new(price_levels.iter().rev()),
        };

        // Process each price level
        for entry in price_iter {
            // Early termination when we have enough quantity
            if matched_quantity >= quantity {
                break;
            }

            let price = *entry.key();

            // Check price limit
            if let Some(limit) = price_limit {
                match side {
                    Side::Buy if price > limit => break,
                    Side::Sell if price < limit => break,
                    _ => {}
                }
            }

            // Get available quantity at this level
            let price_level = entry.value();
            let available_quantity = price_level.total_quantity();
            let needed_quantity = quantity.saturating_sub(matched_quantity);
            let quantity_to_match = needed_quantity.min(available_quantity);
            matched_quantity = matched_quantity.saturating_add(quantity_to_match);
        }

        matched_quantity
    }

    /// Batch operation for multiple order matches (additional optimization)
    pub fn match_orders_batch(
        &self,
        orders: &[(OrderId, Side, u64, Option<u128>)],
    ) -> Vec<Result<MatchResult, OrderBookError>> {
        let mut results = Vec::with_capacity(orders.len());

        for &(order_id, side, quantity, limit_price) in orders {
            let result = OrderBook::<T>::match_order(self, order_id, side, quantity, limit_price);
            results.push(result);
        }

        results
    }
}
