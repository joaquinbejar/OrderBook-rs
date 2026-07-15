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
make bench-hdr                 # all eight scenarios
cargo bench --bench mixed_70_20_10_hdr   # single scenario
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
  elimination.
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

## Run conditions for the numbers below

| Item | Value |
|---|---|
| Host | Apple M4 Max, macOS (Darwin 25.5.0, `arm64`) |
| Pinning | None |
| Toolchain | `rustc 1.97.0` (stable) |
| Profile | `--release` (Cargo `bench` profile = `release` clone) |
| `RUSTFLAGS` | unset |
| Allocator | system allocator |
| Date | 2026-07-15 |
| Crate version | `0.12.0` (`pricelevel` `0.9.1`) |

## Headline numbers

All values in nanoseconds. **Closed-loop service time** — see
"Coordinated omission" above.

### `add_only` — pure passive limit submission, no crossings

200 000 warmup + 1 000 000 measured.

| Quantile | Latency (ns) |
|---|---|
| p50    | 917 |
| p99    | 62 847 |
| p99.9  | 97 727 |
| p99.99 | 130 495 |
| max    | 195 583 |

**Where the tail comes from.** The book grows monotonically across the
measurement window, so each insert must walk the `SkipMap` to the
right level. The dominant contributor at p99.99 is allocator jitter
when `Arc<PriceLevel>` allocations churn under the system allocator;
secondary is L2 cache misses on the price-side `SkipMap` when the
working set outgrows L1.

### `cancel_only` — pre-loaded book, sequential cancels

1 000 000 pre-loaded resting orders, all cancelled in order.

| Quantile | Latency (ns) |
|---|---|
| p50    | 41 |
| p99    | 19 007 |
| p99.9  | 24 047 |
| p99.99 | 27 055 |
| max    | 834 559 |

**Where the tail comes from.** `DashMap::remove` on the order index is
a shard-local lock acquisition; the median is dominated by that
single-cycle CAS path. The very long max tail reflects
shard-contention windows when multiple removals land on the same
shard back to back, plus rare allocator returns of large
`PriceLevel` linked-list nodes.

### `aggressive_walk` — taker market orders sweep multi-level book

50 levels × 100 resting orders pre-loaded, then 100 000 aggressive
buys with qty `5..=20`.

| Quantile | Latency (ns) |
|---|---|
| p50    | 42 |
| p99    | 3 335 |
| p99.9  | 6 795 |
| p99.99 | 8 839 |
| max    | 18 671 |

**Where the tail comes from.** The fill loop iterates per-order at
each level until the requested quantity is consumed. Median is fast
because most sweeps fill within a single level. Tail is driven by
sweeps that span multiple levels and drop several `Arc<PriceLevel>`s
at once.

### `notional_walk` — quote-notional market orders sweep multi-level book

50 levels × 100 resting orders pre-loaded, then 100 000 aggressive
notional buys with budgets `500..2000` quote ticks
(`match_market_order_by_amount` path) — same book shape as
`aggressive_walk` for direct comparison of the two sweep entry points.

| Quantile | Latency (ns) |
|---|---|
| p50    | 42 |
| p99    | 2 543 |
| p99.9  | 4 959 |
| p99.99 | 6 919 |
| max    | 18 463 |

**Where the tail comes from.** Same fill loop as `aggressive_walk`
plus one `u128` divide per level (budget → per-level qty cap) and one
multiply per fill. Both medians sit at the same `~42 ns`, confirming
the notional arithmetic is not the bottleneck; the tail tracks
multi-level walks exactly like the base-qty sweep.

### `mixed_70_20_10` — 70 % submit, 20 % cancel, 10 % aggressive

200 000 warmup + 1 000 000 measured. The "realistic" headline number.

| Quantile | Latency (ns) |
|---|---|
| p50    | 833 |
| p99    | 31 679 |
| p99.9  | 52 031 |
| p99.99 | 72 063 |
| max    | 128 511 |

**Where the tail comes from.** Mix of all three previous tails. The
median tracks `add_only` (because submits are 70 % of the workload).
The p99.99 comes from rare aggressive sweeps that interact with
allocator returns released by recent cancels.

### `thin_book_sweep` — book near-empty, IOC probing

Refills 3 resting asks every 5 ops; 200 000 IOC buy probes with qty
`1..=20`.

| Quantile | Latency (ns) |
|---|---|
| p50    | 42 |
| p99    | 4 543 |
| p99.9  | 5 667 |
| p99.99 | 12 751 |
| max    | 26 335 |

**Where the tail comes from.** Most probes either fully fill the
small resting depth or partial-fill and short-circuit. The p99 is
shaped by the partial-fill-then-cancel-remainder bookkeeping; the max
is allocator jitter when the book transitions empty → non-empty.

### `mass_cancel_burst` — dense book, then `cancel_all_orders`

10 000 orders pre-loaded × 500 bursts. Each measured sample is
**one full burst**, not one cancel — useful as an operator-side
wall-clock guard rather than a per-op tail.

| Quantile | Latency (ns) |
|---|---|
| p50    | 32 591 |
| p99    | 42 271 |
| p99.9  | 54 463 |
| p99.99 | 54 463 |
| max    | 54 463 |

**Where the tail comes from.** Burst latency scales linearly with the
book depth; on a tight host the median is ~19 µs to drain 10 000
orders, ~1.9 ns per order amortised. The p99.9 / p99.99 / max all
collapse to the same value because only 500 samples were taken — the
single worst-case observation dominates.

### `stp_sweep` — self-trade-prevention CancelMaker self-cross (added 0.9.0)

`OrderBook::with_stp_mode(.., CancelMaker)` seeded with 50 ask levels
(each one taker-owned sell + 8 other-maker sells); 100 000 measured
aggressive self-crossing market buys from the taker, each one hitting
the per-level STP scan + inline maker cancel (#107).

| Quantile | Latency (ns) |
|---|---|
| p50    | 1 208 |
| p99    | 4 835 |
| p99.9  | 5 543 |
| p99.99 | 9 503 |
| max    | 21 551 |

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

## Limitations

- **macOS, no pinning.** The host above is a workstation, not a
  performance-tuned bench rig. Tail numbers will be tighter on a
  Linux host with `isolcpus=` + `nohz_full=` + a pinned thread, with
  the system allocator swapped for `jemalloc` or `mimalloc`.
- **Closed-loop only.** As called out under Methodology — these
  numbers are pure service time, not load-induced tail. Open-loop
  measurement is the next iteration of this suite.
- **Single-threaded driver.** The benches issue one op at a time. A
  multi-writer driver would surface `DashMap` shard contention more
  visibly; deferred to a follow-up.

## Reproducing

```bash
git checkout main
make bench-hdr
cat target/bench-hdr/*.hgrm     # raw histograms
```

`hgrm` files are V2 format — readable by `HdrHistogram` plot tooling
or convertible via `hdrhistogram`'s `Reader`.
