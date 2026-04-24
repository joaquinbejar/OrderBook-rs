//! Integration tests for the deterministic-replay contract: with a stable
//! injected [`Clock`], replaying the same journal produces byte-identical
//! book state across independent runs.
//!
//! These tests exercise [`ReplayEngine::replay_from_with_clock`], the
//! clock-aware replay entry point added alongside the [`Clock`] trait.

use super::common::strategies::event_stream;

use orderbook_rs::orderbook::sequencer::{
    InMemoryJournal, Journal, ReplayEngine, SequencerCommand, SequencerEvent, SequencerResult,
    snapshots_match,
};
use orderbook_rs::{Clock, MonotonicClock, StubClock};
use pricelevel::{Hash32, Id, OrderType, Price, Quantity, Side, TimeInForce, TimestampMs};
use proptest::prelude::*;
use proptest::test_runner::TestCaseError;
use std::sync::Arc;

// ─── Hand-built helpers ─────────────────────────────────────────────────────

fn make_add_event(seq: u64, id: Id, price: u128, qty: u64, side: Side) -> SequencerEvent<()> {
    let order = OrderType::Standard {
        id,
        price: Price::new(price),
        quantity: Quantity::new(qty),
        side,
        time_in_force: TimeInForce::Gtc,
        user_id: Hash32::zero(),
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

fn populate_journal(journal: &InMemoryJournal<()>, events: &[SequencerEvent<()>]) {
    for event in events {
        let appended = journal.append(event);
        assert!(appended.is_ok(), "journal append failed: {:?}", appended);
    }
}

fn replay_once(
    journal: &InMemoryJournal<()>,
    symbol: &str,
    start: u64,
    step: u64,
) -> orderbook_rs::OrderBookSnapshot {
    let clock: Arc<dyn Clock> = Arc::new(StubClock::with_step(start, step));
    let outcome = ReplayEngine::<()>::replay_from_with_clock(journal, 1, symbol, clock);
    let (book, _last_seq) =
        outcome.expect("replay_from_with_clock should succeed on a well-formed journal");
    book.create_snapshot(usize::MAX)
}

// ─── Example-based regression test ──────────────────────────────────────────

#[test]
fn replay_with_identical_stub_clocks_produces_matching_snapshots() {
    let journal = InMemoryJournal::<()>::new();
    populate_journal(
        &journal,
        &[
            make_add_event(1, Id::from_u64(1), 100, 10, Side::Buy),
            make_add_event(2, Id::from_u64(2), 101, 5, Side::Sell),
            make_add_event(3, Id::from_u64(3), 99, 7, Side::Buy),
            make_add_event(4, Id::from_u64(4), 102, 3, Side::Sell),
            make_add_event(5, Id::from_u64(5), 100, 2, Side::Buy),
        ],
    );

    let snap_a = replay_once(&journal, "BTC-USD", 42_000, 1);
    let snap_b = replay_once(&journal, "BTC-USD", 42_000, 1);

    assert!(
        snapshots_match(&snap_a, &snap_b),
        "two replays with identical StubClocks must produce matching snapshots"
    );
}

#[test]
fn replay_with_different_stub_clocks_still_snapshots_match() {
    // Timestamps differ → byte-exact serialization diverges, but the snapshot
    // oracle (which ignores engine-assigned timestamp noise on book state)
    // must still match. Guards against unintentional coupling of the snapshot
    // contract to the clock value.
    let journal = InMemoryJournal::<()>::new();
    populate_journal(
        &journal,
        &[
            make_add_event(1, Id::from_u64(1), 100, 10, Side::Buy),
            make_add_event(2, Id::from_u64(2), 101, 5, Side::Sell),
        ],
    );

    let clock_a: Arc<dyn Clock> = Arc::new(StubClock::starting_at(1_000));
    let clock_b: Arc<dyn Clock> = Arc::new(StubClock::starting_at(9_999_999));

    let (book_a, _) =
        ReplayEngine::<()>::replay_from_with_clock(&journal, 1, "ETH-USD", clock_a)
            .expect("replay A succeeds");
    let (book_b, _) =
        ReplayEngine::<()>::replay_from_with_clock(&journal, 1, "ETH-USD", clock_b)
            .expect("replay B succeeds");

    let snap_a = book_a.create_snapshot(usize::MAX);
    let snap_b = book_b.create_snapshot(usize::MAX);
    assert!(
        snapshots_match(&snap_a, &snap_b),
        "snapshots_match must hold regardless of clock start value"
    );
}

#[test]
fn replay_with_monotonic_clock_is_exercised() {
    // Smoke test — replay_from_with_clock accepts the production clock.
    let journal = InMemoryJournal::<()>::new();
    populate_journal(
        &journal,
        &[make_add_event(1, Id::from_u64(1), 100, 10, Side::Buy)],
    );
    let clock: Arc<dyn Clock> = Arc::new(MonotonicClock);
    let outcome = ReplayEngine::<()>::replay_from_with_clock(&journal, 1, "SOL-USD", clock);
    assert!(outcome.is_ok(), "MonotonicClock replay must succeed");
}

// ─── Property-based test ────────────────────────────────────────────────────

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 128,
        max_shrink_iters: 20_000,
        ..ProptestConfig::default()
    })]

    /// For any valid event stream, two replays with identical [`StubClock`]s
    /// produce matching book snapshots (via [`snapshots_match`]). The
    /// strictly byte-identical event-stream oracle is widened in issue #57;
    /// this proptest is the determinism smoke test #51 owns.
    #[test]
    fn proptest_replay_with_identical_stub_clocks_snapshots_match(
        events in event_stream(1..30),
    ) {
        let journal = InMemoryJournal::<()>::new();
        for event in &events {
            let appended = journal.append(event);
            prop_assert!(appended.is_ok(), "journal append failed");
        }

        let clock_a: Arc<dyn Clock> = Arc::new(StubClock::starting_at(1_000_000));
        let clock_b: Arc<dyn Clock> = Arc::new(StubClock::starting_at(1_000_000));

        let (book_a, seq_a) =
            ReplayEngine::<()>::replay_from_with_clock(&journal, 1, "TEST", clock_a)
                .map_err(|e| TestCaseError::fail(format!("replay A failed: {e:?}")))?;
        let (book_b, seq_b) =
            ReplayEngine::<()>::replay_from_with_clock(&journal, 1, "TEST", clock_b)
                .map_err(|e| TestCaseError::fail(format!("replay B failed: {e:?}")))?;

        prop_assert_eq!(seq_a, seq_b, "last-applied sequence diverged");

        let snap_a = book_a.create_snapshot(usize::MAX);
        let snap_b = book_b.create_snapshot(usize::MAX);
        prop_assert!(
            snapshots_match(&snap_a, &snap_b),
            "snapshots_match contract violated"
        );
    }
}
