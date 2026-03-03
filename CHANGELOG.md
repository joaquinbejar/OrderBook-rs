# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.6.0] — 2025-02-28

### Added

- **NATS JetStream Publishers** (`nats` feature): trade event and book change
  publishers with retry, batching, and throttling.
- **Zero-Copy Serialization** (`bincode` feature): pluggable `EventSerializer`
  trait with JSON and Bincode implementations.
- **Sequencer Subsystem**: `SequencerCommand`, `SequencerEvent`,
  `SequencerResult` types for LMAX Disruptor-style total ordering.
- **Append-Only Journal** (`journal` feature): `FileJournal` with
  memory-mapped segments, CRC32 checksums, and segment rotation.
- **In-Memory Journal**: `InMemoryJournal` for testing and benchmarking.
- **Deterministic Replay**: `ReplayEngine` for disaster recovery and state
  verification from journal.
- **Order State Machine**: `OrderStatus`, `CancelReason`,
  `OrderStateTracker` for explicit lifecycle tracking
  (Open → PartiallyFilled → Filled / Cancelled / Rejected).
- **Order Lifecycle Query API**: `get_order_history()`,
  `active_order_count()`, `terminal_order_count()`,
  `purge_terminal_states()`.
- **Cross-Book Mass Cancel**: `cancel_all_across_books()`,
  `cancel_by_user_across_books()`, `cancel_by_side_across_books()` on
  `BookManager`.
- **Snapshot Config Preservation**: `restore_from_snapshot_package()`
  preserves fee schedule, STP mode, tick/lot size, and order size limits.
- **Clone for OrderBookError**: manual `Clone` impl to work around
  `PriceLevelError` not deriving `Clone`.

### Changed

- Upgraded to **pricelevel v0.7** with `Id`, `Price`, `Quantity`,
  `TimestampMs` newtypes for stronger type safety.
- Removed all `.unwrap()` and `.expect()` from production code.

## [0.5.0] — 2025-01-15

### Added

- **Order Validation**: tick size, lot size, and min/max order size
  validation with configurable limits.
- **Self-Trade Prevention (STP)**: `CancelTaker`, `CancelMaker`,
  `CancelBoth` modes with per-order `user_id` enforcement.
- **Fee Model**: configurable `FeeSchedule` with maker/taker fees and fee
  fields in `TradeResult`.
- **Mass Cancel Operations**: cancel all, by side, by user, by price
  range — with `MassCancelResult` tracking.

## [0.4.8] — 2024-12-20

### Added

- **PriceLevelCache**: faster best bid/ask lookups.
- **MatchingPool**: reduced matching engine allocations.

### Changed

- Refactored modification and matching logic for better separation of
  concerns.
- Improved thread-safe operations under heavy concurrent load.

## [0.4.0] — 2024-11-01

### Added

- **Lock-Free Architecture**: `SkipMap` + `DashMap` + `SegQueue` for
  contention-free concurrent access.
- **Multiple Order Types**: Standard, Iceberg, PostOnly, FillOrKill,
  ImmediateOrCancel, GoodTillDate, TrailingStop, Pegged, MarketToLimit,
  Reserve.
- **Thread-Safe Price Levels**: independent concurrent modification per
  level.
- **Advanced Order Matching**: price-time priority for both market and
  limit orders with partial fills.
- **Multi-Book Management**: `BookManagerStd` and `BookManagerTokio` for
  managing multiple order books.
- **Enriched Snapshots**: single-pass snapshot with VWAP, spread, mid
  price, imbalance, and depth metrics.
- **Implied Volatility**: Black-Scholes implied vol calculation.
- **Market Metrics**: VWAP, micro price, queue analysis, depth
  statistics, and functional iterators.
