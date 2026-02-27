//! Benchmarks comparing JSON vs Bincode serialization for trade events and
//! book change events.

use criterion::Criterion;
use orderbook_rs::orderbook::book_change_event::PriceLevelChangedEvent;
use orderbook_rs::orderbook::serialization::{EventSerializer, JsonEventSerializer};
use orderbook_rs::orderbook::trade::TradeResult;
use pricelevel::{Id, MatchResult, Side};
use std::hint::black_box;

fn make_trade_result() -> TradeResult {
    let order_id = Id::new_uuid();
    let match_result = MatchResult::new(order_id, 100);
    TradeResult::new("BTC/USD".to_string(), match_result)
}

fn make_book_change() -> PriceLevelChangedEvent {
    PriceLevelChangedEvent {
        side: Side::Buy,
        price: 50_000_000,
        quantity: 1_000,
    }
}

pub fn register_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("serialization");

    let trade = make_trade_result();
    let book_change = make_book_change();

    // ─── JSON ───────────────────────────────────────────────────────
    let json_ser = JsonEventSerializer::new();

    group.bench_function("json_serialize_trade", |b| {
        b.iter(|| json_ser.serialize_trade(black_box(&trade)))
    });

    group.bench_function("json_serialize_book_change", |b| {
        b.iter(|| json_ser.serialize_book_change(black_box(&book_change)))
    });

    let json_trade_bytes = json_ser
        .serialize_trade(&trade)
        .expect("json trade serialization must succeed in bench setup");
    let json_book_bytes = json_ser
        .serialize_book_change(&book_change)
        .expect("json book_change serialization must succeed in bench setup");

    group.bench_function("json_deserialize_trade", |b| {
        b.iter(|| json_ser.deserialize_trade(black_box(&json_trade_bytes)))
    });

    group.bench_function("json_deserialize_book_change", |b| {
        b.iter(|| json_ser.deserialize_book_change(black_box(&json_book_bytes)))
    });

    // ─── Bincode ────────────────────────────────────────────────────
    #[cfg(feature = "bincode")]
    {
        use orderbook_rs::orderbook::serialization::BincodeEventSerializer;

        let bin_ser = BincodeEventSerializer::new();

        group.bench_function("bincode_serialize_trade", |b| {
            b.iter(|| bin_ser.serialize_trade(black_box(&trade)))
        });

        group.bench_function("bincode_serialize_book_change", |b| {
            b.iter(|| bin_ser.serialize_book_change(black_box(&book_change)))
        });

        let bin_trade_bytes = bin_ser
            .serialize_trade(&trade)
            .expect("bincode trade serialization must succeed in bench setup");
        let bin_book_bytes = bin_ser
            .serialize_book_change(&book_change)
            .expect("bincode book_change serialization must succeed in bench setup");

        group.bench_function("bincode_deserialize_trade", |b| {
            b.iter(|| bin_ser.deserialize_trade(black_box(&bin_trade_bytes)))
        });

        group.bench_function("bincode_deserialize_book_change", |b| {
            b.iter(|| bin_ser.deserialize_book_change(black_box(&bin_book_bytes)))
        });
    }

    group.finish();
}
