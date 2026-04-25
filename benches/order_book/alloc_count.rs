// alloc_count — feature-gated allocation profile of the mixed
// 70/20/10 hot-path workload. Reports `allocs_per_op` and a
// per-counter delta over a measurement window.
//
// Build / run:
//
//     cargo bench --features alloc-counters --bench alloc_count

#![cfg(feature = "alloc-counters")]

#[path = "hdr_common.rs"]
mod common;

use orderbook_rs::utils::CountingAllocator;
use std::alloc::System;

#[global_allocator]
static GLOBAL: CountingAllocator<System> = CountingAllocator::new(System);

use common::{Rng, pick_owner, pick_side};
use pricelevel::{Id, TimeInForce};

const SCENARIO: &str = "alloc_count_mixed_70_20_10";
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
    let mut next_id: u64 = 1;

    // Warmup — discarded.
    for _ in 0..WARMUP_OPS {
        let op = pick_op(&mut rng);
        apply(&book, &mut rng, &mut next_id, op);
    }

    // Capture pre-measurement counters.
    let before = GLOBAL.snapshot();

    for _ in 0..MEASURED_OPS {
        let op = pick_op(&mut rng);
        apply(&book, &mut rng, &mut next_id, op);
    }

    let after = GLOBAL.snapshot();
    let delta = after.since(before);

    let allocs_per_op = delta.allocs as f64 / MEASURED_OPS as f64;
    let bytes_per_op = delta.bytes_allocated as f64 / MEASURED_OPS as f64;

    println!("scenario        : {SCENARIO}");
    println!("warmup ops      : {WARMUP_OPS}");
    println!("measured ops    : {MEASURED_OPS}");
    println!("allocs          : {}", delta.allocs);
    println!("deallocs        : {}", delta.deallocs);
    println!("bytes_alloc     : {}", delta.bytes_allocated);
    println!("bytes_dealloc   : {}", delta.bytes_deallocated);
    println!("allocs/op       : {allocs_per_op:.4}");
    println!("bytes_alloc/op  : {bytes_per_op:.2}");

    let summary = format!(
        "# {SCENARIO}\n\
         \n\
         | counter         | value                |\n\
         |-----------------|----------------------|\n\
         | warmup_ops      | {WARMUP_OPS}        |\n\
         | measured_ops    | {MEASURED_OPS}      |\n\
         | allocs          | {}                  |\n\
         | deallocs        | {}                  |\n\
         | bytes_alloc     | {}                  |\n\
         | bytes_dealloc   | {}                  |\n\
         | allocs/op       | {allocs_per_op:.4}  |\n\
         | bytes_alloc/op  | {bytes_per_op:.2}   |\n",
        delta.allocs, delta.deallocs, delta.bytes_allocated, delta.bytes_deallocated,
    );
    let _ = std::fs::create_dir_all("target/alloc-counters");
    let path = format!("target/alloc-counters/{SCENARIO}.md");
    if let Err(e) = std::fs::write(&path, summary) {
        eprintln!("could not write {path}: {e}");
    } else {
        eprintln!("wrote {path}");
    }
}
