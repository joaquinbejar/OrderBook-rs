//! Thin adapter over the `orderbook-rs` API surface this harness calls.
//!
//! `workloads.rs` is byte-identical whichever side of the comparison it
//! is compiled for; every version-specific detail lives here, isolated
//! behind the `v0_13` / `head` Cargo features selected by
//! `scripts/bench_compare.sh`. The two arms differ in exactly one call
//! today (#259): `OrderBook::create_snapshot` returns the snapshot on
//! `v0.13.1` and a `Result` on HEAD (0.14.0), so [`create_snapshot`]
//! is the one diverging function. Everything else the workloads call
//! (`add_limit_order_with_user`, `submit_market_order_with_user`,
//! `cancel_order`, `cancel_all_orders`, `with_stp_mode`,
//! `with_trade_and_price_level_listener`, `create_snapshot_package` /
//! `restore_from_snapshot_package`, `get_order`, the sequencer's
//! `InMemoryJournal` / `ReplayEngine::replay_from`) has the same
//! signature on both tags (checked against `v0.13.1` during #259). The
//! identical parts live in [`common`] so a future divergence is a
//! one-function move into the two arms, not a rewrite of the workloads.
//!
//! Every scenario submits and cancels with a `user_id` (never the
//! userless `add_limit_order` / `submit_market_order` paths), matching
//! the headline `benches/order_book/*_hdr.rs` shape exactly — a PR
//! review on #258 caught `cancel_only` diverging from
//! `cancel_only_hdr`'s `submit_gtc` (which always attaches an owner),
//! which meant this crate was comparing a different book/index shape
//! than the scenario it claims to mirror.
//!
//! # `owner` returns `[u8; 32]`, not `pricelevel::Hash32`
//!
//! `add_limit_order_with_user` / `submit_market_order_with_user` take
//! `user: [u8; 32]` and convert with `.into()` at the call site, letting
//! type inference resolve to *whichever* `Hash32` the active
//! `orderbook-rs` path dependency's own `pricelevel` edge provides.
//! This crate tried a direct `pricelevel` dependency during #258 review
//! to name `Hash32` explicitly and reverted it (see `Cargo.toml`'s doc
//! comment): `v0.13.1` and HEAD pin semver-incompatible `pricelevel`
//! minors (`0.9` / `0.10`), so a version range wide enough to match
//! both does not make Cargo unify on one of them — it resolves this
//! crate's own direct edge independently (to the newest match) from
//! `orderbook-rs`'s transitive edge, yielding two incompatible copies
//! of the crate and a type-mismatch on every `Hash32` argument.
//! `From<[u8; 32]> for Hash32` is present in both `0.9.2` and `0.10.0`,
//! so passing a plain array and converting through the callee's own
//! resolved type sidesteps the whole problem — this file never needs
//! to name `Hash32` at all.

/// Calls whose signature is identical on every targeted version.
mod common {
    use orderbook_rs::orderbook::OrderBookSnapshotPackage;
    use orderbook_rs::orderbook::book_change_event::{
        PriceLevelChangedEvent, PriceLevelChangedListener,
    };
    use orderbook_rs::orderbook::sequencer::{
        InMemoryJournal, Journal, ReplayEngine, SequencerCommand, SequencerEvent, SequencerResult,
    };
    use orderbook_rs::orderbook::trade::{TradeListener, TradeResult};
    use orderbook_rs::{Id, OrderBook, STPMode, Side, TimeInForce};
    use std::hint::black_box;
    use std::sync::Arc;

    pub type Book = OrderBook<()>;
    pub type Package = OrderBookSnapshotPackage;
    pub type EventJournal = InMemoryJournal<()>;

    #[inline]
    pub fn new_book(symbol: &str) -> Book {
        OrderBook::new(symbol)
    }

    /// A book with self-trade prevention in `CancelMaker` mode.
    #[inline]
    pub fn new_stp_book(symbol: &str) -> Book {
        OrderBook::with_stp_mode(symbol, STPMode::CancelMaker)
    }

    /// A book with no-op trade and price-level listeners (the shape of
    /// `benches/concurrent/register.rs::book_with_listeners`, minus the
    /// HEAD-only `engine_seq` field read).
    pub fn new_listener_book(symbol: &str) -> Book {
        let trade: TradeListener = Arc::new(|result: &TradeResult| {
            black_box(result);
        });
        let level: PriceLevelChangedListener = Arc::new(|event: PriceLevelChangedEvent| {
            black_box(event);
        });
        OrderBook::with_trade_and_price_level_listener(symbol, trade, level)
    }

    #[inline]
    pub fn owner(byte: u8) -> [u8; 32] {
        let mut bytes = [0u8; 32];
        bytes[0] = byte;
        bytes
    }

    /// Every operation returns the engine's own result so the caller can
    /// drop it after the clock stops (#259 PR review): dropping it here
    /// would free the returned order / match result / id list inside the
    /// timed region.
    #[inline]
    pub fn add_limit_order_with_user(
        book: &Book,
        id: Id,
        price: u128,
        qty: u64,
        side: Side,
        user: [u8; 32],
    ) -> impl Sized {
        book.add_limit_order_with_user(id, price, qty, side, TimeInForce::Gtc, user.into(), None)
    }

    #[inline]
    pub fn cancel_order(book: &Book, id: Id) -> impl Sized {
        book.cancel_order(id)
    }

    #[inline]
    pub fn cancel_all(book: &Book) -> impl Sized {
        book.cancel_all_orders()
    }

    #[inline]
    pub fn cancel_by_user(book: &Book, user: [u8; 32]) -> impl Sized {
        book.cancel_orders_by_user(user.into())
    }

    /// Number of resting orders (setup-time assertions only: it walks
    /// every level).
    pub fn resting_orders(book: &Book) -> usize {
        book.get_all_orders().len()
    }

    #[inline]
    pub fn submit_market_order_with_user(
        book: &Book,
        id: Id,
        qty: u64,
        side: Side,
        user: [u8; 32],
    ) -> impl Sized {
        book.submit_market_order_with_user(id, qty, side, user.into())
    }

    /// Full-depth snapshot package of `book`.
    pub fn snapshot_package(book: &Book) -> Package {
        book.create_snapshot_package(usize::MAX)
            .expect("snapshot package")
    }

    /// Restores `package` into `book`, replacing its contents.
    #[inline]
    pub fn restore(book: &mut Book, package: Package) {
        book.restore_from_snapshot_package(package)
            .expect("restore snapshot package");
    }

    /// A journal of one `AddOrder` event per order in `orders`, in order.
    /// The orders are read back from a book (`get_order`), so this crate
    /// never names `pricelevel`'s `Price` / `Quantity` newtypes (see the
    /// module docs on `owner`).
    pub fn journal_of_adds(book: &Book, ids: &[Id]) -> EventJournal {
        let journal = InMemoryJournal::new();
        for (seq, id) in ids.iter().enumerate() {
            let order = book.get_order(*id).expect("seeded order");
            let event = SequencerEvent {
                sequence_num: seq as u64,
                timestamp_ns: seq as u64 * 1_000,
                command: SequencerCommand::AddOrder((*order).clone()),
                result: SequencerResult::OrderAdded { order_id: *id },
            };
            journal.append(&event).expect("journal append");
        }
        journal
    }

    /// Replays `journal` from sequence 0 into a fresh book, returned so
    /// the caller drops it outside the timed region.
    #[inline]
    pub fn replay(journal: &EventJournal) -> Book {
        ReplayEngine::<()>::replay_from(journal, 0, "BENCH")
            .expect("replay")
            .0
    }
}

#[cfg(feature = "head")]
mod imp {
    pub use super::common::*;

    /// Full-depth snapshot; returned so the caller drops it outside the
    /// timed region. HEAD (0.14.0): `create_snapshot` returns a `Result`.
    #[inline]
    pub fn create_snapshot(book: &Book) -> impl Sized {
        book.create_snapshot(usize::MAX).expect("snapshot")
    }
}

#[cfg(feature = "v0_13")]
mod imp {
    pub use super::common::*;

    /// Full-depth snapshot; returned so the caller drops it outside the
    /// timed region. `v0.13.1`: `create_snapshot` returns the snapshot.
    #[inline]
    pub fn create_snapshot(book: &Book) -> impl Sized {
        book.create_snapshot(usize::MAX)
    }
}

#[cfg(not(any(feature = "head", feature = "v0_13")))]
compile_error!(
    "orderbook-rs-bench-compare requires exactly one of the `head` / `v0_13` \
     features — pass `--features head` or `--features v0_13` (scripts/bench_compare.sh \
     does this for you per side)."
);

#[cfg(all(feature = "head", feature = "v0_13"))]
compile_error!(
    "orderbook-rs-bench-compare requires exactly one of the `head` / `v0_13` \
     features, not both — `--no-default-features --features <one>`."
);

pub use imp::*;
