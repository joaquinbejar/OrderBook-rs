// aggressive_walk_hdr — taker market orders sweep multi-level book.
// Measures the fill-loop tail under saturating liquidity.

#[path = "hdr_common.rs"]
mod common;

use common::{Rng, new_histogram, owner, persist, record, report};
use pricelevel::{Id, Side, TimeInForce};

const SCENARIO: &str = "aggressive_walk";
// Pre-load enough resting depth for every aggressive sweep to fill.
const RESTING_PER_LEVEL: u64 = 100;
const NUM_LEVELS: u64 = 50;
const MEASURED_OPS: u64 = 100_000;
const SEED: u64 = 0xA5A5_A5A5_A5A5_A5A5;

fn main() {
    let book = common::fresh_book();
    let mut rng = Rng::new(SEED);
    let mut hist = new_histogram();
    let maker = owner(0xAA);
    let taker = owner(0xBB);

    // Seed RESTING_PER_LEVEL asks at each of NUM_LEVELS prices.
    let mut next_id = 1u64;
    for level in 0..NUM_LEVELS {
        let price = (100 + level) as u128;
        for _ in 0..RESTING_PER_LEVEL {
            let _ = book.add_limit_order_with_user(
                Id::from_u64(next_id),
                price,
                rng.range(1, 10),
                Side::Sell,
                TimeInForce::Gtc,
                maker,
                None,
            );
            next_id += 1;
        }
    }

    // Aggressive Buy sweeps. Each sweeps 5..=20 lots — usually clears
    // a few orders within the same price level.
    for i in 0..MEASURED_OPS {
        let qty = rng.range(5, 20);
        let id = Id::from_u64(next_id + i);
        record(&mut hist, || {
            let _ = book.submit_market_order_with_user(id, qty, Side::Buy, taker);
        });
    }

    report(SCENARIO, &hist);
    persist(SCENARIO, &hist).expect("persist hgrm");
}
