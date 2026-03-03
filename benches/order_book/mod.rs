pub mod add_orders;
pub mod mass_cancel;
pub mod match_orders;
pub mod matching;
pub mod mixed_operations;
pub mod replay;
pub mod snapshot;
pub mod update_orders;

// Import common benchmarks into the main bench group
pub fn register_benchmarks(c: &mut criterion::Criterion) {
    add_orders::register_benchmarks(c);
    match_orders::register_benchmarks(c);
    update_orders::register_benchmarks(c);
    mixed_operations::register_benchmarks(c);
    matching::register_benchmarks(c);
    mass_cancel::register_benchmarks(c);
    snapshot::register_benchmarks(c);
    replay::register_benchmarks(c);
}
