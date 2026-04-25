# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.7.0] — unreleased

> 0.7.0 is unreleased and accumulates issues #51..#60. Sub-headings
> below group changes by feature; everything ships in the same
> 0.7.0 publish.

### Added — global `engine_seq` (#52)

- **Global monotonic `engine_seq`** across every outbound stream.
  `OrderBook<T>` gains an internal `AtomicU64` counter and two public
  accessors: `pub fn next_engine_seq(&self) -> u64` (mints the next
  value via `fetch_add(1, Relaxed)`) and `pub fn engine_seq(&self) -> u64`
  (current value, used by snapshotting). The counter is incremented
  exactly once per outbound emission, in emission order. Per-instance
  contract — replay into a fresh book produces fresh seqs, not the
  original ones; consumers needing the original outbound stream use
  the journal's `SequencerEvent.sequence_num`.
- **`engine_seq: u64` field** on every outbound event type. JSON
  payloads are forward-compatible via `#[serde(default)]` where
  applicable:
  - `TradeResult.engine_seq`
  - `TradeEvent.engine_seq`
  - `PriceLevelChangedEvent.engine_seq`
  - `BookChangeEntry.engine_seq` (NATS path, `Serialize`-only)
- **Snapshot package persistence** — `OrderBookSnapshotPackage` carries
  `engine_seq: u64` so `restore_from_snapshot_package` resumes
  monotonicity exactly from the snapshotted point.
- Integration proptest `tests/unit/engine_seq_monotonic_tests.rs`
  (256 cases) asserts the cross-stream monotonicity contract.

### Changed — global `engine_seq` (#52)

- **`ORDERBOOK_SNAPSHOT_FORMAT_VERSION` bumped from `1` to `2`.**
  Snapshot packages with `version: 1` are now rejected by `validate()`
  with the existing `Unsupported snapshot version` error. JSON payloads
  at `version: 2` that omit `engine_seq` deserialize with `engine_seq = 0`.
- **`BookChangeBatch.sequence`** retains its existing per-batch
  publisher-counter semantics. Cross-stream gap detection now uses the
  new per-event `BookChangeEntry.engine_seq` instead. Both fields ship
  in the same payload; consumers can adopt the new field incrementally.

### Added — `Clock` trait (#51)

- **`Clock` trait** (`src/orderbook/clock.rs`) — pluggable timestamp source
  injected at the operations edge so matching stays deterministic under
  sequencer replay. Two implementations ship: `MonotonicClock` (production,
  wraps `SystemTime::now`) and `StubClock` (replay / tests, monotonic
  `AtomicU64` counter with configurable start and step). Exposed at the
  crate root and via `prelude`.
- **`OrderBook::with_clock(symbol, Arc<dyn Clock>)`** constructor and
  **`OrderBook::set_clock`**, **`OrderBook::clock()`** accessors. The
  default `OrderBook::new` keeps wrapping `MonotonicClock` internally —
  existing callers observe no behavioural change.
- **`OrderStateTracker::with_clock`** and
  **`OrderStateTracker::with_capacity_and_clock`** constructors.
- **`ReplayEngine::replay_from_with_clock`** and
  **`ReplayEngine::replay_from_with_clock_and_progress`** — the canonical
  entry points for byte-identical replay tests and disaster-recovery
  pipelines that must reproduce engine timestamps deterministically.
- Integration proptest `tests/unit/clock_determinism_tests.rs` (128 cases)
  covering "two replays with identical `StubClock` produce matching
  snapshots". A strictly byte-identical event-stream oracle (via
  `EventSerializer`) is widened in issue #57.
- New dev-dependency `proptest = "1.7"`.

### Changed

- **`OrderStateTracker` history unit migrated from nanoseconds to
  milliseconds.** The tracker now stamps via the injected `Clock`, and
  `Clock::now_millis` is the only unit the trait exposes.
  `OrderStateTracker::get_history` and `OrderBook::get_order_history`
  therefore return `Vec<(u64 /* ms */, OrderStatus)>` instead of
  nanoseconds. `purge_terminal_older_than(Duration)` interprets its
  argument in milliseconds accordingly.
- Wall-clock reads (`SystemTime::now` / `current_time_millis`) removed
  from `src/orderbook/operations.rs`, `private.rs`, `book.rs`, and
  `order_state.rs` — every stamp now flows through
  `self.clock().now_millis()`. `utils::current_time_millis` remains
  public for non-library callers and is unchanged.

### Notes

- Non-breaking public API surface for the Clock trait. The
  `engine_seq` field adds (#52) extend public structs that consumers
  may construct via struct literals; while `cargo-semver-checks` may
  flag those, the `0.6.x → 0.7.x` delta in `0.x` semver permits
  minor breaking changes.
- Replay determinism: `ReplayEngine::replay_from` continues to behave
  as before (production stamping via `MonotonicClock`). Byte-identical
  replay requires the new `replay_from_with_clock` entry point with a
  caller-supplied `Arc<StubClock>` and a fixed start value.
- Snapshot format version bumped to `2`. Older `version: 1` snapshots
  do not load. Re-snapshot under 0.7.0 to migrate.

## [0.6.2] — 2026-04-20

### Changed

- **Dependencies**: Bump workspace dependencies to latest stable
  versions — `uuid` → `1.23`, `tokio` → `1.52`, `sha2` → `0.11`,
  `async-nats` → `0.47`, and `bincode` → `2.0` (the crates.io `3.0.0`
  release is a `compile_error!` stub, so `2.0` is the current usable
  major).
- **`bincode` migration (feature `bincode`)**: migrated the
  `BincodeEventSerializer` and the bincode-gated sequencer tests from
  the legacy `bincode::serialize` / `bincode::deserialize` API to the
  serde bridge in `bincode 2.x`
  (`bincode::serde::encode_to_vec` / `bincode::serde::decode_from_slice`
  with `bincode::config::standard()`). The public
  `EventSerializer` trait and the `BincodeEventSerializer` type are
  unchanged.
- **`sha2` 0.11 compat**: the finalized `Digest` output type no
  longer implements `LowerHex` directly, so
  `OrderBookSnapshotPackage::compute_checksum` now formats the hash
  bytes explicitly.

### Notes

- **Wire-format change (bincode NATS payloads)**: bincode 1.x and
  bincode 2.x produce different byte layouts. Consumers that decoded
  NATS payloads with an older `BincodeEventSerializer` build must
  upgrade to the new version. The on-disk journal is unaffected — it
  uses `serde_json`, not bincode. `ORDERBOOK_SNAPSHOT_FORMAT_VERSION`
  stays at `1`.
- No public API changes — `0.6.2` is a compatible minor release.

## [0.6.1] — 2026-03-22

### Changed

- **Performance**: Replace `Box<dyn Iterator>` with `either::Either`
  for bid/ask iterators, eliminating unnecessary heap allocation and
  dynamic dispatch in the matching hot path.

### Fixed

- Updated dependency management workflows for GitHub Actions

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
