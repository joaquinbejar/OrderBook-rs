/******************************************************************************
   Unit tests for issue #101 — caller-injected book config on replay.

   A journal produced by a non-default-config book (lot_size rounding, STP,
   fees) must be replayed through a `*_with_config` variant with the matching
   `ReplayBookConfig`, or the reconstructed book diverges structurally. These
   tests demonstrate the divergence without config and the parity with it.
******************************************************************************/

use orderbook_rs::orderbook::sequencer::{
    InMemoryJournal, Journal, ReplayBookConfig, ReplayEngine, SequencerCommand, SequencerEvent,
    SequencerResult, snapshots_match,
};
use orderbook_rs::orderbook::trade::TradeResult;
use orderbook_rs::{Clock, FeeSchedule, OrderBook, STPMode, StubClock};
use pricelevel::{
    Hash32, Id, MatchResult, OrderType, Price, Quantity, Side, TimeInForce, TimestampMs,
};
use std::sync::Arc;

// ─── Helpers ────────────────────────────────────────────────────────────────

fn make_add_event(
    seq: u64,
    id: Id,
    price: u128,
    qty: u64,
    side: Side,
    user_id: Hash32,
) -> SequencerEvent<()> {
    let order = OrderType::Standard {
        id,
        price: Price::new(price),
        quantity: Quantity::new(qty),
        side,
        time_in_force: TimeInForce::Gtc,
        user_id,
        timestamp: TimestampMs::new(0),
        extra_fields: (),
    };
    SequencerEvent {
        sequence_num: seq,
        timestamp_ns: 0,
        command: SequencerCommand::AddOrder(order),
        result: SequencerResult::OrderAdded { order_id: id },
    }
}

fn make_market_by_amount_event(
    seq: u64,
    taker_id: Id,
    amount: u128,
    side: Side,
) -> SequencerEvent<()> {
    SequencerEvent {
        sequence_num: seq,
        timestamp_ns: 0,
        command: SequencerCommand::MarketOrderByAmount {
            id: taker_id,
            amount,
            side,
        },
        // Informational only — replay re-executes the command. Use
        // TradeExecuted so the journal entry is not skipped as Rejected.
        result: SequencerResult::TradeExecuted {
            trade_result: TradeResult::new(
                "TEST".to_string(),
                MatchResult::new(taker_id, Quantity::new(0)),
            ),
        },
    }
}

fn stub_clock() -> Arc<dyn Clock> {
    Arc::new(StubClock::starting_at(0))
}

// ─── Round-trip: ReplayBookConfig application on a fresh book ────────────────

/// A fresh book plus the `set_*` calls `ReplayBookConfig` performs must yield a
/// book whose configuration matches the carrier field-for-field. This is the
/// pure application round-trip independent of any replay.
#[test]
fn replay_book_config_applies_to_fresh_book() {
    let fee = FeeSchedule::new(-2, 5);
    let config = ReplayBookConfig::new(
        Some(fee),
        STPMode::CancelTaker,
        Some(10),
        Some(5),
        Some(2),
        Some(1_000),
    );

    // Replay applies the config via the `*_with_config` path on a one-event
    // journal; the reconstructed book must carry the configured fields. The
    // single add carries a non-zero user id (STP is CancelTaker and bare
    // orders are rejected with `MissingUserId`), a price on the 10-tick grid,
    // and a quantity that is a multiple of the 5-lot and within [2, 1000].
    let journal: InMemoryJournal<()> = InMemoryJournal::new();
    let id = Id::new_uuid();
    assert!(
        journal
            .append(&make_add_event(
                0,
                id,
                100,
                10,
                Side::Buy,
                Hash32::new([3u8; 32])
            ))
            .is_ok()
    );

    let (book, _) = ReplayEngine::<()>::replay_from_with_config(&journal, 0, "TEST", &config)
        .expect("config replay should succeed");

    assert_eq!(book.fee_schedule(), Some(fee), "fee schedule injected");
    assert_eq!(book.stp_mode(), STPMode::CancelTaker, "stp mode injected");
    assert_eq!(book.tick_size(), Some(10), "tick size injected");
    assert_eq!(book.lot_size(), Some(5), "lot size injected");
    assert_eq!(book.min_order_size(), Some(2), "min order size injected");
    assert_eq!(
        book.max_order_size(),
        Some(1_000),
        "max order size injected"
    );
}

/// `ReplayBookConfig::default` leaves a replayed book at all-defaults, matching
/// the plain `replay_from` entry point.
#[test]
fn replay_book_config_default_is_all_defaults() {
    let journal: InMemoryJournal<()> = InMemoryJournal::new();
    let id = Id::new_uuid();
    assert!(
        journal
            .append(&make_add_event(0, id, 100, 10, Side::Buy, Hash32::zero()))
            .is_ok()
    );

    let (book, _) = ReplayEngine::<()>::replay_from_with_config(
        &journal,
        0,
        "TEST",
        &ReplayBookConfig::default(),
    )
    .expect("default config replay should succeed");

    assert_eq!(book.fee_schedule(), None);
    assert_eq!(book.stp_mode(), STPMode::None);
    assert_eq!(book.tick_size(), None);
    assert_eq!(book.lot_size(), None);
    assert_eq!(book.min_order_size(), None);
    assert_eq!(book.max_order_size(), None);
}

// ─── lot_size divergence / parity via MarketOrderByAmount rounding ──────────

/// Builds the shared journal: one resting ask wall, then a notional market buy
/// that rounds differently under `lot_size`.
///
/// Ask wall: 10 @ 100. Market buy by amount 700 ⇒ 7 base units at price 100.
/// With `lot_size = 5` the per-level fill rounds **down** to 5, leaving 5 @ 100.
/// Without lot_size, 7 fill, leaving 3 @ 100. Same journal, divergent residual.
fn lot_size_journal() -> (InMemoryJournal<()>, u64) {
    let journal: InMemoryJournal<()> = InMemoryJournal::new();
    let mut seq = 0u64;

    let ask_id = Id::new_uuid();
    assert!(
        journal
            .append(&make_add_event(
                seq,
                ask_id,
                100,
                10,
                Side::Sell,
                Hash32::zero()
            ))
            .is_ok()
    );
    seq += 1;

    let taker_id = Id::new_uuid();
    assert!(
        journal
            .append(&make_market_by_amount_event(seq, taker_id, 700, Side::Buy))
            .is_ok()
    );

    (journal, seq)
}

/// The lot-constrained source book leaves a different residual than a
/// default-config book replaying the same journal — proving the bug: replaying
/// without the config reconstructs a STRUCTURALLY DIFFERENT book.
#[test]
fn lot_size_replay_without_config_diverges() {
    let (journal, last_seq) = lot_size_journal();

    // Ground-truth: a live book WITH lot_size = 5 driven through the same ops.
    let mut live = OrderBook::<()>::with_clock("TEST", stub_clock());
    live.set_lot_size(5);
    live.add_order(OrderType::Standard {
        id: Id::new_uuid(),
        price: Price::new(100),
        quantity: Quantity::new(10),
        side: Side::Sell,
        time_in_force: TimeInForce::Gtc,
        user_id: Hash32::zero(),
        timestamp: TimestampMs::new(0),
        extra_fields: (),
    })
    .expect("seed ask");
    let _ = live.match_market_order_by_amount(Id::new_uuid(), 700, Side::Buy);
    let live_snap = live.create_snapshot(usize::MAX);

    // Replay WITHOUT config — fresh book has no lot_size, so it takes 7 (not 5).
    let (replayed, seq) =
        ReplayEngine::<()>::replay_from_with_clock(&journal, 0, "TEST", stub_clock())
            .expect("plain replay should succeed");
    assert_eq!(seq, last_seq);
    let replayed_snap = replayed.create_snapshot(usize::MAX);

    assert!(
        !snapshots_match(&live_snap, &replayed_snap),
        "default-config replay must DIVERGE from a lot-constrained source book"
    );
}

/// Replaying the SAME journal through `replay_from_with_clock_and_config` with
/// the matching `lot_size` reconstructs the original residual exactly —
/// `snapshots_match` is true only with the config injected.
#[test]
fn lot_size_replay_with_config_matches() {
    let (journal, last_seq) = lot_size_journal();

    // Ground-truth live book WITH lot_size = 5.
    let mut live = OrderBook::<()>::with_clock("TEST", stub_clock());
    live.set_lot_size(5);
    live.add_order(OrderType::Standard {
        id: Id::new_uuid(),
        price: Price::new(100),
        quantity: Quantity::new(10),
        side: Side::Sell,
        time_in_force: TimeInForce::Gtc,
        user_id: Hash32::zero(),
        timestamp: TimestampMs::new(0),
        extra_fields: (),
    })
    .expect("seed ask");
    let _ = live.match_market_order_by_amount(Id::new_uuid(), 700, Side::Buy);
    let live_snap = live.create_snapshot(usize::MAX);

    // Replay WITH the matching config.
    let config = ReplayBookConfig {
        lot_size: Some(5),
        ..ReplayBookConfig::default()
    };
    let (replayed, seq) = ReplayEngine::<()>::replay_from_with_clock_and_config(
        &journal,
        0,
        "TEST",
        stub_clock(),
        &config,
    )
    .expect("config replay should succeed");
    assert_eq!(seq, last_seq);
    let replayed_snap = replayed.create_snapshot(usize::MAX);

    assert!(
        snapshots_match(&live_snap, &replayed_snap),
        "config-injected replay must match the lot-constrained source book"
    );
}

// ─── STP: prevented order is recorded Rejected and skipped on replay ─────────

/// Self-trade prevention surfaces as a rejected command at write time, so a
/// faithful journal records the prevented order with a `Rejected` result. The
/// replay engine skips `Rejected` events, so the prevented order never crosses
/// on replay — independent of whether STP config is injected. This documents
/// *why* STP, unlike `lot_size`, does not need structural reconstruction: the
/// non-determinism (the prevention decision) was resolved upstream and baked
/// into the recorded result, exactly as the replay-determinism rule requires.
#[test]
fn stp_prevented_order_recorded_rejected_is_skipped_on_replay() {
    let user = Hash32::new([7u8; 32]);
    let journal: InMemoryJournal<()> = InMemoryJournal::new();

    // Resting ask from user U (succeeds even under STP — nothing to cross).
    let ask_id = Id::new_uuid();
    assert!(
        journal
            .append(&make_add_event(0, ask_id, 100, 10, Side::Sell, user))
            .is_ok()
    );

    // Same-user crossing buy: under STP = CancelTaker this was prevented at
    // write time, so the sequencer recorded it as Rejected.
    let buy_id = Id::new_uuid();
    let rejected_buy = SequencerEvent::<()> {
        sequence_num: 1,
        timestamp_ns: 0,
        command: SequencerCommand::AddOrder(OrderType::Standard {
            id: buy_id,
            price: Price::new(100),
            quantity: Quantity::new(10),
            side: Side::Buy,
            time_in_force: TimeInForce::Gtc,
            user_id: user,
            timestamp: TimestampMs::new(0),
            extra_fields: (),
        }),
        result: SequencerResult::Rejected {
            reason: "self-trade prevented".to_string(),
        },
    };
    assert!(journal.append(&rejected_buy).is_ok());

    // Ground truth: a live STP book where the second add is prevented. The
    // resting ask stays in the book.
    let mut live = OrderBook::<()>::with_clock("TEST", stub_clock());
    live.set_stp_mode(STPMode::CancelTaker);
    live.add_order(OrderType::Standard {
        id: Id::new_uuid(),
        price: Price::new(100),
        quantity: Quantity::new(10),
        side: Side::Sell,
        time_in_force: TimeInForce::Gtc,
        user_id: user,
        timestamp: TimestampMs::new(0),
        extra_fields: (),
    })
    .expect("seed ask");
    // The crossing buy is rejected by STP — tolerate the error, the ask rests.
    let _ = live.add_order(OrderType::Standard {
        id: Id::new_uuid(),
        price: Price::new(100),
        quantity: Quantity::new(10),
        side: Side::Buy,
        time_in_force: TimeInForce::Gtc,
        user_id: user,
        timestamp: TimestampMs::new(0),
        extra_fields: (),
    });
    let live_snap = live.create_snapshot(usize::MAX);

    // Replaying WITH the matching STP config skips the Rejected buy and rebuilds
    // the resting ask, matching the live book.
    let config = ReplayBookConfig {
        stp_mode: STPMode::CancelTaker,
        ..ReplayBookConfig::default()
    };
    let (replayed, seq) = ReplayEngine::<()>::replay_from_with_clock_and_config(
        &journal,
        0,
        "TEST",
        stub_clock(),
        &config,
    )
    .expect("config replay should succeed");
    assert_eq!(
        seq, 0,
        "only the ask (seq 0) is applied; the buy is skipped"
    );
    let replayed_snap = replayed.create_snapshot(usize::MAX);

    assert!(
        snapshots_match(&live_snap, &replayed_snap),
        "STP-config replay must match the STP-protected source book"
    );
}

// ─── Config injection is complete and harmless to non-structural fields ─────

/// Injecting a fully-populated config (fees + STP-None + wide min/max) on top
/// of the structural `lot_size` still reconstructs the lot-constrained residual
/// exactly — extra non-structural configuration does not perturb parity. This
/// guards against `apply_to` mis-wiring a field or rejecting a valid order.
#[test]
fn full_config_injection_preserves_lot_size_parity() {
    let (journal, last_seq) = lot_size_journal();

    // Ground-truth live book WITH the same full config.
    let fee = FeeSchedule::new(-2, 5);
    let mut live = OrderBook::<()>::with_clock("TEST", stub_clock());
    live.set_lot_size(5);
    live.set_fee_schedule(Some(fee));
    live.set_min_order_size(1);
    live.set_max_order_size(1_000);
    live.add_order(OrderType::Standard {
        id: Id::new_uuid(),
        price: Price::new(100),
        quantity: Quantity::new(10),
        side: Side::Sell,
        time_in_force: TimeInForce::Gtc,
        user_id: Hash32::zero(),
        timestamp: TimestampMs::new(0),
        extra_fields: (),
    })
    .expect("seed ask");
    let _ = live.match_market_order_by_amount(Id::new_uuid(), 700, Side::Buy);
    let live_snap = live.create_snapshot(usize::MAX);

    let config = ReplayBookConfig::new(
        Some(fee),
        STPMode::None,
        None,
        Some(5),
        Some(1),
        Some(1_000),
    );
    let (replayed, seq) = ReplayEngine::<()>::replay_from_with_clock_and_config(
        &journal,
        0,
        "TEST",
        stub_clock(),
        &config,
    )
    .expect("config replay should succeed");
    assert_eq!(seq, last_seq);
    let replayed_snap = replayed.create_snapshot(usize::MAX);

    assert!(
        snapshots_match(&live_snap, &replayed_snap),
        "full config injection must preserve lot_size parity"
    );
}
