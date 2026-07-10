//! Replay byte-identity tests for mass-cancel result ordering (Issue #190).
//!
//! A mass-cancel result's `cancelled_order_ids` is journaled inside
//! [`SequencerResult::MassCancelled`]. Replay must reproduce that payload
//! byte-for-byte on an independently constructed book, and the post-session
//! snapshot must match. Before #190 the `cancel_*` ops enumerated orders via
//! order-unstable structures (the `DashMap` hasher is seeded per instance), so
//! the replayed payload could diverge from the journaled original across
//! processes. These tests are the regression guard.

use orderbook_rs::OrderBook;
use orderbook_rs::orderbook::mass_cancel::MassCancelResult;
use orderbook_rs::orderbook::sequencer::{
    InMemoryJournal, Journal, ReplayEngine, SequencerCommand, SequencerEvent, SequencerResult,
    snapshots_match,
};
use pricelevel::{Hash32, Id, OrderType, Price, Quantity, Side, TimeInForce, TimestampMs};

fn order(id: Id, price: u128, qty: u64, side: Side) -> OrderType<()> {
    OrderType::Standard {
        id,
        price: Price::new(price),
        quantity: Quantity::new(qty),
        side,
        time_in_force: TimeInForce::Gtc,
        user_id: Hash32::zero(),
        timestamp: TimestampMs::new(0),
        extra_fields: (),
    }
}

/// Extract the `MassCancelResult` from a journaled event, or fail the test.
fn expect_mass_cancelled(result: &SequencerResult) -> &MassCancelResult {
    match result {
        SequencerResult::MassCancelled { result } => result,
        other => panic!("expected MassCancelled, got {other:?}"),
    }
}

/// #190: a session of adds + `CancelBySide` + `CancelByPriceRange` + `CancelAll`
/// replays with byte-identical `MassCancelled` payloads and a matching snapshot.
///
/// The journal is built from a live book. A fresh, independently constructed
/// book then re-executes the identical command stream read back from the
/// journal, capturing each mass-cancel result; every replayed
/// `cancelled_order_ids` vector (ids AND order) must equal the journaled
/// original. Finally a full `ReplayEngine` pass must produce a snapshot that
/// `snapshots_match` finds equal to the live book's — with residual orders
/// surviving so the oracle is load-bearing rather than trivially empty.
#[test]
fn test_replay_mass_cancel_payloads_match_journaled_originals() {
    let symbol = "TEST";
    let journal: InMemoryJournal<()> = InMemoryJournal::new();
    let live: OrderBook<()> = OrderBook::new(symbol);

    let mut seq = 0u64;
    let mut append = |command: SequencerCommand<()>, result: SequencerResult| {
        let ev = SequencerEvent::<()> {
            sequence_num: seq,
            timestamp_ns: 0,
            command,
            result,
        };
        assert!(journal.append(&ev).is_ok(), "append seq {seq}");
        seq += 1;
    };

    // Interleaved admission across two bid levels (90, 100), two ask levels to
    // be range-cancelled (110, 120), and two surviving ask levels (130, 140) so
    // the final CancelAll spans multiple levels — a stronger byte-identity probe
    // than a single-level tail cancel.
    let b90a = Id::new_uuid();
    let b90b = Id::new_uuid();
    let b100a = Id::new_uuid();
    let b100b = Id::new_uuid();
    let a110a = Id::new_uuid();
    let a110b = Id::new_uuid();
    let a120a = Id::new_uuid();
    let a120b = Id::new_uuid();
    let a130a = Id::new_uuid();
    let a130b = Id::new_uuid();
    let a140a = Id::new_uuid();
    let a140b = Id::new_uuid();

    let adds = [
        order(b100a, 100, 1, Side::Buy),
        order(a120a, 120, 1, Side::Sell),
        order(a140a, 140, 1, Side::Sell),
        order(b90a, 90, 1, Side::Buy),
        order(a110a, 110, 1, Side::Sell),
        order(a130a, 130, 1, Side::Sell),
        order(b100b, 100, 1, Side::Buy),
        order(a110b, 110, 1, Side::Sell),
        order(a140b, 140, 1, Side::Sell),
        order(b90b, 90, 1, Side::Buy),
        order(a120b, 120, 1, Side::Sell),
        order(a130b, 130, 1, Side::Sell),
    ];
    for ord in &adds {
        assert!(live.add_order(*ord).is_ok(), "live add");
        append(
            SequencerCommand::AddOrder(*ord),
            SequencerResult::OrderAdded { order_id: ord.id() },
        );
    }

    // CancelBySide(Buy): all bids, ascending price then FIFO.
    let by_side = live.cancel_orders_by_side(Side::Buy);
    assert_eq!(
        by_side.cancelled_order_ids(),
        &[b90a, b90b, b100a, b100b],
        "live CancelBySide order"
    );
    append(
        SequencerCommand::CancelBySide { side: Side::Buy },
        SequencerResult::MassCancelled {
            result: by_side.clone(),
        },
    );

    // CancelByPriceRange(Sell, 110, 120): the two mid ask levels.
    let by_range = live.cancel_orders_by_price_range(Side::Sell, 110, 120);
    assert_eq!(
        by_range.cancelled_order_ids(),
        &[a110a, a110b, a120a, a120b],
        "live CancelByPriceRange order"
    );
    append(
        SequencerCommand::CancelByPriceRange {
            side: Side::Sell,
            min_price: 110,
            max_price: 120,
        },
        SequencerResult::MassCancelled {
            result: by_range.clone(),
        },
    );

    // CancelAll: the surviving 130 and 140 asks, ascending price then FIFO.
    // Book is empty afterwards.
    let all = live.cancel_all_orders();
    assert_eq!(
        all.cancelled_order_ids(),
        &[a130a, a130b, a140a, a140b],
        "live CancelAll order"
    );
    append(
        SequencerCommand::CancelAll,
        SequencerResult::MassCancelled {
            result: all.clone(),
        },
    );

    // Residual orders so the post-session snapshot is non-empty and the
    // `snapshots_match` oracle is load-bearing.
    let r_bid = order(Id::new_uuid(), 100, 3, Side::Buy);
    let r_ask = order(Id::new_uuid(), 140, 4, Side::Sell);
    for ord in [r_bid, r_ask] {
        assert!(live.add_order(ord).is_ok(), "residual add");
        append(
            SequencerCommand::AddOrder(ord),
            SequencerResult::OrderAdded { order_id: ord.id() },
        );
    }

    // --- Part A: replayed mass-cancel payloads equal the journaled originals.
    // Re-execute the journaled command stream on a fresh, independently
    // constructed book (its own DashMap hasher) and compare every mass-cancel
    // result to the one recorded live. ReplayEngine discards results, so this
    // hand-rolled pass is what captures them.
    let replay_book: OrderBook<()> = OrderBook::new(symbol);
    let entries = journal.read_from(0).expect("read journal");
    for item in entries {
        let entry = item.expect("decode entry");
        let ev = entry.event;
        match &ev.command {
            SequencerCommand::AddOrder(o) => {
                assert!(replay_book.add_order(*o).is_ok(), "replay add");
            }
            SequencerCommand::CancelBySide { side } => {
                let replayed = replay_book.cancel_orders_by_side(*side);
                let journaled = expect_mass_cancelled(&ev.result);
                assert_eq!(
                    replayed.cancelled_order_ids(),
                    journaled.cancelled_order_ids(),
                    "CancelBySide payload divergence at seq {}",
                    ev.sequence_num
                );
                assert_eq!(replayed.cancelled_count(), journaled.cancelled_count());
            }
            SequencerCommand::CancelByPriceRange {
                side,
                min_price,
                max_price,
            } => {
                let replayed =
                    replay_book.cancel_orders_by_price_range(*side, *min_price, *max_price);
                let journaled = expect_mass_cancelled(&ev.result);
                assert_eq!(
                    replayed.cancelled_order_ids(),
                    journaled.cancelled_order_ids(),
                    "CancelByPriceRange payload divergence at seq {}",
                    ev.sequence_num
                );
                assert_eq!(replayed.cancelled_count(), journaled.cancelled_count());
            }
            SequencerCommand::CancelAll => {
                let replayed = replay_book.cancel_all_orders();
                let journaled = expect_mass_cancelled(&ev.result);
                assert_eq!(
                    replayed.cancelled_order_ids(),
                    journaled.cancelled_order_ids(),
                    "CancelAll payload divergence at seq {}",
                    ev.sequence_num
                );
                assert_eq!(replayed.cancelled_count(), journaled.cancelled_count());
            }
            _ => {}
        }
    }

    // --- Part B: full ReplayEngine pass reproduces the live post-session state.
    let (replayed, last_seq) =
        ReplayEngine::<()>::replay_from(&journal, 0, symbol).expect("replay must succeed");
    // `seq` is the count of appended events; the last event's sequence number
    // is one less.
    assert_eq!(last_seq, seq - 1);

    let live_snap = live.create_snapshot(usize::MAX);
    let replayed_snap = replayed.create_snapshot(usize::MAX);
    assert!(
        snapshots_match(&live_snap, &replayed_snap),
        "post-session live and replayed snapshots must match"
    );

    // Sanity: exactly the residual bid and ask survived.
    assert_eq!(replayed_snap.bids.len(), 1, "one residual bid survives");
    assert_eq!(replayed_snap.asks.len(), 1, "one residual ask survives");
    assert_eq!(replayed.best_bid(), Some(100));
    assert_eq!(replayed.best_ask(), Some(140));
}
