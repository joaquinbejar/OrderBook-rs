/******************************************************************************
   A submit that traded and then returned `Err` must replay.

   `add_order` emits real fills before failing on an unfilled IOC remainder
   or an STP-cancelled taker, so a sequencer records those commands as
   rejections over a book they already mutated. Replay used to skip every
   rejected event — resurrecting the consumed liquidity — and to abort on
   any `Err` from a submit it did apply. It now decides by the recorded
   reject code: a `RejectedWithCode` submit whose code replay can reproduce
   is re-executed and must fail the same way again, a code replay cannot
   reproduce (the kill switch) is skipped, and the string-only `Rejected`
   keeps the historical skip. Two things the code alone cannot express are
   recorded next to it: `may_have_mutated`, which pulls the
   residual-admission failure out of the `Other(0)` skip without dragging
   the clock-dependent expired-at-admission rejection with it, and
   `stp_mode`, which lets replay refuse a configuration the reject code
   would have accepted. Each test builds its journal the way a sequencer
   does: run the command against a live book, record the outcome the
   command API returned.
******************************************************************************/

use orderbook_rs::orderbook::sequencer::{
    InMemoryJournal, Journal, ReplayBookConfig, ReplayEngine, ReplayError, SequencerCommand,
    SequencerEvent, SequencerResult, snapshots_match,
};
use orderbook_rs::{Clock, OrderBook, OrderBookError, RejectReason, STPMode, StubClock};
use pricelevel::{
    Hash32, Id, OrderType, Price, PriceLevelError, Quantity, Side, TimeInForce, TimestampMs,
};
use std::cell::RefCell;
use std::sync::Arc;

const SYMBOL: &str = "RSE";

fn stub_clock() -> Arc<dyn Clock> {
    Arc::new(StubClock::starting_at(0))
}

fn user(byte: u8) -> Hash32 {
    Hash32::new([byte; 32])
}

fn order(
    id: u64,
    price: u128,
    qty: u64,
    side: Side,
    tif: TimeInForce,
    user_id: Hash32,
) -> OrderType<()> {
    OrderType::Standard {
        id: Id::from_u64(id),
        price: Price::new(price),
        quantity: Quantity::new(qty),
        side,
        time_in_force: tif,
        user_id,
        timestamp: TimestampMs::new(0),
        extra_fields: (),
    }
}

fn append(
    journal: &InMemoryJournal<()>,
    seq: u64,
    command: SequencerCommand<()>,
    result: SequencerResult,
) {
    journal
        .append(&SequencerEvent {
            sequence_num: seq,
            timestamp_ns: seq,
            command,
            result,
        })
        .expect("journal append");
}

/// A minimal sequencer: execute against the live book, journal the command
/// with the outcome the command API returned — `OrderAdded` on `Ok`,
/// `RejectedWithCode` on `Err` via the `From<&OrderBookError>` impl.
fn sequence(
    live: &OrderBook<()>,
    journal: &InMemoryJournal<()>,
    seq: u64,
    order: OrderType<()>,
) -> Result<(), OrderBookError> {
    let id = order.id();
    let outcome = live.add_order(order);
    let result = match &outcome {
        Ok(_) => SequencerResult::OrderAdded { order_id: id },
        Err(e) => SequencerResult::from(e),
    };
    append(journal, seq, SequencerCommand::AddOrder(order), result);
    outcome.map(|_| ())
}

fn replay(
    journal: &InMemoryJournal<()>,
    config: &ReplayBookConfig,
) -> Result<(OrderBook<()>, u64), ReplayError> {
    ReplayEngine::<()>::replay_from_with_clock_and_config(journal, 0, SYMBOL, stub_clock(), config)
}

/// Structural equality plus the last trade price, which `snapshots_match`
/// cannot see once a level has been emptied: a maker that was traded and a
/// maker that was cancelled leave the same (absent) level behind.
fn assert_books_match(replayed: &OrderBook<()>, live: &OrderBook<()>) {
    assert!(
        snapshots_match(
            &replayed.create_snapshot(usize::MAX),
            &live.create_snapshot(usize::MAX)
        ),
        "replayed book diverged from the live book"
    );
    assert_eq!(
        replayed.last_trade_price(),
        live.last_trade_price(),
        "the replayed book traded differently from the live book"
    );
}

fn code_of(result: &SequencerResult) -> Option<RejectReason> {
    match result {
        SequencerResult::RejectedWithCode { code, .. } => Some(*code),
        _ => None,
    }
}

/// An IOC that consumes the whole book and then reports its unfillable
/// remainder: the fills happened, so replay must consume them too, and the
/// re-executed rejection counts as an applied event.
#[test]
fn ioc_remainder_error_replays_its_fills() {
    let live = OrderBook::<()>::with_clock(SYMBOL, stub_clock());
    let journal: InMemoryJournal<()> = InMemoryJournal::new();

    sequence(
        &live,
        &journal,
        0,
        order(1, 100, 10, Side::Sell, TimeInForce::Gtc, Hash32::zero()),
    )
    .expect("seed ask");
    let err = sequence(
        &live,
        &journal,
        1,
        order(2, 100, 15, Side::Buy, TimeInForce::Ioc, Hash32::zero()),
    )
    .expect_err("the IOC remainder is unfillable");
    assert!(
        matches!(err, OrderBookError::InsufficientLiquidity { .. }),
        "expected InsufficientLiquidity, got {err:?}"
    );
    assert_eq!(live.best_ask(), None, "the live ask was consumed");

    let (replayed, last_applied) =
        replay(&journal, &ReplayBookConfig::default()).expect("replay succeeds");
    assert_eq!(
        replayed.best_ask(),
        None,
        "the replayed ask was consumed too"
    );
    assert_books_match(&replayed, &live);
    assert_eq!(last_applied, 1, "the re-executed rejection was applied");
}

/// A taker that fills against another user and is then cancelled by STP:
/// the non-self fills happened, so replay must consume them too.
#[test]
fn stp_cancelled_taker_replays_its_non_self_fills() {
    let mut live = OrderBook::<()>::with_clock(SYMBOL, stub_clock());
    live.set_stp_mode(STPMode::CancelTaker);
    let journal: InMemoryJournal<()> = InMemoryJournal::new();

    sequence(
        &live,
        &journal,
        0,
        order(1, 100, 5, Side::Sell, TimeInForce::Gtc, user(2)),
    )
    .expect("seed non-self maker");
    sequence(
        &live,
        &journal,
        1,
        order(2, 100, 9, Side::Sell, TimeInForce::Gtc, user(1)),
    )
    .expect("seed same-user maker");
    let err = sequence(
        &live,
        &journal,
        2,
        order(3, 100, 9, Side::Buy, TimeInForce::Gtc, user(1)),
    )
    .expect_err("the taker reaches its own maker");
    assert!(
        matches!(err, OrderBookError::SelfTradePrevented { .. }),
        "expected SelfTradePrevented, got {err:?}"
    );
    assert!(
        live.get_order(Id::from_u64(1)).is_none(),
        "the non-self maker was consumed live"
    );

    let config = ReplayBookConfig::new(None, STPMode::CancelTaker, None, None, None, None);
    let (replayed, last_applied) = replay(&journal, &config).expect("replay succeeds");
    assert!(
        replayed.get_order(Id::from_u64(1)).is_none(),
        "the non-self maker was consumed on replay too"
    );
    assert_eq!(
        replayed
            .get_order(Id::from_u64(2))
            .expect("the same-user maker still rests")
            .visible_quantity()
            .as_u64(),
        9
    );
    assert_books_match(&replayed, &live);
    assert_eq!(last_applied, 2, "the re-executed rejection was applied");
}

/// The market commands take the same path. Without a user id they cannot
/// carry STP effects, so their only rejection is the no-fill one, which
/// re-executes to the same no-op and counts as applied.
#[test]
fn market_rejections_without_fills_replay_as_the_same_no_op() {
    let live = OrderBook::<()>::with_clock(SYMBOL, stub_clock());
    let journal: InMemoryJournal<()> = InMemoryJournal::new();

    let err = live
        .submit_market_order(Id::from_u64(1), 5, Side::Buy)
        .expect_err("empty book");
    assert!(matches!(err, OrderBookError::InsufficientLiquidity { .. }));
    append(
        &journal,
        0,
        SequencerCommand::MarketOrder {
            id: Id::from_u64(1),
            quantity: 5,
            side: Side::Buy,
        },
        SequencerResult::from(&err),
    );

    let err = live
        .submit_market_order_by_amount(Id::from_u64(2), 500, Side::Buy)
        .expect_err("empty book");
    assert!(matches!(
        err,
        OrderBookError::InsufficientLiquidityNotional { .. }
    ));
    append(
        &journal,
        1,
        SequencerCommand::MarketOrderByAmount {
            id: Id::from_u64(2),
            amount: 500,
            side: Side::Buy,
        },
        SequencerResult::from(&err),
    );

    let (replayed, last_applied) =
        replay(&journal, &ReplayBookConfig::default()).expect("replay succeeds");
    assert_books_match(&replayed, &live);
    assert_eq!(last_applied, 1, "both market rejections were re-executed");
}

/// A kill-switch rejection never touches the book and its trigger is not
/// part of `ReplayBookConfig`, so replay must skip it rather than re-execute
/// it: re-executing would consume the ask the live book never touched.
/// The skip does not advance the applied sequence or the progress callback.
#[test]
fn kill_switch_rejection_is_skipped_and_the_books_match() {
    let live = OrderBook::<()>::with_clock(SYMBOL, stub_clock());
    let journal: InMemoryJournal<()> = InMemoryJournal::new();

    sequence(
        &live,
        &journal,
        0,
        order(1, 100, 10, Side::Sell, TimeInForce::Gtc, Hash32::zero()),
    )
    .expect("seed ask");
    live.engage_kill_switch();
    let err = sequence(
        &live,
        &journal,
        1,
        order(2, 100, 15, Side::Buy, TimeInForce::Ioc, Hash32::zero()),
    )
    .expect_err("halted");
    assert!(matches!(err, OrderBookError::KillSwitchActive));
    assert_eq!(live.best_ask(), Some(100), "the halted IOC touched nothing");

    let progress: RefCell<Vec<(u64, u64)>> = RefCell::new(Vec::new());
    let (replayed, last_applied) = ReplayEngine::<()>::replay_from_with_clock_and_progress(
        &journal,
        0,
        SYMBOL,
        stub_clock(),
        |count, seq| progress.borrow_mut().push((count, seq)),
    )
    .expect("replay succeeds");

    assert_eq!(replayed.best_ask(), Some(100), "the ask survived replay");
    assert_books_match(&replayed, &live);
    assert_eq!(
        last_applied, 0,
        "the skipped rejection did not advance the applied sequence"
    );
    assert_eq!(
        progress.into_inner(),
        vec![(1, 0)],
        "the progress callback saw only the dispatched event"
    );
}

/// A pure admission rejection replay can reproduce (a tick violation, with
/// the tick carried in the config) re-executes to the same error, leaves
/// the book untouched, and counts as applied.
#[test]
fn tick_rejection_with_a_matching_config_replays_as_the_same_no_op() {
    let mut live = OrderBook::<()>::with_clock(SYMBOL, stub_clock());
    live.set_tick_size(10);
    let journal: InMemoryJournal<()> = InMemoryJournal::new();

    sequence(
        &live,
        &journal,
        0,
        order(1, 100, 10, Side::Sell, TimeInForce::Gtc, Hash32::zero()),
    )
    .expect("a tick-aligned order is admitted");
    let err = sequence(
        &live,
        &journal,
        1,
        order(2, 105, 10, Side::Sell, TimeInForce::Gtc, Hash32::zero()),
    )
    .expect_err("105 is not a multiple of the tick");
    assert!(
        matches!(err, OrderBookError::InvalidTickSize { .. }),
        "expected InvalidTickSize, got {err:?}"
    );

    let config = ReplayBookConfig::new(None, STPMode::None, Some(10), None, None, None);
    let (replayed, last_applied) = replay(&journal, &config).expect("replay succeeds");
    assert!(
        replayed.get_order(Id::from_u64(2)).is_none(),
        "the tick-rejected order did not rest on replay"
    );
    assert_books_match(&replayed, &live);
    assert_eq!(last_applied, 1, "the re-executed rejection was applied");
}

/// The same journal replayed without the tick in its config: the rejected
/// order now rests, which is a divergence, and replay says so instead of
/// returning a book that quietly differs from the live one.
#[test]
fn rejected_submit_that_succeeds_on_replay_aborts() {
    let mut live = OrderBook::<()>::with_clock(SYMBOL, stub_clock());
    live.set_tick_size(10);
    let journal: InMemoryJournal<()> = InMemoryJournal::new();

    sequence(
        &live,
        &journal,
        0,
        order(1, 100, 10, Side::Sell, TimeInForce::Gtc, Hash32::zero()),
    )
    .expect("seed ask");
    sequence(
        &live,
        &journal,
        1,
        order(2, 105, 10, Side::Sell, TimeInForce::Gtc, Hash32::zero()),
    )
    .expect_err("105 is not a multiple of the tick");

    let err = replay(&journal, &ReplayBookConfig::default())
        .err()
        .expect("a rejected submit that rests on replay is a divergence");
    match err {
        ReplayError::OutcomeMismatch {
            sequence_num,
            recorded,
            actual,
        } => {
            assert_eq!(sequence_num, 1);
            assert_eq!(recorded, RejectReason::InvalidPrice);
            assert!(actual.is_none(), "replay succeeded where live rejected");
        }
        other => panic!("expected OutcomeMismatch, got {other:?}"),
    }
}

/// A re-execution that fails under a different code is just as much a
/// divergence as one that succeeds.
#[test]
fn rejected_submit_that_fails_differently_on_replay_aborts() {
    let journal: InMemoryJournal<()> = InMemoryJournal::new();
    let ask = order(1, 100, 10, Side::Sell, TimeInForce::Gtc, Hash32::zero());
    append(
        &journal,
        0,
        SequencerCommand::AddOrder(ask),
        SequencerResult::OrderAdded {
            order_id: Id::from_u64(1),
        },
    );
    // Journaled as a liquidity rejection, but the re-add of an id that
    // already rests is a duplicate-id rejection on replay.
    append(
        &journal,
        1,
        SequencerCommand::AddOrder(ask),
        SequencerResult::RejectedWithCode {
            reason: "insufficient liquidity".to_string(),
            code: RejectReason::InsufficientLiquidity,
            may_have_mutated: true,
            stp_mode: None,
        },
    );

    let err = replay(&journal, &ReplayBookConfig::default())
        .err()
        .expect("a different verdict is a divergence");
    match &err {
        ReplayError::OutcomeMismatch {
            sequence_num,
            recorded,
            actual,
        } => {
            assert_eq!(*sequence_num, 1);
            assert_eq!(*recorded, RejectReason::InsufficientLiquidity);
            assert!(
                matches!(actual, Some(OrderBookError::DuplicateOrderId { .. })),
                "expected the duplicate-id error, got {actual:?}"
            );
        }
        other => panic!("expected OutcomeMismatch, got {other:?}"),
    }
    let text = err.to_string();
    assert!(
        text.contains("sequence 1")
            && text.contains("insufficient liquidity")
            && text.contains("duplicate order id"),
        "the message names the sequence and both verdicts: {text}"
    );
}

/// A string-only `Rejected` carries no code to decide by, so it keeps the
/// historical skip — including, for a submit that traded first, the
/// pre-existing gap. This pins that such journals still replay rather than
/// abort, and that closing the gap needs `RejectedWithCode`.
#[test]
fn legacy_string_rejection_keeps_the_skip() {
    let live = OrderBook::<()>::with_clock(SYMBOL, stub_clock());
    let journal: InMemoryJournal<()> = InMemoryJournal::new();

    sequence(
        &live,
        &journal,
        0,
        order(1, 100, 10, Side::Sell, TimeInForce::Gtc, Hash32::zero()),
    )
    .expect("seed ask");
    let ioc = order(2, 100, 15, Side::Buy, TimeInForce::Ioc, Hash32::zero());
    let err = live
        .add_order(ioc)
        .expect_err("the IOC remainder is unfillable");
    append(
        &journal,
        1,
        SequencerCommand::AddOrder(ioc),
        SequencerResult::Rejected {
            reason: err.to_string(),
        },
    );
    assert_eq!(live.best_ask(), None, "the live ask was consumed");

    let (replayed, last_applied) =
        replay(&journal, &ReplayBookConfig::default()).expect("a legacy rejection still replays");
    assert_eq!(last_applied, 0, "the string-only rejection was skipped");
    assert_eq!(
        replayed.best_ask(),
        Some(100),
        "without a code the traded IOC is skipped and the ask is rebuilt"
    );
}

/// A journal is expected to record the outcome the command API returned.
/// A success recorded for a submit that returned `Err` replays as a
/// success/failure disagreement and aborts, as it did before.
#[test]
fn success_journaled_for_a_failed_submit_aborts() {
    let live = OrderBook::<()>::with_clock(SYMBOL, stub_clock());
    let journal: InMemoryJournal<()> = InMemoryJournal::new();

    sequence(
        &live,
        &journal,
        0,
        order(1, 100, 10, Side::Sell, TimeInForce::Gtc, Hash32::zero()),
    )
    .expect("seed ask");
    let ioc = order(2, 100, 15, Side::Buy, TimeInForce::Ioc, Hash32::zero());
    live.add_order(ioc)
        .expect_err("the IOC remainder is unfillable");
    append(
        &journal,
        1,
        SequencerCommand::AddOrder(ioc),
        SequencerResult::OrderAdded {
            order_id: Id::from_u64(2),
        },
    );

    let err = replay(&journal, &ReplayBookConfig::default())
        .err()
        .expect("a success recorded for a failed submit must abort");
    assert!(
        matches!(
            err,
            ReplayError::OrderBookError {
                sequence_num: 1,
                source: OrderBookError::InsufficientLiquidity { .. },
            }
        ),
        "expected an aborting OrderBookError, got {err:?}"
    );
}

/// An error replay did not expect on a journaled success is still a hard
/// failure.
#[test]
fn unexpected_submit_error_aborts_replay() {
    let journal: InMemoryJournal<()> = InMemoryJournal::new();
    for seq in 0..2 {
        append(
            &journal,
            seq,
            SequencerCommand::AddOrder(order(
                1,
                100,
                10,
                Side::Sell,
                TimeInForce::Gtc,
                Hash32::zero(),
            )),
            SequencerResult::OrderAdded {
                order_id: Id::from_u64(1),
            },
        );
    }

    let err = ReplayEngine::<()>::replay_from(&journal, 0, SYMBOL)
        .err()
        .expect("the duplicate id must abort replay");
    assert!(
        matches!(
            err,
            ReplayError::OrderBookError {
                sequence_num: 1,
                ..
            }
        ),
        "expected an aborting OrderBookError, got {err:?}"
    );
}

/// A re-executed rejection is an applied event: it advances the applied
/// count, the last applied sequence and the progress callback, exactly like
/// the successful command before it.
#[test]
fn re_executed_rejection_advances_the_applied_sequence_and_progress() {
    let live = OrderBook::<()>::with_clock(SYMBOL, stub_clock());
    let journal: InMemoryJournal<()> = InMemoryJournal::new();

    sequence(
        &live,
        &journal,
        0,
        order(1, 100, 10, Side::Sell, TimeInForce::Gtc, Hash32::zero()),
    )
    .expect("seed ask");
    sequence(
        &live,
        &journal,
        1,
        order(2, 100, 15, Side::Buy, TimeInForce::Ioc, Hash32::zero()),
    )
    .expect_err("the IOC remainder is unfillable");

    let progress: RefCell<Vec<(u64, u64)>> = RefCell::new(Vec::new());
    let (replayed, last_applied) = ReplayEngine::<()>::replay_from_with_clock_and_progress(
        &journal,
        0,
        SYMBOL,
        stub_clock(),
        |count, seq| progress.borrow_mut().push((count, seq)),
    )
    .expect("replay succeeds");

    assert_books_match(&replayed, &live);
    assert_eq!(last_applied, 1);
    assert_eq!(progress.into_inner(), vec![(1, 0), (2, 1)]);
}

/// `From<&OrderBookError>` fills every field, and the code travels as its
/// stable `u16` wire value. `InsufficientLiquidity` is one of the errors
/// `add_order` returns after real fills, so it records
/// `may_have_mutated: true`; it is not an STP rejection, so `stp_mode` is
/// `None`.
#[test]
fn rejected_with_code_carries_the_wire_code() {
    let err = OrderBookError::InsufficientLiquidity {
        side: Side::Buy,
        requested: 15,
        available: 10,
    };
    let result = SequencerResult::from(&err);
    match &result {
        SequencerResult::RejectedWithCode {
            reason,
            code,
            may_have_mutated,
            stp_mode,
        } => {
            assert_eq!(reason, &err.to_string());
            assert_eq!(*code, RejectReason::InsufficientLiquidity);
            assert!(
                *may_have_mutated,
                "the unfillable remainder is reported after the fills"
            );
            assert_eq!(*stp_mode, None, "not an STP rejection");
        }
        other => panic!("expected RejectedWithCode, got {other:?}"),
    }

    let json = serde_json::to_string(&result).expect("serialize");
    assert!(
        json.contains("\"code\":13"),
        "the code encodes as its u16 wire value: {json}"
    );
    let decoded: SequencerResult = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(code_of(&decoded), Some(RejectReason::InsufficientLiquidity));

    let event = SequencerEvent {
        sequence_num: 7,
        timestamp_ns: 7,
        command: SequencerCommand::<()>::CancelAll,
        result: SequencerResult::from(&OrderBookError::KillSwitchActive),
    };
    let json = serde_json::to_vec(&event).expect("serialize event");
    let decoded: SequencerEvent<()> = serde_json::from_slice(&json).expect("deserialize event");
    assert_eq!(decoded.sequence_num, 7);
    assert_eq!(
        code_of(&decoded.result),
        Some(RejectReason::KillSwitchActive)
    );
}

/// The residual-admission failure is the rejection whose reject code lies:
/// `add_order` emits the sweep's trades, fails to admit the residual into
/// its level, logs at `ERROR`, removes the level it created empty and
/// returns `OrderBookError::PriceLevelError`
/// (`src/orderbook/modifications.rs`, the residual-admission arm). That
/// error maps to `RejectReason::Other(0)`
/// (`src/orderbook/reject_reason.rs`), the bucket replay skips by code — so
/// the entry replayed as a no-op and rebuilt the ask the live book had
/// consumed. `SequencerResult::from(&err)` now records
/// `may_have_mutated: true` for it, which forces the re-execution. Replay
/// cannot reproduce the admission failure itself (it takes a concurrent
/// mutation of the same level), so the re-execution rests the residual and
/// the verdict disagreement is reported: `OutcomeMismatch` at sequence 1
/// with `recorded == Other(0)` and `actual == None`, a loud stop instead of
/// a book that quietly carries liquidity the live one spent.
#[test]
fn post_mutation_rejection_under_an_other_code_is_re_executed() {
    let journal: InMemoryJournal<()> = InMemoryJournal::new();
    append(
        &journal,
        0,
        SequencerCommand::AddOrder(order(
            1,
            100,
            10,
            Side::Sell,
            TimeInForce::Gtc,
            Hash32::zero(),
        )),
        SequencerResult::OrderAdded {
            order_id: Id::from_u64(1),
        },
    );

    let err = OrderBookError::PriceLevelError(PriceLevelError::InvalidOperation {
        message: "price level visible quantity overflow on admission".to_string(),
    });
    assert_eq!(
        RejectReason::from(&err),
        RejectReason::Other(0),
        "the residual-admission failure has no reject code of its own"
    );
    let result = SequencerResult::from(&err);
    assert!(
        matches!(
            result,
            SequencerResult::RejectedWithCode {
                may_have_mutated: true,
                ..
            }
        ),
        "the journal must flag it as possibly post-mutation: {result:?}"
    );
    append(
        &journal,
        1,
        SequencerCommand::AddOrder(order(
            2,
            100,
            15,
            Side::Buy,
            TimeInForce::Gtc,
            Hash32::zero(),
        )),
        result,
    );

    let err = replay(&journal, &ReplayBookConfig::default())
        .err()
        .expect("the flagged rejection must not be skipped");
    match err {
        ReplayError::OutcomeMismatch {
            sequence_num,
            recorded,
            actual,
        } => {
            assert_eq!(sequence_num, 1);
            assert_eq!(recorded, RejectReason::Other(0));
            assert!(
                actual.is_none(),
                "the re-execution rested the residual instead of failing"
            );
        }
        other => panic!("expected OutcomeMismatch, got {other:?}"),
    }
}

/// The counterpart: `Other(0)` also carries the expired-at-admission
/// rejection, which `validate_order_shape` raises before the book is
/// touched and which is decided by the book's clock. It records
/// `may_have_mutated: false`, so the code-driven skip stays in place — as
/// it must, since the replay clock reads 0 here and a re-execution would
/// rest an order the live book refused. Live and replayed books match with
/// the seeded ask alone.
#[test]
fn pre_mutation_other_rejection_is_still_skipped() {
    let live = OrderBook::<()>::with_clock(SYMBOL, Arc::new(StubClock::starting_at(10_000)));
    let journal: InMemoryJournal<()> = InMemoryJournal::new();

    sequence(
        &live,
        &journal,
        0,
        order(1, 100, 10, Side::Sell, TimeInForce::Gtc, Hash32::zero()),
    )
    .expect("seed ask");
    let expired = order(
        2,
        99,
        10,
        Side::Buy,
        TimeInForce::Gtd(1_000),
        Hash32::zero(),
    );
    let err = sequence(&live, &journal, 1, expired).expect_err("the deadline is in the past");
    assert!(
        matches!(err, OrderBookError::InvalidOperation { .. }),
        "expected the expired-at-admission rejection, got {err:?}"
    );
    assert_eq!(
        RejectReason::from(&err),
        RejectReason::Other(0),
        "it shares the Other(0) bucket with the residual-admission failure"
    );

    let (replayed, last_applied) =
        replay(&journal, &ReplayBookConfig::default()).expect("replay succeeds");
    assert!(
        replayed.get_order(Id::from_u64(2)).is_none(),
        "the expired order did not rest on replay, where the clock reads 0"
    );
    assert_books_match(&replayed, &live);
    assert_eq!(last_applied, 0, "the skipped rejection did not advance it");
}

/// `may_have_mutated` is derived from the typed error, and only the errors
/// the engine can return after changing the book are flagged: the
/// unfillable IOC / market remainder, the STP-cancelled taker and the
/// residual-admission failure. Everything else is raised by an admission
/// check, an operational gate or an internal path that runs before the
/// book is touched.
#[test]
fn may_have_mutated_is_set_for_exactly_the_post_mutation_errors() {
    let post_mutation = [
        OrderBookError::InsufficientLiquidity {
            side: Side::Buy,
            requested: 15,
            available: 10,
        },
        OrderBookError::InsufficientLiquidityNotional {
            side: Side::Buy,
            requested: 1_000,
            spent: 400,
        },
        OrderBookError::SelfTradePrevented {
            mode: STPMode::CancelTaker,
            taker_order_id: Id::from_u64(1),
            user_id: user(1),
        },
        OrderBookError::PriceLevelError(PriceLevelError::InvalidFormat),
    ];
    let pre_mutation = [
        OrderBookError::KillSwitchActive,
        OrderBookError::RiskMaxOpenOrders {
            account: user(1),
            current: 5,
            limit: 5,
        },
        OrderBookError::PriceCrossing {
            price: 100,
            side: Side::Buy,
            opposite_price: 99,
        },
        OrderBookError::InvalidTickSize {
            price: 105,
            tick_size: 10,
        },
        OrderBookError::DuplicateOrderId {
            order_id: Id::from_u64(1),
        },
        OrderBookError::InvalidOperation {
            message: "Order has already expired".to_string(),
        },
        OrderBookError::OrderNotFound("1".to_string()),
    ];

    for err in post_mutation {
        assert!(
            matches!(
                SequencerResult::from(&err),
                SequencerResult::RejectedWithCode {
                    may_have_mutated: true,
                    ..
                }
            ),
            "{err:?} can follow a mutation and must be flagged"
        );
    }
    for err in pre_mutation {
        assert!(
            matches!(
                SequencerResult::from(&err),
                SequencerResult::RejectedWithCode {
                    may_have_mutated: false,
                    ..
                }
            ),
            "{err:?} is raised before the book is touched"
        );
    }
}

/// Reconciling the reject code cannot tell `CancelTaker` from `CancelBoth`:
/// a taker that fills a foreign maker and then reaches its own is refused
/// by both, under the same `SelfTradePrevention` code, but only
/// `CancelBoth` also cancels that same-user maker. The recorded mode is
/// what catches it — `OrderBookError::SelfTradePrevented` carries the mode
/// that decided the verdict, so the journal pins the source book's
/// `CancelTaker` and replay under `CancelBoth` stops at sequence 2 with
/// `StpModeMismatch` instead of reconstructing a book missing 9 units of
/// ask.
#[test]
fn stp_mode_recorded_in_the_journal_must_match_the_replay_config() {
    let mut live = OrderBook::<()>::with_clock(SYMBOL, stub_clock());
    live.set_stp_mode(STPMode::CancelTaker);
    let journal: InMemoryJournal<()> = InMemoryJournal::new();

    sequence(
        &live,
        &journal,
        0,
        order(1, 100, 5, Side::Sell, TimeInForce::Gtc, user(2)),
    )
    .expect("seed non-self maker");
    sequence(
        &live,
        &journal,
        1,
        order(2, 100, 9, Side::Sell, TimeInForce::Gtc, user(1)),
    )
    .expect("seed same-user maker");
    sequence(
        &live,
        &journal,
        2,
        order(3, 100, 9, Side::Buy, TimeInForce::Gtc, user(1)),
    )
    .expect_err("the taker reaches its own maker");

    let entries: Vec<_> = journal
        .read_from(2)
        .expect("read the rejection back")
        .map(|entry| entry.expect("entry decodes"))
        .collect();
    assert!(
        matches!(
            entries.first().map(|entry| &entry.event.result),
            Some(SequencerResult::RejectedWithCode {
                stp_mode: Some(STPMode::CancelTaker),
                ..
            })
        ),
        "the journal records the mode that decided the rejection"
    );

    let config = ReplayBookConfig::new(None, STPMode::CancelBoth, None, None, None, None);
    let err = replay(&journal, &config)
        .err()
        .expect("an incompatible STP mode must be refused");
    match err {
        ReplayError::StpModeMismatch {
            sequence_num,
            recorded,
            actual,
        } => {
            assert_eq!(sequence_num, 2);
            assert_eq!(recorded, STPMode::CancelTaker);
            assert_eq!(actual, STPMode::CancelBoth);
        }
        other => panic!("expected StpModeMismatch, got {other:?}"),
    }
}

/// What the guard does not cover, pinned rather than claimed. A journal
/// that does not carry the source mode — a hand-built `RejectedWithCode`,
/// or any rejection other than an STP one — leaves replay with the reject
/// code alone, and the code is identical under `CancelTaker` and
/// `CancelBoth`. Replay then succeeds while the reconstructed book is
/// wrong: `CancelBoth` cancels the same-user maker the live `CancelTaker`
/// book left resting, so 9 units of ask vanish. `snapshots_match` is the
/// check that reports it.
#[test]
fn an_unrecorded_stp_mode_leaves_the_divergence_to_snapshots_match() {
    let mut live = OrderBook::<()>::with_clock(SYMBOL, stub_clock());
    live.set_stp_mode(STPMode::CancelTaker);
    let journal: InMemoryJournal<()> = InMemoryJournal::new();

    let makers = [
        order(1, 100, 5, Side::Sell, TimeInForce::Gtc, user(2)),
        order(2, 100, 9, Side::Sell, TimeInForce::Gtc, user(1)),
    ];
    for (seq, maker) in makers.into_iter().enumerate() {
        sequence(&live, &journal, seq as u64, maker).expect("seed maker");
    }
    let taker = order(3, 100, 9, Side::Buy, TimeInForce::Gtc, user(1));
    let err = live
        .add_order(taker)
        .expect_err("the taker reaches its own");
    append(
        &journal,
        2,
        SequencerCommand::AddOrder(taker),
        SequencerResult::RejectedWithCode {
            reason: err.to_string(),
            code: RejectReason::from(&err),
            may_have_mutated: true,
            stp_mode: None,
        },
    );
    assert_eq!(
        live.get_order(Id::from_u64(2))
            .expect("the same-user maker rests under CancelTaker")
            .visible_quantity()
            .as_u64(),
        9
    );

    let config = ReplayBookConfig::new(None, STPMode::CancelBoth, None, None, None, None);
    let (replayed, last_applied) =
        replay(&journal, &config).expect("the reject code matches under both modes");
    assert_eq!(last_applied, 2, "the rejection re-executed and reconciled");
    assert!(
        replayed.get_order(Id::from_u64(2)).is_none(),
        "CancelBoth cancelled the maker CancelTaker left resting"
    );
    assert!(
        !snapshots_match(
            &replayed.create_snapshot(usize::MAX),
            &live.create_snapshot(usize::MAX)
        ),
        "snapshots_match is what exposes the divergence the code hid"
    );
}

/// The journaled shape round-trips through both wire formats with every
/// field intact: the code as its stable `u16`, the mutation flag and the
/// recorded STP mode.
#[test]
fn rejected_with_code_round_trips_through_json() {
    let err = OrderBookError::SelfTradePrevented {
        mode: STPMode::CancelBoth,
        taker_order_id: Id::from_u64(3),
        user_id: user(1),
    };
    let event = SequencerEvent {
        sequence_num: 9,
        timestamp_ns: 9,
        command: SequencerCommand::<()>::MarketOrder {
            id: Id::from_u64(3),
            quantity: 5,
            side: Side::Buy,
        },
        result: SequencerResult::from(&err),
    };

    let json = serde_json::to_string(&event).expect("serialize");
    assert!(
        json.contains("\"code\":6"),
        "the code travels as its u16 wire value: {json}"
    );
    let decoded: SequencerEvent<()> = serde_json::from_str(&json).expect("deserialize");
    match decoded.result {
        SequencerResult::RejectedWithCode {
            reason,
            code,
            may_have_mutated,
            stp_mode,
        } => {
            assert_eq!(reason, err.to_string());
            assert_eq!(code, RejectReason::SelfTradePrevention);
            assert!(
                may_have_mutated,
                "an STP taker may be cancelled after fills"
            );
            assert_eq!(stp_mode, Some(STPMode::CancelBoth));
        }
        other => panic!("expected RejectedWithCode, got {other:?}"),
    }
}

#[cfg(feature = "bincode")]
#[test]
fn rejected_with_code_round_trips_through_bincode() {
    let err = OrderBookError::SelfTradePrevented {
        mode: STPMode::CancelTaker,
        taker_order_id: Id::from_u64(3),
        user_id: user(1),
    };
    let event = SequencerEvent {
        sequence_num: 9,
        timestamp_ns: 9,
        command: SequencerCommand::<()>::AddOrder(order(
            3,
            100,
            9,
            Side::Buy,
            TimeInForce::Gtc,
            user(1),
        )),
        result: SequencerResult::from(&err),
    };

    let cfg = bincode::config::standard();
    let bytes = bincode::serde::encode_to_vec(&event, cfg).expect("encode");
    let (decoded, read) =
        bincode::serde::decode_from_slice::<SequencerEvent<()>, _>(&bytes, cfg).expect("decode");
    assert_eq!(read, bytes.len(), "bincode consumes the whole payload");
    match decoded.result {
        SequencerResult::RejectedWithCode {
            reason,
            code,
            may_have_mutated,
            stp_mode,
        } => {
            assert_eq!(reason, err.to_string());
            assert_eq!(code, RejectReason::SelfTradePrevention);
            assert!(may_have_mutated);
            assert_eq!(stp_mode, Some(STPMode::CancelTaker));
        }
        other => panic!("expected RejectedWithCode, got {other:?}"),
    }
}
