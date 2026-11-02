//! The headline scenarios from `benches/order_book/*_hdr.rs` (plus a
//! snapshot, a replay and a contended scenario), reimplemented against
//! `crate::adapter` so the exact same source runs against two different
//! `orderbook-rs` checkouts (issues #258 / #259).
//!
//! Each scenario returns a `Histogram<u64>` of per-op nanosecond
//! latencies. Only the operation under test is timed (#259 PR review):
//!
//! - every op's inputs (ids, prices, quantities, sides, owners, the op
//!   kind of the mixed stream) are drawn **before** the clock starts;
//! - every op's output (the returned order, match result, cancel result
//!   or mass-cancel result) is kept and dropped **after** the clock
//!   stops ([`record`] returns it; [`record_batch_into`] pushes it into a
//!   buffer reserved before the clock, one `Vec::push` per op);
//! - seeding, refills and setup-time assertions run outside the clock.
//!
//! Scenarios whose single op is at or near the host clock tick (about
//! 41.67 ns on Apple silicon) use [`record_batch_into`], which times
//! `BATCH` ops per `Instant` pair and records the per-op average;
//! scenarios well above the tick use [`record`], one sample per op.

use hdrhistogram::Histogram;
use orderbook_rs::Id;
use orderbook_rs::Side;
use std::sync::{Arc, Barrier};
use std::time::Instant;

use crate::adapter::{
    Book, add_limit_order_with_user, cancel_all, cancel_by_user, cancel_order, create_snapshot,
    journal_of_adds, new_book, new_listener_book, new_stp_book, owner, replay, resting_orders,
    restore, snapshot_package, submit_market_order_with_user,
};
use crate::rng::Rng;

const BATCH: u64 = 32;
const SEED: u64 = 0xA5A5_A5A5_A5A5_A5A5;

pub fn new_histogram() -> Histogram<u64> {
    Histogram::<u64>::new_with_bounds(1, 10_000_000_000, 3).expect("hist bounds")
}

/// Times one op; its return value is handed back so the caller drops it
/// after the clock stopped.
fn record<R, F: FnOnce() -> R>(h: &mut Histogram<u64>, f: F) -> R {
    let t0 = Instant::now();
    let out = std::hint::black_box(f());
    let elapsed = t0.elapsed().as_nanos() as u64;
    h.record(elapsed.max(1)).expect("record");
    out
}

/// Times `k` ops in one `Instant` pair and records the per-op average.
/// Each op's output is pushed into `out` (capacity reserved before the
/// clock) and dropped by the caller after it; `out` is cleared first.
fn record_batch_into<R, F: FnMut(u64) -> R>(
    h: &mut Histogram<u64>,
    k: u64,
    out: &mut Vec<R>,
    mut f: F,
) {
    out.clear();
    out.reserve(k as usize);
    let t0 = Instant::now();
    for i in 0..k {
        out.push(std::hint::black_box(f(i)));
    }
    let elapsed = t0.elapsed().as_nanos() as u64;
    let per_op = elapsed.checked_div(k).unwrap_or(elapsed).max(1);
    h.record(per_op).expect("record");
}

/// Owner pool mirroring `hdr_common::OWNERS` / `pick_owner` — every
/// submit/cancel below picks one of these, never the userless path.
const OWNERS: u8 = 4;

fn pick_owner(rng: &mut Rng) -> [u8; 32] {
    owner(((rng.next() % OWNERS as u64) as u8) + 1)
}

/// One of `n` owners (`n <= 65 536`), for the non-crossing seeds.
fn wide_owner(rng: &mut Rng, n: u64) -> [u8; 32] {
    let mut bytes = [0u8; 32];
    let k = (rng.next() % n) as u16;
    bytes[..2].copy_from_slice(&k.to_le_bytes());
    bytes[2] = 0x5A;
    bytes
}

fn pick_side(rng: &mut Rng) -> Side {
    if rng.next().is_multiple_of(2) {
        Side::Buy
    } else {
        Side::Sell
    }
}

/// A fully drawn limit submission.
#[derive(Clone, Copy)]
struct Submit {
    id: u64,
    price: u128,
    qty: u64,
    side: Side,
    user: [u8; 32],
}

impl Submit {
    /// `hdr_common::submit_gtc`: a GTC limit at a random price in
    /// `99..=101` on a random side. The band is tight on purpose, so a
    /// good share of these CROSS the opposite side (not a passive add).
    fn gtc(rng: &mut Rng, id: u64) -> Self {
        let price = rng.range(99, 101) as u128;
        let qty = rng.range(1, 100);
        let side = pick_side(rng);
        let user = pick_owner(rng);
        Self {
            id,
            price,
            qty,
            side,
            user,
        }
    }

    #[inline]
    fn apply(self, book: &Book) -> impl Sized {
        add_limit_order_with_user(
            book,
            Id::from_u64(self.id),
            self.price,
            self.qty,
            self.side,
            self.user,
        )
    }
}

/// `add_only_hdr`: 200k warmup + N measured `submit_gtc` calls, one
/// sample per op (hundreds of ns, well above the tick). The measured
/// submissions are drawn before the measurement loop.
pub fn add_only(warmup_ops: u64, measured_ops: u64) -> Histogram<u64> {
    let book = new_book("BENCH");
    let mut rng = Rng::new(SEED);
    let mut hist = new_histogram();

    for i in 0..warmup_ops {
        drop(Submit::gtc(&mut rng, i + 1).apply(&book));
    }
    let ops: Vec<Submit> = (0..measured_ops)
        .map(|i| Submit::gtc(&mut rng, warmup_ops + i + 1))
        .collect();
    for op in &ops {
        let out = record(&mut hist, || op.apply(&book));
        drop(out);
    }
    hist
}

/// A non-crossing book of `orders` resting orders (bids on 900..=999,
/// asks on 1 001..=1 100) spread over `owners` owners, with ids
/// `first_id..`. Asserts every order rests.
fn dense_book(book: &Book, rng: &mut Rng, first_id: u64, orders: u64, owners: u64) -> Vec<Id> {
    let before = resting_orders(book);
    let mut ids = Vec::with_capacity(orders as usize);
    for i in 0..orders {
        let id = Id::from_u64(first_id + i);
        let side = pick_side(rng);
        let offset = rng.range(0, 99) as u128;
        let price = match side {
            Side::Buy => 900 + offset,
            Side::Sell => 1_001 + offset,
        };
        let qty = rng.range(1, 100);
        let user = wide_owner(rng, owners);
        drop(add_limit_order_with_user(book, id, price, qty, side, user));
        ids.push(id);
    }
    assert_eq!(
        resting_orders(book) - before,
        orders as usize,
        "seed must rest every order"
    );
    ids
}

/// Owners of the cancel / mass-cancel seeds, spreading the book over many
/// `user_orders` entries. Before the per-user index removal became a
/// binary search, a cancel shifted its owner's whole id list, so few
/// owners would have made that shift dominate; `cancel_single_owner`,
/// `fill_single_owner` and `mass_cancel_by_user` now measure that end.
const SEED_OWNERS: u64 = 4_096;

/// Pre-loaded NON-crossing book (every seeded order rests, asserted),
/// then every order cancelled in insertion order, batched (a single
/// cancel is near the tick). Before the #259 PR review this was seeded
/// with the crossing `submit_gtc` stream, which leaves about 11 % of the
/// ids resting, so most timed cancels were misses.
pub fn cancel_only(preload_ops: u64) -> Histogram<u64> {
    let book = new_book("BENCH");
    let mut rng = Rng::new(SEED);
    let mut hist = new_histogram();
    let ids = dense_book(&book, &mut rng, 1, preload_ops, SEED_OWNERS);

    let mut out = Vec::new();
    for chunk in ids.chunks(BATCH as usize) {
        record_batch_into(&mut hist, chunk.len() as u64, &mut out, |j| {
            cancel_order(&book, chunk[j as usize])
        });
        out.clear();
    }
    assert_eq!(resting_orders(&book), 0, "every cancel must hit");
    hist
}

/// `cancel_single_owner_hdr`: `cancel_only` with every order owned by
/// ONE user, cancelled in a seeded random order (shuffled before the
/// clock). The opposite end from `SEED_OWNERS`: a single market maker, or
/// anonymous flow (which shares `Hash32::zero()`), whose `user_orders`
/// entry holds every resting order.
pub fn cancel_single_owner(preload_ops: u64) -> Histogram<u64> {
    let book = new_book("BENCH");
    let mut rng = Rng::new(SEED);
    let mut hist = new_histogram();
    let mut ids = dense_book(&book, &mut rng, 1, preload_ops, 1);
    for i in (1..ids.len()).rev() {
        let j = rng.range(0, i as u64) as usize;
        ids.swap(i, j);
    }

    let mut out = Vec::new();
    for chunk in ids.chunks(BATCH as usize) {
        record_batch_into(&mut hist, chunk.len() as u64, &mut out, |j| {
            cancel_order(&book, chunk[j as usize])
        });
        out.clear();
    }
    assert_eq!(resting_orders(&book), 0, "every cancel must hit");
    hist
}

/// `fill_single_owner_hdr`: `preload_ops` one-lot asks at random prices,
/// every one owned by ONE maker, each consumed by a one-lot market buy, so
/// every timed op runs the filled-maker cleanup against that owner's
/// `user_orders` entry. One sample per op (a fill is well above the tick).
pub fn fill_single_owner(preload_ops: u64) -> Histogram<u64> {
    let book = new_book("BENCH");
    let mut rng = Rng::new(SEED);
    let mut hist = new_histogram();
    let maker = owner(0xAA);
    let taker = owner(0xBB);
    for id in 1..=preload_ops {
        let price = 1_001 + rng.range(0, 999) as u128;
        drop(add_limit_order_with_user(
            &book,
            Id::from_u64(id),
            price,
            1,
            Side::Sell,
            maker,
        ));
    }
    assert_eq!(
        resting_orders(&book),
        preload_ops as usize,
        "seed must rest every order"
    );

    for id in preload_ops + 1..=2 * preload_ops {
        let out = record(&mut hist, || {
            submit_market_order_with_user(&book, Id::from_u64(id), 1, Side::Buy, taker)
        });
        drop(out);
    }
    assert_eq!(resting_orders(&book), 0, "every maker must be filled");
    hist
}

/// Taker market buys sweep a 50-level x 100-order ask ladder, batched.
///
/// Differs from `aggressive_walk_hdr` before #259 on purpose: that bench
/// seeds the ladder once (about 27 500 lots) and then sends 100 000
/// takers of 5..=20 lots, so the ladder is empty after about 2 200 of
/// them and the remaining ~98 % of the samples time a market order
/// rejected by an empty book. Here the ladder is re-seeded (unmeasured,
/// between batches) whenever the resting quantity could not cover a
/// full batch of the largest taker, so every measured taker trades. The
/// taker quantities are drawn before each batch and the resting-quantity
/// bookkeeping runs after it.
pub fn aggressive_walk(
    resting_per_level: u64,
    num_levels: u64,
    measured_ops: u64,
) -> Histogram<u64> {
    let book = new_book("BENCH");
    let mut rng = Rng::new(SEED);
    let mut hist = new_histogram();
    let maker = owner(0xAA);
    let taker = owner(0xBB);
    let mut next_id = 1u64;
    let mut resting = 0u64;

    let seed = |rng: &mut Rng, next_id: &mut u64, resting: &mut u64| {
        for level in 0..num_levels {
            let price = (100 + level) as u128;
            for _ in 0..resting_per_level {
                let qty = rng.range(1, 10);
                drop(add_limit_order_with_user(
                    &book,
                    Id::from_u64(*next_id),
                    price,
                    qty,
                    Side::Sell,
                    maker,
                ));
                *next_id += 1;
                *resting += qty;
            }
        }
    };
    seed(&mut rng, &mut next_id, &mut resting);

    let mut out = Vec::new();
    let mut qtys = Vec::with_capacity(BATCH as usize);
    let mut done = 0u64;
    while done < measured_ops {
        let k = BATCH.min(measured_ops - done);
        if resting < k * 20 {
            seed(&mut rng, &mut next_id, &mut resting);
        }
        qtys.clear();
        qtys.extend((0..k).map(|_| rng.range(5, 20)));
        let base = next_id;
        record_batch_into(&mut hist, k, &mut out, |j| {
            let id = Id::from_u64(base + j);
            submit_market_order_with_user(&book, id, qtys[j as usize], Side::Buy, taker)
        });
        out.clear();
        next_id += k;
        for qty in &qtys {
            resting -= (*qty).min(resting);
        }
        done += k;
    }
    hist
}

/// One fully drawn op of the mixed stream.
#[derive(Clone, Copy)]
enum MixedOp {
    Submit(Submit),
    Cancel(u64),
    Market {
        id: u64,
        qty: u64,
        side: Side,
        user: [u8; 32],
    },
}

/// Draws the next op of the `mixed_70_20_10_hdr` stream: 70 %
/// `submit_gtc`, 20 % cancel of a random earlier id, 10 % market order
/// of 1..=10 lots.
fn draw_mixed(rng: &mut Rng, next_id: &mut u64) -> Option<MixedOp> {
    let v = rng.next() % 100;
    if v < 70 {
        let op = Submit::gtc(rng, *next_id);
        *next_id += 1;
        Some(MixedOp::Submit(op))
    } else if v < 90 {
        (*next_id > 1).then(|| MixedOp::Cancel(rng.range(1, *next_id - 1)))
    } else {
        let id = *next_id;
        *next_id += 1;
        let qty = rng.range(1, 10);
        let side = pick_side(rng);
        let user = pick_owner(rng);
        Some(MixedOp::Market {
            id,
            qty,
            side,
            user,
        })
    }
}

/// The output of one mixed op, dropped after the clock stops.
enum MixedOut<A, C, M> {
    Add(A),
    Cancel(C),
    Market(M),
}

#[inline]
fn apply_mixed(book: &Book, op: &MixedOp) -> MixedOut<impl Sized, impl Sized, impl Sized> {
    match *op {
        MixedOp::Submit(submit) => MixedOut::Add(submit.apply(book)),
        MixedOp::Cancel(target) => MixedOut::Cancel(cancel_order(book, Id::from_u64(target))),
        MixedOp::Market {
            id,
            qty,
            side,
            user,
        } => MixedOut::Market(submit_market_order_with_user(
            book,
            Id::from_u64(id),
            qty,
            side,
            user,
        )),
    }
}

/// `mixed_70_20_10_hdr`: the stream is drawn in full before the
/// measurement loop; one sample per op times only the dispatch into the
/// book. The 20 % cancels draw a random earlier id, so some miss.
pub fn mixed_70_20_10(warmup_ops: u64, measured_ops: u64) -> Histogram<u64> {
    let book = new_book("BENCH");
    let mut rng = Rng::new(SEED);
    let mut hist = new_histogram();
    let mut next_id = 1u64;

    for _ in 0..warmup_ops {
        if let Some(op) = draw_mixed(&mut rng, &mut next_id) {
            drop(apply_mixed(&book, &op));
        }
    }
    let ops: Vec<Option<MixedOp>> = (0..measured_ops)
        .map(|_| draw_mixed(&mut rng, &mut next_id))
        .collect();
    for op in &ops {
        match op {
            Some(op) => {
                let out = record(&mut hist, || apply_mixed(&book, op));
                drop(out);
            }
            // `mixed_70_20_10_hdr` times an empty closure for a cancel
            // drawn before any id exists; keep the sample count equal.
            None => record(&mut hist, || ()),
        }
    }
    hist
}

/// `thin_book_sweep_hdr`: 3 asks of 1..=5 lots refilled every 5 probes
/// (unmeasured), market-buy probes of 1..=20 lots batched 5 at a time;
/// probe quantities are drawn before each batch.
pub fn thin_book_sweep(measured_ops: u64) -> Histogram<u64> {
    const RESTING_PER_REFILL: u64 = 3;
    const REFILL_EVERY: u64 = 5;
    let book = new_book("BENCH");
    let mut rng = Rng::new(SEED);
    let mut hist = new_histogram();
    let maker = owner(0xAA);
    let taker = owner(0xBB);
    let mut next_id = 1u64;
    let mut op = 0u64;
    let mut out = Vec::new();
    let mut qtys = Vec::with_capacity(REFILL_EVERY as usize);

    while op < measured_ops {
        let k = REFILL_EVERY.min(measured_ops - op);
        for _ in 0..RESTING_PER_REFILL {
            let price = rng.range(99, 101) as u128;
            let qty = rng.range(1, 5);
            drop(add_limit_order_with_user(
                &book,
                Id::from_u64(next_id),
                price,
                qty,
                Side::Sell,
                maker,
            ));
            next_id += 1;
        }
        qtys.clear();
        qtys.extend((0..k).map(|_| rng.range(1, 20)));
        let base = next_id;
        record_batch_into(&mut hist, k, &mut out, |j| {
            let id = Id::from_u64(base + j);
            submit_market_order_with_user(&book, id, qtys[j as usize], Side::Buy, taker)
        });
        out.clear();
        next_id += k;
        op += k;
    }
    hist
}

/// Mass cancel of a dense NON-crossing book: `orders_per_burst` orders
/// seeded (unmeasured, every one asserted resting), then one timed
/// `cancel_all_orders`; its result (with the cancelled-id list) is
/// dropped after the clock. One sample is one whole burst. Before the
/// #259 PR review the seed was the crossing `submit_gtc` stream, so
/// much of the "10 000-order" book had traded away before the cancel.
pub fn mass_cancel_burst(orders_per_burst: u64, bursts: u64) -> Histogram<u64> {
    let book = new_book("BENCH");
    let mut rng = Rng::new(SEED);
    let mut hist = new_histogram();
    let mut next_id = 1u64;

    for _ in 0..bursts {
        dense_book(&book, &mut rng, next_id, orders_per_burst, SEED_OWNERS);
        next_id += orders_per_burst;
        let result = record(&mut hist, || cancel_all(&book));
        drop(result);
        assert_eq!(resting_orders(&book), 0, "mass cancel must empty the book");
    }
    hist
}

/// `mass_cancel_by_user_hdr`: a dense book of `orders_per_burst` orders,
/// every one owned by ONE user, then one timed `cancel_orders_by_user`
/// for that user. One sample per burst ("ns to drain N orders"), like
/// `mass_cancel_burst`. The by-user cancel walks the owner's
/// `user_orders` entry and cancels each id through the single-order path,
/// so it pays one `user_orders` removal per order at that entry's length.
pub fn mass_cancel_by_user(orders_per_burst: u64, bursts: u64) -> Histogram<u64> {
    let book = new_book("BENCH");
    let mut rng = Rng::new(SEED);
    let mut hist = new_histogram();
    let mut next_id = 1u64;
    // `dense_book` with one owner draws this owner for every order.
    let mut user = [0u8; 32];
    user[2] = 0x5A;

    for _ in 0..bursts {
        dense_book(&book, &mut rng, next_id, orders_per_burst, 1);
        next_id += orders_per_burst;
        let result = record(&mut hist, || cancel_by_user(&book, user));
        drop(result);
        assert_eq!(
            resting_orders(&book),
            0,
            "by-user cancel must empty the book"
        );
    }
    hist
}

/// Self-trade prevention, `CancelMaker`: before each op (unmeasured) one
/// level at 1 000 is seeded with the taker's own 5-lot ask at the front
/// and two 5-lot asks from another owner behind it; the timed op is the
/// taker's 10-lot market buy, which cancels its own maker and fills the
/// other two, emptying the level. One sample per op (above the tick).
pub fn stp_cancel_maker(measured_ops: u64) -> Histogram<u64> {
    let book = new_stp_book("BENCH");
    let mut hist = new_histogram();
    let taker = owner(0xBB);
    let other = owner(0xAA);
    let mut next_id = 1u64;

    for _ in 0..measured_ops {
        for user in [taker, other, other] {
            drop(add_limit_order_with_user(
                &book,
                Id::from_u64(next_id),
                1_000,
                5,
                Side::Sell,
                user,
            ));
            next_id += 1;
        }
        let id = Id::from_u64(next_id);
        next_id += 1;
        let out = record(&mut hist, || {
            submit_market_order_with_user(&book, id, 10, Side::Buy, taker)
        });
        drop(out);
    }
    hist
}

/// A non-crossing book of `orders` resting orders: bids on 900..=999,
/// asks on 1 001..=1 100, four owners. Returns the book and its ids.
fn resting_book(orders: u64) -> (Book, Vec<Id>) {
    let book = new_book("BENCH");
    let mut rng = Rng::new(SEED);
    let mut ids = Vec::with_capacity(orders as usize);
    for i in 0..orders {
        let id = Id::from_u64(i + 1);
        let side = pick_side(&mut rng);
        let offset = rng.range(0, 99) as u128;
        let price = match side {
            Side::Buy => 900 + offset,
            Side::Sell => 1_001 + offset,
        };
        let qty = rng.range(1, 100);
        let user = pick_owner(&mut rng);
        drop(add_limit_order_with_user(&book, id, price, qty, side, user));
        ids.push(id);
    }
    assert_eq!(
        resting_orders(&book),
        orders as usize,
        "seed must rest every order"
    );
    (book, ids)
}

/// Full-depth `create_snapshot` of a `orders`-order book; the snapshot
/// is dropped after the clock stops. One sample per snapshot.
pub fn snapshot_create(orders: u64, samples: u64) -> Histogram<u64> {
    let (book, _) = resting_book(orders);
    let mut hist = new_histogram();
    for _ in 0..samples {
        let snapshot = record(&mut hist, || create_snapshot(&book));
        drop(snapshot);
    }
    hist
}

/// `restore_from_snapshot_package` of a full-depth package of a
/// `orders`-order book into a fresh empty book. The package clone and
/// the empty book are built before the clock starts, and the restored
/// book is dropped after it stops. One sample per restore.
pub fn snapshot_restore(orders: u64, samples: u64) -> Histogram<u64> {
    let (book, _) = resting_book(orders);
    let package = snapshot_package(&book);
    let mut hist = new_histogram();
    for _ in 0..samples {
        let input = package.clone();
        let mut target = new_book("BENCH");
        record(&mut hist, || restore(&mut target, input));
        drop(target);
    }
    hist
}

/// `ReplayEngine::replay_from` of an in-memory journal of `events`
/// `AddOrder` events (the resting orders of [`resting_book`], so no
/// event crosses). The replayed book is dropped after the clock stops.
/// One sample per replay.
pub fn replay_adds(events: u64, samples: u64) -> Histogram<u64> {
    let (book, ids) = resting_book(events);
    let journal = journal_of_adds(&book, &ids);
    drop(book);
    let mut hist = new_histogram();
    for _ in 0..samples {
        let replayed = record(&mut hist, || replay(&journal));
        drop(replayed);
    }
    hist
}

/// `threads` threads add GTC buys at ONE price for ONE account on one
/// shared book (the shape of `concurrent_add_limit_orders[_with_listeners]`
/// in `benches/concurrent/register.rs`), each timing `BATCH` adds per
/// `Instant` pair; the per-thread histograms are merged. `warmup` adds
/// per thread run before a barrier and are not recorded.
pub fn contended_same_price_adds(
    threads: u64,
    warmup: u64,
    per_thread: u64,
    listeners: bool,
) -> Histogram<u64> {
    let book = Arc::new(if listeners {
        new_listener_book("BENCH")
    } else {
        new_book("BENCH")
    });
    let barrier = Arc::new(Barrier::new(threads as usize));
    let account = owner(0x11);
    let handles: Vec<_> = (0..threads)
        .map(|t| {
            let book = Arc::clone(&book);
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                let base = (t + 1) * 1_000_000_000;
                let mut next = base;
                for _ in 0..warmup {
                    drop(add_limit_order_with_user(
                        &book,
                        Id::from_u64(next),
                        1_000,
                        10,
                        Side::Buy,
                        account,
                    ));
                    next += 1;
                }
                barrier.wait();
                let mut hist = new_histogram();
                let mut out = Vec::new();
                let mut done = 0u64;
                while done < per_thread {
                    let k = BATCH.min(per_thread - done);
                    let first = next;
                    record_batch_into(&mut hist, k, &mut out, |j| {
                        add_limit_order_with_user(
                            &book,
                            Id::from_u64(first + j),
                            1_000,
                            10,
                            Side::Buy,
                            account,
                        )
                    });
                    out.clear();
                    next += k;
                    done += k;
                }
                hist
            })
        })
        .collect();
    let mut merged = new_histogram();
    for handle in handles {
        let hist = handle.join().expect("contended worker");
        merged.add(&hist).expect("merge histograms");
    }
    merged
}
