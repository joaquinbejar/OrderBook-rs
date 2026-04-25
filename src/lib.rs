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
//! ## What's New in Version 0.7.0
//!
//! ### v0.7.0 — Feature-gated binary wire protocol
//!
//! - **New `wire` feature flag** behind which a small,
//!   length-prefixed binary protocol lives — every frame is
//!   `[len:u32 LE | kind:u8 | payload]`, `len` covers
//!   `kind + payload`, and all multi-byte integers are
//!   little-endian. Disabled by default; the existing JSON and
//!   bincode paths are unchanged. The protocol is additive.
//! - **`MessageKind`** — `#[repr(u8)]` enum with stable explicit
//!   discriminants. Inbound: `NewOrder = 0x01`,
//!   `CancelOrder = 0x02`, `CancelReplace = 0x03`,
//!   `MassCancel = 0x04`. Outbound: `ExecReport = 0x81`,
//!   `TradePrint = 0x82`, `BookUpdate = 0x83`.
//! - **Zero-copy inbound** — `NewOrderWire`, `CancelOrderWire`,
//!   `CancelReplaceWire`, `MassCancelWire` are
//!   `#[repr(C, packed)]` with `zerocopy::{FromBytes, IntoBytes,
//!   Unaligned, Immutable, KnownLayout}` derives. Each ships a
//!   `const _: () = assert!(size_of::<…>() == N)` guard. Decoding
//!   is safe — `#![deny(unsafe_code)]` stays on.
//! - **Byte-cursor outbound** — `ExecReport`, `TradePrintWire`,
//!   `BookUpdateWire` are encoded via explicit
//!   `extend_from_slice` calls. Outbound is I/O-dominated; this
//!   keeps the layout free to evolve.
//! - **`TryFrom<&NewOrderWire> for OrderType<()>`** — boundary
//!   mapping that copies each packed field into a stack local
//!   first (taking a reference to a packed field is UB), validates
//!   the side / TIF / order_type discriminants, and rejects
//!   negative prices via `WireError::InvalidPayload`.
//! - **`doc/wire-protocol.md`** with per-message layout tables,
//!   discriminant table, framing rule, and endianness statement.
//! - **Round-trip `proptest` coverage** in every
//!   `src/wire/{inbound,outbound}/*.rs` module.
//! - Example: `examples/src/bin/wire_roundtrip.rs`
//!   (`required-features = ["wire"]`).
//!
//! ### v0.7.0 — HDR-histogram tail-latency bench suite
//!
//! - **Six new `*_hdr` bench binaries** under
//!   `benches/order_book/`: `add_only`, `cancel_only`,
//!   `aggressive_walk`, `mixed_70_20_10`, `thin_book_sweep`,
//!   `mass_cancel_burst`. Each records per-sample nanosecond
//!   latencies into an `hdrhistogram::Histogram` and emits
//!   `p50` / `p99` / `p99.9` / `p99.99` + `min` / `max`. Coexists
//!   with the existing Criterion benches.
//! - **`make bench-hdr`** convenience target.
//! - **Headline numbers + methodology** in `BENCH.md` at the repo
//!   root, with a closed-loop disclosure block (the suite measures
//!   service time, not load-induced tail).
//!
//! ### v0.7.0 — Closed `RejectReason` enum
//!
//! - **New [`RejectReason`]** — closed
//!   `#[non_exhaustive] #[repr(u16)]` enum with stable explicit
//!   discriminants (1..13 + `Other(u16)`). The canonical wire-side
//!   reject taxonomy; consumers can route on the numeric code without
//!   parsing strings.
//! - **`OrderStatus::Rejected.reason: String`** is now
//!   `RejectReason` — typed, machine-routable, and stable across
//!   `0.7.x`. Breaking change on a public variant shape; allowed under
//!   the `0.6.x → 0.7.x` minor delta in `0.x` semver.
//! - **`impl From<&OrderBookError> for RejectReason`** — operational
//!   ergonomics. Exhaustive match — a future `OrderBookError` variant
//!   addition is caught at compile time.
//! - **Tracker emission on every reject path that already transitioned
//!   the tracker.** Kill switch, risk gates, and the three internal
//!   sites in `modifications.rs` (validation / post-only / missing
//!   user id) now record typed reasons. STP cancel-taker and IOC/FOK
//!   `InsufficientLiquidity` paths still return typed errors without
//!   transitioning the tracker — deferred to a follow-up.
//!
//! ### v0.7.0 — Pre-trade risk layer
//!
//! - **New [`RiskConfig`]** with three opt-in guard-rails:
//!   `max_open_orders_per_account`, `max_notional_per_account`, and
//!   `price_band_bps` against a configurable
//!   [`ReferencePriceSource`] (`LastTrade` / `Mid` / `FixedPrice`).
//!   Builder pattern — `RiskConfig::new().with_*(...)` chained.
//! - **Three new typed reject variants** on the existing
//!   `#[non_exhaustive]` `OrderBookError`:
//!   `RiskMaxOpenOrders`, `RiskMaxNotional`, `RiskPriceBand`. Each
//!   carries enough context (account, current, limit, deviation) for
//!   downstream consumers to act without parsing a string.
//! - **`OrderBook::set_risk_config(...)` / `risk_config()` /
//!   `disable_risk()`** — operator-driven gating. Check ordering on
//!   submit/add: `kill_switch → risk → STP → fees → match`. Market
//!   orders bypass the risk layer (no submitted price, no rest);
//!   kill switch still gates them.
//! - **Allocation-free** on the happy path. Per-account counters are
//!   `(AtomicU64, AtomicCell<u128>)` pairs; per-order risk state is a
//!   `DashMap<Id, RiskEntry>`.
//! - **`OrderBookSnapshotPackage.risk_config: Option<RiskConfig>`** —
//!   config persists across snapshot/restore. On restore, per-account
//!   counters and the per-order map are rebuilt by walking the
//!   snapshot's resting orders. Snapshot format version stays at `2`;
//!   the field is additive via `#[serde(default)]`.
//! - Example: `examples/src/bin/risk_limits.rs`.
//!
//! ### v0.7.0 — Operational kill switch
//!
//! - **New `OrderBook::engage_kill_switch()`,
//!   `OrderBook::release_kill_switch()`, and
//!   `OrderBook::is_kill_switch_engaged()`** — atomic operational halt
//!   for new flow. While engaged, every `submit_market_order*`,
//!   `add_order`, and non-`Cancel` `update_order` call returns the new
//!   [`OrderBookError::KillSwitchActive`] variant before any matching,
//!   fee, or STP work happens. Cancel and mass-cancel paths are
//!   explicitly **not** gated so operators can drain the resting book.
//!   The flag persists across snapshot/restore.
//! - **`OrderBookError::KillSwitchActive`** — new typed reject variant
//!   on the existing `#[non_exhaustive]` enum.
//! - **`OrderBookSnapshotPackage.kill_switch_engaged: bool`** —
//!   operational state persists across snapshot/restore. Snapshot
//!   format version stays at `2`; the field is additive via
//!   `#[serde(default)]`.
//! - When an `OrderStateTracker` is configured, kill-switched
//!   rejections are recorded as
//!   `OrderStatus::Rejected { reason: RejectReason::KillSwitchActive }`.
//! - Example: `examples/src/bin/kill_switch_drain.rs`.
//!
//! ### v0.7.0 — Global `engine_seq` across outbound streams
//!
//! - **New `OrderBook::next_engine_seq()` and `OrderBook::engine_seq()`**
//!   accessors backed by an `AtomicU64` counter. Every outbound emission
//!   (trade event, price-level change event) mints exactly one seq, in
//!   emission order, so external consumers can perform cross-stream gap
//!   detection and merge events from `TradeListener` and
//!   `PriceLevelChangedListener` into a single ordered view.
//! - **`engine_seq: u64` field** added to every outbound event type:
//!   `TradeResult`, `TradeEvent`, `PriceLevelChangedEvent`, and the NATS
//!   `BookChangeEntry`. JSON payloads are forward-compatible
//!   (`#[serde(default)]` falls back to `0` for v0.6.x payloads).
//! - **Snapshot format version bumped to `2`**.
//!   `OrderBookSnapshotPackage` carries `engine_seq` so that
//!   `restore_from_snapshot_package` resumes monotonicity exactly from the
//!   snapshotted point. `version: 1` packages are rejected by `validate()`.
//! - **`BookChangeBatch.sequence`** retains its existing per-batch
//!   publisher-counter semantics; cross-stream gap detection moves to the
//!   per-event `BookChangeEntry.engine_seq`. Both fields ship in the same
//!   payload for incremental adoption.
//!
//! ### v0.7.0 — `Clock` trait for deterministic replay
//!
//! - **New [`Clock`] trait** with two implementations, [`MonotonicClock`]
//!   (production, wraps `SystemTime::now`) and [`StubClock`] (replay /
//!   tests, monotonic `AtomicU64` counter with configurable start and
//!   step). Re-exported at the crate root and via [`prelude`].
//! - **[`OrderBook::with_clock`]** constructor plus `set_clock` and
//!   `clock()` accessors. The default [`OrderBook::new`] keeps wrapping
//!   [`MonotonicClock`] internally — existing callers observe no
//!   behavioural change.
//! - **`ReplayEngine::replay_from_with_clock`** for byte-identical
//!   replay tests and disaster-recovery pipelines that must reproduce
//!   engine timestamps deterministically.
//! - Wall-clock reads are no longer present inside the matching core —
//!   every stamp flows through `self.clock().now_millis()`.
//! - **Behavioural change (same type signature)**:
//!   `OrderStateTracker::get_history` and `OrderBook::get_order_history`
//!   now return `Vec<(u64 /* milliseconds */, OrderStatus)>` instead of
//!   nanoseconds; the `Clock::now_millis` unit is the only one the trait
//!   exposes.
//!
//! ### v0.6.2 — Dependency Bumps & Bincode 2.x Migration
//!
//! - **Dependency refresh**: `uuid` 1.23, `tokio` 1.52, `sha2` 0.11,
//!   `async-nats` 0.47, `bincode` 2.0 (crates.io `bincode 3.0.0` is a
//!   `compile_error!` stub, so `2.0` is the current usable major).
//! - **Bincode API migration** (feature `bincode`): the
//!   `BincodeEventSerializer` now uses `bincode::serde::encode_to_vec`
//!   / `decode_from_slice` with `bincode::config::standard()`. The
//!   public trait and type surface are unchanged.
//! - **Wire-format note**: bincode 1.x and 2.x produce different byte
//!   layouts on the NATS transport path. The on-disk journal uses
//!   `serde_json` and is unaffected (`ORDERBOOK_SNAPSHOT_FORMAT_VERSION`
//!   stays at `1`).
//!
//! ### v0.6.1 — NATS Integration, Sequencer & Order State
//!
//! - **NATS JetStream Publishers**: Trade event and book change publishers with retry, batching, and throttling (`nats` feature)
//! - **Zero-Copy Serialization**: Pluggable `EventSerializer` trait with JSON and Bincode implementations (`bincode` feature)
//! - **Sequencer Subsystem**: `SequencerCommand`, `SequencerEvent`, `SequencerResult` types for LMAX Disruptor-style total ordering
//! - **Append-Only Journal**: `FileJournal` with memory-mapped segments, CRC32 checksums, and segment rotation (`journal` feature)
//! - **In-Memory Journal**: `InMemoryJournal` for testing and benchmarking
//! - **Deterministic Replay**: `ReplayEngine` for disaster recovery and state verification from journal
//! - **Order State Machine**: `OrderStatus`, `CancelReason`, `OrderStateTracker` for explicit lifecycle tracking (Open → PartiallyFilled → Filled / Cancelled / Rejected)
//! - **Order Lifecycle Query API**: `get_order_history()`, `active_order_count()`, `terminal_order_count()`, `purge_terminal_states()`
//! - **Upgrade to pricelevel v0.7**: `Id`, `Price`, `Quantity`, `TimestampMs` newtypes for stronger type safety
//!
//! ### v0.5.x — Validation, STP, Fees & Mass Cancel
//!
//! - **Order Validation**: Tick size, lot size, and min/max order size validation with configurable limits
//! - **Self-Trade Prevention (STP)**: `CancelTaker`, `CancelMaker`, `CancelBoth` modes with per-order `user_id` enforcement
//! - **Fee Model**: Configurable `FeeSchedule` with maker/taker fees, fee fields in `TradeResult`
//! - **Mass Cancel Operations**: Cancel all, by side, by user, by price range — with `MassCancelResult` tracking
//! - **Cross-Book Mass Cancel**: `cancel_all_across_books()`, `cancel_by_user_across_books()`, `cancel_by_side_across_books()` on `BookManager`
//! - **Snapshot Config Preservation**: `restore_from_snapshot_package()` preserves fee schedule, STP mode, tick/lot size, and order size limits
//!
//! ### v0.4.8 — Performance & Architecture
//!
//! - **Performance Boost**: `PriceLevelCache` for faster best bid/ask lookups, `MatchingPool` to reduce matching engine allocations
//! - **Cleaner Architecture**: Refactored modification and matching logic for better separation of concerns
//! - **Enhanced Concurrency**: Improved thread-safe operations under heavy load
//!
//! ## Status
//! This project is in active development. The core matching engine, order validation, STP, fees, mass cancel, NATS integration, sequencer journal, and order state tracking are production-ready. The Sequencer runtime (async event loop) is under development.
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

/// Feature-gated binary wire protocol.
///
/// Length-prefixed `[len:u32 LE | kind:u8 | payload]` framing with
/// fixed-size, little-endian payloads. Inbound messages are
/// `#[repr(C, packed)]` and decode via `zerocopy`; outbound messages
/// use explicit byte-cursor encoders. See `doc/wire-protocol.md` for
/// the canonical layout tables.
///
/// Enabled with `--features wire`. The protocol is additive — JSON and
/// bincode paths are unchanged.
#[cfg(feature = "wire")]
pub mod wire;

#[cfg(feature = "bincode")]
pub use orderbook::BincodeEventSerializer;
#[cfg(feature = "journal")]
pub use orderbook::FileJournal;
#[cfg(feature = "nats")]
pub use orderbook::NatsTradePublisher;
pub use orderbook::clock::{Clock, MonotonicClock, StubClock};
pub use orderbook::implied_volatility::{
    BlackScholes, IVConfig, IVError, IVParams, IVQuality, IVResult, OptionType, PriceSource,
    SolverConfig,
};
pub use orderbook::iterators::LevelInfo;
pub use orderbook::manager::{BookManager, BookManagerStd, BookManagerTokio};
pub use orderbook::market_impact::{MarketImpact, OrderSimulation};
pub use orderbook::order_state::{
    CancelReason, OrderStateListener, OrderStateTracker, OrderStatus,
};
pub use orderbook::reject_reason::RejectReason;
pub use orderbook::risk::{ReferencePriceSource, RiskConfig, RiskState};
pub use orderbook::sequencer::{
    InMemoryJournal, Journal, JournalEntry, JournalError, JournalReadIter, ReplayEngine,
    ReplayError, SequencerCommand, SequencerEvent, SequencerResult, snapshots_match,
};
pub use orderbook::serialization::{EventSerializer, JsonEventSerializer, SerializationError};
pub use orderbook::snapshot::{EnrichedSnapshot, MetricFlags};
pub use orderbook::statistics::{DepthStats, DistributionBin};
pub use orderbook::stp::STPMode;
pub use orderbook::trade::{TradeListener, TradeResult};
#[cfg(feature = "nats")]
pub use orderbook::{BookChangeBatch, BookChangeEntry, NatsBookChangePublisher};
pub use orderbook::{
    FeeSchedule, ManagerError, MassCancelResult, OrderBook, OrderBookError, OrderBookSnapshot,
};
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

// Re-export pricelevel types with aliases
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
