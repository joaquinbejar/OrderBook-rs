// mixed_70_20_10_hdr — 70 % submits, 20 % cancels, 10 % aggressive.
// The "realistic" scenario the BENCH.md headline numbers come from.

#[path = "hdr_common.rs"]
mod common;

use common::{Rng, new_histogram, persist, pick_owner, pick_side, record, report};
use pricelevel::{Id, Side, TimeInForce};

const SCENARIO: &str = "mixed_70_20_10";
const WARMUP_OPS: u64 = 200_000;
const MEASURED_OPS: u64 = 1_000_000;
const SEED: u64 = 0xA5A5_A5A5_A5A5_A5A5;

#[derive(Clone, Copy)]
enum Op {
    Submit,
    Cancel,
    Aggressive,
}

fn pick_op(rng: &mut Rng) -> Op {
    let v = rng.next() % 100;
    if v < 70 {
        Op::Submit
    } else if v < 90 {
        Op::Cancel
    } else {
        Op::Aggressive
    }
}

fn apply(book: &orderbook_rs::OrderBook<()>, rng: &mut Rng, next_id: &mut u64, op: Op) {
    match op {
        Op::Submit => {
            let id = Id::from_u64(*next_id);
            *next_id += 1;
            let price = rng.range(common::PRICE_LO, common::PRICE_HI) as u128;
            let qty = rng.range(common::QTY_LO, common::QTY_HI);
            let _ = book.add_limit_order_with_user(
                id,
                price,
                qty,
                pick_side(rng),
                TimeInForce::Gtc,
                pick_owner(rng),
                None,
            );
        }
        Op::Cancel => {
            // Cancel a random previously-issued id. Some hit, some miss
            // (already cancelled or filled) — both shapes are realistic.
            if *next_id > 1 {
                let target = rng.range(1, *next_id - 1);
                let _ = book.cancel_order(Id::from_u64(target));
            }
        }
        Op::Aggressive => {
            let id = Id::from_u64(*next_id);
            *next_id += 1;
            let qty = rng.range(1, 10);
            let _ = book.submit_market_order_with_user(id, qty, pick_side(rng), pick_owner(rng));
        }
    }
}

fn main() {
    let book = common::fresh_book();
    let mut rng = Rng::new(SEED);
    let mut hist = new_histogram();
    let mut next_id: u64 = 1;

    for _ in 0..WARMUP_OPS {
        let op = pick_op(&mut rng);
        apply(&book, &mut rng, &mut next_id, op);
    }

    for _ in 0..MEASURED_OPS {
        let op = pick_op(&mut rng);
        record(&mut hist, || apply(&book, &mut rng, &mut next_id, op));
    }

    report(SCENARIO, &hist);
    persist(SCENARIO, &hist).expect("persist hgrm");
    let _ = Side::Buy; // keep `Side` import live across feature combos
}
