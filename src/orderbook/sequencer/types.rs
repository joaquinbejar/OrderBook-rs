//! Core types for the Sequencer subsystem.
//!
//! This module defines the command, event, and result types used by the
//! single-threaded Sequencer (LMAX Disruptor pattern). These types are
//! also used by the `Journal` trait for write-ahead
//! logging and deterministic replay.

use crate::orderbook::error::OrderBookError;
use crate::orderbook::mass_cancel::MassCancelResult;
use crate::orderbook::reject_reason::RejectReason;
use crate::orderbook::stp::STPMode;
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
    ///
    /// Carries only a human-readable reason. When the journal feeds
    /// [`ReplayEngine`](crate::ReplayEngine), prefer
    /// [`Self::RejectedWithCode`]: without a machine-readable code replay
    /// cannot tell a rejection that never touched the book from one that
    /// traded first, so a `Rejected` submit is always skipped on replay.
    Rejected {
        /// Human-readable reason for the rejection.
        reason: String,
    },

    /// The command was rejected by the order book, recorded with its
    /// stable wire-side [`RejectReason`] alongside the human-readable
    /// message.
    ///
    /// A submit — `AddOrder`, `MarketOrder`, `MarketOrderByAmount` — can
    /// execute real trades and *then* fail (an IOC whose remainder is
    /// unfillable, a taker STP cancels after non-self fills), so a
    /// rejection alone does not say whether the book moved. The code lets
    /// [`ReplayEngine`](crate::ReplayEngine) re-execute the rejections it
    /// can reproduce from the book state and its config, and check the
    /// re-executed verdict against the recorded one; see the replay
    /// entry points for the rules. Build it from the typed error with the
    /// `From<&OrderBookError>` impl on this enum, which fills every field.
    /// A hand-built value whose fields disagree with that mapping reopens
    /// the gaps they exist to close.
    ///
    /// Wire-compatible addition (a variant appended to a `#[non_exhaustive]`
    /// enum): existing journals decode unchanged and bincode variant
    /// indices are unaffected. Journals carrying `RejectedWithCode` will
    /// fail to decode against older binaries — this matches the precedent
    /// set by [`SequencerCommand::MarketOrderByAmount`]. The code encodes
    /// as its stable `u16` wire value, as `RejectReason` always does. The
    /// variant governs the sequencer event stream, not the snapshot
    /// package, so `ORDERBOOK_SNAPSHOT_FORMAT_VERSION` is unchanged.
    RejectedWithCode {
        /// Human-readable reason for the rejection.
        reason: String,
        /// The stable reject code, as `RejectReason::from(&OrderBookError)`.
        code: RejectReason,
        /// Whether the engine may already have changed the book when it
        /// returned this error.
        ///
        /// `true` for the errors a command can return *after* mutating:
        /// the unfillable IOC / market remainder
        /// (`InsufficientLiquidity`, `InsufficientLiquidityNotional`), the
        /// STP-cancelled taker (`SelfTradePrevented`) and the
        /// residual-admission failure that follows irreversible trades
        /// (`PriceLevelError`). `false` for every error the engine raises
        /// before it touches the book.
        ///
        /// Replay needs this because the reject code alone does not carry
        /// it. [`RejectReason::Other`]`(0)` is the library's bucket for
        /// errors that are not public rejects, and it holds both the
        /// clock-dependent expired-at-admission rejection — pre-mutation,
        /// and skipped on replay because a replay clock cannot be expected
        /// to reproduce it — and the residual-admission `PriceLevelError`,
        /// which the engine returns only after the sweep's trades are
        /// irreversible. Skipping the second one rebuilds liquidity the
        /// live book consumed, so the flag forces replay to re-execute it.
        may_have_mutated: bool,
        /// The source book's self-trade-prevention mode when the STP scan
        /// produced this rejection; `None` for every other rejection.
        ///
        /// `OrderBookError::SelfTradePrevented` carries the mode that
        /// decided the verdict, so recording it lets replay reject a
        /// mismatched [`ReplayBookConfig`](crate::ReplayBookConfig) up
        /// front. Two modes can refuse the same taker under the same
        /// reject code and still leave different books behind —
        /// `CancelTaker` leaves the same-user maker resting where
        /// `CancelBoth` cancels it — which is a divergence the code alone
        /// cannot see.
        stp_mode: Option<STPMode>,
    },
}

/// Whether the engine may already have changed the book when it returned
/// `err`.
///
/// The three add / market paths that emit trades and *then* return `Err`
/// are the reason this exists: an unfillable IOC or market remainder
/// (`src/orderbook/modifications.rs`, the `is_immediate` branch), a taker
/// STP cancels after non-self fills, and the residual admission that fails
/// once the sweep's trades are already irreversible (which logs at `ERROR`
/// and removes the level it created empty). The first two are identifiable
/// from their reject code; the third is not, because it maps to
/// [`RejectReason::Other`]`(0)` together with pre-mutation errors.
///
/// Deliberately conservative: an error is flagged whenever the engine
/// *can* return it after a mutation, even when a particular call did not
/// mutate (an STP taker cancelled with zero fills, a `PriceLevelError`
/// raised by the pre-sweep level-counter read). Over-flagging costs a
/// re-execution whose verdict is checked; under-flagging silently loses
/// state.
///
/// The match is exhaustive on purpose — no `_` arm — so a new
/// [`OrderBookError`] variant must classify itself at compile time.
#[inline]
#[must_use]
fn may_have_mutated(err: &OrderBookError) -> bool {
    match err {
        // Returned after real fills, or after the level mutation the
        // sweep authorised.
        OrderBookError::InsufficientLiquidity { .. }
        | OrderBookError::InsufficientLiquidityNotional { .. }
        | OrderBookError::SelfTradePrevented { .. }
        | OrderBookError::PriceLevelError(_) => true,
        // Admission and shape checks (all evaluated before the sweep), the
        // operational gates, and the non-reject internal errors. The
        // post-sweep post-only rejection is here too: `pricelevel`
        // structurally refuses to trade for a post-only taker, so that
        // sweep books zero fills before `PriceCrossing` is raised.
        OrderBookError::KillSwitchActive
        | OrderBookError::RiskMaxOpenOrders { .. }
        | OrderBookError::RiskMaxNotional { .. }
        | OrderBookError::RiskPriceBand { .. }
        | OrderBookError::PriceCrossing { .. }
        | OrderBookError::InvalidTickSize { .. }
        | OrderBookError::InvalidLotSize { .. }
        | OrderBookError::InvalidPriceLevel(_)
        | OrderBookError::OrderSizeOutOfRange { .. }
        | OrderBookError::MissingUserId { .. }
        | OrderBookError::DuplicateOrderId { .. }
        | OrderBookError::QuantityOverflow { .. }
        | OrderBookError::ZeroVisibleTranche { .. }
        | OrderBookError::ReserveResidualWouldBeDiscarded { .. }
        | OrderBookError::OrderNotFound(_)
        | OrderBookError::InvalidOperation { .. }
        | OrderBookError::SerializationError { .. }
        | OrderBookError::DeserializationError { .. }
        | OrderBookError::ChecksumMismatch { .. } => false,
        #[cfg(feature = "nats")]
        OrderBookError::NatsPublishError { .. } | OrderBookError::NatsSerializationError { .. } => {
            false
        }
    }
}

/// The self-trade-prevention mode that decided an STP rejection.
///
/// Only [`OrderBookError::SelfTradePrevented`] carries one; every other
/// rejection records `None` and replay's configuration guard stays silent.
#[inline]
#[must_use]
fn recorded_stp_mode(err: &OrderBookError) -> Option<STPMode> {
    match err {
        OrderBookError::SelfTradePrevented { mode, .. } => Some(*mode),
        _ => None,
    }
}

/// Record a rejection with its stable reject code.
///
/// Produces [`SequencerResult::RejectedWithCode`] with `reason` set to
/// the error's `Display` text and `code` to
/// [`RejectReason::from(&OrderBookError)`](RejectReason#impl-From<%26OrderBookError>-for-RejectReason),
/// the same mapping `OrderStatus::Rejected` uses — so a sequencer records
/// the outcome the command API returned in one step and replay can act on
/// it.
///
/// It also fills the two fields the reject code cannot express:
/// `may_have_mutated`, so replay re-executes a rejection that may already
/// have changed the book even when its code says nothing, and `stp_mode`,
/// so replay can check its own configuration against the source book's.
/// This impl is the intended way to build the variant; the fields are
/// public for decoding, not for hand-assembly.
impl From<&OrderBookError> for SequencerResult {
    #[inline]
    fn from(err: &OrderBookError) -> Self {
        Self::RejectedWithCode {
            reason: err.to_string(),
            code: RejectReason::from(err),
            may_have_mutated: may_have_mutated(err),
            stp_mode: recorded_stp_mode(err),
        }
    }
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
