//! Order book operations like adding, modifying and canceling orders

use super::book::OrderBook;
use super::error::OrderBookError;
use pricelevel::{
    Hash32, Id, MatchResult, OrderType, Price, Quantity, Side, TimeInForce, TimestampMs,
};
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
    /// * `time_in_force` — Time-in-force policy.
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
        let extra_fields: T = extra_fields.unwrap_or_default();
        let order = OrderType::Standard {
            id,
            price: Price::new(price),
            quantity: Quantity::new(quantity),
            side,
            user_id,
            timestamp: TimestampMs::new(crate::utils::current_time_millis()),
            time_in_force,
            extra_fields,
        };
        trace!(
            "Adding limit order {} {} {} {} {}",
            id, price, quantity, side, time_in_force
        );
        self.add_order(order)
    }

    /// Add an iceberg order to the book.
    ///
    /// This convenience method sets `user_id` to `Hash32::zero()`.  When STP
    /// is enabled, use [`Self::add_iceberg_order_with_user`] instead.
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
    /// * `time_in_force` — Time-in-force policy.
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
        let extra_fields: T = extra_fields.unwrap_or_default();
        let order = OrderType::IcebergOrder {
            id,
            price: Price::new(price),
            visible_quantity: Quantity::new(visible_quantity),
            hidden_quantity: Quantity::new(hidden_quantity),
            side,
            user_id,
            timestamp: TimestampMs::new(crate::utils::current_time_millis()),
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
    /// * `time_in_force` — Time-in-force policy.
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
        let extra_fields: T = extra_fields.unwrap_or_default();
        let order = OrderType::PostOnly {
            id,
            price: Price::new(price),
            quantity: Quantity::new(quantity),
            side,
            user_id,
            timestamp: TimestampMs::new(crate::utils::current_time_millis()),
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
    pub fn submit_market_order(
        &self,
        id: Id,
        quantity: u64,
        side: Side,
    ) -> Result<MatchResult, OrderBookError> {
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
    /// taker before any fills occur.
    pub fn submit_market_order_with_user(
        &self,
        id: Id,
        quantity: u64,
        side: Side,
        user_id: Hash32,
    ) -> Result<MatchResult, OrderBookError> {
        trace!(
            "Submitting market order {} {} {} (user: {})",
            id, quantity, side, user_id
        );
        OrderBook::<T>::match_market_order_with_user(self, id, quantity, side, user_id)
    }
}
