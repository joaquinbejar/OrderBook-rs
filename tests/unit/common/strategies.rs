//! `proptest` strategies that generate valid journal event streams.
//!
//! The strategies target determinism / replay invariants: every generated
//! event is a `SequencerEvent<()>` with a monotonic sequence number, a
//! zero-initialised `timestamp_ns` (timestamps are re-stamped by the engine
//! clock during replay), and a `SequencerResult` variant consistent with the
//! command (`OrderAdded` for `AddOrder`, `OrderCancelled` for `CancelOrder`).
//!
//! The strategies stay biased to produce realistic traffic on a narrow price
//! band so crossings occur frequently on short streams. This keeps failure
//! shrinks small and interpretable.

use orderbook_rs::orderbook::sequencer::{SequencerCommand, SequencerEvent, SequencerResult};
use pricelevel::{Hash32, Id, OrderType, Price, Quantity, Side, TimeInForce, TimestampMs};
use proptest::collection::vec;
use proptest::prelude::*;

/// A deterministic, small pool of owner ids so self-cross / STP paths stay
/// reachable on short streams.
const OWNER_POOL: [u8; 4] = [1, 2, 3, 4];

/// Tight crossing-friendly price band (ticks). Keeping the band narrow
/// guarantees a non-trivial fraction of crossings without swamping the book.
const PRICE_MIN: u128 = 99;
const PRICE_MAX: u128 = 101;

/// Quantity range per order.
const QTY_MIN: u64 = 1;
const QTY_MAX: u64 = 100;

fn side_strategy() -> impl Strategy<Value = Side> {
    prop_oneof![Just(Side::Buy), Just(Side::Sell)]
}

fn owner_strategy() -> impl Strategy<Value = Hash32> {
    (0usize..OWNER_POOL.len()).prop_map(|i| {
        let mut bytes = [0u8; 32];
        bytes[0] = OWNER_POOL[i];
        Hash32::new(bytes)
    })
}

fn price_strategy() -> impl Strategy<Value = Price> {
    (PRICE_MIN..=PRICE_MAX).prop_map(Price::new)
}

fn qty_strategy() -> impl Strategy<Value = Quantity> {
    (QTY_MIN..=QTY_MAX).prop_map(Quantity::new)
}

/// Generates a `Standard` GTC order with a deterministic id derived from the
/// enclosing sequence. Id uniqueness is preserved by the caller via the
/// sequence counter — see [`event_stream`].
///
/// Limited to `TimeInForce::Gtc` on purpose: an `Ioc` / `Fok` that cannot
/// fill makes `book.add_order` return `InsufficientLiquidity`, which
/// [`orderbook_rs::orderbook::sequencer::ReplayEngine`] propagates as a
/// `ReplayError`. A proptest-generated stream would need a stateful
/// strategy to avoid that. GTC always rests safely on any crossing
/// shape, making the stream deterministically replayable. Issue #57
/// widens this with a stateful strategy that tracks live ids.
fn standard_order_strategy(id_seed: u64) -> impl Strategy<Value = OrderType<()>> {
    (
        owner_strategy(),
        side_strategy(),
        price_strategy(),
        qty_strategy(),
    )
        .prop_map(move |(user_id, side, price, quantity)| OrderType::Standard {
            id: Id::from_u64(id_seed),
            price,
            quantity,
            side,
            time_in_force: TimeInForce::Gtc,
            user_id,
            timestamp: TimestampMs::new(0),
            extra_fields: (),
        })
}

/// Only AddOrder for #51's determinism proptest. Cancels require a
/// stateful strategy to avoid `OrderNotFound` during replay and are
/// deferred to #57.
fn command_strategy(seq: u64) -> impl Strategy<Value = SequencerCommand<()>> {
    standard_order_strategy(seq).prop_map(SequencerCommand::AddOrder)
}

/// Generate a stream of monotonic `SequencerEvent<()>` values of length in
/// `len_range`. Sequence numbers start at 1 and increment by 1 per event.
pub fn event_stream(
    len_range: std::ops::Range<usize>,
) -> impl Strategy<Value = Vec<SequencerEvent<()>>> {
    vec(any::<()>(), len_range).prop_flat_map(|placeholders| {
        let len = placeholders.len();
        let per_event: Vec<_> = (1..=len as u64)
            .map(|seq| command_strategy(seq).prop_map(move |cmd| (seq, cmd)))
            .collect();
        per_event.prop_map(|pairs| {
            pairs
                .into_iter()
                .map(|(seq, command)| {
                    let result = match &command {
                        SequencerCommand::AddOrder(order) => SequencerResult::OrderAdded {
                            order_id: order.id(),
                        },
                        SequencerCommand::CancelOrder(id) => {
                            SequencerResult::OrderCancelled { order_id: *id }
                        }
                        // Remaining variants not produced by this strategy.
                        _ => SequencerResult::Rejected {
                            reason: "unreachable in this strategy".to_string(),
                        },
                    };
                    SequencerEvent {
                        sequence_num: seq,
                        timestamp_ns: 0,
                        command,
                        result,
                    }
                })
                .collect()
        })
    })
}
