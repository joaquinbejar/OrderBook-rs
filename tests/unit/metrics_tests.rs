//! Metrics integration tests have moved to `tests/metrics/` so they
//! run in their own test binary, isolated from the broader integration
//! suite that perturbs the global `metrics` recorder as a side effect
//! of every `OrderBook` mutation.
