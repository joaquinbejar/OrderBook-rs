//! Core types for the Sequencer subsystem.
//!
//! This module defines the command, event, and result types used by the
//! single-threaded Sequencer (LMAX Disruptor pattern). These types are
//! also used by the `Journal` trait for write-ahead
//! logging and deterministic replay.

use crate::orderbook::mass_cancel::MassCancelResult;
use crate::orderbook::trade::TradeResult;
use pricelevel::{Hash32, Id, OrderType, OrderUpdate, Side, TimestampMs};
use serde::{Deserialize, Serialize};

/// A command submitted to the Sequencer for total-ordered execution.
///
/// Each variant maps to a single order book operation. The Sequencer
/// assigns a monotonic sequence number and nanosecond timestamp before
/// executing the command against the underlying `OrderBook`.
///
/// The generic parameter `T` represents extra fields carried by
/// `OrderType<T>` (e.g., custom metadata per order).
///
/// This enum is `#[non_exhaustive]`: new commands are added over time, so
/// downstream `match` expressions must include a wildcard arm. This makes
/// future variant additions source-compatible; wire compatibility is
/// preserved separately by only ever appending variants (existing bincode
/// variant indices never shift).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub enum SequencerCommand<T> {
    /// Submit a new order to the book.
    AddOrder(OrderType<T>),

    /// Cancel an existing order by its identifier.
    CancelOrder(Id),

    /// Update an existing order (price, quantity, or both).
    UpdateOrder(OrderUpdate),

    /// Submit an aggressive market order that sweeps available liquidity.
    MarketOrder {
        /// The order identifier.
        id: Id,
        /// The quantity to fill.
        quantity: u64,
        /// The side of the market order (Buy sweeps asks, Sell sweeps bids).
        side: Side,
    },

    /// Submit an aggressive market order specified by quote-notional
    /// amount. Walks the opposite side until `amount` is consumed, the
    /// book is exhausted, or — when `lot_size` is configured on the
    /// destination book — the residual notional cannot fund another
    /// whole lot. This is the additive Binance-style `quoteOrderQty`
    /// counterpart to [`Self::MarketOrder`].
    ///
    /// Adding this variant is non-breaking: existing journals replay
    /// unchanged. Journals carrying `MarketOrderByAmount` will fail to
    /// decode against older binaries — this matches the precedent for
    /// previous `SequencerCommand` variant rollouts.
    MarketOrderByAmount {
        /// The order identifier.
        id: Id,
        /// The quote-asset value to consume from the book.
        amount: u128,
        /// The side of the market order (Buy sweeps asks, Sell sweeps bids).
        side: Side,
    },

    /// Cancel all orders in the book.
    CancelAll,

    /// Cancel all orders on the specified side.
    CancelBySide {
        /// The side to cancel (Buy or Sell).
        side: Side,
    },

    /// Cancel all orders belonging to the specified user.
    CancelByUser {
        /// The user identifier whose orders should be cancelled.
        user_id: Hash32,
    },

    /// Cancel all orders within a price range on the specified side.
    CancelByPriceRange {
        /// The side to cancel (Buy or Sell).
        side: Side,
        /// Minimum price (inclusive).
        min_price: u128,
        /// Maximum price (inclusive).
        max_price: u128,
    },

    /// Evict every resting order whose time-in-force has expired as of
    /// `now_ms` (Unix milliseconds), in the engine's deterministic sweep
    /// order. Ferries through [`OrderBook::evict_expired_orders`] on replay.
    ///
    /// [`OrderBook::evict_expired_orders`]:
    /// crate::orderbook::OrderBook::evict_expired_orders
    ///
    /// The journaled `now_ms` is the sole deterministic input: replay MUST
    /// apply the journaled value rather than read the replay clock, so the
    /// sweep reproduces the exact set of evictions on every run. `now_ms` is
    /// a [`TimestampMs`], which is `#[serde(transparent)]` over `u64`, so the
    /// variant encodes to the same bytes a bare millisecond count would in
    /// both JSON and bincode.
    ///
    /// Wire-compatible addition: existing journals replay unchanged and
    /// their bincode variant indices are unaffected because it is appended
    /// after every prior variant. Journals carrying `EvictExpiredOrders`
    /// will fail to decode against older binaries — this matches the
    /// precedent set by [`Self::MarketOrderByAmount`]. No
    /// `ORDERBOOK_SNAPSHOT_FORMAT_VERSION` bump is required (that version
    /// governs the snapshot package format, not the sequencer command enum).
    /// On the Rust API side the variant ships together with
    /// `#[non_exhaustive]` on this enum in 0.10.0, so subsequent additions
    /// are source-compatible as well.
    EvictExpiredOrders {
        /// Caller-supplied cutoff in Unix milliseconds. Every resting order
        /// whose time-in-force has expired at `now_ms` is evicted:
        /// `Gtd(deadline)` when `now_ms >= deadline`, and `Day` when
        /// `now_ms >=` the book's configured market close.
        now_ms: TimestampMs,
    },
}

/// The outcome of executing a [`SequencerCommand`] against the order book.
///
/// Each variant captures the result of the corresponding command, including
/// any generated trades or the reason for rejection.
///
/// Like [`SequencerCommand`], this enum is `#[non_exhaustive]`: new result
/// shapes accompany new commands, so downstream `match` expressions must
/// include a wildcard arm.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub enum SequencerResult {
    /// An order was successfully added to the book.
    OrderAdded {
        /// The identifier of the newly added order.
        order_id: Id,
    },

    /// An order was successfully cancelled.
    OrderCancelled {
        /// The identifier of the cancelled order.
        order_id: Id,
    },

    /// An order was successfully updated.
    OrderUpdated {
        /// The identifier of the updated order.
        order_id: Id,
    },

    /// A trade was executed (possibly partially filled).
    TradeExecuted {
        /// The trade result containing match details, fees, and transactions.
        trade_result: TradeResult,
    },

    /// A mass cancel operation was executed.
    MassCancelled {
        /// The result containing the count and IDs of cancelled orders.
        result: MassCancelResult,
    },

    /// The command was rejected by the order book.
    Rejected {
        /// Human-readable reason for the rejection.
        reason: String,
    },
}

/// A sequenced event emitted by the Sequencer after processing a command.
///
/// Every event carries a monotonically increasing `sequence_num` and a
/// nanosecond-precision `timestamp_ns`, enabling deterministic replay
/// and total ordering of all order book operations.
///
/// The generic parameter `T` matches the extra-fields type of the
/// underlying [`OrderType<T>`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SequencerEvent<T> {
    /// Monotonically increasing sequence number assigned by the Sequencer.
    /// Guaranteed to be unique and gap-free within a single Sequencer
    /// instance.
    pub sequence_num: u64,

    /// Wall-clock timestamp in nanoseconds since the Unix epoch when the
    /// event was created by the Sequencer.
    pub timestamp_ns: u64,

    /// The command that was executed.
    pub command: SequencerCommand<T>,

    /// The result of executing the command.
    pub result: SequencerResult,
}
