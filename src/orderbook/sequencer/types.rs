//! Core types for the Sequencer subsystem.
//!
//! This module defines the command, event, and result types used by the
//! single-threaded Sequencer (LMAX Disruptor pattern). These types are
//! also used by the `Journal` trait for write-ahead
//! logging and deterministic replay.

use crate::orderbook::trade::TradeResult;
use pricelevel::{Id, OrderType, OrderUpdate, Side};
use serde::{Deserialize, Serialize};

/// A command submitted to the Sequencer for total-ordered execution.
///
/// Each variant maps to a single order book operation. The Sequencer
/// assigns a monotonic sequence number and nanosecond timestamp before
/// executing the command against the underlying `OrderBook`.
///
/// The generic parameter `T` represents extra fields carried by
/// `OrderType<T>` (e.g., custom metadata per order).
#[derive(Debug, Clone, Serialize, Deserialize)]
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
}

/// The outcome of executing a [`SequencerCommand`] against the order book.
///
/// Each variant captures the result of the corresponding command, including
/// any generated trades or the reason for rejection.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
