// thin_book_sweep_hdr — book near-empty, IOC probing.
// Exercises the partial-fill / cancel-the-remainder path.

#[path = "hdr_common.rs"]
mod common;

use common::{Rng, new_histogram, owner, persist, record, report};
use pricelevel::{Id, Side, TimeInForce};

const SCENARIO: &str = "thin_book_sweep";
// Re-seed a thin slice (RESTING orders at one or two prices) every
// REFILL_EVERY ops so the book never goes fully empty across the
// measurement window.
const RESTING_PER_REFILL: u64 = 3;
const REFILL_EVERY: u64 = 5;
const MEASURED_OPS: u64 = 200_000;
const SEED: u64 = 0xA5A5_A5A5_A5A5_A5A5;

fn main() {
    let book = common::fresh_book();
    let mut rng = Rng::new(SEED);
    let mut hist = new_histogram();
    let maker = owner(0xAA);
    let taker = owner(0xBB);
    let mut next_id: u64 = 1;

    for i in 0..MEASURED_OPS {
        if i % REFILL_EVERY == 0 {
            // Drop a few resting asks. No measurement around the
            // refill — only the IOC probe is timed.
            for _ in 0..RESTING_PER_REFILL {
                let _ = book.add_limit_order_with_user(
                    Id::from_u64(next_id),
                    rng.range(99, 101) as u128,
                    rng.range(1, 5),
                    Side::Sell,
                    TimeInForce::Gtc,
                    maker,
                    None,
                );
                next_id += 1;
            }
        }

        // IOC buy probe — frequently larger than the resting depth so
        // the engine ends up partial-filling and cancelling the
        // remainder.
        let id = Id::from_u64(next_id);
        next_id += 1;
        let qty = rng.range(1, 20);
        record(&mut hist, || {
            let _ = book.submit_market_order_with_user(id, qty, Side::Buy, taker);
        });
    }

    report(SCENARIO, &hist);
    persist(SCENARIO, &hist).expect("persist hgrm");
}
