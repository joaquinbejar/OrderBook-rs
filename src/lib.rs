//! # High-Performance Lock-Free Order Book Engine
//!
//! A high-performance, thread-safe limit order book implementation written in Rust. This project provides a comprehensive order matching engine designed for low-latency trading systems, with a focus on concurrent access patterns and lock-free data structures.
//!
//! ## Key Features
//!
//! - **Lock-Free Architecture**: Built using atomics and lock-free data structures to minimize contention and maximize throughput in high-frequency trading scenarios.
//!
//! - **Multiple Order Types**: Support for various order types including standard limit orders, iceberg orders, post-only, fill-or-kill, immediate-or-cancel, good-till-date, trailing stop, pegged, market-to-limit, and reserve orders with custom replenishment logic.
//!
//! - **Thread-Safe Price Levels**: Each price level can be independently and concurrently modified by multiple threads without blocking.
//!
//! - **Advanced Order Matching**: Efficient matching algorithm for both market and limit orders, correctly handling complex order types and partial fills.
//!
//! - **Performance Metrics**: Built-in statistics tracking for benchmarking and monitoring system performance.
//!
//! - **Memory Efficient**: Designed to scale to millions of orders with minimal memory overhead.
//!
//! ## Design Goals
//!
//! This order book engine is built with the following design principles:
//!
//! 1. **Correctness**: Ensure that all operations maintain the integrity of the order book, even under high concurrency.
//! 2. **Performance**: Optimize for low latency and high throughput in both write-heavy and read-heavy workloads.
//! 3. **Scalability**: Support for millions of orders and thousands of price levels without degradation.
//! 4. **Flexibility**: Easily extendable to support additional order types and matching algorithms.
//!
//! ## Use Cases
//!
//! - **Trading Systems**: Core component for building trading systems and exchanges
//! - **Market Simulation**: Tool for back-testing trading strategies with realistic market dynamics
//! - **Research**: Platform for studying market microstructure and order flow
//! - **Educational**: Reference implementation for understanding modern exchange architecture
//!
//! ## What's New in Version 0.4.8
//!
//! This version introduces significant performance optimizations and architectural improvements:
//!
//! - **Performance Boost**: Reintroduced `PriceLevelCache` for faster best bid/ask lookups and a `MatchingPool` to reduce memory allocations in the matching engine, leading to lower latency.
//! - **Cleaner Architecture**: Refactored modification and matching logic for better separation of concerns and maintainability.
//! - **Enhanced Concurrency**: Improved thread-safe operations, ensuring robustness under heavy load.
//! - **Improved Documentation**: All code comments have been translated to English, and crate-level documentation has been expanded for clarity.
//!
//! ## Status
//! This project is currently in active development and is not yet suitable for production use.
//!
//! ## Advanced Features
//!
//! ### Market Metrics & Analysis
//!
//! The order book provides comprehensive market analysis capabilities:
//!
//! - **VWAP Calculation**: Volume-Weighted Average Price for analyzing true market price
//! - **Spread Analysis**: Absolute and basis point spread calculations
//! - **Micro Price**: Fair price estimation incorporating depth
//! - **Order Book Imbalance**: Buy/sell pressure indicators
//! - **Market Impact Simulation**: Pre-trade analysis for estimating slippage and execution costs
//! - **Depth Analysis**: Cumulative depth and liquidity distribution
//!
//! ### Intelligent Order Placement
//!
//! Advanced utilities for market makers and algorithmic traders:
//!
//! - **Queue Analysis**: `queue_ahead_at_price()` - Check depth at specific price levels
//! - **Tick-Based Pricing**: `price_n_ticks_inside()` - Calculate prices N ticks from best bid/ask
//! - **Position Targeting**: `price_for_queue_position()` - Find prices for target queue positions
//! - **Depth-Based Strategy**: `price_at_depth_adjusted()` - Optimal prices based on cumulative depth
//!
//! ### Functional Iterators
//!
//! Memory-efficient, composable iterators for order book analysis:
//!
//! - **Cumulative Depth Iteration**: `levels_with_cumulative_depth()` - Lazy iteration with running depth totals
//! - **Depth-Limited Iteration**: `levels_until_depth()` - Auto-stop when target depth is reached
//! - **Range-Based Iteration**: `levels_in_range()` - Filter levels by price range
//! - **Predicate Search**: `find_level()` - Find first level matching custom conditions
//!
//! **Benefits:**
//! - Zero allocation - O(1) memory vs O(N) for vectors
//! - Lazy evaluation - compute only what's needed
//! - Composable - works with standard iterator combinators (`.map()`, `.filter()`, `.take()`)
//! - Short-circuit - stops early when conditions are met
//!
//! ### Multi-Book Management
//!
//! Centralized trade event routing and multi-book orchestration:
//!
//! - **BookManager**: Manage multiple order books with unified trade listener
//! - **Standard & Tokio Support**: Synchronous and async variants
//! - **Event Routing**: Centralized trade notifications across all books
//!
//! ### Aggregate Statistics
//!
//! Comprehensive statistical analysis for market condition detection:
//!
//! - **Depth Statistics**: `depth_statistics()` - Volume, average sizes, weighted prices, std dev
//! - **Market Pressure**: `buy_sell_pressure()` - Total volume on each side
//! - **Liquidity Health**: `is_thin_book()` - Detect insufficient liquidity
//! - **Distribution Analysis**: `depth_distribution()` - Histogram of liquidity concentration
//! - **Imbalance Detection**: `order_book_imbalance()` - Buy/sell pressure ratio (-1.0 to 1.0)
//!
//! **Use cases:**
//! - Market condition detection and trend identification
//! - Risk management and liquidity monitoring
//! - Strategy adaptation based on real-time conditions
//! - Trading decision support and analytics
//!
//! ### Enriched Snapshots
//!
//! Pre-calculated metrics in snapshots for high-frequency trading:
//!
//! - **Enriched Snapshots**: `enriched_snapshot()` - Single-pass snapshot with all metrics
//! - **Custom Metrics**: `enriched_snapshot_with_metrics()` - Select specific metrics for optimization
//! - **Metric Flags**: Bitflags for precise control over calculated metrics
//!
//! **Metrics included:**
//! - Mid price and spread (in basis points)
//! - Total depth on each side
//! - VWAP for top N levels
//! - Order book imbalance
//!
//! **Benefits:**
//! - Single pass through data vs multiple passes
//! - Better cache locality and performance
//! - Reduced computational overhead
//! - Flexibility with optional metric selection
//!
//! # Performance Analysis of the OrderBook System
//!
//! This analyzes the performance of the OrderBook system based on tests conducted on an Apple M4 Max processor. The data comes from a High-Frequency Trading (HFT) simulation and price level distribution performance tests.
//!
//! ## 1. High-Frequency Trading (HFT) Simulation
//!
//! ### Test Configuration
//! - **Symbol:** BTC/USD
//! - **Duration:** 5000 ms (5 seconds)
//! - **Threads:** 30 threads total
//!   - 10 maker threads (order creators)
//!   - 10 taker threads (order executors)
//!   - 10 canceller threads (order cancellers)
//! - **Initial orders:** 1020 pre-loaded orders
//!
//! ### Performance Results
//!
//! | Metric | Total Operations | Operations/Second |
//! |---------|---------------------|---------------------|
//! | Orders Added | 506,105 | 101,152.80 |
//! | Orders Matched | 314,245 | 62,806.66 |
//! | Orders Cancelled | 204,047 | 40,781.91 |
//! | **Total Operations** | **1,024,397** | **204,741.37** |
//!
//! ### Initial vs. Final OrderBook State
//!
//! | Metric | Initial State | Final State |
//! |---------|----------------|---------------|
//! | Best Bid | 9,900 | 9,900 |
//! | Best Ask | 10,000 | 10,070 |
//! | Spread | 100 | 170 |
//! | Mid Price | 9,950.00 | 9,985.00 |
//! | Total Orders | 1,020 | 34,850 |
//! | Bid Price Levels | 21 | 11 |
//! | Ask Price Levels | 21 | 10 |
//! | Total Bid Quantity | 7,750 | 274,504 |
//! | Total Ask Quantity | 7,750 | 360,477 |
//!
//! ## 2. Price Level Distribution Performance Tests
//!
//! ### Configuration
//! - **Test Duration:** 5000 ms (5 seconds)
//! - **Concurrent Operations:** Multi-threaded lock-free architecture
//!
//! ### Price Level Distribution Performance
//!
//! | Read % | Operations/Second |
//! |------------|---------------------|
//! | 0%         | 430,081.91          |
//! | 25%        | 17,031.12           |
//! | 50%        | 15,965.15           |
//! | 75%        | 20,590.32           |
//! | 95%        | 42,451.24           |
//!
//! ### Hot Spot Contention Test
//!
//! | % Operations on Hot Spot | Operations/Second   |
//! |--------------------------|---------------------|
//! | 0%                       | 2,742,810.37        |
//! | 25%                      | 3,414,940.27        |
//! | 50%                      | 4,542,931.02        |
//! | 75%                      | 8,834,677.82        |
//! | 100%                     | 19,403,341.34       |
//!
//! ### Performance Improvements and Deadlock Resolution
//!
//! The significant performance gains, especially in the "Hot Spot Contention Test," and the resolution of the previous deadlocks are a direct result of refactoring the internal concurrency model of the `PriceLevel`.
//!
//! - **Previous Bottleneck:** The original implementation relied on a `crossbeam::queue::SegQueue` for storing orders. While the queue itself is lock-free, operations like finding or removing a specific order required draining the entire queue into a temporary list, performing the action, and then pushing all elements back. This process was inefficient and created a major point of contention, leading to deadlocks under heavy multi-threaded load.
//!
//! - **New Implementation:** The `OrderQueue` was re-designed to use a combination of:
//!   1. A `dashmap::DashMap` for storing orders, allowing for highly concurrent, O(1) average-case time complexity for insertions, lookups, and removals by `Id`.
//!   2. A `crossbeam::queue::SegQueue` that now only stores `Id`s to maintain the crucial First-In-First-Out (FIFO) order for matching.
//!
//! This hybrid approach eliminates the previous bottleneck, allowing threads to operate on the order collection with minimal contention, which is reflected in the massive throughput increase in the hot spot tests.
//!
//! ## 3. Analysis and Conclusions
//!
//! ### Overall Performance
//! The system demonstrates excellent capability to handle over **200,000 operations per second** in the high-frequency trading simulation, distributed across order creations, matches, and cancellations.
//!
//! ### Price Level Distribution Behavior
//! - **Optimal Performance Range:** The system performs best with 50-100 price levels, achieving 66,000-67,000 operations per second.
//! - **Performance Degradation:** Performance decreases significantly with fewer price levels, dropping to around 23,000-29,000 operations per second with 1-10 levels.
//! - **Scalability:** The lock-free architecture demonstrates excellent scalability characteristics across different price level distributions.
//!
//! ### Hot Spot Contention
//! - Surprisingly, performance **increases** as more operations concentrate on a hot spot, reaching its maximum with 100% concentration (19,403,341 ops/s).
//! - This counter-intuitive behavior might indicate:
//!   1. Very efficient cache effects when operations are concentrated in one memory area
//!   2. Internal optimizations to handle high-contention cases
//!   3. Benefits of the system's lock-free architecture
//!
//! ### OrderBook State Behavior
//! - During the HFT simulation, the order book handled a significant increase in order volume (from 1,020 to 34,850).
//! - The spread increased from 100 to 170, reflecting realistic market behavior under pressure.
//! - The final state shows substantial liquidity with over 274,000 bid quantity and 360,000 ask quantity.
//!
//! ## 4. Practical Implications
//!
//! - The system is suitable for high-frequency trading environments with the capacity to process over 200,000 operations per second.
//! - The lock-free architecture proves to be extremely effective at handling contention, especially at hot spots.
//! - Optimal performance is achieved with moderate price level distribution (50-100 levels).
//! - For real-world use cases, the system demonstrates excellent scalability and maintains performance under concurrent load.
//!
//! This analysis confirms that the system design is highly scalable and appropriate for demanding financial applications requiring high-speed processing with data consistency.

pub mod orderbook;

pub mod prelude;
mod utils;

#[cfg(feature = "bincode")]
pub use orderbook::BincodeEventSerializer;
#[cfg(feature = "journal")]
pub use orderbook::FileJournal;
#[cfg(feature = "nats")]
pub use orderbook::NatsTradePublisher;
pub use orderbook::implied_volatility::{
    BlackScholes, IVConfig, IVError, IVParams, IVQuality, IVResult, OptionType, PriceSource,
    SolverConfig,
};
pub use orderbook::iterators::LevelInfo;
pub use orderbook::manager::{BookManager, BookManagerStd, BookManagerTokio};
pub use orderbook::market_impact::{MarketImpact, OrderSimulation};
pub use orderbook::sequencer::{
    Journal, JournalEntry, JournalError, JournalReadIter, SequencerCommand, SequencerEvent,
    SequencerResult,
};
pub use orderbook::serialization::{EventSerializer, JsonEventSerializer, SerializationError};
pub use orderbook::snapshot::{EnrichedSnapshot, MetricFlags};
pub use orderbook::statistics::{DepthStats, DistributionBin};
pub use orderbook::stp::STPMode;
pub use orderbook::trade::{TradeListener, TradeResult};
#[cfg(feature = "nats")]
pub use orderbook::{BookChangeBatch, BookChangeEntry, NatsBookChangePublisher};
pub use orderbook::{FeeSchedule, MassCancelResult, OrderBook, OrderBookError, OrderBookSnapshot};
pub use utils::current_time_millis;

/// Legacy type alias for `OrderBook<()>` to maintain backward compatibility.
///
/// This type provides the same functionality as the original `OrderBook` before
/// the migration to generic types. Use this when you don't need custom extra fields.
pub type LegacyOrderBook = OrderBook<()>;

/// Default type alias for `OrderBook<()>` representing the most common use case.
///
/// This is the recommended type to use when you don't need to store additional
/// data with your orders. It provides all the standard order book functionality
/// with unit type `()` as the extra fields parameter.
pub type DefaultOrderBook = OrderBook<()>;

// Re-export tipos de pricelevel con alias
pub use pricelevel::{Id, OrderType, Side, TimeInForce};

/// Legacy type alias for backward compatibility with code using `OrderId`.
pub type OrderId = Id;

/// Legacy type alias for `OrderType<()>` to maintain backward compatibility.
///
/// This type provides the same functionality as the original `OrderType` before
/// the migration to generic types. Use this when you don't need custom extra fields.
pub type LegacyOrderType = OrderType<()>;

/// Default type alias for `OrderType<()>` representing the most common use case.
///
/// This is the recommended type to use when you don't need to store additional
/// data with your orders. It provides all the standard order type functionality
/// with unit type `()` as the extra fields parameter.
pub type DefaultOrderType = OrderType<()>;
