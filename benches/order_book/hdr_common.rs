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

/// Print a fixed-format summary block to stdout. Matches what
/// `BENCH.md` quotes: scenario, sample count, p50/p99/p99.9/p99.99,
/// min, max — all in nanoseconds.
pub fn report(name: &str, h: &Histogram<u64>) {
    println!("scenario     : {name}");
    println!("samples      : {}", h.len());
    println!("p50    (ns)  : {}", h.value_at_quantile(0.50));
    println!("p99    (ns)  : {}", h.value_at_quantile(0.99));
    println!("p99.9  (ns)  : {}", h.value_at_quantile(0.999));
    println!("p99.99 (ns)  : {}", h.value_at_quantile(0.9999));
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
