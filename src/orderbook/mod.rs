//! OrderBook implementation for managing multiple price levels and order matching.

pub mod book;
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
/// Contains the core logic for modifying the order book state, such as adding, canceling, or updating orders.
pub mod modifications;
pub mod operations;
mod pool;
mod private;
pub mod snapshot;
mod tests;
/// Enhanced trade result that includes symbol information
pub mod trade;

/// Fee schedule implementation for trading fees
pub mod fees;

/// Mass cancel operations for bulk order removal.
pub mod mass_cancel;

/// NATS JetStream trade event publisher.
#[cfg(feature = "nats")]
pub mod nats;

/// NATS JetStream order book change publisher with batching and throttling.
#[cfg(feature = "nats")]
pub mod nats_book_change;

/// Re-pricing logic for special order types (PeggedOrder and TrailingStop).
#[cfg(feature = "special_orders")]
pub mod repricing;

/// Sequencer subsystem: types, journal trait, and file-based journal.
pub mod sequencer;

pub use book::OrderBook;
pub use error::OrderBookError;
pub use fees::FeeSchedule;
pub use implied_volatility::{
    BlackScholes, IVConfig, IVError, IVParams, IVQuality, IVResult, OptionType, PriceSource,
    SolverConfig,
};
pub use iterators::LevelInfo;
pub use market_impact::{MarketImpact, OrderSimulation};
pub use mass_cancel::MassCancelResult;
#[cfg(feature = "nats")]
pub use nats::NatsTradePublisher;
#[cfg(feature = "nats")]
pub use nats_book_change::{BookChangeBatch, BookChangeEntry, NatsBookChangePublisher};
#[cfg(feature = "special_orders")]
pub use repricing::{RepricingOperations, RepricingResult, SpecialOrderTracker};
#[cfg(feature = "journal")]
pub use sequencer::FileJournal;
pub use sequencer::journal::{Journal, JournalEntry};
pub use sequencer::{JournalError, SequencerCommand, SequencerEvent, SequencerResult};
pub use snapshot::{
    EnrichedSnapshot, MetricFlags, ORDERBOOK_SNAPSHOT_FORMAT_VERSION, OrderBookSnapshot,
    OrderBookSnapshotPackage,
};
pub use statistics::{DepthStats, DistributionBin};
