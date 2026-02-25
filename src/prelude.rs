/******************************************************************************
   Author: Joaquín Béjar García
   Email: jb@taunais.com
   Date: 2/10/25
******************************************************************************/

//! Prelude module that re-exports commonly used types and traits.
//!
//! This module provides a convenient way to import the most commonly used
//! types, traits, and functions from the orderbook-rs crate. Instead of
//! importing each type individually, you can use:
//!
//! ```rust
//! use orderbook_rs::prelude::*;
//! ```
//!
//! This will import all the essential types needed for working with the order book.

// Core order book types
pub use crate::orderbook::OrderBook;
pub use crate::orderbook::OrderBookError;
pub use crate::orderbook::manager::{BookManager, BookManagerStd, BookManagerTokio};

// Iterator types
pub use crate::orderbook::iterators::LevelInfo;

// Market impact and simulation types
pub use crate::orderbook::market_impact::{MarketImpact, OrderSimulation};

// Snapshot types
pub use crate::orderbook::snapshot::{EnrichedSnapshot, MetricFlags, OrderBookSnapshot};

// Statistics types
pub use crate::orderbook::statistics::{DepthStats, DistributionBin};

// Trade-related types
pub use crate::orderbook::trade::{
    TradeEvent, TradeInfo, TradeListener, TradeResult, TransactionInfo,
};

// Order types and enums from pricelevel
pub use pricelevel::{Id, OrderType, Side, TimeInForce};

// Legacy alias for backward compatibility
pub use crate::OrderId;

// NATS integration types
#[cfg(feature = "nats")]
pub use crate::orderbook::nats::NatsTradePublisher;

// Utility functions
pub use crate::utils::current_time_millis;

// Type aliases for common use cases
pub use crate::{DefaultOrderBook, DefaultOrderType, LegacyOrderBook, LegacyOrderType};
