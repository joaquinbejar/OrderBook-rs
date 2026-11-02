//! OrderBook implementation for managing multiple price levels and order matching.
//!
//! Trailing stops (`special_orders`) are held off book as pending stops,
//! trail the last trade price and execute as market orders when the last
//! trade crosses them (#286); see the `stop_orders` module docs.

pub mod book;
/// Pluggable timestamp source for the matching core.
pub mod clock;
pub mod error;
/// Implied volatility calculation from order book prices.
pub mod implied_volatility;
/// Functional-style iterators for order book analysis.
pub mod iterators;
/// Multi-book management with centralized trade event routing.
pub mod manager;
/// Market impact simulation and liquidity analysis.
pub mod market_impact;
pub mod matching;
/// Aggregate statistics for order book analysis.
pub mod statistics;

/// Self-Trade Prevention (STP) types and logic.
pub mod stp;

/// Price level change events for real-time order book updates.
pub mod book_change_event;
mod cache;
mod emission;
/// Contains the core logic for modifying the order book state, such as adding, canceling, or updating orders.
pub mod modifications;
pub mod operations;
mod pool;
mod private;
pub mod snapshot;
#[cfg(test)]
mod tests;
/// Enhanced trade result that includes symbol information
pub mod trade;
mod user_orders;

/// Fee schedule implementation for trading fees
pub mod fees;

/// Mass cancel operations for bulk order removal.
pub mod mass_cancel;

/// Operational Prometheus-style metrics hooks (feature-gated).
pub mod metrics;

/// Order state machine for explicit lifecycle tracking.
pub mod order_state;

/// Closed-taxonomy reject reasons surfaced on `OrderStatus::Rejected`.
pub mod reject_reason;

/// Pre-trade risk layer: per-account counters, configurable limits.
pub mod risk;

/// Pluggable event serialization for NATS publishers and consumers.
pub mod serialization;

/// NATS JetStream trade event publisher.
#[cfg(feature = "nats")]
pub mod nats;

/// NATS JetStream order book change publisher with batching and throttling.
#[cfg(feature = "nats")]
pub mod nats_book_change;

/// Shared NATS publisher plumbing: limits, backoff, task lifecycle.
#[cfg(feature = "nats")]
mod nats_common;

/// Re-pricing logic for pegged orders, and the trailing-stop price helpers
/// (trailing stops themselves are pending off-book stops, #286).
#[cfg(feature = "special_orders")]
pub mod repricing;

/// Off-book trailing stops: storage, trailing, election and execution
/// (#286).
#[cfg(feature = "special_orders")]
pub mod stop_orders;

/// Protection collar for elected stop orders (#302); available in every
/// build so snapshots and replay configurations carry it identically.
pub mod stop_protection;

/// Sequencer subsystem: types, journal trait, and file-based journal.
pub mod sequencer;

pub use book::OrderBook;
pub use clock::{Clock, MonotonicClock, StubClock};
pub use error::{ManagerError, OrderBookError};
pub use fees::{FeeOverflow, FeeSchedule};
pub use implied_volatility::{
    BlackScholes, IVConfig, IVError, IVParams, IVQuality, IVResult, OptionType, PriceSource,
    SolverConfig,
};
pub use iterators::LevelInfo;
pub use market_impact::{MarketImpact, OrderSimulation};
pub use mass_cancel::{EvictionResult, MassCancelFailure, MassCancelResult};
#[cfg(feature = "nats")]
pub use nats::{NatsPublisherError, NatsTradePublisher};
#[cfg(feature = "nats")]
pub use nats_book_change::{BookChangeBatch, BookChangeEntry, NatsBookChangePublisher};
pub use order_state::{CancelReason, OrderStateListener, OrderStateTracker, OrderStatus};
pub use reject_reason::RejectReason;
#[cfg(feature = "special_orders")]
pub use repricing::{RepricingOperations, RepricingResult, SpecialOrderTracker};
pub use risk::{ReferencePriceSource, RiskConfig, RiskState};
#[cfg(feature = "journal")]
pub use sequencer::FileJournal;
pub use sequencer::journal::{Journal, JournalEntry};
pub use sequencer::{
    CommittedPrefix, CommittedTrade, JournalError, SequencerCommand, SequencerEvent,
    SequencerResult,
};
#[cfg(feature = "bincode")]
pub use serialization::{
    BincodeEventSerializer, DEFAULT_MAX_BINCODE_PAYLOAD_BYTES, MAX_BINCODE_PAYLOAD_BYTES_CEILING,
};
pub use serialization::{EventSerializer, JsonEventSerializer, SerializationError};
pub use snapshot::{
    EnrichedSnapshot, MetricFlags, ORDERBOOK_SNAPSHOT_FORMAT_VERSION,
    ORDERBOOK_SNAPSHOT_MIN_READ_VERSION, OrderBookSnapshot, OrderBookSnapshotPackage,
};
pub use statistics::{DepthStats, DistributionBin};
pub use stop_protection::StopProtection;
