mod book_coverage_tests;
mod book_manager_cross_cancel_tests;
mod clock_determinism_tests;
mod common;
mod engine_seq_monotonic_tests;
#[cfg(feature = "journal")]
mod filejournal_edge_case_tests;
mod implied_volatility_tests;
mod integration_workflow_tests;
mod kill_switch_tests;
mod manager_coverage_tests;
mod mass_cancel_tests;
mod matching_coverage_tests;
mod matching_coverage_tests_extended;
mod modifications_coverage_tests;
mod operations_coverage_tests;
mod operations_coverage_tests_extended;
mod order_state_tests;
mod private_coverage_tests;
mod reject_reason_tests;
mod replay_coverage_tests;
#[cfg(feature = "journal")]
mod replay_determinism;
mod risk_layer_tests;
mod sequencer_types_tests;
mod snapshot_restore_tests;
mod validation_tests;
