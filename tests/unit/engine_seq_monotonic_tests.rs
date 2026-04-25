//! Property-based test for the global `engine_seq` monotonicity contract.
//!
//! For any random op stream applied to a fresh `OrderBook`, every observed
//! `engine_seq` across both the trade-listener stream and the
//! price-level-changed-listener stream must be strictly increasing under
//! `<`. This is the single most important invariant on the outbound
//! sequencing surface — consumers rely on it to detect gaps and to merge
//! events from the two streams into a single ordered view.

use super::common::strategies::event_stream;

use orderbook_rs::OrderBook;
use orderbook_rs::orderbook::book_change_event::{
    PriceLevelChangedEvent, PriceLevelChangedListener,
};
use orderbook_rs::orderbook::sequencer::{SequencerCommand, SequencerEvent};
use orderbook_rs::orderbook::trade::{TradeListener, TradeResult};
use proptest::prelude::*;
use std::sync::{Arc, Mutex};

/// Replays every `AddOrder` command in `events` against a fresh `OrderBook`
/// and returns the merged sequence of every observed `engine_seq` from both
/// outbound streams, in the order events were emitted.
fn collect_observed_engine_seqs(events: &[SequencerEvent<()>]) -> Vec<u64> {
    let observed: Arc<Mutex<Vec<u64>>> = Arc::new(Mutex::new(Vec::new()));

    let trade_observed = Arc::clone(&observed);
    let trade_listener: TradeListener = Arc::new(move |result: &TradeResult| {
        let mut guard = trade_observed
            .lock()
            .expect("observed mutex poisoned in trade listener");
        guard.push(result.engine_seq);
    });

    let price_observed = Arc::clone(&observed);
    let price_listener: PriceLevelChangedListener =
        Arc::new(move |event: PriceLevelChangedEvent| {
            let mut guard = price_observed
                .lock()
                .expect("observed mutex poisoned in price listener");
            guard.push(event.engine_seq);
        });

    let book = OrderBook::<()>::with_trade_and_price_level_listener(
        "TEST",
        trade_listener,
        price_listener,
    );

    for event in events {
        if let SequencerCommand::AddOrder(order) = &event.command {
            let _ = book.add_order(*order);
        }
    }

    let guard = observed
        .lock()
        .expect("observed mutex poisoned after replay");
    guard.clone()
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 256,
        max_shrink_iters: 50_000,
        ..ProptestConfig::default()
    })]

    /// For any valid op stream, every `engine_seq` observed on the merged
    /// outbound stream (trades + price-level changes) is strictly
    /// monotonically increasing.
    #[test]
    fn proptest_engine_seq_strictly_monotonic_across_streams(
        events in event_stream(1..40),
    ) {
        let observed = collect_observed_engine_seqs(&events);

        if observed.len() >= 2 {
            for window in observed.windows(2) {
                prop_assert!(
                    window[0] < window[1],
                    "engine_seq monotonicity violated: {} >= {} at adjacent emissions",
                    window[0], window[1]
                );
            }
        }
    }

    /// Every observed `engine_seq` is unique — the `AtomicU64::fetch_add`
    /// contract on `next_engine_seq` guarantees this; the proptest is a
    /// regression guard against accidental shared-counter clones.
    #[test]
    fn proptest_engine_seq_values_are_unique(events in event_stream(1..40)) {
        let observed = collect_observed_engine_seqs(&events);

        let unique: std::collections::HashSet<u64> = observed.iter().copied().collect();
        prop_assert_eq!(unique.len(), observed.len(), "duplicate engine_seq emitted");
    }
}
