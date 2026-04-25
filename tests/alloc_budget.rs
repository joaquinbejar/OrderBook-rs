//! Allocation-budget regression test for the mixed hot-path workload.
//!
//! Feature-gated on `alloc-counters`. Runs 10 000 mixed ops after a
//! 1 000-op warmup and asserts the per-op allocation count stays
//! below a conservative ceiling tuned to catch regressions, **not**
//! to certify zero — `DashMap` + `SkipMap` allocate during bucket
//! grow on early submissions and that is fine.
//!
//! The ceiling is intentionally loose so the test does not flake on
//! shard-grow events or platform-specific allocator behaviour. A real
//! "one alloc per regression" guard belongs in the bench output's
//! tighter floor. This integration test is the CI guard.

#![cfg(feature = "alloc-counters")]

use orderbook_rs::OrderBook;
use orderbook_rs::utils::CountingAllocator;
use pricelevel::{Hash32, Id, Side, TimeInForce};
use std::alloc::System;

#[global_allocator]
static GLOBAL: CountingAllocator<System> = CountingAllocator::new(System);

const WARMUP_OPS: u64 = 1_000;
const MEASURED_OPS: u64 = 10_000;
// Conservative ceiling. Mixed workload allocates per-op via `DashMap`
// shard-grow on early submissions plus per-resting-order
// `Arc<PriceLevel>` allocations. Real engines hit ~1-2 allocs/op
// amortised; this ceiling fires only on a 5x or worse regression.
const ALLOCS_PER_OP_CEILING: f64 = 10.0;

fn account(byte: u8) -> Hash32 {
    let mut bytes = [0u8; 32];
    bytes[0] = byte;
    Hash32::new(bytes)
}

fn run_workload(book: &OrderBook<()>, count: u64, base: u64) {
    let acct = account(1);
    for i in 0..count {
        let id = Id::from_u64(base + i);
        let bucket = (base + i) % 5;
        match bucket {
            0..=2 => {
                let _ = book.add_limit_order_with_user(
                    id,
                    100 + (bucket as u128),
                    1 + (i % 10),
                    Side::Buy,
                    TimeInForce::Gtc,
                    acct,
                    None,
                );
            }
            3 => {
                let target = Id::from_u64(base + i.saturating_sub(1));
                let _ = book.cancel_order(target);
            }
            _ => {
                let _ = book.submit_market_order_with_user(id, 1, Side::Sell, acct);
            }
        }
    }
}

#[test]
fn alloc_budget_mixed_workload_stays_under_ceiling() {
    let book = OrderBook::<()>::new("BUDGET");

    // Seed liquidity so cancels and aggressive market orders find
    // something to interact with.
    for i in 0..50 {
        let _ = book.add_limit_order_with_user(
            Id::from_u64(1_000_000 + i),
            100,
            10,
            Side::Sell,
            TimeInForce::Gtc,
            account(2),
            None,
        );
    }

    run_workload(&book, WARMUP_OPS, 1);
    let before = GLOBAL.snapshot();
    run_workload(&book, MEASURED_OPS, WARMUP_OPS + 1);
    let after = GLOBAL.snapshot();

    let delta = after.since(before);
    let allocs_per_op = delta.allocs as f64 / MEASURED_OPS as f64;

    assert!(
        allocs_per_op < ALLOCS_PER_OP_CEILING,
        "alloc-budget regression: {} allocs across {} ops = {:.4} allocs/op (ceiling {:.4})",
        delta.allocs,
        MEASURED_OPS,
        allocs_per_op,
        ALLOCS_PER_OP_CEILING,
    );
}
