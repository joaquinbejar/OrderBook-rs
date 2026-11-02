//! CLI entry point for the A/B comparison harness (issues #258 / #259).
//!
//! Prints one JSON line per scenario to stdout:
//! `{"scenario":"add_only","class":"uncontended","timer":"single","samples":N,"mean":..,"p50":..,"p99":..,"p999":..,"p9999":..,"min":..,"max":..}`
//! — `scripts/bench_compare.sh` parses this with `python3`'s stdlib
//! `json` module (no new dependency). Diagnostics go to stderr so stdout
//! stays clean JSON-lines.
//!
//! - `class` is `uncontended` (one thread drives the book) or
//!   `contended` (several threads share one book); the summarizer applies
//!   the +3 % / +5 % regression thresholds per class.
//! - `timer` is `single` (one `Instant` pair per op: values are
//!   quantized to the host clock tick, so a p50 delta of one tick is not
//!   a measured regression) or `batch` (one pair per `BATCH` ops, per-op
//!   average recorded).
//!
//! Usage: `compare [--quick] [SCENARIO...]` — with no scenario names,
//! runs every scenario in [`SCENARIOS`]; `--quick` shrinks every op count
//! for a fast pipeline smoke test (see `scripts/bench_compare.sh
//! --quick`), not a real measurement.

mod adapter;
mod rng;
mod workloads;

use hdrhistogram::Histogram;

struct Sizes {
    add_only_warmup: u64,
    add_only_measured: u64,
    cancel_only_preload: u64,
    single_owner_preload: u64,
    walk_resting_per_level: u64,
    walk_num_levels: u64,
    walk_measured: u64,
    mixed_warmup: u64,
    mixed_measured: u64,
    thin_measured: u64,
    mass_cancel_orders: u64,
    mass_cancel_bursts: u64,
    by_user_bursts: u64,
    stp_measured: u64,
    snapshot_orders: u64,
    snapshot_samples: u64,
    replay_events: u64,
    replay_samples: u64,
    contended_warmup: u64,
    contended_per_thread: u64,
}

const FULL: Sizes = Sizes {
    add_only_warmup: 200_000,
    add_only_measured: 1_000_000,
    cancel_only_preload: 1_000_000,
    single_owner_preload: 100_000,
    walk_resting_per_level: 100,
    walk_num_levels: 50,
    walk_measured: 100_000,
    mixed_warmup: 200_000,
    mixed_measured: 1_000_000,
    thin_measured: 200_000,
    mass_cancel_orders: 10_000,
    mass_cancel_bursts: 500,
    by_user_bursts: 100,
    stp_measured: 100_000,
    snapshot_orders: 10_000,
    snapshot_samples: 2_000,
    replay_events: 10_000,
    replay_samples: 300,
    contended_warmup: 5_000,
    contended_per_thread: 50_000,
};

// Tiny op counts — only meant to prove the pipeline runs end to end
// (issue #258 task 6), never to produce a measurement. #259 uses `FULL`.
const QUICK: Sizes = Sizes {
    add_only_warmup: 200,
    add_only_measured: 2_000,
    cancel_only_preload: 2_000,
    single_owner_preload: 2_000,
    walk_resting_per_level: 5,
    walk_num_levels: 5,
    walk_measured: 2_000,
    mixed_warmup: 200,
    mixed_measured: 2_000,
    thin_measured: 2_000,
    mass_cancel_orders: 200,
    mass_cancel_bursts: 5,
    by_user_bursts: 5,
    stp_measured: 500,
    snapshot_orders: 200,
    snapshot_samples: 5,
    replay_events: 200,
    replay_samples: 5,
    contended_warmup: 50,
    contended_per_thread: 500,
};

/// Every scenario, in run order: `(name, class, timer)`.
const SCENARIOS: &[(&str, &str, &str)] = &[
    ("add_only", "uncontended", "single"),
    ("cancel_only", "uncontended", "batch"),
    ("cancel_single_owner", "uncontended", "batch"),
    ("fill_single_owner", "uncontended", "single"),
    ("aggressive_walk", "uncontended", "batch"),
    ("mixed_70_20_10", "uncontended", "single"),
    ("thin_book_sweep", "uncontended", "batch"),
    ("mass_cancel_burst", "uncontended", "single"),
    ("mass_cancel_by_user", "uncontended", "single"),
    ("stp_cancel_maker", "uncontended", "single"),
    ("snapshot_create_10k", "uncontended", "single"),
    ("snapshot_restore_10k", "uncontended", "single"),
    ("replay_10k", "uncontended", "single"),
    ("contended_add_4t", "contended", "batch"),
    ("contended_add_8t", "contended", "batch"),
    ("contended_add_listeners_4t", "contended", "batch"),
    ("contended_add_listeners_8t", "contended", "batch"),
];

fn emit(scenario: &str, class: &str, timer: &str, hist: &Histogram<u64>) {
    println!(
        "{{\"scenario\":\"{scenario}\",\"class\":\"{class}\",\"timer\":\"{timer}\",\"samples\":{},\"mean\":{:.1},\"p50\":{},\"p99\":{},\"p999\":{},\"p9999\":{},\"min\":{},\"max\":{}}}",
        hist.len(),
        hist.mean(),
        hist.value_at_quantile(0.50),
        hist.value_at_quantile(0.99),
        hist.value_at_quantile(0.999),
        hist.value_at_quantile(0.9999),
        hist.min(),
        hist.max(),
    );
}

fn run_one(scenario: &str, s: &Sizes) {
    let Some(&(_, class, timer)) = SCENARIOS.iter().find(|(name, _, _)| *name == scenario) else {
        let names: Vec<&str> = SCENARIOS.iter().map(|(name, _, _)| *name).collect();
        eprintln!(
            "unknown scenario {scenario:?} — expected one of: {}",
            names.join(", ")
        );
        std::process::exit(2);
    };
    let hist = match scenario {
        "add_only" => workloads::add_only(s.add_only_warmup, s.add_only_measured),
        "cancel_only" => workloads::cancel_only(s.cancel_only_preload),
        "cancel_single_owner" => workloads::cancel_single_owner(s.single_owner_preload),
        "fill_single_owner" => workloads::fill_single_owner(s.single_owner_preload),
        "aggressive_walk" => {
            workloads::aggressive_walk(s.walk_resting_per_level, s.walk_num_levels, s.walk_measured)
        }
        "mixed_70_20_10" => workloads::mixed_70_20_10(s.mixed_warmup, s.mixed_measured),
        "thin_book_sweep" => workloads::thin_book_sweep(s.thin_measured),
        "mass_cancel_burst" => {
            workloads::mass_cancel_burst(s.mass_cancel_orders, s.mass_cancel_bursts)
        }
        "mass_cancel_by_user" => {
            workloads::mass_cancel_by_user(s.mass_cancel_orders, s.by_user_bursts)
        }
        "stp_cancel_maker" => workloads::stp_cancel_maker(s.stp_measured),
        "snapshot_create_10k" => workloads::snapshot_create(s.snapshot_orders, s.snapshot_samples),
        "snapshot_restore_10k" => {
            workloads::snapshot_restore(s.snapshot_orders, s.snapshot_samples)
        }
        "replay_10k" => workloads::replay_adds(s.replay_events, s.replay_samples),
        "contended_add_4t" => workloads::contended_same_price_adds(
            4,
            s.contended_warmup,
            s.contended_per_thread,
            false,
        ),
        "contended_add_8t" => workloads::contended_same_price_adds(
            8,
            s.contended_warmup,
            s.contended_per_thread,
            false,
        ),
        "contended_add_listeners_4t" => workloads::contended_same_price_adds(
            4,
            s.contended_warmup,
            s.contended_per_thread,
            true,
        ),
        "contended_add_listeners_8t" => workloads::contended_same_price_adds(
            8,
            s.contended_warmup,
            s.contended_per_thread,
            true,
        ),
        _ => unreachable!("every SCENARIOS entry has a match arm"),
    };
    emit(scenario, class, timer, &hist);
}

fn main() {
    let mut quick = false;
    let mut scenarios: Vec<String> = Vec::new();
    for arg in std::env::args().skip(1) {
        if arg == "--quick" {
            quick = true;
        } else {
            scenarios.push(arg);
        }
    }
    if scenarios.is_empty() {
        scenarios = SCENARIOS
            .iter()
            .map(|(name, _, _)| name.to_string())
            .collect();
    }

    let sizes = if quick { &QUICK } else { &FULL };
    for scenario in &scenarios {
        run_one(scenario, sizes);
    }
}
