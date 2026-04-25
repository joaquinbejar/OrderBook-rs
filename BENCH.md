# Tail-latency benchmarks

This document covers the **HDR-histogram** bench suite added in 0.7.0
under `benches/order_book/*_hdr.rs`. The default Criterion benches in
the same directory remain — they publish HTML reports to
`target/criterion/` and report a mean-centric statistical comparison
that Criterion does well. The HDR benches are the source of truth for
the **tail** numbers (`p50` / `p99` / `p99.9` / `p99.99`) that tier-one
electronic exchanges quote in SLOs.

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
1 000 000 measured). A reference run on the same M4 Max host:

| counter        | value         |
|----------------|---------------|
| allocs         | 17 757 222    |
| deallocs       | 17 690 635    |
| bytes_alloc    | 4 926 064 834 |
| bytes_dealloc  | 4 897 062 482 |
| **allocs/op**  | **17.76**     |
| bytes_alloc/op | 4 926         |

This is the headline number for "what does the matching engine cost
in alloc pressure on a realistic workload" — useful as a regression
signal much more than as an absolute target. The integration test
`tests/unit/alloc_budget_tests.rs` runs a smaller 10 000-op slice and
asserts `allocs/op < 10` to catch order-of-magnitude regressions in
CI.

Run yourself:

```bash
cargo bench --features alloc-counters --bench alloc_count
cargo test  --features alloc-counters alloc_budget
```

Per-run summaries land in `target/alloc-counters/<scenario>.md`.

## How to run

```bash
make bench-hdr                 # all six scenarios
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
  `mass_cancel_burst`) seed the book in a non-measured loop instead.
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
| Host | Apple M4 Max, macOS 25.4 (Darwin 25.4.0, `arm64`) |
| Pinning | None |
| Toolchain | `rustc 1.95.0` (stable) |
| Profile | `--release` (Cargo `bench` profile = `release` clone) |
| `RUSTFLAGS` | unset |
| Allocator | system allocator |
| Date | 2026-04-25 |
| Crate version | `0.7.0-unreleased` (commit on `issue-56-hdr-bench`) |

## Headline numbers

All values in nanoseconds. **Closed-loop service time** — see
"Coordinated omission" above.

### `add_only` — pure passive limit submission, no crossings

200 000 warmup + 1 000 000 measured.

| Quantile | Latency (ns) |
|---|---|
| p50    | 791 |
| p99    | 78 847 |
| p99.9  | 146 303 |
| p99.99 | 401 663 |
| max    | 528 895 |

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
| p50    | 42 |
| p99    | 25 167 |
| p99.9  | 34 047 |
| p99.99 | 172 031 |
| max    | 1 271 807 |

**Where the tail comes from.** `DashMap::remove` on the order index is
a shard-local lock acquisition; the median is dominated by that
single-cycle CAS path. The very long p99.99 / max tails reflect
shard-contention windows when multiple removals land on the same
shard back to back, plus rare allocator returns of large
`PriceLevel` linked-list nodes.

### `aggressive_walk` — taker market orders sweep multi-level book

50 levels × 100 resting orders pre-loaded, then 100 000 aggressive
buys with qty `5..=20`.

| Quantile | Latency (ns) |
|---|---|
| p50    | 41 |
| p99    | 7 083 |
| p99.9  | 16 959 |
| p99.99 | 33 823 |
| max    | 203 263 |

**Where the tail comes from.** The fill loop iterates per-order at
each level until the requested quantity is consumed. Median is fast
because most sweeps fill within a single level. Tail is driven by
sweeps that span multiple levels and drop several `Arc<PriceLevel>`s
at once.

### `mixed_70_20_10` — 70 % submit, 20 % cancel, 10 % aggressive

200 000 warmup + 1 000 000 measured. The "realistic" headline number.

| Quantile | Latency (ns) |
|---|---|
| p50    | 667 |
| p99    | 39 487 |
| p99.9  | 71 999 |
| p99.99 | 298 239 |
| max    | 644 607 |

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
| p99    | 5 711 |
| p99.9  | 15 127 |
| p99.99 | 50 431 |
| max    | 418 303 |

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
| p50    | 25 711 |
| p99    | 48 447 |
| p99.9  | 312 575 |
| p99.99 | 312 575 |
| max    | 312 575 |

**Where the tail comes from.** Burst latency scales linearly with the
book depth; on a tight host the median is ~26 µs to drain 10 000
orders, ~0.5 ns per order amortised. The p99.9 / p99.99 / max all
collapse to the same value because only 500 samples were taken — the
single worst-case observation dominates.

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
git checkout issue-56-hdr-bench  # or main once merged
make bench-hdr
cat target/bench-hdr/*.hgrm     # raw histograms
```

`hgrm` files are V2 format — readable by `HdrHistogram` plot tooling
or convertible via `hdrhistogram`'s `Reader`.
