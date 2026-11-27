//! #328: pegged re-price sweeps (`special_orders`).
//!
//! The book: a third-party bid at 100_000 and ask at 200_000, then 500
//! buy pegs on `BestBid - 1` and 500 sell pegs on `BestAsk + 1`, each
//! resting at its own deep price.
//!
//! - `sweep_moves/1000`: one sweep on a fresh book; every peg moves to its
//!   target (one cancel-then-add per peg).
//! - `sweep_steady/1000`: one sweep on a book whose pegs already sit at
//!   their targets; no peg moves, so the row measures the per-peg touch
//!   read and price computation alone.
//!
//! Registered in its own group, last, so the shared rows run the same
//! sequence of benches in a comparison against a baseline without it.

use criterion::{BatchSize, BenchmarkId, Criterion};
use orderbook_rs::OrderBook;
use orderbook_rs::orderbook::repricing::RepricingOperations;
use pricelevel::{
    Hash32, Id, OrderType, PegReferenceType, Price, Quantity, Side, TimeInForce, TimestampMs,
};
use std::hint::black_box;

const PEGS: u64 = 1000;

fn peg(
    id: u64,
    price: u128,
    side: Side,
    reference: PegReferenceType,
    offset: i64,
) -> OrderType<()> {
    OrderType::PeggedOrder {
        id: Id::from_u64(id),
        price: Price::new(price),
        quantity: Quantity::new(1),
        side,
        user_id: Hash32::new([1; 32]),
        timestamp: TimestampMs::new(0),
        time_in_force: TimeInForce::Gtc,
        reference_price_offset: offset,
        reference_price_type: reference,
        extra_fields: (),
    }
}

/// The book described in the module docs.
fn pegged_book() -> OrderBook<()> {
    let book = OrderBook::<()>::new("PEG/BENCH");
    let third = Hash32::new([9; 32]);
    book.add_limit_order_with_user(
        Id::from_u64(1),
        100_000,
        10,
        Side::Buy,
        TimeInForce::Gtc,
        third,
        None,
    )
    .unwrap();
    book.add_limit_order_with_user(
        Id::from_u64(2),
        200_000,
        10,
        Side::Sell,
        TimeInForce::Gtc,
        third,
        None,
    )
    .unwrap();
    for i in 0..PEGS / 2 {
        let offset = u128::from(i) + 2;
        book.add_order(peg(
            10 + i,
            100_000 - offset,
            Side::Buy,
            PegReferenceType::BestBid,
            -1,
        ))
        .unwrap();
        book.add_order(peg(
            10 + PEGS + i,
            200_000 + offset,
            Side::Sell,
            PegReferenceType::BestAsk,
            1,
        ))
        .unwrap();
    }
    book
}

pub fn register_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("pegged_reprice");
    group.bench_with_input(BenchmarkId::new("sweep_moves", PEGS), &PEGS, |b, _| {
        b.iter_batched(
            pegged_book,
            |book| {
                black_box(book.reprice_pegged_orders().unwrap());
                book
            },
            BatchSize::LargeInput,
        );
    });
    group.bench_with_input(BenchmarkId::new("sweep_steady", PEGS), &PEGS, |b, _| {
        let book = pegged_book();
        book.reprice_pegged_orders().unwrap();
        b.iter(|| black_box(book.reprice_pegged_orders().unwrap()));
    });
    group.finish();
}
