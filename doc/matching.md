# Matching

How a taker order is matched against the book: sweep order, order types,
self-trade prevention, fees, deterministic emission, and the execution of
elected trailing stops (including the protection collar). Code lives in
`src/orderbook/matching.rs` (sweep), `operations.rs` / `modifications.rs`
(entry points), `stp.rs`, `fees.rs`, `stop_orders.rs` and
`stop_protection.rs`; the per-level queue and matcher come from
`pricelevel`.

## Sweep

A taker walks the opposite side from the best price outward: bids from
the highest, asks from the lowest. At each level it matches the resting
orders in time priority (FIFO) through `PriceLevel::match_order`, until its
quantity is exhausted, the next level is beyond its limit price, or the
side is empty. Before anything is mutated the call runs its preflights
(kill switch, risk, trade-id headroom, notional and fee arithmetic); a
failing preflight rejects the order untouched.

- **Limit** takers stop at their limit price; the remainder rests
  (`TimeInForce::Gtc` / `Gtd` / `Day`) or is cancelled (`Ioc`).
- **Fill-or-kill** takers first measure the fillable quantity and
  reserve result capacity without mutating the book; they trade in full
  or not at all.
- **Market** takers are unpriced: they walk as far as their quantity
  needs. A market order that finds no liquidity fails with
  `InsufficientLiquidity`; a partial remainder is cancelled.
- **Post-only** orders never take: one that would cross is rejected.
- Other `pricelevel` order types (iceberg, reserve, market-to-limit,
  pegged) follow the same sweep; pegged and trailing orders are repriced
  by `special_orders` (`repricing.rs`, `stop_orders.rs`).
- A pegged re-price is passive (#328): each peg reads the touch as it
  stands when it is re-priced, without its own order, and a target that
  would cross is reported in `RepricingResult::failed_orders` instead of
  trading. The guarantee is strict when sweeps run serialized with
  submits, as the sequencer runs them.

The sweep reuses per-thread scratch buffers (`MatchingPool`) so the hot
path stays allocation-light. Trades carry the maker and taker order ids,
price, quantity and fees.

## Self-trade prevention

`STPMode` is per book (`None` by default, zero overhead). When enabled,
each level is checked for same-user makers before it is matched:

| Mode | Effect |
|---|---|
| `None` | no check |
| `CancelTaker` | the taker is cancelled when it would reach its own maker; earlier fills stand |
| `CancelMaker` | every same-user maker at a touched level is cancelled, matching continues |
| `CancelBoth` | the taker and the reached maker are cancelled, matching stops |

`CancelTaker` and `CancelBoth` are reachability-gated (a taker already
satisfied by the depth ahead of its own maker fills normally);
`CancelMaker` is not. See the `STPMode` rustdoc for the details.

## Fees

`FeeSchedule { maker_fee_bps, taker_fee_bps }` is per book (optional).
Each trade's fee is its notional times the signed rate over `10_000`;
negative maker rates are rebates. The fee is carried on the trade in
`TradeResult`, the journal and the NATS payloads. Since 0.14.0 the engine
never clamps a fee: the worst-case notional is validated against the
schedule before the sweep and an unrepresentable one is rejected untouched
(`FeeOverflow`).

## Deterministic emission

Trades are produced in sweep order (level by level, FIFO inside a level).
A call's trades, level changes and order-state transitions are buffered in
its emission scope (`emission.rs`), stamped with a strictly increasing
`engine_seq`, and delivered after the submit gate is released. Events of
elected stops land in the same scope after the call's own events. Given the
same command stream and book configuration, replay reproduces the same
trades; `snapshots_match` is the oracle.

## Trailing stops and the protection collar

Pending trailing stops (`special_orders`) are off book. The prints of every
call that trades trail them and elect those they cross (see the
`stop_orders` module docs and `USER_GUIDE.md`). An elected stop leaves the
store, records `Triggered { child_id, trigger_price, limit_price }`
(`limit_price` is the child's collar limit, `None` for a market child),
and runs a child
order through the ungated matching path under the gate the call already
holds, with `origin_stop_id` on its trades. The child is always
immediate-or-cancel and never rests.

Its price depends on the book's `StopProtection` (#302,
[ADR 0001](adr/0001-stop-protection-collar.md)):

- **No collar** (default): an unpriced market order. It walks the
  opposite side as far as its quantity needs; a thin or gapped book fills
  it far from the stop price.
- **Collar `c`**: an IOC limit order at `stop - c` for a sell stop and
  `stop + c` for a buy stop, where `stop` is the stop's current (trailed)
  stop price at election, not the electing print. It trades only at levels
  at or inside that limit; the remainder is cancelled. This resembles CME
  protection points, **except** that CME rests the remainder at the limit
  while this book cancels it: a stop whose band is exhausted is consumed
  and leaves its position unprotected.

| Child outcome | Stop terminal state |
|---|---|
| fully filled | `Filled` |
| market child (no collar) with a remainder | `Cancelled { filled_quantity, reason: InsufficientLiquidity }` |
| collared child, remainder while liquidity is left beyond the limit (partial, or nothing within the band) | `Cancelled { filled_quantity, reason: StopProtectionBand }` |
| collared child, remainder because the side ran out within the band | `Cancelled { filled_quantity, reason: InsufficientLiquidity }` |
| self-trade prevented | `Cancelled { .., reason: SelfTradePrevention }` |
| publication of the child's trades failed | `Cancelled { filled_quantity, reason: MatchAborted }` |
| preflight failure (kill switch, arithmetic, duplicate child id) | `Rejected { reason }` |

Collar rules:

- The collar is an absolute offset in price units, never zero (unset is
  `None`). With a tick size it must be a multiple of it, so the limit of a
  tick-aligned stop is tick-aligned; the limit is only a matching bound and
  is not rounded.
- A band that reaches or passes the representable bound (a sell collar
  `>=` the stop price, a buy `stop + collar >= u128::MAX`) has the bound
  as its limit, `0` / `u128::MAX`: unbounded on that side, so that stop is not protected at
  all (silently; no per-election log on this hot path).
- An empty band is not counted in the `InsufficientLiquidity` reject
  metric: a limit that does not cross is not a rejection.
- STP, fees, risk and the trade-id / notional preflights apply to the
  child as to any taker.
- No stop child trades beyond its own band (the taker whose print elects
  the first stop is not collared). The number of children one print can
  run is still bounded only by the number of pending stops: a ladder of
  stops spaced one collar apart walks the book `k × collar` in one call
  (no depth limit or velocity pause). A gap of more than one collar
  through a stop always consumes it with zero fill.
- The collar is read once per elected stop; paths without a pending stop
  never read it. It travels in the snapshot package (format 6) and in
  `ReplayBookConfig::stop_protection`; replay must use the source book's
  collar, constant over the replayed range (it is not journaled), to
  reproduce its elections. A mismatch is not an
  `ReplayError::OutcomeMismatch`, and `snapshots_match` catches it only
  when it changed an outcome (a stop elected and filled differently):
  `OrderBookSnapshot` does not carry the collar, so with no election in
  the range, or coinciding fills, the snapshots match while future
  elections differ. Compare `stop_protection()` of the replayed and source
  books (or the snapshot package's field) explicitly.
