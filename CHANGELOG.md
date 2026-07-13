# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.11.0] — 2026-07-13

### Added

- **Replay reproduces the trade-ID stream: `trade_id_namespace` on
  `ReplayBookConfig` (#200).** v0.10.5 (#199) made the trade-ID namespace
  injectable on `OrderBook`, but every `ReplayEngine::replay_from*` entry
  point still constructed its book internally with a random
  `Uuid::new_v4()` namespace, so trade IDs produced through the shipped
  replay API remained non-reproducible against the live run and across
  repeated replays of the same journal. `ReplayBookConfig` now carries
  `trade_id_namespace: Option<Uuid>`, applied via
  `OrderBook::set_trade_id_namespace` before any journal events are
  replayed (the book is fresh, honoring the counter-restart contract), so
  `replay_from_with_clock_and_config` with the live namespace and an
  injected `Clock` reproduces the live trade-ID stream byte-identically.
  `ReplayBookConfig::new` keeps its six structural parameters — the new
  builder-style `with_trade_id_namespace(namespace)` sets the field. The
  non-config entry points intentionally keep the random namespace
  (documented) rather than growing more constructor variants.
- **Suffix replays with a namespace are rejected.** Applying a namespace
  restarts the trade-ID counter at 0, so a namespace-carrying config
  combined with `from_sequence != 0` would mint wrong IDs for the suffix
  and duplicates of IDs already emitted live under that namespace. The
  `*_with_config` entry points return the new typed
  `ReplayError::NamespaceRequiresFullReplay` instead; namespace-free
  suffix replay keeps working. Residual caveat (documented on the field):
  the journal must cover the trade-ID stream origin — the engine cannot
  detect a rotated segment whose earlier segments already produced trades
  under the same namespace.

### Changed

- **Breaking:** `ReplayBookConfig` gained a public field
  (`trade_id_namespace`), so exhaustive struct literals no longer compile —
  add `trade_id_namespace: None` or construct with `..Default::default()`.
  `ReplayError` gained the `NamespaceRequiresFullReplay` variant, so
  exhaustive matches need a new arm. Callers using
  `ReplayBookConfig::new(...)` / `::default()` are unaffected. Hence the
  0.11.0 (pre-1.0 breaking) version bump. No journal or snapshot format
  change, and no `ORDERBOOK_SNAPSHOT_FORMAT_VERSION` bump.

## [0.10.5] — 2026-07-13

### Added

- **Injectable trade-ID namespace (#199).** Every `OrderBook` constructor
  minted its trade/transaction-ID namespace internally with
  `Uuid::new_v4()`, so trade IDs differed between a live run and its replay
  even when the command stream, the injected `Clock`, and the matching were
  identical — the namespace was the only entropy left in the trade-ID
  stream (`pricelevel::UuidGenerator` derives UUID v5 from namespace +
  atomic counter). `OrderBook::set_trade_id_namespace(&mut self, namespace)`
  is a pre-publication setter symmetric with `set_clock`: it replaces the
  generator (the counter restarts at 0, so call it before any orders are
  submitted) and composes with every existing constructor.
  `OrderBook::with_clock_and_namespace(symbol, clock, namespace)` covers the
  common deterministic-venue setup in one call; a deterministically chosen
  namespace (e.g. UUID v5 of the symbol under a venue root) then yields
  byte-identical trade IDs across live/replay. Default constructors are
  unchanged (fresh random namespace per book). No wire-format or snapshot
  change, and no `ORDERBOOK_SNAPSHOT_FORMAT_VERSION` bump. The guarantee
  currently applies to books constructed by the caller; the sequencer's
  `ReplayEngine` entry points still build their books with a random
  namespace — wiring the seam into `ReplayBookConfig` is tracked in #200.

## [0.10.4] — 2026-07-12

### Added

- **Exact-fee API: `FeeSchedule::try_calculate_fee` + published
  guaranteed-exact input bound (#197).** `calculate_fee` deliberately
  saturates when `notional × |bps|` overflows `u128`, and consumers that
  must guarantee exact integer fees (journaled, replayable venues) could not
  distinguish a clamped fee from an exact one, nor validate inputs against a
  documented bound. `try_calculate_fee(notional, is_maker) ->
  Result<i128, FeeOverflow>` performs the identical computation (same
  truncation-toward-zero rounding, sign applied after the unsigned-domain
  magnitude) but returns the new `FeeOverflow` error — carrying the
  offending `notional`, the signed `bps`, and the
  `max_guaranteed_exact_notional` for that rate — instead of clamping, so an
  `Ok` is always mathematically exact and equal to `calculate_fee`'s output.
  The bound itself is published as
  `FeeSchedule::max_guaranteed_exact_notional_for_bps(bps)` (`const fn`; the
  multiplication-safety bound `u128::MAX / |bps|`, `u128::MAX` for a zero
  rate) and `FeeSchedule::max_guaranteed_exact_notional()` (minimum over the
  maker and taker legs), so venues can enforce it at admission time and make
  the saturating branch provably unreachable. The guarantee is sufficient,
  not tight: above the bound `try_calculate_fee` rejects conservatively even
  though the clamped `calculate_fee` value can coincide with the exact fee
  at isolated notionals (documented and pinned by test). `FeeOverflow` is
  re-exported at the crate root alongside `FeeSchedule`.
- `calculate_fee` behavior is unchanged (bit-identical, including the
  saturated clamp of magnitude `u128::MAX / 10_000`, signed per `bps`); it
  now delegates to `try_calculate_fee` and its docs state the exactness
  condition. No wire-format or snapshot change, and no
  `ORDERBOOK_SNAPSHOT_FORMAT_VERSION` bump.

## [0.10.3] — 2026-07-11

### Fixed

- **Special-order tracker now survives snapshot restore (#194).**
  `OrderBook::restore_from_snapshot` (the shared rebuild path behind
  `restore_from_snapshot_package` and the JSON entry points) rebuilt the resting
  bids / asks, `order_locations`, and `user_orders`, but left the
  `special_order_tracker` (`special_orders` feature) freshly-initialized. A
  restored resting pegged or trailing-stop order was therefore never
  re-registered with the tracker, so `reprice_pegged_orders` /
  `reprice_trailing_stops` never visited it and the order stayed stuck at its
  snapshotted price. The rebuild now re-registers every restored resting special
  order in the same fixed price-then-insertion-sequence walk that repopulates
  `order_locations` / `user_orders` (a single pass, no extra traversal), so
  re-pricing resumes after restore. The tracker holds only order ids; the
  trailing-stop watermark (`last_reference_price`) and the pegged / stop price
  are part of the order data and survive the snapshot round-trip, so no
  re-pricing state is lost or re-initialized. `restore_from_snapshot` clears the
  tracker before the rebuild so a restore is a full replacement. Non-special
  orders and books with no special orders are unaffected.
- No wire-format or public-API change: no new fields, no event-shape change, and
  no `ORDERBOOK_SNAPSHOT_FORMAT_VERSION` bump.

## [0.10.2] — 2026-07-11

### Fixed

- **Deterministic `user_orders` rebuild on snapshot restore (#192).**
  `OrderBook::restore_from_snapshot_package` (and `restore_from_snapshot`)
  rebuilt the `user_orders` index by walking each level's order-unstable
  `iter_orders()` view, whose `DashMap` hasher is seeded per instance. As a
  result the per-user `Vec<Id>` was rebuilt in a different order on each fresh
  book, and a `cancel_orders_by_user` issued after the restore returned a
  `MassCancelResult::cancelled_order_ids` sequence that diverged across restores
  of the same package. The rebuild now walks price levels in the same fixed
  price-then-insertion-sequence order the mass-cancel and eviction sweeps use
  (bids ascending price, then asks ascending price; within each level ascending
  insertion sequence via `PriceLevel::snapshot_by_seq_into`), so the restored
  `user_orders` index — and therefore any subsequent by-user cancel — is
  byte-identical across every restore of the same package. This order reflects
  the resting book at snapshot time, not the original admission history (a
  snapshot cannot recover that), but it is now fully deterministic. Pure journal
  replay was unaffected and is unchanged. `order_locations` is a map, so its
  rebuild order never leaked into emitted output.
- No wire-format or public-API change: no new fields, no event-shape change, and
  no `ORDERBOOK_SNAPSHOT_FORMAT_VERSION` bump.

## [0.10.1] — 2026-07-10

### Changed

- **Deterministic, replay-stable mass-cancel result ordering (#190).**
  `OrderBook::cancel_all_orders`, `cancel_orders_by_side`, and
  `cancel_orders_by_price_range` now enumerate the cancelled orders through the
  same fixed traversal the eviction sweep uses — bids first then asks; within a
  side, price levels in ascending price (the `SkipMap`'s natural key order); and
  within a level, ascending insertion sequence via
  `PriceLevel::snapshot_by_seq_into`. Previously these methods collected ids from
  order-unstable structures (`order_locations` / per-level `iter_orders`), whose
  `DashMap` hasher is seeded per instance, so two processes replaying the same
  command stream could produce different `MassCancelResult::cancelled_order_ids`
  orderings — and therefore divergent journaled `SequencerResult::MassCancelled`
  payloads. The cancelled **set** and **count** are unchanged; only the order of
  `cancelled_order_ids` is now deterministic across processes and replay.
  `cancel_orders_by_user` was already replay-stable (it drains the `user_orders`
  index in admission-history order) and is unchanged; its determinism contract is
  now documented.
- No wire-format or public-API change: no new fields, no event-shape change, and
  no `ORDERBOOK_SNAPSHOT_FORMAT_VERSION` bump.

## [0.10.0] — 2026-07-10

### Breaking

- **`SequencerCommand` and `SequencerResult` are now `#[non_exhaustive]`.**
  Downstream code that matches them exhaustively must add a wildcard arm
  (`_ => …`) and recompile. This is a one-time source break: future
  command/result variants become source-compatible additions instead of
  repeating it. Surfaced by adding `EvictExpiredOrders` below — on the
  previously-exhaustive enum that addition would itself have broken
  downstream matches silently inside the 0.9.x range (which is why this
  release is 0.10.0 and not 0.9.3). Wire format is unaffected: bincode
  variant indices and JSON encodings are unchanged, and existing journals
  replay as-is.

### Added

- **Host-driven GTD / DAY expiry sweep (#189)** — new
  `OrderBook::evict_expired_orders(now_ms)` removes every resting order whose
  time-in-force has expired as of the caller-supplied timestamp. `now_ms` is a
  `TimestampMs` (Unix milliseconds, the unit `clock().now_millis()` compares
  against) passed in by the caller — the sweep never reads the book's own clock,
  so a scheduler drives cadence and the sequencer can journal the exact cutoff.
  The matching hot path is untouched: there is no lazy per-match expiry check, so
  expiry is an explicit maintenance pass rather than an implicit per-submit cost.
  Expiry uses the same boundary predicate as admission (`now >= deadline` for
  `Gtd`, `now >= market_close` for `Day`), so an order admitted at an instant is
  never simultaneously evictable at that instant. Returns the evicted orders as
  `Vec<Arc<OrderType<T>>>`; a second sweep at the same `now_ms` is idempotent and
  returns empty.
- **Manager parity for the sweep (#189).** `BookManagerStd` and
  `BookManagerTokio` gain `evict_expired_orders(symbol, now_ms)` (per-symbol
  pass-through, `None` for an unknown symbol) and
  `evict_expired_across_books(now_ms)` (all books, mirroring the
  `cancel_*_across_books` idiom).
- **`SequencerCommand::EvictExpiredOrders { now_ms }` (#189).** New command
  variant (appended, so existing journals' bincode variant indices are
  unchanged). Replay applies the journaled cutoff — never the replay clock — so
  the sweep reproduces byte-identically and `snapshots_match` holds between a
  live book and its replay. Old journals replay unchanged; new journals carrying
  the variant fail on older binaries, consistent with the `MarketOrderByAmount`
  precedent. No `ORDERBOOK_SNAPSHOT_FORMAT_VERSION` bump required — the version
  gates the snapshot package, not the journal command enum.
- **`TimestampMs` re-exported** at the crate root and via `prelude`, so the new
  `now_ms` parameter can be constructed without importing from `pricelevel`
  directly.

### Documentation

- **Deterministic eviction order documented (#189).** The rustdoc on
  `evict_expired_orders` states the fixed, replay-stable order in which orders
  are evicted and side-effect events emitted: bids first then asks; within a
  side, ascending price (the `SkipMap`'s natural key order, no sort); within a
  level, ascending insertion sequence (the exact order the matching engine
  consumes resting orders, not the non-deterministic `iter_orders` view). Each
  order is removed through the same single-order cancel path as `cancel_order`,
  tagged `CancelReason::TimeInForceExpired`, so the price-level cache, depth
  statistics, `order_locations` / `user_orders` indices, risk state,
  special-order tracker, and order-state tracker all stay consistent.
- Runnable example `examples/src/bin/gtd_expiry_sweep.rs`
  (`cargo run -p examples --bin gtd_expiry_sweep`) seeds GTD orders on a
  `StubClock` book, sweeps at explicit timestamps, and logs the evicted orders
  via `tracing`.

## [0.9.2] — 2026-07-10

### Added

- **Constant-work per-price aggregate accessors (#186)** — an O(log N) point
  lookup + O(1) counter read, with no per-order materialization. Four read-only
  methods on `OrderBook<T>`: `visible_quantity_at_price`,
  `hidden_quantity_at_price`, `total_quantity_at_price`, and
  `order_count_at_price`. Each performs an O(log N) `SkipMap` point lookup then
  reads the level's maintained atomic counter (one relaxed load; two for
  `total_quantity_at_price`, which sums visible + hidden) — no per-order `Arc`
  is materialized and no `T: Default` conversion runs, so they are the cheap
  way to poll one level's depth or order count. `order_count_at_price` is the
  counterpart to `queue_ahead_at_price` that drops the per-order term: O(log N)
  here vs O(log N + K) for the queue-walking version, which is left unchanged.
  All four return `None` for an absent level and read advisory,
  eventually-consistent counters; use `create_snapshot` for a
  mutually-consistent view. `total_quantity_at_price` saturates a (practically
  unreachable) `visible + hidden` overflow to `u64::MAX`, never `0`, so an
  overflow signals "enormous", never "empty".
- **`add_limit_order_with_result` / `add_limit_order_with_user_and_result`
  (#185).** Result-returning counterparts of `add_limit_order` /
  `add_limit_order_with_user`: they build the same `Standard` order and route
  through `add_order_with_result`, returning
  `Ok((Arc<OrderType<T>>, Option<TradeResult>))` so callers get the match's
  `TradeResult` directly instead of relying on the `TradeListener` callback.

### Documentation

- **Per-call fill attribution guarantee for `add_order_with_result` (#185).**
  The rustdoc now states explicitly that concurrent submits on the same book
  each receive exactly their own fills — the `TradeResult` is built from that
  call's private `MatchResult`, never from shared capture state, because the
  engine holds no cross-call trade accumulator. On the error-after-fills paths
  (an unfillable IOC remainder, or a self-trade-prevention cancellation after
  earlier non-self fills) the caller instead gets the typed `Err` and the
  executed fills reach only the trade listener. A multi-thread concurrency test
  (`test_add_order_with_result_concurrent_per_call_attribution`) pins the
  guarantee.
- **GTD / market-close timestamps documented as milliseconds (#187).**
  `has_expired`, `set_market_close_timestamp`, and the `time_in_force`
  parameter docs on the limit / iceberg / post-only builders now state that GTD
  deadlines and the market-close timestamp are **milliseconds since the Unix
  epoch** — the same unit `clock().now_millis()` compares against. The pinning
  test `gtd_expiry_unit_is_milliseconds` proves a seconds-form deadline reads
  as instantly expired.

## [0.9.1] — 2026-07-08

### Added

- **`OrderBook::add_order_with_result` (#184).** Submit an order and receive
  the `TradeResult` produced by the match directly in the return value —
  `Ok((Arc<OrderType<T>>, Option<TradeResult>))` — instead of relying on the
  `TradeListener` callback. The trade result is `None` when the order produced
  no fills; when a listener is installed it still fires with the exact same
  `TradeResult` (same fills, fees, and `engine_seq`). `add_order` is now a thin
  wrapper that discards the result, and the `TradeResult` is only constructed
  when a listener is installed and/or the caller asked for it, so the plain
  `add_order` path without a listener stays free of the extra `MatchResult`
  clone. Contributed by @Dev380.

## [0.9.0] — 2026-06-24

### Performance

- **Bump `pricelevel` 0.8.2 → 0.8.3 — fixes per-match over-allocation
  (PriceLevel#106).** `PriceLevel::match_order` previously pre-sized each
  `MatchResult` to the *whole* level depth (`order_count`), so a small taker
  against a deep price level allocated (and immediately freed) a multi-MB
  transient buffer — a qty-1 market order against a 100 k-deep level allocated
  ~17.6 MB. 0.8.3 bounds the pre-allocation to
  `min(incoming_quantity, order_count)`. The `alloc_count` bench's
  `bytes_alloc/op` drops from `~790 KB` back to `~6 KB` (the per-match cost is
  now flat in level depth instead of linear); `allocs/op` is unchanged. No
  source change in orderbook-rs — dependency bump only.

### Fixed

- **Surface swallowed re-price failures + clamp telemetry (#174).** The special-order
  re-price loops used `if self.update_order(update).is_ok()` and discarded the
  `Err`, and `reprice_special_orders` hardcoded `failed_orders: Vec::new()`, so a
  re-price rejected by admission (e.g. a risk `RiskMaxNotional`) silently left the
  order at its stale price with nothing recorded. The loops now capture each
  rejected `update_order` and `RepricingResult::failed_orders` is populated with a
  `(order_id, reason)` pair for every failure (a rejected re-price keeps the
  order's prior price — validate-first modify, #98/#168). The public
  `RepricingOperations` trait signatures are unchanged: `reprice_pegged_orders` /
  `reprice_trailing_stops` still return the repriced count; the failure detail is
  reported through `reprice_special_orders`. Separately, `calculate_pegged_price`
  now emits a `trace!` when a peg is price-slid off its requested
  `reference ± offset` to the passive side (or skipped because no valid passive
  tick exists), so a consumer can distinguish a peg that tracked its reference
  from one that was clamped.
- **Extend modify atomicity to the STP self-cross taker-cancellation edge (#168).**
  #98 made `UpdatePrice` / `UpdatePriceAndQuantity` / `Replace` validate-first for
  every *pre-match* admission rejection, but one *post-match* case slipped
  through: under `STPMode::CancelTaker` / `CancelBoth`, re-pricing an order so it
  crosses into the **same user's** resting liquidity on the opposite side made
  `add_order` match post-cancel and cancel the taker (the re-added order) — *after*
  the original was already removed, destroying it. The modify guard now runs a
  `check_modify_stp_self_cross` pre-check (after the risk check, before
  `cancel_order`): it dry-runs the crossable opposite side in the sweep's
  price-time order, consuming each non-self level's authoritative
  `matchable_quantity`, and if it reaches a same-user maker while the taker still
  has unfilled quantity — the exact condition under which the engine sets
  `stp_taker_cancelled` — returns `OrderBookError::SelfTradePrevented` so the
  original survives unchanged. No-op for STP `None` / `CancelMaker` (the taker
  rests, never destroyed) and anonymous takers; lot-aligned by construction
  (`validate_order_shape` runs first), so the verdict matches the engine exactly.

### Changed

- **Delegate FOK matchable-depth to `PriceLevel::matchable_quantity` (#136).**
  `fok_fillable_quantity` (#96) computed per-level reachable depth with a
  hand-rolled `order_matchable_qty` sum (`visible + drawable_hidden`), a
  re-implementation of pricelevel's authoritative dry-run that could silently
  drift from `OrderType::match_against` if upstream replenishment/order-kind
  semantics changed. The non-STP and STP-`NoConflict` paths now delegate to
  `PriceLevel::matchable_quantity` (made `pub` in pricelevel 0.8.2) — the single
  upstream source of truth for what `match_order` would consume — so the FOK
  all-or-nothing verdict can no longer diverge from the real sweep. The
  hand-rolled helper remains only for the STP `CancelMaker` case, which must sum
  the *non-self* makers' depth (a per-user filter the upstream primitive cannot
  express). Behavior is unchanged; the #96 reserve/iceberg FOK regression tests
  still pass, plus a new iceberg-replenishable-hidden FOK fill test.

### Performance — Pool the per-level STP scan buffer (#107)

- **Zero per-level heap allocation on the STP match path.** Under an active
  `STPMode`, each crossed price level previously allocated a fresh
  `Vec<Arc<OrderType<()>>>` for the self-trade scan
  (`PriceLevel::snapshot_by_insertion_seq`). The matching engine now reuses a
  single pooled scratch buffer (`MatchingPool::get_order_snapshot_vec` /
  `return_order_snapshot_vec`), refilled in place via the new
  `PriceLevel::snapshot_by_seq_into` (pricelevel 0.8.2), so the snapshot is
  reused across every conflicting level instead of allocated per level.
- **Dropped the per-level maker-id `Vec`.** `STPAction::CancelMaker` is now a
  unit variant — `check_stp_at_level` no longer `collect()`s same-user maker
  IDs into a `Vec<Id>`. The matching engine re-scans the pooled snapshot in
  insertion-sequence order and cancels each same-user maker inline. The cancel
  order (and therefore emitted events / journal) is bit-identical to before, so
  determinism and snapshot round-trip are unchanged.
- **Bumped `pricelevel` 0.8.1 → 0.8.2** for the determinism-preserving
  `snapshot_by_seq_into` drop-in.
- New `stp_sweep_hdr` Criterion HDR bench covers the aggressive self-crossing
  sweep under `STPMode::CancelMaker`.

### Changed — Upgrade to `pricelevel` 0.8.0 (#130)

- **Bumped `pricelevel` 0.7 → 0.8.0.** Picks up the upstream price-time-priority
  fix where a partially-filled resting maker keeps its place at the front of the
  level queue (PriceLevel#39), resolving #88 — a partial fill no longer demotes
  the maker behind later same-price arrivals. A regression test on the matching
  path (`test_partial_fill_preserves_price_time_priority_issue_88`) locks it in.
- **Deterministic match timestamps.** `PriceLevel::match_order` no longer reads
  the wall clock; the matching engine passes the book's `Clock` time as the taker
  timestamp, so trade timestamps follow the installed clock (replay-safe).
- **Domain newtypes on the public surface (breaking).** Through the `pricelevel`
  re-exports and `MatchResult` / `OrderType` accessors, several values now carry
  `Quantity` / `Price` / `TimestampMs` instead of raw `u64` / `u128`
  (e.g. `MatchResult::remaining_quantity()` → `Quantity`). OrderBook-rs's own
  method signatures (snapshot / statistics queries) are unchanged and still
  return raw integers; downstream code reading `pricelevel` types through the
  re-exports may need `.as_u64()` / `.as_u128()`. Minor bump under `0.x` semver.
- **`ReserveOrder.replenish_amount` is now `Option<NonZeroU64>`** (pricelevel 0.8).

### Changed — Dependency refresh

- `async-nats` 0.47 → 0.49, `dashmap` 6.1 → 6.2, `bitflags` 2.11 → 2.13,
  `either` 1.15 → 1.16, `crc32fast` 1 → 1.5, `proptest` 1.7 → 1.11.

### Removed

- Dropped the stale `ISSUE_IV.md` implied-volatility design draft from the repo
  root (the implied-volatility solver now lives in `src/orderbook/implied_volatility/`).

### Fixed

- **Reject zero quantity and zero price at the `NewOrder` wire boundary (#125).**
  `TryFrom<&NewOrderWire> for OrderType<()>` validated padding, negative price,
  side, time-in-force, and order type but never checked `qty`, so a wire `qty == 0`
  became a degenerate `OrderType::Standard { quantity: 0 }` that slipped past the
  default-config lot check (passes for 0) and `min_order_size` (defaults to `None`)
  and reached the insert/match path. The boundary now rejects `qty == 0` with
  `WireError::InvalidPayload("NewOrder: zero quantity")`. **Decision (price 0):**
  also reject `price == 0` (`"NewOrder: zero price"`) — price 0 is the cache's
  "no best price" sentinel and a zero-priced limit order is structurally
  meaningless, so only `price > 0` is admissible. `CancelReplaceWire` carries a
  `new_qty` field but has no domain conversion yet, so there is nothing to mirror;
  the same guard should be added when it gains one.
- **Resolve declared-but-unemitted wire/event surfaces (#119).** Three public
  surfaces claimed behavior the engine never implemented; all three are now backed
  by real engine paths. (a) `RejectReason::DuplicateOrderId` (stable wire code 12)
  was never emitted and `add_order` performed no duplicate-id check — it silently
  overwrote the resting order's location, orphaning it. `add_order` now rejects an
  incoming order whose id already rests on the book with the new
  `OrderBookError::DuplicateOrderId { order_id }` (mapped to wire code 12); the
  check lives in `add_order` (not the shared `validate_order_shape`) so the
  validate-first atomic modify is unaffected, and it does not clobber the live
  order's tracked state. (b) `TransactionInfo::maker_fee` / `taker_fee` were
  documented as per-transaction fees but no engine path populated them. The new
  `TradeInfo::from_trade_result(&TradeResult, Option<&FeeSchedule>)` computes each
  transaction's maker/taker fee from the schedule; the per-transaction fees sum to
  the aggregate `TradeResult::total_maker_fees` / `total_taker_fees`, so the
  detailed and aggregate views agree. (c) `MarketImpact::total_quantity_available`
  was documented as total available depth but was set to the requested-capped fill
  quantity, making `can_fill` trivially true and `fill_ratio` capped at `1.0`. It
  now accumulates the true resting depth across the whole side being hit (the
  impact metrics still describe only the consumed portion), so `fill_ratio` can
  exceed `1.0` when the book holds more depth than requested.
- **Evict zeroed per-account risk counters; self-balancing fill accounting (#115).**
  Two hardening fixes in the opt-in pre-trade risk layer (`risk.rs`). (a) `RiskState`
  kept a per-account `RiskCounters` entry forever — `on_fill`/`on_cancel` decremented
  the atomics but never removed the entry once `open_count` and `resting_notional`
  both reached zero, so the map grew monotonically with every distinct account ever
  seen (a slow leak on a long-running, high-cardinality venue that gradually raised
  `DashMap` cost on the risk path). A new `evict_if_zeroed` now drops the entry via
  `DashMap::remove_if` once both counters are zero, called from the `on_fill` full-fill
  branch and from `on_cancel`. `remove_if` re-checks the predicate under the shard
  write lock, so it cannot race a concurrent `on_admission` (which holds the same lock
  for its `entry().or_default()` increment): an admission in flight is observed as a
  non-zero `open_count` and the entry is kept. (b) `on_fill` decremented
  `resting_notional` using the passed `maker_price` (`trade.price()`); it now uses the
  maker's **stored admission price** (`RiskEntry::price`), so admission/fill/cancel are
  self-balancing by construction rather than relying on the cross-module
  `maker.price == trade.price` guarantee. `maker_price` is retained only as a debug
  assertion documenting that the two coincide today (a future price-improvement path
  that breaks the equality must revisit the accounting).
- **Deterministic, non-crossing pegged/trailing-stop repricing (#106).** Two
  fixes in the `special_orders`-gated repricing path. (a) `pegged_order_ids()` and
  `trailing_stop_ids()` collected from a `DashSet<Id>` in unspecified order, so the
  re-pricing sequence — and the events / journal entries it produced — was
  non-reproducible across runs, breaking replay determinism and price-time
  tie-breaking on re-insert. `Id` does not implement `Ord`, so both methods now
  sort by the deterministic `Display`/`to_string` key (`ids.sort_by_key(|id| id.to_string())`)
  before returning; the `to_string` allocation is acceptable off the matching hot
  path (operator-triggered maintenance). (b) `calculate_pegged_price` ignored the
  order `side` and could return a `reference ± offset` price that crossed the
  spread, so a pegged re-price would aggressively fill during a maintenance
  operation. It now takes the book `tick_size` and clamps the computed price to
  the passive side of the market — one tick inside the touch (`best_ask - tick`
  for a Buy, `best_bid + tick` for a Sell) — then snaps the result onto the tick
  grid in the passive direction (round down for a Buy, up for a Sell) so the
  re-priced order is always tick-aligned and restable. This is required because
  the re-price path swallows `add_order`'s tick-validation error: a `± 1`
  (off-tick) clamp on a `tick_size > 1` book was silently rejected on re-insert,
  leaving the peg stuck at its stale price. When no valid passive tick exists
  (degenerate cases such as `best_ask == tick`), the re-price is skipped (returns
  `None`) instead of crossing or resting off-book. The order now rests passively,
  tick-aligned, just inside the spread instead of trading.
- **`add_book` refuses to overwrite an existing book (#105, breaking).** Both
  `BookManagerStd` and `BookManagerTokio` did `self.books.insert(symbol, book)` and
  ignored the returned `Option`, so a second `add_book` for the same symbol
  silently replaced the first book — dropping all its resting orders and order
  locations with no warning. `BookManager::add_book` now returns
  `Result<(), ManagerError>` and returns the new `ManagerError::BookAlreadyExists { symbol }`
  instead of overwriting; both managers stay in parity. Breaking: callers must
  handle the `Result` (the trait method signature changed) — permitted under the
  0.8 → 0.9 window.
- **Wire `NewOrder` rejects `account_id == 0` (#103).** `TryFrom<&NewOrderWire>`
  encoded the numeric `account_id` into the low 8 bytes of a `Hash32` to build the
  `user_id`, with a comment claiming it avoided colliding with `Hash32::zero()`
  (the documented "no STP" sentinel) — but `account_id == 0` yields an all-zero
  array, i.e. exactly `Hash32::zero()`, so an order from numeric account 0 silently
  lost self-trade protection (it could match its own resting orders and was never
  grouped with other account-0 orders for STP). The conversion now rejects
  `account_id == 0` at the trust boundary with `WireError::InvalidPayload`, and the
  inline comment is corrected. `wire`-gated.
- **Replay now reconstructs non-default-config books deterministically (#101).**
  Every public `ReplayEngine` entry point built the target book with all
  configuration left at its defaults (`tick_size` / `lot_size` / `min_order_size`
  / `max_order_size` = `None`, `stp_mode` = `None`, `fee_schedule` = `None`), so
  replaying a journal produced by a book that used those — for example a
  `MarketOrderByAmount` rounding per level under a `lot_size` — rebuilt a
  **structurally different** book and could fail `snapshots_match` at verify.
  Two caller-supplied config variants now inject the original configuration into
  the fresh book *before* replay: `ReplayEngine::replay_from_with_config` and
  `ReplayEngine::replay_from_with_clock_and_config`, both taking a new
  `ReplayBookConfig` carrier (the same six fields persisted in
  `OrderBookSnapshotPackage`). Two `Option`-taking book setters back this up —
  `OrderBook::set_tick_size_opt` and `OrderBook::set_lot_size_opt` — alongside
  the existing `set_min_order_size` / `set_max_order_size` / `set_fee_schedule` /
  `set_stp_mode`. The configuration is supplied by the **caller** and is not read
  from the journal, so the on-disk format is unchanged and
  `ORDERBOOK_SNAPSHOT_FORMAT_VERSION` is **not** bumped. The plain `replay_from`
  / `replay_from_with_clock` entry points are now documented as valid only for
  default-config books. `ReplayBookConfig` is re-exported at the crate root and
  in the prelude.

- **Atomic modify: a rejected modify no longer destroys the original order (#98).**
  `UpdatePrice` / `UpdatePriceAndQuantity` / `Replace` previously cancelled the
  resting order *before* re-adding it, so a **pre-match admission rejection** in
  `add_order` (risk admission, missing `user_id` under STP, tick / lot size,
  min/max order size, expiry, post-only-would-cross, or FOK insufficient
  liquidity) destroyed the live order and returned only an error. These paths are
  now **validate-first**: a new pure, side-effect-free `validate_order_shape` runs
  every non-risk admission check (the same checks, in the same order, returning
  the same typed errors, but without mutating the book, recording state, emitting
  metrics, or invalidating the cache), and a new modify-aware risk check runs
  *before* the cancel. The original order is only cancelled once both pass; on
  any of these pre-match rejections it survives unchanged — no book mutation, no
  events, no trades. `add_order` now calls the same validator as its single
  source of truth, preserving its existing reject side-effects on the direct
  submit path. (Scope: the enumerated pre-match admission rejects. A re-price that
  would self-cross the same user's resting liquidity under `CancelTaker` /
  `CancelBoth` STP is a *post-match* cancellation not covered here — tracked as a
  follow-up; and a concurrent kill-switch flip between the guard and the cancel is
  a pre-existing, inherent two-step race.)
- **Modify-aware risk admission (#98).** Added `RiskState::check_modify_admission`
  for in-place modifies. A modify keeps the account's `open_count` unchanged
  (one order out, one in) and the original order is still counted in the
  account's counters at validation time, so the normal limit-admission check
  would double-count the original and falsely reject. The modify-aware check
  therefore (a) skips the open-order-count limit entirely, (b) runs the price
  band on the new price, and (c) checks notional against the *projected* resting
  notional `current − old_price·old_qty + new_price·new_qty` (saturating `u128`;
  the old order's contribution is already inside `current`), so a valid modify
  by an account sitting exactly at `max_open_orders_per_account` now succeeds.
- **Barrier-synchronized risk concurrency tests (#116).** The risk module's core
  safety claim — bounded over-admission and no wrap-to-MAX lockout under fill /
  cancel races — was uncovered by any concurrency test (all existing risk tests
  were single-threaded). Added two `std::thread::Barrier`-synchronized,
  sleep-free, deterministic tests: an N-thread admission race asserting
  `open_count` equals the admissions that incremented it and stays within
  `limit + thread_count` (bounded over-admission), and a full-fill-vs-cancel race
  over many orders asserting the saturating decrements land both `open_count` and
  `resting_notional` at `0` and never wrap to a large value. Test-only; no
  behavior change.
- **Manager trade-event channel semantics documented (#129).** `BookManagerStd`
  (std `mpsc`) and `BookManagerTokio` (tokio `unbounded_channel`) push trade events
  onto an **unbounded** channel by design — the matching path must never block to
  deliver an audit event, so the producer applies no backpressure (a bounded
  channel would force the synchronous matching path to block or drop events). This
  was previously unstated; the type-level docs and both `start_trade_processor`
  methods now document the unbounded design and the requirement to **start the
  processor before submitting orders** (otherwise events buffer without bound).
  Docs-only; no channel-type or hot-path change.
- **`with_channel_capacity` clamps instead of asserting on zero (#128).** The NATS
  publisher builder used `assert!(channel_capacity > 0, …)` on a caller-supplied
  argument, so a runtime-derived capacity of `0` aborted the whole process where
  graceful handling is expected (the error rules reserve `assert!` for truly
  unrecoverable invariant violations). A `0` capacity is now **clamped up to `1`**
  with a `tracing::warn!` (the minimum a Tokio mpsc accepts), via a shared
  `clamp_channel_capacity` helper, and the `# Panics` doc is replaced with the
  clamp note. Applied to both `NatsTradePublisher` and `NatsBookChangePublisher`.
  `nats`-gated.
- **NATS trade publisher metric counters share one per-trade granularity (#127).**
  `publish_count` incremented once per trade (only when both subjects succeeded)
  while `error_count` incremented once per **failed subject** (and once per trade
  on a serialize failure), so the two counters lived on different scales:
  `error_count` could exceed the number of trades, and a partial failure (one
  subject ok, the other exhausted) was invisible to both — making NATS health
  monitoring unreliable. Counting is now uniformly **per trade**: a trade
  increments `publish_count` on a clean success or `error_count` otherwise (a
  shared `account_publish_outcome` helper), so `publish_count + error_count`
  equals the number of trades processed and a partial failure is attributable to
  exactly one trade. The field docs are corrected to match. `nats`-gated;
  observability only, no functional effect.
- **`modify` uses a direct deep clone instead of a no-op `Arc::try_unwrap` (#124).**
  The `UpdatePrice` / `UpdatePriceAndQuantity` paths used
  `Arc::try_unwrap(order.clone()).unwrap_or_else(|arc| (*arc).clone())`, which
  cloned the `Arc` (strong count ≥ 2) and so the `try_unwrap` always failed and
  fell through to the deep clone — the cheap branch was dead and the intermediate
  `Arc` clone was pure overhead. Both now use `(*order).clone()`. Behavior
  unchanged.
- **Outbound event types surfaced at the crate root; `current_time_millis` gains a
  determinism caveat (#123).** `TradeEvent`, `TradeInfo`, `TransactionInfo`,
  `PriceLevelChangedEvent`, and `PriceLevelChangedListener` were re-exported only
  from the prelude, so `orderbook_rs::TradeEvent` failed to resolve even though the
  crate docs name `TradeEvent` / `PriceLevelChangedEvent` as first-class outbound
  types (`orderbook_rs::TradeResult` already resolved). They are now re-exported at
  the crate root too. Separately, `current_time_millis` documents that it reads the
  **non-monotonic wall clock**, truncates to `u64` ms, and must **not** be used on
  deterministic / matching / replay paths — those must take time from the `Clock`
  trait (`MonotonicClock` / `StubClock`) — and gained `#[must_use]`.
- **Doc gaps closed on IV / fee / wire items (#122).** The `Result`-returning IV
  functions (`solve_iv`, `solve_iv_bisection`, `implied_volatility`,
  `implied_volatility_with_config`) and the `TryFrom<&NewOrderWire> for OrderType<()>`
  conversion now carry `# Errors` sections enumerating their failure modes.
  `FeeSchedule::calculate_fee` documents its rounding rule (integer division
  truncates **toward zero**, symmetric in magnitude for a taker fee and a maker
  rebate) with a fractional-bps doctest asserting both signs. The `BookUpdateWire`
  layout rustdoc is corrected to match the code: **25 bytes of fields + a single
  7-byte `_pad`** at offset 25 (it previously claimed 26 fields + 6 pad with
  phantom `_pad0`/`_pad`). Docs-only, no functional change.
- **`#[must_use]` on pure iterator constructors and fallible decoders/IV entry points (#121).**
  Several public functions lacked the project-mandated `#[must_use]`: the
  `OrderBook` iterator constructors (`levels_with_cumulative_depth`,
  `levels_until_depth`, `levels_in_range` — dropping their result is a silent
  no-op, so these get a bare `#[must_use]`); the wire decoders (`decode_frame`,
  `decode_new_order`/`_cancel_order`/`_cancel_replace`/`_mass_cancel`/`_exec_report`/
  `_trade_print`/`_book_update`) and `MessageKind::from_u8`; and the IV entry
  points (`solve_iv`, `solve_iv_bisection`, `implied_volatility`,
  `implied_volatility_with_config`). The `Result`-returning functions use the
  `#[must_use = "…"]` message form (a bare `#[must_use]` on an
  already-`#[must_use]` `Result` trips `clippy::double_must_use`). No behavior
  change.
- **Analytics paths use ordered skiplist iteration with early exit (#120).**
  `enriched_snapshot_with_metrics` collected every bid/ask key into `Vec`s, sorted
  them, truncated to depth, then did a redundant second skiplist lookup per kept
  level — despite the `SkipMap` already being price-ordered; it now iterates
  `bids.iter().rev().take(depth)` / `asks.iter().take(depth)` and snapshots the
  entry directly (O(N log N)+2N-lookups → O(depth)). `LevelsInRange` scanned every
  remaining entry and never short-circuited at the band edge (its comment falsely
  claimed it did); it now threads the side and terminates as soon as iteration
  passes the far edge (Sell/ascending price > max; Buy/descending price < min),
  turning a narrow-band query on a wide book from O(N) into O(band). Answers are
  unchanged. `create_snapshot` has the same collect/sort idiom but is left
  untouched here — it is on the replay-critical path and out of this issue's scope.
- **`SerializationError` is a typed `thiserror` enum that bridges into `OrderBookError` (#118).**
  The `EventSerializer` error was a hand-rolled `struct { message: String }` with
  manual `Display`/`Error` impls and no `#[from]` bridge, flattening the structured
  serde/bincode failure to a string and deviating from the documented typed-error
  convention. It is now a `thiserror` enum — `Json(#[from] serde_json::Error)`
  (preserves the typed serde error), `Bincode(String)`, `TrailingBytes(String)` —
  and a `From<SerializationError> for OrderBookError` bridge folds it into
  `OrderBookError::SerializationError`, so an `EventSerializer` failure can be
  `?`-propagated on paths that return `OrderBookError`. Breaking for code that
  constructed/matched the old struct (`SerializationError { message }`); permitted
  under the 0.8 → 0.9 window.
- **`OrderBook` Serialize is deterministic and documented as lossy (#117).** The
  hand-written `Serialize` for `OrderBook<T>` collected bids/asks/order_locations
  into `HashMap`s (non-deterministic JSON key order across runs) and serialized
  the volatile best-bid/ask cache, while omitting the matching configuration
  (`stp_mode`, tick/lot/min/max order size, engine sequence, kill switch, risk
  config) that `create_snapshot_package` preserves — with no `Deserialize`, so it
  is a one-way inspection dump, not a persistence path. The collectors now use
  `BTreeMap` (deterministic key order; order_locations keyed by the id string),
  the cache is no longer serialized, and the impl carries rustdoc documenting it
  as a lossy debug/inspection view, steering callers to `snapshot_to_json` /
  `create_snapshot_package` for durable, reproducible persistence.
- **Best bid/ask cache serves both sides and represents price 0 (#93).**
  `PriceLevelCache` stored both sides behind a single shared `cache_valid` flag
  and overloaded price `0` as the absent sentinel, so `best_bid()` zeroed the ask
  slot (and vice versa) — two consecutive top-of-book reads never benefited from
  the cache, and a genuine best level at price `0` was permanently uncacheable.
  The cache now carries an independent `AtomicBool` validity flag per side (with
  `Acquire`/`Release` so a `valid` reader sees the stored price): `best_bid()`
  updates only the bid slot and `best_ask()` only the ask slot, so both-sides
  readers (`mid_price`, `spread`, `micro_price`, `resolve_reference_price(Mid)`)
  hit the cache in a single call and price `0` is a valid cached value.
- **Doc examples use `?` instead of `.unwrap()` (#92).** The remaining `///` doc
  examples that modelled `.unwrap()` on fallible order-book calls — all nine in
  `mass_cancel.rs` — now use `?` inside a hidden `Result`-returning harness,
  matching the idiomatic error handling the rules ask downstream users to follow
  (the other modules were already swept). Doc-comments only; `#[cfg(test)]` and
  `tests/` keep their `.unwrap()` per the testing allowance. `cargo test --doc`
  stays green.
- **Restored `#![deny(unsafe_code)]` and `#![warn(missing_docs)]` on `lib.rs` (#90).**
  Both crate-level attributes — mandated by `rules/global_rules.md` and `CLAUDE.md` —
  had drifted off `src/lib.rs`, silently allowing `unsafe` to creep in and `pub`
  items to ship undocumented (the `counting_allocator` module even documented a
  `deny` that no longer existed). The deny is restored; the only authorized `unsafe`
  — the four `memmap2` mmap blocks in `sequencer::file_journal` and the
  `CountingAllocator` `GlobalAlloc` impl — now carry an explicit
  `#[allow(unsafe_code)]` alongside their existing `// SAFETY:` rationale. The
  `missing_docs` warn surfaces zero warnings on `--all-features`.
- **Protocol counters use `checked_*` instead of `saturating_*` (#91).** Per the
  no-saturating-on-protocol-counters rule, the remaining protocol-state counters
  no longer silently cap on overflow. `file_journal`'s `archive_segments_before`
  tally and the `SegmentIterator` segment index now use `checked_add` and surface
  overflow as a new typed `JournalError::CounterOverflow`. The two NATS retry
  bounds (`max_attempts = max_retries + 1`) are computed in `u64` so the `+ 1`
  cannot overflow even at `max_retries == u32::MAX` — no saturating cap — while the
  backoff-delay `saturating_mul` clamps stay (they bound a duration, not a
  counter). The replay sequence counters (#126) and the dead `saturating_sub(0)`
  in `encode_entry` (#110) were already converted earlier in this cycle. Unreachable
  at any realistic journal size; the value is rule compliance and correct failure
  semantics at the boundary.
- **Boundary arithmetic in fee/analytics math is overflow-safe (#114).** Several
  monetary/price sites used unguarded casts or sums that wrap/panic on extreme
  inputs. `FeeSchedule::calculate_fee` cast `notional: u128` to `i128` with a bare
  `as` before the multiply, so a `notional > i128::MAX` truncated to a negative
  value and silently produced a wrong-sign/magnitude fee into a journaled
  `TradeResult`; it now computes the magnitude in the u128 domain (saturating) and
  applies the sign afterward, with an accurate doc comment. `resolve_reference_price(Mid)`
  and `DistributionBin::midpoint` use `u128::midpoint` instead of `(a + b) / 2`;
  `EnrichedSnapshot::calculate_imbalance` folds volumes with `saturating_add`; and
  `OrderSimulation::total_cost` folds with `saturating_mul`/`saturating_add`. Behavior
  is unchanged for all realistic inputs; each site gained a boundary-value test.
- **Price-band risk check cross-multiplies to stop sub-bps under-enforcement (#113).**
  `check_limit_admission` computed the deviation via truncating integer division
  (`diff * 10_000 / reference`) and rejected only when the floored bps exceeded the
  limit, so an order whose true deviation was fractionally above the band rounded
  down to the limit and slipped through (e.g. reference 30000, limit 100 bps, price
  30301 = 100.33 bps was admitted). The check now cross-multiplies — rejects when
  `diff * 10_000 > bps_limit * reference` — so the band never under-enforces, while
  an order exactly at the limit is still admitted (strict-`>` boundary preserved).
  The floored bps is recomputed only for the error-payload display.
- **IV solver guards NaN/Inf inputs and crossed/locked books (#112).** The
  Black-Scholes IV solver only checked sign/magnitude — all `false` for NaN — so
  a NaN/Inf `spot`/`strike`/`time`/`rate`/`market_price` passed validation and
  propagated NaN through the Newton/bisection loops to a meaningless
  `ConvergenceFailure { last_iv: NaN }`. `validate_params` now rejects non-finite
  `spot`/`strike`/`time_to_expiry`/`risk_free_rate`, both solver entry points
  reject a non-finite `market_price`, and the Newton loop bails with a typed
  error if a value goes non-finite mid-iteration. Separately, `extract_price_for_iv`
  computed the spread without checking `bid <= ask`, so a crossed (negative) or
  locked (zero) spread bypassed the max-spread gate and was classified
  high-quality; it now rejects such a book with a new `IVError::CrossedBook` before
  classification (observable via a transient torn read across the independent
  `best_bid`/`best_ask` calls).
- **`file_journal` no longer truncates a mapped segment or swallows poisoned
  mutexes (#111).** Two robustness defects in the journal subsystem the recovery
  path depends on. (1) `rotate_segment` called `set_len` to shrink a just-rotated
  segment that a concurrent reader may already have mmap'd at full capacity —
  touching pages past the new EOF is UB / SIGBUS on Unix and contradicted the
  `SegmentWriter` SAFETY invariant. The best-effort truncation is removed; the
  unused tail is a sparse hole (grown with `set_len`, never written), so there is
  no physical disk to reclaim and the "never truncated while mapped" invariant now
  holds. (2) `append` and `rotate_segment` updated `last_seq` / `segment_start_seq`
  behind `if let Ok(..)`, silently swallowing a poisoned lock — leaving
  `last_sequence()` under-reporting (breaking replay bounds) and `segment_start_seq`
  stale (so `archive_segments_before` could archive the active segment). Both now
  map a poisoned lock to `JournalError::MutexPoisoned` and propagate, so `append`
  never reports success while `last_seq` is unadvanced. `journal`-gated.
- **Journal reopen CRC-validates the tail and truncates a torn entry (#110).**
  `scan_write_position` determined the write position purely from
  `entry_length`, so a crash mid-flush that left an intact header but a torn
  payload/CRC was accepted: the journal resumed on top of the corrupt bytes and
  `last_sequence()` returned an undecodable sequence (later surfacing as a
  `CorruptEntry` at replay). The reopen scan now CRC-checks each entry (shared
  `entry_crc_valid` helper) and treats the first CRC failure as end-of-valid
  data — `write_pos` points at the torn entry's start so the next append
  overwrites it, and `last_sequence()` reports the last decodable sequence. A
  `tracing::warn!` fires on a detected torn tail. `journal`-gated.
- **NATS publishers expose a graceful `shutdown`/flush path (#109).** Both
  `NatsTradePublisher` and `NatsBookChangePublisher` spawned their background
  batch task and discarded the `JoinHandle` with no cancellation signal, so a
  pending batch could be silently lost on teardown and the detached task could
  outlive the publisher. Each now retains the join handle plus a one-shot
  shutdown signal and exposes an async `shutdown()` that signals the task to
  drain every event still buffered in the channel, flush it, and exit, then
  awaits the handle — no fire-and-forget task remains. A shared `drain_buffered`
  helper (unit-tested) performs the non-blocking, FIFO, chunked drain. `nats`-gated.
- **NATS trade publishing is batched/throttled off the matching hot path (#108).**
  `NatsTradePublisher::into_listener`'s callback used to serialize the payload,
  build two subjects with `format!`, convert to `Bytes`, and `runtime.spawn` a
  fresh Tokio task **per trade** on the matching thread — per-operation heap
  allocation and task-spawn pressure that floods the runtime under a burst. The
  callback now only clones the `TradeResult` into a bounded channel and returns;
  a single background task drains, batches (configurable window / size), and
  optionally throttles before serializing and publishing — mirroring
  `NatsBookChangePublisher`. The `{prefix}.all` subject is precomputed once at
  construction. New builders (`with_batch_window_ms`, `with_max_batch_size`,
  `with_channel_capacity`, `with_min_publish_interval_ms`) and metrics
  (`events_received`, `batches_published`, `dropped_events`) match the
  book-change publisher; the per-trade wire format, subjects, and pluggable
  serializer are unchanged. `nats`-gated.
- **Replay protocol sequence counter uses `checked_add` (#126).** `replay_into`
  advanced `expected_seq` (and the applied-event tally) with `saturating_add`,
  which violates the no-saturating-on-protocol-counters rule and would silently
  stop advancing at the `u64` ceiling — masking a real gap instead of surfacing
  it. Both now use `checked_add` and return the new `ReplayError::SequenceOverflow`
  on overflow. Unreachable at any realistic journal length; the value is rule
  compliance and correct failure semantics at the boundary.
- **`snapshots_match` compares full per-level structure (#102).** The replay
  equality oracle now compares each level's `hidden_quantity` and `order_count`
  in addition to `price` and `visible_quantity`. Previously two books that
  agreed on visible quantity but differed in reserve/iceberg hidden depth or in
  the number of resting orders at a level were reported as equal, so a replay
  that reconstructed the wrong hidden depth or order count could pass
  verification. The check is now a true structural equality, not a
  visible-quantity subset.
- **STP per-level scan is now deterministic (#94).** The self-trade-prevention
  pre-scan reads `PriceLevel::snapshot_orders()` (timestamp-ordered) instead of
  `iter_orders()` (DashMap, non-stable order), so `safe_quantity` and the
  CancelBoth `maker_order_id` follow price-time priority and are reproducible for a
  given book state. Non-determinism there previously broke replay (`snapshots_match`
  could diverge) for `CancelTaker` / `CancelBoth`.
- **STP maker cancels now fire the full cancel side-effects (#95).** Under
  `CancelMaker` / `CancelBoth`, each STP-cancelled resting maker is routed through
  `cancel_order_with_reason`, so it emits a `PriceLevelChangedEvent`, transitions to
  `OrderStatus::Cancelled { SelfTradePrevention }`, and releases its per-account risk
  counter. Previously these three effects were skipped, desynchronizing book-change
  consumers, leaving the maker in a non-terminal state, and leaking per-account
  open-order / notional counters.
- **Fully-consumed makers record their true filled quantity (#104).** The
  matching batch-removal path recorded `OrderStatus::Filled { filled_quantity: 0 }`
  (a placeholder) for every fully-consumed resting maker; it now records the real
  executed amount (the sum of the maker's trades in the submit), so
  `OrderStateTracker` / lifecycle consumers and any audit/risk reconciliation that
  sums filled quantity from terminal events are correct.
- **Fill-or-kill feasibility is self-trade-prevention aware (#96).** FOK admission
  checked feasibility with `peek_match`, which sums raw level depth: under
  `STPMode::CancelMaker` it counted same-user resting quantity (which the real
  match *cancels*, not fills), so a FOK could pass the check, cancel the maker,
  fill nothing, and still return `InsufficientLiquidity` — with the book already
  mutated. A new faithful `fok_fillable_quantity` mirrors the real walk —
  `lot_size`-rounded budget, per-level STP via `check_stp_at_level`, and per-order
  *drawable* depth (a non-auto-replenish reserve's hidden tranche is dropped
  unfilled by the sweep, so it is excluded) — so a FOK that cannot be fully filled
  is killed *before* any trade or cancel. (The `lot_size` divergence the report
  also posited is not reachable through the validated admission path — it rejects
  non-lot-multiple orders — but the rounding is kept so the check stays faithful to
  the matching walk.)
- **STP-cancelled takers no longer rest a self-cross residual (#97).** When a taker
  partially filled against another user and then would self-cross under
  `CancelTaker` / `CancelBoth`, the engine returned `Ok` with a resting remainder
  (GTC) — defeating STP and never recording the terminal `SelfTradePrevention`
  state. `match_order_inner` now returns a `MatchOutcome` carrying a
  `taker_stp_cancelled` flag; `add_order` cancels the residual (records
  `Cancelled { SelfTradePrevention }` with the true filled quantity and returns
  `SelfTradePrevented`) instead of resting it. The public `match_order` /
  `match_order_with_user` signatures are unchanged.
- **STP scan follows insertion-sequence (sweep) order (#132).** Bumped `pricelevel`
  0.8.0 → 0.8.1 for `PriceLevel::snapshot_by_insertion_seq()`. The STP pre-scan and
  the FOK feasibility scan now read it instead of `snapshot_orders()`
  (`(timestamp, sequence)`-ordered), so `safe_quantity` and the cancelled / selected
  maker match what `match_order` actually consumes even under non-monotonic
  timestamps — closing the consumption-fidelity gap that #94 left open (and which the
  #94 determinism fix only addressed for monotonic timestamps).
- **`cancel_all_orders` resets per-account risk counters (#99).** The bulk cancel
  drained the book but never touched `risk_state`, so after a mass unwind every
  account's `open_orders` / `notional` counters stayed at pre-cancel values and
  permanently rejected new flow (the exact failure bulk cancel exists to avoid). It
  now calls a new `RiskState::clear()` (also reused by `rebuild_from_snapshot`),
  zeroing the per-account counters and the per-order risk map.
- **Snapshot packages preserve the scheduled market close (#100).**
  `create_snapshot_package` did not capture `market_close_timestamp` /
  `has_market_close`, and `restore_from_snapshot` reset them to `0` / `false`, so a
  book with a configured market close silently lost it (and its DAY / GTD expiry
  schedule) after a snapshot round-trip or replay. The two values are now carried on
  `OrderBookSnapshotPackage` (additive `#[serde(default)]`, format version stays 2)
  and re-applied on restore, mirroring `kill_switch_engaged`.

## [0.8.0] — 2026-05-03

### Added — Quote-notional market orders (#85)

- **New public API** on `OrderBook<T>`: `match_market_order_by_amount`
  and `match_market_order_by_amount_with_user`, plus the convenience
  wrappers `submit_market_order_by_amount` and
  `submit_market_order_by_amount_with_user` (run kill-switch and
  pre-trade risk gates before matching). Implements Binance
  `quoteOrderQty` semantics — callers say "buy ~$1,000 of BTC" without
  converting to base quantity. Fees are exclusive: caller pays
  `amount + taker_fee`.
- **Lot enforcement.** When `OrderBook::with_lot_size` is configured,
  the per-level base quantity is rounded down to a multiple of
  `lot_size`. Notional walks never emit `qty = 0` trades when the
  remaining budget cannot fund one full lot at the current level.
- **New error variant `OrderBookError::InsufficientLiquidityNotional
  { side, requested, spent }`** distinguishes notional from base-qty
  insufficiencies.
- **`TradeResult.quote_notional: u128`** — populated for both
  base-quantity and quote-notional market-order paths. Carries
  `Σ price × quantity` so consumers do not recompute per-trade.
  `#[serde(default)]` keeps pre-0.7.x-tail JSON / Bincode payloads
  parseable.
- **Additive `SequencerCommand::MarketOrderByAmount { id, amount, side }`**
  variant. Old journals replay byte-identical; the new variant ferries
  through `submit_market_order_by_amount` on replay. No
  `ORDERBOOK_SNAPSHOT_FORMAT_VERSION` bump required.
- **`StopCondition` refactor of the matching loop** — single inner
  implementation drives both base-qty and notional walks. The base-qty
  path retains its previous arithmetic profile when `lot_size` is unset
  (`lot <= 1` ⇒ no rounding work).
- Runnable example: `cargo run -p examples --bin market_order_by_amount`.
- HDR latency bench: `notional_walk_hdr` mirrors `aggressive_walk_hdr`
  on the notional path.

## [0.7.0] — 2026-04-25

> 0.7.0 ships issues #51..#60 and the centralised `engine_seq` minting
> refactor (#73). Sub-headings below group changes by feature.

### Added — feature-gated allocation counter (#58)

- **New feature `alloc-counters`** (default off). Exposes
  `CountingAllocator<Inner: GlobalAlloc>` and `AllocSnapshot` at the
  crate root, layering four `AtomicU64` counters (`allocs`,
  `deallocs`, `bytes_allocated`, `bytes_deallocated`) on top of any
  inner allocator. Bench / test binaries opt in by installing the
  wrapper as `#[global_allocator]`.
- **Bench `alloc_count`** at `benches/order_book/alloc_count.rs`
  (also feature-gated) runs the mixed 70 / 20 / 10 workload, prints
  `allocs_per_op` + `bytes_alloc/op` to stdout, and writes a small
  markdown summary to `target/alloc-counters/<scenario>.md`.
- **Integration test `alloc_budget_tests`** at
  `tests/unit/alloc_budget_tests.rs` runs 10 000 mixed ops and
  asserts `allocs/op < 10` — conservative ceiling tuned to catch
  order-of-magnitude regressions in CI, not to certify zero.
- **`BENCH.md`** gains an "Allocation profile" section with the
  workflow + a reference number from a single M4 Max run.
- **`mod utils` made `pub mod utils`** so the new types are
  reachable via `orderbook_rs::utils::CountingAllocator` as well as
  the crate-root re-export. Existing `pub use utils::current_time_millis`
  unchanged.

### Notes — alloc counter

- The library `rlib` does **not** install a `#[global_allocator]` —
  consumers pick their own (`jemalloc`, `mimalloc`, system, …). The
  wrapper exists to give bench / test binaries a measurement hook
  without forcing a global choice on the library.
- `counting_allocator.rs` carries a documented
  `#[allow(unsafe_code)]` exception to the crate's
  `#![deny(unsafe_code)]` policy because Rust's `GlobalAlloc` trait
  requires `unsafe impl`. The exception is gated on the feature flag
  and confined to the wrapper module; every `unsafe` block
  delegates immediately to the inner allocator.

### Added — Prometheus metrics feature (#60)

- **New optional `metrics` feature flag** (default off). When
  enabled, the matching core emits Prometheus-style counters and
  gauges through the [`metrics`](https://docs.rs/metrics) crate's
  global facade. Any compatible recorder (Prometheus exporter,
  OpenTelemetry bridge, custom collector) can scrape them.
- **Surface (stable across `0.7.x`):**
  - `orderbook_rejects_total{reason="..."}` — counter,
    incremented exactly once per rejection. Label value is the
    `RejectReason` `Display` string.
  - `orderbook_depth_levels_bid` / `orderbook_depth_levels_ask`
    — gauges, current count of distinct price levels per side,
    refreshed on every add / cancel / modify / fill.
  - `orderbook_trades_total` — counter, monotonic count of every
    emitted trade transaction (one increment per `MatchResult`
    transaction, summed across all listener-emitted and
    internal-only matches).
- **Out-of-band emission.** Allocation-free on the happy path,
  no influence on matching outcomes, no recorder dependency on
  the core engine. `restore_from_snapshot_package` does **not**
  rehydrate counters — operational only, process-lifetime.
- **Compile-time no-op when the feature is disabled.** Every
  helper in `orderbook::metrics` compiles down to an empty
  function so call-sites in the matching hot path stay
  unconditional.
- **`metrics = "0.24"`** is the new optional dependency.
- Integration test `tests/metrics/` (its own test binary so the
  global recorder isn't perturbed by the rest of the suite)
  covers reject counts, trade counts, depth gauges, and a
  determinism guard that proves metrics emission does not alter
  byte-identical snapshots.
- Example `examples/src/bin/prometheus_export.rs` demonstrates
  installing `metrics-exporter-prometheus` and dumping the
  exposition payload.

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
