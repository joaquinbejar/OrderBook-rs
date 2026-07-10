//! Order book operations like adding, modifying and canceling orders

use super::book::OrderBook;
use super::error::OrderBookError;
use super::trade::TradeResult;
use pricelevel::{Hash32, Id, MatchResult, OrderType, Price, Quantity, Side, TimeInForce};
use std::sync::Arc;
use tracing::trace;

impl<T> OrderBook<T>
where
    T: Clone + Send + Sync + Default + 'static,
{
    /// Add a limit order to the book.
    ///
    /// This convenience method sets `user_id` to `Hash32::zero()`.  When STP
    /// is enabled on this book, use [`Self::add_limit_order_with_user`] instead
    /// to supply the owner identity.
    ///
    /// `time_in_force` accepts a GTD deadline in Unix milliseconds — see
    /// [`Self::add_limit_order_with_user`] for full argument docs.
    ///
    /// # Errors
    /// Returns [`OrderBookError::MissingUserId`] when STP is enabled.
    pub fn add_limit_order(
        &self,
        id: Id,
        price: u128,
        quantity: u64,
        side: Side,
        time_in_force: TimeInForce,
        extra_fields: Option<T>,
    ) -> Result<Arc<OrderType<T>>, OrderBookError> {
        self.add_limit_order_with_user(
            id,
            price,
            quantity,
            side,
            time_in_force,
            Hash32::zero(),
            extra_fields,
        )
    }

    /// Add a limit order to the book with an explicit `user_id`.
    ///
    /// When Self-Trade Prevention (STP) is enabled, `user_id` must be non-zero
    /// so the matching engine can detect same-user conflicts.
    ///
    /// # Arguments
    /// * `id` — Unique order identifier.
    /// * `price` — Limit price.
    /// * `quantity` — Order quantity.
    /// * `side` — Buy or Sell.
    /// * `time_in_force` — Time-in-force policy (GTD deadline in Unix milliseconds).
    /// * `user_id` — Owner identity for STP checks.
    /// * `extra_fields` — Optional application-specific payload.
    ///
    /// # Errors
    /// Returns [`OrderBookError::MissingUserId`] when STP is enabled and
    /// `user_id` is `Hash32::zero()`.
    #[allow(clippy::too_many_arguments)]
    pub fn add_limit_order_with_user(
        &self,
        id: Id,
        price: u128,
        quantity: u64,
        side: Side,
        time_in_force: TimeInForce,
        user_id: Hash32,
        extra_fields: Option<T>,
    ) -> Result<Arc<OrderType<T>>, OrderBookError> {
        // Top-of-fn kill-switch gate so we skip the clock read and
        // extra_fields / OrderType construction below when halted.
        self.check_kill_switch_or_reject(id)?;
        let extra_fields: T = extra_fields.unwrap_or_default();
        let order = OrderType::Standard {
            id,
            price: Price::new(price),
            quantity: Quantity::new(quantity),
            side,
            user_id,
            timestamp: self.clock().now_millis(),
            time_in_force,
            extra_fields,
        };
        trace!(
            "Adding limit order {} {} {} {} {}",
            id, price, quantity, side, time_in_force
        );
        self.add_order(order)
    }

    /// Add a limit order to the book and return the [`TradeResult`] produced by
    /// the match directly to the caller.
    ///
    /// The result-returning counterpart of [`Self::add_limit_order`]: it builds
    /// the same `Standard` order and routes through
    /// [`Self::add_order_with_result`], so the returned tuple carries the
    /// resting order plus `Some(TradeResult)` when the order produced fills, or
    /// `None` when it rested without matching. This convenience method sets
    /// `user_id` to `Hash32::zero()`; when STP is enabled use
    /// [`Self::add_limit_order_with_user_and_result`] instead.
    ///
    /// Per-call attribution: the `TradeResult` is built from this call's own
    /// private match outcome, never from shared capture state, so concurrent
    /// submits on the same book each receive exactly their own fills.
    ///
    /// `time_in_force` accepts a GTD deadline in Unix milliseconds — see
    /// [`Self::add_limit_order_with_user_and_result`] for full argument docs.
    ///
    /// # Errors
    /// Returns [`OrderBookError::MissingUserId`] when STP is enabled.
    pub fn add_limit_order_with_result(
        &self,
        id: Id,
        price: u128,
        quantity: u64,
        side: Side,
        time_in_force: TimeInForce,
        extra_fields: Option<T>,
    ) -> Result<(Arc<OrderType<T>>, Option<TradeResult>), OrderBookError> {
        self.add_limit_order_with_user_and_result(
            id,
            price,
            quantity,
            side,
            time_in_force,
            Hash32::zero(),
            extra_fields,
        )
    }

    /// Add a limit order to the book with an explicit `user_id` and return the
    /// [`TradeResult`] produced by the match directly to the caller.
    ///
    /// The result-returning counterpart of [`Self::add_limit_order_with_user`]:
    /// it builds the same `Standard` order and routes through
    /// [`Self::add_order_with_result`]. The returned tuple carries the resting
    /// order plus `Some(TradeResult)` when the order produced fills, or `None`
    /// when it rested without matching. When a trade listener is installed it
    /// still fires with the exact same `TradeResult` (same fills, same fees,
    /// same `engine_seq`).
    ///
    /// Per-call attribution: the `TradeResult` is built from this call's own
    /// private match outcome, never from shared capture state, so concurrent
    /// submits on the same book each receive exactly their own fills.
    ///
    /// # Arguments
    /// * `id` — Unique order identifier.
    /// * `price` — Limit price.
    /// * `quantity` — Order quantity.
    /// * `side` — Buy or Sell.
    /// * `time_in_force` — Time-in-force policy (GTD deadline in Unix milliseconds).
    /// * `user_id` — Owner identity for STP checks.
    /// * `extra_fields` — Optional application-specific payload.
    ///
    /// # Errors
    /// Returns [`OrderBookError::MissingUserId`] when STP is enabled and
    /// `user_id` is `Hash32::zero()`.
    /// On error paths that follow real fills (an unfillable IOC remainder, or a
    /// self-trade-prevention cancellation after earlier non-self fills) the
    /// typed error is returned instead, so those fills reach the trade listener
    /// only.
    #[allow(clippy::too_many_arguments)]
    pub fn add_limit_order_with_user_and_result(
        &self,
        id: Id,
        price: u128,
        quantity: u64,
        side: Side,
        time_in_force: TimeInForce,
        user_id: Hash32,
        extra_fields: Option<T>,
    ) -> Result<(Arc<OrderType<T>>, Option<TradeResult>), OrderBookError> {
        // Top-of-fn kill-switch gate so we skip the clock read and
        // extra_fields / OrderType construction below when halted.
        self.check_kill_switch_or_reject(id)?;
        let extra_fields: T = extra_fields.unwrap_or_default();
        let order = OrderType::Standard {
            id,
            price: Price::new(price),
            quantity: Quantity::new(quantity),
            side,
            user_id,
            timestamp: self.clock().now_millis(),
            time_in_force,
            extra_fields,
        };
        trace!(
            "Adding limit order (with result) {} {} {} {} {}",
            id, price, quantity, side, time_in_force
        );
        self.add_order_with_result(order)
    }

    /// Add an iceberg order to the book.
    ///
    /// This convenience method sets `user_id` to `Hash32::zero()`.  When STP
    /// is enabled, use [`Self::add_iceberg_order_with_user`] instead.
    ///
    /// `time_in_force` accepts a GTD deadline in Unix milliseconds — see
    /// [`Self::add_iceberg_order_with_user`] for full argument docs.
    ///
    /// # Errors
    /// Returns [`OrderBookError::MissingUserId`] when STP is enabled.
    #[allow(clippy::too_many_arguments)]
    pub fn add_iceberg_order(
        &self,
        id: Id,
        price: u128,
        visible_quantity: u64,
        hidden_quantity: u64,
        side: Side,
        time_in_force: TimeInForce,
        extra_fields: Option<T>,
    ) -> Result<Arc<OrderType<T>>, OrderBookError> {
        self.add_iceberg_order_with_user(
            id,
            price,
            visible_quantity,
            hidden_quantity,
            side,
            time_in_force,
            Hash32::zero(),
            extra_fields,
        )
    }

    /// Add an iceberg order to the book with an explicit `user_id`.
    ///
    /// # Arguments
    /// * `id` — Unique order identifier.
    /// * `price` — Limit price.
    /// * `visible_quantity` — Displayed quantity.
    /// * `hidden_quantity` — Hidden (reserve) quantity.
    /// * `side` — Buy or Sell.
    /// * `time_in_force` — Time-in-force policy (GTD deadline in Unix milliseconds).
    /// * `user_id` — Owner identity for STP checks.
    /// * `extra_fields` — Optional application-specific payload.
    ///
    /// # Errors
    /// Returns [`OrderBookError::MissingUserId`] when STP is enabled and
    /// `user_id` is `Hash32::zero()`.
    #[allow(clippy::too_many_arguments)]
    pub fn add_iceberg_order_with_user(
        &self,
        id: Id,
        price: u128,
        visible_quantity: u64,
        hidden_quantity: u64,
        side: Side,
        time_in_force: TimeInForce,
        user_id: Hash32,
        extra_fields: Option<T>,
    ) -> Result<Arc<OrderType<T>>, OrderBookError> {
        // Top-of-fn kill-switch gate so we skip the clock read and
        // extra_fields / OrderType construction below when halted.
        self.check_kill_switch_or_reject(id)?;
        let extra_fields: T = extra_fields.unwrap_or_default();
        let order = OrderType::IcebergOrder {
            id,
            price: Price::new(price),
            visible_quantity: Quantity::new(visible_quantity),
            hidden_quantity: Quantity::new(hidden_quantity),
            side,
            user_id,
            timestamp: self.clock().now_millis(),
            time_in_force,
            extra_fields,
        };
        trace!(
            "Adding iceberg order {} {} {} {} {}",
            id, price, visible_quantity, hidden_quantity, side
        );
        self.add_order(order)
    }

    /// Add a post-only order to the book.
    ///
    /// This convenience method sets `user_id` to `Hash32::zero()`.  When STP
    /// is enabled, use [`Self::add_post_only_order_with_user`] instead.
    ///
    /// `time_in_force` accepts a GTD deadline in Unix milliseconds — see
    /// [`Self::add_post_only_order_with_user`] for full argument docs.
    ///
    /// # Errors
    /// Returns [`OrderBookError::MissingUserId`] when STP is enabled.
    pub fn add_post_only_order(
        &self,
        id: Id,
        price: u128,
        quantity: u64,
        side: Side,
        time_in_force: TimeInForce,
        extra_fields: Option<T>,
    ) -> Result<Arc<OrderType<T>>, OrderBookError> {
        self.add_post_only_order_with_user(
            id,
            price,
            quantity,
            side,
            time_in_force,
            Hash32::zero(),
            extra_fields,
        )
    }

    /// Add a post-only order to the book with an explicit `user_id`.
    ///
    /// # Arguments
    /// * `id` — Unique order identifier.
    /// * `price` — Limit price.
    /// * `quantity` — Order quantity.
    /// * `side` — Buy or Sell.
    /// * `time_in_force` — Time-in-force policy (GTD deadline in Unix milliseconds).
    /// * `user_id` — Owner identity for STP checks.
    /// * `extra_fields` — Optional application-specific payload.
    ///
    /// # Errors
    /// Returns [`OrderBookError::MissingUserId`] when STP is enabled and
    /// `user_id` is `Hash32::zero()`.
    #[allow(clippy::too_many_arguments)]
    pub fn add_post_only_order_with_user(
        &self,
        id: Id,
        price: u128,
        quantity: u64,
        side: Side,
        time_in_force: TimeInForce,
        user_id: Hash32,
        extra_fields: Option<T>,
    ) -> Result<Arc<OrderType<T>>, OrderBookError> {
        // Top-of-fn kill-switch gate so we skip the clock read and
        // extra_fields / OrderType construction below when halted.
        self.check_kill_switch_or_reject(id)?;
        let extra_fields: T = extra_fields.unwrap_or_default();
        let order = OrderType::PostOnly {
            id,
            price: Price::new(price),
            quantity: Quantity::new(quantity),
            side,
            user_id,
            timestamp: self.clock().now_millis(),
            time_in_force,
            extra_fields,
        };
        trace!(
            "Adding post-only order {} {} {} {} {}",
            id, price, quantity, side, time_in_force
        );
        self.add_order(order)
    }

    /// Submit a simple market order.
    ///
    /// This convenience method bypasses STP (uses `Hash32::zero()`).
    /// Use [`Self::submit_market_order_with_user`] when STP is needed.
    ///
    /// # Errors
    /// Returns [`OrderBookError::KillSwitchActive`] when the kill switch
    /// is engaged. The check happens at the top of the function before
    /// any matching, fee, or STP work.
    pub fn submit_market_order(
        &self,
        id: Id,
        quantity: u64,
        side: Side,
    ) -> Result<MatchResult, OrderBookError> {
        self.check_kill_switch_or_reject(id)?;
        // Pre-trade risk gate. Per design decision C, market orders
        // currently bypass every check (no submitted price; no rest);
        // the call exists to keep the gate ordering consistent across
        // submit and add paths.
        self.risk_state.check_market_admission(Hash32::zero())?;
        trace!("Submitting market order {} {} {}", id, quantity, side);
        OrderBook::<T>::match_market_order(self, id, quantity, side)
    }

    /// Submit a market order with Self-Trade Prevention support.
    ///
    /// When STP is enabled and `user_id` is non-zero, the matching engine
    /// checks resting orders for same-user conflicts before executing fills.
    ///
    /// # Arguments
    /// * `id` — Unique identifier for this market order.
    /// * `quantity` — Quantity to match.
    /// * `side` — Buy or Sell.
    /// * `user_id` — Owner of the incoming order for STP checks.
    ///   Pass `Hash32::zero()` to bypass STP.
    ///
    /// # Errors
    /// Returns [`OrderBookError::SelfTradePrevented`] when STP cancels the
    /// taker before any fills occur. Returns
    /// [`OrderBookError::KillSwitchActive`] when the kill switch is
    /// engaged; the check happens at the top of the function before any
    /// matching, fee, or STP work.
    pub fn submit_market_order_with_user(
        &self,
        id: Id,
        quantity: u64,
        side: Side,
        user_id: Hash32,
    ) -> Result<MatchResult, OrderBookError> {
        self.check_kill_switch_or_reject(id)?;
        // Pre-trade risk gate. Per design decision C, market orders
        // currently bypass every check; the call exists to keep the
        // gate ordering consistent across submit and add paths.
        self.risk_state.check_market_admission(user_id)?;
        trace!(
            "Submitting market order {} {} {} (user: {})",
            id, quantity, side, user_id
        );
        OrderBook::<T>::match_market_order_with_user(self, id, quantity, side, user_id)
    }

    /// Submit a quote-notional market order.
    ///
    /// Convenience wrapper around
    /// [`OrderBook::match_market_order_by_amount`] that runs the kill
    /// switch and pre-trade risk gates before matching. Bypasses STP
    /// (uses `Hash32::zero()`); use
    /// [`Self::submit_market_order_by_amount_with_user`] when STP is
    /// needed.
    ///
    /// # Errors
    /// Returns [`OrderBookError::KillSwitchActive`] when the kill switch
    /// is engaged. Propagates [`OrderBookError::InsufficientLiquidityNotional`]
    /// from the matching engine when no liquidity is available.
    pub fn submit_market_order_by_amount(
        &self,
        id: Id,
        amount: u128,
        side: Side,
    ) -> Result<MatchResult, OrderBookError> {
        self.check_kill_switch_or_reject(id)?;
        // Pre-trade risk gate. Per design decision C, market orders
        // currently bypass every check (no submitted price; no rest);
        // the call exists to keep the gate ordering consistent across
        // submit and add paths.
        self.risk_state.check_market_admission(Hash32::zero())?;
        trace!(
            "Submitting notional market order {} amount={} {}",
            id, amount, side
        );
        OrderBook::<T>::match_market_order_by_amount(self, id, amount, side)
    }

    /// Submit a quote-notional market order with Self-Trade Prevention.
    ///
    /// See [`Self::submit_market_order_by_amount`] for the amount / lot /
    /// fee semantics.
    ///
    /// # Errors
    /// Returns [`OrderBookError::SelfTradePrevented`] when STP cancels
    /// the taker before any fills occur. Returns
    /// [`OrderBookError::KillSwitchActive`] when the kill switch is
    /// engaged. Returns [`OrderBookError::InsufficientLiquidityNotional`]
    /// when the book had zero matchable depth.
    pub fn submit_market_order_by_amount_with_user(
        &self,
        id: Id,
        amount: u128,
        side: Side,
        user_id: Hash32,
    ) -> Result<MatchResult, OrderBookError> {
        self.check_kill_switch_or_reject(id)?;
        self.risk_state.check_market_admission(user_id)?;
        trace!(
            "Submitting notional market order {} amount={} {} (user: {})",
            id, amount, side, user_id
        );
        OrderBook::<T>::match_market_order_by_amount_with_user(self, id, amount, side, user_id)
    }
}
