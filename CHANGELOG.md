# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.7.0] — unreleased

> 0.7.0 is unreleased and accumulates issues #51..#60. Sub-headings
> below group changes by feature; everything ships in the same
> 0.7.0 publish.

### Added — feature-gated binary wire protocol (#59)

- **New `wire` feature flag** in `Cargo.toml` plus an optional
  dependency on `zerocopy = "0.8"` (with `derive`). Disabled by
  default; the crate's existing JSON and bincode paths are
  unchanged — the wire protocol is purely additive.
- **Length-prefixed framing** — every frame on the wire is
  `[len:u32 LE | kind:u8 | payload]`. `len` covers `kind + payload`
  (it does NOT include the 4-byte `len` prefix itself). All
  multi-byte integers are little-endian. Implementation in
  `src/wire/framing.rs` with `encode_frame` / `decode_frame`.
- **`MessageKind` enum** (`#[repr(u8)]`, `#[non_exhaustive]`) with
  stable explicit discriminants documented as stable across
  `0.7.x`:

  | Code   | Direction | Message         | Payload size |
  |--------|-----------|-----------------|-------------:|
  | `0x01` | inbound   | `NewOrder`      | 48 B         |
  | `0x02` | inbound   | `CancelOrder`   | 24 B         |
  | `0x03` | inbound   | `CancelReplace` | 40 B         |
  | `0x04` | inbound   | `MassCancel`    | 24 B         |
  | `0x81` | outbound  | `ExecReport`    | 44 B         |
  | `0x82` | outbound  | `TradePrint`    | 48 B         |
  | `0x83` | outbound  | `BookUpdate`    | 32 B         |

- **Inbound messages** are `#[repr(C, packed)]` and derive the
  `zerocopy` traits (`FromBytes`, `IntoBytes`, `Unaligned`,
  `Immutable`, `KnownLayout`). Decoding is safe — the crate keeps
  `#![deny(unsafe_code)]` on the lib root. Each struct ships a
  compile-time `const _: () = assert!(size_of::<…>() == N)` size
  guard. Exposed: `NewOrderWire`, `CancelOrderWire`,
  `CancelReplaceWire`, `MassCancelWire` and the matching
  `decode_*` helpers.
- **Outbound messages** use explicit byte-cursor encoders
  (`Vec<u8>::extend_from_slice`) rather than packed structs.
  Outbound is I/O-dominated so the cost of a few dozen bytes of
  field-by-field copy is dwarfed by socket overhead, and the
  layout is free to evolve. Exposed: `ExecReport` +
  `encode_exec_report` + `status_to_wire`,
  `TradePrintWire` + `encode_trade_print`,
  `BookUpdateWire` + `encode_book_update`.
- **Wire ↔ domain mapping** at the boundary —
  `impl TryFrom<&NewOrderWire> for OrderType<()>` performs the
  conversion, copies each packed field into a local first
  (taking a reference to a packed field is undefined behaviour),
  and returns `WireError::InvalidPayload` on unknown
  side / TIF / order_type bytes or a negative price.
- **Errors** routed through a manual-`Display`
  `#[non_exhaustive] WireError` (no `thiserror`, matches the
  crate's existing manual style for the wire surface): variants
  `Truncated`, `UnknownKind(u8)`, `InvalidPayload(&'static str)`.
- **`doc/wire-protocol.md`** with per-message offset / size /
  field / type / notes tables, the `MessageKind` discriminant
  table, the framing rule, and the LE-endianness statement.
- **Round-trip `proptest` tests** in every
  `src/wire/{inbound,outbound}/*.rs` module — encode through the
  framer, decode back, assert byte-for-byte equality.
- **Crate-root re-exports** under `#[cfg(feature = "wire")]` —
  callers reach types via `orderbook_rs::wire::*`.
- **Example** `examples/src/bin/wire_roundtrip.rs` (gated by
  `required-features = ["wire"]`) — builds a `NewOrderWire`,
  encodes it through the framer, decodes it back, converts to a
  domain `OrderType<()>`, and prints every field via
  `tracing::info!`.

### Added — HDR-histogram tail-latency bench suite (#56)

- **Six new bench binaries** under `benches/order_book/*_hdr.rs` that
  record per-sample latency into an `hdrhistogram::Histogram` and
  emit `p50` / `p99` / `p99.9` / `p99.99` + min / max + sample count
  to stdout. Scenarios: `add_only`, `cancel_only`,
  `aggressive_walk`, `mixed_70_20_10`, `thin_book_sweep`,
  `mass_cancel_burst`. Each is a `harness = false` binary that
  coexists with the existing Criterion benches.
- **Shared helpers** in `benches/order_book/hdr_common.rs`
  (`new_histogram`, `record`, `report`, `persist`) and a
  self-contained xorshift PRNG so the bench tree pulls no extra
  runtime dependency beyond `hdrhistogram`.
- **`hdrhistogram` ^7** as a dev-dependency.
- **`make bench-hdr`** target — runs all six scenarios in series.
- **`BENCH.md`** at repo root with methodology (warmup, closed-loop
  vs open-loop disclosure), reproducibility steps, run conditions
  block, and an honest table of the headline numbers from a single
  M4 Max run plus a one-paragraph "where the tail comes from"
  paragraph per scenario. Format-version stays at `2`.
- Raw histograms persist to `target/bench-hdr/<scenario>.hgrm` (V2
  HDR format, gitignored under `target/`).

### Notes — HDR bench

- **Closed-loop service time only.** The driver waits for each call
  before issuing the next — tail latencies under saturation will be
  worse than what these numbers report. Used as a regression signal
  and a lower-bound on production tail, not as a published SLO.
  Open-loop measurement is a follow-up.
- The Criterion benches under `benches/order_book/` (`add_orders.rs`,
  `match_orders.rs`, etc.) are unchanged.

### Added — closed `RejectReason` enum (#55)

- **New `RejectReason`** closed `#[non_exhaustive] #[repr(u16)]` enum
  with stable explicit discriminants (1..13 + `Other(u16)`). It is the
  canonical wire-side reject taxonomy — consumers can route on the
  numeric code without parsing strings, and the discriminants are
  documented as stable across `0.7.x` patch upgrades.
- **`OrderStatus::Rejected.reason: String`** → `RejectReason`
  (breaking change to a public enum's variant shape; allowed under the
  `0.6.x → 0.7.x` minor delta in `0.x` semver).
- **Crate-root + prelude re-export** of `RejectReason`.
- **`impl From<&OrderBookError> for RejectReason`** — operational
  ergonomics. Maps every `OrderBookError` variant to its wire-side
  reject code (or `Other(0)` for internal-state errors with no public
  reject mapping). Exhaustive match — adding an `OrderBookError`
  variant in the future is caught at compile time inside the crate.
- **Risk-gate rejection now records the tracker.** When an
  `OrderStateTracker` is configured and `add_order` is rejected by the
  risk layer, the engine records
  `OrderStatus::Rejected { reason: RejectReason::Risk* }` against the
  rejected order id before propagating the typed error. Mirrors the
  kill-switch tracker pattern.
- **Kill-switch reject now uses the typed code.** The previous string
  `"kill switch active"` is replaced by
  `RejectReason::KillSwitchActive`.
- **Validation / post-only / missing-user-id rejects also typed.** The
  internal sites in `modifications.rs` that already transitioned the
  tracker to `OrderStatus::Rejected` now emit `RejectReason::InvalidPrice`,
  `RejectReason::PostOnlyWouldCross`, and `RejectReason::MissingUserId`
  respectively (incidental migration — these paths previously stored a
  free-form string).
- New integration tests `tests/unit/reject_reason_tests.rs` cover the
  kill-switch and three risk-gate tracker emissions and a Display
  smoke test.

### Notes — `RejectReason`

- Discriminants are stable wire codes. Do not reorder or reuse a
  retired discriminant within the `0.7.x` series.
- `Other(u16)` is the forward-compat escape hatch for application-side
  extensions. Values `>= 1000` are reserved for caller use; the
  library will never emit a value in that range.
- The reverse direction `From<RejectReason> for OrderBookError` is
  **not** provided. The enum is the stable public contract; the error
  is the internal impl detail.
- Snapshot format unchanged. `OrderStateTracker` history is not
  persisted in `OrderBookSnapshotPackage`; format version stays at `2`.
- Out of scope (deferred to a follow-up issue): wiring tracker
  `Rejected` emission for STP cancel-taker and `InsufficientLiquidity`
  IOC/FOK paths, both of which currently return errors without
  transitioning the tracker.

### Added — pre-trade risk layer (#54)

- **Pre-trade risk layer** on `OrderBook<T>`. New `RiskConfig` with
  three opt-in guard-rails and three new typed reject variants on
  `OrderBookError`:
  - `max_open_orders_per_account: Option<u64>` →
    `OrderBookError::RiskMaxOpenOrders { account, current, limit }`
  - `max_notional_per_account: Option<u128>` →
    `OrderBookError::RiskMaxNotional { account, current, attempted, limit }`
  - `price_band_bps: Option<u32>` (with
    `ReferencePriceSource::{LastTrade, Mid, FixedPrice(u128)}`) →
    `OrderBookError::RiskPriceBand { submitted, reference, deviation_bps, limit_bps }`
- **Public API on `OrderBook<T>`**:
  `pub fn set_risk_config(&mut self, RiskConfig)`,
  `pub fn risk_config(&self) -> Option<&RiskConfig>`,
  `pub fn disable_risk(&mut self)`. `RiskConfig` is a builder:
  `RiskConfig::new().with_max_open_orders_per_account(n).with_max_notional_per_account(n).with_price_band_bps(bps, source)`.
- **Per-account counters** in `DashMap<Hash32, RiskCounters>` with
  `open_count: AtomicU64` and `resting_notional: AtomicCell<u128>`.
  Per-resting-order risk state in `DashMap<Id, RiskEntry>`. All hooks
  are allocation-free on the happy path.
- **Check ordering** on submit/add: `kill_switch → risk → STP →
  fees → match`. Documented in the rustdoc on
  `RiskState::check_limit_admission`.
- **Market orders bypass the risk layer** (no submitted price, no
  rest, no contribution to the open-order count). Kill switch still
  gates them. Documented.
- **Reference-price resolution** for `price_band_bps`:
  - `LastTrade` → `last_trade_price`. Skipped (with one-time
    `tracing::warn!`) when no trades have occurred.
  - `Mid` → integer `(best_bid + best_ask) / 2`. One-sided book
    falls back to `LastTrade`.
  - `FixedPrice(p)` → caller-supplied `u128` ticks.
- **Snapshot persistence**. `OrderBookSnapshotPackage` carries
  `risk_config: Option<RiskConfig>` (with `#[serde(default)]` for
  forward-compat). On restore, counters and the per-order map are
  rebuilt by walking the snapshot's resting orders. Snapshot format
  version stays at `2` — the field is purely additive.
- **Crate-root re-exports**: `RiskConfig`, `RiskState`,
  `ReferencePriceSource`. Also surfaced via `prelude`.
- New example: `examples/src/bin/risk_limits.rs` — operator demo
  that breaches each gate in sequence.
- Integration tests `tests/unit/risk_layer_tests.rs` cover every
  reject path, every state-update hook, market-order bypass, and
  snapshot round-trip.

### Notes — pre-trade risk layer

- Counters are estimative. The `open_count` and `resting_notional`
  pair is two independent atomics; no atomic snapshot of the pair is
  taken. Under high concurrency the check may admit one order beyond
  the limit before settling — acceptable for a guard-rail (vs. a
  hard regulatory cap).
- Risk config is operator-driven, not journaled. Replays via
  `ReplayEngine::replay_from*` start with no risk gating; operators
  re-attach config post-replay.
- `disable_risk()` lifts the gates without dropping per-account
  counters, so subsequent `set_risk_config(...)` calls re-engage
  with the existing history intact.

### Added — kill switch (#53)

- **Operational kill switch** on `OrderBook<T>`. New `AtomicBool` on
  the book and three public methods:
  `pub fn engage_kill_switch(&self)`,
  `pub fn release_kill_switch(&self)`,
  `pub fn is_kill_switch_engaged(&self) -> bool`.
  While engaged, every public `submit_market_order*`, `add_order`,
  and non-`Cancel` `update_order` call returns the new
  `OrderBookError::KillSwitchActive` variant before any matching, fee,
  or STP work happens — at the cost of a single
  `AtomicBool::load(Relaxed)` on the gate. Cancel and mass-cancel
  paths are explicitly **not** gated so operators can drain the
  resting book in an orderly way. Idempotent.
- **`OrderBookError::KillSwitchActive`** — new typed reject variant.
  Additive on the existing `#[non_exhaustive]` enum.
- **Snapshot persistence**. `OrderBookSnapshotPackage` carries
  `kill_switch_engaged: bool` (with `#[serde(default)]` for JSON
  forward-compat). `restore_from_snapshot_package` resumes the
  operational state. Snapshot format version stays at `2` — the
  field is purely additive.
- **`OrderStateTracker` integration**. When a tracker is configured
  on the book and a kill-switched submit / modify is rejected, the
  engine records `OrderStatus::Rejected { reason: "kill switch active" }`
  via the existing `OrderStateTracker::transition`. A future typed
  `RejectReason` (issue #55) will replace the string code.
- New example: `examples/src/bin/kill_switch_drain.rs` — operator
  halt-and-drain demo. Run with
  `cargo run --bin kill_switch_drain --manifest-path examples/Cargo.toml`.
- Integration tests: `tests/unit/kill_switch_tests.rs` covers every
  gated and non-gated entry point plus snapshot round-trip and
  legacy v2 payload (without the new field) defaulting to `false`.

### Notes — kill switch

- The low-level `OrderBook::match_market_order` /
  `OrderBook::match_limit_order` entry points are **not** gated.
  Production flow goes through the `submit_*` / `add_order` /
  `update_order` public surface; this is documented in the rustdoc on
  `engage_kill_switch`.
- The kill switch is operator-driven, not journaled. A book restored
  via `ReplayEngine::replay_from*` starts with the kill switch
  disengaged regardless of the original journal author's state.
  Snapshot/restore preserves it; replay does not.

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

- Non-breaking public API surface for the Clock trait. Adding the
  `engine_seq` fields extends public structs that consumers may
  construct via struct literals; while `cargo-semver-checks`
  may flag those, the `0.6.x → 0.7.x` delta in `0.x` semver permits
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
