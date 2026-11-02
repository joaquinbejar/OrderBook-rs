# Tail-latency benchmarks

This document covers the **HDR-histogram** bench suite added in 0.7.0
under `benches/order_book/*_hdr.rs`. The default Criterion benches in
the same directory remain — they publish HTML reports to
`target/criterion/` and report a mean-centric statistical comparison
that Criterion does well (see `BENCHMARKS.md`). The HDR benches are the
source of truth for the **tail** numbers (`p50` / `p99` / `p99.9` /
`p99.99`) that tier-one electronic exchanges quote in SLOs.

## Allocation profile (feature `alloc-counters`)

Under the `alloc-counters` feature the crate exposes a
`CountingAllocator<Inner: GlobalAlloc>` wrapper that tracks
`allocs` / `deallocs` / `bytes_allocated` / `bytes_deallocated` as
`AtomicU64` counters. Bench / test binaries opt in via:

```rust
use orderbook_rs::CountingAllocator;
use std::alloc::System;

#[global_allocator]
static A: CountingAllocator<System> = CountingAllocator::new(System);
```

`benches/order_book/alloc_count.rs` runs the same mixed 70 / 20 / 10
workload as `mixed_70_20_10_hdr` but reports `allocs_per_op` and
`bytes_alloc/op` over the measurement window (200 000 warmup +
1 000 000 measured). A reference run on the M4 Max host (orderbook-rs
0.12.0, `pricelevel` 0.9.1):

| counter        | value         |
|----------------|---------------|
| allocs         | 18 234 168    |
| deallocs       | 18 123 830    |
| bytes_alloc    | 6 173 361 482 |
| bytes_dealloc  | 6 145 419 658 |
| **allocs/op**  | **18.23**     |
| bytes_alloc/op | 6 173         |

Both counters are balanced (allocs ≈ deallocs, no leak). `allocs/op` is
the headline number for "what does the matching engine cost in alloc
pressure on a realistic workload" — useful as a regression signal much
more than as an absolute target; it is in the same ballpark as the
0.7.0 (`~17.76`) and 0.9.0 (`~18.81`) references and is
workload-randomness-sensitive on this synthetic stream (repeat runs
land in the `~15–19` range). `bytes_alloc/op` (`~6 KB`) is likewise
stable across 0.9.0 → 0.12.0 — the pricelevel 0.9 hardening and the
0.12.0 atomicity work added no allocation pressure.

> **Fixed in `pricelevel` 0.8.3 (PriceLevel#106).** Earlier `pricelevel`
> 0.8.2 pre-sized each match `MatchResult` to the *whole* level depth, so
> a qty-1 market order against a deep level allocated a multi-MB transient
> buffer (`bytes_alloc/op` ballooned to `~790 KB`). 0.8.3 bounds the
> pre-allocation to `min(incoming_quantity, order_count)`, so a small
> taker no longer reserves the level — `bytes_alloc/op` is back to the
> low-KB range.

### 0.14.0 allocation profile (#259)

Three runs per point on the host in "Run conditions" below;
`allocs/op` / `bytes_alloc/op`. "before" is `main` at `5447250`
(pricelevel 0.10.1, all 0.14.0 correctness work merged), "after" is the
0.14.0 candidate with the two #259 fixes. A range is the spread over the
three runs.

| scenario | before | after |
|---|---|---|
| `alloc_count_mixed_70_20_10` | 16.5 to 32.4 / 9.4 to 9.8 KB | 3.35 / 3.3 KB |
| `alloc_count_add_only_one_level_with_user` | 6.29 / 18.3 KB | 3.28 / 0.95 KB |
| `alloc_count_add_only_one_level_no_user` | 6.28 / 18.2 KB | 3.28 / 0.94 KB |
| `alloc_count_add_only_distinct_levels_with_user` | 8.14 / 18.3 KB | 8.14 / 18.4 KB |
| `alloc_count_cross_one_level_full_fill` | 18.0 to 128.0 / 1.4 to 4.1 KB | 3.02 / 1.07 KB |
| `alloc_count_cross_one_level_partial_fill` | 4.00 / 1.2 KB | 4.00 / 1.2 KB |
| `alloc_count_cross_deep_level_large_taker` | 4.00 / 88.3 KB | 4.00 / 88.3 KB |
| `alloc_count_market_sweep_one_level` | 44.0 to 119.0 / 1.2 to 3.0 KB | 2.03 / 0.24 KB |
| `alloc_count_market_sweep_three_levels` | 209 to 248 / 6.3 to 7.2 KB | 8.10 / 1.4 KB |

- **Fill path (fix 1).** Each fully filled maker was removed from the
  `user_orders` index by `untrack_order_by_id`, a `DashMap::iter_mut`
  scan over every user entry: O(active users) per filled maker, one
  shard-guard allocation per shard visited, and a visit count set by the
  per-process hash seed, hence the 18-to-128 swing. The order-location
  index now carries the owner and the fill path untracks by key.
- **Passive add (fix 2).** The "~18 KB and ~6 allocations per passive
  add" lead (#262) was `SkipMap::get_or_insert(price,
  Arc::new(PriceLevel::new(price)))` in `rest_on_level`: the value is
  evaluated eagerly, so every add to an existing level built and dropped
  a whole `PriceLevel` (its order map's DashMap shard array is 16 KB on
  an 18-core host, plus the level and its statistics). Fixed with
  `get_or_insert_with`. What remains per passive add is the order's two
  `Arc<OrderType>` (the level's copy and the one returned to the caller)
  and one order-queue skiplist node.
- **Left in pricelevel (filed upstream as
  [PriceLevel#224](https://github.com/joaquinbejar/PriceLevel/issues/224)
  and [PriceLevel#225](https://github.com/joaquinbejar/PriceLevel/issues/225)).** A NEW
  level still costs about 17.4 KB (`distinct_levels`: 16 KB of it is the
  per-level `DashMap::new()` shard array, sized `4 × available
  parallelism` shards rounded up to a power of two). A crossing taker
  pre-sizes its `MatchResult` to `min(taker qty, level order count)`
  trades and ids even when one large maker absorbs it
  (`cross_deep_level_large_taker`: 72 KB of `Trade` + 16 KB of ids for
  one trade).

The integration test `tests/alloc_budget.rs` runs a smaller 10 000-op
slice and asserts `allocs/op` stays under a fixed ceiling to catch
order-of-magnitude regressions in CI.

Run yourself:

```bash
cargo bench --features alloc-counters --bench alloc_count
cargo test  --features alloc-counters alloc_budget
```

Per-run summaries land in `target/alloc-counters/<scenario>.md`.

## How to run

```bash
make bench-hdr                 # every _hdr bench binary, incl. stp_contention_hdr's 8 scenarios
cargo bench --bench mixed_70_20_10_hdr   # single scenario
cargo bench --bench stp_contention_hdr   # all 8 mode x thread-count scenarios in one binary
cargo bench --features special_orders --bench pending_stops_hdr   # pending trailing stops (#286)
```

Each bench writes its raw HDR histogram to
`target/bench-hdr/<scenario>.hgrm` (V2 format) for downstream HDR
plotters; the directory lives under `target/` and is gitignored.

## Methodology

- **Histogram resolution.** `Histogram::<u64>` sized for `1 ns` to `1 s`
  with three significant figures. Three sig-figs is enough to
  distinguish `p99 ≠ p99.9` an order of magnitude apart while staying
  memory-cheap (~80 KB per histogram).
- **Sample collection.** Each measured operation is wrapped in a closure
  passed to `record(...)`, which times the closure with
  `std::time::Instant::now()` (one call before, one after) and writes
  the elapsed-nanosecond value into the histogram. The closure result
  is consumed via `std::hint::black_box` to prevent dead-code
  elimination. Scenarios whose single-op cost is at or near the host's
  clock-tick resolution use `record_batch(...)` instead — see "Timing
  only the operation under test" below.
- **Warmup.** Long-running scenarios (`add_only`, `mixed_70_20_10`)
  discard 200 000 ops before the measurement window starts.
  Pre-loading scenarios (`cancel_only`, `aggressive_walk`,
  `notional_walk`, `mass_cancel_burst`, `stp_sweep`) seed the book in a
  non-measured loop instead.
- **Workload determinism.** All scenarios drive a self-contained
  xorshift PRNG seeded with `0xA5A5_A5A5_A5A5_A5A5`. Reproducing a run
  with the same code produces the same op stream, modulo concurrent
  scheduling jitter on the host.
- **Coordinated omission.** The bench loop is **closed-loop**: the
  driver waits for each engine call to return before issuing the next.
  Closed-loop measurements **systematically under-report** tail
  latencies that a real load generator would observe under saturation,
  because queueing delays that would build up under a fixed arrival
  rate never materialize. **The numbers below are pure service time —
  use them as a regression signal and a lower bound on the production
  tail, not as a production SLO.** Open-loop measurement (record
  `now - scheduled_arrival`, not `now - call_start`) is the right
  follow-up; tracked but not in the initial drop.
- **CPU pinning.** Optional. On Linux, `taskset -c <core> cargo bench
  --bench mixed_70_20_10_hdr` reduces variance from cross-core
  scheduling. On macOS the benches were run without pinning — see the
  run conditions block below.

### Timing only the operation under test (issue #258)

An audit of every Criterion and HDR bench in `benches/` found two
classes of methodology bug, both fixed in the same pass that landed
this section:

1. **Timed setup / teardown.** Several of the plain Criterion benches
   under `benches/order_book/*.rs` (not the `_hdr` files) built a fresh
   `OrderBook` — sometimes populated with dozens to thousands of
   orders — *inside* the timed `b.iter(...)` closure and let it drop
   there too, so the reported number was construction + N ops + Drop,
   not the N ops the bench claimed to measure
   (`add_orders.rs`, `match_orders.rs`, `update_orders.rs`,
   `mixed_operations.rs`). A few more (`snapshot.rs::restore_from_snapshot`,
   `replay.rs::journal_append`) built the *output* of the operation
   under test inside the timed closure and dropped it there instead.
   Every one of these now moves construction into an unmeasured
   `iter_batched_ref` / `iter_batched` `setup` closure and takes the
   value under test by `&mut` (or reuses one long-lived value across
   samples, for `restore_from_snapshot`, which replaces every level on
   `&self`) so the corresponding drop also lands after Criterion's
   `end()` call closes the measurement window — see each file's own
   `# Methodology (issue #258)` doc comment for the specific fix.
   `snapshot.rs`'s `create_snapshot` / `enriched_snapshot_*` and
   `replay.rs`'s `replay_from_journal` switched from plain `b.iter` to
   `b.iter_with_large_drop`, deferring the drop of a snapshot /
   replayed book that is not a trivial value at 10 000 orders.
   `mass_cancel.rs` already excluded setup correctly but consumed the
   populated book inside `routine`, dropping it — post-cancel, so
   cheap, but still timed — before Criterion's own `end()` call;
   switched to `iter_batched_ref`.
2. **Sub-tick single-op timing.** The HDR benches record one
   `Instant::now()` pair per operation. On this host class (Apple
   silicon) the clock's tick resolution is about 41.67 ns — at or above
   the actual cost of `cancel_only`, `aggressive_walk`, `notional_walk`,
   `thin_book_sweep` and the thin `reserve_sweep_nonauto` /
   `reserve_sweep_auto` scenarios (pre-fix `p50` values of 41-83 ns,
   several with *zero* run-to-run jitter — the tell that the number is
   the clock, not the operation). Below that floor, timing a single
   call measures `Instant::now()`'s own resolution. These scenarios now
   use `hdr_common::record_batch`, which wraps `BATCH` (`32`)
   invocations in one `Instant` pair and records the per-op average;
   the trade-off — the recorded value is an average over the batch, not
   that batch's own per-op tail — is documented on `record_batch` itself
   and in each affected file. `add_only`, `mixed_70_20_10`,
   `mass_cancel_burst`, `stp_sweep`, `stp_contention` and
   `reserve_sweep_dense_nonauto` / `reserve_sweep_mixed_*` all measure
   comfortably above the tick per single op (hundreds of ns to tens of
   us) and were left on single-op `record`, preserving full per-op tail
   fidelity.

A new `add_only_risk_hdr` scenario (§ below) and three new
`alloc_count_add_only_*` allocation scenarios (§ "Allocation profile"
above) were added in the same pass — see their own doc comments for
what they isolate.

### `add_only_risk` — passive limit entry with a `RiskConfig` installed

Every other scenario above runs on a book with no `RiskConfig` at all
(`RiskState` is `None`, so every pre-trade check is a no-op
`Option::is_none` branch — see `src/orderbook/risk.rs`'s module docs).
`add_only_risk_hdr` is `add_only_hdr` unchanged in every other respect
with a `RiskConfig` enabling all three checks
(`max_open_orders_per_account`, `max_notional_per_account`,
`price_band_bps`) at limits wide enough that this workload never trips
a rejection, isolating the fixed per-op cost of the risk gate
(counter lookups, the notional product, the price-band comparison)
from a rejection branch. Compare its `p50` / `p99` / `p99.9` / `p99.99`
directly against `add_only`'s above — the delta is the risk-admission
overhead on the passive-add path. Run: `cargo bench --bench
add_only_risk_hdr`.

### Cross-version comparison harness (issues #258 / #259)

`scripts/bench_compare.sh` builds a small, standalone bench crate
(`benches/compare/`, its own `[workspace]` — never a member of the main
crate's own workspace or touched by `cargo test --all-features` /
`cargo build --release` at the repo root) against two git refs, each in
its own detached worktree with its own `CARGO_TARGET_DIR`, and runs
both binaries for `--rounds` interleaved rounds (baseline, candidate,
baseline, candidate, ...; `--rounds 3` minimum for a real comparison —
see #259). `benches/compare/src/adapter.rs` isolates every
version-specific API detail behind `v0_13` / `head` Cargo features so
`workloads.rs` — the actual benchmark code — is byte-identical on both
sides. Since #259 it drives fourteen scenarios: `add_only`,
`cancel_only`, `aggressive_walk`, `mixed_70_20_10`, `thin_book_sweep`,
`mass_cancel_burst`, `stp_cancel_maker`, `snapshot_create_10k`,
`snapshot_restore_10k`, `replay_10k` and same-price, same-account adds
from 4 / 8 threads with and without no-op listeners
(`contended_add[_listeners]_{4,8}t`). The two feature arms share every
call except `create_snapshot` (a plain value on `v0.13.1`, a `Result`
on 0.14.0). Snapshot and replay outputs are returned from the timed
closure and dropped after the clock stops; `snapshot_restore_10k`
clones its package and builds the empty target book before the clock
starts.

**Pitfall found in PR review: don't give the compare crate its own
`pricelevel` dependency.** Every scenario attaches a `user_id`, which
needs a `pricelevel::Hash32` value. Depending on `pricelevel` directly
from `benches/compare/Cargo.toml` (even with a version range wide
enough to nominally cover both `0.9` and `0.10`) does not make Cargo
unify on the version `orderbook-rs`'s own path dependency already
resolves: a `0.9` → `0.10` bump is SemVer-*incompatible* under Cargo's
pre-1.0 rules, so Cargo resolves the compare crate's own edge to the
newest match independently of `orderbook-rs`'s transitive edge,
producing two incompatible copies of the `pricelevel` crate in one
build and an `E0308` on every `Hash32`-typed argument ("there are
multiple different versions of crate `pricelevel` in the dependency
graph"). `adapter::owner` returns a plain `[u8; 32]` instead, and
`add_limit_order_with_user` / `submit_market_order_with_user` convert
with `.into()` at the call site, so type inference resolves to
whichever `Hash32` the active `orderbook-rs` edge provides —
`From<[u8; 32]> for Hash32` is present in both `0.9.2` and `0.10.0`, so
this crate never needs to name `pricelevel::Hash32`, or depend on
`pricelevel` at all.

Each round's raw output plus a `summary.md` / `summary.csv` (median
`p50` and `p99` per side, round-to-round `p50` spread as a percentage,
deltas, verdict and verdict counts), a `load.csv` with the load average
before and after every run (`--max-load X` waits for the 1-minute load
to drop below `X` first)
and `system_info.md` (CPU, cores, RAM, OS, rustc, load average
before/after, each side's resolved `Cargo.lock`) land in a fresh
`bench-results/<UTC timestamp>/` directory at the repo root —
deliberately outside `benches/`, since `Cargo.toml`'s `include` list
ships `benches/**/*` in the published crate tarball and comparison
results never should be. `bench-results/` is gitignored outright (see
`.gitignore`); a maintainer who wants to publish a comparison's numbers
copies the `summary.md` table into `BENCH.md` / `BENCHMARKS.md` by
hand, the same way every existing table in these two files was
produced.

**Noise policy.** A row whose round-to-round spread exceeds 10
percentage points on either side is marked `NOISY` (inconclusive) —
never a pass, regardless of the delta — and must be re-measured (more
rounds, a quieter host) before drawing any conclusion. Otherwise the
verdict is per class: a median `p50` delta above +3 % (uncontended) or
+5 % (contended) is a `REGRESSION`, except on a single-op-timed row
(`timer: single`) whose `p50` moved by at most one clock tick
(41.67 ns), which is not a measured regression. A delta inside the
threshold is reported as "within threshold", never as "no change".
**Separation rule:** a row that is NOISY by spread but has at least 5
rounds per side with disjoint per-round p50 ranges is REGRESSION
(separated) when the candidate is slower beyond the threshold, and
FASTER (separated) when it is faster, counted apart from OK (full
separation of two 5-round samples from one distribution has a chance of
2 / C(10, 5), under 1 %). The summarizer applies every rule; the tables
in this file and `BENCHMARKS.md` are copied from its output. This mirrors the
`reserve_sweep_dense_nonauto` handling elsewhere in this document (nine
runs reported as a range, not a point estimate, because this scenario's
single-threaded p50 is bimodal across this host's performance/
efficiency core split).

Usage: `make bench-compare-refs ARGS="--quick --rounds 1"` for a fast
pipeline smoke test (tiny op counts, never a real measurement); `make
bench-compare-refs ARGS="--baseline v0.13.1 --candidate HEAD --rounds
5"` for a real comparison. `scripts/bench_compare.sh --help` documents
every flag.

## Run conditions for the numbers below

| Item | Value |
|---|---|
| Host | Apple M5 Max, 18 cores, 128 GiB, macOS 27.0 (Darwin 27.0.0, `arm64`) |
| Pinning | None |
| Toolchain | `rustc 1.98.1` (stable) |
| Profile | `--release` (Cargo `bench` profile = `release` clone) |
| `RUSTFLAGS` | unset |
| Allocator | system allocator |
| Date | 2026-09-29 |
| Crate version | `0.14.0` candidate (`pricelevel` `0.10.1`), branch `issue-259-performance` |
| Load average | 6.6 to 7.2 (1 min) during the run: a desktop host, not a bench rig |

The eight headline tables below were re-measured for 0.14.0 (#259); the
`stp_contention` and `reserve_sweep` tables keep the measurement context
stated in their own sections. Absolute numbers moved between hosts
(0.12.0 was measured on an M4 Max): compare versions with the
cross-version harness ("0.13.1 → 0.14.0 delta" below), never across
these tables and older ones.

## Headline numbers

All values in nanoseconds. **Closed-loop service time** — see
"Coordinated omission" above.

### `add_only` — limit submission into a tight band

200 000 warmup + 1 000 000 measured `submit_gtc` calls (random side,
price `99..=101`). Despite the historical "no crossings" label, the band
is tight on both sides, so a good share of these adds **cross** and
trade; the scenario measures limit entry as a whole, not a purely
passive add (`alloc_count_add_only_*` isolates that, see "Allocation
profile").

| Quantile | Latency (ns) |
|---|---|
| p50    | 750 |
| p99    | 37 055 |
| p99.9  | 81 727 |
| p99.99 | 164 607 |
| max    | 553 471 |

**Where the tail comes from.** The book grows monotonically across the
measurement window, so each insert must walk the `SkipMap` to the
right level. The dominant contributor at p99.99 is allocator jitter
when `Arc<PriceLevel>` allocations churn under the system allocator;
secondary is L2 cache misses on the price-side `SkipMap` when the
working set outgrows L1.

### `cancel_only` — pre-loaded book, sequential cancels

1 000 000 resting orders on a NON-crossing book (bids 900..=999, asks
1 001..=1 100, 4 096 owners), every one asserted resting, then all
cancelled in insertion order, 32 per `Instant` pair; every cancel is
asserted to hit and the removed orders are dropped after the clock.
Before the #259 PR review the book was seeded with the crossing
`submit_gtc` stream, which leaves about 11 % of the ids resting: the
historical 29 to 42 ns p50 was mostly the cancel-miss path.

| Quantile | Latency (ns) |
|---|---|
| p50    | 790 |
| p99    | 1 027 |
| p99.9  | 1 399 |
| p99.99 | 3 009 |
| max    | 145 663 |

**Where the time goes.** A hit cancel takes the shared submit gate and
the price's level stripe, removes the order from the level (its order
map and FIFO skiplist), then untracks it from the location, user and
risk indices, and removes the level when it empties (about 10 orders per
level here, so often). The user-index removal is a binary search on the
order's admission sequence in its owner's entry, so it no longer grows
with the number of orders that owner has resting; `cancel_single_owner`
measures a single owner with 100 000 resting orders.

### `aggressive_walk` — taker market orders sweep multi-level book

50 levels × 100 resting orders, then 100 000 aggressive buys with qty
`5..=20`, batched 32 per `Instant` pair; taker inputs are drawn before
each batch and results inspected after it. Since #259 the ladder is
re-seeded (unmeasured, between batches) whenever it could not cover a
full batch: before, it held about 27 500 lots and was empty after about
2 200 takers, so ~98 % of the samples (and the historical 42 ns p50)
timed a market order rejected by an empty book.

| Quantile | Latency (ns) |
|---|---|
| p50    | 2 573 |
| p99    | 6 395 |
| p99.9  | 6 935 |
| p99.99 | 7 599 |
| max    | 7 599 |

**Where the tail comes from.** The fill loop iterates per-order at
each level until the requested quantity is consumed; with makers of
1..=10 lots a taker usually fills 2 to 4 makers. The per-trade id is a
UUID v5 (SHA-1) from pricelevel's `UuidGenerator`, about a fifth of the
sweep's CPU time in a profile; the rest is the level's order queue,
the per-fill `MatchResult` bookkeeping and the filled-maker release.
Tail: sweeps that span a level boundary and drop an emptied
`Arc<PriceLevel>`.

### `notional_walk` — quote-notional market orders sweep multi-level book

50 levels × 100 resting orders pre-loaded, then 100 000 aggressive
notional buys with budgets `500..2000` quote ticks
(`match_market_order_by_amount` path) — same book shape as
`aggressive_walk` for direct comparison of the two sweep entry points.
Re-seeded like `aggressive_walk` since #259.

| Quantile | Latency (ns) |
|---|---|
| p50    | 2 031 |
| p99    | 6 091 |
| p99.9  | 6 795 |
| p99.99 | 6 999 |
| max    | 6 999 |

**Where the tail comes from.** Same fill loop as `aggressive_walk`
plus one `u128` divide per level (budget → per-level qty cap) and one
multiply per fill. A budget of `500..2000` quote ticks buys 5 to 20
lots at price 100 but fewer as the sweep walks up the ladder (prices
100 to 149), hence a lower median than `aggressive_walk`.

### `mixed_70_20_10` — 70 % submit, 20 % cancel, 10 % aggressive

200 000 warmup + 1 000 000 measured. The "realistic" headline number.

| Quantile | Latency (ns) |
|---|---|
| p50    | 542 |
| p99    | 15 671 |
| p99.9  | 26 799 |
| p99.99 | 44 255 |
| max    | 181 631 |

**Where the tail comes from.** Mix of all three previous tails. The
median tracks `add_only` (because submits are 70 % of the workload).
The p99.99 comes from rare aggressive sweeps that interact with
allocator returns released by recent cancels.

### `thin_book_sweep` — book near-empty, IOC probing

Refills 3 resting asks every 5 ops; 200 000 IOC buy probes with qty
`1..=20`.

| Quantile | Latency (ns) |
|---|---|
| p50    | 375 |
| p99    | 1 225 |
| p99.9  | 4 627 |
| p99.99 | 11 703 |
| max    | 21 183 |

**Where the tail comes from.** Most probes either fully fill the
small resting depth or partial-fill and short-circuit. The p99 is
shaped by the partial-fill-then-cancel-remainder bookkeeping; the max
is allocator jitter when the book transitions empty → non-empty.

### `mass_cancel_burst` — dense book, then `cancel_all_orders`

10 000 resting orders on a NON-crossing book (asserted) × 500 bursts.
Each measured sample is **one full burst**, not one cancel — an
operator-side wall-clock guard rather than a per-op tail; the
`MassCancelResult` is dropped after the clock. Before the #259 PR review
the book was seeded with the crossing `submit_gtc` stream, so much of it
had traded away before the cancel (the historical 20 to 40 µs).

| Quantile | Latency (ns) |
|---|---|
| p50    | 745 471 |
| p99    | 1 018 367 |
| p99.9  | 1 522 687 |
| p99.99 | 1 522 687 |
| max    | 1 522 687 |

**Where the time goes.** About 75 ns per resting order: the exclusive
submit gate, then per order the level removal, the order-state and
cancelled-id bookkeeping, and the index clears. The p99.9 / p99.99 / max
collapse to one value because only 500 samples are taken.

### `stp_sweep` — self-trade-prevention CancelMaker self-cross (added 0.9.0)

`OrderBook::with_stp_mode(.., CancelMaker)` seeded with 50 ask levels
(each one taker-owned sell + 8 other-maker sells); 100 000 measured
aggressive self-crossing market buys from the taker, each one hitting
the per-level STP scan + inline maker cancel (#107).

**Liquidity profile (fixed for #225).** The other-maker depth is not a
one-shot seed: before every measured op, the levels the sweep is
currently walking (the current best ask and the next few above it) are
topped back up to their seeded other-maker order count. Earlier
versions of this bench seeded the 8-per-level other-maker depth once,
up front, with no refill — foreign liquidity was gone within the first
few hundred of the 100 000 measured ops, so almost the entire
measured window was sweeping a book with nothing real left to fill
against. The numbers directly below predate that fix; they are kept
for the pricelevel-upgrade bisection narrative underneath, not as a
representative baseline. See "#225 gate-mode comparison" below for the
post-fix, sustained-liquidity numbers.

| Quantile | Latency (ns) |
|---|---|
| p50    | 2 751 |
| p99    | 5 127 |
| p99.9  | 21 679 |
| p99.99 | 43 807 |
| max    | 102 079 |

**Where the tail comes from.** Every measured op runs the per-level
self-trade scan and cancels the same-user maker inline over the pooled
snapshot buffer (#107, no per-level `Vec` allocation). The median is
the scan + single cancel + the validated re-seed of the taker order;
the tail is the rare sweep that touches several levels.

**Median shift in 0.12.0 (pricelevel 0.9).** The p50 moved from
`~291 ns` (pricelevel 0.8.4) to `~1.2 µs`. Bisection against the
pre-hardening baseline attributes the entire shift to the pricelevel
0.9 upgrade — validated admission (duplicate-id / counter-capacity /
topology checks on the re-seeded taker), the atomic
cancel-vs-partial-fill index re-key, and the seqlock'd execution
statistics all run on this scenario's per-op path. The 0.12.0
book-level atomicity work (#206–#211 + the FOK submit gate) added
nothing measurable on top — and tightened this scenario's tail
(p99.9 `14.3 µs → 5.5 µs`, p99.99 `26.3 µs → 9.5 µs` vs the
pre-stack midpoint). Correctness bought with median latency on the
STP self-cross path; every other scenario's median is unchanged.

**#225 gate-mode comparison — corrected.** #225 makes STP-active
submits (STP enabled, non-zero taker `user_id`) take the exclusive
side of the `submit_gate` `RwLock` instead of the shared side, so the
per-level STP scan and the fill it authorises see a consistent queue.
`stp_sweep` is single-threaded and uncontended, so it isolates the
fixed per-op cost of exclusive vs shared acquisition on this
workload's own submits, independent of any cross-thread contention —
see `stp_contention` below for the multi-threaded contention cost.

An earlier version of this table reported a `main` baseline slower
than the #225 branch built on top of it (p50 3 375 ns vs 2 835 ns) and
concluded the exclusive gate had no measurable cost. That baseline was
pulled from a contaminated worktree; the branch cannot legitimately run
faster than the unmodified code it branched from on the same
single-threaded, uncontended scenario. A clean bisect of
`stp_sweep_hdr` — two runs per point, medians in ns, same host,
`Cargo.lock` aligned, worktree rebuilt fresh at every point — replaces
it:

| point | p50 | p99 | p99.9 |
|---|---|---|---|
| `e7331f0` v0.12.1 | 1 188 | 4 981 | 6 939 |
| `f167327` +#221 (pre-#225) | 1 167 | 4 919 | 6 607 |
| `bffaf00` +#225 | 3 021 | 6 063 | 12 631 |
| `b821df2` +#226 | 3 168 | 6 835 | 15 023 |
| `8ba6511` +#232 | 3 396 | 7 357 | 15 359 |
| `1d8bef2` main 0.13.0 | 2 667 | 5 917 | 12 255 |

The step lands at #225 and nowhere else in this walk: p50 goes
`1 167 → 3 021` ns, p99.9 goes `6 607 → 12 631` ns; #226 and #232 move
the numbers a little further but do not repeat a step of that size, and
0.13.0 final settles a bit below the #232 point. On this
single-threaded, uncontended STP scenario the exclusive submit gate
roughly doubles both the median and the p99.9. That is the measured
price of the correctness fix #225 makes; the earlier "no measurable
cost" conclusion above is withdrawn as a measurement error, not
reproduced by this bisection.

### `stp_contention` — N-thread contention on one book, gate-mode comparison (added for #225)

`stp_contention_hdr` is the multi-threaded counterpart to `stp_sweep`:
`N` threads (1 / 2 / 4 / 8) share ONE `OrderBook<()>`, each running
50 000 closed-loop ops (own `Rng`, own histogram) released together on a
`Barrier`. Op mix per thread: 70% passive limit adds a few ticks off a
fixed mid (rest, never cross), 20% cancels of that thread's own resting
orders, 10% aggressive limit orders that cross the whole passive band in
one shot. Each thread owns one user id from an 8-id pool, so under
`CancelMaker` a thread's own aggressive crossings routinely hit its own
resting makers.

Two configurations run back to back on the same book geometry:
`STPMode::None` (baseline — every submit stays on the shared side of the
`submit_gate` `RwLock`) and `STPMode::CancelMaker` (#225 — STP-active
submits take the exclusive side instead). The `None` column is the
STP-disabled baseline and must not move across the #225 change. The
`None` versus `CancelMaker` gap measured on `main` is the intrinsic cost
of the per-level STP scan and its inline same-user cancels, not the
gate; the cost of the gate-mode change itself is isolated by comparing
`main` and the #225 branch at the same thread count under `CancelMaker`,
which is what the table below does. Reported per thread-count: the
merged (all threads) p50 / p99 / p99.9 plus aggregate throughput
(`ops/s`, wall clock from barrier release to last-thread-done).

Like every scenario in this suite, this is **closed-loop, per-thread
service time** — see "Coordinated omission" above; it under-reports the
queueing delay a saturated real load generator would see, and it adds
its own dimension (lock / structure contention across threads) that the
single-threaded scenarios cannot show at all.

| Threads | `None` p50 (main / #225) | `None` ops/s (main / #225) | `CancelMaker` p50 (main / #225) | `CancelMaker` ops/s (main / #225) |
|---|---|---|---|---|
| 1 | 958 / 917 ns | 378k / 389k | 750 / 791 ns | 899k / 902k |
| 2 | 1 250 / 1 250 ns | 599k / 613k | 1 208 / 1 000 ns | 1 036k / 601k |
| 4 | 1 583 / 1 625 ns | 873k / 879k | 1 459 / 6 251 ns | 1 105k / 271k |
| 8 | 2 417 / 2 459 ns | 861k / 1 031k | 2 333 / 17 423 ns | 1 048k / 214k |

Medians of three runs per side, same host and conditions as the
`stp_sweep` comparison above, `main` at `f167327`. The `None` column is
unchanged within noise, as required: an `STPMode::None` book never takes
the exclusive side. The single-threaded `CancelMaker` row moves only
slightly here (`750 → 791` ns); this is not evidence that the
uncontended exclusive acquisition is free — the dedicated single-thread
`stp_sweep` bisection above, where every measured op is STP-active,
puts its fixed per-op cost at roughly double the median. That earlier
"no measurable cost" reading of this row is withdrawn; the small delta
here reflects this scenario's own mix, not the true cost of the gate.
From two threads up the `CancelMaker` column carries the cost of #225 by
design: in this mix 80 % of the operations (identified passive adds and
IOC takers) are STP-relevant and now serialize through the exclusive
gate, so aggregate throughput drops by about 43 % at two threads, 74 %
at four and 80 % at eight, with the merged p50 rising in step. The
p99.9 / p99.99 columns of the merged histograms (in the `.hgrm` files)
move by a similar factor. Restoring submit concurrency on STP books
needs the per-level STP-aware match in pricelevel, tracked as a
follow-up; the book-level gate is the correctness fix.

### `reserve_sweep` — IOC probes into reserve makers, strandable vs replenishing (added for #230)

`reserve_sweep_hdr` measures `capture_strandable_makers` (#230) and
`strandable_makers_resting`, an exact `AtomicUsize` count of currently
resting non-auto-replenishing `ReserveOrder`s with hidden depth:
incremented on admission and on snapshot restore, decremented on
cancel, mass cancel, expiry, STP removal and fill. Each sweep in
`match_order_inner` reads the count once, before any level is touched.
On a book where it reads zero, that single relaxed atomic load is the
sweep's entire cost: no pool buffer is acquired,
`capture_strandable_makers` is never called for any level, and the
post-sweep drain does no lookup. While the count is greater than zero,
every matching-capable submit and every cancel-then-add re-price runs
under the exclusive submit gate instead of the shared side, and
admitting a new strandable reserve is itself always exclusive, in
every `STPMode`, so no strandable maker can be admitted, cancelled or
replaced while a sweep is capturing against the count it read; that is
what keeps a sweep's capture attribution exact. Only when the count is
positive does each matched level get checked, and, if it still holds
hidden depth, walked with `PriceLevel::iter_orders()` to record which
resting non-auto reserves have hidden quantity behind them, so a sweep
can report the hidden depth it strands when `pricelevel` drops a
depleted maker's hidden tranche instead of refreshing it. `iter_orders`
is `DashMap::iter` upstream, which read-locks every shard of the map
regardless of how few orders rest at the level, so the walk is not
free on any level holding hidden depth. `main` has none of this
machinery at all, so on `main` every scenario below is just its plain
matching workload.

Same book geometry as `thin_book_sweep`: 3 resting asks refilled every
5 ops (not timed), 200 000 IOC buy probes with qty `1..=20` against
them. The only difference from `thin_book_sweep` is that the resting
side is `OrderType::ReserveOrder` (visible `1..=5`, hidden `4..=12`,
`replenish_threshold: 0`) instead of plain limits. Five scenarios, run
back to back on fresh books:

- `reserve_sweep_nonauto` (`auto_replenish: false`): every resting
  maker is strandable, so the gate opens on the first rest and every
  level-match that still holds hidden depth pays the `iter_orders()`
  walk. This is the scenario `capture_strandable_makers` adds cost to.
- `reserve_sweep_auto` (`auto_replenish: true`): hidden depth still
  rests and is still consumed, but nothing is strandable, so this book
  never opens the gate. Each sweep pays the one gate load and nothing
  else; `capture_strandable_makers` is never called.
- `reserve_sweep_dense_nonauto`: one price level (100) holding 64
  non-auto reserve makers (visible `1..=2`, hidden `4..=12`), refilled
  back to 64 whenever the level is fully consumed (not timed). IOC buy
  probes qty `16..=96`, large enough that one probe routinely strands
  several makers from that single level in one sweep. Isolates the
  capture pass over a dense level and the post-sweep drain, which
  looks up every filled maker against the captured list; both scale
  with how many makers a single probe strands, not with how many
  levels a sweep visits.
- `reserve_sweep_mixed_armed`: one non-auto reserve (10 visible, 20
  hidden) rests once at admission, far above every probe price, so it
  is never touched and the gate stays open for the whole run. The rest
  of the book runs the `thin_book_sweep` geometry with `IcebergOrder`
  resting makers in place of `ReserveOrder` ones. An iceberg holds
  hidden depth but can never match `capture_strandable_makers`'s
  `ReserveOrder` pattern, so every sweep pays the `iter_orders()` walk
  on a level holding hidden depth without ever finding anything
  strandable there: the pure cost of an open gate on levels that were
  never going to report anything. Because `main` has no gate at all,
  `reserve_sweep_mixed_armed` on `main` is just the iceberg-maker
  version of `thin_book_sweep`, with no armed order and no walk.
- `reserve_sweep_mixed_disarmed`: identical setup to
  `reserve_sweep_mixed_armed`, except the arming maker is cancelled
  right after resting, before the first probe. The cancel decrements
  `strandable_makers_resting` back to zero, closing the gate before the
  measured loop starts, so every sweep for the rest of the run pays
  the same single relaxed atomic load as `reserve_sweep_auto` and
  never calls `capture_strandable_makers`. This is the case the
  maintainer asked to be exercised, since leaving a maker resting at a
  distant price, as `reserve_sweep_mixed_armed` does, never closes the
  gate at all.

**`reserve_sweep_nonauto`** (`auto_replenish: false`; every resting
maker is strandable, so the capture runs on the branch):

| Quantile | `main` (no #230) | branch (#230) |
|---|---|---|
| p50    | 83 ns [83..83] | 83 ns [83..83] |
| p99    | 4 335 ns [4 001..6 167] | 7 043 ns [5 711..8 711] |
| p99.9  | 5 711 ns [5 003..29 759] | 10 295 ns [8 295..20 127] |
| p99.99 | 12 543 ns [6 087..79 423] | 21 583 ns [11 255..40 191] |

**`reserve_sweep_auto`** (`auto_replenish: true`; hidden depth rests
but nothing is strandable, so `strandable_makers_resting` stays zero):

| Quantile | `main` (no #230) | branch (#230) |
|---|---|---|
| p50    | 833 ns [666..1 166] | 958 ns [708..959] |
| p99    | 3 793 ns [3 541..12 375] | 3 917 ns [3 541..4 583] |
| p99.9  | 5 503 ns [4 543..32 175] | 5 419 ns [4 711..8 543] |
| p99.99 | 13 503 ns [6 167..92 799] | 14 591 ns [6 459..21 135] |

**`reserve_sweep_dense_nonauto`** (one level, 64 non-auto reserve
makers, probes `16..=96`):

| Quantile | `main` (no #230) | branch (#230) |
|---|---|---|
| p50    | 21 375 ns [8 295..24 879] | 15 127 ns [12 047..27 967] |
| p99    | 56 543 ns [22 127..158 079] | 37 151 ns [30 047..70 783] |
| p99.9  | 68 223 ns [40 127..1 936 383] | 64 319 ns [41 631..80 127] |
| p99.99 | 110 271 ns [88 063..36 143 103] | 106 303 ns [73 343..221 567] |

**`reserve_sweep_mixed_armed`** (one strandable maker rests at
`ARMING_PRICE` for the whole run; icebergs at `99..=101` do the
matching):

| Quantile | `main` (no #230) | branch (#230) |
|---|---|---|
| p50    | 1 167 ns [958..1 375] | 1 167 ns [1 000..1 334] |
| p99    | 6 003 ns [4 875..6 627] | 5 751 ns [5 083..6 751] |
| p99.9  | 8 543 ns [7 335..14 671] | 7 795 ns [6 627..20 015] |
| p99.99 | 21 375 ns [9 335..510 719] | 21 135 ns [10 295..140 159] |

**`reserve_sweep_mixed_disarmed`** (same setup, but the arming maker
is cancelled before the first probe):

| Quantile | `main` (no #230) | branch (#230) |
|---|---|---|
| p50    | 1 166 ns [959..1 417] | 1 167 ns [958..1 416] |
| p99    | 5 711 ns [4 959..7 003] | 5 835 ns [4 751..6 711] |
| p99.9  | 8 215 ns [6 751..11 463] | 8 127 ns [6 127..11 007] |
| p99.99 | 22 047 ns [7 875..54 975] | 20 719 ns [9 255..38 815] |

Medians of nine interleaved runs per side, `main` at `b821df2`
(`orderbook-rs` 0.12.1) against this branch (`orderbook-rs` 0.13.0),
both on `pricelevel` 0.9.1 with `Cargo.lock` aligned, same host and
method as the rest of this document; full run range in brackets. p50
on the microsecond-range scenarios (`reserve_sweep_auto`,
`reserve_sweep_dense_nonauto`, `reserve_sweep_mixed_armed`,
`reserve_sweep_mixed_disarmed`) is bimodal run to run on this host: 12
performance cores plus 4 efficiency cores, and a single-threaded run
lands on either core type, so its per-op cost shifts with it. That is
why nine runs are reported here instead of three, why medians carry
their full range instead of a single figure, and why no single-run
number is quoted for these scenarios. `reserve_sweep_dense_nonauto`'s
`main` range additionally has two extreme single-sample outliers, one
run's p99.9 at 1.9 ms and another's p99.99 at 36.1 ms against medians
in the tens of microseconds; that is one worst sample in nine runs of
200 000 probes each, consistent with ordinary host scheduling jitter
on a dense, many-order level, not a systematic effect.

**What the comparison shows.** In `reserve_sweep_nonauto`, where the
capture runs, the branch adds roughly 2.7 µs at p99 (4 335 ns →
7 043 ns) and roughly 4.6 µs at p99.9 (5 711 ns → 10 295 ns) over
`main`: on a book holding strandable makers, every matching-capable
submit now runs under the exclusive submit gate in addition to paying
the shard-locked `DashMap` capture pass, and `iter_orders` read-locks
every shard regardless of how few orders rest there. `reserve_sweep_auto`,
`reserve_sweep_mixed_armed` and `reserve_sweep_mixed_disarmed` are all
unchanged between `main` and the branch within their run-to-run range:
`auto` never rests a strandable maker, so the count stays zero
throughout; `mixed_disarmed` cancels its one strandable maker before
the first probe, confirming the count closes the gate again once the
last strandable maker is gone; `mixed_armed` keeps its one maker
resting for the whole run, so the branch pays the walk on every
iceberg level throughout, but that cost does not separate from
`main`'s no-gate baseline at this sample size. `reserve_sweep_dense_nonauto`
measured faster on the branch at p50 and p99 (21 375 ns → 15 127 ns
and 56 543 ns → 37 151 ns); the added admission-time counting and
exclusive-gate work cannot explain a branch that is faster than `main`,
so no improvement is claimed here, only that the dense, many-maker
level is not slower.

Separately, `reserve_sweep_auto`'s p50 (roughly 833-958 ns across the
two sides) sits well above `reserve_sweep_nonauto`'s (83 ns) on both
`main` and the branch: a non-auto reserve maker is fully consumed and
removed after one or two probes and the book then sits empty until
the next refill, while an auto-replenishing maker keeps refilling from
hidden and stays matchable across most of the refill window, so more
of the 200 000 probes do real matching work against it. `main` shows
the same gap, so this is a `pricelevel` matching-cost difference
between the two reserve behaviours, not anything #230 adds; it is why
each scenario is compared against its own `main` baseline above rather
than against another scenario.

`reserve_sweep_mixed_armed` and `reserve_sweep_mixed_disarmed` land
within noise of each other on the branch too (p50 1 167 ns vs
1 167 ns, p99 5 751 ns vs 5 835 ns): on this thin, iceberg-heavy
workload the wasted walk `mixed_armed` pays is too small relative to
run-to-run noise to separate from the zero-cost closed-gate path
`mixed_disarmed` takes. `reserve_sweep_dense_nonauto` above, where the
same walk runs against up to 64 makers instead of 3 thin icebergs, is
the clearer window onto its absolute cost.

Like every scenario in this suite, this is **closed-loop, per-probe
service time**: see "Coordinated omission" above; it under-reports the
queueing delay a saturated real load generator would see.

### `pending_stops` — books holding pending trailing stops (added for #286)

Quantiles a row's sample count cannot resolve are `n/a` (the HDR
report's rule: p99 needs at least 1 000 samples, p99.9 10 000, p99.99
100 000). The quiet rows take 3 125 batched samples (32 ops each), the
trailing rows 50 000 and 5 000, the election and cascade rows 5 000 (10
stops) and 200 (1 000 stops).

`pending_stops_hdr` (needs `--features special_orders`; `make bench-hdr`
runs it with that feature) measures what pending off-book trailing stops cost
the call that trades. `N` is the number of pending sell stops. With a
stop pending, every call that can trade runs under the exclusive
submit gate and, if it traded, evaluates the stops along its price
path before it returns; trailing re-keys every stop the print moves
(`O(N log N)`), so the trailing and election rows grow linearly in `N`
by construction.

- `pending_stops_quiet_{0,10,1000}`: market buys of 1 at 1000 that
  neither trail nor elect (stop 500, watermark 1000); `_0` is the
  control with the same setup. Batched, 32 ops per clock pair.
- `pending_stops_trailing_{10,1000}`: every market buy prints a new
  high and trails all `N` stops. One clock pair per op.
- `pending_stops_elect_{10,1000}`: one market sell elects all `N`
  stops at one print; each runs its market order into a deep bid. The
  book is re-seeded between samples, untimed.
- `pending_stops_cascade_{10,1000}`: one market sell elects the first
  of `N` stops on a ladder of one-lot bids; each stop's sale elects the
  next. Re-seeded between samples, untimed.

One run, host as in "Run conditions" but loaded (1-minute load average
10 to 13 during the run), so read the tails as indicative; values in ns:

| scenario | samples | p50 | p99 | p99.9 | p99.99 |
|---|---|---|---|---|---|
| `pending_stops_quiet_0` | 3 125 | 325 | 466 | n/a | n/a |
| `pending_stops_quiet_10` | 3 125 | 397 | 579 | n/a | n/a |
| `pending_stops_quiet_1000` | 3 125 | 398 | 532 | n/a | n/a |
| `pending_stops_trailing_10` | 50 000 | 2 209 | 3 251 | 8 335 | n/a |
| `pending_stops_trailing_1000` | 5 000 | 220 287 | 243 199 | n/a | n/a |
| `pending_stops_elect_10` | 5 000 | 6 335 | 7 375 | n/a | n/a |
| `pending_stops_elect_1000` | 200 | 628 735 | n/a | n/a | n/a |
| `pending_stops_cascade_10` | 5 000 | 10 047 | 13 007 | n/a | n/a |
| `pending_stops_cascade_1000` | 200 | 1 880 063 | n/a | n/a | n/a |

A quiet print costs about 70 ns more with stops pending than without,
independent of `N` (the exclusive gate, recording the path, and the
evaluation at its first and last print, which reads only the head of
each side's ordered stop maps when nothing moves). Trailing
costs about 220 ns per stop moved, and an election about 630 ns per
stop including its market order's own match. The Criterion group
"OrderBook - Pending Stops" in `benches/order_book/pending_stops.rs`
carries the same shapes, plus `concurrent_add_limit_orders_{0,1}_stops`
and `concurrent_post_only_adds_{0,1}_stops` at 2 and 4 threads: with
one stop pending, limit adds (which can trade) serialize on the
exclusive gate, while post-only adds stay on the shared side and match
their `_0_stops` control.

### `pending_stops` with a protection collar (added for #302)

`pending_stops_hdr` also runs the election and cascade shapes with a
`StopProtection` on the book, so each elected stop's child is an
immediate-or-cancel limit order instead of a market order:

- `pending_stops_elect_collar_{10,1000}`: as `pending_stops_elect_*`
  with a collar of 10; every child still fills at the deep bid (900,
  inside its band), so the work matches the uncollared row.
- `pending_stops_cascade_collar_{10,1000}`: as `pending_stops_cascade_*`
  with a collar of 1; each child fills the next one-lot bid (its limit),
  and the last child finds nothing within its band
  (`Cancelled { StopProtectionBand }`) instead of trading the deep bid.

Run conditions: 2026-09-30 11:32 UTC, Apple M5 Max (18 cores, 128 GiB,
macOS 27.0, `arm64`), `rustc 1.98.1`, no pinning, system allocator,
crate `0.15.0` (branch `issue-302-stop-protection-collar`), 1-minute load
average 6.7 before and 6.9 after the run (a desktop host, not a bench
rig). Command: `cargo bench --features special_orders --bench
pending_stops_hdr`. One run; the uncollared rows are from the same run,
as the paired control. Quantiles the sample count cannot resolve print
`n/a` (the HDR report's rule: p99 needs at least 1 000 samples, p99.9
10 000, p99.99 100 000). Values in ns:

| scenario | samples | p50 | p99 | p99.9 | p99.99 |
|---|---|---|---|---|---|
| `pending_stops_elect_10` | 5 000 | 6 375 | 7 375 | n/a | n/a |
| `pending_stops_elect_collar_10` | 5 000 | 6 503 | 14 919 | n/a | n/a |
| `pending_stops_elect_1000` | 200 | 633 343 | n/a | n/a | n/a |
| `pending_stops_elect_collar_1000` | 200 | 657 919 | n/a | n/a | n/a |
| `pending_stops_cascade_10` | 5 000 | 10 919 | 43 583 | n/a | n/a |
| `pending_stops_cascade_collar_10` | 5 000 | 9 879 | 13 167 | n/a | n/a |
| `pending_stops_cascade_1000` | 200 | 1 530 879 | n/a | n/a | n/a |
| `pending_stops_cascade_collar_1000` | 200 | 1 368 063 | n/a | n/a | n/a |

Interpretation: the collar adds no measurable cost to an election. It is
one `Copy` read and one checked add or subtract per elected stop, plus
one best-price cache read when a collared child leaves a remainder. The
elect medians differ by 2 % (10 stops) and 4 % (1 000), within this
host's run-to-run spread. The collared cascade is cheaper at the median
because its last child trades nothing. The p99 gaps (elect 7.4 vs
14.9 µs, cascade 43.6 vs 13.2 µs) point in opposite directions and are
single-run tail noise on a loaded host, not an effect of the collar.
Paths without a pending stop never read the collar: `quiet_0`,
`thin_book_sweep` and `aggressive_walk` stayed within noise of `v0.14.0`
in a paired comparison on the same host (two rounds each).

## 0.11.0 → 0.12.0 delta

The 0.12.0 release combines the pricelevel 0.9 hardening upgrade with
the book-level atomicity work (#206–#211). A three-point bisection
(0.11.0 / pricelevel 0.8.4 → post-upgrade midpoint → 0.12.0 final) on
the same host and session attributes the differences:

- **Medians unchanged** on `add_only` (917 = 917), `cancel_only`
  (41 = 41), `aggressive_walk` / `notional_walk` / `thin_book_sweep`
  (42 = 42). `mixed_70_20_10` p50 `792 → 833` (+41 ns) arrived with the
  pricelevel upgrade, not with the atomicity work; the FOK submit gate's
  uncontended read acquisition is not measurable on any scenario in a
  clean back-to-back run.
- **`stp_sweep` p50 `291 → 1 208`** — entirely from pricelevel 0.9's
  hardening (see the scenario note above); the 0.12.0 stack tightened
  its tail instead.
- **Improvements:** `mass_cancel_burst` p50 `43.6 µs → 32.6 µs` on the
  same session (−25 %), `add_only` p99 `−9 %`, `thin_book_sweep` p99
  `−17 %`.
- **Allocation profile flat:** `18.23 allocs/op`, `~6.2 KB/op` — within
  the historical `15–19` band.

## 0.12.1 → 0.13.0 delta

Medians of three interleaved runs per side, `v0.12.1` worktree versus
`main` at `1d8bef2` (0.13.0 final), same host (Apple M-series, 12
performance + 4 efficiency cores, macOS Darwin 25.6.0, `arm64`),
release profile, `Cargo.lock` aligned, `pricelevel` `0.9.1` on both
sides. All values in ns.

| scenario | 0.12.1 (p50 / p99 / p99.9) | 0.13.0 (p50 / p99 / p99.9) |
|---|---|---|
| `add_only` | 1 083 / 70 015 / 110 463 | 1 084 / 69 311 / 111 231 |
| `cancel_only` | 42 / 20 799 / 27 087 | 42 / 20 927 / 26 639 |
| `aggressive_walk` | 42 / 4 291 / 8 631 | 42 / 4 335 / 8 711 |
| `mass_cancel_burst` | 40 959 / 100 735 / 160 895 | 39 935 / 101 311 / 116 351 |
| `mixed_70_20_10` | 916 / 32 463 / 54 399 | 917 / 34 559 / 57 695 |
| `thin_book_sweep` | 42 / 4 875 / 7 127 | 83 / 4 667 / 6 543 |
| `notional_walk` | 42 / 3 167 / 5 959 | 42 / 3 793 / 7 375 |
| `stp_sweep` | 1 166 / 4 875 / 6 875 | 2 959 / 5 419 / 12 671 |

- **Medians unchanged** on every scenario except `stp_sweep` and, at
  p50 only, `thin_book_sweep`.
- **`stp_sweep` p50 `1 166 → 2 959`, p99.9 `6 875 → 12 671`** is #225's
  exclusive submit gate, isolated by the commit-by-commit bisection in
  the `stp_sweep` scenario section above; it is a real, attributable
  step, not noise.
- **`thin_book_sweep` p50 `42 → 83`** moves one HDR histogram bucket.
  The same bisection method finds no step at any single commit across
  this release for this scenario; on a host where a single-threaded run
  can land on either a performance or an efficiency core, a bimodal p50
  like this can appear on its own between two runs with no code change
  involved. Not attributed to any commit.
- **`notional_walk` p99 / p99.9 read about 20 % higher** in this
  pairing, but a commit-by-commit walk across the release shows no step
  at any single commit (p99 wanders `4 271 → 3 396 → 3 064 → 3 480 →
  4 043 → 4 376` ns across the intermediate points) — tail noise on
  this scenario, not a regression.
- **`mass_cancel_burst` p99.9 reads 28 % lower on 0.13.0.** Not claimed
  as an improvement; this scenario's tail is noisy run to run.
- **`mixed_70_20_10` p99 / p99.9 read about 6 % higher** on 0.13.0,
  within this scenario's own run-to-run spread.

## 0.13.1 → 0.14.0 delta

`scripts/bench_compare.sh --baseline v0.13.1 --candidate b8e2337 --rounds 5`,
measured 2026-09-30 against the release head (`main` at `b8e2337`: every
0.14.0 change, #239 through #295, including #286 off-book trailing
stops): the same `benches/compare` source built in two worktrees with
separate `CARGO_TARGET_DIR`s, five interleaved rounds, all fourteen
scenarios in one session, host and toolchain as in "Run conditions"
above, load average 4.1 to 6.7 (1 min) throughout; `pricelevel` resolves
to `0.9.2` on `v0.13.1` and `0.10.2` on the candidate. Both `Cargo.lock`
files, the per-round JSON, the load log and the generated summary are in
[`doc/bench/0.14.0/`](doc/bench/0.14.0/). Values are the median over the
five rounds of each round's p50, in ns; spread is the round-to-round
`(max - min) / median` of p50. Table and verdicts are the summarizer's
output, unedited.

This supersedes an earlier 2026-09-29 comparison whose candidate,
branch `issue-259-performance` at `55efa0b`, predates #286, #291, #294
and #295: it was not the release head, and a table that carried five of
its rows forward next to a partial nine-scenario re-measurement was not
evidence about `b8e2337` either. This is one complete run against the
actual release head.

| scenario | class / timer | v0.13.1 p50 (spread) | 0.14.0 p50 (spread) | p50 Δ | p99 Δ | verdict |
|---|---|---|---|---|---|---|
| `add_only` | uncontended / single | 1 167 (7.2 pp) | 667 (12.4 pp) | -42.8 % | -51.3 % | FASTER (separated) |
| `aggressive_walk` | uncontended / batch | 2 885 (57.5 pp) | 2 695 (22.1 pp) | -6.6 % | +37.3 % | NOISY |
| `cancel_only` | uncontended / batch | 907 (19.5 pp) | 848 (25.4 pp) | -6.5 % | -34.1 % | NOISY |
| `contended_add_4t` | contended / batch | 2 293 (5.1 pp) | 1 377 (7.3 pp) | -39.9 % | -39.2 % | OK |
| `contended_add_8t` | contended / batch | 3 611 (6.0 pp) | 3 447 (3.4 pp) | -4.5 % | +16.7 % | OK |
| `contended_add_listeners_4t` | contended / batch | 2 119 (57.5 pp) | 1 575 (5.0 pp) | -25.7 % | -13.6 % | FASTER (separated) |
| `contended_add_listeners_8t` | contended / batch | 3 873 (7.7 pp) | 4 511 (3.9 pp) | +16.5 % | +5.5 % | REGRESSION |
| `mass_cancel_burst` | uncontended / single | 885 247 (23.4 pp) | 529 407 (7.1 pp) | -40.2 % | -30.4 % | FASTER (separated) |
| `mixed_70_20_10` | uncontended / single | 1 042 (4.0 pp) | 542 (15.3 pp) | -48.0 % | -51.8 % | FASTER (separated) |
| `replay_10k` | uncontended / single | 10 354 687 (20.7 pp) | 4 202 495 (5.1 pp) | -59.4 % | -56.1 % | FASTER (separated) |
| `snapshot_create_10k` | uncontended / single | 402 431 (8.8 pp) | 198 655 (4.6 pp) | -50.6 % | -57.5 % | OK |
| `snapshot_restore_10k` | uncontended / single | 11 829 247 (4.2 pp) | 3 942 399 (4.8 pp) | -66.7 % | -68.7 % | OK |
| `stp_cancel_maker` | uncontended / single | 3 543 (52.9 pp) | 1 125 (7.4 pp) | -68.3 % | -56.8 % | FASTER (separated) |
| `thin_book_sweep` | uncontended / batch | 508 (101.8 pp) | 300 (5.7 pp) | -40.9 % | -41.9 % | NOISY |

Counts (generated): 4 OK, 6 FASTER (separated), 3 NOISY, 1 REGRESSION.

- **The two #259 fixes** (see "0.14.0 allocation profile" above) still
  carry most of the improvement: measured alone against `main` at
  `5447250` (5 interleaved rounds each, before the harness fixes and
  before #286/#291/#294/#295), the fill-path fix moved `stp_cancel_maker`
  2 959 → 2 042 ns and `thin_book_sweep` 833 → 350 ns, and the
  passive-add fix moved `add_only` 1 084 → 625 ns, `mixed_70_20_10`
  958 → 500 ns, `contended_add_4t` 2 321 → 1 347 ns and
  `contended_add_8t` 4 111 → 3 381 ns. Snapshot restore and replay were
  already faster on `main` (pricelevel 0.10's restore path). This
  release-head run reproduces the same order of magnitude on every one
  of those rows, so the later changes (#286, #291, #294, #295) do not
  erode the gain.
- **`contended_add_listeners_8t` +16.5 %, REGRESSION, accepted by the
  maintainer** ([decision on
  #259](https://github.com/joaquinbejar/OrderBook-rs/issues/259#issuecomment-5896324957)).
  Every candidate round (4 379 to 4 555 ns) reads above every baseline
  round (3 675 to 3 973 ns), a real, separated difference on the release
  head. Attributed to #247's level stripes and #249's ordered listener
  outbox (bisection below): every thread adds at one price for one
  account and the listener does nothing, so the lock handoffs are the
  whole difference; this scenario never has a pending stop, so #286 is
  not the cause. The listener-free `contended_add_8t` is 4.5 % faster
  than v0.13.1 on this same run.
  - Bisected on an earlier, busier host (five rounds per point):
    `v0.13.1` 4 053 → `898a825` 4 207 → #247 level stripes (`4a21d2b`)
    4 391 → `c59d74f` 4 539 → #249 ordered outbox (`4567530`) 4 999 →
    `main` 4 899 → 0.14.0 4 643 ns. The two steps are the ones accepted
    in #247 (+4.9 %) and #249 (+6.7 % at 8 threads); the exact
    percentage moves with host load, but the direction and the
    attributed cause hold across every measurement taken so far.
- **`snapshot_create_10k` is no longer a regression: -50.6 %, OK.** The
  2026-09-29 comparison's +6.2 % was measured against the pre-#286
  partial candidate and does not hold on the release head. `pricelevel`
  resolves to `0.10.2` here (one patch above the `0.10.1` that earlier
  comparison saw); `pricelevel` 0.10.2 fixed the default per-level
  `DashMap` shard count (`PriceLevel#224`, a host-independent 32 shards
  instead of four times the logical core count) and bounded
  `match_order`'s up-front trade reservation (`PriceLevel#225`), both
  cited under "0.14.0 allocation profile" above as open costs; either is
  a plausible cause of a faster snapshot walk, though this comparison
  did not bisect it commit by commit. The earlier bisection against
  `c1a4cbb` (372 → 398 µs, inside `PriceLevel::snapshot`) explains the
  regression that existed on `pricelevel` 0.10.1, not the number
  reported here.
- **NOISY** (`aggressive_walk`, `cancel_only`, `thin_book_sweep`): every
  row's baseline spread exceeds the 10 pp threshold this run
  (`aggressive_walk` 2 285 to 3 945 ns, `cancel_only` 854 to 1 031 ns,
  `thin_book_sweep` 308 to 825 ns); for `aggressive_walk` and
  `cancel_only` the candidate's spread does too (2 563 to 3 159 ns and
  786 to 1 001 ns). The two sides' ranges overlap for all three, so none
  separates. Not attributed to a specific cause on this run; the earlier
  hypothesis (v0.13.1's per-process hash-seed dependent untracked-maker
  scan) does not explain a candidate spread this wide and is not
  repeated here without support from this run's data.
- **FASTER (separated)** (`add_only`, `contended_add_listeners_4t`,
  `mass_cancel_burst`, `mixed_70_20_10`, `replay_10k`,
  `stp_cancel_maker`): despite one side's spread exceeding 10 pp, every
  candidate round is faster than every baseline round, so the
  separation rule (five rounds per side, disjoint ranges) reads the
  direction as conclusive.

## Limitations

- **macOS, no pinning.** The host above is a workstation, not a
  performance-tuned bench rig. Tail numbers will be tighter on a
  Linux host with `isolcpus=` + `nohz_full=` + a pinned thread, with
  the system allocator swapped for `jemalloc` or `mimalloc`.
- **Closed-loop only.** As called out under Methodology — these
  numbers are pure service time, not load-induced tail. Open-loop
  measurement is the next iteration of this suite.
- **Single-threaded driver, except `stp_contention`.** Every scenario but
  `stp_contention` issues one op at a time from a single thread.
  `stp_contention` (added for #225) is the first multi-writer scenario in
  this suite — up to 8 threads sharing one book — but it only exercises
  the STP gate-mode comparison, not the other seven workloads; a general
  multi-writer driver for the rest is deferred to a follow-up.

## Reproducing

```bash
git checkout main
make bench-hdr
cat target/bench-hdr/*.hgrm     # raw histograms
```

`hgrm` files are V2 format — readable by `HdrHistogram` plot tooling
or convertible via `hdrhistogram`'s `Reader`.
