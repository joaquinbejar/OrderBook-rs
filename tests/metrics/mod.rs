//! Standalone integration-test binary for the optional `metrics`
//! feature (issue #60). Lives in its own crate test entry point so
//! the global `metrics` recorder isn't perturbed by the broader
//! integration suite under `tests/unit/`.

#[cfg(feature = "metrics")]
mod metrics_tests;
