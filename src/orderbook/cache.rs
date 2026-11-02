/******************************************************************************
   Author: Joaquín Béjar García
   Email: jb@taunais.com
   Date: 15/7/25
******************************************************************************/

//! Generation-tagged best bid / ask fast path (#309).
//!
//! Each side of the book owns a [`BestPriceSide`]: a generation counter and
//! a one-entry seqlock slot holding `(generation, price)`.
//!
//! - **Writers** bump the side's generation with `Release` **after** every
//!   insert or removal of a level key on that side has completed
//!   ([`BestPriceSide::bump`]). Only key changes move the best price
//!   (`best_bid` / `best_ask` return the first key), so adds to and
//!   removals from an existing level that keep it linked do not bump.
//! - **Readers** load the generation with `Acquire` **before** walking the
//!   skiplist ([`BestPriceSide::current`]), and publish the walked price
//!   tagged with that generation ([`BestPriceSide::publish`]).
//! - A **hit** ([`BestPriceSide::read`]) needs a torn-free seqlock read
//!   whose tag equals the side's current generation.
//!
//! A reader that walked before a mutation and publishes after it tags its
//! price with the pre-mutation generation, which the mutation's bump has
//! already retired, so that price can never be served again. A reader
//! that loads the post-bump generation synchronizes with the bump and so
//! walks a skiplist that already holds the mutation. This is what the
//! pre-#309 cache (a validity flag cleared before the mutation completed)
//! got wrong.
//!
//! Everything here is plain `AtomicU64` loads, stores and one CAS: lock
//! free on every target with 64-bit atomics. The previous cache stored the
//! price in a `crossbeam::atomic::AtomicCell<u128>`, which on stable Rust
//! falls back to a global striped spin lock.

use std::sync::atomic::{AtomicU64, Ordering, fence};

/// Mask of the low 64 bits of a price.
const LOW_MASK: u128 = u64::MAX as u128;

/// One side's generation counter and its published best price.
#[derive(Debug)]
pub(crate) struct BestPriceSide {
    /// Bumped after every level key insert or removal on this side. Starts
    /// at `1` so the slot's initial tag `0` never hits.
    generation: AtomicU64,
    /// Seqlock word: odd while a publisher writes the slot.
    seq: AtomicU64,
    /// Generation the slot's price was walked under.
    tag: AtomicU64,
    /// Low 64 bits of the published price.
    price_lo: AtomicU64,
    /// High 64 bits of the published price.
    price_hi: AtomicU64,
}

impl Default for BestPriceSide {
    fn default() -> Self {
        Self::new()
    }
}

impl BestPriceSide {
    /// A side with nothing published: every read misses until a reader
    /// publishes.
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            generation: AtomicU64::new(1),
            seq: AtomicU64::new(0),
            tag: AtomicU64::new(0),
            price_lo: AtomicU64::new(0),
            price_hi: AtomicU64::new(0),
        }
    }

    /// Retire every published price of this side. Call **after** the level
    /// key insert or removal has completed.
    ///
    /// The generation is a cache tag, not a protocol counter: `fetch_add`
    /// wraps, and a wrap would take 2^64 level changes, after which the only
    /// risk is a tag from 2^64 bumps ago, which the slot no longer holds.
    #[inline]
    pub(crate) fn bump(&self) {
        self.generation.fetch_add(1, Ordering::Release);
    }

    /// The side's current generation. Call **before** walking the
    /// skiplist, and pass the value to [`read`](Self::read) and
    /// [`publish`](Self::publish).
    #[inline]
    #[must_use]
    pub(crate) fn current(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    /// The published price, if it was walked under `generation` and the
    /// slot was not being written during the read.
    #[inline]
    #[must_use]
    pub(crate) fn read(&self, generation: u64) -> Option<u128> {
        let before = self.seq.load(Ordering::Acquire);
        if before & 1 == 1 {
            return None;
        }
        let tag = self.tag.load(Ordering::Relaxed);
        let lo = self.price_lo.load(Ordering::Relaxed);
        let hi = self.price_hi.load(Ordering::Relaxed);
        // Orders the slot loads above before the seq re-check below
        // (pairs with the `Release` fence in `publish`).
        fence(Ordering::Acquire);
        let after = self.seq.load(Ordering::Relaxed);
        if after != before || tag != generation {
            return None;
        }
        Some((u128::from(hi) << 64) | u128::from(lo))
    }

    /// Publish `price`, walked under `generation`. Concurrent publishers
    /// race on a CAS of the seq word; a loser, or a caller that finds a
    /// publish in progress, skips publishing (the next read misses and
    /// walks again).
    #[inline]
    pub(crate) fn publish(&self, generation: u64, price: u128) {
        let seq = self.seq.load(Ordering::Relaxed);
        if seq & 1 == 1 {
            return;
        }
        // `seq` is even here, so `seq + 1` never overflows; `seq + 2` does
        // only after 2^63 publishes, and then publishing just stops (every
        // read misses and walks), which is slower but never stale.
        let (Some(writing), Some(done)) = (seq.checked_add(1), seq.checked_add(2)) else {
            return;
        };
        if self
            .seq
            .compare_exchange(seq, writing, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            return;
        }
        // Orders the odd seq store above before the slot stores below
        // (pairs with the `Acquire` fence in `read`).
        fence(Ordering::Release);
        self.tag.store(generation, Ordering::Relaxed);
        self.price_lo.store(
            u64::try_from(price & LOW_MASK).unwrap_or(0),
            Ordering::Relaxed,
        );
        self.price_hi
            .store(u64::try_from(price >> 64).unwrap_or(0), Ordering::Relaxed);
        self.seq.store(done, Ordering::Release);
    }
}

/// The best bid / ask fast path of an [`OrderBook`](crate::OrderBook): one
/// [`BestPriceSide`] per side, so a mutation of one side never retires the
/// other side's published price.
///
/// Advisory and volatile: it is never serialized and never snapshotted, and
/// an empty side is never published (its read is an O(1) skiplist probe).
#[derive(Debug, Default)]
pub(crate) struct PriceLevelCache {
    /// Bid side.
    pub(crate) bids: BestPriceSide,
    /// Ask side.
    pub(crate) asks: BestPriceSide,
}

impl PriceLevelCache {
    /// Both sides empty and unpublished.
    #[must_use]
    pub(crate) fn new() -> Self {
        Self::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    #[test]
    fn test_default_side_misses() {
        let side = BestPriceSide::new();
        assert_eq!(side.read(side.current()), None);
    }

    #[test]
    fn test_publish_under_current_generation_hits() {
        let side = BestPriceSide::new();
        let generation = side.current();
        side.publish(generation, 100);
        assert_eq!(side.read(side.current()), Some(100));
    }

    #[test]
    fn test_bump_retires_the_published_price() {
        let side = BestPriceSide::new();
        side.publish(side.current(), 100);
        side.bump();
        assert_eq!(side.read(side.current()), None);
    }

    #[test]
    fn test_fill_published_under_an_old_generation_never_hits() {
        // A reader loads the generation, walks, and is preempted; a writer
        // mutates and bumps; the reader then publishes its stale walk. The
        // slot holds it, but under a retired tag.
        let side = BestPriceSide::new();
        let walked_under = side.current();
        side.bump();
        side.publish(walked_under, 9_880);
        assert_eq!(side.read(side.current()), None);
        // A fresh walk under the new generation replaces it and hits.
        side.publish(side.current(), 9_882);
        assert_eq!(side.read(side.current()), Some(9_882));
    }

    #[test]
    fn test_price_zero_and_high_prices_round_trip() {
        let side = BestPriceSide::new();
        side.publish(side.current(), 0);
        assert_eq!(side.read(side.current()), Some(0));
        let high = (1u128 << 127) + 5;
        side.publish(side.current(), high);
        assert_eq!(side.read(side.current()), Some(high));
        side.publish(side.current(), u128::MAX);
        assert_eq!(side.read(side.current()), Some(u128::MAX));
    }

    #[test]
    fn test_sides_are_independent() {
        let cache = PriceLevelCache::new();
        cache.bids.publish(cache.bids.current(), 100);
        cache.asks.publish(cache.asks.current(), 110);
        cache.asks.bump();
        assert_eq!(cache.bids.read(cache.bids.current()), Some(100));
        assert_eq!(cache.asks.read(cache.asks.current()), None);
    }

    #[test]
    fn test_concurrent_publishers_never_tear_a_price() {
        // Each publisher writes a price whose two halves are equal; a torn
        // read would mix halves of two prices. The reader runs until it has
        // seen enough hits (or a time bound), so a slow thread start cannot
        // leave it with nothing checked.
        const PUBLISHERS: u64 = 4;
        let side = Arc::new(BestPriceSide::new());
        let stop = Arc::new(AtomicBool::new(false));
        let start = Arc::new(std::sync::Barrier::new(5));
        let generation = side.current();
        let publishers: Vec<_> = (1..=PUBLISHERS)
            .map(|k| {
                let (side, stop, start) =
                    (Arc::clone(&side), Arc::clone(&stop), Arc::clone(&start));
                std::thread::spawn(move || {
                    start.wait();
                    let mut n = k;
                    while !stop.load(Ordering::Relaxed) {
                        let half = u128::from(n);
                        side.publish(generation, (half << 64) | half);
                        n = n.wrapping_add(PUBLISHERS);
                    }
                })
            })
            .collect();
        start.wait();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let (mut reads, mut hits) = (0u64, 0u64);
        while hits < 10_000 && std::time::Instant::now() < deadline {
            reads = reads.wrapping_add(1);
            if let Some(price) = side.read(generation) {
                assert_eq!(price >> 64, price & LOW_MASK, "torn read: {price:#x}");
                hits = hits.wrapping_add(1);
            }
        }
        stop.store(true, Ordering::Relaxed);
        for publisher in publishers {
            publisher.join().expect("publisher");
        }
        assert!(
            hits > 0,
            "the reader must hit at least once ({reads} reads)"
        );
    }

    // Book-level ordering: every level key change bumps its side's
    // generation after the skiplist reflects it, so a price walked before
    // the change and published after it never hits.

    use crate::OrderBook;
    use pricelevel::{Id, Side, TimeInForce};

    fn ask_book() -> OrderBook<()> {
        let book = OrderBook::<()>::new("CACHE");
        book.add_limit_order(
            Id::from_u64(1),
            9_882,
            10,
            Side::Sell,
            TimeInForce::Gtc,
            None,
        )
        .expect("resting ask");
        book.add_limit_order(
            Id::from_u64(2),
            9_000,
            10,
            Side::Buy,
            TimeInForce::Gtc,
            None,
        )
        .expect("resting bid");
        book
    }

    #[test]
    fn test_new_level_bumps_and_stale_publish_never_hits() {
        let book = ask_book();
        assert_eq!(book.best_ask(), Some(9_882));
        let walked_under = book.cache.asks.current();
        book.add_limit_order(
            Id::from_u64(3),
            9_880,
            1,
            Side::Sell,
            TimeInForce::Gtc,
            None,
        )
        .expect("better ask");
        assert_ne!(book.cache.asks.current(), walked_under, "a new level bumps");
        // A reader that walked before the add publishes after it.
        book.cache.asks.publish(walked_under, 9_882);
        assert_eq!(book.best_ask(), Some(9_880));
    }

    #[test]
    fn test_emptying_cancel_bumps_and_stale_publish_never_hits() {
        let book = ask_book();
        book.add_limit_order(
            Id::from_u64(3),
            9_880,
            1,
            Side::Sell,
            TimeInForce::Gtc,
            None,
        )
        .expect("better ask");
        assert_eq!(book.best_ask(), Some(9_880));
        let walked_under = book.cache.asks.current();
        book.cancel_order(Id::from_u64(3)).expect("cancel");
        assert_ne!(
            book.cache.asks.current(),
            walked_under,
            "a removed level bumps"
        );
        book.cache.asks.publish(walked_under, 9_880);
        assert_eq!(book.best_ask(), Some(9_882));
    }

    #[test]
    fn test_changes_inside_a_level_do_not_bump() {
        let book = ask_book();
        let before = book.cache.asks.current();
        book.add_limit_order(
            Id::from_u64(3),
            9_882,
            1,
            Side::Sell,
            TimeInForce::Gtc,
            None,
        )
        .expect("join the level");
        book.cancel_order(Id::from_u64(3)).expect("leave the level");
        assert_eq!(
            book.cache.asks.current(),
            before,
            "the level key never changed"
        );
        assert_eq!(book.best_ask(), Some(9_882));
        assert_eq!(book.cache.asks.read(book.cache.asks.current()), Some(9_882));
    }

    #[test]
    fn test_sweep_that_empties_the_opposite_level_bumps_that_side() {
        let book = ask_book();
        assert_eq!(book.best_ask(), Some(9_882));
        let walked_under = book.cache.asks.current();
        book.submit_market_order(Id::from_u64(3), 10, Side::Buy)
            .expect("sweep the ask");
        assert_ne!(book.cache.asks.current(), walked_under);
        book.cache.asks.publish(walked_under, 9_882);
        assert_eq!(book.best_ask(), None);
    }

    #[test]
    fn test_mass_cancel_and_restore_bump_both_sides() {
        let book = ask_book();
        let package = book.create_snapshot_package(usize::MAX).expect("package");
        assert_eq!(
            (book.best_bid(), book.best_ask()),
            (Some(9_000), Some(9_882))
        );
        let (bid_gen, ask_gen) = (book.cache.bids.current(), book.cache.asks.current());
        let cancelled = book.cancel_all_orders();
        assert_eq!(cancelled.cancelled_count(), 2);
        book.cache.bids.publish(bid_gen, 9_000);
        book.cache.asks.publish(ask_gen, 9_882);
        assert_eq!((book.best_bid(), book.best_ask()), (None, None));

        let mut restored = OrderBook::<()>::new("CACHE");
        assert_eq!(restored.best_ask(), None);
        let ask_gen = restored.cache.asks.current();
        restored
            .restore_from_snapshot_package(package)
            .expect("restore");
        assert_ne!(restored.cache.asks.current(), ask_gen);
        assert_eq!(
            (restored.best_bid(), restored.best_ask()),
            (Some(9_000), Some(9_882))
        );
    }

    #[test]
    fn test_join_cannot_complete_before_the_creators_bump() {
        // #309 review: writer A links a new best ask level and parks before
        // bumping the generation; writer B adds to that same level. B must
        // not return while A is parked, and once B returns best_ask() must
        // serve the new level, never the cached old best.
        use std::sync::mpsc;
        use std::time::Duration;

        let (parked_tx, parked_rx) = mpsc::channel::<()>();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let release_rx = std::sync::Mutex::new(release_rx);
        let fired = AtomicBool::new(false);
        let mut book = ask_book();
        book.level_create_hook = Some(Arc::new(move |price: u128| {
            if price == 9_880 && !fired.swap(true, Ordering::SeqCst) {
                parked_tx.send(()).expect("signal parked");
                release_rx
                    .lock()
                    .expect("release lock")
                    .recv()
                    .expect("release");
            }
        }));
        let book = Arc::new(book);
        assert_eq!(book.best_ask(), Some(9_882), "the old best is published");

        let creator = {
            let book = Arc::clone(&book);
            std::thread::spawn(move || {
                book.add_limit_order(
                    Id::from_u64(3),
                    9_880,
                    1,
                    Side::Sell,
                    TimeInForce::Gtc,
                    None,
                )
                .expect("creator add")
            })
        };
        parked_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("creator parked after linking");

        let joiner_done = Arc::new(AtomicBool::new(false));
        let joiner = {
            let (book, done) = (Arc::clone(&book), Arc::clone(&joiner_done));
            std::thread::spawn(move || {
                book.add_limit_order(
                    Id::from_u64(4),
                    9_880,
                    1,
                    Side::Sell,
                    TimeInForce::Gtc,
                    None,
                )
                .expect("joiner add");
                done.store(true, Ordering::SeqCst);
                book.best_ask()
            })
        };
        std::thread::sleep(Duration::from_millis(100));
        assert!(
            !joiner_done.load(Ordering::SeqCst),
            "the joiner returned while the creator had not bumped the generation"
        );

        release_tx.send(()).expect("release creator");
        creator.join().expect("creator thread");
        let seen_by_joiner = joiner.join().expect("joiner thread");
        assert_eq!(seen_by_joiner, Some(9_880));
        assert_eq!(book.best_ask(), Some(9_880));
    }
}
