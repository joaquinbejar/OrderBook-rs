// benches/order_book/hdr_common.rs
//
// Shared helpers for the `_hdr` bench binaries (issue #56).
//
// The Criterion benches coexist unchanged under the same directory.
// These helpers exist so each `_hdr` bench binary can record per-sample
// nanosecond latencies into an `hdrhistogram::Histogram` and emit a
// stable p50 / p99 / p99.9 / p99.99 + max table to stdout.

#![allow(dead_code)]

use hdrhistogram::Histogram;
use std::time::Instant;

use orderbook_rs::OrderBook;
use pricelevel::{Hash32, Id, Side, TimeInForce};

/// Histogram sized for `1 ns .. 1 s` with three significant figures of
/// resolution. Three sig-figs is enough to distinguish p99 ≠ p99.9 when
/// they're an order of magnitude apart while staying memory-cheap.
pub fn new_histogram() -> Histogram<u64> {
    Histogram::<u64>::new_with_bounds(1, 1_000_000_000, 3).expect("hist bounds")
}

/// Record one closure invocation's wall-clock duration into `h`.
///
/// Uses `std::hint::black_box` on the closure result to prevent
/// dead-code elimination of the observed work.
///
/// # When NOT to use this (issue #258)
///
/// `record` pays one `Instant::now()` pair per call. On this host class
/// (Apple silicon) the monotonic clock's tick resolution is about
/// 41.67 ns — measurably close to, or below, the cost of some of this
/// suite's single operations (`cancel_only`, `aggressive_walk`,
/// `notional_walk`, `thin_book_sweep`, the thin `reserve_sweep_*`
/// scenarios). Below that floor `record` does not measure the
/// operation, it measures the clock: the reported histogram is a
/// quantization artifact (a p50 that lands exactly on `41` or `83` ns
/// with zero jitter run to run is the tell). Use [`record_batch`]
/// instead for any scenario whose per-op cost is at or near the host's
/// tick resolution.
#[inline(always)]
pub fn record<F, R>(h: &mut Histogram<u64>, f: F) -> R
where
    F: FnOnce() -> R,
{
    let t0 = Instant::now();
    let r = std::hint::black_box(f());
    let elapsed = t0.elapsed().as_nanos() as u64;
    // hdrhistogram refuses zero — clamp at 1ns. Non-issue for matching
    // operations that always exceed a few hundred ns.
    h.record(elapsed.max(1)).expect("record");
    r
}

/// Time a batch of `k` closure invocations with a *single* `Instant`
/// pair and record the per-op average (`elapsed_ns / k`) into `h`
/// (issue #258).
///
/// `f` is called `k` times, indexed `0..k`, so the caller can vary the
/// op per call (e.g. cancel a different pre-loaded id each time). Use
/// this instead of [`record`] whenever a single invocation of the
/// operation under test is at or below the host's clock-tick
/// resolution (about 42 ns on Apple silicon): a per-op `Instant::now()`
/// pair at that scale reports the tick, not the operation. Batching
/// amortizes the timer call over `k` ops so the reported value tracks
/// the operation's real cost.
///
/// # Trade-off
///
/// The value recorded is a per-op *average over the batch*, not that
/// batch's own per-op tail — a slow outlier among the `k` calls is
/// smeared across the average rather than surfacing as its own sample.
/// This is a deliberate, documented loss of single-op tail fidelity in
/// exchange for a real (non-quantized) measurement; see `BENCH.md`
/// "Methodology" for which scenarios this applies to and why. Prefer
/// [`record`] whenever the op is comfortably above the tick (hundreds
/// of ns or more) so the tail stays op-level.
#[inline(always)]
pub fn record_batch<F>(h: &mut Histogram<u64>, k: u64, mut f: F)
where
    F: FnMut(u64),
{
    debug_assert!(k > 0, "batch size must be positive");
    let t0 = Instant::now();
    for i in 0..k {
        f(i);
        std::hint::black_box(());
    }
    let elapsed = t0.elapsed().as_nanos() as u64;
    let per_op = elapsed.checked_div(k).unwrap_or(elapsed).max(1);
    h.record(per_op).expect("record");
}

/// Print a fixed-format summary block to stdout. Matches what
/// `BENCH.md` quotes: scenario, sample count, p50/p99/p99.9/p99.99,
/// min, max — all in nanoseconds.
///
/// A quantile is printed only when the histogram holds enough samples to
/// resolve it (at least 10 samples beyond it: 1,000 for p99, 10,000 for
/// p99.9, 100,000 for p99.99); otherwise it prints `n/a`, so a low-sample
/// row never reports its maximum as a tail quantile.
pub fn report(name: &str, h: &Histogram<u64>) {
    let quantile = |q: f64, min_samples: u64| -> String {
        if h.len() >= min_samples {
            h.value_at_quantile(q).to_string()
        } else {
            format!("n/a ({} samples)", h.len())
        }
    };
    println!("scenario     : {name}");
    println!("samples      : {}", h.len());
    println!("p50    (ns)  : {}", h.value_at_quantile(0.50));
    println!("p99    (ns)  : {}", quantile(0.99, 1_000));
    println!("p99.9  (ns)  : {}", quantile(0.999, 10_000));
    println!("p99.99 (ns)  : {}", quantile(0.9999, 100_000));
    println!("min    (ns)  : {}", h.min());
    println!("max    (ns)  : {}", h.max());
}

/// Persist the raw histogram to `target/bench-hdr/<name>.hgrm` (V2
/// format) for downstream HDR plotters. `target/` is gitignored.
pub fn persist(name: &str, h: &Histogram<u64>) -> std::io::Result<()> {
    use hdrhistogram::serialization::{Serializer, V2Serializer};
    std::fs::create_dir_all("target/bench-hdr")?;
    let path = format!("target/bench-hdr/{name}.hgrm");
    let mut file = std::fs::File::create(&path)?;
    V2Serializer::new()
        .serialize(h, &mut file)
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    eprintln!("wrote {path}");
    Ok(())
}

/// Tiny deterministic xorshift PRNG. Self-contained so no `rand`
/// dependency creeps into the dev-dep tree just for benches.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }

    #[inline]
    pub fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    #[inline]
    pub fn range(&mut self, lo: u64, hi: u64) -> u64 {
        debug_assert!(lo <= hi);
        let span = hi - lo + 1;
        lo + (self.next() % span)
    }
}

/// Common cross-bench constants. Tight price band forces frequent
/// crossings on the aggressive bench; the small owner pool keeps
/// per-account bookkeeping non-trivial without ballooning state.
pub const PRICE_LO: u64 = 99;
pub const PRICE_HI: u64 = 101;
pub const QTY_LO: u64 = 1;
pub const QTY_HI: u64 = 100;
pub const OWNERS: u8 = 4;

pub fn owner(byte: u8) -> Hash32 {
    let mut bytes = [0u8; 32];
    bytes[0] = byte;
    Hash32::new(bytes)
}

/// Produce a fresh `OrderBook` with no listeners and no risk gating —
/// the bench measures the engine itself, not the publisher pipeline.
pub fn fresh_book() -> OrderBook<()> {
    OrderBook::<()>::new("BENCH")
}

/// Side picker that yields `Buy` / `Sell` 50/50 from the rng.
#[inline]
pub fn pick_side(rng: &mut Rng) -> Side {
    if rng.next().is_multiple_of(2) {
        Side::Buy
    } else {
        Side::Sell
    }
}

/// Picker for owner ids: yields one of `[1, 2, 3, 4]` byte-tagged
/// `Hash32` accounts.
#[inline]
pub fn pick_owner(rng: &mut Rng) -> Hash32 {
    owner(((rng.next() % OWNERS as u64) as u8) + 1)
}

/// Common GTC submit shape used by `add_only`, `mixed`, and the seed
/// phases of the other scenarios.
#[inline]
pub fn submit_gtc(book: &OrderBook<()>, rng: &mut Rng, id: u64) {
    let price = rng.range(PRICE_LO, PRICE_HI) as u128;
    let qty = rng.range(QTY_LO, QTY_HI);
    let _ = book.add_limit_order_with_user(
        Id::from_u64(id),
        price,
        qty,
        pick_side(rng),
        TimeInForce::Gtc,
        pick_owner(rng),
        None,
    );
}

/// Seed `n` NON-crossing resting orders (bids on 900..=999, asks on
/// 1 001..=1 100) with ids `first_id..`, spread over `owners` owners,
/// and assert that every one rests (#259 PR review). `submit_gtc`'s
/// tight band crosses, so a book seeded with it holds only a fraction of
/// its ids; a scenario that means to act on resting orders seeds here.
/// Many owners spread the book over many `user_orders` entries;
/// `cancel_single_owner_hdr` and `fill_single_owner_hdr` cover the
/// single-owner end.
pub fn seed_resting(book: &OrderBook<()>, rng: &mut Rng, first_id: u64, n: u64, owners: u16) {
    let before = book.get_all_orders().len();
    for i in 0..n {
        let side = pick_side(rng);
        let offset = rng.range(0, 99) as u128;
        let price = match side {
            Side::Buy => 900 + offset,
            Side::Sell => 1_001 + offset,
        };
        let qty = rng.range(QTY_LO, QTY_HI);
        let mut bytes = [0u8; 32];
        bytes[..2].copy_from_slice(&((rng.next() % u64::from(owners)) as u16).to_le_bytes());
        bytes[2] = 0x5A;
        book.add_limit_order_with_user(
            Id::from_u64(first_id + i),
            price,
            qty,
            side,
            TimeInForce::Gtc,
            Hash32::new(bytes),
            None,
        )
        .expect("seed order rests");
    }
    assert_eq!(
        book.get_all_orders().len() - before,
        n as usize,
        "every seeded order rests"
    );
}

/// Owners used by [`seed_resting`] callers.
pub const SEED_OWNERS: u16 = 4_096;
