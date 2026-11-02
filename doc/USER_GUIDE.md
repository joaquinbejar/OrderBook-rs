# OrderBook-rs User Guide

Complete guide for using the OrderBook-rs library in your trading systems.

## Table of Contents

1. [Introduction](#introduction)
2. [Installation](#installation)
3. [Quick Start](#quick-start)
4. [Core Concepts](#core-concepts)
5. [Basic Operations](#basic-operations)
6. [Advanced Features](#advanced-features)
7. [Performance Optimization](#performance-optimization)
8. [Best Practices](#best-practices)
9. [Examples](#examples)
10. [Troubleshooting](#troubleshooting)

---

## Introduction

OrderBook-rs is a high-performance, lock-free order book implementation for financial trading systems. It provides:

- **Lock-free architecture** using crossbeam-skiplist for concurrent access
- **Multiple order types**: Limit, Market, Iceberg, Reserve, Post-only, FOK, IOC, GTD, Pegged and Trailing stop (`special_orders`; trailing stops are held off book, see [Trailing stops](#trailing-stops-special_orders))
- **Real-time metrics**: VWAP, spread, imbalance, depth statistics
- **Market impact simulation** for pre-trade analysis
- **Intelligent order placement** strategies
- **Enriched snapshots** with pre-calculated metrics
- **Trade notifications** with listener pattern

**Performance characteristics:**
- Single-threaded: ~1M orders/second
- 30-thread HFT simulation: ~600K orders/second
- Low latency: <1µs for order operations
- Lock-free data structures on the matching path: skiplist price levels,
  concurrent maps and atomics; no lock is taken per price level

Mutating entry points additionally pass a book-level gate that serializes
the two decisions that must not interleave with another mutation:
fill-or-kill feasibility and self-trade prevention. See
[Concurrent Access](#4-concurrent-access) for what is serialized and what
stays concurrent.

---

## Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
orderbook-rs = "0.15"
pricelevel = "0.10"
```

Optional features: `special_orders` (pegged repricing, off-book trailing stops),
`journal` (memory-mapped `FileJournal`), `nats` (JetStream publishers),
`bincode` (binary event serializer), `wire` (binary wire codec), `metrics`
(Prometheus-style counters through the `metrics` facade) and
`alloc-counters` (allocation counting for benches).

For simplified imports, use the prelude:

```rust
use orderbook_rs::prelude::*;
```

The snippets below are fragments. Unless shown otherwise they run inside a
function returning `Result<(), Box<dyn std::error::Error>>`, with `book` an
`OrderBook<()>`. Since 0.14.0 almost every query that aggregates quantities
or prices returns a `Result` (checked arithmetic, no panics), hence the `?`.

---

## Quick Start

### Creating an OrderBook

```rust
use orderbook_rs::prelude::*;

// Create order book
let book = OrderBook::<()>::new("BTC/USD");

// Add a buy order (returns the resting order)
book.add_limit_order(
    OrderId::from_u64(1),
    50_000, // price (u128, base units)
    10,     // quantity (u64)
    Side::Buy,
    TimeInForce::Gtc,
    None,   // no extra data
)?;

// Add a sell order
book.add_limit_order(
    OrderId::from_u64(2),
    50_100,
    10,
    Side::Sell,
    TimeInForce::Gtc,
    None,
)?;

// Best bid / ask
if let Some(best_bid) = book.best_bid() {
    println!("Best bid: {best_bid}");
}
if let Some(best_ask) = book.best_ask() {
    println!("Best ask: {best_ask}");
}
```

### Executing Market Orders

```rust
// Execute a market buy for 20 units
let result = book.submit_market_order(OrderId::from_u64(3), 20, Side::Buy)?;

// Check execution
println!("Filled: {} units", result.executed_quantity()?.as_u64());
println!("Unfilled: {} units", result.remaining_quantity().as_u64());
println!("Fills: {}", result.trades().len());
```

A market order that finds no liquidity at all returns
`OrderBookError::InsufficientLiquidity`. `submit_market_order_by_amount`
sweeps by quote notional instead of base quantity.

---

## Core Concepts

### Order Types

**Limit Orders:**
- Placed at specific price level
- Only execute at or better than limit price
- Can be partially filled

**Market Orders:**
- Execute immediately at best available price
- Consume liquidity from order book
- May experience slippage

**Iceberg and Reserve Orders (two tranches):**
- Hide large orders by showing only the visible tranche
- Replenish the visible tranche as it is filled (Reserve orders carry an
  explicit replenishment policy)
- Reduce market impact
- The tranches are independent and the order's total is `visible + hidden`.
  The quantity supplied to these modification variants addresses the
  **visible** tranche and leaves the hidden tranche untouched:
  `OrderUpdate::UpdateQuantity`, `OrderUpdate::UpdatePriceAndQuantity` and
  `OrderUpdate::Replace`. One exception: `UpdateQuantity` with a **zero**
  quantity is a removal and cancels the whole order, hidden depth
  included. `add_iceberg_order` takes the visible and hidden tranches as
  separate arguments
- On a book with a lot size the two tranches are validated individually,
  not on the total: a 15 visible / 5 hidden split is rejected on a lot-10
  book even though its total of 20 is a whole multiple. A Reserve order is
  additionally validated on the quantity its replenishment would move into
  the visible tranche, capped by the hidden tranche. That check applies only
  while `auto_replenish` is on, the single flag that decides whether anything
  is ever transferred: `min(replenish_amount, hidden)` when an explicit
  amount is set; `min(DEFAULT_RESERVE_REPLENISH_AMOUNT, hidden)` when there
  is none. With `auto_replenish` off nothing is ever transferred and no
  check applies, whatever `replenish_amount` says; nothing is checked either
  when the order carries no hidden tranche. `replenish_threshold` is
  unrestricted. Rejections return `OrderBookError::InvalidLotSize` naming the
  offending quantity
- A Reserve order with `auto_replenish` off must display a positive visible
  tranche: a `visible_quantity` of 0 behind a non-empty `hidden_quantity` is
  rejected with `OrderBookError::ZeroVisibleTranche`, on `add_order`, on
  every quantity-carrying modify (`UpdateQuantity`,
  `UpdatePriceAndQuantity`, `Replace`, all of which set the visible tranche)
  and on snapshot restore, where a snapshot holding the shape must be
  repaired before it can be restored. That shape shows no depth, never fills, and loses
  its whole hidden tranche to the first taker that reaches its level. The
  other zero-visible two-tranche shapes execute instead of vanishing and
  stay admissible: an Iceberg draws its entire hidden tranche into visible
  on match, and an auto-replenishing Reserve refreshes and re-queues
- An aggressive two-tranche order sweeps with its **total**, not with its
  visible tranche: a 10 visible / 20 hidden Reserve submitted into 20 units
  of contra liquidity executes 20. What its unmatched residual does follows
  `auto_replenish`. With it on and hidden left, a visible tranche the sweep
  left below `max(replenish_threshold, 1)` — an emptied one always is — is
  refreshed from hidden (the explicit `replenish_amount`, or
  `DEFAULT_RESERVE_REPLENISH_AMOUNT` when there is none, capped by hidden)
  and the residual rests. With it off the residual is discarded **only when
  the fill exhausted the visible tranche**: the order then ends as
  `OrderStatus::Filled { filled_quantity }` carrying only what executed,
  mirroring the resting side, where `pricelevel` removes a depleted
  non-replenishing maker and strands its hidden tranche. A shallower fill
  rests normally — that same 10 visible / 20 hidden Reserve filled for 5
  rests 5 / 20 with nothing discarded. The accounting rule
  holds in every case, and discarded quantity is never counted as executed:
  `submitted = executed + resting (visible + hidden) + discarded`
- An explicit `replenish_amount` is the **transfer**, not a target display
  size: it is added to whatever visible quantity survived the fill. With
  `replenish_amount = 10`, `replenish_threshold = 5` and a remainder of 2
  visible, the residual rests 12 visible. Without an explicit amount the
  transfer is `DEFAULT_RESERVE_REPLENISH_AMOUNT` capped by the hidden
  tranche, so that 10 visible / 20 hidden Reserve filled for 10 refreshes
  with `min(DEFAULT_RESERVE_REPLENISH_AMOUNT, 20) = 20` and rests 20 visible
  / 0 hidden — more than it first displayed

**Pegged orders (`special_orders`):**
- Pegged orders track a reference price (best bid, best ask, mid, last
  trade) and are re-priced by `reprice_pegged_orders` /
  `reprice_special_orders`

#### Trailing stops (`special_orders`)

A `TrailingStop` is a **pending off-book stop** (#286). It never rests on a
price level: it is not liquidity, does not appear in depth, analytics or the
level lists of a snapshot, and a crossing order never trades with it. It is
driven by the book's **prints**:

- **Trail.** `last_reference_price` is the watermark: a sell stop keeps the
  highest print seen since admission, a buy stop the lowest. The stop price
  (`price`) follows it at `trail_amount` (sell: `watermark - trail`, buy:
  `watermark + trail`) and only ever tightens. The watermark you submit is
  taken as is; trailing starts with the next trade.
- **Trigger.** A sell stop is elected by a print at or below its stop
  price, a buy stop by one at or above. The stop then leaves the book and
  executes as an immediate-or-cancel **market order** for its quantity, on
  its side, for its user: self-trade prevention, fees and the notional /
  trade-id checks apply as for any market taker, and an unexecuted
  remainder is cancelled. With a protection collar (below) it is an
  immediate-or-cancel **limit** order instead.
- **Protection collar (#302).** `book.set_stop_protection(Some(p))`
  installs a per-book collar `p = StopProtection::try_new(units)?`, an
  absolute offset in price units (like CME protection points, **except**
  that the remainder is cancelled, not rested at the limit: a stop whose
  band is exhausted is consumed and leaves its position unprotected). An
  elected sell stop then executes as an IOC limit at `stop - collar`, a buy
  stop at `stop + collar`, where `stop` is its current (trailed) stop price
  at election, not the print that elected it. It trades only at levels at
  or inside that limit and nothing ever rests. A remainder the collar cut
  (liquidity left beyond the limit, including an empty band) ends
  `Cancelled { reason: StopProtectionBand }`; a remainder left because the
  side ran out within the band ends `Cancelled { reason:
  InsufficientLiquidity }`. `Triggered { limit_price }` records the
  child's limit (`None` for a market child). The collar must be a multiple
  of the tick size (`InvalidTickSize`) and cannot be zero
  (`InvalidStopProtection`; unset is `None`); a later `set_tick_size` does
  not re-validate it. A band that reaches or passes the representable
  bound (a sell collar `>=` the stop price, a buy `stop + collar >=
  u128::MAX`) has the bound as its limit, `0` / `u128::MAX`: that stop is
  **not protected** on that side, silently, so size the collar well below the stop prices. Without a
  collar (the default) the child is the unpriced market order of 0.14.
  The collar travels in the snapshot package (format 6). For replay, pass
  it with `ReplayBookConfig::with_stop_protection`: it must match the
  source book **and** have been constant over the replayed range
  (`set_stop_protection` is not journaled). A mismatch is not reported as
  `ReplayError::OutcomeMismatch`, and `snapshots_match` catches it only
  when it changed an outcome (a stop elected and filled differently): the
  snapshot does not carry the collar, so with no election in the range, or
  coinciding fills, the snapshots match while future elections differ.
  Verify the configuration explicitly:
  `replayed.stop_protection() == source.stop_protection()` (or compare
  with the snapshot package's `stop_protection`).
- **Price path.** Each sweep is evaluated at its first print and then at
  its last one (a sweep's prints move one way, so these are its
  extremes): at each, stops trail first and are elected second. A falling
  sweep's first print can elect a buy stop, and a favourable first print
  trails a stop before the last print is tested against it.
- **When.** Automatically, under the submit gate, before every call that
  traded returns. Elections cascade (a stop's child order is a new sweep,
  evaluated next) until none is left; each stop fires at most once. The
  stops one print elects run in trigger order: sell stops highest first,
  buy stops lowest first, equal prices in admission order, the side the
  price moved towards first.
- **Link to the child order.** At election the stop records
  `OrderStatus::Triggered { child_id, trigger_price, limit_price }` (the
  order-state listener's election event), and the child order's
  `TradeResult`s carry
  `origin_stop_id = Some(stop_id)`; its taker id is
  `book.stop_trigger_order_id(stop_id)`, a UUIDv5 of the book's trade-id
  namespace (keep the namespace private: an order placed under that id
  first makes the stop end `Rejected { DuplicateOrderId }`). Replay with
  the same namespace reproduces it.
- **Lifecycle.** `Open` while pending, `Triggered` at election, then the
  child order's terminal state (`Filled`, `Cancelled { InsufficientLiquidity }`
  for a remainder, `Cancelled { StopProtectionBand }` for a remainder the
  collar cut, `Cancelled { SelfTradePrevention }`, ...).
  `cancel_order`, `update_order` (quantity, stop price, replace), every mass
  cancel (the price range matches the **current**, trailed stop price),
  `evict_expired_orders` and `get_order` cover pending stops;
  `trailing_stop_ids()` lists them in admission order.
- **Admission.** Time-in-force `Gtc`, `Gtd` or `Day` (not `Ioc` / `Fok`);
  a positive quantity and `trail_amount` (a multiple of the tick size), a
  stop price not beyond its own watermark (sell at or below it, buy at or
  above); tick / lot / size / STP-user validation as for any order. A stop
  the last trade already crosses is rejected with
  `OrderBookError::StopWouldTrigger` (code 24) rather than turned into a
  market order; a modify that would move a stop through the last trade is
  rejected the same way. Invalid terms are `OrderBookError::InvalidStopTerms`
  (code 25).
- **Risk.** A pending stop counts as an open order and books its notional
  at the stop price (the price band does not apply to a trigger price).
  The booking follows the stop as it trails **without enforcing limits**:
  a trailing sell stop can lift its account above
  `max_notional_per_account`, and that account's later admissions are then
  rejected. It is released on trigger or cancel.
- **Kill switch.** While engaged, elections are suspended (prints still
  trail), so protective stops are not consumed by child orders the kill
  switch would reject; the next print at or through a stop after the
  release elects it.
- **Limits.** Without a collar the market order is unpriced and the book
  applies no price band to market orders, so a thin or gapped book fills
  it far from the stop (and the risk booking at the stop price
  understates that fill); a collar bounds each child's price, at the cost
  of leaving a stop unexecuted when its band is empty: a gap of more than
  one collar through the stop always consumes it with zero fill. A
  cascade's length is bounded only by the number of pending stops: a
  ladder of stops spaced one collar apart still walks the book
  `k × collar` in one call (there is no cascade depth limit or velocity
  pause).
- **Cost.** While a stop is pending, every call on the book that can trade
  (limit and market orders, cancel-then-add modifies) and every call that
  targets a stop takes the exclusive submit gate; post-only adds and the
  cancels and quantity updates of other orders stay concurrent. A book
  with no pending stop pays a few relaxed atomic loads per call and one per
  matched level.

```rust
use orderbook_rs::OrderBook;
use pricelevel::{Hash32, Id, OrderType, Price, Quantity, Side, TimeInForce, TimestampMs};

let book: OrderBook<()> = OrderBook::new("BTC/USD");
let trader = Hash32::new([7u8; 32]);
// A protective sell stop 5 below a watermark of 100: stop price 95.
book.add_order(OrderType::TrailingStop {
    id: Id::from_u64(50),
    price: Price::new(95),
    quantity: Quantity::new(3),
    side: Side::Sell,
    user_id: trader,
    timestamp: TimestampMs::new(0),
    time_in_force: TimeInForce::Gtc,
    trail_amount: Quantity::new(5),
    last_reference_price: Price::new(100),
    extra_fields: (),
})?;
// Trades at 104 move it to 99; a print at or below 99 sells 3 at market.
```

With a collar the same stop sells only down to its stop price minus the
collar:

```rust
use orderbook_rs::{OrderBook, StopProtection};

let mut book: OrderBook<()> = OrderBook::with_tick_size("BTC/USD", 1);
// Elected at stop price 99: IOC limit at 97, the rest is cancelled.
book.set_stop_protection(Some(StopProtection::try_new(2)?))?;
```

Without the `special_orders` feature a `TrailingStop` is rejected untouched
with `OrderBookError::StopOrdersUnsupported` (reject code 23).

**Time-In-Force:**
- `Gtc` (Good-Till-Cancel): Remain until filled or cancelled
- `Ioc` (Immediate-Or-Cancel): Fill immediately or cancel
- `Fok` (Fill-Or-Kill): Fill completely or cancel entirely
- `Gtd(ms)` / `Day`: expire at a deadline, swept by
  `evict_expired_orders(now)`

### Sides

- `Side::Buy`: Bid side (buyers)
- `Side::Sell`: Ask side (sellers)

### Price Levels

Prices are `u128` and quantities `u64`, both in base units (e.g. cents,
satoshis).

Example: $500.00 = 50000 (in cents)

---

## Basic Operations

### Adding Orders

```rust
// Limit order
let order_id = OrderId::from_u64(10);
book.add_limit_order(
    order_id,
    50000,           // price
    100,             // quantity
    Side::Buy,
    TimeInForce::Gtc,
    None
)?;

// Iceberg order (visible tranche: 10, hidden tranche: 90, total: 100)
let order_id = OrderId::from_u64(11);
book.add_iceberg_order(
    order_id,
    50000,           // price
    10,              // visible quantity
    90,              // hidden quantity
    Side::Buy,
    TimeInForce::Gtc,
    None
)?;

// Market order (takes liquidity, never rests)
book.submit_market_order(OrderId::from_u64(12), 50, Side::Sell)?;
```

### Modifying Orders

Modifications go through `OrderBook::update_order` with an `OrderUpdate`
variant:

```rust
use pricelevel::{OrderUpdate, Price, Quantity};

// Resize in place
book.update_order(OrderUpdate::UpdateQuantity {
    order_id,
    new_quantity: Quantity::new(80),
})?;

// Re-price only
book.update_order(OrderUpdate::UpdatePrice {
    order_id,
    new_price: Price::new(50100),
})?;

// Re-price and resize in one call
book.update_order(OrderUpdate::UpdatePriceAndQuantity {
    order_id,
    new_price: Price::new(50100),
    new_quantity: Quantity::new(80),
})?;
```

**Queue priority:** `UpdateQuantity` keeps the order's queue position when the
new size is unchanged or smaller, and moves it to the back of its price level
when the size grows. `UpdatePrice`, `UpdatePriceAndQuantity` and `Replace` are
cancel-then-add: the order always re-enters at the back of its (possibly new)
price level. `Replace` and `UpdatePriceAndQuantity` do so even when the price
is unchanged; `UpdatePrice` to the current price is rejected instead.

**Two-tranche orders (iceberg / reserve):** the quantity carried by
`UpdateQuantity`, `UpdatePriceAndQuantity` and `Replace` sets the **visible**
tranche and leaves the hidden tranche untouched, so the resulting total is
`new_quantity + hidden`; an increase is applied, not clamped. Reducing the
hidden tranche is not reachable through an update: cancel and re-submit
instead. Shape validation (tick size, lot size, `visible + hidden`
representability) and the risk gate run on the projected order, and the
min / max order size limits apply to its `visible + hidden` total, so an
update can be rejected for a size larger than the quantity you passed.
A cancel-then-add modify (`UpdatePrice`, `UpdatePriceAndQuantity`,
`Replace`) of a Reserve order with `auto_replenish` off and a non-empty
hidden tranche is additionally rejected with
`OrderBookError::ReserveResidualWouldBeDiscarded` when the projected price
would cross into at least its visible tranche but less than its total,
because the re-added order's residual would not rest and its hidden
remainder would be destroyed; a projected full fill is allowed, and so is a
re-price that crosses less than the visible tranche. These pre-admission
rejections leave the original order unchanged.

**Zero `UpdateQuantity`:** a zero quantity on `UpdateQuantity` is a removal,
not a resize. It cancels the entire order (`Cancelled { UserRequested }`),
hidden depth included, and runs no projected validation, so a configured
`min_order_size` does not reject it; only the kill switch still refuses it,
as it refuses every modify. This applies to `UpdateQuantity` alone: a zero
quantity on `Replace` or `UpdatePriceAndQuantity` re-adds the order through
validate-first, so an iceberg or auto-replenishing reserve rests with a zero
visible tranche and its hidden depth live, while a reserve with
`auto_replenish` off is rejected with `ZeroVisibleTranche` and keeps resting.

### Cancelling Orders

```rust
// Cancel a specific order: Ok(Some(order)) if it was resting,
// Ok(None) if it was already gone
book.cancel_order(order_id)?;

// Cancel all orders on one side (mass cancels return a MassCancelResult)
let result = book.cancel_orders_by_side(Side::Buy);
println!("Cancelled {} order(s)", result.cancelled_count());
if result.has_failures() {
    // A level or order the book could not cancel stays resting and tracked
    eprintln!("Mass cancel failures: {:?}", result.failures());
}
```

`cancel_all_orders`, `cancel_orders_by_user` and
`cancel_orders_by_price_range` follow the same shape.

### Querying Order Book State

```rust
// Best prices
let best_bid = book.best_bid();
let best_ask = book.best_ask();

// Spread (absolute and in basis points; `None` uses the default multiplier)
let spread = book.spread_absolute();
let spread_bps = book.spread_bps(None);

// Depth of the top 5 levels on each side
let bid_depth = book.total_depth_at_levels(5, Side::Buy)?;
let ask_depth = book.total_depth_at_levels(5, Side::Sell)?;

// Check if an order is resting
let exists = book.get_order(order_id).is_some();
```

---

## Advanced Features

### 1. Market Metrics

Calculate key trading metrics for decision making.

```rust
// VWAP to fill 100 units against the asks (buy side)
let vwap: Option<f64> = book.vwap(100, Side::Buy)?;

// Mid price
let mid = book.mid_price();

// Spread in basis points
let spread_bps = book.spread_bps(None);

// Order book imbalance over the top 5 levels (-1.0 to 1.0)
let imbalance = book.order_book_imbalance(5)?;

// Micro price (imbalance-adjusted)
let micro_price: Option<f64> = book.micro_price()?;
```

**Use cases:**
- Trading signal generation
- Fair value calculation
- Market condition detection
- Risk assessment

### 2. Market Impact Simulation

Simulate order execution to assess pre-trade impact. Neither call mutates
the book.

```rust
// Level-by-level fills a 1000-unit market buy would get
let simulation = book.simulate_market_order(1000, Side::Buy)?;
println!("Average price: {}", simulation.avg_price);
println!("Filled / unfilled: {} / {}", simulation.total_filled, simulation.remaining_quantity);
println!("Total cost: {}", simulation.total_cost()?);

// Aggregate impact of the same order
let impact = book.market_impact(1000, Side::Buy)?;
println!("Slippage: {:.2} bps", impact.slippage_bps);
println!("Levels consumed: {}", impact.levels_consumed);

// Decide based on impact
if impact.slippage_bps < 50.0 {
    // Execute order
    book.submit_market_order(OrderId::from_u64(20), 1000, Side::Buy)?;
} else if let Some(best_bid) = book.best_bid() {
    // Impact too high: rest passively at the best bid instead of taking
    // liquidity. Post-only guarantees the order never crosses: a price at
    // or through the best ask would be rejected, not matched.
    book.add_post_only_order(
        OrderId::from_u64(20),
        best_bid,
        1000,
        Side::Buy,
        TimeInForce::Gtc,
        None,
    )?;
} else {
    // No bid to join: defer the order rather than guess a price.
}
```

Note that a limit buy at `impact.worst_price` would **not** rest: that
price is on the ask side, so the order crosses and executes the same
sweep. Use it only deliberately, as a marketable limit that caps the
execution price (for example with `TimeInForce::Ioc`).

**Use cases:**
- Pre-trade risk assessment
- Order type selection
- Position sizing
- Execution strategy

### 3. Intelligent Order Placement

Optimize order placement for market makers and smart routing.

```rust
// Orders queued ahead at a specific price
let queue_ahead = book.queue_ahead_at_price(50_000, Side::Buy);
println!("Orders ahead: {queue_ahead}");

// Price 3 ticks inside the best bid (tick size 1)
let price = book.price_n_ticks_inside(3, 1, Side::Buy);

// Price at which an order would be at queue position 100
let target_price = book.price_for_queue_position(100, Side::Buy);

// Price reaching 1000 units of depth, snapped to a tick size of 1
let adjusted_price = book.price_at_depth_adjusted(1000, 1, Side::Buy)?;
```

**Use cases:**
- Market maker order placement
- Smart order routing
- Execution optimization
- Liquidity provision

### 4. Functional Iterators

Lazy iteration over levels, best price first. Every item is a
`Result<LevelInfo, OrderBookError>` (a level whose `visible + hidden`
total overflows is reported, not read as empty), and an iterator stops
after its first error.

```rust
// Levels until 1000 units of cumulative depth are reached
let levels: Vec<LevelInfo> = book
    .levels_until_depth(1000, Side::Buy)
    .collect::<Result<_, _>>()?;

// Cumulative depth tracking over the top 10 ask levels
for level in book.levels_with_cumulative_depth(Side::Sell).take(10) {
    let level = level?;
    println!(
        "Price: {}, Size: {}, Cumulative: {}",
        level.price, level.quantity, level.cumulative_depth
    );
}

// Levels within a price range
let in_range: Vec<LevelInfo> = book
    .levels_in_range(49_000, 50_000, Side::Buy)
    .collect::<Result<_, _>>()?;

// Combine with functional operations (checked sum)
let total_volume = book
    .levels_until_depth(5000, Side::Buy)
    .try_fold(0u64, |acc, level| {
        let level = level?;
        acc.checked_add(level.quantity)
            .ok_or(OrderBookError::ArithmeticOverflow { operation: "sum level quantities" })
    })?;
```

**Benefits:**
- Zero-allocation iteration
- Lazy evaluation (compute only what's needed)
- Composable operations
- Short-circuit optimization

### 5. Aggregate Statistics

Comprehensive statistical analysis for market condition detection.

```rust
// Depth statistics over the top 10 bid levels (0 = all levels)
let stats = book.depth_statistics(Side::Buy, 10)?;
println!("Total volume: {}", stats.total_volume);
println!("Average level size: {:.2}", stats.avg_level_size);
println!("Weighted avg price: {:.2}", stats.weighted_avg_price);
println!("Std dev: {:.2}", stats.std_dev_level_size);
println!("Min/Max: {} / {}", stats.min_level_size, stats.max_level_size);

// Market pressure
let (buy_pressure, sell_pressure) = book.buy_sell_pressure()?;
println!("Buy pressure: {buy_pressure}, Sell pressure: {sell_pressure}");

// Thin book detection (threshold 1000 units over the top 5 levels)
if book.is_thin_book(1000, 5)? {
    println!("Low liquidity detected");
}

// Depth distribution in 5 bins (capped at MAX_DEPTH_DISTRIBUTION_BINS)
for bin in book.depth_distribution(Side::Buy, 5)? {
    println!(
        "Price range: {} - {}, Volume: {}, Levels: {}",
        bin.min_price, bin.max_price, bin.volume, bin.level_count
    );
}
```

**Use cases:**
- Market condition detection
- Risk management
- Strategy adaptation
- Trading decision support

### 6. Enriched Snapshots

Pre-calculated metrics in snapshots for high-frequency trading.

```rust
// Snapshot of the top 10 levels with every metric
let snapshot = book.enriched_snapshot(10)?;
println!("Mid price: {:?}", snapshot.mid_price);
println!("Spread: {:?} bps", snapshot.spread_bps);
println!("Bid depth: {}", snapshot.bid_depth_total);
println!("Ask depth: {}", snapshot.ask_depth_total);
println!("Imbalance: {}", snapshot.order_book_imbalance);
println!("VWAP bid: {:?}", snapshot.vwap_bid);
println!("VWAP ask: {:?}", snapshot.vwap_ask);

// Only the metrics you need
use orderbook_rs::MetricFlags;

let snapshot = book.enriched_snapshot_with_metrics(10, MetricFlags::MID_PRICE | MetricFlags::SPREAD)?;

// Serialize for distribution
let json = serde_json::to_string(&snapshot)?;
```

**Benefits:**
- Single pass through data (vs 5+ passes)
- Better cache locality
- Lower latency
- Consistent timestamp for all metrics
- Optional metric selection

---

## Performance Optimization

### 1. Choose the Right Data Types

```rust
// Integer base units for prices (u128) and quantities (u64)
let price: u128 = 50_000; // $500.00 in cents
let quantity: u64 = 100;

// f64 only for calculated metrics
let vwap: f64 = book.vwap(quantity, Side::Buy)?.unwrap_or(0.0);
```

### 2. Minimize Allocations

```rust
// Fold over the iterator instead of collecting
let sum = book
    .levels_until_depth(1000, Side::Buy)
    .try_fold(0u64, |acc, level| {
        let level = level?;
        acc.checked_add(level.quantity)
            .ok_or(OrderBookError::ArithmeticOverflow { operation: "sum level quantities" })
    })?; // No allocation

// Instead of:
let levels: Vec<LevelInfo> = book
    .levels_until_depth(1000, Side::Buy)
    .collect::<Result<_, _>>()?; // Allocates a Vec
```

### 3. Use Enriched Snapshots for Multiple Metrics

```rust
// Inefficient: multiple passes
let mid = book.mid_price();
let spread = book.spread_bps(None);
let depth = book.total_depth_at_levels(10, Side::Buy)?;
let vwap = book.vwap(100, Side::Buy)?;

// Efficient: single pass
let snapshot = book.enriched_snapshot(10)?;
let mid = snapshot.mid_price;
let spread = snapshot.spread_bps;
let depth = snapshot.bid_depth_total;
let vwap = snapshot.vwap_bid;
```

### 4. Batch Operations

```rust
// Add multiple orders
let orders = [
    (OrderId::from_u64(100), 50_000, 10),
    (OrderId::from_u64(101), 49_990, 20),
    (OrderId::from_u64(102), 49_980, 30),
];

for (id, price, qty) in orders {
    book.add_limit_order(id, price, qty, Side::Buy, TimeInForce::Gtc, None)?;
}
```

### 5. Use Appropriate Depth Limits

```rust
// Only analyze what you need
let stats = book.depth_statistics(Side::Buy, 5)?; // Top 5 levels only

// Instead of:
let stats = book.depth_statistics(Side::Buy, 0)?; // 0 = all levels (slower)
```

---

## Best Practices

### 1. Error Handling

```rust
use orderbook_rs::OrderBookError;

match book.add_limit_order(order_id, price, qty, Side::Buy, TimeInForce::Gtc, None) {
    Ok(_order) => {
        println!("Order added successfully");
    }
    Err(OrderBookError::DuplicateOrderId { order_id }) => {
        eprintln!("Order {order_id} already exists");
    }
    Err(e) => {
        // `OrderBookError` is #[non_exhaustive]: keep a catch-all arm
        eprintln!("Error: {e}");
    }
}
```

Every rejection maps to a stable wire code through
`RejectReason::from(&err)` (for example `DuplicateOrderId` is 12,
`MatchAborted` 15, `FeeOverflow` 18). A `MatchAborted`, `ModifyRolledBack`,
`ModifyOrderLost` or `RiskRejectedAfterTrades` error can follow real
trades: check its fields before assuming nothing happened.

### 2. Trade Notifications

A listener is installed on the book, not passed per call:

```rust
use orderbook_rs::prelude::*;
use std::sync::{Arc, mpsc};

let (tx, rx) = mpsc::channel();

let mut book = OrderBook::<()>::new("BTC/USD");
book.set_trade_listener(Arc::new(move |trade: &TradeResult| {
    // Do not touch the book from here; hand the event off and return.
    let _ = tx.send((trade.engine_seq, trade.match_result.trades().len()));
}));

let book = Arc::new(book);
book.submit_market_order(OrderId::from_u64(200), 100, Side::Buy)?;

while let Ok((seq, fills)) = rx.try_recv() {
    println!("engine_seq {seq}: {fills} fill(s)");
}
```

**Delivery and re-entrancy (0.14.0, #249).** `TradeListener`,
`PriceLevelChangedListener` and `OrderStateListener` run **after** the book
operation that produced the event has committed and released the
book-level gate described in [Concurrent Access](#4-concurrent-access),
never mid-mutation. Per book, events are delivered in one total order
consistent with commit order (`engine_seq` strictly increases across the
trade and price-level streams, also with concurrent submitters), by
whichever thread is dispatching: usually the submitter before its call
returns, but under concurrency possibly another thread after the submit
returned. A listener may call back into the same `OrderBook` (the nested
call's events are delivered after the current batch). Listeners must not
panic and must return quickly: a slow listener delays every later event
of the book, so pushing onto a channel is still the recommended shape.

### 3. State Management

Use a snapshot **package** for persistence: it carries the format version
(currently 6; versions 2 to 6 restore), a checksum, and the book's
configuration (fees, STP mode, tick / lot size, order-size limits, risk
config, kill switch, `engine_seq`, stop protection collar), and restore
validates all of it before touching the live book. From version 5 the
checksummed payload also carries the pending trailing stops and the last
trade price; version 6 adds the collar (a 0.14 reader refuses it).

```rust
use orderbook_rs::orderbook::OrderBookSnapshotPackage;

// Capture every level (usize::MAX = no depth limit) and persist
let package = book.create_snapshot_package(usize::MAX)?;
std::fs::write("orderbook_snapshot.json", package.to_json()?)?;

// Restore later into a book you own (`&mut self`)
let json = std::fs::read_to_string("orderbook_snapshot.json")?;
let package = OrderBookSnapshotPackage::from_json(&json)?;
let mut restored = OrderBook::<()>::new("BTC/USD");
restored.restore_from_snapshot_package(package)?;
```

`create_snapshot(depth)` / `restore_from_snapshot(snapshot)` still exist
for the levels and orders alone, without configuration, version or
checksum. A crossed or locked snapshot is rejected on restore
(`OrderBookError::SnapshotCrossed`).

### 4. Concurrent Access

```rust
use std::sync::Arc;

// Share order book across threads
let book = Arc::new(OrderBook::<()>::new("BTC/USD"));

// Clone Arc for each thread
let book_clone = Arc::clone(&book);
std::thread::spawn(move || {
    // Use book_clone in thread
    let _ = book_clone.add_limit_order(
        OrderId::from_u64(300),
        50000,
        10,
        Side::Buy,
        TimeInForce::Gtc,
        None
    );
});
```

**What the book serializes.** The price-level map, the order index and the
statistics counters are lock-free structures: skiplists, concurrent maps
and atomics, with no per-level lock. On top of those, the mutating entry
points pass a single book-level gate, which is taken in one of two modes:

| Operation | Gate mode |
|---|---|
| Ordinary submit, cancel, `UpdateQuantity` | shared |
| Mass cancel (all, by side, by user, by price range) and `evict_expired_orders` | exclusive |
| Fill-or-kill submit | exclusive |
| Identified submit (any kind except post-only) or market sweep with STP enabled | exclusive |
| Post-only submit, any STP mode (never runs the STP scan) | shared |
| `UpdatePrice` / `UpdatePriceAndQuantity` / `Replace` with STP enabled | exclusive |

The exclusive modes exist because both decisions are made in one step and
applied in a second one: a fill-or-kill checks multi-level feasibility and
then sweeps, and a self-trade-prevention submit scans a price level's queue
and then fills it. Holding the gate exclusively across both steps is what
stops another thread from admitting, cancelling or re-pricing an order in
between and having the decision applied to a book state it was never taken
on.

Everything else stays on the shared side and runs concurrently. A book left
on the default `STPMode::None` takes the exclusive side for fill-or-kill
and, from #230, while it **holds** a Reserve order with `auto_replenish` off
that carries hidden quantity: every **sweep** on such a book is exclusive —
every matching-capable submit, every cancel-then-add re-price and every
match-only entry point (`match_order`, `match_market_order*`) — as is the
admission of the first such reserve. A sweep decides once whether to capture
makers whose hidden depth it would strand, so nothing may cancel, admit or
replace an order inside that sweep's capture window; otherwise the sweep
could consume a maker it never captured, or report a captured maker's hidden
quantity after a cancel freed its id for an unrelated order. Post-only
submits, `UpdateQuantity` and single cancels keep the shared side and
never consult the count: they are excluded by the sweep's hold, not by
taking the exclusive side themselves. Those books serialize their sweeps, as
STP books do; books holding no such reserve are unchanged. Enabling STP therefore serializes every identified submit
except post-only, and every matching-capable re-price, on that book.
Post-only orders never take liquidity and never run the STP scan, so
they keep the shared side; they are excluded from an identified taker's
window by that taker's exclusive hold, not by their own. The shared path for an
anonymous taker (`Hash32::zero()`, which skips the STP scan) is reachable
only through the match-only entry points (`match_order_with_user`,
`match_market_order_with_user`, `match_market_order_by_amount_with_user`):
`add_order` rejects a zero `user_id` with `MissingUserId` whenever STP is
enabled, so mixing anonymous and identified flow does not preserve submit
concurrency on an STP book.

**Scope.** The guarantee covers every mutation: the submit, cancel,
modify, mass-cancel and market-sweep entry points listed above, plus
snapshot restores. Nothing sits outside it; two things are worth
spelling out:

- The public API hands out no level handles. `get_bids()` / `get_asks()`
  cloned the live `Arc<PriceLevel>` handles, and `PriceLevel` exposes
  `add_order`, `update_order` and `match_order` publicly, so a caller could
  mutate a level behind the gate — and behind the indices, the risk state,
  STP, the kill switch, the order-state tracker and the listeners. Both were
  removed in 0.13.0 (issue #228); every level mutation now goes through
  `OrderBook`. Read a level's contents through the value-returning APIs
  instead: `create_snapshot(depth)` for a full snapshot of every level and
  order; `levels_with_cumulative_depth`, `levels_until_depth`,
  `levels_in_range` and `find_level` for `LevelInfo` views;
  `order_count_at_price`, `get_orders_at_price`, `get_all_orders` and
  `total_depth_at_levels` for per-price / per-book order data; `best_bid` /
  `best_ask` for the top of book.
- Snapshot restores are covered: the live `restore_from_snapshot(&self)`
  takes the exclusive side of the gate for its commit phase, and the
  `&mut self` package / JSON restores are exclusive by construction.

**Read-only queries are never gated**, so `best_bid`, `best_ask`,
snapshots, statistics and the iterators can be called from any thread,
including from inside a listener callback.

### 5. Custom Extra Data

```rust
use serde::{Deserialize, Serialize};

// `OrderBook<T>` needs `T: Default + Clone + Send + Sync + 'static`
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct OrderMetadata {
    user_id: String,
    strategy: String,
}

let book = OrderBook::<OrderMetadata>::new("BTC/USD");

let metadata = OrderMetadata {
    user_id: "user123".to_string(),
    strategy: "market_maker".to_string(),
};

book.add_limit_order(
    OrderId::from_u64(400),
    50_000,
    10,
    Side::Buy,
    TimeInForce::Gtc,
    Some(metadata),
)?;
```

---

## Examples

### Example 1: Simple Market Maker

```rust
use orderbook_rs::prelude::*;

fn market_maker_strategy(book: &OrderBook<()>, next_id: u64) -> Result<(), OrderBookError> {
    // Current market state
    let snapshot = book.enriched_snapshot(5)?;

    if let (Some(mid), Some(spread_bps)) = (snapshot.mid_price, snapshot.spread_bps) {
        // Only make markets if the spread is tight enough
        if spread_bps < 20.0 {
            let offset = 5.0;
            let bid_price = (mid - offset) as u128;
            let ask_price = (mid + offset) as u128;

            book.add_limit_order(
                OrderId::from_u64(next_id),
                bid_price,
                10,
                Side::Buy,
                TimeInForce::Gtc,
                None,
            )?;
            book.add_limit_order(
                OrderId::from_u64(next_id + 1),
                ask_price,
                10,
                Side::Sell,
                TimeInForce::Gtc,
                None,
            )?;

            println!("Market making: bid @ {bid_price}, ask @ {ask_price}");
        } else {
            println!("Spread too wide: {spread_bps:.2} bps");
        }
    }

    Ok(())
}
```

### Example 2: Smart Order Execution

```rust
use orderbook_rs::prelude::*;

fn execute_large_order(
    book: &OrderBook<()>,
    id: OrderId,
    quantity: u64,
    side: Side,
) -> Result<(), OrderBookError> {
    // Assess impact first (read-only)
    let impact = book.market_impact(quantity, side)?;
    println!("Average price: {}", impact.avg_price);
    println!("Slippage: {:.2} bps", impact.slippage_bps);

    if impact.slippage_bps < 50.0 {
        // Low impact: market order
        book.submit_market_order(id, quantity, side)?;
    } else {
        // High impact: join our own side's best price passively. Post-only
        // guarantees it rests (a crossing price is rejected, not matched);
        // with no price to join, defer the order.
        let passive_price = match side {
            Side::Buy => book.best_bid(),
            Side::Sell => book.best_ask(),
        };
        if let Some(price) = passive_price {
            book.add_post_only_order(id, price, quantity, side, TimeInForce::Gtc, None)?;
        }
    }

    Ok(())
}
```

### Example 3: Liquidity Monitoring

```rust
use orderbook_rs::prelude::*;

fn monitor_liquidity(book: &OrderBook<()>) -> Result<(), OrderBookError> {
    let stats_bid = book.depth_statistics(Side::Buy, 10)?;
    let stats_ask = book.depth_statistics(Side::Sell, 10)?;

    println!("Liquidity Report:");
    println!("  Bid volume: {} (std dev {:.2})", stats_bid.total_volume, stats_bid.std_dev_level_size);
    println!("  Ask volume: {} (std dev {:.2})", stats_ask.total_volume, stats_ask.std_dev_level_size);

    if book.is_thin_book(1000, 5)? {
        println!("WARNING: thin book detected; reduce position sizes");
    }

    let imbalance = book.order_book_imbalance(5)?;
    if imbalance > 0.3 {
        println!("Strong buy pressure detected");
    } else if imbalance < -0.3 {
        println!("Strong sell pressure detected");
    }

    Ok(())
}
```

---

## Troubleshooting

### Common Issues

**Issue: Order not added**

```rust
// Check for a duplicate order ID
match book.add_limit_order(order_id, price, qty, Side::Buy, TimeInForce::Gtc, None) {
    Err(OrderBookError::DuplicateOrderId { .. }) => {
        // Use a fresh id
        book.add_limit_order(OrderId::from_u64(500), price, qty, Side::Buy, TimeInForce::Gtc, None)?;
    }
    Ok(_order) => { /* success */ }
    Err(e) => eprintln!("Error: {e}"),
}
```

**Issue: Market order not filled**

```rust
// Check available liquidity first (usize::MAX = every level)
let depth = book.total_depth_at_levels(usize::MAX, Side::Sell)?;
if depth < quantity {
    println!("Insufficient liquidity: {depth} available, {quantity} needed");
    // Rest a limit order instead
    book.add_limit_order(order_id, price, quantity, Side::Buy, TimeInForce::Gtc, None)?;
} else {
    book.submit_market_order(order_id, quantity, Side::Buy)?;
}
```

**Issue: Performance degradation**

```rust
// Use enriched snapshots instead of multiple metric calls
// Slow
let mid = book.mid_price();
let spread = book.spread_bps(None);
let vwap = book.vwap(100, Side::Buy)?;

// Fast
let snapshot = book.enriched_snapshot(10)?;
```

**Issue: Memory usage**

```rust
// Limit snapshot depth
let snapshot = book.create_snapshot(10)?; // Only the top 10 levels per side

// Instead of:
let snapshot = book.create_snapshot(usize::MAX)?; // Every level (high memory)
```

**Issue: A listener callback is slow or blocks**

Since 0.14.0 listeners run after the gate is released, so a listener that
mutates the same book no longer deadlocks. A listener that blocks still
delays every later event of the book (one dispatcher per book delivers them
in order), so hand the event off and do the work elsewhere.

```rust
// Preferred: hand the event off and work from another context.
book.set_trade_listener(Arc::new(move |trade: &TradeResult| {
    let _ = tx.send(trade.engine_seq);
}));
```

This applies to `TradeListener`, `PriceLevelChangedListener` and
`OrderStateListener` alike. See
[Trade Notifications](#2-trade-notifications) for the full contract.

### Debug Tips

```rust
// Enable logging (set RUST_LOG=debug in the environment; needs the
// `tracing-subscriber` crate with its `env-filter` feature in your binary;
// the library never installs a subscriber itself)
tracing_subscriber::fmt()
    .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
    .init();

// Check order book state
let snapshot = book.create_snapshot(usize::MAX)?;
println!("Bid levels: {}", snapshot.bids.len());
println!("Ask levels: {}", snapshot.asks.len());
println!("Total orders: {}", book.get_all_orders().len());

// Verify an order exists
match book.get_order(order_id) {
    Some(order) => println!("Order {order_id} rests: {order:?}"),
    None => println!("Order {order_id} not found"),
}

// Operational health
println!("Kill switch engaged: {}", book.is_kill_switch_engaged());
println!("Listener panics: {}", book.listener_panics());
```

---

## Performance Benchmarks

Based on Apple M4 Max processor:

**Single-threaded operations:**
- Add limit order: ~1.2M ops/sec
- Cancel order: ~1.5M ops/sec
- Market order: ~900K ops/sec
- Best bid/ask: ~15M ops/sec

**Multi-threaded (30 threads):**
- Total throughput: ~600K orders/sec
- Per-thread: ~20K orders/sec
- No per-price-level locking; concurrency across threads depends on the
  gate mode of the ops being issued (see
  [Concurrent Access](#4-concurrent-access)). These figures do not
  characterise a book running with self-trade prevention engaged: for the
  STP gate-mode comparison see `BENCH.md`.

**Metrics calculation:**
- VWAP: ~2µs (10 levels)
- Depth statistics: ~3µs (10 levels)
- Enriched snapshot: ~5µs (all metrics)

**Memory usage:**
- Base order book: ~1KB
- Per order: ~120 bytes
- 10,000 orders: ~1.2MB

---

## Further Reading

- [API Documentation](https://docs.rs/orderbook-rs)
- [Examples Directory](../examples/README.md)
- [GitHub Repository](https://github.com/joaquinbejar/OrderBook-rs)
- [Performance Analysis](../README.md#performance-analysis)

---

## Support

For issues, questions, or contributions:
- GitHub Issues: https://github.com/joaquinbejar/OrderBook-rs/issues
- Email: jb@taunais.com

---

**Version:** 0.15.2  
**Last Updated:** November 2026  
**License:** MIT
