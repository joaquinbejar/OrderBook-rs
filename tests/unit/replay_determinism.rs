//! Property-based tests for replay determinism.
//!
//! These tests verify that replaying a recorded command stream from the journal
//! produces byte-identical execution results and snapshot consistency.

#[cfg(feature = "journal")]
mod inner {
    use orderbook_rs::orderbook::sequencer::{
        InMemoryJournal, Journal, ReplayEngine, SequencerCommand, SequencerEvent, SequencerResult,
        snapshots_match,
    };
    use pricelevel::{Hash32, Id, OrderType, Price, Quantity, Side, TimeInForce, TimestampMs};
    use proptest::prelude::*;

    fn make_standard_order(id: Id, price: u128, qty: u64, side: Side) -> OrderType<()> {
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

    /// Deterministic test: replaying the same journal twice produces identical snapshots.
    #[test]
    fn replay_twice_produces_identical_result() {
        let journal: InMemoryJournal<()> = InMemoryJournal::new();

        // Build journal with deterministic orders.
        let id1 = Id::new_uuid();
        let id2 = Id::new_uuid();
        let order1 = make_standard_order(id1, 100, 10, Side::Buy);
        let order2 = make_standard_order(id2, 101, 20, Side::Sell);

        let evt1 = SequencerEvent {
            sequence_num: 0,
            timestamp_ns: 0,
            command: SequencerCommand::AddOrder(order1),
            result: SequencerResult::OrderAdded { order_id: id1 },
        };
        let evt2 = SequencerEvent {
            sequence_num: 1,
            timestamp_ns: 1,
            command: SequencerCommand::AddOrder(order2),
            result: SequencerResult::OrderAdded { order_id: id2 },
        };
        assert!(journal.append(&evt1).is_ok());
        assert!(journal.append(&evt2).is_ok());

        // Replay once.
        let (book1, _) = ReplayEngine::<()>::replay_from(&journal, 0, "TEST")
            .expect("first replay should succeed");

        // Replay again.
        let (book2, _) = ReplayEngine::<()>::replay_from(&journal, 0, "TEST")
            .expect("second replay should succeed");

        // Snapshots should match structurally (via snapshots_match oracle).
        let snap1 = book1.create_snapshot(usize::MAX);
        let snap2 = book2.create_snapshot(usize::MAX);
        assert!(
            snapshots_match(&snap1, &snap2),
            "replayed snapshots should match"
        );
    }

    // Proptest: random sequence of adds deterministically replays.
    proptest! {
        #[test]
        fn prop_replay_deterministic_across_runs(
            add_count in 1usize..5,
        ) {
            let journal: InMemoryJournal<()> = InMemoryJournal::new();

            // Build deterministic journal from add_count.
            for (seq, i) in (0..add_count).enumerate() {
                let id = Id::new_uuid();
                let price = 100 + (i as u128 * 10);
                let order = make_standard_order(
                    id,
                    price,
                    10,
                    if i % 2 == 0 { Side::Buy } else { Side::Sell },
                );
                let evt = SequencerEvent {
                    sequence_num: seq as u64,
                    timestamp_ns: seq as u64,
                    command: SequencerCommand::AddOrder(order),
                    result: SequencerResult::OrderAdded { order_id: id },
                };
                assert!(journal.append(&evt).is_ok());
            }

            // Replay multiple times.
            let (book1, _) = ReplayEngine::<()>::replay_from(&journal, 0, "TEST")
                .expect("first replay should succeed");
            let (book2, _) = ReplayEngine::<()>::replay_from(&journal, 0, "TEST")
                .expect("second replay should succeed");

            // Snapshots must match.
            let snap1 = book1.create_snapshot(usize::MAX);
            let snap2 = book2.create_snapshot(usize::MAX);
            assert!(snapshots_match(&snap1, &snap2), "prop: replayed snapshots should be identical");
        }
    }
}

#[cfg(not(feature = "journal"))]
mod no_journal {
    #[test]
    fn journal_feature_not_enabled() {
        // This test file requires the `journal` feature.
        // If journal is disabled, this test passes as a no-op.
    }
}
