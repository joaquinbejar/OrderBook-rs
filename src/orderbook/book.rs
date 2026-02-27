//! Core OrderBook implementation for managing price levels and orders

use super::cache::PriceLevelCache;
use super::error::OrderBookError;
use super::fees::FeeSchedule;
use super::iterators::{LevelInfo, LevelsInRange, LevelsUntilDepth, LevelsWithCumulativeDepth};
use super::market_impact::{MarketImpact, OrderSimulation};
use super::snapshot::{EnrichedSnapshot, MetricFlags, OrderBookSnapshot, OrderBookSnapshotPackage};
use super::statistics::{DepthStats, DistributionBin};
use crate::orderbook::book_change_event::PriceLevelChangedListener;
#[cfg(feature = "special_orders")]
use crate::orderbook::repricing::SpecialOrderTracker;
use crate::orderbook::stp::STPMode;
use crate::orderbook::trade::{TradeListener, TradeResult};
use crate::utils::current_time_millis;
use crossbeam::atomic::AtomicCell;
use crossbeam_skiplist::SkipMap;
use dashmap::DashMap;
#[cfg(feature = "special_orders")]
use pricelevel::OrderUpdate;
use pricelevel::{Hash32, Id, MatchResult, OrderType, PriceLevel, Side, UuidGenerator};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap};
use std::marker::PhantomData;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use tracing::trace;
use uuid::Uuid;

/// Default basis points multiplier for spread calculations
/// One basis point = 0.01% = 0.0001
const DEFAULT_BASIS_POINTS_MULTIPLIER: f64 = 10_000.0;

/// The OrderBook manages a collection of price levels for both bid and ask sides.
/// It supports adding, cancelling, and matching orders with lock-free operations where possible.
pub struct OrderBook<T = ()> {
    /// The symbol or identifier for this order book
    pub(super) symbol: String,

    /// Bid side price levels (buy orders), stored in a concurrent ordered map (skip list)
    /// The map is keyed by price levels and stores Arc references to PriceLevel instances
    /// Using SkipMap provides O(log N) operations with automatic ordering, eliminating
    /// the need to sort prices during matching (optimization from O(N log N) to O(M log N))
    pub(super) bids: SkipMap<u128, Arc<PriceLevel>>,

    /// Ask side price levels (sell orders), stored in a concurrent ordered map (skip list)
    /// The map is keyed by price levels and stores Arc references to PriceLevel instances
    /// Using SkipMap provides O(log N) operations with automatic ordering, eliminating
    /// the need to sort prices during matching (optimization from O(N log N) to O(M log N))
    pub(super) asks: SkipMap<u128, Arc<PriceLevel>>,

    /// A concurrent map from order ID to (price, side) for fast lookups
    /// This avoids having to search through all price levels to find an order
    pub(super) order_locations: DashMap<Id, (u128, Side)>,

    /// A concurrent map from user ID to their order IDs for fast lookup.
    /// Maintained by `add_order`, `cancel_order`, and the matching engine
    /// to enable O(1) user-based mass cancellation.
    pub(super) user_orders: DashMap<Hash32, Vec<Id>>,

    /// Generator for unique transaction IDs
    pub(super) transaction_id_generator: UuidGenerator,

    /// Counter for generating sequential order IDs
    #[allow(dead_code)]
    pub(super) next_order_id: AtomicU64,

    /// The last price at which a trade occurred
    pub(super) last_trade_price: AtomicCell<u128>,

    /// Flag indicating if there was a trade
    pub(super) has_traded: AtomicBool,

    /// The timestamp of market close, if applicable (for DAY orders)
    pub(super) market_close_timestamp: AtomicU64,

    /// Flag indicating if market close is set
    pub(super) has_market_close: AtomicBool,

    /// A cache for storing best bid/ask prices to avoid recalculation
    pub(super) cache: PriceLevelCache,

    /// listens to possible trades when an order is added
    pub trade_listener: Option<TradeListener>,

    /// Phantom data to maintain generic type parameter
    _phantom: PhantomData<T>,

    /// listens to order book changes. This provides a point to update a corresponding external order book e.g. in the UI
    pub price_level_changed_listener: Option<PriceLevelChangedListener>,

    /// Tracker for special orders that require re-pricing (PeggedOrder and TrailingStop)
    #[cfg(feature = "special_orders")]
    pub(super) special_order_tracker: SpecialOrderTracker,

    /// Minimum price increment for orders. When set, order prices must be
    /// exact multiples of this value. `None` disables validation (default).
    pub(super) tick_size: Option<u128>,

    /// Minimum quantity increment for orders. When set, order quantities must be
    /// exact multiples of this value. `None` disables validation (default).
    pub(super) lot_size: Option<u64>,

    /// Minimum order size. When set, orders with `total_quantity() < min` are
    /// rejected. `None` disables validation (default).
    pub(super) min_order_size: Option<u64>,

    /// Maximum order size. When set, orders with `total_quantity() > max` are
    /// rejected. `None` disables validation (default).
    pub(super) max_order_size: Option<u64>,

    /// Self-Trade Prevention mode. When set to a mode other than `None`,
    /// the matching engine checks `user_id` on incoming and resting orders
    /// to prevent self-trades. Default is `STPMode::None` (disabled).
    pub(super) stp_mode: STPMode,

    /// Fee schedule for calculating trading fees. When None, no fees are applied.
    /// Fees are calculated during trade execution and can be configured per orderbook.
    pub(super) fee_schedule: Option<FeeSchedule>,
}

impl<T> Serialize for OrderBook<T>
where
    T: Serialize,
{
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        use std::collections::HashMap;
        use std::sync::atomic::Ordering;

        let mut state = serializer.serialize_struct("OrderBook", 10)?;

        // Serialize symbol
        state.serialize_field("symbol", &self.symbol)?;

        // Serialize bids as HashMap<u128, PriceLevel> using snapshots
        let bids: HashMap<u128, _> = self
            .bids
            .iter()
            .map(|entry| (*entry.key(), entry.value().snapshot()))
            .collect();
        state.serialize_field("bids", &bids)?;

        // Serialize asks as HashMap<u128, PriceLevel> using snapshots
        let asks: HashMap<u128, _> = self
            .asks
            .iter()
            .map(|entry| (*entry.key(), entry.value().snapshot()))
            .collect();
        state.serialize_field("asks", &asks)?;

        // Serialize order_locations as HashMap
        let order_locations: HashMap<Id, (u128, Side)> = self
            .order_locations
            .iter()
            .map(|entry| (*entry.key(), *entry.value()))
            .collect();
        state.serialize_field("order_locations", &order_locations)?;

        // Serialize atomic values by loading them
        state.serialize_field("last_trade_price", &self.last_trade_price.load())?;
        state.serialize_field("has_traded", &self.has_traded.load(Ordering::Relaxed))?;
        state.serialize_field(
            "market_close_timestamp",
            &self.market_close_timestamp.load(Ordering::Relaxed),
        )?;
        state.serialize_field(
            "has_market_close",
            &self.has_market_close.load(Ordering::Relaxed),
        )?;

        // Serialize cache
        state.serialize_field("cache", &self.cache)?;

        // Serialize fee schedule
        state.serialize_field("fee_schedule", &self.fee_schedule)?;

        // Skip trade_listener (cannot be serialized) and transaction_id_generator, _phantom

        state.end()
    }
}

impl<T> OrderBook<T>
where
    T: Default + Clone + Send + Sync + 'static,
{
    /// Convert OrderType<()> to `OrderType<T>` for return values
    pub fn convert_from_unit_type(&self, order: &OrderType<()>) -> OrderType<T>
    where
        T: Default,
    {
        match order {
            OrderType::Standard {
                id,
                price,
                quantity,
                side,
                user_id,
                timestamp,
                time_in_force,
                ..
            } => OrderType::Standard {
                id: *id,
                price: *price,
                quantity: *quantity,
                side: *side,
                user_id: *user_id,
                timestamp: *timestamp,
                time_in_force: *time_in_force,
                extra_fields: T::default(),
            },
            OrderType::IcebergOrder {
                id,
                price,
                visible_quantity,
                hidden_quantity,
                side,
                user_id,
                timestamp,
                time_in_force,
                ..
            } => OrderType::IcebergOrder {
                id: *id,
                price: *price,
                visible_quantity: *visible_quantity,
                hidden_quantity: *hidden_quantity,
                side: *side,
                user_id: *user_id,
                timestamp: *timestamp,
                time_in_force: *time_in_force,
                extra_fields: T::default(),
            },
            OrderType::PostOnly {
                id,
                price,
                quantity,
                side,
                user_id,
                timestamp,
                time_in_force,
                ..
            } => OrderType::PostOnly {
                id: *id,
                price: *price,
                quantity: *quantity,
                side: *side,
                user_id: *user_id,
                timestamp: *timestamp,
                time_in_force: *time_in_force,
                extra_fields: T::default(),
            },
            OrderType::TrailingStop {
                id,
                price,
                quantity,
                side,
                user_id,
                timestamp,
                time_in_force,
                trail_amount,
                last_reference_price,
                ..
            } => OrderType::TrailingStop {
                id: *id,
                price: *price,
                quantity: *quantity,
                side: *side,
                user_id: *user_id,
                timestamp: *timestamp,
                time_in_force: *time_in_force,
                trail_amount: *trail_amount,
                last_reference_price: *last_reference_price,
                extra_fields: T::default(),
            },
            OrderType::PeggedOrder {
                id,
                price,
                quantity,
                side,
                user_id,
                timestamp,
                time_in_force,
                reference_price_offset,
                reference_price_type,
                ..
            } => OrderType::PeggedOrder {
                id: *id,
                price: *price,
                quantity: *quantity,
                side: *side,
                user_id: *user_id,
                timestamp: *timestamp,
                time_in_force: *time_in_force,
                reference_price_offset: *reference_price_offset,
                reference_price_type: *reference_price_type,
                extra_fields: T::default(),
            },
            OrderType::MarketToLimit {
                id,
                price,
                quantity,
                side,
                user_id,
                timestamp,
                time_in_force,
                ..
            } => OrderType::MarketToLimit {
                id: *id,
                price: *price,
                quantity: *quantity,
                side: *side,
                user_id: *user_id,
                timestamp: *timestamp,
                time_in_force: *time_in_force,
                extra_fields: T::default(),
            },
            OrderType::ReserveOrder {
                id,
                price,
                visible_quantity,
                hidden_quantity,
                side,
                user_id,
                timestamp,
                time_in_force,
                replenish_threshold,
                replenish_amount,
                auto_replenish,
                ..
            } => OrderType::ReserveOrder {
                id: *id,
                price: *price,
                visible_quantity: *visible_quantity,
                hidden_quantity: *hidden_quantity,
                side: *side,
                user_id: *user_id,
                timestamp: *timestamp,
                time_in_force: *time_in_force,
                replenish_threshold: *replenish_threshold,
                replenish_amount: *replenish_amount,
                auto_replenish: *auto_replenish,
                extra_fields: T::default(),
            },
        }
    }
    /// Create a new order book for the given symbol
    pub fn new(symbol: &str) -> Self {
        // Create a unique namespace for this order book's transaction IDs
        let namespace = Uuid::new_v4();

        Self {
            symbol: symbol.to_string(),
            bids: SkipMap::new(),
            asks: SkipMap::new(),
            order_locations: DashMap::new(),
            user_orders: DashMap::new(),
            transaction_id_generator: UuidGenerator::new(namespace),
            next_order_id: AtomicU64::new(1),
            last_trade_price: AtomicCell::new(0),
            has_traded: AtomicBool::new(false),
            market_close_timestamp: AtomicU64::new(0),
            has_market_close: AtomicBool::new(false),
            cache: PriceLevelCache::new(),
            trade_listener: None,
            _phantom: PhantomData,
            price_level_changed_listener: None,
            #[cfg(feature = "special_orders")]
            special_order_tracker: SpecialOrderTracker::new(),
            tick_size: None,
            lot_size: None,
            min_order_size: None,
            max_order_size: None,
            stp_mode: STPMode::None,
            fee_schedule: None,
        }
    }

    /// Create a new order book for the given symbol with tick size validation.
    ///
    /// Orders added to this book must have prices that are exact multiples
    /// of `tick_size`. For example, with `tick_size = 100`, prices 100, 200,
    /// 300 are valid but 150 is rejected.
    ///
    /// # Arguments
    /// - `symbol`: The trading symbol for this order book
    /// - `tick_size`: Minimum price increment. Must be > 0
    ///
    /// # Returns
    /// A new `OrderBook` instance with tick size validation enabled
    pub fn with_tick_size(symbol: &str, tick_size: u128) -> Self {
        let mut book = Self::new(symbol);
        book.tick_size = Some(tick_size);
        book
    }

    /// Create a new order book for the given symbol with lot size validation.
    ///
    /// Orders added to this book must have quantities that are exact multiples
    /// of `lot_size`. For iceberg orders, both visible and hidden quantities
    /// are validated individually.
    ///
    /// # Arguments
    /// - `symbol`: The trading symbol for this order book
    /// - `lot_size`: Minimum quantity increment. Must be > 0
    ///
    /// # Returns
    /// A new `OrderBook` instance with lot size validation enabled
    pub fn with_lot_size(symbol: &str, lot_size: u64) -> Self {
        let mut book = Self::new(symbol);
        book.lot_size = Some(lot_size);
        book
    }

    /// Create a new order book for the given symbol with a trade listener
    pub fn with_trade_listener(symbol: &str, trade_listener: TradeListener) -> Self {
        let namespace = Uuid::new_v4();

        Self {
            symbol: symbol.to_string(),
            bids: SkipMap::new(),
            asks: SkipMap::new(),
            order_locations: DashMap::new(),
            user_orders: DashMap::new(),
            transaction_id_generator: UuidGenerator::new(namespace),
            next_order_id: AtomicU64::new(1),
            last_trade_price: AtomicCell::new(0),
            has_traded: AtomicBool::new(false),
            market_close_timestamp: AtomicU64::new(0),
            has_market_close: AtomicBool::new(false),
            cache: PriceLevelCache::new(),
            trade_listener: Some(trade_listener),
            _phantom: PhantomData,
            price_level_changed_listener: None,
            #[cfg(feature = "special_orders")]
            special_order_tracker: SpecialOrderTracker::new(),
            tick_size: None,
            lot_size: None,
            min_order_size: None,
            max_order_size: None,
            stp_mode: STPMode::None,
            fee_schedule: None,
        }
    }

    /// Creates a new order book with both a trade listener and a price level change listener.
    ///
    /// # Arguments
    /// - `symbol`: The trading symbol for this order book
    /// - `trade_listener`: Callback invoked when trades are executed
    /// - `book_changed_listener`: Callback invoked when price levels change
    ///
    /// # Returns
    /// A new `OrderBook` instance with both listeners configured
    pub fn with_trade_and_price_level_listener(
        symbol: &str,
        trade_listener: TradeListener,
        book_changed_listener: PriceLevelChangedListener,
    ) -> Self {
        let namespace = Uuid::new_v4();

        Self {
            symbol: symbol.to_string(),
            bids: SkipMap::new(),
            asks: SkipMap::new(),
            order_locations: DashMap::new(),
            user_orders: DashMap::new(),
            transaction_id_generator: UuidGenerator::new(namespace),
            next_order_id: AtomicU64::new(1),
            last_trade_price: AtomicCell::new(0),
            has_traded: AtomicBool::new(false),
            market_close_timestamp: AtomicU64::new(0),
            has_market_close: AtomicBool::new(false),
            cache: PriceLevelCache::new(),
            trade_listener: Some(trade_listener),
            _phantom: PhantomData,
            price_level_changed_listener: Some(book_changed_listener),
            #[cfg(feature = "special_orders")]
            special_order_tracker: SpecialOrderTracker::new(),
            tick_size: None,
            lot_size: None,
            min_order_size: None,
            max_order_size: None,
            stp_mode: STPMode::None,
            fee_schedule: None,
        }
    }

    /// Set a trade listener for this order book
    pub fn set_trade_listener(&mut self, trade_listener: TradeListener) {
        self.trade_listener = Some(trade_listener);
    }

    /// Remove the trade listener from this order book
    pub fn remove_trade_listener(&mut self) {
        self.trade_listener = None;
    }

    /// set price level listener for this order book
    pub fn set_price_level_listener(&mut self, listener: PriceLevelChangedListener) {
        self.price_level_changed_listener = Some(listener);
    }

    /// remove price level listener for this order book
    pub fn remove_price_level_listener(&mut self) {
        self.price_level_changed_listener = None;
    }

    /// Set the fee schedule for this order book
    ///
    /// The fee schedule defines maker and taker fees in basis points.
    /// When set, fees will be calculated during trade execution.
    /// Set to None to disable fees.
    ///
    /// # Arguments
    ///
    /// * `fee_schedule` - The fee schedule to use, or None to disable fees
    ///
    /// # Examples
    ///
    /// ```
    /// use orderbook_rs::{OrderBook, FeeSchedule};
    ///
    /// let mut book = OrderBook::<()>::new("BTC/USD");
    ///
    /// // Set standard fees: 2 bps maker rebate, 5 bps taker fee
    /// let schedule = FeeSchedule::new(-2, 5);
    /// book.set_fee_schedule(Some(schedule));
    ///
    /// // Disable fees
    /// book.set_fee_schedule(None);
    /// ```
    pub fn set_fee_schedule(&mut self, fee_schedule: Option<FeeSchedule>) {
        self.fee_schedule = fee_schedule;
    }

    /// Get the current fee schedule for this order book
    ///
    /// # Returns
    ///
    /// The current fee schedule, or None if no fees are configured
    #[must_use]
    pub fn fee_schedule(&self) -> Option<FeeSchedule> {
        self.fee_schedule
    }

    /// Set the minimum price increment for orders.
    ///
    /// When set, order prices must be exact multiples of this value.
    /// For example, with `tick_size = 100`, prices 100, 200, 300 are valid
    /// but 150 is rejected with `OrderBookError::InvalidTickSize`.
    ///
    /// # Arguments
    /// - `tick_size`: Minimum price increment. Must be > 0
    pub fn set_tick_size(&mut self, tick_size: u128) {
        self.tick_size = Some(tick_size);
    }

    /// Returns the configured tick size, if any.
    ///
    /// `None` means tick size validation is disabled (all prices accepted).
    #[must_use]
    pub fn tick_size(&self) -> Option<u128> {
        self.tick_size
    }

    /// Set the minimum quantity increment for orders.
    ///
    /// When set, order quantities must be exact multiples of this value.
    /// For iceberg orders, both visible and hidden quantities are validated
    /// individually. Rejection returns `OrderBookError::InvalidLotSize`.
    ///
    /// # Arguments
    /// - `lot_size`: Minimum quantity increment. Must be > 0
    pub fn set_lot_size(&mut self, lot_size: u64) {
        self.lot_size = Some(lot_size);
    }

    /// Returns the configured lot size, if any.
    ///
    /// `None` means lot size validation is disabled (all quantities accepted).
    #[must_use]
    #[inline]
    pub fn lot_size(&self) -> Option<u64> {
        self.lot_size
    }

    /// Set the minimum order size.
    ///
    /// Orders with `total_quantity() < min_order_size` are rejected with
    /// `OrderBookError::OrderSizeOutOfRange`.
    ///
    /// # Arguments
    /// - `size`: Minimum allowed order quantity
    pub fn set_min_order_size(&mut self, size: u64) {
        self.min_order_size = Some(size);
    }

    /// Set the maximum order size.
    ///
    /// Orders with `total_quantity() > max_order_size` are rejected with
    /// `OrderBookError::OrderSizeOutOfRange`.
    ///
    /// # Arguments
    /// - `size`: Maximum allowed order quantity
    pub fn set_max_order_size(&mut self, size: u64) {
        self.max_order_size = Some(size);
    }

    /// Returns the configured minimum order size, if any.
    ///
    /// `None` means no minimum size validation (default).
    #[must_use]
    #[inline]
    pub fn min_order_size(&self) -> Option<u64> {
        self.min_order_size
    }

    /// Returns the configured maximum order size, if any.
    ///
    /// `None` means no maximum size validation (default).
    #[must_use]
    #[inline]
    pub fn max_order_size(&self) -> Option<u64> {
        self.max_order_size
    }

    /// Set the Self-Trade Prevention mode.
    ///
    /// When set to a mode other than [`STPMode::None`], the matching engine
    /// checks `user_id` on incoming and resting orders to prevent self-trades.
    /// Orders with `Hash32::zero()` always bypass STP checks.
    ///
    /// # Arguments
    /// - `mode`: The STP mode to activate
    pub fn set_stp_mode(&mut self, mode: STPMode) {
        self.stp_mode = mode;
    }

    /// Returns the configured Self-Trade Prevention mode.
    ///
    /// [`STPMode::None`] means STP is disabled (default).
    #[must_use]
    #[inline]
    pub fn stp_mode(&self) -> STPMode {
        self.stp_mode
    }

    /// Create a new order book for the given symbol with Self-Trade Prevention.
    ///
    /// # Arguments
    /// - `symbol`: The trading symbol for this order book
    /// - `stp_mode`: The STP mode to activate
    ///
    /// # Returns
    /// A new `OrderBook` instance with STP enabled
    pub fn with_stp_mode(symbol: &str, stp_mode: STPMode) -> Self {
        let mut book = Self::new(symbol);
        book.stp_mode = stp_mode;
        book
    }

    /// Get the symbol of this order book
    pub fn symbol(&self) -> &str {
        &self.symbol
    }

    /// Set the market close timestamp for DAY orders
    pub fn set_market_close_timestamp(&self, timestamp: u64) {
        self.market_close_timestamp
            .store(timestamp, Ordering::SeqCst);
        self.has_market_close.store(true, Ordering::SeqCst);
        trace!(
            "Order book {}: Set market close timestamp to {}",
            self.symbol, timestamp
        );
    }

    /// Clear the market close timestamp
    pub fn clear_market_close_timestamp(&self) {
        self.has_market_close.store(false, Ordering::SeqCst);
    }

    /// Get the best bid price, if any
    ///
    /// # Performance
    /// O(1) operation using SkipMap's ordered structure (highest price is last)
    pub fn best_bid(&self) -> Option<u128> {
        if let Some(cached_bid) = self.cache.get_cached_best_bid() {
            return Some(cached_bid);
        }

        // SkipMap maintains sorted order, best bid (highest price) is last
        let best_price = self.bids.iter().next_back().map(|entry| *entry.key());

        self.cache.update_best_prices(best_price, None);

        best_price
    }

    /// Get the best ask price, if any
    ///
    /// # Performance
    /// O(1) operation using SkipMap's ordered structure (lowest price is first)
    pub fn best_ask(&self) -> Option<u128> {
        if let Some(cached_ask) = self.cache.get_cached_best_ask() {
            return Some(cached_ask);
        }

        // SkipMap maintains sorted order, best ask (lowest price) is first
        let best_price = self.asks.iter().next().map(|entry| *entry.key());

        self.cache.update_best_prices(None, best_price);

        best_price
    }

    /// Get the mid price (average of best bid and best ask)
    pub fn mid_price(&self) -> Option<f64> {
        match (
            OrderBook::<T>::best_bid(self),
            OrderBook::<T>::best_ask(self),
        ) {
            (Some(bid), Some(ask)) => Some((bid as f64 + ask as f64) / 2.0),
            _ => None,
        }
    }

    /// Get the last trade price, if any
    pub fn last_trade_price(&self) -> Option<u128> {
        if self.has_traded.load(Ordering::Relaxed) {
            Some(self.last_trade_price.load())
        } else {
            None
        }
    }

    /// Get the spread (best ask - best bid)
    pub fn spread(&self) -> Option<u128> {
        match (
            OrderBook::<T>::best_bid(self),
            OrderBook::<T>::best_ask(self),
        ) {
            (Some(bid), Some(ask)) => Some(ask.saturating_sub(bid)),
            _ => None,
        }
    }

    /// Finds the price where cumulative depth reaches the target quantity
    ///
    /// # Arguments
    /// - `target_depth`: The target cumulative quantity to reach
    /// - `side`: The side of the order book (Buy for bids, Sell for asks)
    ///
    /// # Returns
    /// The price at which the cumulative depth reaches or exceeds the target,
    /// or `None` if the target depth cannot be reached with available liquidity.
    ///
    /// # Performance
    /// O(M log N) where M is the number of levels needed to reach the target depth.
    /// Leverages SkipMap's natural ordering for efficient iteration.
    ///
    /// # Examples
    /// ```
    /// use orderbook_rs::OrderBook;
    /// use pricelevel::Side;
    ///
    /// let orderbook = OrderBook::<()>::new("BTC/USD");
    /// // Find where 50 units of cumulative depth is reached
    /// if let Some(price) = orderbook.price_at_depth(50, Side::Buy) {
    ///     println!("50 units cumulative depth reached at price: {}", price);
    /// }
    /// ```
    #[must_use]
    pub fn price_at_depth(&self, target_depth: u64, side: Side) -> Option<u128> {
        let price_levels = match side {
            Side::Buy => &self.bids,
            Side::Sell => &self.asks,
        };

        if price_levels.is_empty() {
            return None;
        }

        let mut cumulative = 0u64;

        // Iterate in price-priority order
        let iter: Box<dyn Iterator<Item = _>> = match side {
            Side::Buy => Box::new(price_levels.iter().rev()), // Highest to lowest
            Side::Sell => Box::new(price_levels.iter()),      // Lowest to highest
        };

        for entry in iter {
            let price = *entry.key();
            let price_level = entry.value();
            cumulative = cumulative.saturating_add(price_level.total_quantity().unwrap_or(0));

            if cumulative >= target_depth {
                return Some(price);
            }
        }

        None
    }

    /// Returns both the price and actual cumulative depth when target is reached
    ///
    /// # Arguments
    /// - `target_depth`: The target cumulative quantity to reach
    /// - `side`: The side of the order book (Buy for bids, Sell for asks)
    ///
    /// # Returns
    /// A tuple of `(price, cumulative_depth)` where the cumulative depth reaches
    /// or exceeds the target, or `None` if the target depth cannot be reached.
    ///
    /// # Performance
    /// O(M log N) where M is the number of levels needed to reach the target depth.
    ///
    /// # Examples
    /// ```
    /// use orderbook_rs::OrderBook;
    /// use pricelevel::Side;
    ///
    /// let orderbook = OrderBook::<()>::new("BTC/USD");
    /// // Get both price and actual depth
    /// if let Some((price, depth)) = orderbook.cumulative_depth_to_target(50, Side::Buy) {
    ///     println!("Target depth 50 reached at {} (actual: {})", price, depth);
    /// }
    /// ```
    #[must_use]
    pub fn cumulative_depth_to_target(&self, target_depth: u64, side: Side) -> Option<(u128, u64)> {
        let price_levels = match side {
            Side::Buy => &self.bids,
            Side::Sell => &self.asks,
        };

        if price_levels.is_empty() {
            return None;
        }

        let mut cumulative = 0u64;

        // Iterate in price-priority order
        let iter: Box<dyn Iterator<Item = _>> = match side {
            Side::Buy => Box::new(price_levels.iter().rev()), // Highest to lowest
            Side::Sell => Box::new(price_levels.iter()),      // Lowest to highest
        };

        for entry in iter {
            let price = *entry.key();
            let price_level = entry.value();
            cumulative = cumulative.saturating_add(price_level.total_quantity().unwrap_or(0));

            if cumulative >= target_depth {
                return Some((price, cumulative));
            }
        }

        None
    }

    /// Calculates total depth available in the first N price levels
    ///
    /// # Arguments
    /// - `levels`: The number of price levels to include (from best price)
    /// - `side`: The side of the order book (Buy for bids, Sell for asks)
    ///
    /// # Returns
    /// The total cumulative quantity across the specified number of levels.
    /// Returns 0 if the side is empty or if levels is 0.
    ///
    /// # Performance
    /// O(min(levels, N) * log N) where N is the total number of price levels.
    ///
    /// # Examples
    /// ```
    /// use orderbook_rs::OrderBook;
    /// use pricelevel::Side;
    ///
    /// let orderbook = OrderBook::<()>::new("BTC/USD");
    /// // Total depth in top 10 bid levels
    /// let top_10_depth = orderbook.total_depth_at_levels(10, Side::Buy);
    /// println!("Total depth in top 10 bid levels: {}", top_10_depth);
    /// ```
    #[must_use]
    pub fn total_depth_at_levels(&self, levels: usize, side: Side) -> u64 {
        if levels == 0 {
            return 0;
        }

        let price_levels = match side {
            Side::Buy => &self.bids,
            Side::Sell => &self.asks,
        };

        if price_levels.is_empty() {
            return 0;
        }

        let mut total = 0u64;

        // Iterate in price-priority order
        let iter: Box<dyn Iterator<Item = _>> = match side {
            Side::Buy => Box::new(price_levels.iter().rev()), // Highest to lowest
            Side::Sell => Box::new(price_levels.iter()),      // Lowest to highest
        };

        for (count, entry) in iter.enumerate() {
            if count >= levels {
                break;
            }

            let price_level = entry.value();
            total = total.saturating_add(price_level.total_quantity().unwrap_or(0));
        }

        total
    }

    /// Returns the absolute spread (ask - bid) in price units
    ///
    /// This is an alias for `spread()` provided for API consistency.
    ///
    /// # Returns
    /// - `Some(spread)` if both best bid and best ask exist
    /// - `None` if either side is empty
    ///
    /// # Examples
    /// ```
    /// use orderbook_rs::OrderBook;
    /// use pricelevel::{Id, Side, TimeInForce};
    ///
    /// let book = OrderBook::<()>::new("BTC/USD");
    /// let _ = book.add_limit_order(Id::new(), 100, 10, Side::Buy, TimeInForce::Gtc, None);
    /// let _ = book.add_limit_order(Id::new(), 105, 10, Side::Sell, TimeInForce::Gtc, None);
    ///
    /// if let Some(spread) = book.spread_absolute() {
    ///     println!("Absolute spread: {}", spread); // 5
    /// }
    /// ```
    #[must_use]
    pub fn spread_absolute(&self) -> Option<u128> {
        self.spread()
    }

    /// Returns the spread in basis points (bps)
    ///
    /// Basis points are calculated as: ((ask - bid) / mid_price) * multiplier
    /// One basis point = 0.01% = 0.0001
    ///
    /// # Arguments
    /// - `bps_multiplier`: Optional custom multiplier for basis points calculation.
    ///   If `None`, uses the default value of 10,000.
    ///   Common values: 10,000 for bps, 1,000,000 for pips in FX
    ///
    /// # Returns
    /// - `Some(bps)` if both best bid and best ask exist
    /// - `None` if either side is empty or mid price is zero
    ///
    /// # Examples
    /// ```
    /// use orderbook_rs::OrderBook;
    /// use pricelevel::{Id, Side, TimeInForce};
    ///
    /// let book = OrderBook::<()>::new("BTC/USD");
    /// let _ = book.add_limit_order(Id::new(), 10000, 10, Side::Buy, TimeInForce::Gtc, None);
    /// let _ = book.add_limit_order(Id::new(), 10010, 10, Side::Sell, TimeInForce::Gtc, None);
    ///
    /// // Using default 10,000 multiplier
    /// if let Some(spread_bps) = book.spread_bps(None) {
    ///     println!("Spread: {:.2} bps", spread_bps); // ~10 bps
    /// }
    ///
    /// // Using custom multiplier for percentage
    /// if let Some(spread_pct) = book.spread_bps(Some(100.0)) {
    ///     println!("Spread: {:.2}%", spread_pct); // ~0.10%
    /// }
    /// ```
    #[must_use]
    pub fn spread_bps(&self, bps_multiplier: Option<f64>) -> Option<f64> {
        let multiplier = bps_multiplier.unwrap_or(DEFAULT_BASIS_POINTS_MULTIPLIER);

        match (self.best_bid(), self.best_ask(), self.mid_price()) {
            (Some(bid), Some(ask), Some(mid)) if mid > 0.0 => {
                let spread = ask.saturating_sub(bid) as f64;
                Some((spread / mid) * multiplier)
            }
            _ => None,
        }
    }

    /// Calculates the volume-weighted average price (VWAP) for a given quantity
    ///
    /// VWAP walks through price levels in order until the target quantity is filled,
    /// calculating the weighted average price based on the quantities at each level.
    ///
    /// # Arguments
    /// - `quantity`: The target quantity to fill (in units)
    /// - `side`: The side to calculate VWAP for (Buy = execute against asks, Sell = execute against bids)
    ///
    /// # Returns
    /// - `Some(vwap)` if sufficient liquidity exists to fill the quantity
    /// - `None` if insufficient liquidity or quantity is zero
    ///
    /// # Performance
    /// O(M log N) where M is the number of levels needed to reach the target quantity.
    ///
    /// # Examples
    /// ```
    /// use orderbook_rs::OrderBook;
    /// use pricelevel::{Id, Side, TimeInForce};
    ///
    /// let book = OrderBook::<()>::new("BTC/USD");
    /// let _ = book.add_limit_order(Id::new(), 100, 10, Side::Sell, TimeInForce::Gtc, None);
    /// let _ = book.add_limit_order(Id::new(), 105, 15, Side::Sell, TimeInForce::Gtc, None);
    ///
    /// // Calculate VWAP for buying 20 units
    /// if let Some(vwap) = book.vwap(20, Side::Buy) {
    ///     println!("VWAP for buying 20 units: {:.2}", vwap);
    /// }
    /// ```
    #[must_use]
    pub fn vwap(&self, quantity: u64, side: Side) -> Option<f64> {
        if quantity == 0 {
            return None;
        }

        // For Buy orders, we execute against asks (in ascending order)
        // For Sell orders, we execute against bids (in descending order)
        let price_levels = match side {
            Side::Buy => &self.asks,
            Side::Sell => &self.bids,
        };

        if price_levels.is_empty() {
            return None;
        }

        let mut remaining = quantity;
        let mut total_cost = 0u128; // Use u128 to avoid overflow
        let mut total_filled = 0u64;

        // Iterate in price-priority order
        let iter: Box<dyn Iterator<Item = _>> = match side {
            Side::Buy => Box::new(price_levels.iter()), // Lowest to highest (asks)
            Side::Sell => Box::new(price_levels.iter().rev()), // Highest to lowest (bids)
        };

        for entry in iter {
            if remaining == 0 {
                break;
            }

            let price = *entry.key();
            let price_level = entry.value();
            let available = price_level.total_quantity().unwrap_or(0);

            if available == 0 {
                continue;
            }

            let fill_qty = remaining.min(available);
            total_cost = total_cost.saturating_add(price * (fill_qty as u128));
            total_filled = total_filled.saturating_add(fill_qty);
            remaining = remaining.saturating_sub(fill_qty);
        }

        if total_filled == quantity {
            Some(total_cost as f64 / total_filled as f64)
        } else {
            None // Insufficient liquidity
        }
    }

    /// Calculates the micro price (weighted price by volume at best bid and ask)
    ///
    /// The micro price is calculated as:
    /// `(best_ask * bid_volume + best_bid * ask_volume) / (bid_volume + ask_volume)`
    ///
    /// This metric gives more weight to the side with more volume, providing
    /// a better estimate of the "true" price than the simple mid price.
    ///
    /// # Returns
    /// - `Some(micro_price)` if both best bid and best ask exist with non-zero volumes
    /// - `None` if either side is empty or both volumes are zero
    ///
    /// # Examples
    /// ```
    /// use orderbook_rs::OrderBook;
    /// use pricelevel::{Id, Side, TimeInForce};
    ///
    /// let book = OrderBook::<()>::new("BTC/USD");
    /// let _ = book.add_limit_order(Id::new(), 100, 50, Side::Buy, TimeInForce::Gtc, None);
    /// let _ = book.add_limit_order(Id::new(), 105, 30, Side::Sell, TimeInForce::Gtc, None);
    ///
    /// if let Some(micro) = book.micro_price() {
    ///     println!("Micro price: {:.2}", micro);
    /// }
    /// ```
    #[must_use]
    pub fn micro_price(&self) -> Option<f64> {
        let best_bid_price = self.best_bid()?;
        let best_ask_price = self.best_ask()?;

        // Get volumes at best levels
        let bid_volume = self
            .bids
            .get(&best_bid_price)?
            .value()
            .total_quantity()
            .unwrap_or(0);
        let ask_volume = self
            .asks
            .get(&best_ask_price)?
            .value()
            .total_quantity()
            .unwrap_or(0);

        let total_volume = bid_volume.saturating_add(ask_volume);

        if total_volume == 0 {
            return None;
        }

        // micro_price = (ask_price * bid_volume + bid_price * ask_volume) / (bid_volume + ask_volume)
        let numerator = (best_ask_price as f64 * bid_volume as f64)
            + (best_bid_price as f64 * ask_volume as f64);
        let denominator = total_volume as f64;

        Some(numerator / denominator)
    }

    /// Calculates the order book imbalance ratio for the top N levels
    ///
    /// The imbalance is calculated as:
    /// `(bid_volume - ask_volume) / (bid_volume + ask_volume)`
    ///
    /// # Arguments
    /// - `levels`: Number of top price levels to consider (must be > 0)
    ///
    /// # Returns
    /// - A value between -1.0 and 1.0:
    ///   - `> 0`: More buy pressure (bids dominate)
    ///   - `< 0`: More sell pressure (asks dominate)
    ///   - `≈ 0`: Balanced order book
    ///   - Returns `0.0` if both sides are empty or `levels` is 0
    ///
    /// # Performance
    /// O(M log N) where M is the number of levels requested.
    ///
    /// # Examples
    /// ```
    /// use orderbook_rs::OrderBook;
    /// use pricelevel::{Id, Side, TimeInForce};
    ///
    /// let book = OrderBook::<()>::new("BTC/USD");
    /// let _ = book.add_limit_order(Id::new(), 100, 60, Side::Buy, TimeInForce::Gtc, None);
    /// let _ = book.add_limit_order(Id::new(), 105, 40, Side::Sell, TimeInForce::Gtc, None);
    ///
    /// let imbalance = book.order_book_imbalance(5);
    /// if imbalance > 0.0 {
    ///     println!("More buy pressure: {:.2}", imbalance);
    /// }
    /// ```
    #[must_use]
    pub fn order_book_imbalance(&self, levels: usize) -> f64 {
        if levels == 0 {
            return 0.0;
        }

        let bid_volume = self.total_depth_at_levels(levels, Side::Buy);
        let ask_volume = self.total_depth_at_levels(levels, Side::Sell);

        let total_volume = bid_volume.saturating_add(ask_volume);

        if total_volume == 0 {
            return 0.0;
        }

        let bid_f64 = bid_volume as f64;
        let ask_f64 = ask_volume as f64;

        (bid_f64 - ask_f64) / (bid_f64 + ask_f64)
    }

    /// Calculates the market impact of a hypothetical order
    ///
    /// Analyzes how an order would affect the market by walking through
    /// available liquidity and calculating key metrics including average price,
    /// slippage, and the number of levels consumed.
    ///
    /// # Arguments
    /// - `quantity`: The order quantity to analyze (in units)
    /// - `side`: The side of the order (Buy = execute against asks, Sell = execute against bids)
    ///
    /// # Returns
    /// A `MarketImpact` struct containing:
    /// - `avg_price`: Volume-weighted average execution price
    /// - `worst_price`: Furthest price from the best price
    /// - `slippage`: Absolute difference from best price
    /// - `slippage_bps`: Slippage in basis points
    /// - `levels_consumed`: Number of price levels used
    /// - `total_quantity_available`: Total liquidity available
    ///
    /// # Performance
    /// O(M log N) where M is the number of levels needed.
    ///
    /// # Examples
    /// ```
    /// use orderbook_rs::OrderBook;
    /// use pricelevel::{Id, Side, TimeInForce};
    ///
    /// let book = OrderBook::<()>::new("BTC/USD");
    /// let _ = book.add_limit_order(Id::new(), 100, 10, Side::Sell, TimeInForce::Gtc, None);
    /// let _ = book.add_limit_order(Id::new(), 105, 15, Side::Sell, TimeInForce::Gtc, None);
    ///
    /// let impact = book.market_impact(20, Side::Buy);
    /// println!("Average price: {}", impact.avg_price);
    /// println!("Slippage: {} bps", impact.slippage_bps);
    /// println!("Levels consumed: {}", impact.levels_consumed);
    /// ```
    #[must_use]
    pub fn market_impact(&self, quantity: u64, side: Side) -> MarketImpact {
        if quantity == 0 {
            return MarketImpact::empty();
        }

        // For Buy orders, we execute against asks (in ascending order)
        // For Sell orders, we execute against bids (in descending order)
        let price_levels = match side {
            Side::Buy => &self.asks,
            Side::Sell => &self.bids,
        };

        if price_levels.is_empty() {
            return MarketImpact::empty();
        }

        let best_price = match side {
            Side::Buy => self.best_ask(),
            Side::Sell => self.best_bid(),
        };

        let best_price = match best_price {
            Some(price) => price,
            None => return MarketImpact::empty(),
        };

        let mut remaining = quantity;
        let mut total_cost = 0u128;
        let mut total_filled = 0u64;
        let mut worst_price = best_price;
        let mut levels_consumed = 0;

        // Iterate in price-priority order
        let iter: Box<dyn Iterator<Item = _>> = match side {
            Side::Buy => Box::new(price_levels.iter()), // Lowest to highest (asks)
            Side::Sell => Box::new(price_levels.iter().rev()), // Highest to lowest (bids)
        };

        for entry in iter {
            if remaining == 0 {
                break;
            }

            let price = *entry.key();
            let price_level = entry.value();
            let available = price_level.total_quantity().unwrap_or(0);

            if available == 0 {
                continue;
            }

            levels_consumed += 1;
            let fill_qty = remaining.min(available);
            total_cost = total_cost.saturating_add(price * (fill_qty as u128));
            total_filled = total_filled.saturating_add(fill_qty);
            worst_price = price;
            remaining = remaining.saturating_sub(fill_qty);
        }

        let avg_price = if total_filled > 0 {
            total_cost as f64 / total_filled as f64
        } else {
            0.0
        };

        let slippage = match side {
            Side::Buy => worst_price.saturating_sub(best_price),
            Side::Sell => best_price.saturating_sub(worst_price),
        };

        let slippage_bps = if best_price > 0 {
            (slippage as f64 / best_price as f64) * DEFAULT_BASIS_POINTS_MULTIPLIER
        } else {
            0.0
        };

        MarketImpact {
            avg_price,
            worst_price,
            slippage,
            slippage_bps,
            levels_consumed,
            total_quantity_available: total_filled,
        }
    }

    /// Simulates the execution of a market order
    ///
    /// Provides a detailed step-by-step simulation of how a market order
    /// would be filled, including all individual fills at different price levels.
    ///
    /// # Arguments
    /// - `quantity`: The order quantity to simulate (in units)
    /// - `side`: The side of the order (Buy = execute against asks, Sell = execute against bids)
    ///
    /// # Returns
    /// An `OrderSimulation` struct containing:
    /// - `fills`: Vector of (price, quantity) pairs for each fill
    /// - `avg_price`: Volume-weighted average execution price
    /// - `total_filled`: Total quantity that would be filled
    /// - `remaining_quantity`: Quantity that could not be filled
    ///
    /// # Performance
    /// O(M log N) where M is the number of levels needed.
    ///
    /// # Examples
    /// ```
    /// use orderbook_rs::OrderBook;
    /// use pricelevel::{Id, Side, TimeInForce};
    ///
    /// let book = OrderBook::<()>::new("BTC/USD");
    /// let _ = book.add_limit_order(Id::new(), 100, 10, Side::Sell, TimeInForce::Gtc, None);
    /// let _ = book.add_limit_order(Id::new(), 105, 15, Side::Sell, TimeInForce::Gtc, None);
    ///
    /// let simulation = book.simulate_market_order(20, Side::Buy);
    /// for (price, qty) in &simulation.fills {
    ///     println!("Fill: {} @ {}", qty, price);
    /// }
    /// println!("Average price: {}", simulation.avg_price);
    /// ```
    #[must_use]
    pub fn simulate_market_order(&self, quantity: u64, side: Side) -> OrderSimulation {
        if quantity == 0 {
            return OrderSimulation::empty();
        }

        // For Buy orders, we execute against asks (in ascending order)
        // For Sell orders, we execute against bids (in descending order)
        let price_levels = match side {
            Side::Buy => &self.asks,
            Side::Sell => &self.bids,
        };

        if price_levels.is_empty() {
            let mut sim = OrderSimulation::empty();
            sim.remaining_quantity = quantity;
            return sim;
        }

        let mut remaining = quantity;
        let mut total_cost = 0u128;
        let mut total_filled = 0u64;
        let mut fills = Vec::new();

        // Iterate in price-priority order
        let iter: Box<dyn Iterator<Item = _>> = match side {
            Side::Buy => Box::new(price_levels.iter()), // Lowest to highest (asks)
            Side::Sell => Box::new(price_levels.iter().rev()), // Highest to lowest (bids)
        };

        for entry in iter {
            if remaining == 0 {
                break;
            }

            let price = *entry.key();
            let price_level = entry.value();
            let available = price_level.total_quantity().unwrap_or(0);

            if available == 0 {
                continue;
            }

            let fill_qty = remaining.min(available);
            total_cost = total_cost.saturating_add(price * (fill_qty as u128));
            total_filled = total_filled.saturating_add(fill_qty);
            fills.push((price, fill_qty));
            remaining = remaining.saturating_sub(fill_qty);
        }

        let avg_price = if total_filled > 0 {
            total_cost as f64 / total_filled as f64
        } else {
            0.0
        };

        OrderSimulation {
            fills,
            avg_price,
            total_filled,
            remaining_quantity: remaining,
        }
    }

    /// Calculates available liquidity within a specific price range
    ///
    /// Sums up the total quantity available at price levels that fall
    /// within the specified price range (inclusive).
    ///
    /// # Arguments
    /// - `min_price`: Minimum price of the range (inclusive, in price units)
    /// - `max_price`: Maximum price of the range (inclusive, in price units)
    /// - `side`: The side to analyze (Buy for bids, Sell for asks)
    ///
    /// # Returns
    /// Total quantity available in the specified price range (in units)
    ///
    /// # Performance
    /// O(M log N) where M is the number of levels in the range.
    ///
    /// # Examples
    /// ```
    /// use orderbook_rs::OrderBook;
    /// use pricelevel::{Id, Side, TimeInForce};
    ///
    /// let book = OrderBook::<()>::new("BTC/USD");
    /// let _ = book.add_limit_order(Id::new(), 100, 10, Side::Buy, TimeInForce::Gtc, None);
    /// let _ = book.add_limit_order(Id::new(), 105, 15, Side::Buy, TimeInForce::Gtc, None);
    /// let _ = book.add_limit_order(Id::new(), 110, 20, Side::Buy, TimeInForce::Gtc, None);
    ///
    /// // Get liquidity between 100 and 105 (inclusive)
    /// let liquidity = book.liquidity_in_range(100, 105, Side::Buy);
    /// assert_eq!(liquidity, 25); // 10 + 15
    /// ```
    #[must_use]
    pub fn liquidity_in_range(&self, min_price: u128, max_price: u128, side: Side) -> u64 {
        if min_price > max_price {
            return 0;
        }

        let price_levels = match side {
            Side::Buy => &self.bids,
            Side::Sell => &self.asks,
        };

        if price_levels.is_empty() {
            return 0;
        }

        let mut total_liquidity = 0u64;

        for entry in price_levels.iter() {
            let price = *entry.key();

            if price < min_price {
                continue;
            }

            if price > max_price {
                break;
            }

            let price_level = entry.value();
            let quantity = price_level.total_quantity().unwrap_or(0);
            total_liquidity = total_liquidity.saturating_add(quantity);
        }

        total_liquidity
    }

    /// Returns the number of orders ahead in queue at a specific price level
    ///
    /// Calculates how many orders are already in the queue at the specified
    /// price level. Useful for estimating execution probability and queue position.
    ///
    /// # Arguments
    /// - `price`: The price level to check (in price units)
    /// - `side`: The side to check (Buy for bids, Sell for asks)
    ///
    /// # Returns
    /// The number of orders at that price level. Returns 0 if the price level doesn't exist.
    ///
    /// # Performance
    /// O(1) for price level lookup, O(N) for counting orders where N is orders at that level.
    ///
    /// # Examples
    /// ```
    /// use orderbook_rs::OrderBook;
    /// use pricelevel::{Id, Side, TimeInForce};
    ///
    /// let book = OrderBook::<()>::new("BTC/USD");
    /// let _ = book.add_limit_order(Id::new(), 100, 10, Side::Buy, TimeInForce::Gtc, None);
    /// let _ = book.add_limit_order(Id::new(), 100, 20, Side::Buy, TimeInForce::Gtc, None);
    ///
    /// let orders_ahead = book.queue_ahead_at_price(100, Side::Buy);
    /// assert_eq!(orders_ahead, 2);
    /// ```
    #[must_use]
    pub fn queue_ahead_at_price(&self, price: u128, side: Side) -> usize {
        let price_levels = match side {
            Side::Buy => &self.bids,
            Side::Sell => &self.asks,
        };

        if let Some(entry) = price_levels.get(&price) {
            entry.value().iter_orders().count()
        } else {
            0
        }
    }

    /// Calculates the price N ticks inside the best price
    ///
    /// Useful for placing orders that are competitive but not at the best price.
    /// For buy orders, "inside" means lower than best bid.
    /// For sell orders, "inside" means higher than best ask.
    ///
    /// # Arguments
    /// - `n_ticks`: Number of ticks to move inside (in ticks)
    /// - `tick_size`: The size of each tick (in price units)
    /// - `side`: The side to calculate for (Buy or Sell)
    ///
    /// # Returns
    /// - `Some(price)` if best price exists and calculation is valid
    /// - `None` if no best price exists or calculation would underflow/overflow
    ///
    /// # Performance
    /// O(1) operation using cached best prices.
    ///
    /// # Examples
    /// ```
    /// use orderbook_rs::OrderBook;
    /// use pricelevel::{Id, Side, TimeInForce};
    ///
    /// let book = OrderBook::<()>::new("BTC/USD");
    /// let _ = book.add_limit_order(Id::new(), 100, 10, Side::Buy, TimeInForce::Gtc, None);
    /// let _ = book.add_limit_order(Id::new(), 105, 10, Side::Sell, TimeInForce::Gtc, None);
    ///
    /// // Buy side: best bid is 100, 1 tick inside = 99 (if tick_size = 1)
    /// if let Some(price) = book.price_n_ticks_inside(1, 1, Side::Buy) {
    ///     assert_eq!(price, 99);
    /// }
    ///
    /// // Sell side: best ask is 105, 1 tick inside = 106 (if tick_size = 1)
    /// if let Some(price) = book.price_n_ticks_inside(1, 1, Side::Sell) {
    ///     assert_eq!(price, 106);
    /// }
    /// ```
    #[must_use]
    pub fn price_n_ticks_inside(
        &self,
        n_ticks: usize,
        tick_size: u128,
        side: Side,
    ) -> Option<u128> {
        if n_ticks == 0 || tick_size == 0 {
            return None;
        }

        let adjustment = (n_ticks as u128).checked_mul(tick_size)?;

        match side {
            Side::Buy => {
                let best_bid = self.best_bid()?;
                best_bid.checked_sub(adjustment)
            }
            Side::Sell => {
                let best_ask = self.best_ask()?;
                best_ask.checked_add(adjustment)
            }
        }
    }

    /// Calculates the optimal price to be at a specific queue position
    ///
    /// Determines what price level would place you at the Nth position in the queue.
    /// Position 1 means front of queue (best price), position 2 means second-best, etc.
    ///
    /// # Arguments
    /// - `position`: Target queue position (1 = best price, 2 = second best, etc.)
    /// - `side`: The side to calculate for (Buy or Sell)
    ///
    /// # Returns
    /// - `Some(price)` if the position exists in the order book
    /// - `None` if position is 0 or exceeds available price levels
    ///
    /// # Performance
    /// O(N) where N is the target position, due to iteration through price levels.
    ///
    /// # Examples
    /// ```
    /// use orderbook_rs::OrderBook;
    /// use pricelevel::{Id, Side, TimeInForce};
    ///
    /// let book = OrderBook::<()>::new("BTC/USD");
    /// let _ = book.add_limit_order(Id::new(), 100, 10, Side::Buy, TimeInForce::Gtc, None);
    /// let _ = book.add_limit_order(Id::new(), 99, 10, Side::Buy, TimeInForce::Gtc, None);
    /// let _ = book.add_limit_order(Id::new(), 98, 10, Side::Buy, TimeInForce::Gtc, None);
    ///
    /// // Position 1 should be best bid (100)
    /// assert_eq!(book.price_for_queue_position(1, Side::Buy), Some(100));
    /// // Position 2 should be second best (99)
    /// assert_eq!(book.price_for_queue_position(2, Side::Buy), Some(99));
    /// ```
    #[must_use]
    pub fn price_for_queue_position(&self, position: usize, side: Side) -> Option<u128> {
        if position == 0 {
            return None;
        }

        let price_levels = match side {
            Side::Buy => &self.bids,
            Side::Sell => &self.asks,
        };

        if price_levels.is_empty() {
            return None;
        }

        // For bids: iterate from highest to lowest (reverse)
        // For asks: iterate from lowest to highest (forward)
        let mut current_position = 1;

        let iter: Box<dyn Iterator<Item = _>> = match side {
            Side::Buy => Box::new(price_levels.iter().rev()),
            Side::Sell => Box::new(price_levels.iter()),
        };

        for entry in iter {
            if current_position == position {
                return Some(*entry.key());
            }
            current_position += 1;
        }

        None
    }

    /// Suggests optimal price to place an order just inside a target depth
    ///
    /// Calculates the price level where placing an order would position it
    /// just inside (better than) the specified cumulative depth. Useful for
    /// depth-based market making strategies.
    ///
    /// # Arguments
    /// - `target_depth`: Target cumulative quantity (in units)
    /// - `tick_size`: The size of each tick (in price units)
    /// - `side`: The side to calculate for (Buy or Sell)
    ///
    /// # Returns
    /// - `Some(price)` adjusted by one tick inside the depth level
    /// - `None` if insufficient depth exists or calculation fails
    ///
    /// # Performance
    /// O(M log N) where M is the number of levels to reach target depth.
    ///
    /// # Examples
    /// ```
    /// use orderbook_rs::OrderBook;
    /// use pricelevel::{Id, Side, TimeInForce};
    ///
    /// let book = OrderBook::<()>::new("BTC/USD");
    /// let _ = book.add_limit_order(Id::new(), 100, 50, Side::Buy, TimeInForce::Gtc, None);
    /// let _ = book.add_limit_order(Id::new(), 99, 60, Side::Buy, TimeInForce::Gtc, None);
    /// let _ = book.add_limit_order(Id::new(), 98, 70, Side::Buy, TimeInForce::Gtc, None);
    ///
    /// // Want to be just inside 100 units of depth
    /// // Depth at 100: 50, at 99: 110, so we want to be at 100 (just inside 110)
    /// if let Some(price) = book.price_at_depth_adjusted(100, 1, Side::Buy) {
    ///     assert_eq!(price, 100); // One tick better than the level that reaches depth
    /// }
    /// ```
    #[must_use]
    pub fn price_at_depth_adjusted(
        &self,
        target_depth: u64,
        tick_size: u128,
        side: Side,
    ) -> Option<u128> {
        if target_depth == 0 || tick_size == 0 {
            return None;
        }

        let price_levels = match side {
            Side::Buy => &self.bids,
            Side::Sell => &self.asks,
        };

        if price_levels.is_empty() {
            return None;
        }

        let mut cumulative_depth = 0u64;
        let mut last_price = None;

        // For bids: iterate from highest to lowest (reverse)
        // For asks: iterate from lowest to highest (forward)
        let iter: Box<dyn Iterator<Item = _>> = match side {
            Side::Buy => Box::new(price_levels.iter().rev()),
            Side::Sell => Box::new(price_levels.iter()),
        };

        for entry in iter {
            let price = *entry.key();
            let quantity = entry.value().total_quantity().unwrap_or(0);
            cumulative_depth = cumulative_depth.saturating_add(quantity);

            if cumulative_depth >= target_depth {
                // Found the level where we exceed target depth
                // Return one tick better than this price
                return match side {
                    Side::Buy => price.checked_add(tick_size),
                    Side::Sell => price.checked_sub(tick_size),
                };
            }

            last_price = Some(price);
        }

        // If we didn't reach target depth, return the last price seen
        // (deepest level available)
        last_price
    }

    /// Returns an iterator over price levels with cumulative depth tracking
    ///
    /// Iterates through price levels in price-priority order (best to worst),
    /// maintaining cumulative depth as it progresses. This provides a memory-efficient
    /// way to analyze market depth distribution without allocating vectors.
    ///
    /// # Arguments
    /// - `side`: The side to iterate (Buy for bids from highest to lowest, Sell for asks from lowest to highest)
    ///
    /// # Returns
    /// An iterator yielding `LevelInfo` containing price, quantity, and cumulative depth
    ///
    /// # Performance
    /// Lazy evaluation with O(1) memory overhead. Each iteration is O(log N) for skipmap traversal.
    ///
    /// # Examples
    /// ```
    /// use orderbook_rs::OrderBook;
    /// use pricelevel::{Id, Side, TimeInForce};
    ///
    /// let book = OrderBook::<()>::new("BTC/USD");
    /// let _ = book.add_limit_order(Id::new(), 100, 10, Side::Buy, TimeInForce::Gtc, None);
    /// let _ = book.add_limit_order(Id::new(), 99, 15, Side::Buy, TimeInForce::Gtc, None);
    /// let _ = book.add_limit_order(Id::new(), 98, 20, Side::Buy, TimeInForce::Gtc, None);
    ///
    /// // Functional-style analysis
    /// for level in book.levels_with_cumulative_depth(Side::Buy).take(5) {
    ///     println!("Price: {}, Qty: {}, Cumulative: {}",
    ///              level.price, level.quantity, level.cumulative_depth);
    ///     
    ///     if level.cumulative_depth >= 30 {
    ///         println!("Target depth reached!");
    ///         break;
    ///     }
    /// }
    /// ```
    pub fn levels_with_cumulative_depth(&self, side: Side) -> LevelsWithCumulativeDepth<'_> {
        let price_levels = match side {
            Side::Buy => &self.bids,
            Side::Sell => &self.asks,
        };

        LevelsWithCumulativeDepth::new(price_levels, side)
    }

    /// Returns an iterator over price levels until target depth is reached
    ///
    /// Automatically stops when cumulative depth reaches or exceeds the target.
    /// This is useful for determining how many price levels are needed to fill
    /// a specific quantity, without processing unnecessary deeper levels.
    ///
    /// # Arguments
    /// - `target_depth`: Target cumulative quantity (in units)
    /// - `side`: The side to iterate (Buy for bids, Sell for asks)
    ///
    /// # Returns
    /// An iterator that stops when target depth is reached
    ///
    /// # Performance
    /// Short-circuits early, processing only the minimum levels needed. O(M log N) where M is levels to reach target.
    ///
    /// # Examples
    /// ```
    /// use orderbook_rs::OrderBook;
    /// use pricelevel::{Id, Side, TimeInForce};
    ///
    /// let book = OrderBook::<()>::new("BTC/USD");
    /// let _ = book.add_limit_order(Id::new(), 100, 10, Side::Buy, TimeInForce::Gtc, None);
    /// let _ = book.add_limit_order(Id::new(), 99, 15, Side::Buy, TimeInForce::Gtc, None);
    /// let _ = book.add_limit_order(Id::new(), 98, 20, Side::Buy, TimeInForce::Gtc, None);
    ///
    /// // Collect levels needed for 30 units
    /// let levels: Vec<_> = book.levels_until_depth(30, Side::Buy).collect();
    /// println!("Levels needed: {}", levels.len());
    /// ```
    pub fn levels_until_depth(&self, target_depth: u64, side: Side) -> LevelsUntilDepth<'_> {
        let price_levels = match side {
            Side::Buy => &self.bids,
            Side::Sell => &self.asks,
        };

        LevelsUntilDepth::new(price_levels, side, target_depth)
    }

    /// Returns an iterator over price levels within a specific price range
    ///
    /// Only yields levels where the price falls within [min_price, max_price] inclusive.
    /// Useful for analyzing liquidity distribution in specific price bands without
    /// allocating intermediate collections.
    ///
    /// # Arguments
    /// - `min_price`: Minimum price of the range (inclusive, in price units)
    /// - `max_price`: Maximum price of the range (inclusive, in price units)
    /// - `side`: The side to iterate (Buy for bids, Sell for asks)
    ///
    /// # Returns
    /// An iterator yielding only levels within the price range
    ///
    /// # Performance
    /// Skips levels outside range, O(M log N) where M is levels in range.
    ///
    /// # Examples
    /// ```
    /// use orderbook_rs::OrderBook;
    /// use pricelevel::{Id, Side, TimeInForce};
    ///
    /// let book = OrderBook::<()>::new("BTC/USD");
    /// let _ = book.add_limit_order(Id::new(), 100, 10, Side::Buy, TimeInForce::Gtc, None);
    /// let _ = book.add_limit_order(Id::new(), 95, 15, Side::Buy, TimeInForce::Gtc, None);
    /// let _ = book.add_limit_order(Id::new(), 90, 20, Side::Buy, TimeInForce::Gtc, None);
    ///
    /// // Analyze levels between 90 and 100
    /// let total_qty: u64 = book
    ///     .levels_in_range(90, 100, Side::Buy)
    ///     .map(|level| level.quantity)
    ///     .sum();
    /// println!("Total quantity in range: {}", total_qty);
    /// ```
    pub fn levels_in_range(
        &self,
        min_price: u128,
        max_price: u128,
        side: Side,
    ) -> LevelsInRange<'_> {
        let price_levels = match side {
            Side::Buy => &self.bids,
            Side::Sell => &self.asks,
        };

        LevelsInRange::new(price_levels, side, min_price, max_price)
    }

    /// Finds the first price level matching a predicate
    ///
    /// Searches through price levels in price-priority order and returns the first
    /// level that satisfies the given predicate function. The predicate receives
    /// both the level information and cumulative depth for context-aware decisions.
    ///
    /// # Arguments
    /// - `side`: The side to search (Buy for bids, Sell for asks)
    /// - `predicate`: Function that takes `LevelInfo` and returns `true` if the level matches
    ///
    /// # Returns
    /// - `Some(LevelInfo)` if a matching level is found
    /// - `None` if no level matches or the book is empty
    ///
    /// # Performance
    /// Short-circuits on first match, O(M log N) where M is position of match.
    ///
    /// # Examples
    /// ```
    /// use orderbook_rs::OrderBook;
    /// use pricelevel::{Id, Side, TimeInForce};
    ///
    /// let book = OrderBook::<()>::new("BTC/USD");
    /// let _ = book.add_limit_order(Id::new(), 100, 5, Side::Buy, TimeInForce::Gtc, None);
    /// let _ = book.add_limit_order(Id::new(), 99, 15, Side::Buy, TimeInForce::Gtc, None);
    /// let _ = book.add_limit_order(Id::new(), 98, 25, Side::Buy, TimeInForce::Gtc, None);
    ///
    /// // Find first level with quantity > 10
    /// if let Some(level) = book.find_level(Side::Buy, |info| info.quantity > 10) {
    ///     println!("First large level at price: {}", level.price);
    /// }
    ///
    /// // Find first level where cumulative depth exceeds 20
    /// if let Some(level) = book.find_level(Side::Buy, |info| info.cumulative_depth > 20) {
    ///     println!("Depth threshold at: {}", level.price);
    /// }
    /// ```
    pub fn find_level<F>(&self, side: Side, predicate: F) -> Option<LevelInfo>
    where
        F: Fn(&LevelInfo) -> bool,
    {
        self.levels_with_cumulative_depth(side)
            .find(|level| predicate(level))
    }

    /// Get all orders at a specific price level
    pub fn get_orders_at_price(&self, price: u128, side: Side) -> Vec<Arc<OrderType<T>>>
    where
        T: Default,
    {
        trace!(
            "Order book {}: Getting orders at price {} for side {:?}",
            self.symbol, price, side
        );
        let price_levels = match side {
            Side::Buy => &self.bids,
            Side::Sell => &self.asks,
        };

        if let Some(entry) = price_levels.get(&price) {
            entry
                .value()
                .iter_orders()
                .map(|order| Arc::new(self.convert_from_unit_type(&order)))
                .collect()
        } else {
            Vec::new()
        }
    }

    /// Get all orders in the book
    pub fn get_all_orders(&self) -> Vec<Arc<OrderType<T>>>
    where
        T: Default,
    {
        trace!("Order book {}: Getting all orders", self.symbol);
        let mut result = Vec::new();

        // Get all bid orders
        for item in self.bids.iter() {
            let price_level = item.value();
            let converted_orders: Vec<Arc<OrderType<T>>> = price_level
                .iter_orders()
                .map(|order| Arc::new(self.convert_from_unit_type(&order)))
                .collect();
            result.extend(converted_orders);
        }

        // Get all ask orders
        for item in self.asks.iter() {
            let price_level = item.value();
            let converted_orders: Vec<Arc<OrderType<T>>> = price_level
                .iter_orders()
                .map(|order| Arc::new(self.convert_from_unit_type(&order)))
                .collect();
            result.extend(converted_orders);
        }

        result
    }

    /// Get an order by its ID
    pub fn get_order(&self, order_id: Id) -> Option<Arc<OrderType<T>>>
    where
        T: Default,
    {
        // Get the order location without locking
        if let Some(location) = self.order_locations.get(&order_id) {
            let (price, side) = *location;

            let price_levels = match side {
                Side::Buy => &self.bids,
                Side::Sell => &self.asks,
            };

            // Get the price level
            if let Some(entry) = price_levels.get(&price) {
                let price_level = entry.value();
                // Iterate through the orders at this level to find the one with the matching ID
                for order in price_level.iter_orders() {
                    if order.id() == order_id {
                        return Some(Arc::new(self.convert_from_unit_type(&order)));
                    }
                }
            }
        }

        None
    }

    /// Match a market order against the book.
    ///
    /// This is a convenience wrapper that bypasses STP (uses `Hash32::zero()`).
    /// Use [`Self::match_market_order_with_user`] when STP is needed.
    pub fn match_market_order(
        &self,
        order_id: Id,
        quantity: u64,
        side: Side,
    ) -> Result<MatchResult, OrderBookError> {
        self.match_market_order_with_user(order_id, quantity, side, Hash32::zero())
    }

    /// Match a market order against the book with Self-Trade Prevention.
    ///
    /// When STP is enabled and `user_id` is non-zero, the matching engine
    /// checks resting orders for same-user conflicts before executing fills.
    ///
    /// # Arguments
    /// * `order_id` — Unique identifier for this market order.
    /// * `quantity` — Quantity to match.
    /// * `side` — Buy or Sell.
    /// * `user_id` — Owner of the incoming order for STP checks.
    ///   Pass `Hash32::zero()` to bypass STP.
    ///
    /// # Errors
    /// Returns [`OrderBookError::InsufficientLiquidity`] when no liquidity
    /// is available, or [`OrderBookError::SelfTradePrevented`] when STP
    /// cancels the taker before any fills occur.
    pub fn match_market_order_with_user(
        &self,
        order_id: Id,
        quantity: u64,
        side: Side,
        user_id: Hash32,
    ) -> Result<MatchResult, OrderBookError> {
        trace!(
            "Order book {}: Matching market order {} for {} at side {:?}",
            self.symbol, order_id, quantity, side
        );
        let match_result =
            OrderBook::<T>::match_order_with_user(self, order_id, side, quantity, None, user_id)?;

        // Trigger trade listener if there are transactions
        if !match_result.trades().as_vec().is_empty()
            && let Some(ref listener) = self.trade_listener
        {
            let trade_result = TradeResult::with_fees(
                self.symbol.clone(),
                match_result.clone(),
                self.fee_schedule,
            );
            listener(&trade_result);
        }

        Ok(match_result)
    }

    /// Attempts to match a limit order in the order book.
    ///
    /// This is a convenience wrapper that bypasses STP (uses `Hash32::zero()`).
    /// Use [`Self::match_limit_order_with_user`] when STP is needed.
    ///
    /// # Parameters
    /// - `order_id`: The unique identifier of the order to be matched.
    /// - `quantity`: The quantity of the order to be matched.
    /// - `side`: The side of the order book (e.g., Buy or Sell) on which the order resides.
    /// - `limit_price`: The maximum (for Buy) or minimum (for Sell) acceptable price
    ///   for the order.
    ///
    /// # Returns
    /// - `Ok(MatchResult)`: If the order is successfully matched, returning information
    ///   about the match, including possibly filled quantities and pricing details.
    /// - `Err(OrderBookError)`: If the order cannot be matched due to an error.
    pub fn match_limit_order(
        &self,
        order_id: Id,
        quantity: u64,
        side: Side,
        limit_price: u128,
    ) -> Result<MatchResult, OrderBookError> {
        self.match_limit_order_with_user(order_id, quantity, side, limit_price, Hash32::zero())
    }

    /// Attempts to match a limit order with Self-Trade Prevention support.
    ///
    /// # Arguments
    /// * `order_id` — Unique identifier for this limit order.
    /// * `quantity` — Quantity to match.
    /// * `side` — Buy or Sell.
    /// * `limit_price` — Maximum (Buy) or minimum (Sell) acceptable price.
    /// * `user_id` — Owner of the incoming order for STP checks.
    ///   Pass `Hash32::zero()` to bypass STP.
    ///
    /// # Errors
    /// Returns [`OrderBookError::SelfTradePrevented`] when STP cancels the
    /// taker before any fills occur.
    pub fn match_limit_order_with_user(
        &self,
        order_id: Id,
        quantity: u64,
        side: Side,
        limit_price: u128,
        user_id: Hash32,
    ) -> Result<MatchResult, OrderBookError> {
        trace!(
            "Order book {}: Matching limit order {} for {} at side {:?} with limit price {}",
            self.symbol, order_id, quantity, side, limit_price
        );
        let match_result = OrderBook::<T>::match_order_with_user(
            self,
            order_id,
            side,
            quantity,
            Some(limit_price),
            user_id,
        )?;

        // Trigger trade listener if there are transactions
        if !match_result.trades().as_vec().is_empty()
            && let Some(ref listener) = self.trade_listener
        {
            let trade_result = TradeResult::with_fees(
                self.symbol.clone(),
                match_result.clone(),
                self.fee_schedule,
            );
            listener(&trade_result);
        }

        Ok(match_result)
    }

    /// Create a snapshot of the current order book state
    pub fn create_snapshot(&self, depth: usize) -> OrderBookSnapshot {
        // Get all bid prices and sort them in descending order
        let mut bid_prices: Vec<u128> = self.bids.iter().map(|item| *item.key()).collect();
        bid_prices.sort_by(|a, b| b.cmp(a)); // Descending order
        bid_prices.truncate(depth);

        // Get all ask prices and sort them in ascending order
        let mut ask_prices: Vec<u128> = self.asks.iter().map(|item| *item.key()).collect();
        ask_prices.sort(); // Ascending order
        ask_prices.truncate(depth);

        let mut bid_levels = Vec::with_capacity(bid_prices.len());
        let mut ask_levels = Vec::with_capacity(ask_prices.len());

        // Create snapshots for each bid level
        for price in bid_prices {
            if let Some(entry) = self.bids.get(&price) {
                bid_levels.push(entry.value().snapshot());
            }
        }

        // Create snapshots for each ask level
        for price in ask_prices {
            if let Some(entry) = self.asks.get(&price) {
                ask_levels.push(entry.value().snapshot());
            }
        }

        OrderBookSnapshot {
            symbol: self.symbol.clone(),
            timestamp: current_time_millis(),
            bids: bid_levels,
            asks: ask_levels,
        }
    }

    /// Create a checksum-protected snapshot package of the entire book.
    pub fn create_snapshot_package(
        &self,
        depth: usize,
    ) -> Result<OrderBookSnapshotPackage, OrderBookError> {
        let snapshot = self.create_snapshot(depth);
        OrderBookSnapshotPackage::new(snapshot)
    }

    /// Serialize a checksum-protected snapshot package to JSON.
    pub fn snapshot_to_json(&self, depth: usize) -> Result<String, OrderBookError> {
        self.create_snapshot_package(depth)?.to_json()
    }

    /// Restore the book state from a checksum-validated snapshot package.
    pub fn restore_from_snapshot_package(
        &self,
        package: OrderBookSnapshotPackage,
    ) -> Result<(), OrderBookError> {
        self.restore_from_snapshot(package.into_snapshot()?)
    }

    /// Restore the book state from a JSON payload containing a checksum-protected snapshot package.
    pub fn restore_from_snapshot_json(&self, data: &str) -> Result<(), OrderBookError> {
        let package = OrderBookSnapshotPackage::from_json(data)?;
        self.restore_from_snapshot_package(package)
    }

    /// Restore the book state from a snapshot, without checksum validation.
    pub fn restore_from_snapshot(&self, snapshot: OrderBookSnapshot) -> Result<(), OrderBookError> {
        if snapshot.symbol != self.symbol {
            return Err(OrderBookError::InvalidOperation {
                message: format!(
                    "Snapshot symbol {} does not match order book symbol {}",
                    snapshot.symbol, self.symbol
                ),
            });
        }

        self.cache.invalidate();

        // Clear all existing data
        while let Some(entry) = self.bids.pop_front() {
            drop(entry);
        }
        while let Some(entry) = self.asks.pop_front() {
            drop(entry);
        }
        self.order_locations.clear();
        self.user_orders.clear();
        self.has_traded.store(false, Ordering::Relaxed);
        self.last_trade_price.store(0);
        self.has_market_close.store(false, Ordering::Relaxed);
        self.market_close_timestamp.store(0, Ordering::Relaxed);

        for level_snapshot in snapshot.bids {
            let price = level_snapshot.price();
            let price_level = PriceLevel::from_snapshot(level_snapshot)
                .map_err(OrderBookError::PriceLevelError)?;
            let arc_level = Arc::new(price_level);
            self.bids.insert(price, arc_level);
        }

        for level_snapshot in snapshot.asks {
            let price = level_snapshot.price();
            let price_level = PriceLevel::from_snapshot(level_snapshot)
                .map_err(OrderBookError::PriceLevelError)?;
            let arc_level = Arc::new(price_level);
            self.asks.insert(price, arc_level);
        }

        // Rebuild order location and user_orders maps
        for item in self.bids.iter() {
            let price = *item.key();
            let level = item.value();
            for order in level.iter_orders() {
                self.order_locations.insert(order.id(), (price, Side::Buy));
                self.track_user_order(order.user_id(), order.id());
            }
        }

        for item in self.asks.iter() {
            let price = *item.key();
            let level = item.value();
            for order in level.iter_orders() {
                self.order_locations.insert(order.id(), (price, Side::Sell));
                self.track_user_order(order.user_id(), order.id());
            }
        }

        Ok(())
    }

    /// Creates an enriched snapshot with pre-calculated metrics
    ///
    /// This provides better performance than creating a snapshot and calculating
    /// metrics separately, as it computes everything in a single pass through the data.
    /// All metrics are calculated by default.
    ///
    /// # Arguments
    /// - `depth`: Maximum number of price levels to include on each side
    ///
    /// # Returns
    /// `EnrichedSnapshot` with all metrics pre-calculated
    ///
    /// # Performance
    /// O(N) where N is depth, single pass through data for all metrics.
    ///
    /// # Examples
    /// ```
    /// use orderbook_rs::OrderBook;
    /// use pricelevel::{Id, Side, TimeInForce};
    ///
    /// let book = OrderBook::<()>::new("BTC/USD");
    /// let _ = book.add_limit_order(Id::new(), 100, 10, Side::Buy, TimeInForce::Gtc, None);
    /// let _ = book.add_limit_order(Id::new(), 101, 10, Side::Sell, TimeInForce::Gtc, None);
    ///
    /// let snapshot = book.enriched_snapshot(10);
    ///
    /// if let Some(mid) = snapshot.mid_price {
    ///     println!("Mid price: {}", mid);
    /// }
    /// if let Some(spread) = snapshot.spread_bps {
    ///     println!("Spread: {} bps", spread);
    /// }
    /// println!("Bid depth: {}", snapshot.bid_depth_total);
    /// println!("Imbalance: {}", snapshot.order_book_imbalance);
    /// ```
    #[must_use]
    pub fn enriched_snapshot(&self, depth: usize) -> EnrichedSnapshot {
        self.enriched_snapshot_with_metrics(depth, MetricFlags::ALL)
    }

    /// Creates an enriched snapshot with custom metric selection
    ///
    /// Allows you to specify which metrics to calculate for optimization.
    /// Only the selected metrics will be computed, others will have default values.
    ///
    /// # Arguments
    /// - `depth`: Maximum number of price levels to include on each side
    /// - `flags`: Bitflags specifying which metrics to calculate
    ///
    /// # Returns
    /// `EnrichedSnapshot` with selected metrics calculated
    ///
    /// # Performance
    /// O(N) where N is depth, but faster than `enriched_snapshot()` if fewer metrics selected.
    ///
    /// # Examples
    /// ```
    /// use orderbook_rs::{OrderBook, MetricFlags};
    /// use pricelevel::{Id, Side, TimeInForce};
    ///
    /// let book = OrderBook::<()>::new("BTC/USD");
    /// let _ = book.add_limit_order(Id::new(), 100, 10, Side::Buy, TimeInForce::Gtc, None);
    /// let _ = book.add_limit_order(Id::new(), 101, 10, Side::Sell, TimeInForce::Gtc, None);
    ///
    /// // Calculate only mid price and spread for performance
    /// let snapshot = book.enriched_snapshot_with_metrics(
    ///     10,
    ///     MetricFlags::MID_PRICE | MetricFlags::SPREAD
    /// );
    ///
    /// assert!(snapshot.mid_price.is_some());
    /// assert!(snapshot.spread_bps.is_some());
    /// ```
    #[must_use]
    pub fn enriched_snapshot_with_metrics(
        &self,
        depth: usize,
        flags: MetricFlags,
    ) -> EnrichedSnapshot {
        // Get all bid prices and sort them in descending order
        let mut bid_prices: Vec<u128> = self.bids.iter().map(|item| *item.key()).collect();
        bid_prices.sort_by(|a, b| b.cmp(a)); // Descending order
        bid_prices.truncate(depth);

        // Get all ask prices and sort them in ascending order
        let mut ask_prices: Vec<u128> = self.asks.iter().map(|item| *item.key()).collect();
        ask_prices.sort(); // Ascending order
        ask_prices.truncate(depth);

        let mut bid_levels = Vec::with_capacity(bid_prices.len());
        let mut ask_levels = Vec::with_capacity(ask_prices.len());

        // Create snapshots for each bid level
        for price in bid_prices {
            if let Some(entry) = self.bids.get(&price) {
                bid_levels.push(entry.value().snapshot());
            }
        }

        // Create snapshots for each ask level
        for price in ask_prices {
            if let Some(entry) = self.asks.get(&price) {
                ask_levels.push(entry.value().snapshot());
            }
        }

        // Create enriched snapshot with pre-calculated metrics
        EnrichedSnapshot::with_metrics(
            self.symbol.clone(),
            current_time_millis(),
            bid_levels,
            ask_levels,
            depth, // Use depth for VWAP calculation
            depth, // Use depth for imbalance calculation
            flags,
        )
    }

    /// Get the total volume at each price level
    pub fn get_volume_by_price(&self) -> (HashMap<u128, u64>, HashMap<u128, u64>) {
        let mut bid_volumes = HashMap::new();
        let mut ask_volumes = HashMap::new();

        // Calculate bid volumes
        for item in self.bids.iter() {
            let price = *item.key();
            let price_level = item.value();
            bid_volumes.insert(price, price_level.total_quantity().unwrap_or(0));
        }

        // Calculate ask volumes
        for item in self.asks.iter() {
            let price = *item.key();
            let price_level = item.value();
            ask_volumes.insert(price, price_level.total_quantity().unwrap_or(0));
        }

        (bid_volumes, ask_volumes)
    }

    /// Get an Arc reference to the bids as a DashMap
    ///
    /// # Note
    /// Creates a snapshot by collecting all entries into a DashMap
    pub fn get_bids(&self) -> Arc<DashMap<u128, Arc<PriceLevel>>> {
        let map = DashMap::new();
        for entry in self.bids.iter() {
            map.insert(*entry.key(), entry.value().clone());
        }
        Arc::new(map)
    }

    /// Get an Arc reference to the asks as a DashMap
    ///
    /// # Note
    /// Creates a snapshot by collecting all entries into a DashMap
    pub fn get_asks(&self) -> Arc<DashMap<u128, Arc<PriceLevel>>> {
        let map = DashMap::new();
        for entry in self.asks.iter() {
            map.insert(*entry.key(), entry.value().clone());
        }
        Arc::new(map)
    }

    /// Get a BTreeMap of bids with price as key and PriceLevel as value
    pub fn get_bt_bids(&self) -> BTreeMap<u128, PriceLevel> {
        self.bids
            .iter()
            .map(|entry| {
                let price = *entry.key();
                let snapshot = entry.value().snapshot();
                let price_level = PriceLevel::from(&snapshot);
                (price, price_level)
            })
            .collect()
    }

    /// Get a BTreeMap of asks with price as key and PriceLevel as value
    pub fn get_bt_asks(&self) -> BTreeMap<u128, PriceLevel> {
        self.asks
            .iter()
            .map(|entry| {
                let price = *entry.key();
                let snapshot = entry.value().snapshot();
                let price_level = PriceLevel::from(&snapshot);
                (price, price_level)
            })
            .collect()
    }

    /// Get an Arc reference to the order_locations DashMap
    pub fn get_order_locations_arc(&self) -> Arc<DashMap<Id, (u128, Side)>> {
        Arc::new(self.order_locations.clone())
    }

    /// Computes comprehensive depth statistics for a side of the order book
    ///
    /// Analyzes the top N price levels to provide detailed statistical metrics
    /// about liquidity distribution, including volume, average sizes, weighted
    /// prices, and variability measures.
    ///
    /// # Arguments
    /// - `side`: The side to analyze (Buy for bids, Sell for asks)
    /// - `levels`: Maximum number of top levels to analyze (0 = all levels)
    ///
    /// # Returns
    /// `DepthStats` containing comprehensive statistics. Returns zero stats if no levels exist.
    ///
    /// # Performance
    /// O(N) where N is the number of levels analyzed.
    ///
    /// # Examples
    /// ```
    /// use orderbook_rs::OrderBook;
    /// use pricelevel::{Id, Side, TimeInForce};
    ///
    /// let book = OrderBook::<()>::new("BTC/USD");
    /// let _ = book.add_limit_order(Id::new(), 100, 10, Side::Buy, TimeInForce::Gtc, None);
    /// let _ = book.add_limit_order(Id::new(), 99, 20, Side::Buy, TimeInForce::Gtc, None);
    /// let _ = book.add_limit_order(Id::new(), 98, 30, Side::Buy, TimeInForce::Gtc, None);
    ///
    /// let stats = book.depth_statistics(Side::Buy, 10);
    /// println!("Total volume: {}", stats.total_volume);
    /// println!("Average level size: {:.2}", stats.avg_level_size);
    /// println!("Weighted avg price: {:.2}", stats.weighted_avg_price);
    /// ```
    #[must_use]
    pub fn depth_statistics(&self, side: Side, levels: usize) -> DepthStats {
        let price_levels = match side {
            Side::Buy => &self.bids,
            Side::Sell => &self.asks,
        };

        if price_levels.is_empty() {
            return DepthStats::zero();
        }

        let iter: Box<dyn Iterator<Item = _>> = match side {
            Side::Buy => Box::new(price_levels.iter().rev()),
            Side::Sell => Box::new(price_levels.iter()),
        };

        let mut total_volume = 0u64;
        let mut weighted_price_sum = 0u128;
        let mut sizes = Vec::new();
        let mut min_size = u64::MAX;
        let mut max_size = 0u64;
        let mut count = 0usize;

        for entry in iter {
            if levels > 0 && count >= levels {
                break;
            }

            let price = *entry.key();
            let quantity = entry.value().total_quantity().unwrap_or(0);

            if quantity == 0 {
                continue;
            }

            total_volume = total_volume.saturating_add(quantity);
            weighted_price_sum =
                weighted_price_sum.saturating_add(price.saturating_mul(quantity as u128));
            sizes.push(quantity);
            min_size = min_size.min(quantity);
            max_size = max_size.max(quantity);
            count += 1;
        }

        if count == 0 || total_volume == 0 {
            return DepthStats::zero();
        }

        let avg_level_size = total_volume as f64 / count as f64;
        let weighted_avg_price = weighted_price_sum as f64 / total_volume as f64;

        // Calculate standard deviation
        let variance: f64 = sizes
            .iter()
            .map(|&size| {
                let diff = size as f64 - avg_level_size;
                diff * diff
            })
            .sum::<f64>()
            / count as f64;
        let std_dev = variance.sqrt();

        DepthStats {
            total_volume,
            levels_count: count,
            avg_level_size,
            weighted_avg_price,
            min_level_size: if min_size == u64::MAX { 0 } else { min_size },
            max_level_size: max_size,
            std_dev_level_size: std_dev,
        }
    }

    /// Calculates buy and sell pressure based on total volume on each side
    ///
    /// Returns the total quantity on the bid and ask sides as a measure
    /// of market pressure. Higher values indicate stronger interest.
    ///
    /// # Returns
    /// Tuple of `(buy_pressure, sell_pressure)` where each value is the total
    /// quantity available on that side (in units).
    ///
    /// # Performance
    /// O(N + M) where N is bid levels and M is ask levels.
    ///
    /// # Examples
    /// ```
    /// use orderbook_rs::OrderBook;
    /// use pricelevel::{Id, Side, TimeInForce};
    ///
    /// let book = OrderBook::<()>::new("BTC/USD");
    /// let _ = book.add_limit_order(Id::new(), 100, 50, Side::Buy, TimeInForce::Gtc, None);
    /// let _ = book.add_limit_order(Id::new(), 101, 30, Side::Sell, TimeInForce::Gtc, None);
    ///
    /// let (buy_pressure, sell_pressure) = book.buy_sell_pressure();
    /// println!("Buy: {}, Sell: {}", buy_pressure, sell_pressure);
    ///
    /// if buy_pressure > sell_pressure {
    ///     println!("More buying interest");
    /// }
    /// ```
    #[must_use]
    pub fn buy_sell_pressure(&self) -> (u64, u64) {
        let buy_pressure: u64 = self
            .bids
            .iter()
            .map(|entry| entry.value().total_quantity().unwrap_or(0))
            .sum();

        let sell_pressure: u64 = self
            .asks
            .iter()
            .map(|entry| entry.value().total_quantity().unwrap_or(0))
            .sum();

        (buy_pressure, sell_pressure)
    }

    /// Detects if the order book is thin (has low liquidity)
    ///
    /// A thin book has insufficient liquidity, which can lead to high slippage
    /// and price volatility. This method checks if the total volume in the top
    /// N levels falls below a threshold.
    ///
    /// # Arguments
    /// - `threshold`: Minimum total volume required (in units)
    /// - `levels`: Number of top levels to check on each side
    ///
    /// # Returns
    /// `true` if either side has insufficient liquidity, `false` otherwise
    ///
    /// # Performance
    /// O(N) where N is levels to check.
    ///
    /// # Examples
    /// ```
    /// use orderbook_rs::OrderBook;
    /// use pricelevel::{Id, Side, TimeInForce};
    ///
    /// let book = OrderBook::<()>::new("BTC/USD");
    /// let _ = book.add_limit_order(Id::new(), 100, 5, Side::Buy, TimeInForce::Gtc, None);
    /// let _ = book.add_limit_order(Id::new(), 101, 5, Side::Sell, TimeInForce::Gtc, None);
    ///
    /// if book.is_thin_book(100, 5) {
    ///     println!("Warning: Thin book detected - high slippage risk!");
    /// }
    /// ```
    #[must_use]
    pub fn is_thin_book(&self, threshold: u64, levels: usize) -> bool {
        let bid_stats = self.depth_statistics(Side::Buy, levels);
        let ask_stats = self.depth_statistics(Side::Sell, levels);

        bid_stats.total_volume < threshold || ask_stats.total_volume < threshold
    }

    /// Calculates depth distribution histogram for a side
    ///
    /// Divides the order book depth into equal price bins and calculates
    /// the total volume in each bin. Useful for visualizing liquidity
    /// distribution and identifying concentration points.
    ///
    /// # Arguments
    /// - `side`: The side to analyze (Buy for bids, Sell for asks)
    /// - `bins`: Number of bins to divide the depth into (must be > 0)
    ///
    /// # Returns
    /// Vector of `DistributionBin` containing price ranges and volumes.
    /// Returns empty vector if bins is 0 or no levels exist.
    ///
    /// # Performance
    /// O(N) where N is total number of levels.
    ///
    /// # Examples
    /// ```
    /// use orderbook_rs::OrderBook;
    /// use pricelevel::{Id, Side, TimeInForce};
    ///
    /// let book = OrderBook::<()>::new("BTC/USD");
    /// for i in 0..10 {
    ///     let price = 100 - i;
    ///     let _ = book.add_limit_order(Id::new(), price, 10, Side::Buy, TimeInForce::Gtc, None);
    /// }
    ///
    /// let distribution = book.depth_distribution(Side::Buy, 5);
    /// for bin in distribution {
    ///     println!("Price {}-{}: {} units in {} levels",
    ///              bin.min_price, bin.max_price, bin.volume, bin.level_count);
    /// }
    /// ```
    #[must_use]
    pub fn depth_distribution(&self, side: Side, bins: usize) -> Vec<DistributionBin> {
        if bins == 0 {
            return Vec::new();
        }

        let price_levels = match side {
            Side::Buy => &self.bids,
            Side::Sell => &self.asks,
        };

        if price_levels.is_empty() {
            return Vec::new();
        }

        // Find min and max prices
        let mut min_price = u128::MAX;
        let mut max_price = 0u128;

        for entry in price_levels.iter() {
            let price = *entry.key();
            min_price = min_price.min(price);
            max_price = max_price.max(price);
        }

        if min_price == u128::MAX || max_price < min_price {
            return Vec::new();
        }

        // Calculate bin width
        let price_range = max_price - min_price;
        let bin_width = if price_range == 0 {
            1
        } else {
            price_range.div_ceil(bins as u128) // Ceiling division
        };

        // Initialize bins
        let mut distribution = Vec::with_capacity(bins);
        for i in 0..bins {
            let bin_min = min_price + (i as u128 * bin_width);
            let bin_max = if i == bins - 1 {
                max_price + 1 // Make last bin inclusive
            } else {
                bin_min + bin_width
            };

            distribution.push(DistributionBin {
                min_price: bin_min,
                max_price: bin_max,
                volume: 0,
                level_count: 0,
            });
        }

        // Fill bins with data
        for entry in price_levels.iter() {
            let price = *entry.key();
            let quantity = entry.value().total_quantity().unwrap_or(0);

            if quantity == 0 {
                continue;
            }

            // Find which bin this price belongs to
            let bin_index = if price >= max_price {
                bins - 1
            } else {
                ((price - min_price) / bin_width).min((bins - 1) as u128) as usize
            };

            distribution[bin_index].volume =
                distribution[bin_index].volume.saturating_add(quantity);
            distribution[bin_index].level_count += 1;
        }

        distribution
    }
}

// Implementation of RepricingOperations trait for OrderBook
#[cfg(feature = "special_orders")]
use crate::orderbook::repricing::{
    RepricingOperations, RepricingResult, calculate_pegged_price, calculate_trailing_stop_price,
};

#[cfg(feature = "special_orders")]
impl<T> RepricingOperations<T> for OrderBook<T>
where
    T: Clone + Default + Send + Sync + 'static,
{
    /// Re-prices all pegged orders based on current market conditions
    fn reprice_pegged_orders(&self) -> Result<usize, OrderBookError> {
        let pegged_ids = self.special_order_tracker.pegged_order_ids();
        if pegged_ids.is_empty() {
            return Ok(0);
        }

        let best_bid = self.best_bid();
        let best_ask = self.best_ask();
        let mid_price = self.mid_price().map(|p| p as u128);
        let last_trade = if self.has_traded.load(Ordering::Relaxed) {
            Some(self.last_trade_price.load())
        } else {
            None
        };

        let mut repriced_count = 0;

        for order_id in pegged_ids {
            if let Some(order) = self.get_order(order_id) {
                if let OrderType::PeggedOrder {
                    price: current_price,
                    side,
                    reference_price_offset,
                    reference_price_type,
                    ..
                } = order.as_ref()
                    && let Some(new_price) = calculate_pegged_price(
                        *reference_price_type,
                        *reference_price_offset,
                        *side,
                        best_bid,
                        best_ask,
                        mid_price,
                        last_trade,
                    )
                    && new_price != current_price.as_u128()
                {
                    let update = OrderUpdate::UpdatePrice {
                        order_id,
                        new_price: pricelevel::Price::new(new_price),
                    };
                    if self.update_order(update).is_ok() {
                        repriced_count += 1;
                        trace!(
                            "Re-priced pegged order {} from {} to {}",
                            order_id, current_price, new_price
                        );
                    }
                }
            } else {
                // Order no longer exists, unregister it
                self.special_order_tracker
                    .unregister_pegged_order(&order_id);
            }
        }

        Ok(repriced_count)
    }

    /// Re-prices all trailing stop orders based on current market conditions
    fn reprice_trailing_stops(&self) -> Result<usize, OrderBookError> {
        let trailing_ids = self.special_order_tracker.trailing_stop_ids();
        if trailing_ids.is_empty() {
            return Ok(0);
        }

        let mut repriced_count = 0;

        for order_id in trailing_ids {
            if let Some(order) = self.get_order(order_id) {
                if let OrderType::TrailingStop {
                    price: current_stop_price,
                    side,
                    trail_amount,
                    last_reference_price,
                    ..
                } = order.as_ref()
                {
                    // Get current market price based on side
                    let current_market_price = match side {
                        Side::Sell => self.best_bid(), // Sell stop tracks bid (market high)
                        Side::Buy => self.best_ask(),  // Buy stop tracks ask (market low)
                    };

                    if let Some(market_price) = current_market_price
                        && let Some((new_stop_price, new_reference)) = calculate_trailing_stop_price(
                            *side,
                            current_stop_price.as_u128(),
                            trail_amount.as_u64(),
                            last_reference_price.as_u128(),
                            market_price,
                        )
                    {
                        // Update the order with new stop price
                        // We need to update both price and last_reference_price
                        // For now, we update the price; the reference price update
                        // requires modifying the order directly
                        let update = OrderUpdate::UpdatePrice {
                            order_id,
                            new_price: pricelevel::Price::new(new_stop_price),
                        };
                        if self.update_order(update).is_ok() {
                            repriced_count += 1;
                            trace!(
                                "Re-priced trailing stop {} from {} to {} (ref: {} -> {})",
                                order_id,
                                current_stop_price,
                                new_stop_price,
                                last_reference_price,
                                new_reference
                            );
                        }
                    }
                }
            } else {
                // Order no longer exists, unregister it
                self.special_order_tracker
                    .unregister_trailing_stop(&order_id);
            }
        }

        Ok(repriced_count)
    }

    /// Re-prices all special orders (both pegged and trailing stops)
    fn reprice_special_orders(&self) -> Result<RepricingResult, OrderBookError> {
        let pegged_count = self.reprice_pegged_orders()?;
        let trailing_count = self.reprice_trailing_stops()?;

        Ok(RepricingResult {
            pegged_orders_repriced: pegged_count,
            trailing_stops_repriced: trailing_count,
            failed_orders: Vec::new(),
        })
    }

    /// Checks if a trailing stop order should be triggered
    fn should_trigger_trailing_stop(
        &self,
        order: &OrderType<T>,
        current_market_price: u128,
    ) -> bool {
        if let OrderType::TrailingStop {
            price: stop_price,
            side,
            ..
        } = order
        {
            match side {
                // Sell trailing stop triggers when market falls to or below stop price
                Side::Sell => current_market_price <= stop_price.as_u128(),
                // Buy trailing stop triggers when market rises to or above stop price
                Side::Buy => current_market_price >= stop_price.as_u128(),
            }
        } else {
            false
        }
    }
}

#[cfg(feature = "special_orders")]
impl<T> OrderBook<T>
where
    T: Clone + Default + Send + Sync + 'static,
{
    /// Returns the number of tracked pegged orders
    pub fn pegged_order_count(&self) -> usize {
        self.special_order_tracker.pegged_order_count()
    }

    /// Returns the number of tracked trailing stop orders
    pub fn trailing_stop_count(&self) -> usize {
        self.special_order_tracker.trailing_stop_count()
    }

    /// Returns all tracked pegged order IDs
    pub fn pegged_order_ids(&self) -> Vec<Id> {
        self.special_order_tracker.pegged_order_ids()
    }

    /// Returns all tracked trailing stop order IDs
    pub fn trailing_stop_ids(&self) -> Vec<Id> {
        self.special_order_tracker.trailing_stop_ids()
    }
}
