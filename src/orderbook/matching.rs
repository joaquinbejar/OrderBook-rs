//! Contains the core matching engine logic for the order book.
//!
//! The matching engine supports Self-Trade Prevention (STP) when configured
//! via [`crate::STPMode`]. When STP is disabled (`STPMode::None`, the default),
//! the matching hot path is unchanged with zero overhead.

use crate::orderbook::book_change_event::PriceLevelChangedEvent;
use crate::orderbook::order_state::{CancelReason, OrderStatus};
use crate::orderbook::pool::MatchingPool;
use crate::orderbook::stp::{STPAction, check_stp_at_level};
use crate::{OrderBook, OrderBookError};
use either::Either;
use pricelevel::{Hash32, Id, MatchResult, Quantity, Side, TakerKind, TimeInForce};
use std::sync::atomic::Ordering;

/// Selects how the matching loop measures its budget.
///
/// `BaseQty` is the legacy base-asset quantity path (existing market and
/// limit orders). `QuoteAmount` is the quote-notional path used by
/// `match_market_order_by_amount` (Binance `quoteOrderQty` semantics).
/// Always `None` limit price for the notional path — quote-notional is
/// market-only.
#[derive(Debug, Clone, Copy)]
pub(crate) enum MatchMode {
    /// Base-quantity match. `limit_price = None` for market orders.
    BaseQty {
        /// Total base-asset quantity to match.
        quantity: u64,
        /// Optional price ceiling (Buy) / floor (Sell). `None` for
        /// market orders.
        limit_price: Option<u128>,
    },
    /// Quote-notional match (market-only).
    QuoteAmount {
        /// Total quote-asset value to consume from the book.
        amount: u128,
    },
}

impl MatchMode {
    /// Returns the limit-price guard used inside the level walk. `None`
    /// for any market path (base-qty market or quote-notional).
    #[inline]
    #[must_use]
    fn limit_price(&self) -> Option<u128> {
        match self {
            Self::BaseQty { limit_price, .. } => *limit_price,
            Self::QuoteAmount { .. } => None,
        }
    }

    /// Returns the initial `MatchResult` quantity slot. For quote-notional
    /// the actual base-qty filled is unknown upfront, so `u64::MAX` is
    /// used as a working upper bound during the loop (see
    /// `MatchResult::add_trade` invariants). The notional path is
    /// normalized at the end of `match_order_inner` so that the returned
    /// `MatchResult.remaining_quantity()` is `0` rather than the sentinel.
    #[inline]
    #[must_use]
    fn initial_match_quantity(&self) -> u64 {
        match self {
            Self::BaseQty { quantity, .. } => *quantity,
            Self::QuoteAmount { .. } => u64::MAX,
        }
    }
}

/// Tracks the matching loop's remaining budget against either base
/// quantity or quote notional. Designed to keep the base-qty hot path
/// allocation- and branch-light: the `BaseQty` arm of every helper is
/// a single arithmetic op the optimizer can fold.
#[derive(Debug, Clone)]
enum StopCondition {
    /// Base-quantity remaining.
    BaseQty {
        /// Base-asset quantity left to fill.
        remaining: u64,
    },
    /// Quote-notional remaining.
    QuoteAmount {
        /// Quote-asset value left to consume.
        remaining: u128,
    },
}

impl StopCondition {
    /// Build a fresh stop condition from the matching mode.
    #[inline]
    fn from_mode(mode: &MatchMode) -> Self {
        match mode {
            MatchMode::BaseQty { quantity, .. } => Self::BaseQty {
                remaining: *quantity,
            },
            MatchMode::QuoteAmount { amount } => Self::QuoteAmount { remaining: *amount },
        }
    }

    /// Per-level base-qty cap respecting `lot_size`. A return of `0`
    /// signals the caller to stop walking (dust below one full lot at
    /// the current level price).
    ///
    /// `lot <= 1` ⇒ no rounding (single arithmetic path); preserves the
    /// existing base-qty performance profile when lot enforcement is not
    /// configured.
    #[inline]
    #[must_use]
    fn level_qty_cap(&self, level_price: u128, lot: u64) -> u64 {
        let raw = match self {
            Self::BaseQty { remaining } => *remaining,
            Self::QuoteAmount { remaining } => {
                if level_price == 0 || *remaining < level_price {
                    return 0;
                }
                (*remaining / level_price).min(u128::from(u64::MAX)) as u64
            }
        };
        if lot <= 1 { raw } else { raw - (raw % lot) }
    }

    /// Decrement the remaining budget by what was actually executed at
    /// the given price.
    #[inline]
    fn consume(&mut self, executed_qty: u64, level_price: u128) {
        match self {
            Self::BaseQty { remaining } => {
                *remaining = remaining.saturating_sub(executed_qty);
            }
            Self::QuoteAmount { remaining } => {
                let spent = level_price.saturating_mul(u128::from(executed_qty));
                *remaining = remaining.saturating_sub(spent);
            }
        }
    }

    /// Returns `true` when no further fills are needed (budget exhausted).
    #[inline]
    #[must_use]
    fn is_done(&self) -> bool {
        match self {
            Self::BaseQty { remaining } => *remaining == 0,
            Self::QuoteAmount { remaining } => *remaining == 0,
        }
    }
}

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
        order_id: Id,
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
        order_id: Id,
        side: Side,
        quantity: u64,
        limit_price: Option<u128>,
        taker_user_id: Hash32,
    ) -> Result<MatchResult, OrderBookError> {
        self.match_order_inner(
            order_id,
            side,
            MatchMode::BaseQty {
                quantity,
                limit_price,
            },
            taker_user_id,
        )
    }

    /// Internal entry point for the quote-notional matching path.
    ///
    /// Public callers reach this through
    /// [`OrderBook::match_market_order_by_amount_with_user`]; the function
    /// here is the matching-loop seam that drives the unified inner loop
    /// with `MatchMode::QuoteAmount`. Always market-only — there is no
    /// `limit_price` analogue for notional orders.
    ///
    /// # Errors
    /// Returns [`OrderBookError::InsufficientLiquidityNotional`] when no
    /// liquidity could be consumed (empty book or budget below one full
    /// lot at every reachable level), or
    /// [`OrderBookError::SelfTradePrevented`] when STP cancels the taker
    /// before any fills occur.
    pub(crate) fn match_order_by_amount_with_user(
        &self,
        order_id: Id,
        side: Side,
        amount: u128,
        taker_user_id: Hash32,
    ) -> Result<MatchResult, OrderBookError> {
        self.match_order_inner(
            order_id,
            side,
            MatchMode::QuoteAmount { amount },
            taker_user_id,
        )
    }

    /// Unified matching loop driven by [`MatchMode`] / [`StopCondition`].
    ///
    /// One inner implementation handles both base-quantity and
    /// quote-notional walks. The `BaseQty` path is identical in shape to
    /// the previous implementation: `level_qty_cap` is a no-op for
    /// `lot <= 1` and a single `% lot` otherwise; `consume` is one
    /// `saturating_sub` per level. The `QuoteAmount` path adds one
    /// `u128` divide per level (to derive the per-level qty cap) and one
    /// `u128` multiply per fill (to deduct from the remaining notional).
    ///
    /// `lot_size`, when configured, is enforced uniformly: per-level qty
    /// is rounded **down** to a multiple of `lot`. This complements the
    /// admission-time validation in `modifications.rs` and ensures
    /// notional walks never emit `qty=0` trades when budget is below one
    /// full lot.
    fn match_order_inner(
        &self,
        order_id: Id,
        side: Side,
        mode: MatchMode,
        taker_user_id: Hash32,
    ) -> Result<MatchResult, OrderBookError> {
        self.cache.invalidate();
        let mut match_result =
            MatchResult::new(order_id, Quantity::new(mode.initial_match_quantity()));
        let mut stop = StopCondition::from_mode(&mode);
        let limit_price = mode.limit_price();
        let lot = self.lot_size.unwrap_or(1);
        // Deterministic taker timestamp for per-level matching: `pricelevel` 0.8's
        // `match_order` no longer reads the wall clock. Computed once so every trade
        // in this submit shares the taker's match time and replay stays deterministic.
        let taker_ts = self.clock().now_millis();

        // Determine if STP checks are needed for this match
        let stp_active = self.stp_mode.is_enabled() && taker_user_id != Hash32::zero();

        // Choose the appropriate side for matching
        let match_side = match side {
            Side::Buy => &self.asks,
            Side::Sell => &self.bids,
        };

        // Early exit if the opposite side is empty
        if match_side.is_empty() {
            return self.empty_book_result(side, &mode, match_result);
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
        let price_iter = match side {
            Side::Buy => Either::Left(match_side.iter()),
            Side::Sell => Either::Right(match_side.iter().rev()),
        };

        // Process each price level
        for entry in price_iter {
            let price = *entry.key();
            // Check price limit constraint early (only set for limit orders)
            if let Some(limit) = limit_price {
                match side {
                    Side::Buy if price > limit => break,
                    Side::Sell if price < limit => break,
                    _ => {}
                }
            }

            // Compute per-level base-qty cap respecting both the budget
            // (base-qty or notional) and `lot_size`. A zero cap means
            // dust-below-lot at the current price ⇒ stop walking.
            let qty_cap = stop.level_qty_cap(price, lot);
            if qty_cap == 0 {
                break;
            }

            // Get price level value from the entry
            let price_level = entry.value();

            // --- STP pre-processing ---
            // When STP is active, check for self-trade conflicts before matching.
            // This is done per-price-level to handle partial fills correctly.
            if stp_active {
                // `check_stp_at_level` needs the resting orders in the order the sweep
                // will consume them. `iter_orders()` is DashMap-backed (non-stable) and
                // makes `safe_quantity` / the CancelBoth `maker_order_id` non-deterministic,
                // which breaks replay (#94) — so use the deterministic `snapshot_orders()`.
                //
                // PRECONDITION: `snapshot_orders()` is `(timestamp, sequence)`-ordered,
                // whereas `match_order` consumes by pure insertion sequence; these coincide
                // only when timestamps are monotonic with insertion (the normal case). Under
                // non-monotonic timestamps the scan can diverge from the sweep — tracked in
                // #132 (needs an insertion-sequence accessor upstream, PriceLevel#102).
                let orders = price_level.snapshot_orders();
                let action = check_stp_at_level(&orders, taker_user_id, self.stp_mode);

                match action {
                    STPAction::NoConflict => {
                        // No self-trade at this level; match normally below
                    }

                    STPAction::CancelTaker { safe_quantity } => {
                        // Match up to safe_quantity, then cancel the taker
                        if safe_quantity > 0 {
                            let match_qty = qty_cap.min(safe_quantity);
                            if match_qty > 0 {
                                let price_level_match = price_level.match_order(
                                    match_qty,
                                    order_id,
                                    TimeInForce::Gtc,
                                    TakerKind::Standard,
                                    taker_ts,
                                    &self.transaction_id_generator,
                                );
                                let executed = match_qty.saturating_sub(
                                    price_level_match.remaining_quantity().as_u64(),
                                );
                                self.process_level_match(
                                    &mut match_result,
                                    &price_level_match,
                                    &mut filled_orders,
                                    price,
                                    price_level,
                                    side,
                                    &mut empty_price_levels,
                                );
                                stop.consume(executed, price);
                            }
                        }
                        stp_taker_cancelled = true;
                        break;
                    }

                    STPAction::CancelMaker { maker_order_ids } => {
                        // Cancel same-user resting orders, then match normally.
                        // Each cancel runs on the level we already hold — it emits the
                        // level-change event, records OrderStatus::Cancelled
                        // { SelfTradePrevention }, and releases the per-account risk slot
                        // in lockstep, but does NOT remove the level from the map (no
                        // order_locations re-resolution either), so level removal stays
                        // with the post-walk empty_price_levels drain (#95).
                        for maker_id in &maker_order_ids {
                            self.cancel_resting_maker_on_level(
                                price_level,
                                side.opposite(),
                                *maker_id,
                                CancelReason::SelfTradePrevention,
                            );
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
                            let match_qty = qty_cap.min(safe_quantity);
                            if match_qty > 0 {
                                let price_level_match = price_level.match_order(
                                    match_qty,
                                    order_id,
                                    TimeInForce::Gtc,
                                    TakerKind::Standard,
                                    taker_ts,
                                    &self.transaction_id_generator,
                                );
                                let executed = match_qty.saturating_sub(
                                    price_level_match.remaining_quantity().as_u64(),
                                );
                                self.process_level_match(
                                    &mut match_result,
                                    &price_level_match,
                                    &mut filled_orders,
                                    price,
                                    price_level,
                                    side,
                                    &mut empty_price_levels,
                                );
                                stop.consume(executed, price);
                            }
                        }
                        // Cancel the maker on the held level for the same lockstep
                        // event + state + risk effects as CancelMaker (#95); level
                        // removal stays with the empty_price_levels drain below.
                        self.cancel_resting_maker_on_level(
                            price_level,
                            side.opposite(),
                            maker_order_id,
                            CancelReason::SelfTradePrevention,
                        );
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
                qty_cap,
                order_id,
                TimeInForce::Gtc,
                TakerKind::Standard,
                taker_ts,
                &self.transaction_id_generator,
            );
            let executed = qty_cap.saturating_sub(price_level_match.remaining_quantity().as_u64());

            self.process_level_match(
                &mut match_result,
                &price_level_match,
                &mut filled_orders,
                price,
                price_level,
                side,
                &mut empty_price_levels,
            );
            stop.consume(executed, price);

            // Early exit if budget is exhausted
            if stop.is_done() {
                break;
            }
        }

        // Batch remove empty price levels
        let levels_removed = !empty_price_levels.is_empty();
        for price in &empty_price_levels {
            match_side.remove(price);
        }
        if levels_removed {
            // Refresh the operational depth gauges now that levels may
            // have been removed. No-op when the `metrics` feature is
            // disabled.
            self.record_depth_metric();
        }

        // Batch remove filled orders from tracking and update state. Each entry
        // carries the maker's TRUE filled quantity (captured per-level in
        // `process_level_match`), so OrderStateTracker / lifecycle consumers and
        // any audit/risk reconciliation that sums filled quantity from terminal
        // events see the real executed amount instead of a `0` placeholder (#104).
        for (filled_id, filled_quantity) in &filled_orders {
            self.track_state(
                *filled_id,
                OrderStatus::Filled {
                    filled_quantity: *filled_quantity,
                },
            );
            self.order_locations.remove(filled_id);
            self.untrack_order_by_id(filled_id);
        }

        // Return vectors to pool for reuse
        MATCHING_POOL.with(|pool| {
            pool.return_filled_orders_vec(filled_orders);
            pool.return_price_vec(empty_price_levels);
        });

        let no_fills = match_result.trades().as_vec().is_empty();

        // If STP cancelled the taker and no fills occurred at all, return STP error.
        // When partial fills happened, return Ok with the partial result so the
        // caller can see what was executed.
        if stp_taker_cancelled && no_fills {
            self.track_state(
                order_id,
                OrderStatus::Cancelled {
                    filled_quantity: 0,
                    reason: CancelReason::SelfTradePrevention,
                },
            );
            crate::orderbook::metrics::record_reject(
                crate::orderbook::reject_reason::RejectReason::SelfTradePrevention,
            );
            return Err(OrderBookError::SelfTradePrevented {
                mode: self.stp_mode,
                taker_order_id: order_id,
                user_id: taker_user_id,
            });
        }

        // Check for insufficient liquidity on market paths.
        if no_fills {
            match mode {
                MatchMode::BaseQty {
                    quantity,
                    limit_price: None,
                } => {
                    crate::orderbook::metrics::record_reject(
                        crate::orderbook::reject_reason::RejectReason::InsufficientLiquidity,
                    );
                    return Err(OrderBookError::InsufficientLiquidity {
                        side,
                        requested: quantity,
                        available: 0,
                    });
                }
                MatchMode::QuoteAmount { amount } => {
                    crate::orderbook::metrics::record_reject(
                        crate::orderbook::reject_reason::RejectReason::InsufficientLiquidity,
                    );
                    return Err(OrderBookError::InsufficientLiquidityNotional {
                        side,
                        requested: amount,
                        spent: 0,
                    });
                }
                MatchMode::BaseQty {
                    limit_price: Some(_),
                    ..
                } => {
                    // Limit orders that fail to match return Ok with an
                    // empty result — the unfilled portion becomes resting
                    // depth in the caller-driven flow.
                }
            }
        }

        // Normalize the quote-notional path so the public `MatchResult`
        // does not leak the `u64::MAX` working sentinel through
        // `remaining_quantity()`. The notional path measures progress in
        // quote currency, not base qty, so the natural meaning of
        // "remaining base qty" is zero — the residual the caller cares
        // about is `requested - executed_value`, available directly on
        // `MatchResult`.
        if matches!(mode, MatchMode::QuoteAmount { .. }) {
            match_result = Self::normalize_notional_match_result(order_id, match_result);
        }

        Ok(match_result)
    }

    /// Rebuild a `MatchResult` produced by the quote-notional path so its
    /// internal `remaining_quantity` is `0` (rather than
    /// `u64::MAX - executed_qty`). Trade list, filled-order ids, and
    /// monotonic engine sequence stamping are preserved.
    fn normalize_notional_match_result(order_id: Id, src: MatchResult) -> MatchResult {
        let executed_qty: u64 = src
            .trades()
            .as_vec()
            .iter()
            .map(|t| t.quantity().as_u64())
            .fold(0u64, u64::saturating_add);
        let mut rebuilt = MatchResult::new(order_id, Quantity::new(executed_qty));
        for trade in src.trades().as_vec() {
            // `add_trade` only fails on underflow; with `executed_qty`
            // exactly equal to the sum of trade quantities this cannot
            // underflow. Treat any error as a logic bug surfaced by
            // returning the original (unnormalized) result.
            if rebuilt.add_trade(*trade).is_err() {
                return src;
            }
        }
        for filled_id in src.filled_order_ids() {
            rebuilt.add_filled_order_id(*filled_id);
        }
        rebuilt
    }

    /// Build the empty-book result. Market paths return a typed error;
    /// limit paths return `Ok` with a zero-trade `MatchResult` so the
    /// caller can rest the order.
    #[cold]
    fn empty_book_result(
        &self,
        side: Side,
        mode: &MatchMode,
        match_result: MatchResult,
    ) -> Result<MatchResult, OrderBookError> {
        match mode {
            MatchMode::BaseQty {
                quantity,
                limit_price: None,
            } => {
                crate::orderbook::metrics::record_reject(
                    crate::orderbook::reject_reason::RejectReason::InsufficientLiquidity,
                );
                Err(OrderBookError::InsufficientLiquidity {
                    side,
                    requested: *quantity,
                    available: 0,
                })
            }
            MatchMode::QuoteAmount { amount } => {
                crate::orderbook::metrics::record_reject(
                    crate::orderbook::reject_reason::RejectReason::InsufficientLiquidity,
                );
                Err(OrderBookError::InsufficientLiquidityNotional {
                    side,
                    requested: *amount,
                    spent: 0,
                })
            }
            MatchMode::BaseQty {
                limit_price: Some(_),
                ..
            } => Ok(match_result),
        }
    }

    /// Processes match results from a single price level, updating the
    /// aggregate match result and bookkeeping vectors.
    ///
    /// Extracted to avoid code duplication between the normal path and
    /// the STP safe-quantity pre-match path. Routes outbound `engine_seq`
    /// stamping through [`OrderBook::next_engine_seq`] so the minting
    /// contract has a single source of truth.
    ///
    /// The book's installed `risk_state` is consulted on every trade so
    /// the maker's per-account `resting_notional` (and `open_count` on
    /// full fill) is decremented. The hook is a no-op when no
    /// `RiskConfig` is installed, matching the rest of the risk plumbing.
    #[allow(clippy::too_many_arguments)]
    fn process_level_match(
        &self,
        match_result: &mut MatchResult,
        price_level_match: &MatchResult,
        filled_orders: &mut Vec<(Id, u64)>,
        price: u128,
        price_level: &std::sync::Arc<pricelevel::PriceLevel>,
        side: Side,
        empty_price_levels: &mut Vec<u128>,
    ) {
        // Process trades if any occurred
        if !price_level_match.trades().as_vec().is_empty() {
            // Update last trade price atomically
            self.last_trade_price.store(price);
            self.has_traded.store(true, Ordering::Relaxed);

            // Add trades to result and update per-account risk counters
            // for the maker side of every trade.
            for trade in price_level_match.trades().as_vec() {
                // add_trade returns Result in v0.7; ignore error since
                // pricelevel already validated the quantities during matching
                let _ = match_result.add_trade(*trade);
                self.risk_state.on_fill(
                    trade.maker_order_id(),
                    trade.quantity().as_u64(),
                    trade.price().as_u128(),
                );
            }

            // Notify price level changes
            if let Some(listener) = &self.price_level_changed_listener {
                let engine_seq = self.next_engine_seq();
                listener(PriceLevelChangedEvent {
                    side: side.opposite(),
                    price: price_level.price(),
                    quantity: price_level.visible_quantity(),
                    engine_seq,
                });
            }
        }

        // Collect fully-consumed makers for batch removal, each with its true
        // filled quantity. Sum the maker's trades from THIS per-level result,
        // where `filled_order_ids()` and `trades()` are kept consistent by
        // pricelevel (an id is recorded only after its trade is added) — so the
        // recorded `Filled { filled_quantity }` stays correct even if an aggregate
        // `match_result.add_trade` were dropped (#104). Per-level trade counts are
        // small; this is the cold path, not the matching hot loop.
        for &filled_order_id in price_level_match.filled_order_ids() {
            match_result.add_filled_order_id(filled_order_id);
            let filled_quantity: u64 = price_level_match
                .trades()
                .as_vec()
                .iter()
                .filter(|trade| trade.maker_order_id() == filled_order_id)
                .map(|trade| trade.quantity().as_u64())
                .sum();
            filled_orders.push((filled_order_id, filled_quantity));
        }

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
        let price_iter = match side {
            Side::Buy => Either::Left(price_levels.iter()),
            Side::Sell => Either::Right(price_levels.iter().rev()),
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
            let available_quantity = price_level.total_quantity().unwrap_or(0);
            let needed_quantity = quantity.saturating_sub(matched_quantity);
            let quantity_to_match = needed_quantity.min(available_quantity);
            matched_quantity = matched_quantity.saturating_add(quantity_to_match);
        }

        matched_quantity
    }

    /// Batch operation for multiple order matches (additional optimization)
    pub fn match_orders_batch(
        &self,
        orders: &[(Id, Side, u64, Option<u128>)],
    ) -> Vec<Result<MatchResult, OrderBookError>> {
        let mut results = Vec::with_capacity(orders.len());

        for &(order_id, side, quantity, limit_price) in orders {
            let result = OrderBook::<T>::match_order(self, order_id, side, quantity, limit_price);
            results.push(result);
        }

        results
    }
}

#[cfg(test)]
mod stop_condition_tests {
    use super::*;

    #[test]
    fn test_base_qty_cap_no_lot_returns_remaining() {
        let stop = StopCondition::BaseQty { remaining: 1_000 };
        assert_eq!(stop.level_qty_cap(100, 1), 1_000);
    }

    #[test]
    fn test_base_qty_cap_rounds_down_to_lot() {
        let stop = StopCondition::BaseQty { remaining: 1_005 };
        // lot=100 ⇒ 1_005 - (1_005 % 100) = 1_005 - 5 = 1_000
        assert_eq!(stop.level_qty_cap(50, 100), 1_000);
    }

    #[test]
    fn test_base_qty_cap_zero_when_below_lot() {
        let stop = StopCondition::BaseQty { remaining: 5 };
        assert_eq!(stop.level_qty_cap(50, 100), 0);
    }

    #[test]
    fn test_quote_amount_cap_basic() {
        // 10_000 / 100 = 100 base
        let stop = StopCondition::QuoteAmount { remaining: 10_000 };
        assert_eq!(stop.level_qty_cap(100, 1), 100);
    }

    #[test]
    fn test_quote_amount_cap_dust_below_one_unit() {
        // remaining < level_price ⇒ 0
        let stop = StopCondition::QuoteAmount { remaining: 50 };
        assert_eq!(stop.level_qty_cap(100, 1), 0);
    }

    #[test]
    fn test_quote_amount_cap_lot_rounds_down() {
        // 1_400 / 100 = 14, lot=10 ⇒ 14 - (14 % 10) = 10
        let stop = StopCondition::QuoteAmount { remaining: 1_400 };
        assert_eq!(stop.level_qty_cap(100, 10), 10);
    }

    #[test]
    fn test_quote_amount_cap_zero_when_below_one_full_lot() {
        // 1_400 / 1_000 = 1, lot=10 ⇒ 1 - (1 % 10) = 0
        let stop = StopCondition::QuoteAmount { remaining: 1_400 };
        assert_eq!(stop.level_qty_cap(1_000, 10), 0);
    }

    #[test]
    fn test_quote_amount_cap_zero_price_is_zero_cap() {
        // Adversarial input: zero price should not divide-by-zero.
        let stop = StopCondition::QuoteAmount { remaining: 1_000 };
        assert_eq!(stop.level_qty_cap(0, 1), 0);
    }

    #[test]
    fn test_quote_amount_cap_saturates_to_u64_max() {
        // remaining = u128::MAX, level_price = 1 ⇒ derived qty would
        // exceed u64::MAX; must saturate at u64::MAX.
        let stop = StopCondition::QuoteAmount {
            remaining: u128::MAX,
        };
        assert_eq!(stop.level_qty_cap(1, 1), u64::MAX);
    }

    #[test]
    fn test_consume_base_qty_subtracts_executed() {
        let mut stop = StopCondition::BaseQty { remaining: 100 };
        stop.consume(30, 999);
        assert!(matches!(stop, StopCondition::BaseQty { remaining: 70 }));
    }

    #[test]
    fn test_consume_base_qty_saturates() {
        let mut stop = StopCondition::BaseQty { remaining: 5 };
        stop.consume(10, 999);
        assert!(matches!(stop, StopCondition::BaseQty { remaining: 0 }));
    }

    #[test]
    fn test_consume_quote_amount_deducts_price_times_qty() {
        let mut stop = StopCondition::QuoteAmount { remaining: 10_000 };
        stop.consume(30, 100); // spent = 100 * 30 = 3_000
        assert!(matches!(
            stop,
            StopCondition::QuoteAmount { remaining: 7_000 }
        ));
    }

    #[test]
    fn test_consume_quote_amount_saturates() {
        let mut stop = StopCondition::QuoteAmount { remaining: 100 };
        stop.consume(10, 1_000); // spent = 10_000 > 100, saturates
        assert!(matches!(stop, StopCondition::QuoteAmount { remaining: 0 }));
    }

    #[test]
    fn test_is_done_base_qty() {
        assert!(StopCondition::BaseQty { remaining: 0 }.is_done());
        assert!(!StopCondition::BaseQty { remaining: 1 }.is_done());
    }

    #[test]
    fn test_is_done_quote_amount() {
        assert!(StopCondition::QuoteAmount { remaining: 0 }.is_done());
        assert!(!StopCondition::QuoteAmount { remaining: 1 }.is_done());
    }
}
