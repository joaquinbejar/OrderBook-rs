// This crate root is entirely integration-test code, not production
// (issue #242's Production Panic Policy gate, `[lints.clippy]` in
// `Cargo.toml`, is package-wide and would otherwise apply here too).
// Test fixtures freely `.unwrap()` / `.expect()` setup, index scratch
// buffers and do raw arithmetic on sample sizes; none of that reaches
// `src/`. Panics and assertions in tests are permitted by
// `rules/global_rules.md`'s Testing section.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap
)]

mod atomic_postonly_fok_tests;
mod best_price_race_tests;
mod book_coverage_tests;
mod book_manager_cross_cancel_tests;
mod clock_determinism_tests;
mod common;
mod concurrent_level_statistics_tests;
mod engine_seq_monotonic_tests;
mod evict_expired_tests;
#[cfg(feature = "journal")]
mod filejournal_edge_case_tests;
mod implied_volatility_tests;
mod integration_workflow_tests;
mod kill_switch_tests;
mod manager_coverage_tests;
mod manager_lifecycle_tests;
mod market_order_by_amount_tests;
mod mass_cancel_determinism_tests;
mod mass_cancel_tests;
mod matching_coverage_tests;
mod matching_coverage_tests_extended;
mod modifications_coverage_tests;
mod modify_atomic_tests;
mod mutation_failure_atomicity_tests;
mod operations_coverage_tests;
mod operations_coverage_tests_extended;
mod order_state_tests;
mod private_coverage_tests;
mod props_quantity_update_priority;
mod reject_reason_tests;
mod replay_config_tests;
mod replay_coverage_tests;
#[cfg(feature = "journal")]
mod replay_determinism;
mod replay_submit_error_tests;
#[cfg(feature = "special_orders")]
mod repricing_determinism_tests;
mod reserve_lot_size_tests;
mod reserve_residual_policy_tests;
mod reserve_residual_replay_tests;
mod restore_user_orders_determinism_tests;
mod risk_layer_tests;
mod sequencer_types_tests;
mod snapshot_concurrency_tests;
mod snapshot_restore_tests;
#[cfg(feature = "special_orders")]
mod special_order_restore_tests;
mod stp_reachability_tests;
mod two_tranche_conservation_tests;
mod update_price_and_quantity_two_tranche_tests;
mod update_quantity_zero_tests;
mod validation_tests;
