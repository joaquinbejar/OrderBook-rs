/******************************************************************************
   Integration workflow tests for the orderbook crate.
   Covers cross-module workflows:
   - Order → Match → Snapshot → Restore round-trip
   - Journal → Replay deterministic state reconstruction
   - Serialization round-trips (JSON, Bincode)
   - BookManager with trade listener end-to-end
   - Order state tracking through full lifecycle
******************************************************************************/

use orderbook_rs::orderbook::manager::{BookManager, BookManagerStd};
use orderbook_rs::orderbook::order_state::OrderStatus;
use orderbook_rs::orderbook::sequencer::{
    InMemoryJournal, Journal, ReplayEngine, SequencerCommand, SequencerEvent, SequencerResult,
    snapshots_match,
};
use orderbook_rs::orderbook::serialization::{EventSerializer, JsonEventSerializer};
use orderbook_rs::orderbook::snapshot::MetricFlags;
use orderbook_rs::{OrderBook, OrderBookSnapshot};
use pricelevel::{Hash32, Id, Price, Quantity, Side, TimeInForce, TimestampMs};

// ─── Order → Match → Snapshot → Restore ────────────────────────────────────

#[test]
fn order_match_snapshot_restore_round_trip() {
    let book = OrderBook::<()>::new("BTC/USD");

    // Place orders on both sides
    let bid_id = Id::new_uuid();
    let ask_id = Id::new_uuid();
    let _ = book.add_limit_order(bid_id, 100, 50, Side::Buy, TimeInForce::Gtc, None);
    let _ = book.add_limit_order(ask_id, 110, 30, Side::Sell, TimeInForce::Gtc, None);

    // Verify pre-match state
    let snap_before = book.create_snapshot(usize::MAX);
    assert_eq!(snap_before.bids.len(), 1);
    assert_eq!(snap_before.asks.len(), 1);

    // Submit a market buy that crosses the spread
    let market_id = Id::new_uuid();
    let result = book.submit_market_order(market_id, 10, Side::Buy);
    assert!(result.is_ok());

    // Snapshot after match
    let snap_after = book.create_snapshot(usize::MAX);
    assert_eq!(snap_after.symbol, "BTC/USD");

    // Restore from snapshot into a new book
    let restored = OrderBook::<()>::new("BTC/USD");
    let restore_result = restored.restore_from_snapshot(snap_after.clone());
    assert!(restore_result.is_ok());

    // Verify restored state matches
    let snap_restored = restored.create_snapshot(usize::MAX);
    assert!(snapshots_match(&snap_after, &snap_restored));
}

#[test]
fn snapshot_enriched_metrics_validation() {
    let book = OrderBook::<()>::new("ETH/USD");
    let _ = book.add_limit_order(Id::new_uuid(), 3000, 100, Side::Buy, TimeInForce::Gtc, None);
    let _ = book.add_limit_order(Id::new_uuid(), 3010, 50, Side::Sell, TimeInForce::Gtc, None);

    // Enriched snapshot with specific metrics
    let flags = MetricFlags::MID_PRICE | MetricFlags::SPREAD | MetricFlags::IMBALANCE;
    let enriched = book.enriched_snapshot_with_metrics(usize::MAX, flags);

    assert!(enriched.mid_price.is_some());
    assert!(enriched.spread_bps.is_some());
    // order_book_imbalance is always calculated (not Option)
    assert!(enriched.order_book_imbalance.abs() <= 1.0);

    // EnrichedSnapshot has bids/asks directly — verify non-empty
    assert!(!enriched.bids.is_empty());
    assert!(!enriched.asks.is_empty());
}

// ─── Journal → Replay Deterministic State ───────────────────────────────────

fn make_sequencer_add(seq: u64, id: Id, price: u128, qty: u64, side: Side) -> SequencerEvent<()> {
    let order = pricelevel::OrderType::Standard {
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
        timestamp_ns: seq.saturating_mul(1_000_000),
        command: SequencerCommand::AddOrder(order),
        result: SequencerResult::OrderAdded { order_id: id },
    }
}

#[test]
fn journal_replay_reconstructs_identical_state() {
    let journal: InMemoryJournal<()> = InMemoryJournal::new();

    // Simulate a sequence of operations
    let ids: Vec<Id> = (0..5).map(|_| Id::new_uuid()).collect();
    assert!(
        journal
            .append(&make_sequencer_add(0, ids[0], 100, 10, Side::Buy))
            .is_ok()
    );
    assert!(
        journal
            .append(&make_sequencer_add(1, ids[1], 95, 20, Side::Buy))
            .is_ok()
    );
    assert!(
        journal
            .append(&make_sequencer_add(2, ids[2], 110, 15, Side::Sell))
            .is_ok()
    );
    assert!(
        journal
            .append(&make_sequencer_add(3, ids[3], 115, 25, Side::Sell))
            .is_ok()
    );
    assert!(
        journal
            .append(&make_sequencer_add(4, ids[4], 90, 5, Side::Buy))
            .is_ok()
    );

    // Full replay
    let (replayed_book, last_seq) =
        ReplayEngine::<()>::replay_from(&journal, 0, "TEST").expect("replay should succeed");
    assert_eq!(last_seq, 4);

    // Build the same state manually
    let manual_book = OrderBook::<()>::new("TEST");
    let _ = manual_book.add_limit_order(ids[0], 100, 10, Side::Buy, TimeInForce::Gtc, None);
    let _ = manual_book.add_limit_order(ids[1], 95, 20, Side::Buy, TimeInForce::Gtc, None);
    let _ = manual_book.add_limit_order(ids[2], 110, 15, Side::Sell, TimeInForce::Gtc, None);
    let _ = manual_book.add_limit_order(ids[3], 115, 25, Side::Sell, TimeInForce::Gtc, None);
    let _ = manual_book.add_limit_order(ids[4], 90, 5, Side::Buy, TimeInForce::Gtc, None);

    let snap_replayed = replayed_book.create_snapshot(usize::MAX);
    let snap_manual = manual_book.create_snapshot(usize::MAX);
    assert!(snapshots_match(&snap_replayed, &snap_manual));
}

#[test]
fn journal_replay_partial_from_sequence() {
    let journal: InMemoryJournal<()> = InMemoryJournal::new();
    let id1 = Id::new_uuid();
    let id2 = Id::new_uuid();
    let id3 = Id::new_uuid();

    assert!(
        journal
            .append(&make_sequencer_add(0, id1, 100, 10, Side::Buy))
            .is_ok()
    );
    assert!(
        journal
            .append(&make_sequencer_add(1, id2, 200, 20, Side::Sell))
            .is_ok()
    );
    assert!(
        journal
            .append(&make_sequencer_add(2, id3, 50, 30, Side::Buy))
            .is_ok()
    );

    // Replay from sequence 1 (skip first event)
    let (book, last_seq) =
        ReplayEngine::<()>::replay_from(&journal, 1, "TEST").expect("replay should succeed");
    assert_eq!(last_seq, 2);

    let snap = book.create_snapshot(usize::MAX);
    // Should have 1 bid (50) and 1 ask (200), NOT the first bid at 100
    assert_eq!(snap.bids.len(), 1);
    assert_eq!(snap.asks.len(), 1);
}

#[test]
fn journal_verify_matches_snapshot() {
    let journal: InMemoryJournal<()> = InMemoryJournal::new();
    let id = Id::new_uuid();
    assert!(
        journal
            .append(&make_sequencer_add(0, id, 500, 100, Side::Buy))
            .is_ok()
    );

    // Replay and snapshot
    let (book, _) =
        ReplayEngine::<()>::replay_from(&journal, 0, "TEST").expect("replay should succeed");
    let expected_snapshot = book.create_snapshot(usize::MAX);

    // Verify should return true
    let verified = ReplayEngine::<()>::verify(&journal, &expected_snapshot);
    assert!(verified.is_ok());
    assert!(verified.expect("verify should succeed"));
}

// ─── Serialization Round-Trips ──────────────────────────────────────────────

#[test]
fn json_serializer_trade_result_round_trip() {
    let serializer = JsonEventSerializer;

    let book = OrderBook::<()>::new("BTC/USD");
    let _ = book.add_limit_order(Id::new_uuid(), 100, 50, Side::Sell, TimeInForce::Gtc, None);

    let match_result = book.submit_market_order(Id::new_uuid(), 10, Side::Buy);
    assert!(match_result.is_ok());
    let trade_result = orderbook_rs::TradeResult::new(
        "BTC/USD".to_string(),
        match_result.expect("should have match result"),
    );

    // Serialize
    let bytes = serializer.serialize_trade(&trade_result);
    assert!(bytes.is_ok());
    let bytes = bytes.expect("serialize should succeed");
    assert!(!bytes.is_empty());

    // Deserialize
    let deserialized = serializer.deserialize_trade(&bytes);
    assert!(deserialized.is_ok());
    let deserialized = deserialized.expect("deserialize should succeed");
    assert_eq!(deserialized.symbol, "BTC/USD");
}

#[test]
fn json_serializer_book_change_round_trip() {
    let serializer = JsonEventSerializer;

    let event = orderbook_rs::orderbook::book_change_event::PriceLevelChangedEvent {
        side: Side::Buy,
        price: 3000,
        quantity: 100,
        engine_seq: 0,
    };

    let bytes = serializer.serialize_book_change(&event);
    assert!(bytes.is_ok());
    let bytes = bytes.expect("serialize should succeed");

    let deserialized = serializer.deserialize_book_change(&bytes);
    assert!(deserialized.is_ok());
    let deserialized = deserialized.expect("deserialize should succeed");
    assert_eq!(deserialized.price, 3000);
    assert_eq!(deserialized.quantity, 100);
}

#[cfg(feature = "bincode")]
#[test]
fn bincode_serializer_trade_result_round_trip() {
    let serializer = orderbook_rs::BincodeEventSerializer;

    let book = OrderBook::<()>::new("BTC/USD");
    let _ = book.add_limit_order(Id::new_uuid(), 100, 50, Side::Sell, TimeInForce::Gtc, None);

    let match_result = book.submit_market_order(Id::new_uuid(), 10, Side::Buy);
    assert!(match_result.is_ok());
    let trade_result = orderbook_rs::TradeResult::new(
        "BTC/USD".to_string(),
        match_result.expect("should have match result"),
    );

    let bytes = serializer.serialize_trade(&trade_result);
    assert!(bytes.is_ok());
    let bytes = bytes.expect("serialize should succeed");

    let deserialized = serializer.deserialize_trade(&bytes);
    assert!(deserialized.is_ok());
    let deserialized = deserialized.expect("deserialize should succeed");
    assert_eq!(deserialized.symbol, "BTC/USD");
}

// ─── BookManager with Trade Listener ────────────────────────────────────────

#[test]
fn book_manager_multi_book_operations() {
    let mut mgr: BookManagerStd<()> = BookManagerStd::new();
    mgr.add_book("BTC/USD");

    // Place a sell order
    let book = mgr.get_book("BTC/USD").expect("BTC/USD book must exist");
    let _ = book.add_limit_order(Id::new_uuid(), 100, 50, Side::Sell, TimeInForce::Gtc, None);

    // Place a buy market order that crosses
    let book = mgr.get_book("BTC/USD").expect("BTC/USD book must exist");
    let result = book.submit_market_order(Id::new_uuid(), 10, Side::Buy);
    assert!(result.is_ok());

    // Verify book state after match
    let book = mgr.get_book("BTC/USD").expect("BTC/USD book must exist");
    let snap = book.create_snapshot(usize::MAX);
    assert_eq!(snap.asks.len(), 1);
    // Remaining ask quantity should be 40
    assert_eq!(snap.asks[0].visible_quantity(), 40);
}

#[test]
fn book_manager_multi_book_independent_state() {
    let mut mgr: BookManagerStd<()> = BookManagerStd::new();
    mgr.add_book("BTC/USD");
    mgr.add_book("ETH/USD");

    let btc_book = mgr.get_book("BTC/USD").expect("BTC/USD must exist");
    let _ = btc_book.add_limit_order(Id::new_uuid(), 50000, 1, Side::Buy, TimeInForce::Gtc, None);
    let eth_book = mgr.get_book("ETH/USD").expect("ETH/USD must exist");
    let _ = eth_book.add_limit_order(Id::new_uuid(), 3000, 10, Side::Sell, TimeInForce::Gtc, None);

    // Each book should have independent state
    let btc_snap = mgr
        .get_book("BTC/USD")
        .map(|b| b.create_snapshot(usize::MAX));
    let eth_snap = mgr
        .get_book("ETH/USD")
        .map(|b| b.create_snapshot(usize::MAX));

    assert!(btc_snap.is_some());
    assert!(eth_snap.is_some());

    let btc = btc_snap.expect("btc should exist");
    let eth = eth_snap.expect("eth should exist");

    assert_eq!(btc.bids.len(), 1);
    assert!(btc.asks.is_empty());
    assert!(eth.bids.is_empty());
    assert_eq!(eth.asks.len(), 1);
}

// ─── Order State Lifecycle ──────────────────────────────────────────────────

#[test]
fn order_lifecycle_open_to_filled() {
    let mut book = OrderBook::<()>::new("BTC/USD");
    book.set_order_state_tracker(orderbook_rs::orderbook::order_state::OrderStateTracker::new());

    // Add a sell order
    let sell_id = Id::new_uuid();
    let _ = book.add_limit_order(sell_id, 100, 10, Side::Sell, TimeInForce::Gtc, None);

    // Check initial state
    let status = book.order_status(sell_id);
    assert!(status.is_some());
    assert!(matches!(status, Some(OrderStatus::Open)));

    // Fill it completely with a market buy
    let _ = book.submit_market_order(Id::new_uuid(), 10, Side::Buy);

    // Check final state
    let status = book.order_status(sell_id);
    assert!(status.is_some());
    assert!(matches!(status, Some(OrderStatus::Filled { .. })));

    // Check history
    let history = book.get_order_history(sell_id);
    assert!(history.is_some());
    let history = history.expect("history should exist");
    assert!(history.len() >= 2); // At least Open → Filled
}

#[test]
fn order_lifecycle_open_to_cancelled() {
    let mut book = OrderBook::<()>::new("BTC/USD");
    book.set_order_state_tracker(orderbook_rs::orderbook::order_state::OrderStateTracker::new());

    let sell_id = Id::new_uuid();
    let _ = book.add_limit_order(sell_id, 100, 20, Side::Sell, TimeInForce::Gtc, None);

    // Verify it's open
    let status = book.order_status(sell_id);
    assert!(matches!(status, Some(OrderStatus::Open)));

    // Cancel the order
    let cancel_result = book.cancel_order(sell_id);
    assert!(cancel_result.is_ok());

    let status = book.order_status(sell_id);
    assert!(matches!(status, Some(OrderStatus::Cancelled { .. })));
}

// ─── File Journal End-to-End ────────────────────────────────────────────────

#[cfg(feature = "journal")]
#[test]
fn file_journal_write_read_verify_round_trip() {
    use orderbook_rs::orderbook::sequencer::FileJournal;

    let dir = tempfile::tempdir().expect("should create temp dir");
    let journal = FileJournal::<()>::open(dir.path()).expect("should open journal");

    let id1 = Id::new_uuid();
    let id2 = Id::new_uuid();
    assert!(
        journal
            .append(&make_sequencer_add(0, id1, 100, 10, Side::Buy))
            .is_ok()
    );
    assert!(
        journal
            .append(&make_sequencer_add(1, id2, 200, 20, Side::Sell))
            .is_ok()
    );

    // Verify integrity
    assert!(journal.verify_integrity().is_ok());

    // Read back
    let entries: Vec<_> = journal
        .read_from(0)
        .expect("should read")
        .collect::<Result<Vec<_>, _>>()
        .expect("should collect");
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].event.sequence_num, 0);
    assert_eq!(entries[1].event.sequence_num, 1);

    // Replay from file journal
    let (book, last_seq) =
        ReplayEngine::<()>::replay_from(&journal, 0, "TEST").expect("replay should succeed");
    assert_eq!(last_seq, 1);

    let snap = book.create_snapshot(usize::MAX);
    assert_eq!(snap.bids.len(), 1);
    assert_eq!(snap.asks.len(), 1);
}

// ─── Snapshot JSON Serialization ────────────────────────────────────────────

#[test]
fn snapshot_json_round_trip() {
    let book = OrderBook::<()>::new("BTC/USD");
    let _ = book.add_limit_order(Id::new_uuid(), 100, 10, Side::Buy, TimeInForce::Gtc, None);
    let _ = book.add_limit_order(Id::new_uuid(), 110, 5, Side::Sell, TimeInForce::Gtc, None);

    let snapshot = book.create_snapshot(usize::MAX);
    let json = serde_json::to_string(&snapshot);
    assert!(json.is_ok());
    let json = json.expect("serialize should succeed");

    let deserialized: Result<OrderBookSnapshot, _> = serde_json::from_str(&json);
    assert!(deserialized.is_ok());
    let deserialized = deserialized.expect("deserialize should succeed");
    assert!(snapshots_match(&snapshot, &deserialized));
}

// ─── Mass Cancel with Snapshot Verification ─────────────────────────────────

#[test]
fn mass_cancel_then_snapshot_shows_empty_book() {
    let book = OrderBook::<()>::new("BTC/USD");

    // Add multiple orders
    for i in 0..10 {
        let _ = book.add_limit_order(
            Id::new_uuid(),
            100u128.saturating_add(i),
            10,
            Side::Buy,
            TimeInForce::Gtc,
            None,
        );
        let _ = book.add_limit_order(
            Id::new_uuid(),
            200u128.saturating_add(i),
            10,
            Side::Sell,
            TimeInForce::Gtc,
            None,
        );
    }

    let snap_before = book.create_snapshot(usize::MAX);
    assert_eq!(snap_before.bids.len(), 10);
    assert_eq!(snap_before.asks.len(), 10);

    // Mass cancel all
    let result = book.cancel_all_orders();
    assert!(result.cancelled_count() > 0);

    let snap_after = book.create_snapshot(usize::MAX);
    assert!(snap_after.bids.is_empty());
    assert!(snap_after.asks.is_empty());
}

// ─── Validation + Matching Integration ──────────────────────────────────────

#[test]
fn validation_prevents_invalid_then_valid_order_succeeds() {
    let mut book = OrderBook::<()>::new("BTC/USD");
    book.set_tick_size(10);
    book.set_lot_size(5);

    // Invalid: price not aligned to tick
    let result = book.add_limit_order(Id::new_uuid(), 105, 10, Side::Buy, TimeInForce::Gtc, None);
    assert!(result.is_err());

    // Invalid: quantity not aligned to lot
    let result = book.add_limit_order(Id::new_uuid(), 100, 7, Side::Buy, TimeInForce::Gtc, None);
    assert!(result.is_err());

    // Valid: both aligned
    let result = book.add_limit_order(Id::new_uuid(), 100, 10, Side::Buy, TimeInForce::Gtc, None);
    assert!(result.is_ok());

    let snap = book.create_snapshot(usize::MAX);
    assert_eq!(snap.bids.len(), 1);
}
