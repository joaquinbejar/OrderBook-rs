[![Dual License](https://img.shields.io/badge/license-MIT-blue)](./LICENSE)
[![Crates.io](https://img.shields.io/crates/v/orderbook-rs.svg)](https://crates.io/crates/orderbook-rs)
[![Downloads](https://img.shields.io/crates/d/orderbook-rs.svg)](https://crates.io/crates/orderbook-rs)
[![Stars](https://img.shields.io/github/stars/joaquinbejar/OrderBook-rs.svg)](https://github.com/joaquinbejar/OrderBook-rs/stargazers)
[![Issues](https://img.shields.io/github/issues/joaquinbejar/OrderBook-rs.svg)](https://github.com/joaquinbejar/OrderBook-rs/issues)
[![PRs](https://img.shields.io/github/issues-pr/joaquinbejar/OrderBook-rs.svg)](https://github.com/joaquinbejar/OrderBook-rs/pulls)

[![Build Status](https://img.shields.io/github/actions/workflow/status/joaquinbejar/OrderBook-rs/build.yml)](https://github.com/joaquinbejar/OrderBook-rs/actions)
[![Coverage](https://img.shields.io/codecov/c/github/joaquinbejar/OrderBook-rs)](https://codecov.io/gh/joaquinbejar/OrderBook-rs)
[![Dependencies](https://img.shields.io/librariesio/github/joaquinbejar/OrderBook-rs)](https://libraries.io/github/joaquinbejar/OrderBook-rs)
[![Documentation](https://img.shields.io/badge/docs-latest-blue.svg)](https://docs.rs/orderbook-rs)



## High-Performance Lock-Free Order Book Engine

A high-performance, thread-safe limit order book implementation written in Rust. This project provides a comprehensive order matching engine designed for low-latency trading systems, with a focus on concurrent access patterns and lock-free data structures.

### Key Features

- **Lock-Free Architecture**: Built using atomics and lock-free data structures to minimize contention and maximize throughput in high-frequency trading scenarios.

- **Multiple Order Types**: Support for various order types including standard limit orders, iceberg orders, post-only, fill-or-kill, immediate-or-cancel, good-till-date, trailing stop, pegged, market-to-limit, and reserve orders with custom replenishment logic.

- **Thread-Safe Price Levels**: Each price level can be independently and concurrently modified by multiple threads without blocking.

- **Advanced Order Matching**: Efficient matching algorithm for both market and limit orders, correctly handling complex order types and partial fills.

- **Performance Metrics**: Built-in statistics tracking for benchmarking and monitoring system performance.

- **Memory Efficient**: Designed to scale to millions of orders with minimal memory overhead.

### Design Goals

This order book engine is built with the following design principles:

1. **Correctness**: Ensure that all operations maintain the integrity of the order book, even under high concurrency.
2. **Performance**: Optimize for low latency and high throughput in both write-heavy and read-heavy workloads.
3. **Scalability**: Support for millions of orders and thousands of price levels without degradation.
4. **Flexibility**: Easily extendable to support additional order types and matching algorithms.

### Use Cases

- **Trading Systems**: Core component for building trading systems and exchanges
- **Market Simulation**: Tool for back-testing trading strategies with realistic market dynamics
- **Research**: Platform for studying market microstructure and order flow
- **Educational**: Reference implementation for understanding modern exchange architecture

### What's New in Version 0.13.0

#### v0.13.0 — the public API hands out no level handles (#228); exclusive submit gate under STP (#225); replay re-executes coded submit rejections (#224)

- **Breaking (semver-minor under 0.x): `OrderBook::get_bids` and
  `OrderBook::get_asks` are removed (#228).** Both cloned the book's live
  `Arc<PriceLevel>` handles into a `DashMap`, and `PriceLevel` exposes
  `add_order`, `update_order` and `match_order` publicly, so a caller
  holding one could mutate a price level behind the submit gate, the
  `order_locations` / user-order indices, the risk state, self-trade
  prevention, the kill switch, the order-state tracker and the trade /
  book-change listeners. Deprecating them would have left the bypass
  reachable, so they are gone and 0.13.0 is the release boundary for
  breaking changes. Migrate to the read-only APIs, which return values
  rather than handles: `create_snapshot(depth)` for a full snapshot of
  every level and order; `levels_with_cumulative_depth`,
  `levels_until_depth`, `levels_in_range` and `find_level` for `LevelInfo`
  views; `order_count_at_price`, `get_orders_at_price`, `get_all_orders`
  and `total_depth_at_levels` for per-price and per-book order data;
  `best_bid` / `best_ask` for the top of book. Every level mutation now
  goes through `OrderBook`.
- **Breaking (semver-minor under 0.x): the level iterators' `new`
  constructors are crate-private (#228).**
  `LevelsWithCumulativeDepth::new`, `LevelsUntilDepth::new` and
  `LevelsInRange::new` each take a reference to the book's live price-level
  map, and with `get_bids` / `get_asks` gone no public API yields one. The
  iterator types stay public; obtain them from
  `OrderBook::levels_with_cumulative_depth`, `levels_until_depth` and
  `levels_in_range`.
- **Self-trade prevention holds under concurrent same-user admission
  (#225).** An STP-relevant submit decided a price level's `STPAction`
  from a queue snapshot and then filled that level in a second operation,
  both under the *shared* side of the submit gate — so a concurrent
  same-user admission could land between the two and be filled by the very
  sweep the scan was protecting. STP-relevant submits and the
  cancel-then-add modify variants whose re-add can match (`UpdatePrice`,
  `UpdatePriceAndQuantity`, `Replace`) now take the **exclusive** side, so
  the scan and the fill it authorises observe the same queue. Cost: on an
  STP book every identified submit except post-only, and every
  matching-capable re-price, is serialized. `STPMode::None` books,
  post-only submits, `UpdateQuantity` and `Cancel` keep the shared, fully
  concurrent path. With no level handles left to bypass it (#228), the
  gate now covers every mutation.
- **`SequencerResult::RejectedWithCode { reason, code, may_have_mutated,
  stp_mode }`.** `add_order` emits real fills and *then* returns `Err`
  for an IOC's unfillable remainder and for a taker STP cancels after
  non-self fills; `ReplayEngine` skipped every rejected event, so replay
  rebuilt liquidity the live book had consumed. Producers now opt in by
  recording the typed outcome — `SequencerResult::from(&error)` fills
  all four fields — and replay decides by the recorded code: a submit
  rejected under a code replay can reproduce from the book state and
  `ReplayBookConfig` is re-executed and must fail the same way again,
  while codes whose trigger lives outside the config (kill switch, risk
  limits, `Other`) are skipped rather than re-executed, because a
  rejection that never touched the book is reproduced by doing nothing.
  `last_applied_seq` / the applied count / the progress callback follow
  what was **dispatched**, so a re-executed rejection advances them.
- **The two facts the reject code cannot carry.** `may_have_mutated`
  flags the errors the engine can return after changing the book,
  including the residual-admission `PriceLevelError` that maps to
  `RejectReason::Other(0)`; a flagged submit is re-executed whatever its
  code says, so that rejection no longer replays as a no-op that
  resurrects consumed liquidity. `stp_mode` records the mode that
  decided a self-trade-prevention rejection, and replay refuses a
  mismatched `ReplayBookConfig`.
- **`ReplayError::OutcomeMismatch { sequence_num, recorded, actual }`**
  aborts replay when a re-executed rejection succeeds or fails under a
  different code than the journal recorded;
  **`ReplayError::StpModeMismatch { sequence_num, recorded, actual }`**
  aborts it when a journaled STP rejection was decided under a different
  `STPMode` than the replay book uses.
- **Migration.** The string-only `SequencerResult::Rejected` keeps its
  historical skip, so a journal written with it keeps the pre-existing
  gap for traded-then-rejected submits; switch producers to
  `RejectedWithCode`. Journals carrying the new variant cannot be
  decoded by older readers (existing journals decode unchanged, as for
  `MarketOrderByAmount`). **Limitations:** only the reject code is
  reconciled, never the error's details or the fills behind it, so a
  discrepancy confined to them can go undetected — `snapshots_match`
  (directly, or via `ReplayEngine::verify`) is the check that catches a
  diverged book, and `replay_from` performs none. The `stp_mode` guard
  only fires on journals that recorded an STP rejection.
  **Breaking (semver-minor under 0.x):** `ReplayError` gained two
  variants, so exhaustive matches need new arms; 0.13.0 is the release
  boundary for them together with the #228 removal. No snapshot format
  change.
- **Reserve orders are lot-size validated per tranche and on their
  replenishment transfer (#226).** A `ReserveOrder` used to be checked on
  its **total** only, so a 15 visible / 5 hidden reserve was admitted to a
  lot-10 book while the identical iceberg was rejected. It now takes the
  iceberg's per-tranche rule and, additionally, validates the capped
  quantity replenishment transfers from hidden into the visible tranche —
  `min(replenish_amount.unwrap_or(DEFAULT_RESERVE_REPLENISH_AMOUNT),
  hidden)`, checked while `hidden > 0` and `auto_replenish` is on.
  Admission is strictly tighter: a shape previously admitted on its total
  is now rejected with `InvalidLotSize`, carrying the offending tranche or
  transfer.
- **A reserve residual follows `auto_replenish` (#230).** The
  residual-resting helper behind `OrderQuantity::set_total_remaining`
  refreshed an emptied visible tranche from `replenish_amount` alone,
  ignoring `auto_replenish`, falling back to a refresh of zero (which
  could rest a zero-visible order) and never consulting
  `replenish_threshold`. It now applies `pricelevel`'s rule: with
  automatic replenishment on and hidden left, a visible tranche below
  `max(replenish_threshold, 1)` grows by the explicit amount or
  `DEFAULT_RESERVE_REPLENISH_AMOUNT`, capped by hidden; with it off the
  residual does not rest at all and its hidden remainder is discarded,
  mirroring the removal of a depleted non-auto maker. A 10 visible / 20
  hidden reserve with `replenish_amount = Some(10)` and no automatic
  replenishment, filled for 10, used to rest 10 / 10 and now ends as
  `Filled { filled_quantity: 10 }`; the same order with automatic
  replenishment and a threshold of 5, filled for 8, used to rest 2 / 20
  and now rests 12 / 10. The discard needs the visible tranche to be
  **exhausted**: with automatic replenishment off, a 10 / 20 reserve
  filled for 5 still rests 5 / 20. An explicit `replenish_amount` is the
  transfer, added to whatever visible quantity survived, not a target
  display size. The accounting rule is
  `submitted = executed + resting (visible + hidden) + discarded`, and
  discarded quantity is never counted as executed. A discard emits an
  `INFO` trace and, under the `metrics` feature, the new
  `orderbook_reserve_discards_total` /
  `orderbook_reserve_hidden_discarded_total` counters, carrying a `path`
  field so the aggressive taker and the removed maker report the same
  discard the same way; the returned order handle carries both tranches
  at zero. Because the three
  cancel-then-add modify variants re-add the order as a taker, a
  validate-first pre-check now rejects a re-price that would exhaust such
  a reserve's visible tranche with the new
  `OrderBookError::ReserveResidualWouldBeDiscarded` **before** the
  original is cancelled, so **a re-price of such a reserve cannot destroy
  the order it modifies**: the exclusive guard covers the lookup, the
  validation, the cancel and the re-add, so the dry run is exact. That
  is the scope of the guarantee; it is not a claim about every possible
  modification failure. Crossing into depth smaller than the visible tranche, a
  non-crossing re-price and a projected full fill are all allowed
  through. The error carries both the projected `hidden_quantity` and the
  `discarded_quantity` that would actually be destroyed. `RejectReason`
  gains the matching wire code 14; both enums are `#[non_exhaustive]`.
  In a book that **holds** such a reserve, every sweep now takes the
  **exclusive** submit gate in every `STPMode` — matching-capable
  submits, cancel-then-add re-prices and the match-only entry points
  alike, plus the admission of the first one — so nothing can cancel,
  admit or replace an order inside a sweep's capture window: the sweep
  cannot consume a maker it never captured, nor report a captured maker
  after a cancel freed its id. Cancels and mass cancels keep the shared
  side. Those books serialize their sweeps; books holding none are
  unchanged.
- **A non-replenishing reserve must display a positive visible tranche
  (#230).** A `ReserveOrder` with `auto_replenish == false`,
  `visible_quantity == 0` and `hidden_quantity > 0` used to rest as a
  ghost: no visible depth, and `pricelevel` removes it without a trade,
  stranding the whole hidden tranche, on the first taker to reach the
  level. Since #221 a zero quantity on `UpdatePriceAndQuantity` /
  `Replace` could drive a healthy resting reserve into that shape too;
  `UpdateQuantity` with a zero quantity cannot, because it is a removal
  taken before the validator runs (#223, below). `validate_order_shape`
  now rejects it with the new `OrderBookError::ZeroVisibleTranche`,
  covering `add_order`,
  every modify projection and snapshot restore; a rejected modify leaves
  the original resting and a rejected restore leaves the book untouched.
  The rule is that shape **only**: a zero-visible iceberg draws its whole
  hidden tranche into visible on match, and a zero-visible
  auto-replenishing reserve refreshes and re-queues, so both execute and
  stay admissible. Single-tranche kinds are unaffected. Maps to the
  existing `RejectReason::InvalidQuantity`.
- **`OrderUpdate::UpdateQuantity` with a zero quantity cancels the order
  (#223).** A zero `new_quantity` was accepted and applied as a resize:
  `pricelevel` keeps a non-growing total in place, so the maker rested at
  zero depth, held `best_bid` / `best_ask` on a level with nothing behind
  it and was later dropped by a sweep with no trade and no cancel event,
  leaking its `order_locations` entry — `cancel_order` then returned
  `Ok(None)` while re-adding the id reported `DuplicateOrderId`. The arm
  now routes to the same `UserRequested` cancel `OrderBook::cancel_order`
  performs, so the level-change event, the `Cancelled { UserRequested }`
  transition, the per-account risk release, the location / user-index
  untrack and the empty-level removal happen in lockstep. **A zero
  requested quantity is a removal, not a resize:** it cancels the
  *entire* order, hidden depth of an iceberg or reserve included (a
  nonzero `new_quantity` still resizes only the visible tranche), and it
  runs neither the projected-order validator nor the modify-aware risk
  check, so neither a configured `min_order_size` nor a risk limit vetoes
  it. The kill switch still refuses it, as it refuses every modify. The
  removal semantic is `UpdateQuantity`'s alone: a zero quantity on
  `Replace` / `UpdatePriceAndQuantity` re-adds through validate-first
  (#230 above). Compatibility: a journal recorded before this change that
  contains a zero `UpdateQuantity` replays to the new outcome, so the
  replayed book legitimately differs from the one the original run
  produced.
- **Reserve `UpdatePriceAndQuantity` honours the requested visible
  quantity (#221).** `OrderQuantity::set_quantity` read a reserve's
  argument as a **total** target and only ever reduced, so a requested
  increase was silently dropped (a 30 / 70 reserve asked to move to 80
  ended at 10 / 70) and a decrease was drawn across both tranches. It now
  sets the **visible** tranche and leaves hidden untouched for both
  two-tranche kinds, matching `UpdateQuantity`, `Replace` and the upstream
  `pricelevel` contract.
- **`CancelTaker` and `CancelBoth` fire only on a same-user maker the
  taker can reach (#222).** Both arms used to cancel unconditionally once
  a same-user maker rested at a crossed level, even when the non-self
  depth queued ahead of it already satisfied the taker. A client saw
  `SelfTradePrevented` on an order that had in fact filled, and
  `CancelBoth` destroyed a maker the sweep never touched — silently on
  the market paths, which drop the taker-cancelled flag and return `Ok`.
  The arms now execute against the non-self depth first and cancel only
  if the taker could still execute at that price afterwards.
  `STPMode::CancelMaker` is deliberately unchanged: it still cancels every
  same-user order at a level the sweep touches, since it never destroys
  the taker. The modify pre-check `check_modify_stp_self_cross` follows
  the same per-level rule and sizes the pre-match with `pricelevel`'s
  authoritative dry run rather than the counted visible depth, so it
  cannot admit a re-price the sweep would then kill after the original
  was cancelled.
- **A quote-notional sell walks past a bid it cannot afford (#222).** A
  zero per-level quantity cap ended the whole sweep. That is right for a
  base-quantity budget, whose cap ignores the level price, and for a
  notional buy, which walks asks ascending so the cap only shrinks. It
  was wrong for a notional sell, which walks bids descending: a budget
  too small at one bid can fund a whole lot at a cheaper one. Selling 150
  into bids of 100, 75 and 50 executed one unit instead of two. The sell
  walk now skips the unaffordable level and stops only on a spent budget,
  an exhausted side, or a remainder below one lot — the point at which no
  price could fund a lot. It may therefore visit every level on the bid
  side; each skipped level costs one division and mutates nothing.

### What's New in Version 0.12.0

#### v0.12.0 — pricelevel 0.9 hardening bump; upsize demotion survives snapshot restore (#205)

- **`pricelevel` 0.8.4 → 0.9.1.** Major upstream hardening release: level
  admission validates before mutating (duplicate id, counter capacity,
  price/side topology), PostOnly / fill-or-kill decisions are atomic with
  the sweep, execution statistics are torn-read-safe, and level snapshots
  materialize orders in queue-consumption order. 0.9.1 fixes the
  `MatchResult` bincode round-trip (PriceLevel#135), keeping the
  `bincode` feature's trade-event round-trip intact.
- **The upsize queue-priority demotion now survives a snapshot
  round-trip (#205).** Restoring a snapshot rebuilds each level's queue
  exactly as matching would consume it, so an order demoted by a quantity
  increase keeps its back-of-queue position after
  `restore_from_snapshot_package`. Locked in by a proptest regression
  (`tests/unit/props_quantity_update_priority.rs`). Snapshots captured
  with pricelevel < 0.9 restore demoted orders at their old
  `(timestamp, seq)` position — re-snapshot to pin the corrected order.
- **Breaking (semver-minor under 0.x):** `get_bt_bids` / `get_bt_asks`
  now return `Result<BTreeMap<u128, PriceLevel>, OrderBookError>`
  (snapshot-to-level conversion is validating and fallible upstream), and
  the re-exported pricelevel surface changed —
  `PriceLevel::add_order` returns `Result`, `matchable_quantity` takes
  the taker id, `PriceLevelError` gained `DuplicateOrderId`.
- **Atomic PostOnly / multi-level FOK (#209).** PostOnly submits thread
  `TakerKind::PostOnly` into every per-level match, making it
  structurally impossible for a post-only order to take liquidity under
  any interleaving; fill-or-kill submits hold a new book-level submit
  gate exclusively across feasibility + sweep, so multi-level
  all-or-nothing can no longer partially execute against concurrent
  cancels. Other mutating entry points take the gate's uncontended read
  side; the matching core stays lock-free. Full 0.11.0 → 0.12.0 HDR
  tail-latency comparison in `BENCH.md`: every scenario's median is
  unchanged by this release's book-level work; the one median shift
  (`stp_sweep`, from the pricelevel 0.9 hardening) is documented there
  with its bisection.
- **Atomic, observable mutation failures (#211).** `UpdateQuantity` is
  validate-first (projected tick / lot / min-max / representability /
  risk before touching the level), propagates upstream
  `PriceLevelError`s instead of returning `Ok(None)`, and updates risk
  counters on success; a taker whose residual cannot rest is rejected
  before the sweep trades; a failed racy admission cleans up any empty
  level it created.
- **Two-tranche quantity conservation (#210).** An aggressive iceberg's
  residual rests with exactly the unmatched total distributed across
  tranches (`visible = min(display, remainder)`, rest hidden) instead of
  inflating the book, and a `visible + hidden` overflow is rejected at
  admission with the new typed `OrderBookError::QuantityOverflow`
  before any trade or mutation. Conservation
  (`executed + resting == submitted`) is property-tested.
- **`snapshots_match` compares full maker state and FIFO (#208).** The
  replay oracle now checks every level's order vector in
  queue-consumption order (ids, variants, users, quantities,
  timestamps, TIF, type-specific fields) and the deterministic
  statistics counters including `stats_degraded`; only the wall-time
  statistics aggregates (`first_arrival_time`, `last_execution_time`,
  `sum_waiting_time` — see the `snapshots_match` docs for why each is
  inherently divergent) and the capture timestamp stay excluded.
  Contract tightening: aggregate-equal books with reversed FIFO or
  different maker identity no longer certify as replay-equal.
- **Failure-atomic snapshot restore (#207).** `restore_from_snapshot` and
  `restore_from_snapshot_package` validate every level (and reject
  cross-level duplicate order ids with `DuplicateOrderId`) against
  off-book structures before clearing the live book, so a failed restore
  leaves the pre-restore state — orders, indices, config, risk,
  kill-switch, engine sequence — completely untouched.
- **Snapshot package format v3 (#206).** Pricelevel 0.9 statistics can
  serialize a `stats_degraded` field that 0.8 readers reject, so newly
  written packages are stamped `ORDERBOOK_SNAPSHOT_FORMAT_VERSION = 3`.
  Reads accept `ORDERBOOK_SNAPSHOT_MIN_READ_VERSION (2)..=3` — legacy v2
  packages still restore — while `1` and future versions stay rejected
  with the existing typed error.

### What's New in Version 0.11.0

#### v0.11.0 — replay reproduces the trade-ID stream: namespace in `ReplayBookConfig` (#200)

- **`ReplayBookConfig.trade_id_namespace: Option<Uuid>`.** v0.10.5 (#199)
  made the trade-ID namespace injectable on `OrderBook`, but every
  `ReplayEngine::replay_from*` entry point still built its book with a
  random namespace, so trade IDs produced through the shipped replay API
  were not reproducible. The config now carries the live book's
  namespace and applies it via `OrderBook::set_trade_id_namespace`
  before any journal events are replayed; a `*_with_config` replay under
  an injected `Clock` then reproduces the live trade-ID stream
  byte-identically. `ReplayBookConfig::new` keeps its six structural
  parameters (namespace defaults to `None`) — chain the new
  `with_trade_id_namespace(namespace)` builder to set it. Without a
  namespace the fresh book keeps a random one, as before.
- **Suffix replays with a namespace are rejected.** Applying a
  namespace restarts the trade-ID counter at 0, so a namespace-carrying
  config with `from_sequence != 0` would mint wrong or duplicate IDs;
  the `*_with_config` entry points return the new typed
  `ReplayError::NamespaceRequiresFullReplay` instead. Namespace-free
  suffix replay keeps working.
- **Breaking (semver-minor under 0.x):** `ReplayBookConfig` gained a
  public field, so exhaustive struct literals no longer compile — add
  `trade_id_namespace: None` or use `..Default::default()`; and
  `ReplayError` gained the `NamespaceRequiresFullReplay` variant, so
  exhaustive matches need a new arm.
  `ReplayBookConfig::new(...)` callers are unaffected. No journal or
  snapshot format change, no `ORDERBOOK_SNAPSHOT_FORMAT_VERSION` bump.

### What's New in Version 0.10.5

#### v0.10.5 — injectable trade-ID namespace (#199)

- **`OrderBook::set_trade_id_namespace(&mut self, namespace: Uuid)`.**
  Every constructor used to mint the trade-ID namespace internally with
  `Uuid::new_v4()`, so trade IDs differed between a live run and its
  replay even with an injected `Clock` and an identical command stream —
  the namespace was the only entropy left in the trade-ID stream
  (`pricelevel::UuidGenerator` is UUID v5 over namespace + counter).
  The new setter, symmetric with `set_clock`, replaces the generator
  (counter restarts at 0) and composes with every existing constructor.
  Call it before any orders are submitted.
- **`OrderBook::with_clock_and_namespace(symbol, clock, namespace)`.**
  Convenience constructor for the fully deterministic setup (injected
  clock + injected namespace): the same command stream then produces
  byte-identical trade IDs across live/replay. A deterministic
  namespace choice such as UUID v5 of the symbol under a venue root
  gives every book a stable, distinct stream.
- Default constructors are unchanged: without injection each book still
  gets a fresh random namespace. No wire-format or snapshot change, no
  `ORDERBOOK_SNAPSHOT_FORMAT_VERSION` bump. Note the guarantee currently
  applies to books you construct yourself; the sequencer's
  `ReplayEngine` entry points still build their books with a random
  namespace — wiring the seam into `ReplayBookConfig` is tracked in
  issue #200.

### What's New in Version 0.10.4

#### v0.10.4 — exact-fee API: `try_calculate_fee` + published guaranteed-exact bound (#197)

- **`FeeSchedule::try_calculate_fee(notional, is_maker) -> Result<i128, FeeOverflow>`.**
  Fallible variant of `calculate_fee` with identical rounding
  (truncation toward zero, sign applied after the unsigned-domain
  magnitude) that returns the new `FeeOverflow` error instead of
  clamping when `notional × |bps|` overflows `u128`. An `Ok` value is
  always the mathematically exact fee and equals `calculate_fee`'s
  output, so journaled / replayable venues can reject an order rather
  than record a clamped fee.
- **Published guaranteed-exact input bound.**
  `FeeSchedule::max_guaranteed_exact_notional_for_bps(bps)` (`const fn`)
  returns the multiplication-safety bound `u128::MAX / |bps|`
  (`u128::MAX` for a zero rate) at or below which the fee is guaranteed
  exact, and `FeeSchedule::max_guaranteed_exact_notional()` takes the
  minimum over the maker and taker legs — a single venue-level admission
  bound that makes the saturating branch of `calculate_fee` provably
  unreachable. The guarantee is sufficient, not tight: above the bound
  `try_calculate_fee` rejects conservatively even though the clamped
  `calculate_fee` value can coincide with the exact fee at isolated
  notionals.
- `calculate_fee` behavior is unchanged (bit-identical, including the
  saturated clamp of magnitude `u128::MAX / 10_000`); its docs now state
  the exactness guarantee. `FeeOverflow` is re-exported at the crate
  root. No wire-format change, no
  `ORDERBOOK_SNAPSHOT_FORMAT_VERSION` bump.

### What's New in Version 0.10.3

#### v0.10.3 — special-order tracker survives snapshot restore (#194)

- **Restored pegged / trailing-stop orders re-price again.**
  `restore_from_snapshot` rebuilt the resting book but left the
  `special_order_tracker` (the `special_orders` feature) freshly-initialized,
  so a restored pegged or trailing-stop order was never re-registered and
  never re-priced after a snapshot restore. The shared rebuild pass now
  re-registers every restored resting special order in the same deterministic
  price-then-insertion-sequence walk that repopulates `order_locations` /
  `user_orders`. The tracker holds only order ids — the trailing-stop
  watermark (`last_reference_price`) and the pegged / stop price live in the
  order data and survive the round-trip, so no re-pricing state is lost.
- No wire-format or public-API change: no new fields, no event-shape change,
  and no `ORDERBOOK_SNAPSHOT_FORMAT_VERSION` bump.

### What's New in Version 0.10.2

#### v0.10.2 — deterministic `user_orders` rebuild on snapshot restore (#192)

- **`cancel_orders_by_user` is now byte-identical across restores.**
  `restore_from_snapshot_package` rebuilt the `user_orders` index from each
  level's order-unstable `iter_orders()` view, so the per-user `Vec<Id>` came
  back in a different order on every fresh book (the `DashMap` hasher is seeded
  per instance) and a post-restore `cancel_orders_by_user` diverged across
  restores of the same package. The rebuild now walks price levels in the same
  fixed price-then-insertion-sequence order the mass-cancel sweeps use
  (`PriceLevel::snapshot_by_seq_into`), so the restored index — and any
  subsequent by-user cancel — is deterministic across every restore. The order
  reflects the resting book at snapshot time, not the original admission
  history (a snapshot cannot recover that). Pure journal replay was unaffected.
- No wire-format or public-API change: no new fields, no event-shape change,
  and no `ORDERBOOK_SNAPSHOT_FORMAT_VERSION` bump.

### What's New in Version 0.10.1

#### v0.10.1 — replay-stable mass-cancel result ordering (#190)

- **Deterministic `cancelled_order_ids` ordering.** `cancel_all_orders`,
  `cancel_orders_by_side`, and `cancel_orders_by_price_range` now enumerate
  cancelled orders through the same fixed traversal the eviction sweep uses:
  bids first then asks; within a side, price levels in ascending price (the
  `SkipMap`'s natural key order, no sort); within a level, ascending insertion
  sequence (`PriceLevel::snapshot_by_seq_into`, the exact order the matching
  engine consumes resting orders). Previously the ids were read from
  order-unstable structures (`order_locations` / per-level `iter_orders`) whose
  `DashMap` hasher is seeded per instance, so two processes replaying the same
  command stream could journal divergent `SequencerResult::MassCancelled`
  payloads. The cancelled **set** and **count** are unchanged — only the order
  of `cancelled_order_ids` is now byte-identical across processes and replay.
- **`cancel_orders_by_user` is unchanged** and was already replay-stable: it
  drains the `user_orders` index in admission-history order. That determinism
  contract is now documented alongside the others.
- No wire-format or public-API change: no new fields, no event-shape change,
  and no `ORDERBOOK_SNAPSHOT_FORMAT_VERSION` bump.

### What's New in Version 0.10.0

#### v0.10.0 — host-driven GTD / DAY expiry sweep (#189)

- **New `OrderBook::evict_expired_orders(now_ms)`** — a host-driven sweep
  that removes every resting order whose time-in-force has expired as of the
  caller-supplied timestamp. `now_ms` is [`TimestampMs`] (Unix milliseconds,
  the same unit `clock().now_millis()` compares against) and is passed in by
  the caller — the sweep never reads the book's own clock — so a scheduler
  drives cadence and the sequencer can journal the exact cutoff. The matching
  hot path is untouched: there is no lazy per-match expiry check, so expiry is
  an explicit maintenance pass, not an implicit cost on every submit. The
  honest consequence: an expired-but-unswept `Gtd` / `Day` order remains
  resting and matchable — it can still trade until the host calls the sweep;
  the no-post-expiry-trade guarantee holds only after the sweep runs. Expiry
  uses the single boundary predicate that admission uses (`now >= deadline`
  for `Gtd`, `now >= market_close` for `Day`), so an order admitted at a given
  instant is never simultaneously evictable at that instant. Returns the
  evicted orders as `Vec<Arc<OrderType<T>>>`; a second sweep at the same
  `now_ms` is idempotent and returns empty.
- **Deterministic eviction order.** Evicted orders — and the
  `Cancelled { reason: TimeInForceExpired }` state transitions and
  `PriceLevelChangedEvent`s emitted as a side effect — follow one fixed,
  replay-stable order: bids first then asks; within a side, price levels in
  ascending price (the `SkipMap`'s natural key order, no sort); within a
  level, ascending insertion sequence (the exact order the matching engine
  consumes resting orders — not the non-deterministic `iter_orders` view).
  Each order is removed through the same single-order cancel path as
  `cancel_order`, so the price-level cache, depth statistics,
  `order_locations` / `user_orders` indices, risk state, special-order
  tracker, and order-state tracker all stay consistent.
- **Manager parity.** `BookManagerStd` and `BookManagerTokio` gain
  `evict_expired_orders(symbol, now_ms)` (per-symbol pass-through, `None` for
  an unknown symbol) and `evict_expired_across_books(now_ms)` (all books,
  mirroring the `cancel_*_across_books` idiom).
- **Journaled as a sequencer command.** New
  `SequencerCommand::EvictExpiredOrders { now_ms }` variant (appended, so
  existing journals' bincode variant indices are unchanged). Replay applies
  the journaled cutoff — never the replay clock — so the sweep reproduces
  byte-identically; `snapshots_match` holds between a live book and its
  replay. Old journals replay unchanged; new journals carrying the variant
  fail on older binaries, consistent with the `MarketOrderByAmount`
  precedent. No `ORDERBOOK_SNAPSHOT_FORMAT_VERSION` bump required (the version
  gates the snapshot package, not the journal command enum).
- **Breaking (the reason this is 0.10.0):** `SequencerCommand` and
  `SequencerResult` are now `#[non_exhaustive]`. Downstream code that
  matches them exhaustively must add a wildcard arm (`_ => …`) once and
  recompile; in exchange, future command/result additions are
  source-compatible instead of repeating this break. Adding the
  `EvictExpiredOrders` variant itself is what surfaced the hazard: on the
  previously-exhaustive enum it would have silently broken downstream
  matches inside the 0.9.x range.
- **`TimestampMs` re-exported** at the crate root and via [`prelude`], so the
  new `now_ms` parameter can be constructed without reaching into
  `pricelevel` directly.
- Runnable example: `cargo run -p examples --bin gtd_expiry_sweep`.

### What's New in Version 0.9.2

#### v0.9.2 — constant-work per-price aggregates + attribution & unit docs (#185, #186, #187)

- **Constant-work per-price aggregate accessors (#186)** — an O(log N)
  point lookup + O(1) counter read, with no per-order materialization. New
  read-only methods on [`OrderBook<T>`]: `visible_quantity_at_price`,
  `hidden_quantity_at_price`, `total_quantity_at_price`, and
  `order_count_at_price`. Each does an O(log N) `SkipMap` point lookup then
  reads the level's maintained atomic counter (one relaxed load; two for
  `total_quantity_at_price`, which sums visible + hidden) — no per-order
  `Arc` is materialized and no `T: Default` conversion runs, so they are the
  cheap way to poll one level's depth or count. `order_count_at_price` is
  the counterpart to `queue_ahead_at_price` that drops the per-order term:
  O(log N) here vs O(log N + K) for the queue-walking version. All four
  return `None` for an absent level and read advisory, eventually-consistent
  counters — take `create_snapshot` for a mutually-consistent view.
- **Per-call fill attribution, documented and proven (#185).** The
  `add_order_with_result` guarantee is now explicit: concurrent submits on
  the same book each receive exactly their own fills, because the
  `TradeResult` is built from that call's private `MatchResult` and the
  engine holds no shared trade accumulator. On the error-after-fills paths
  (an unfillable IOC remainder, or a self-trade-prevention cancellation
  after earlier non-self fills) the caller instead gets the typed `Err` and
  the executed fills reach only the trade listener. A multi-thread
  concurrency test pins it. New convenience wrappers
  `add_limit_order_with_result` and `add_limit_order_with_user_and_result`
  mirror the plain `add_limit_order*` builders while returning the
  `TradeResult` directly.
- **GTD / market-close millisecond unit documented (#187).** `has_expired`,
  `set_market_close_timestamp`, and the `time_in_force` parameter docs now
  state that GTD deadlines and the market-close timestamp are milliseconds
  since the Unix epoch (the same unit `clock().now_millis()` compares
  against). A pinning test proves a seconds-form deadline reads as instantly
  expired.

### What's New in Version 0.9.1

#### v0.9.1 — `add_order_with_result` (#184)

- **New public API** on [`OrderBook<T>`]: `add_order_with_result` submits an
  order and returns the `TradeResult` produced by the match directly —
  `Ok((Arc<OrderType<T>>, Option<TradeResult>))` — instead of relying on the
  `TradeListener` callback. `None` when the order produced no fills; an
  installed listener still fires with the exact same `TradeResult`.
  `add_order` is unchanged in behavior and stays free of the extra
  `MatchResult` clone when no listener is installed.

### What's New in Version 0.9.0

#### v0.9.0 — Upgrade to `pricelevel` 0.8.0 (#130)

- **Price-time priority preserved across partial fills.** Picks up the
  upstream `pricelevel` fix (PriceLevel#39) where a partially-filled resting
  maker keeps its place at the front of the level queue, resolving #88: a
  partial fill no longer demotes the maker behind later same-price arrivals.
  Locked in by `test_partial_fill_preserves_price_time_priority_issue_88`.
- **Deterministic match timestamps.** `PriceLevel::match_order` no longer
  reads the wall clock; the engine passes the book's [`Clock`]
  time as the taker timestamp, so trade timestamps follow the installed clock
  and replay stays deterministic.
- **Domain newtypes on the public surface (breaking).** Through the
  `pricelevel` re-exports and `MatchResult` / `OrderType` accessors, several
  values now carry `Quantity` / `Price` / `TimestampMs` instead of raw
  `u64` / `u128` (e.g. `MatchResult::remaining_quantity()` now returns
  `Quantity`). OrderBook-rs's own snapshot / statistics queries are unchanged
  and still return raw integers; downstream code reading `pricelevel` types
  through the re-exports may need `.as_u64()` / `.as_u128()`. Minor bump under
  `0.x` semver.
- **Dependency refresh:** `pricelevel` 0.7→0.8, `async-nats` 0.47→0.49,
  `dashmap` 6.1→6.2, `bitflags` 2.11→2.13, `either` 1.15→1.16,
  `crc32fast` 1→1.5, `proptest` 1.7→1.11.

### What's New in Version 0.8.0

#### v0.8.0 — Quote-notional market orders (#85)

- **New public API** on [`OrderBook<T>`]:
  [`match_market_order_by_amount`](OrderBook::match_market_order_by_amount)
  and the STP-aware
  [`match_market_order_by_amount_with_user`](OrderBook::match_market_order_by_amount_with_user),
  plus the convenience
  [`submit_market_order_by_amount`](OrderBook::submit_market_order_by_amount)
  and
  [`submit_market_order_by_amount_with_user`](OrderBook::submit_market_order_by_amount_with_user)
  wrappers that run the kill-switch and pre-trade risk gates.
- **Binance `quoteOrderQty` semantics.** Callers say "buy ~$1,000 of BTC"
  without converting to base quantity. The matching loop walks the
  opposite side until the requested quote-notional `amount` is
  consumed, the book is exhausted, or — when `lot_size` is configured
  on the book — the residual notional cannot fund another whole lot.
  Fees are **exclusive**: caller pays `amount + taker_fee`.
- **Lot enforcement preserved.** Per-level base quantity is rounded
  down to a multiple of `lot_size`, so notional walks never emit
  `qty=0` trades when the budget falls below one full lot at the
  current level price. `lot_size = None` is equivalent to `lot = 1`.
- **New error variant
  [`OrderBookError::InsufficientLiquidityNotional`]** — distinct from
  `InsufficientLiquidity` so callers can pattern-match on
  quote-vs-base semantics.
- **`TradeResult.quote_notional: u128`** — populated for *both* the
  base-quantity and quote-notional market-order paths so consumers
  read `Σ price × quantity` directly without recomputing per-trade.
  `#[serde(default)]` keeps existing JSON / Bincode payloads
  parseable.
- **Additive `SequencerCommand::MarketOrderByAmount { id, amount, side }`**
  variant. Old journals replay byte-identical; new journals carrying
  this variant fail on older binaries — consistent with the precedent
  for prior `SequencerCommand` rollouts. No
  `ORDERBOOK_SNAPSHOT_FORMAT_VERSION` bump required.
- **`StopCondition` refactor of the matching loop** — single inner
  implementation drives both base-qty and notional walks. Base-qty
  path stays allocation- and branch-light: the new helpers fold to
  the same arithmetic the previous loop emitted when `lot <= 1`.
- Runnable example: `cargo run -p examples --bin market_order_by_amount`.
- HDR bench: `notional_walk_hdr` mirrors `aggressive_walk_hdr` with
  the notional path so p50/p99/p99.9/p99.99 can be compared.

### What's New in Version 0.7.0

#### v0.7.0 — Feature-gated allocation counter

- **New feature `alloc-counters`** (default off). Exposes
  \[`CountingAllocator`\] and \[`AllocSnapshot`\] at the crate root.
  Wraps any inner [`GlobalAlloc`](std::alloc::GlobalAlloc) and
  tracks four `AtomicU64` counters: `allocs`, `deallocs`,
  `bytes_allocated`, `bytes_deallocated`.
- Bench / test binaries opt in via
  `#[global_allocator] static A: CountingAllocator<System> = ...`.
  The library `rlib` does **not** install a global allocator.
- **`bench_count`** bench + **`alloc_budget_tests`** integration
  test run the mixed 70/20/10 workload; the bench reports
  `allocs_per_op`, the test asserts a conservative ceiling for
  regression detection.
- **`BENCH.md`** gains an "Allocation profile" section.

#### v0.7.0 — Metrics and Observability (#60)

- **New optional `metrics` feature** wires Prometheus-style
  counters and gauges into the matching engine. Default `off`;
  when enabled, every increment goes through the global
  [`metrics`](https://docs.rs/metrics) facade so any compatible
  recorder (Prometheus exporter, OpenTelemetry bridge, etc.)
  can collect them.
- **Surface (stable across `0.7.x`):**
  - `orderbook_rejects_total{reason="..."}` — counter, one
    increment per rejected order. Label value is the
    [`RejectReason`] [`Display`](std::fmt::Display) string.
  - `orderbook_depth_levels_bid` /
    `orderbook_depth_levels_ask` — gauges, current count of
    distinct price levels on each side. Updated on every
    structural mutation (add, cancel, modify, fill).
  - `orderbook_trades_total` — counter, monotonic count of
    every emitted trade transaction (one increment per
    `MatchResult` transaction).
- **Determinism preserved.** Metrics emission is out-of-band:
  no allocation on the happy path, no influence on matching
  outcomes, and `restore_from_snapshot_package` deliberately
  does **not** rehydrate counters — they are operational only
  and live for the process lifetime. The integration test
  `tests/metrics/` proves byte-identical snapshots between two
  books with metrics enabled.
- **Compile-time no-op.** When the feature is off every helper
  in [`orderbook::metrics`] compiles to an empty function so
  call-sites in the matching hot path stay unconditional.
- Example: `examples/src/bin/prometheus_export.rs` (run with
  `cargo run --features metrics --bin prometheus_export`)
  demonstrates installing the `metrics-exporter-prometheus`
  recorder and dumping the exposition payload.

#### v0.7.0 — Feature-gated binary wire protocol

- **New `wire` feature flag** behind which a small,
  length-prefixed binary protocol lives — every frame is
  `[len:u32 LE | kind:u8 | payload]`, `len` covers
  `kind + payload`, and all multi-byte integers are
  little-endian. Disabled by default; the existing JSON and
  bincode paths are unchanged. The protocol is additive.
- **`MessageKind`** — `#[repr(u8)]` enum with stable explicit
  discriminants. Inbound: `NewOrder = 0x01`,
  `CancelOrder = 0x02`, `CancelReplace = 0x03`,
  `MassCancel = 0x04`. Outbound: `ExecReport = 0x81`,
  `TradePrint = 0x82`, `BookUpdate = 0x83`.
- **Zero-copy inbound** — `NewOrderWire`, `CancelOrderWire`,
  `CancelReplaceWire`, `MassCancelWire` are
  `#[repr(C, packed)]` with `zerocopy::{FromBytes, IntoBytes,
  Unaligned, Immutable, KnownLayout}` derives. Each ships a
  `const _: () = assert!(size_of::<…>() == N)` guard. Decoding
  is safe — `zerocopy` performs the layout validation, no
  `unsafe` is required at any wire call site.
- **Byte-cursor outbound** — `ExecReport`, `TradePrintWire`,
  `BookUpdateWire` are encoded via explicit
  `extend_from_slice` calls. Outbound is I/O-dominated; this
  keeps the layout free to evolve.
- **`TryFrom<&NewOrderWire> for OrderType<()>`** — boundary
  mapping that copies each packed field into a stack local
  first (taking a reference to a packed field is UB), validates
  the side / TIF / order_type discriminants, and rejects
  negative prices via `WireError::InvalidPayload`.
- **`doc/wire-protocol.md`** with per-message layout tables,
  discriminant table, framing rule, and endianness statement.
- **Round-trip `proptest` coverage** in every
  `src/wire/{inbound,outbound}/*.rs` module.
- Example: `examples/src/bin/wire_roundtrip.rs`
  (`required-features = ["wire"]`).

#### v0.7.0 — HDR-histogram tail-latency bench suite

- **Six new `*_hdr` bench binaries** under
  `benches/order_book/`: `add_only`, `cancel_only`,
  `aggressive_walk`, `mixed_70_20_10`, `thin_book_sweep`,
  `mass_cancel_burst`. Each records per-sample nanosecond
  latencies into an `hdrhistogram::Histogram` and emits
  `p50` / `p99` / `p99.9` / `p99.99` + `min` / `max`. Coexists
  with the existing Criterion benches.
- **`make bench-hdr`** convenience target.
- **Headline numbers + methodology** in `BENCH.md` at the repo
  root, with a closed-loop disclosure block (the suite measures
  service time, not load-induced tail).

#### v0.7.0 — Closed `RejectReason` enum

- **New [`RejectReason`]** — closed
  `#[non_exhaustive] #[repr(u16)]` enum with stable explicit
  discriminants (1..13 + `Other(u16)`). The canonical wire-side
  reject taxonomy; consumers can route on the numeric code without
  parsing strings.
- **`OrderStatus::Rejected.reason: String`** is now
  `RejectReason` — typed, machine-routable, and stable across
  `0.7.x`. Breaking change on a public variant shape; allowed under
  the `0.6.x → 0.7.x` minor delta in `0.x` semver.
- **`impl From<&OrderBookError> for RejectReason`** — operational
  ergonomics. Exhaustive match — a future `OrderBookError` variant
  addition is caught at compile time.
- **Tracker emission on every reject path that already transitioned
  the tracker.** Kill switch, risk gates, and the three internal
  sites in `modifications.rs` (validation / post-only / missing
  user id) now record typed reasons. STP cancel-taker and IOC/FOK
  `InsufficientLiquidity` paths still return typed errors without
  transitioning the tracker — deferred to a follow-up.

#### v0.7.0 — Pre-trade risk layer

- **New [`RiskConfig`]** with three opt-in guard-rails:
  `max_open_orders_per_account`, `max_notional_per_account`, and
  `price_band_bps` against a configurable
  [`ReferencePriceSource`] (`LastTrade` / `Mid` / `FixedPrice`).
  Builder pattern — `RiskConfig::new().with_*(...)` chained.
- **Three new typed reject variants** on the existing
  `#[non_exhaustive]` `OrderBookError`:
  `RiskMaxOpenOrders`, `RiskMaxNotional`, `RiskPriceBand`. Each
  carries enough context (account, current, limit, deviation) for
  downstream consumers to act without parsing a string.
- **`OrderBook::set_risk_config(...)` / `risk_config()` /
  `disable_risk()`** — operator-driven gating. Check ordering on
  submit/add: `kill_switch → risk → STP → fees → match`. Market
  orders bypass the risk layer (no submitted price, no rest);
  kill switch still gates them.
- **Allocation-free** on the happy path. Per-account counters are
  `(AtomicU64, AtomicCell<u128>)` pairs; per-order risk state is a
  `DashMap<Id, RiskEntry>`.
- **`OrderBookSnapshotPackage.risk_config: Option<RiskConfig>`** —
  config persists across snapshot/restore. On restore, per-account
  counters and the per-order map are rebuilt by walking the
  snapshot's resting orders. Snapshot format version stays at `2`;
  the field is additive via `#[serde(default)]`.
- Example: `examples/src/bin/risk_limits.rs`.

#### v0.7.0 — Operational kill switch

- **New `OrderBook::engage_kill_switch()`,
  `OrderBook::release_kill_switch()`, and
  `OrderBook::is_kill_switch_engaged()`** — atomic operational halt
  for new flow. While engaged, every `submit_market_order*`,
  `add_order`, and non-`Cancel` `update_order` call returns the new
  [`OrderBookError::KillSwitchActive`] variant before any matching,
  fee, or STP work happens. Cancel and mass-cancel paths are
  explicitly **not** gated so operators can drain the resting book.
  The flag persists across snapshot/restore.
- **`OrderBookError::KillSwitchActive`** — new typed reject variant
  on the existing `#[non_exhaustive]` enum.
- **`OrderBookSnapshotPackage.kill_switch_engaged: bool`** —
  operational state persists across snapshot/restore. Snapshot
  format version stays at `2`; the field is additive via
  `#[serde(default)]`.
- When an `OrderStateTracker` is configured, kill-switched
  rejections are recorded as
  `OrderStatus::Rejected { reason: RejectReason::KillSwitchActive }`.
- Example: `examples/src/bin/kill_switch_drain.rs`.

#### v0.7.0 — Global `engine_seq` across outbound streams

- **New `OrderBook::next_engine_seq()` and `OrderBook::engine_seq()`**
  accessors backed by an `AtomicU64` counter. Every outbound emission
  (trade event, price-level change event) mints exactly one seq, in
  emission order, so external consumers can perform cross-stream gap
  detection and merge events from `TradeListener` and
  `PriceLevelChangedListener` into a single ordered view.
- **`engine_seq: u64` field** added to every outbound event type:
  `TradeResult`, `TradeEvent`, `PriceLevelChangedEvent`, and the NATS
  `BookChangeEntry`. JSON payloads are forward-compatible
  (`#[serde(default)]` falls back to `0` for v0.6.x payloads).
- **Snapshot format version bumped to `2`**.
  `OrderBookSnapshotPackage` carries `engine_seq` so that
  `restore_from_snapshot_package` resumes monotonicity exactly from the
  snapshotted point. `version: 1` packages are rejected by `validate()`.
- **`BookChangeBatch.sequence`** retains its existing per-batch
  publisher-counter semantics; cross-stream gap detection moves to the
  per-event `BookChangeEntry.engine_seq`. Both fields ship in the same
  payload for incremental adoption.

#### v0.7.0 — `Clock` trait for deterministic replay

- **New [`Clock`] trait** with two implementations, [`MonotonicClock`]
  (production, wraps `SystemTime::now`) and [`StubClock`] (replay /
  tests, monotonic `AtomicU64` counter with configurable start and
  step). Re-exported at the crate root and via [`prelude`].
- **[`OrderBook::with_clock`]** constructor plus `set_clock` and
  `clock()` accessors. The default [`OrderBook::new`] keeps wrapping
  [`MonotonicClock`] internally — existing callers observe no
  behavioural change.
- **`ReplayEngine::replay_from_with_clock`** for byte-identical
  replay tests and disaster-recovery pipelines that must reproduce
  engine timestamps deterministically.
- Wall-clock reads are no longer present inside the matching core —
  every stamp flows through `self.clock().now_millis()`.
- **Behavioural change (same type signature)**:
  `OrderStateTracker::get_history` and `OrderBook::get_order_history`
  now return `Vec<(u64 /* milliseconds */, OrderStatus)>` instead of
  nanoseconds; the `Clock::now_millis` unit is the only one the trait
  exposes.

#### v0.6.2 — Dependency Bumps & Bincode 2.x Migration

- **Dependency refresh**: `uuid` 1.23, `tokio` 1.52, `sha2` 0.11,
  `async-nats` 0.47, `bincode` 2.0 (crates.io `bincode 3.0.0` is a
  `compile_error!` stub, so `2.0` is the current usable major).
- **Bincode API migration** (feature `bincode`): the
  `BincodeEventSerializer` now uses `bincode::serde::encode_to_vec`
  / `decode_from_slice` with `bincode::config::standard()`. The
  public trait and type surface are unchanged.
- **Wire-format note**: bincode 1.x and 2.x produce different byte
  layouts on the NATS transport path. The on-disk journal uses
  `serde_json` and is unaffected (`ORDERBOOK_SNAPSHOT_FORMAT_VERSION`
  stays at `1`).

#### v0.6.1 — NATS Integration, Sequencer & Order State

- **NATS JetStream Publishers**: Trade event and book change publishers with retry, batching, and throttling (`nats` feature)
- **Zero-Copy Serialization**: Pluggable `EventSerializer` trait with JSON and Bincode implementations (`bincode` feature)
- **Sequencer Subsystem**: `SequencerCommand`, `SequencerEvent`, `SequencerResult` types for LMAX Disruptor-style total ordering
- **Append-Only Journal**: `FileJournal` with memory-mapped segments, CRC32 checksums, and segment rotation (`journal` feature)
- **In-Memory Journal**: `InMemoryJournal` for testing and benchmarking
- **Deterministic Replay**: `ReplayEngine` for disaster recovery and state verification from journal
- **Order State Machine**: `OrderStatus`, `CancelReason`, `OrderStateTracker` for explicit lifecycle tracking (Open → PartiallyFilled → Filled / Cancelled / Rejected)
- **Order Lifecycle Query API**: `get_order_history()`, `active_order_count()`, `terminal_order_count()`, `purge_terminal_states()`
- **Upgrade to pricelevel v0.7**: `Id`, `Price`, `Quantity`, `TimestampMs` newtypes for stronger type safety

#### v0.5.x — Validation, STP, Fees & Mass Cancel

- **Order Validation**: Tick size, lot size, and min/max order size validation with configurable limits
- **Self-Trade Prevention (STP)**: `CancelTaker`, `CancelMaker`, `CancelBoth` modes with per-order `user_id` enforcement
- **Fee Model**: Configurable `FeeSchedule` with maker/taker fees, fee fields in `TradeResult`
- **Mass Cancel Operations**: Cancel all, by side, by user, by price range — with `MassCancelResult` tracking
- **Cross-Book Mass Cancel**: `cancel_all_across_books()`, `cancel_by_user_across_books()`, `cancel_by_side_across_books()` on `BookManager`
- **Snapshot Config Preservation**: `restore_from_snapshot_package()` preserves fee schedule, STP mode, tick/lot size, and order size limits

#### v0.4.8 — Performance & Architecture

- **Performance Boost**: `PriceLevelCache` for faster best bid/ask lookups, `MatchingPool` to reduce matching engine allocations
- **Cleaner Architecture**: Refactored modification and matching logic for better separation of concerns
- **Enhanced Concurrency**: Improved thread-safe operations under heavy load

### Status
This project is in active development. The core matching engine, order validation, STP, fees, mass cancel, NATS integration, sequencer journal, and order state tracking are production-ready. The Sequencer runtime (async event loop) is under development.

### Advanced Features

#### Market Metrics & Analysis

The order book provides comprehensive market analysis capabilities:

- **VWAP Calculation**: Volume-Weighted Average Price for analyzing true market price
- **Spread Analysis**: Absolute and basis point spread calculations
- **Micro Price**: Fair price estimation incorporating depth
- **Order Book Imbalance**: Buy/sell pressure indicators
- **Market Impact Simulation**: Pre-trade analysis for estimating slippage and execution costs
- **Depth Analysis**: Cumulative depth and liquidity distribution

#### Intelligent Order Placement

Advanced utilities for market makers and algorithmic traders:

- **Queue Analysis**: `queue_ahead_at_price()` - Check depth at specific price levels
- **Tick-Based Pricing**: `price_n_ticks_inside()` - Calculate prices N ticks from best bid/ask
- **Position Targeting**: `price_for_queue_position()` - Find prices for target queue positions
- **Depth-Based Strategy**: `price_at_depth_adjusted()` - Optimal prices based on cumulative depth

#### Functional Iterators

Memory-efficient, composable iterators for order book analysis:

- **Cumulative Depth Iteration**: `levels_with_cumulative_depth()` - Lazy iteration with running depth totals
- **Depth-Limited Iteration**: `levels_until_depth()` - Auto-stop when target depth is reached
- **Range-Based Iteration**: `levels_in_range()` - Filter levels by price range
- **Predicate Search**: `find_level()` - Find first level matching custom conditions

**Benefits:**
- Zero allocation - O(1) memory vs O(N) for vectors
- Lazy evaluation - compute only what's needed
- Composable - works with standard iterator combinators (`.map()`, `.filter()`, `.take()`)
- Short-circuit - stops early when conditions are met

#### Multi-Book Management

Centralized trade event routing and multi-book orchestration:

- **BookManager**: Manage multiple order books with unified trade listener
- **Standard & Tokio Support**: Synchronous and async variants
- **Event Routing**: Centralized trade notifications across all books

#### Aggregate Statistics

Comprehensive statistical analysis for market condition detection:

- **Depth Statistics**: `depth_statistics()` - Volume, average sizes, weighted prices, std dev
- **Market Pressure**: `buy_sell_pressure()` - Total volume on each side
- **Liquidity Health**: `is_thin_book()` - Detect insufficient liquidity
- **Distribution Analysis**: `depth_distribution()` - Histogram of liquidity concentration
- **Imbalance Detection**: `order_book_imbalance()` - Buy/sell pressure ratio (-1.0 to 1.0)

**Use cases:**
- Market condition detection and trend identification
- Risk management and liquidity monitoring
- Strategy adaptation based on real-time conditions
- Trading decision support and analytics

#### Enriched Snapshots

Pre-calculated metrics in snapshots for high-frequency trading:

- **Enriched Snapshots**: `enriched_snapshot()` - Single-pass snapshot with all metrics
- **Custom Metrics**: `enriched_snapshot_with_metrics()` - Select specific metrics for optimization
- **Metric Flags**: Bitflags for precise control over calculated metrics

**Metrics included:**
- Mid price and spread (in basis points)
- Total depth on each side
- VWAP for top N levels
- Order book imbalance

**Benefits:**
- Single pass through data vs multiple passes
- Better cache locality and performance
- Reduced computational overhead
- Flexibility with optional metric selection

## Performance Analysis of the OrderBook System

This analyzes the performance of the OrderBook system based on tests conducted on an Apple M4 Max processor. The data comes from a High-Frequency Trading (HFT) simulation and price level distribution performance tests. The figures below are representative single-run numbers measured on **orderbook-rs 0.9.0** with the bundled examples `orderbook_hft_simulation` and `orderbook_contention_test` (`cargo run --release -p examples --bin <name>`); absolute throughput is workload-, machine-, and run-dependent.

### 1. High-Frequency Trading (HFT) Simulation

#### Test Configuration
- **Symbol:** BTC/USD
- **Duration:** 5000 ms (5 seconds)
- **Threads:** 30 threads total
  - 10 maker threads (order creators)
  - 10 taker threads (order executors)
  - 10 canceller threads (order cancellers)
- **Initial orders:** 1020 pre-loaded orders

#### Performance Results

| Metric | Total Operations | Operations/Second |
|---------|---------------------|---------------------|
| Orders Added | 465,314 | 93,040.99 |
| Orders Matched | 191,555 | 38,302.02 |
| Orders Cancelled | 183,700 | 36,731.39 |
| **Total Operations** | **840,569** | **168,074.41** |

#### Initial vs. Final OrderBook State

| Metric | Initial State | Final State |
|---------|----------------|---------------|
| Best Bid | 9,900 | 9,840 |
| Best Ask | 10,000 | 10,010 |
| Spread | 100 | 170 |
| Mid Price | 9,950.00 | 9,925.00 |
| Total Orders | 1,020 | 44,987 |
| Bid Price Levels | 21 | 11 |
| Ask Price Levels | 21 | 12 |
| Total Bid Quantity | 7,750 | 346,031 |
| Total Ask Quantity | 7,750 | 473,092 |

### 2. Contention Pattern Performance Tests

#### Configuration
- **Threads:** 12
- **Test Duration:** 3000 ms per sub-test
- **Concurrent Operations:** Multi-threaded lock-free architecture

#### Read / Write Operation Ratio

Mixed read/write workload over 500 resting orders across 40 price levels;
the `Read %` is the fraction of operations that are read-only (snapshot /
best-price / depth queries) versus mutating (add / cancel / match).

| Read % | Operations/Second |
|------------|---------------------|
| 0%         | 305,435.88          |
| 25%        | 63,103.90           |
| 50%        | 51,933.72           |
| 75%        | 54,960.34           |
| 95%        | 100,379.54          |

#### Price Level Distribution

Throughput as the resting depth is spread across a varying number of price
levels (100 orders per level, except the 5- and 1-level cases which pack the
same orders into fewer levels).

| Price Levels | Operations/Second |
|--------------|---------------------|
| 100          | 184,986.35          |
| 50           | 188,085.24          |
| 10           | 70,338.52           |
| 5            | 68,610.25           |
| 1            | 61,302.26           |

#### Hot Spot Contention Test

All threads hammer a single shared price level (20 hot-spot orders + 480
regular); higher hot-spot percentages concentrate more operations on that one
lock-free level, where the `crossbeam-skiplist` + `dashmap` + atomics design
shines.

| % Operations on Hot Spot | Operations/Second   |
|--------------------------|---------------------|
| 0%                       | 14,978,484.36       |
| 25%                      | 19,191,927.99       |
| 50%                      | 25,890,620.87       |
| 75%                      | 31,529,898.64       |
| 100%                     | 31,607,744.24       |

#### Performance Improvements and Deadlock Resolution

The significant performance gains, especially in the "Hot Spot Contention Test," and the resolution of the previous deadlocks are a direct result of refactoring the internal concurrency model of the `PriceLevel`.

- **Previous Bottleneck:** The original implementation relied on a `crossbeam::queue::SegQueue` for storing orders. While the queue itself is lock-free, operations like finding or removing a specific order required draining the entire queue into a temporary list, performing the action, and then pushing all elements back. This process was inefficient and created a major point of contention, leading to deadlocks under heavy multi-threaded load.

- **New Implementation:** The `OrderQueue` was re-designed to use a combination of:
  1. A `dashmap::DashMap` for storing orders, allowing for highly concurrent, O(1) average-case time complexity for insertions, lookups, and removals by `Id`.
  2. A sequence-keyed index (a `crossbeam_skiplist::SkipMap<sequence, Id>`) that maintains the crucial First-In-First-Out (FIFO) order for matching while still allowing O(log n) ordered iteration and deterministic snapshots.

This hybrid approach eliminates the previous bottleneck, allowing threads to operate on the order collection with minimal contention, which is reflected in the massive throughput increase in the hot spot tests.

### 3. Analysis and Conclusions

#### Overall Performance
The system demonstrates excellent capability to handle over **165,000 operations per second** in the high-frequency trading simulation, distributed across order creations, matches, and cancellations.

#### Price Level Distribution Behavior
- **Optimal Performance Range:** The system performs best with 50-100 price levels, achieving roughly 185,000-188,000 operations per second.
- **Performance Degradation:** Performance decreases with fewer price levels (more per-level contention), dropping to around 61,000-70,000 operations per second with 1-10 levels.
- **Scalability:** The lock-free architecture demonstrates excellent scalability characteristics across different price level distributions.

#### Hot Spot Contention
- Surprisingly, performance **increases** as more operations concentrate on a hot spot, reaching its maximum with 100% concentration (31,607,744 ops/s).
- This counter-intuitive behavior might indicate:
  1. Very efficient cache effects when operations are concentrated in one memory area
  2. Internal optimizations to handle high-contention cases
  3. Benefits of the system's lock-free architecture

#### OrderBook State Behavior
- During the HFT simulation, the order book handled a significant increase in order volume (from 1,020 to 44,987).
- The spread increased from 100 to 170, reflecting realistic market behavior under pressure.
- The final state shows substantial liquidity with over 346,000 bid quantity and 473,000 ask quantity.

### 4. Practical Implications

- The system is suitable for high-frequency trading environments with the capacity to process over 165,000 mixed operations per second (and tens of millions of operations per second on a single hot price level).
- The lock-free architecture proves to be extremely effective at handling contention, especially at hot spots.
- Optimal performance is achieved with moderate price level distribution (50-100 levels).
- For real-world use cases, the system demonstrates excellent scalability and maintains performance under concurrent load.

This analysis confirms that the system design is highly scalable and appropriate for demanding financial applications requiring high-speed processing with data consistency.


## 🛠 Makefile Commands

This project includes a `Makefile` with common tasks to simplify development. Here's a list of useful commands:

### 🔧 Build & Run

```sh
make build         # Compile the project
make release       # Build in release mode
make run           # Run the main binary
```

### 🧪 Test & Quality

```sh
make test          # Run all tests
make fmt           # Format code
make fmt-check     # Check formatting without applying
make lint          # Run clippy with warnings as errors
make lint-fix      # Auto-fix lint issues
make fix           # Auto-fix Rust compiler suggestions
make check         # Run fmt-check + lint + test
```

### 📦 Packaging & Docs

```sh
make doc           # Check for missing docs via clippy
make doc-open      # Build and open Rust documentation
make create-doc    # Generate internal docs
make readme        # Regenerate README using cargo-readme
make publish       # Prepare and publish crate to crates.io
```

### 📈 Coverage & Benchmarks

```sh
make coverage            # Generate code coverage report (XML)
make coverage-html       # Generate HTML coverage report
make open-coverage       # Open HTML report
make bench               # Run benchmarks using Criterion
make bench-show          # Open benchmark report
make bench-save          # Save benchmark history snapshot
make bench-compare       # Compare benchmark runs
make bench-json          # Output benchmarks in JSON
make bench-clean         # Remove benchmark data
```

### 🧪 Git & Workflow Helpers

```sh
make git-log             # Show commits on current branch vs main
make check-spanish       # Check for Spanish words in code
make zip                 # Create zip without target/ and temp files
make tree                # Visualize project tree (excludes common clutter)
```

### 🤖 GitHub Actions (via act)

```sh
make workflow-build      # Simulate build workflow
make workflow-lint       # Simulate lint workflow
make workflow-test       # Simulate test workflow
make workflow-coverage   # Simulate coverage workflow
make workflow            # Run all workflows
```

ℹ️ Requires act for local workflow simulation and cargo-tarpaulin for coverage.

## Contribution and Contact

We welcome contributions to this project! If you would like to contribute, please follow these steps:

1. Fork the repository.
2. Create a new branch for your feature or bug fix.
3. Make your changes and ensure that the project still builds and all tests pass.
4. Commit your changes and push your branch to your forked repository.
5. Submit a pull request to the main repository.

If you have any questions, issues, or would like to provide feedback, please feel free to contact the project
maintainer:

### **Contact Information**
- **Author**: Joaquín Béjar García
- **Email**: jb@taunais.com
- **Telegram**: [@joaquin_bejar](https://t.me/joaquin_bejar)
- **Repository**: <https://github.com/joaquinbejar/OrderBook-rs>
- **Documentation**: <https://docs.rs/orderbook-rs>


We appreciate your interest and look forward to your contributions!

**License**: MIT

<!-- related-projects:start -->
## Related projects

Repositories by the same author that this project depends on, and repositories that depend on it.

### Depends on

| Repository | Description |
|------------|-------------|
| [PriceLevel](https://github.com/joaquinbejar/PriceLevel) · [crates.io](https://crates.io/crates/pricelevel) | Lock-free price level implementation for limit order books. |

### Used by

| Repository | Description |
|------------|-------------|
| [hydra-amm](https://github.com/joaquinbejar/hydra-amm) · [crates.io](https://crates.io/crates/hydra-amm) | Universal AMM engine: build, configure and operate any Automated Market Maker through one interface. |
| [market-maker-rs](https://github.com/joaquinbejar/market-maker-rs) | Quantitative market making strategies, starting with the Avellaneda-Stoikov model. |
| [Option-Chain-OrderBook](https://github.com/joaquinbejar/Option-Chain-OrderBook) · [crates.io](https://crates.io/crates/option-chain-orderbook) | Option chain order book system (underlying, expiration, strike) built on OrderBook-rs, PriceLevel and OptionStratLib. |
| [Option-Chain-OrderBook-Backend](https://github.com/joaquinbejar/Option-Chain-OrderBook-Backend) | REST and WebSocket backend service exposing Option-Chain-OrderBook. |

<!-- related-projects:end -->
