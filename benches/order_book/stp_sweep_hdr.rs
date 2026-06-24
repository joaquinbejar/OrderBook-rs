// stp_sweep_hdr — aggressive self-crossing sweep under STP `CancelMaker`.
// Every measured op crosses a multi-level book where each level holds a
// same-user (taker) resting maker, so the per-level STP scan finds a maker
// to cancel inline. Exercises the pooled snapshot scan path (#107).

#[path = "hdr_common.rs"]
mod common;

use common::{Rng, new_histogram, owner, persist, record, report};
use orderbook_rs::{OrderBook, STPMode};
use pricelevel::{Id, Side, TimeInForce};

const SCENARIO: &str = "stp_sweep";
// Other-maker depth per level — the liquidity the taker actually fills
// against after its own same-user makers are cancelled by STP.
const OTHER_PER_LEVEL: u64 = 8;
const NUM_LEVELS: u64 = 50;
const MEASURED_OPS: u64 = 100_000;
const SEED: u64 = 0xA5A5_A5A5_A5A5_A5A5;

fn main() {
    // STP `CancelMaker`: an incoming taker order that would self-cross cancels
    // the resting same-user makers per level and keeps matching. This is the
    // path that re-scans the pooled snapshot buffer.
    let book = OrderBook::<()>::with_stp_mode("BENCH", STPMode::CancelMaker);
    let mut rng = Rng::new(SEED);
    let mut hist = new_histogram();

    let taker = owner(0xBB);
    let other = owner(0xCC);

    // Seed each ask level with other-maker Sell depth plus at least one
    // taker-owned Sell, so every measured Buy sweep hits the CancelMaker scan
    // (a same-user maker to cancel) before filling against the other maker.
    let mut next_id = 1u64;
    for level in 0..NUM_LEVELS {
        let price = (100 + level) as u128;
        // One taker-owned resting Sell at this level — the STP target.
        let _ = book.add_limit_order_with_user(
            Id::from_u64(next_id),
            price,
            rng.range(1, 5),
            Side::Sell,
            TimeInForce::Gtc,
            taker,
            None,
        );
        next_id += 1;
        // Other-maker depth the taker actually fills against.
        for _ in 0..OTHER_PER_LEVEL {
            let _ = book.add_limit_order_with_user(
                Id::from_u64(next_id),
                price,
                rng.range(1, 10),
                Side::Sell,
                TimeInForce::Gtc,
                other,
                None,
            );
            next_id += 1;
        }
    }

    // Aggressive self-crossing Buys from the taker. Each crosses the best ask
    // levels: the per-level STP scan finds the taker's own resting Sell and
    // cancels it inline, then matches against the other maker's depth. We
    // re-seed a fresh taker-owned Sell at the best level (unmeasured) before
    // each op so every measured sweep keeps hitting the CancelMaker path.
    for i in 0..MEASURED_OPS {
        let reseed_id = Id::from_u64(next_id + (2 * i));
        let _ = book.add_limit_order_with_user(
            reseed_id,
            100u128,
            1,
            Side::Sell,
            TimeInForce::Gtc,
            taker,
            None,
        );

        let qty = rng.range(5, 20);
        let id = Id::from_u64(next_id + (2 * i) + 1);
        record(&mut hist, || {
            let _ = book.submit_market_order_with_user(id, qty, Side::Buy, taker);
        });
    }

    report(SCENARIO, &hist);
    persist(SCENARIO, &hist).expect("persist hgrm");
}
