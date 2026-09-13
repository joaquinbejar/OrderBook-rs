# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **`OrderBookError::ReserveResidualWouldBeDiscarded` and
  `OrderBookError::ZeroVisibleTranche` (#230).** Two new typed rejections on
  the reserve admission and modify paths; see the `Fixed` entries below
  for when each is raised. `ReserveResidualWouldBeDiscarded` carries both
  the projected `hidden_quantity` and the `discarded_quantity` that would
  actually be destroyed. `OrderBookError` is `#[non_exhaustive]`, so
  downstream matches keep compiling.
- **`RejectReason::ReserveResidualWouldBeDiscarded`, wire code 14 (#230).**
  Additive: `RejectReason` is `#[non_exhaustive]`, serializes as its `u16`
  code, and `RejectReason::from_u16` maps unknown codes to `Other`, so an
  older reader decodes the new code as `RejectReason::Other(14)`.
  `ZeroVisibleTranche` reuses the existing `RejectReason::InvalidQuantity`
  (code 8) and mints no new code.
- **`orderbook_reserve_discards_total` and
  `orderbook_reserve_hidden_discarded_total` (#230, `metrics` feature).**
  Counters for reserve residuals discarded because their visible tranche was
  exhausted without automatic replenishment: the first counts orders, the
  second sums the hidden quantity dropped. Both are fed from the aggressive
  taker path and from the maker removal, which also emit a matching `INFO`
  trace carrying a `path` field (`"taker"` / `"maker"`), the order id, the
  executed quantity and the discarded hidden quantity.

- **`SequencerResult::RejectedWithCode { reason, code, may_have_mutated,
  stp_mode }`** — a rejection carrying its stable wire-side `RejectReason`
  next to the message, plus the two facts the code cannot express:
  `may_have_mutated`, set for the errors the engine can return after it has
  already changed the book, and `stp_mode`, the source book's `STPMode` for
  a rejection the self-trade-prevention scan produced. `impl
  From<&OrderBookError> for SequencerResult` fills all four from the typed
  error in one step and is the intended way to build the variant. Appended
  variant on the `#[non_exhaustive]` enum: existing journals decode
  unchanged; journals carrying it fail to decode against older binaries,
  matching the `MarketOrderByAmount` precedent. The code encodes as its
  `u16` wire value. The variant governs the sequencer event stream, not
  the snapshot package, so `ORDERBOOK_SNAPSHOT_FORMAT_VERSION` is
  unchanged.
- **`ReplayError::OutcomeMismatch { sequence_num, recorded, actual }`** —
  raised when a re-executed rejected submit succeeds or fails under a
  different code than the journal recorded.
- **`ReplayError::StpModeMismatch { sequence_num, recorded, actual }`** —
  raised when a journaled self-trade-prevention rejection records a
  different `STPMode` than the replay book is configured with.

### Changed (breaking, semver-minor under 0.x)

- **`OrderBook::get_bids` and `OrderBook::get_asks` are removed (#228).**
  Both returned `Arc<DashMap<u128, Arc<PriceLevel>>>` holding clones of the
  book's **live** level handles, and `PriceLevel` exposes `add_order`,
  `update_order` and `match_order` publicly, so any caller could mutate a
  price level directly — bypassing the submit gate, the `order_locations`
  and user-order indices, the risk state, self-trade prevention, the kill
  switch, the order-state tracker and the trade / book-change listeners.
  The resulting book was silently inconsistent: orders unreachable by id,
  depth gauges and caches stale, no events emitted. Deprecating the two
  accessors would have left the bypass reachable for another release, so
  they are removed outright. **0.13.0 is the release boundary for breaking
  changes.**

  Migration — every replacement is read-only and returns values, not
  handles:

  | Removed usage | Use instead |
  | --- | --- |
  | Enumerate levels and their orders | `create_snapshot(depth)` |
  | Walk levels with running depth | `levels_with_cumulative_depth`, `levels_until_depth` |
  | Levels within a price band | `levels_in_range` |
  | One level by price | `find_level` (`LevelInfo`), `order_count_at_price` |
  | Orders at a price / in the book | `get_orders_at_price`, `get_all_orders` |
  | Aggregate depth over N levels | `total_depth_at_levels` |
  | Top of book | `best_bid`, `best_ask` |

  Mutation has no replacement by design: it must go through `add_order`,
  `update_order`, `cancel_order` or the mass-cancel entry points, which is
  the point of the removal. No snapshot or journal format change, and
  `ORDERBOOK_SNAPSHOT_FORMAT_VERSION` is unchanged.

  Following from the same removal, the `new` constructors of the level
  iterators (`LevelsWithCumulativeDepth`, `LevelsUntilDepth`,
  `LevelsInRange`) are now crate-private: each takes a reference to the
  book's live `SkipMap` of price levels, and with `get_bids` / `get_asks`
  gone no public API yields one. The iterator types themselves stay public
  — obtain them from `OrderBook::levels_with_cumulative_depth`,
  `levels_until_depth` and `levels_in_range`, which is how every caller in
  this repository already did.

- **`ReplayError` gained the `OutcomeMismatch` and `StpModeMismatch`
  variants (#224)**, so exhaustive matches need new arms; 0.13.0 is the
  release boundary for them together with the #228 removal above, as
  `NamespaceRequiresFullReplay` shipped under 0.11.0. Journals carrying
  `SequencerResult::RejectedWithCode` fail to decode against older binaries
  (existing journals decode unchanged); no snapshot format change and no
  `ORDERBOOK_SNAPSHOT_FORMAT_VERSION` bump.

### Fixed

- **`STPMode::CancelTaker` and `STPMode::CancelBoth` fire only on a
  same-user maker the taker can reach (#222).** Scope: those two modes.
  `STPMode::CancelMaker` is deliberately unchanged and still cancels every
  same-user order at a level the sweep touches, reachable or not — it never
  destroys the taker, so the gate would only change which resting orders
  survive. `check_stp_at_level` reports a conflict whenever a
  same-user maker rests at a crossed level, and the `CancelTaker` /
  `CancelBoth` arms then cancelled the taker (and, under `CancelBoth`, the
  maker) unconditionally after their safe-quantity pre-match. A taker the
  non-self depth ahead of that maker already satisfied therefore returned
  `SelfTradePrevented` with `Cancelled { filled_quantity: n }` — a client
  that retried double-filled — and `CancelBoth` destroyed a maker the
  sweep never touched; market takers reach the same arms through
  `match_order_with_user`, which drops the taker flag, so the caller saw
  `Ok` while the maker was gone. The arms now cancel only when the sweep
  can still execute into the same-user maker, with two guards: a budget
  the pre-match exhausted (`is_done()`) is an ordinary complete fill, and
  quote-notional dust — a residual that cannot fund one more lot at the
  level's price, the usual end of a notional sweep since the budget rarely
  lands on exactly zero, and the lot-rounded case — leaves the maker
  untouched and walks on to the next level instead of breaking, because a
  quote-amount sell can still afford a whole lot at a cheaper bid further
  down. A base-quantity residual keeps the STP verdict whatever its size:
  a maker admitted before a lot-size change keeps resting with a
  misaligned tranche (documented on `set_lot_size`), so a sub-lot
  residual is reachable, and walked past it would rest crossed against
  the taker's own maker. The modify precheck
  `check_modify_stp_self_cross` (#168) rejected a reprice as soon as any
  same-user order rested at a crossed level, before subtracting the
  non-self depth queued ahead of it; it now mirrors the sweep's
  lot-rounded per-level cap and consults the same insertion-sequence
  `check_stp_at_level` verdict, so a reprice the non-self depth covers is
  admitted, one it does not cover is still refused before the original is
  cancelled, and dust the sweep stops on before a deeper same-user level
  is admitted exactly as a direct submit is. The pre-match is sized by
  `PriceLevel::matchable_quantity` — the authoritative dry run the
  no-conflict arm already used — bounded by the non-self prefix, not by
  `safe_quantity` itself. That sum counts *visible* quantity and can
  overstate what the sweep executes (a no-progress maker is set aside; a
  replenish whose checked net delta would overflow the level's visible
  counter aborts the sweep untouched, #124), and overstating it admitted a
  reprice the sweep then killed after the original was already cancelled —
  the destruction #168 exists to prevent. Pinned for base-quantity,
  fill-or-kill and market takers, quote-amount buys (dust and reachable),
  a quote-amount sell past an unaffordable self level, a lot-rounded
  notional residual, a sub-lot reserve tranche resting from before a
  lot-size change (sweep and reprice),
  precheck/sweep agreement on dust before a deeper self level, the
  exact-depth boundary, a maker ahead that is counted in `safe_quantity`
  yet delivers nothing (the #124 replenish-headroom abort, where the
  reprice is now refused with the original left resting), and same-level
  reprices (admitted and refused); a
  reachable maker still yields `SelfTradePrevented` with the true non-self
  fill. The notional-sell walk is pinned on a plain STP-off book too, since
  the walk direction and not the STP mode decides the zero-cap verdict.

  The same walk had a second, older bug the reachability guards exposed:
  a zero per-level cap ended the whole sweep, which is right for a
  base-quantity budget (the cap is the lot-rounded residual and ignores
  the level price) and for a quote-notional buy (asks ascend, so
  `remaining / price` only shrinks), but wrong for a quote-notional sell,
  whose walk descends the bids so that a budget too small here can still
  fund a whole lot lower down. A sell of 150 into bids 100, 75 and 50
  executed one unit instead of two. The direction-aware decision now
  lives in `StopCondition::zero_cap_is_terminal`, and the walk skips the
  level instead of breaking when a notional sell can still reach cheaper
  bids. Its exact terminal condition is `remaining < lot`: the cheapest a
  level can be is a price of 1, where the cap is the whole remaining
  notional, so below one lot no level still ahead can execute. With no lot
  size configured that reduces to a spent budget, which the loop's own
  completion check reaches first, so a notional sell then ends only on a
  spent budget or an exhausted side and visits every level on that side.
  Each skipped level costs one `u128` division and mutates nothing.

- **A reserve residual follows `auto_replenish` (#230).**
  `reduce_reserve_to_total` — the residual-resting helper behind
  `OrderQuantity::set_total_remaining`, which `add_order` uses to distribute
  an aggressive taker's unmatched total across its tranches — refreshed an
  emptied visible tranche with
  `replenish_amount.map(get).unwrap_or(0).min(hidden)`, ignoring
  `auto_replenish` entirely. That diverged from `pricelevel`'s contract for
  a resting maker in two ways. With `auto_replenish = false` upstream
  removes a maker whose visible tranche is depleted and strands its hidden
  depth, while the helper refreshed the taker's residual from an explicit
  amount and rested it. With `replenish_amount = None` and
  `auto_replenish = true` upstream refreshes with
  `DEFAULT_RESERVE_REPLENISH_AMOUNT` (80) capped by hidden, while the helper
  refreshed **zero** and rested an order with no visible tranche at all —
  `PriceLevel::add_order` does not reject one, so the book displayed nothing
  for an order that could never refill.

  A third divergence sat in the same branch: the helper only ever looked at
  an *emptied* visible tranche, ignoring `replenish_threshold` entirely,
  while upstream also refreshes a **partially** consumed tranche that falls
  below `safe_threshold = max(replenish_threshold, 1)`. A
  `{10 visible, 20 hidden, threshold 5, replenish_amount 10}` reserve filled
  for 8 rested 2 / 20 as a residual while the identical resting maker would
  have shown 12 / 10.

  `auto_replenish` now governs both paths identically, and the two upstream
  arms collapse into one rule applied here: with the flag on and hidden
  left, a post-reduction visible tranche below `safe_threshold` grows by
  `min(replenish_amount.unwrap_or(DEFAULT_RESERVE_REPLENISH_AMOUNT), hidden)`,
  drawn out of hidden. An emptied tranche is just that rule at its smallest,
  since `safe_threshold >= 1`.
  With the flag off the visible tranche is left empty, nothing is drawn from
  hidden, and a new scoped guard in `add_order_inner` — matching only a
  `ReserveOrder { auto_replenish: false, .. }` whose visible tranche the
  sweep exhausted while hidden remains — ends the order instead of resting
  it, with the same bookkeeping as the fully-matched branch
  (`OrderStatus::Filled { filled_quantity }` carrying the executed quantity)
  plus an `INFO` trace naming the order, the executed quantity and the
  discarded hidden quantity, and, under the `metrics` feature, the new
  `orderbook_reserve_discards_total` (orders) and
  `orderbook_reserve_hidden_discarded_total` (quantity units) counters, both
  fed from the aggressive taker path and from the maker removal so either
  side of the trade reports the same loss. Reporting the maker side costs a
  pre-match pass over a level's resting orders, and `PriceLevel::iter_orders`
  is a `DashMap` iterator that read-locks **every shard** of the map per
  level match; the book therefore records whether it has ever rested a
  `ReserveOrder { auto_replenish: false, .. }` with hidden depth (a
  monotonic flag, set on admission and re-derived on snapshot restore) and
  skips the pass entirely otherwise. The flag is read once per sweep, so a
  book that never rested one pays a single relaxed atomic load and allocates
  no capture buffer. A book that did pays the full pass on every level
  holding hidden depth; that pass is **not** free and shows up in the tails
  of a reserve-maker sweep, while the common-path benchmarks
  (`aggressive_walk_hdr`, `thin_book_sweep_hdr`) are unaffected. Both arms
  are covered by the `reserve_sweep_hdr` benchmark — see `BENCH.md` for the
  figures.

  That once-per-sweep read is only coherent because of a new gate rule: in a
  book that **holds** strandable makers, **every sweep takes the exclusive
  side of the submit gate, in every `STPMode`** — matching-capable submits,
  cancel-then-add re-prices and the match-only entry points (`match_order`,
  `match_order_with_user`, `match_market_order_by_amount*`) alike —
  alongside fill-or-kill (#209) and the STP-relevant submits (#225).
  Admitting a strandable maker is exclusive too, which covers the very first
  one, when the count is still zero. Post-only submits, `UpdateQuantity`,
  cancels and mass cancels keep the shared side and never read the count;
  they are excluded from a sweep's window by that sweep's hold, not by
  taking the exclusive side themselves. The invariant:

  > Every **sweep** in a book holding a strandable maker runs exclusively,
  > so no cancel, mass cancel, admission or re-price can land inside its
  > capture window.

  Two interleavings needed it. A maker admitted at a level a sweep had not
  reached yet would be consumed with no report, because the sweep had
  already decided not to capture. And a maker the sweep *had* captured could
  be cancelled and its id reused by an unrelated `Standard` order at the
  same level, so the sweep would fill the impostor and report the reserve's
  hidden quantity as discarded when nothing was stranded.

  The gate mode is decided **before** anything is read, from the count and
  the STP mode, never from a lookup that could go stale before acquisition;
  a caller that takes the shared side re-reads the count and, if it grew,
  drops the guard and restarts the whole operation exclusively (a fresh
  acquisition, not a lock upgrade, which `std::sync::RwLock` cannot do). The
  count can only increase under the exclusive side, so a shared holder that
  saw zero knows it stays zero. Cost: books holding strandable makers
  serialize their sweeps exactly as STP books have since #225; books holding
  none pay one relaxed load and are otherwise unchanged. The count is exact
  rather than conservative: increments sit at the only two places an order
  is rested, decrements at the only three places such a maker leaves a
  level and each decides from the removed order's own body, and the fill
  drain's attribution cannot be stale because of the rule above.

  The
  returned `Arc<OrderType>` on that branch now carries **both tranches at
  zero** rather than the stale hidden remainder, so `total_quantity()` is
  `0` for an order that rests nowhere; `add_order`'s docs state what each
  of the fully-matched, rested and discarded branches returns. The guard is deliberately narrow: an iceberg
  residual always keeps `min(display, remaining) > 0` visible, an
  auto-replenishing reserve was refreshed, and a reserve that did not trade
  rests as submitted — none of them reach it. The accounting rule is
  `submitted = executed + resting (visible + hidden) + discarded`, and
  discarded quantity is **never** counted as executed.

  Because the three cancel-then-add modify variants (`UpdatePrice`,
  `UpdatePriceAndQuantity`, `Replace`) re-add the order as a taker, that
  guard would otherwise let a re-price destroy the order it modifies: the
  original is cancelled, the re-add exhausts the visible tranche, the guard
  discards the hidden remainder, and `update_order` still returns
  `Ok(Some(..))`. A new validate-first pre-check beside the #168 STP
  self-cross check closes it. When the projected order is a `ReserveOrder`
  with `auto_replenish == false` and a non-empty hidden tranche, the engine
  dry-runs the crossable depth at the projected price with the same
  lot-size- and STP-aware feasibility walk fill-or-kill uses; if that depth
  is at least the projected visible tranche **and** less than the projected
  total, the modify is rejected with the new
  `OrderBookError::ReserveResidualWouldBeDiscarded { order_id,
  visible_quantity, crossable_quantity, hidden_quantity,
  discarded_quantity }` before anything
  is cancelled, so the original keeps resting untouched. `hidden_quantity`
  is the tranche as it would be re-added; `discarded_quantity` is what would
  actually be destroyed (`visible + hidden - crossable`), so a 10 / 20
  reserve crossing 15 reports hidden 20 and discarded 15. Crossing into depth
  smaller than the visible tranche is allowed (the residual rests with a
  positive visible tranche), and so is a projected **full** fill (it
  executes everything and discards nothing) and a non-crossing re-price.
  Auto-replenishing reserves,
  icebergs, single-tranche kinds and non-crossing re-prices never reach the
  walk. The dry run is **exact**, not best-effort: whenever the check can
  fire, the order being modified is itself a strandable maker, so the book's
  count is at least one and the whole cancel-then-add — lookup, validation,
  cancel and re-add — already runs on the exclusive side (see the gate note
  below). The error maps to the new wire code
  `RejectReason::ReserveResidualWouldBeDiscarded = 14`; older deserializers
  carry it forward as `RejectReason::Other(14)`.

  Unchanged and documented rather than fixed: an aggressive two-tranche
  order sweeps with its **total** quantity (`add_order_inner` passes
  `total_quantity()` to matching), so a 10 visible / 20 hidden reserve
  submitted into 20 units of contra liquidity executes 20, whereas the
  identical order resting as a maker without automatic replenishment
  executes only its 10 visible units before `pricelevel` removes it and
  discards the 20 hidden. This release only makes the residual's fate follow
  `auto_replenish` the way the maker's already did.

  Compatibility: `OrderBookError` gains
  `ReserveResidualWouldBeDiscarded` and `RejectReason` gains the matching
  variant; both enums are `#[non_exhaustive]`, so downstream matches keep
  compiling, and `RejectReason::from_u16` already carries unknown codes
  forward through `Other`. No snapshot or journal format change, and
  `ORDERBOOK_SNAPSHOT_FORMAT_VERSION` is unchanged. Observable behaviour
  changes on two counts. A non-auto-replenishing reserve taker whose visible
  tranche is exhausted now ends instead of resting: a 10 / 20 reserve with
  `replenish_amount = Some(10)` filled for 10 used to rest 10 / 10 and now
  ends as `Filled { filled_quantity: 10 }`. And an auto-replenishing
  residual left below its threshold now refreshes where it previously did
  not: the threshold-5 example above rests 12 / 10 instead of 2 / 20. A
  journal recorded before this change that contains either submit replays to
  the new outcome, so the replayed book legitimately differs from the one
  the original run produced and `ReplayEngine::verify` on it may return
  `Ok(false)`. The #226 lot-size transfer check below is narrowed to match
  this policy: relative to 0.12.x admission is tighter exactly as the #226
  entry describes and no further, and the narrowing removes rejections
  #226 would otherwise have raised on non-auto-replenishing reserves.

- **A non-replenishing reserve must display a positive visible tranche
  (#230).** A `ReserveOrder` with `auto_replenish == false`,
  `visible_quantity == 0` and `hidden_quantity > 0` was admitted happily,
  and rested as a ghost: it showed no visible depth, could not be filled,
  and `pricelevel` removed it — stranding the whole hidden tranche — the
  first time a taker reached its level. Since #221 every quantity-carrying
  modify sets the **visible** tranche, so `UpdatePriceAndQuantity` and
  `Replace` with a zero quantity could drive a healthy resting reserve into
  that shape as well. `UpdateQuantity { new_quantity: 0 }` cannot: it is a
  removal, taken before the validator ever runs (#223, below).
  `validate_order_shape` now rejects it with the new
  `OrderBookError::ZeroVisibleTranche { order_id, hidden_quantity }`, which
  covers `add_order` and every modify projection
  through the one shared validator; a rejected modify leaves the original
  resting untouched. The error maps to the existing wire code
  `RejectReason::InvalidQuantity`, alongside `QuantityOverflow`.

  Scope: that shape **only**. The other zero-visible two-tranche shapes
  execute rather than vanishing, so they stay admissible — an
  `IcebergOrder` draws its whole hidden tranche into visible on match, and
  an auto-replenishing `ReserveOrder` refreshes and re-queues.
  Single-tranche kinds are unaffected, and a `(0, 0)` reserve is not
  covered — it carries nothing to strand. The residual paths already
  produce a positive visible tranche for every admitted order, so this
  closes the remaining way to create one.

  Compatibility: admission is strictly tighter, and on the modify paths the
  rule reaches `Replace` and `UpdatePriceAndQuantity` only. The case to
  check for is a **soft cancel** through one of those two: an integrator
  that sent `Replace { quantity: 0 }` or
  `UpdatePriceAndQuantity { .., new_quantity: 0 }` on a reserve carrying
  hidden depth and no automatic replenishment, expecting the order to go
  away, now receives `OrderBookError::ZeroVisibleTranche` and the original
  is left resting. Neither variant ever cancelled anything: the re-add
  rested a zero-visible ghost, which then lost its hidden tranche to the
  first taker that reached the level — so the rejection replaces a silent
  misbehaviour, not a working idiom. Call `cancel_order` instead. On an
  iceberg or an auto-replenishing reserve the same two variants are **not**
  rejected: they re-add with a zero visible tranche and live hidden depth,
  and the order keeps executing.

  `UpdateQuantity { new_quantity: 0 }` is outside this rule. It is a
  removal taken before the validator runs, so it cancels the whole order —
  hidden depth included — on every order kind (#223, below). An integrator
  who reaches for that variant now gets the removal they asked for; it is
  the supported soft cancel.

  A journal recorded before this change that contains such an `AddOrder`,
  or a `Replace` / `UpdatePriceAndQuantity` projecting that shape, fails on
  replay with `ReplayError::OrderBookError`. Snapshot restoration does not
  run `validate_order_shape`, but its prepare phase rejects the same shape
  before touching any book state, so a legacy snapshot holding one fails
  atomically with `ZeroVisibleTranche` and the live book is left as it was;
  such an order must be cancelled at the source and re-submitted with a
  positive visible tranche.

- **Reserve orders are lot-size validated per tranche and on their
  replenishment transfer (#226).** On a book with a lot size,
  `validate_order_shape` checked an `IcebergOrder` per tranche (visible and
  hidden individually) but routed a `ReserveOrder` through a `_` catch-all
  that only checked its **total**. With `lot_size = 10` a reserve of 15
  visible / 5 hidden was admitted on its total of 20 while the identical
  iceberg was rejected, and because the validator also runs on the
  projected order of every modify arm (`UpdatePrice`, `UpdateQuantity`,
  `UpdatePriceAndQuantity`, `Replace`) the asymmetry was reachable through
  updates as well as through `add_order`.

  The lot-size check is now an exhaustive match over every `OrderType`
  variant. `Standard`, `PostOnly`, `TrailingStop`, `PeggedOrder` and
  `MarketToLimit` keep the single-quantity rule. `ReserveOrder` takes the
  iceberg's per-tranche rule — `visible` and `hidden` each a whole multiple
  of the lot — and, additionally, validates the **capped transfer** that
  replenishment moves from hidden into the visible tranche, since that
  transfer is itself a quantity the book displays. The transfer is checked
  only while `hidden > 0` **and** `auto_replenish` is on, the single flag
  that decides whether anything is ever transferred on either path (#230):
  with `replenish_amount = Some(a)` it is `min(a, hidden)`; without an
  explicit amount it is `min(DEFAULT_RESERVE_REPLENISH_AMOUNT, hidden)`, the
  amount `pricelevel` transfers on depletion or below-threshold refresh and
  the one the residual-resting path behind `set_total_remaining` falls back
  to. With `auto_replenish = false` no transfer check applies whatever
  `replenish_amount` says: `pricelevel` removes a resting maker whose
  visible tranche is depleted, and the residual helper leaves the visible
  tranche empty so `add_order` ends the order — a depleted visible tranche
  ends the order on both paths and the book can never display a non-aligned
  quantity for it. `replenish_threshold` is
  unrestricted — it is only compared against the visible tranche, never
  transferred. Rejections use the existing
  `OrderBookError::InvalidLotSize`, carrying the offending quantity — the
  tranche or the transfer that failed.

  Compatibility: admission is strictly tighter. A reserve order that was
  previously admitted on its total — and any update projecting such a shape
  — is now rejected with `InvalidLotSize`. A journal recorded before this
  change that contains such an `AddOrder` or update therefore fails on
  replay with `ReplayError::OrderBookError` when the `ReplayBookConfig`
  carries the lot size; snapshot restoration does not run
  `validate_order_shape`, so a legacy snapshot may still hold orders that
  would now fail admission. Such a legacy order — restored from a snapshot,
  or orphaned by a later `set_lot_size` — can be repaired through
  `UpdateQuantity`, `UpdatePriceAndQuantity` or `Replace` when only its
  visible tranche is misaligned (a 15 / 20 reserve on a new lot of 10
  becomes 20 / 20, since every quantity-carrying update re-validates the
  projected order); when its hidden tranche or its replenishment
  configuration is what fails, no update can correct it and it must be
  cancelled and re-submitted with aligned tranches. No public API, snapshot
  format or
  `ORDERBOOK_SNAPSHOT_FORMAT_VERSION` change. This entry changes validation
  only; the residual-resting behaviour it describes
  (`reduce_reserve_to_total`, `set_total_remaining`) is changed separately
  by #230 above, and matching plus the upstream `pricelevel` semantics are
  untouched by both.

- **Self-trade prevention holds under concurrent same-user admission
  (#225).** With STP engaged the matching engine decided the
  `STPAction` for a price level from a snapshot of that level's queue and
  then acted on the decision in a second operation on the same level. Both
  steps ran under the *shared* side of the book's submit gate, so any other
  thread could admit, cancel or re-price an order in between and the verdict
  was applied to state it had never been taken on. Observed under
  `CancelMaker`: a same-user ask scanned a bid level, a same-user post-only
  landed in that level while the scan's verdict was still in flight, and the
  ask's sweep filled it — a same-user trade with the post-only as maker. The
  same window corrupted `safe_quantity` under `CancelTaker` and `CancelBoth`
  when the foreign maker resting ahead of the same-user maker was cancelled
  concurrently.

  The gate mode is now decided once, at the public boundary: an
  STP-relevant submit (STP enabled and a non-zero taker `user_id`) and the
  cancel-then-add modify variants whose re-add can match (`UpdatePrice`,
  `UpdatePriceAndQuantity`, `Replace`) take the **exclusive** side of the
  submit gate, so the scan and the fill it authorises observe the same
  queue. Books on `STPMode::None`, post-only submits (they never run the
  STP scan and never take liquidity), anonymous takers on the match-only
  entry points (`add_order` rejects a zero `user_id` under STP with
  `MissingUserId`), `UpdateQuantity` and `Cancel` are unchanged and keep
  the shared, fully concurrent path — with one addition from #230, which
  takes the exclusive side in every `STPMode` for a submit or re-price of a
  non-replenishing reserve carrying hidden quantity; on an STP book every other submit
  and every matching-capable re-price is therefore serialized.
  Fill-or-kill keeps its existing exclusive gate
  (#209). Internal helpers (`add_order_inner`, `cancel_order_with_reason`,
  `match_order_with_user_outcome`, `match_order_by_amount_with_user`)
  remain ungated and never upgrade, downgrade or re-acquire the lock.

  Scope: the guarantee covers every mutation. The live
  `restore_from_snapshot(&self)` now takes the exclusive
  side for its commit phase as well, so a restore can no longer
  interleave with an in-flight submit (the `&mut self` package and JSON
  restores were already exclusive by construction). The one remaining way
  to mutate a level behind the gate — the `Arc<PriceLevel>` handles
  returned by `get_bids()` / `get_asks()` — is gone: both accessors are
  removed in this release (#228, above).

  Callback re-entrancy: the documented contract now also covers
  `OrderStateListener`, which fires while the gate is held like
  `TradeListener` and `PriceLevelChangedListener`. Any gated `OrderBook`
  call made from one of these callbacks on the invoking thread may
  deadlock, and always deadlocks when the gate is held exclusively; the
  prohibition is absolute.

  No public API, snapshot or journal change;
  `ORDERBOOK_SNAPSHOT_FORMAT_VERSION` is unchanged. Single-thread and
  contention measurements are reported in the pull request.

- **`OrderUpdate::UpdateQuantity` with a zero quantity cancels the order
  (#223).** A zero `new_quantity` was accepted and applied as a resize:
  pricelevel keeps a non-growing total in place, so the maker rested at
  zero depth, holding `best_bid` / `best_ask` on a level with nothing
  behind it (a post-only at that price was refused against liquidity that
  did not exist) until a sweep dropped it with no trade and no cancel
  event, leaking its `order_locations` entry — `cancel_order` then
  returned `Ok(None)` while re-adding the id reported `DuplicateOrderId`,
  and the tracked status stayed `Open` forever. The arm now routes to the
  same `UserRequested` cancel that `OrderBook::cancel_order` performs
  (`cancel_order_with_reason`), so the level-change event, the
  `Cancelled { UserRequested }` transition, the per-account risk release,
  the location / user-index untrack and the empty-level removal happen in
  lockstep. **Contract:** a zero requested quantity is a removal, not a
  resize. It cancels the *entire* order, hidden depth of an iceberg or
  reserve order included (a nonzero `new_quantity` still resizes only the
  visible tranche), and it bypasses the projected-order validator and the
  modify-aware risk check, so a configured `min_order_size` no longer
  rejects it with `OrderSizeOutOfRange`. The kill switch still refuses it,
  as it refuses every modify. The removal semantic is `UpdateQuantity`'s
  alone: a zero quantity on `Replace` / `UpdatePriceAndQuantity` re-adds
  through validate-first, so an iceberg or auto-replenishing reserve rests
  with a zero visible tranche and its hidden depth live, while a
  non-replenishing reserve is rejected with `ZeroVisibleTranche` and keeps
  resting (#230); and on a single-tranche maker a zero quantity on either
  of those two variants ends the order as a terminal
  `Filled { filled_quantity: 0 }` — it vanishes with a fill status and no
  fill, which this release pins but does not change.

  Compatibility: this is a behaviour change on a call that previously
  succeeded, so a journal recorded before it that contains a zero
  `UpdateQuantity` replays to the new outcome — the order is removed where
  it previously replayed to a resting zero-quantity maker. The replayed
  book therefore legitimately differs from the one the original run
  produced, and `ReplayEngine::verify` against a snapshot taken before this
  change may return `Ok(false)`. Nothing fails on replay: the update itself
  is still accepted. No snapshot or journal format change, and
  `ORDERBOOK_SNAPSHOT_FORMAT_VERSION` is unchanged.

  Pinned for plain, iceberg and reserve makers, a `min_order_size` book, a
  risk-limited book, a shared level, an absent id and an engaged kill
  switch, plus the level-change event, the per-account risk release, the
  strandable-maker count and the contrast against the two cancel-then-add
  variants.

- **Reserve `UpdatePriceAndQuantity` honours the requested visible quantity
  (#221).** `OrderQuantity::set_quantity` read a `ReserveOrder`'s argument
  as a **total** target and only ever reduced: a requested increase was
  silently ignored (the order kept its previous size while the call
  reported success) and a decrease was drawn total-wise across both
  tranches, visible first and then hidden with replenish-on-empty. The
  single production caller is the `OrderUpdate::UpdatePriceAndQuantity`
  arm of `OrderBook::update_order`, so a reserve re-price plus re-size
  landed on a size nobody asked for; amplified case: a 30 visible / 70
  hidden reserve asked to move to 80 ended at 10 / 70 instead of 80 / 70.
  `set_quantity` now sets the **visible** tranche and leaves hidden
  untouched for both two-tranche kinds, so the new total is
  `new_quantity + hidden`. That matches `OrderUpdate::UpdateQuantity`,
  `OrderUpdate::Replace`, the iceberg arm of the same method and the
  upstream `pricelevel` contract (`OrderType::with_reduced_quantity`),
  which all treat the submitted quantity as the display size.

  Compatibility: no signature changes, but `OrderQuantity` is public, so
  the observable behaviour of `set_quantity` on a `ReserveOrder` changes
  for direct callers. `OrderQuantity::set_total_remaining` (the #210
  residual resting path) is unchanged and remains the total-target entry
  point. The journal format is unchanged and
  `ORDERBOOK_SNAPSHOT_FORMAT_VERSION` is **not** bumped; however, a
  journal recorded before this fix that contains a reserve
  `UpdatePriceAndQuantity` may produce different historical results on
  replay: the replayed book can differ from the one the original run
  produced, and the update can surface a validation rejection as
  `ReplayError::OrderBookError` (`QuantityOverflow`, or
  `OrderSizeOutOfRange` when the `ReplayBookConfig` carries the original
  size limits), because the requested size is now actually applied and
  the projected total is larger than the one the original run evaluated.
  Account risk limits are not part of `ReplayBookConfig`, so
  `RiskMaxNotional` cannot arise on replay. `ReplayEngine::verify` on
  such a journal may return `Ok(false)` when the replay succeeds and the
  resulting snapshot differs from the one captured by the pre-fix run,
  or propagate the replay error when the re-executed update is rejected.

- **Replay re-executes journaled submits that traded before returning an
  error (#224).** `add_order` emits real fills and *then* returns `Err`
  for an IOC's unfillable remainder and for a taker STP cancels after
  non-self fills. A sequencer records those as rejections, and
  `ReplayEngine` skipped every rejected event, so replay silently rebuilt
  liquidity the live book had consumed: journal a resting ask of 10 and
  an IOC buy of 15, and the replayed book still carried the ask. Replay
  now decides by the *recorded reject code* rather than by the
  success/failure classification. A submit (`AddOrder`, `MarketOrder`,
  `MarketOrderByAmount`) journaled as the new
  `SequencerResult::RejectedWithCode` is re-executed when its code is one
  replay can reproduce from the book state and `ReplayBookConfig` — the
  two post-fill codes (`InsufficientLiquidity`, `SelfTradePrevention`)
  and the pure admission rejections (tick, lot, size band, duplicate id,
  missing user, post-only crossing), which re-derive the same no-op and
  double as a check that the config matches the source book — and the
  re-execution must fail under the same `RejectReason`, or replay aborts
  with the new `ReplayError::OutcomeMismatch` (sequence, recorded code,
  and what replay produced instead: a success or a different error). A
  code whose trigger lives outside the config — the kill switch, the risk
  limits, `Other` — is **skipped, not re-executed**: those rejections
  never touch the book, so the skip reproduces the live outcome exactly,
  where re-executing a kill-switch rejection would rest an order the live
  book refused or consume liquidity it never touched. Replay therefore
  does not report a missing kill switch or `RiskConfig` as an
  `OutcomeMismatch`. A rejected non-submit is skipped as before (the
  modify paths validate first, cancels are no-ops on a missing order), and
  a journaled success whose re-execution fails still aborts with
  `ReplayError::OrderBookError`.

  The reject code is not always enough, so the journal records two more
  facts. `may_have_mutated` covers the one rejection whose code lies: the
  residual-admission failure, which the engine returns as a
  `PriceLevelError` — mapped to `RejectReason::Other(0)` — *after* the
  sweep's trades are irreversible, and which the code-driven skip would
  have replayed as a no-op that rebuilds consumed liquidity. A flagged
  submit is re-executed whatever its code says; since replay cannot
  reproduce that failure (it takes a concurrent mutation of the level),
  the disagreement surfaces as `OutcomeMismatch` rather than a silently
  wrong book. The rest of `Other(0)`, notably the clock-dependent
  expired-at-admission rejection, is unflagged and still skipped.
  `stp_mode` records the mode that decided a self-trade-prevention
  rejection, and replay refuses a `ReplayBookConfig` whose mode differs
  with the new `ReplayError::StpModeMismatch`.

  `last_applied_seq`, the applied-event count and the progress callback
  now follow what replay **dispatched** to the book, so a re-executed
  rejection advances them whether or not it traded; for a journal carrying
  such rejections they report more than the number of events that changed
  the book.

  **Stated limitations.** A submit journaled as the string-only `Rejected`
  keeps the historical skip and therefore the pre-existing gap: replay
  cannot tell a pure rejection from one that traded first without a code,
  and producers close it by recording `RejectedWithCode`. Only the reject
  code is compared — not the error's details (`requested` / `available`
  and the like), and not the fills behind the rejection — so a discrepancy
  confined to them can go undetected, because different fills can exhaust
  the same levels and leave identical books. The same holds for a replay
  config that differs in a way the code cannot see: under `CancelTaker`
  and `CancelBoth` alike a taker that fills a foreign maker and then
  reaches its own is refused under `SelfTradePrevention`, yet only
  `CancelBoth` cancels that same-user maker. The `stp_mode` guard catches
  exactly that case, but only for journals that recorded an STP rejection;
  a mode difference in a run that never prevented a self-trade stays
  invisible. `replay_from` performs no snapshot comparison —
  `snapshots_match` is the check that does catch a diverged book, via
  `ReplayEngine::verify` or run directly against a snapshot of the source
  book — and matching the source book's configuration remains the caller's
  contract on every `*_with_config` entry point. Finally, `MarketOrder` /
  `MarketOrderByAmount` carry no user id, so a market order journaled
  after STP effects under a user re-executes through the STP-less path and
  a rejection recorded for it aborts with `OutcomeMismatch` rather than
  diverging silently.

  Pinned end-to-end from a live book: the IOC remainder and the
  STP-cancelled taker replay their fills and advance the applied sequence;
  a kill-switch rejection is skipped with matching books; a tick rejection
  re-derives the same no-op; the string-only rejection keeps the skip; a
  rejected submit that succeeds on replay, or fails under a different
  code, aborts with `OutcomeMismatch`; a success journaled for a failed
  submit aborts with `OrderBookError`; a rejection flagged
  `may_have_mutated` under `Other(0)` is re-executed while the expired
  one is still skipped; a journaled `CancelTaker` rejection replayed under
  `CancelBoth` aborts with `StpModeMismatch`, and the same journal with
  the mode unrecorded replays "successfully" into a book `snapshots_match`
  rejects.

## [0.12.0] — 2026-07-14

### Changed (breaking, semver-minor under 0.x)

- **`pricelevel` 0.8.4 → 0.9.1.** Major upstream hardening release
  (PriceLevel #111–#120): validated admission (duplicate id, counter
  capacity, price/side topology), atomic PostOnly / fill-or-kill decisions,
  torn-read-safe execution statistics, and level snapshots materialized in
  queue-consumption order. 0.9.1 additionally fixes the `MatchResult`
  bincode round-trip (PriceLevel#135, found by this bump: 0.9.0 serialized
  a shape its own validated decoder rejected positionally, breaking the
  `bincode` feature's trade-event round-trip). Ripple on the re-exported
  surface:
  `PriceLevel::add_order` returns `Result`, `PriceLevel::matchable_quantity`
  takes the taker id (the dry run applies the sweep's self-match skip), and
  `PriceLevelError` gained the `DuplicateOrderId` variant.
- **`OrderBook::get_bt_bids` / `get_bt_asks` are now fallible**, returning
  `Result<BTreeMap<u128, PriceLevel>, OrderBookError>` — rebuilding a
  `PriceLevel` from a snapshot validates admission upstream (`TryFrom`
  replaced the infallible `From`).

### Fixed

- **PostOnly and multi-level FOK decisions are atomic at the book boundary
  (#209).** Both policies were checked before the sweep but every per-level
  match ran as a standard GTC taker, leaving a check-then-act race: a
  post-only order could take liquidity admitted between its precheck and
  its sweep, and a multi-level fill-or-kill could partially execute when a
  later level was cancelled after feasibility. PostOnly now threads
  `TakerKind::PostOnly` into every per-level match — pricelevel's
  structural guard makes trading impossible under any interleaving, with
  the sweep-time refusal surfacing as the same `PriceCrossing` rejection
  as the precheck. The crossability verdict is resolved BEFORE the STP
  block (post-only precedence over STP, documented on the submit APIs):
  a rejected post-only is a pure no-op that never cancels same-user
  makers as a CancelMaker/CancelBoth side effect. Multi-level FOK
  acquires a new book-level submit gate (`std::sync::RwLock`, write
  side) across its feasibility check and sweep; every other mutating
  entry point (add / update / cancel / mass cancel / evict / market
  sweeps) takes the read side. **Stated limitation:** while an FOK holds
  the gate the whole book serializes for that window, and FOK acquisition
  latency grows with in-flight readers (platform locks are
  writer-preferring, so FOK does not starve); trade / level listeners
  fire under the gate and must never re-enter the book on the invoking
  thread (contract documented on `TradeListener` /
  `PriceLevelChangedListener`). The matching core itself stays lock-free;
  a clean back-to-back HDR bisection shows the gate's uncontended read
  acquisition is not measurable on any scenario (add-only p50 917 = 917;
  see `BENCH.md`'s 0.11.0 → 0.12.0 delta section). Pinned by
  barrier-synchronized race regressions (contra-admission, later-level
  cancel, same-user CancelMaker) plus deterministic all-or-nothing tests.
  Three gaps closed on the fallible-mutation integration: (1)
  `OrderUpdate::UpdateQuantity` silently converted every upstream
  `PriceLevelError` into `Ok(None)` and bypassed book-level validation — it
  is now validate-first (projected order checked against tick / lot /
  min-max / two-tranche representability and the modify-aware risk gate
  before the level is touched), propagates upstream errors as
  `OrderBookError::PriceLevelError`, reserves `Ok(None)` for a genuinely
  absent order, and keeps per-account risk counters in lockstep with the
  applied update (new `on_quantity_update` hook). Routing through the
  shared validator also means an expired-but-unevicted GTD/DAY maker and
  a resting post-only maker whose price now crosses are rejected on
  quantity update — previously both silently succeeded. (2) A non-immediate
  taker whose residual could not be admitted into its same-side level
  (checked aggregate at capacity) used to consume contra liquidity and
  THEN fail — a headroom pre-check now rejects before the sweep emits any
  trade (conservative: full submitted total; the concurrent-only remainder
  is still guarded by pricelevel's validated admission). (3) If that racy
  post-trade admission ever fails, a level created empty by the attempt is
  removed — `best_bid` / `best_ask`, the cache, and depth gauges never see
  a phantom level — and the failure is logged at ERROR.
- **Two-tranche quantity conservation (#210).** An aggressive Iceberg /
  Reserve partial fill assigned the **total** unmatched remainder to the
  visible tranche while keeping the original hidden tranche — an iceberg
  `20v/80h` filled by 10 rested `90v/80h`, manufacturing 80 units of
  liquidity. The residual now rests via the new
  `OrderQuantity::set_total_remaining` (visible = min(display size,
  remainder), hidden = remainder − visible; Reserve keeps its
  visible-first-with-replenish policy), so
  `executed + resting == submitted` always holds (property-tested).
  Additionally, a two-tranche total that overflows `u64` (previously
  silently saturated) is rejected at admission with the new typed
  `OrderBookError::QuantityOverflow` — on the direct add path before even
  the risk gate (which would otherwise judge the saturated total), and
  always before any trade, listener, or book mutation — mapping to the
  wire code `RejectReason::InvalidQuantity`. `set_quantity` keeps its user-facing
  visible-tranche semantics for icebergs and is now documented as such.
- **`snapshots_match` is a full replay oracle (#208).** The replay equality
  check compared only per-level aggregates (price, visible/hidden quantity,
  order count), so two books with the same aggregates but reversed maker
  FIFO — or different maker ids, users, order variants, or time-in-force —
  certified as equal while emitting different trades on the next sweep. It
  now compares every level's complete order vector in queue-consumption
  order (pricelevel ≥ 0.9 materializes it that way) via full `OrderType`
  equality, plus the deterministic statistics counters including
  `stats_degraded`. Intentionally excluded and documented:
  `first_arrival_time` (pricelevel stamps it from raw `SystemTime::now()`,
  outside the injectable clock), `last_execution_time` /
  `sum_waiting_time` (live ingestion and replay consume different
  clock-tick budgets by design, so these wall-time aggregates diverge even
  under identically-seeded injected clocks), and the top-level capture
  timestamp. Order admission timestamps participate — the journal carries
  the admitted order verbatim and replay never re-stamps it. **Contract
  note:** this tightens the public `snapshots_match` /
  `ReplayEngine::verify` pass condition; aggregate-equal books with
  different maker identity or FIFO no longer certify as equal.
- **The upsize queue-priority demotion survives a snapshot round-trip
  (#205).** Level snapshots now materialize orders in queue-consumption
  order (pricelevel 0.9, PriceLevel#109), so
  `restore_from_snapshot_package` rebuilds the exact queue and a
  quantity-increased order keeps its back-of-queue position after restore.
  Locked in by a proptest regression
  (`tests/unit/props_quantity_update_priority.rs`). **Migration note:**
  snapshots captured with pricelevel < 0.9 restore a demoted order at its
  old `(timestamp, seq)` position — re-snapshot after upgrading to pin the
  corrected order.
- **Snapshot restoration is failure-atomic (#207).** `restore_from_snapshot`
  used to clear the live book before converting each level through
  pricelevel 0.9's fallible `PriceLevel::from_snapshot`, so an invalid later
  level destroyed the original state and left a valid prefix of the
  replacement installed; `restore_from_snapshot_package` additionally
  installed risk configuration before that fallible work. Restore is now
  two-phase: every fallible step (level validation, plus a new cross-level
  duplicate-order-id check that reports `OrderBookError::DuplicateOrderId`)
  runs against off-book structures, and the live book, indices, risk state,
  and configuration are only touched after all of it succeeds — any error
  leaves the pre-restore book byte-identical. The package path rebuilds risk
  entries during the commit walk (same registration as live admission),
  replacing the snapshot-based `rebuild_from_snapshot`. Two adjacent fixes
  ride along: a snapshot carrying two same-side levels at one price is now
  rejected (the install would keep one level while the index rebuild
  registered both), and restoring a package without a risk config now also
  purges any pre-restore risk entries — previously they survived `disable()`
  and could resurrect stale counters on a later `set_risk_config`.
- **Snapshot package format bumped to v3 (#206).** The pricelevel 0.9
  statistics schema can serialize a `stats_degraded` field that a
  pricelevel 0.8 reader rejects with `unknown field`, so labelling such
  payloads `version: 2` (as #205 initially shipped) mislabelled them as
  0.11-compatible. Newly written packages are stamped `3`; reads accept
  `2..=3` (`ORDERBOOK_SNAPSHOT_MIN_READ_VERSION`), so legacy v2 packages
  with the 8-field statistics shape still restore; `1` and future versions
  are rejected with the existing typed error, and the checksum is
  validated for both supported versions. Covered by a legacy-v2 restore
  fixture and a real-engine degraded-statistics (notional > `u64::MAX`)
  round-trip.

### Added

- **Queue-priority contract documented and property-tested (#203).** The
  price-time-priority semantics of `OrderBook::update_order` — an in-place
  quantity decrease keeps the maker's queue position, a quantity increase
  demotes to the back of the level, and `UpdatePrice` /
  `UpdatePriceAndQuantity` / `Replace` are cancel-then-add and always lose
  time priority — were previously documented only inside the embedded
  `pricelevel` engine. External conformance tooling (Tracebook) now consumes
  this contract, so it is promoted to the public `update_order` docs and
  locked in by four proptest invariants
  (`tests/unit/props_quantity_update_priority.rs`) driven through the public
  API: quantity decrease keeps the queue position, quantity increase demotes,
  and same-price `Replace` / `UpdatePriceAndQuantity` always demote. The docs
  also state a known limitation: the upsize demotion does not yet survive a
  snapshot restore (#205, upstream fix in `pricelevel`). No behavior change.

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
