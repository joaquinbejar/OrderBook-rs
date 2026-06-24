//! Newton-Raphson solver for implied volatility calculation.
//!
//! This module provides a numerical solver to find the implied volatility
//! that makes the Black-Scholes price equal to the observed market price.

use super::black_scholes::BlackScholes;
use super::error::IVError;
use super::types::IVParams;

/// Configuration for the Newton-Raphson solver.
#[derive(Debug, Clone)]
pub struct SolverConfig {
    /// Maximum iterations before giving up.
    pub max_iterations: u32,
    /// Convergence tolerance for price difference.
    pub tolerance: f64,
    /// Initial IV guess (default: 0.25 = 25%).
    pub initial_guess: f64,
    /// Minimum IV bound (default: 0.001 = 0.1%).
    pub min_iv: f64,
    /// Maximum IV bound (default: 5.0 = 500%).
    pub max_iv: f64,
    /// Minimum vega threshold to avoid division by near-zero.
    pub min_vega: f64,
}

impl Default for SolverConfig {
    fn default() -> Self {
        Self {
            max_iterations: 100,
            tolerance: 1e-8,
            initial_guess: 0.25,
            min_iv: 0.001,
            max_iv: 5.0,
            min_vega: 1e-10,
        }
    }
}

impl SolverConfig {
    /// Creates a new solver configuration with default values.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the maximum number of iterations.
    #[must_use]
    pub fn with_max_iterations(mut self, max_iterations: u32) -> Self {
        self.max_iterations = max_iterations;
        self
    }

    /// Sets the convergence tolerance.
    #[must_use]
    pub fn with_tolerance(mut self, tolerance: f64) -> Self {
        self.tolerance = tolerance;
        self
    }

    /// Sets the initial IV guess.
    #[must_use]
    pub fn with_initial_guess(mut self, initial_guess: f64) -> Self {
        self.initial_guess = initial_guess;
        self
    }

    /// Sets the IV bounds.
    #[must_use]
    pub fn with_bounds(mut self, min_iv: f64, max_iv: f64) -> Self {
        self.min_iv = min_iv;
        self.max_iv = max_iv;
        self
    }
}

/// Validates input parameters for IV calculation.
///
/// # Arguments
/// - `params`: Option parameters to validate
///
/// # Returns
/// - `Ok(())` if parameters are valid
/// - `Err(IVError)` if any parameter is invalid
fn validate_params(params: &IVParams) -> Result<(), IVError> {
    // Reject non-finite inputs first: NaN/Inf pass every sign/magnitude check
    // below (all comparisons against NaN are false) and would otherwise
    // propagate NaN through the solver loops to a meaningless
    // `ConvergenceFailure { last_iv: NaN }`.
    for (name, value) in [
        ("spot", params.spot),
        ("strike", params.strike),
        ("time_to_expiry", params.time_to_expiry),
        ("risk_free_rate", params.risk_free_rate),
    ] {
        if !value.is_finite() {
            return Err(IVError::InvalidParams {
                message: format!("{name} must be finite, got {value}"),
            });
        }
    }

    if params.spot <= 0.0 {
        return Err(IVError::InvalidParams {
            message: format!("spot price must be positive, got {}", params.spot),
        });
    }

    if params.strike <= 0.0 {
        return Err(IVError::InvalidParams {
            message: format!("strike price must be positive, got {}", params.strike),
        });
    }

    if params.time_to_expiry < 0.0 {
        return Err(IVError::InvalidParams {
            message: format!(
                "time to expiry must be non-negative, got {}",
                params.time_to_expiry
            ),
        });
    }

    // Minimum time to expiry for numerical stability (about 1 hour)
    const MIN_TIME: f64 = 1.0 / (365.0 * 24.0);
    if params.time_to_expiry < MIN_TIME {
        return Err(IVError::TimeToExpiryTooSmall {
            time_to_expiry: params.time_to_expiry,
            min_time: MIN_TIME,
        });
    }

    Ok(())
}

/// Calculates a smart initial guess for IV based on option characteristics.
///
/// Uses the Brenner-Subrahmanyam approximation for ATM options and
/// adjusts for moneyness.
///
/// # Arguments
/// - `params`: Option parameters
/// - `market_price`: Observed market price
///
/// # Returns
/// Initial IV estimate
fn smart_initial_guess(params: &IVParams, market_price: f64) -> f64 {
    let sqrt_time = params.time_to_expiry.sqrt();

    // Brenner-Subrahmanyam approximation for ATM: σ ≈ price / (0.4 * S * √T)
    let bs_approx = market_price / (0.4 * params.spot * sqrt_time);

    // Clamp to reasonable bounds
    bs_approx.clamp(0.05, 2.0)
}

/// Solves for implied volatility using Newton-Raphson method.
///
/// The Newton-Raphson method iteratively refines the IV estimate using:
/// σ_{n+1} = σ_n - (BS(σ_n) - market_price) / vega(σ_n)
///
/// Convergence is typically fast (3-5 iterations) because vega is always positive.
///
/// # Arguments
/// - `params`: Option parameters (spot, strike, time, rate, type)
/// - `market_price`: Observed market price to match
/// - `config`: Solver configuration
///
/// # Returns
/// - `Ok((iv, iterations))`: Converged IV and number of iterations
/// - `Err(IVError)`: If solver fails to converge or inputs are invalid
///
/// # Example
/// ```ignore
/// use orderbook_rs::implied_volatility::{IVParams, SolverConfig, solve_iv};
///
/// let params = IVParams::call(100.0, 100.0, 0.25, 0.05);
/// let market_price = 5.0;
/// let config = SolverConfig::default();
///
/// let (iv, iterations) = solve_iv(&params, market_price, &config)?;
/// println!("IV: {:.2}%, converged in {} iterations", iv * 100.0, iterations);
/// ```
pub fn solve_iv(
    params: &IVParams,
    market_price: f64,
    config: &SolverConfig,
) -> Result<(f64, u32), IVError> {
    // Validate inputs
    validate_params(params)?;

    if !market_price.is_finite() || market_price <= 0.0 {
        return Err(IVError::InvalidParams {
            message: format!("market price must be positive and finite, got {market_price}"),
        });
    }

    // Check if price is below intrinsic value
    let intrinsic = params.intrinsic_value();
    if market_price < intrinsic - config.tolerance {
        return Err(IVError::PriceBelowIntrinsic {
            price: market_price,
            intrinsic,
        });
    }

    // Use smart initial guess if default is used
    let mut iv = if (config.initial_guess - 0.25).abs() < 1e-10 {
        smart_initial_guess(params, market_price)
    } else {
        config.initial_guess
    };

    // Clamp initial guess to bounds
    iv = iv.clamp(config.min_iv, config.max_iv);

    // Newton-Raphson iteration
    for iteration in 0..config.max_iterations {
        let price = BlackScholes::price(params, iv);

        // Inputs are validated finite, so a non-finite price/iv here means the
        // iteration degenerated numerically. Bail with a typed error instead of
        // letting NaN poison `iv` and surface as `ConvergenceFailure { last_iv: NaN }`.
        if !price.is_finite() || !iv.is_finite() {
            return Err(IVError::InvalidParams {
                message: format!(
                    "non-finite value during Newton iteration (iv={iv}, price={price})"
                ),
            });
        }

        let diff = price - market_price;

        // Check convergence
        if diff.abs() < config.tolerance {
            // Validate final IV is within bounds
            if iv < config.min_iv || iv > config.max_iv {
                return Err(IVError::VolatilityOutOfBounds {
                    volatility: iv,
                    min_bound: config.min_iv,
                    max_bound: config.max_iv,
                });
            }
            return Ok((iv, iteration + 1));
        }

        let vega = BlackScholes::vega(params, iv);

        // Handle near-zero vega (can happen for deep ITM/OTM or near expiry)
        if vega.abs() < config.min_vega {
            // Fall back to bisection-like step
            if diff > 0.0 {
                iv *= 0.9; // Price too high, reduce vol
            } else {
                iv *= 1.1; // Price too low, increase vol
            }
        } else {
            // Standard Newton-Raphson step
            let step = diff / vega;

            // Dampen large steps to improve stability
            let damped_step = if step.abs() > 0.5 {
                step.signum() * 0.5
            } else {
                step
            };

            iv -= damped_step;
        }

        // Clamp to bounds
        iv = iv.clamp(config.min_iv, config.max_iv);
    }

    // Failed to converge
    Err(IVError::ConvergenceFailure {
        iterations: config.max_iterations,
        last_iv: iv,
    })
}

/// Solves for IV using bisection method as a fallback.
///
/// Slower than Newton-Raphson but guaranteed to converge if a solution exists.
///
/// # Arguments
/// - `params`: Option parameters
/// - `market_price`: Target market price
/// - `config`: Solver configuration
///
/// # Returns
/// - `Ok((iv, iterations))`: Converged IV and iterations
/// - `Err(IVError)`: If no solution exists in bounds
pub fn solve_iv_bisection(
    params: &IVParams,
    market_price: f64,
    config: &SolverConfig,
) -> Result<(f64, u32), IVError> {
    validate_params(params)?;

    if !market_price.is_finite() || market_price <= 0.0 {
        return Err(IVError::InvalidParams {
            message: format!("market price must be positive and finite, got {market_price}"),
        });
    }

    let intrinsic = params.intrinsic_value();
    if market_price < intrinsic - config.tolerance {
        return Err(IVError::PriceBelowIntrinsic {
            price: market_price,
            intrinsic,
        });
    }

    let mut low = config.min_iv;
    let mut high = config.max_iv;

    // Verify solution exists in bounds
    let price_low = BlackScholes::price(params, low);
    let price_high = BlackScholes::price(params, high);

    if market_price < price_low || market_price > price_high {
        return Err(IVError::VolatilityOutOfBounds {
            volatility: if market_price < price_low {
                config.min_iv
            } else {
                config.max_iv
            },
            min_bound: config.min_iv,
            max_bound: config.max_iv,
        });
    }

    for iteration in 0..config.max_iterations {
        let mid = (low + high) / 2.0;
        let price = BlackScholes::price(params, mid);
        let diff = price - market_price;

        if diff.abs() < config.tolerance || (high - low) < config.tolerance {
            return Ok((mid, iteration + 1));
        }

        if diff > 0.0 {
            high = mid;
        } else {
            low = mid;
        }
    }

    Err(IVError::ConvergenceFailure {
        iterations: config.max_iterations,
        last_iv: (low + high) / 2.0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOLERANCE: f64 = 1e-4;

    #[test]
    fn test_solve_iv_atm_call() {
        let params = IVParams::call(100.0, 100.0, 0.25, 0.05);
        let target_vol = 0.25;
        let market_price = BlackScholes::price(&params, target_vol);

        let config = SolverConfig::default();
        let (iv, iterations) = solve_iv(&params, market_price, &config).unwrap();

        assert!((iv - target_vol).abs() < TOLERANCE);
        assert!(iterations < 10);
    }

    #[test]
    fn test_solve_iv_atm_put() {
        let params = IVParams::put(100.0, 100.0, 0.25, 0.05);
        let target_vol = 0.30;
        let market_price = BlackScholes::price(&params, target_vol);

        let config = SolverConfig::default();
        let (iv, _) = solve_iv(&params, market_price, &config).unwrap();

        assert!((iv - target_vol).abs() < TOLERANCE);
    }

    #[test]
    fn test_solve_iv_itm_call() {
        let params = IVParams::call(110.0, 100.0, 0.25, 0.05);
        let target_vol = 0.20;
        let market_price = BlackScholes::price(&params, target_vol);

        let config = SolverConfig::default();
        let (iv, _) = solve_iv(&params, market_price, &config).unwrap();

        assert!((iv - target_vol).abs() < TOLERANCE);
    }

    #[test]
    fn test_solve_iv_otm_call() {
        let params = IVParams::call(90.0, 100.0, 0.25, 0.05);
        let target_vol = 0.35;
        let market_price = BlackScholes::price(&params, target_vol);

        let config = SolverConfig::default();
        let (iv, _) = solve_iv(&params, market_price, &config).unwrap();

        assert!((iv - target_vol).abs() < TOLERANCE);
    }

    #[test]
    fn test_solve_iv_high_volatility() {
        let params = IVParams::call(100.0, 100.0, 0.25, 0.0);
        let target_vol = 1.5; // 150% volatility
        let market_price = BlackScholes::price(&params, target_vol);

        let config = SolverConfig::default();
        let (iv, _) = solve_iv(&params, market_price, &config).unwrap();

        assert!((iv - target_vol).abs() < TOLERANCE);
    }

    #[test]
    fn test_solve_iv_low_volatility() {
        let params = IVParams::call(100.0, 100.0, 0.25, 0.0);
        let target_vol = 0.05; // 5% volatility
        let market_price = BlackScholes::price(&params, target_vol);

        let config = SolverConfig::default();
        let (iv, _) = solve_iv(&params, market_price, &config).unwrap();

        assert!((iv - target_vol).abs() < TOLERANCE);
    }

    #[test]
    fn test_solve_iv_invalid_spot() {
        let params = IVParams::call(-100.0, 100.0, 0.25, 0.05);
        let config = SolverConfig::default();

        let result = solve_iv(&params, 5.0, &config);
        assert!(matches!(result, Err(IVError::InvalidParams { .. })));
    }

    #[test]
    fn test_solve_iv_invalid_strike() {
        let params = IVParams::call(100.0, 0.0, 0.25, 0.05);
        let config = SolverConfig::default();

        let result = solve_iv(&params, 5.0, &config);
        assert!(matches!(result, Err(IVError::InvalidParams { .. })));
    }

    #[test]
    fn test_solve_iv_rejects_non_finite_params() {
        let config = SolverConfig::default();
        // Each non-finite field must produce an immediate InvalidParams, not a
        // NaN propagated to ConvergenceFailure.
        let cases = [
            IVParams::call(f64::NAN, 100.0, 0.25, 0.05),
            IVParams::call(f64::INFINITY, 100.0, 0.25, 0.05),
            IVParams::call(100.0, f64::NAN, 0.25, 0.05),
            IVParams::call(100.0, 100.0, f64::NAN, 0.05),
            IVParams::call(100.0, 100.0, f64::INFINITY, 0.05),
            IVParams::call(100.0, 100.0, 0.25, f64::NAN),
            IVParams::call(100.0, 100.0, 0.25, f64::INFINITY),
        ];
        for params in cases {
            let result = solve_iv(&params, 5.0, &config);
            assert!(
                matches!(result, Err(IVError::InvalidParams { .. })),
                "non-finite param must yield InvalidParams, got {result:?}"
            );
        }
    }

    #[test]
    fn test_solve_iv_rejects_non_finite_market_price() {
        let params = IVParams::call(100.0, 100.0, 0.25, 0.05);
        let config = SolverConfig::default();
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let newton = solve_iv(&params, bad, &config);
            let bisect = solve_iv_bisection(&params, bad, &config);
            assert!(
                matches!(newton, Err(IVError::InvalidParams { .. })),
                "Newton must reject non-finite market price, got {newton:?}"
            );
            assert!(
                matches!(bisect, Err(IVError::InvalidParams { .. })),
                "bisection must reject non-finite market price, got {bisect:?}"
            );
        }
    }

    #[test]
    fn test_solve_iv_never_returns_nan_convergence_failure() {
        // Even though non-finite inputs are now rejected up front, assert the
        // contract directly: no solver path returns ConvergenceFailure with a
        // NaN last_iv for non-finite inputs.
        let config = SolverConfig::default();
        let params = IVParams::call(f64::NAN, 100.0, 0.25, 0.05);
        match solve_iv(&params, f64::NAN, &config) {
            Err(IVError::ConvergenceFailure { last_iv, .. }) => {
                panic!("got ConvergenceFailure with last_iv={last_iv}");
            }
            Err(IVError::InvalidParams { .. }) => {}
            other => panic!("expected InvalidParams, got {other:?}"),
        }
    }

    #[test]
    fn test_solve_iv_time_too_small() {
        let params = IVParams::call(100.0, 100.0, 0.00001, 0.05);
        let config = SolverConfig::default();

        let result = solve_iv(&params, 5.0, &config);
        assert!(matches!(result, Err(IVError::TimeToExpiryTooSmall { .. })));
    }

    #[test]
    fn test_solve_iv_price_below_intrinsic() {
        // ITM call with intrinsic value of 10
        let params = IVParams::call(110.0, 100.0, 0.25, 0.0);
        let config = SolverConfig::default();

        // Price below intrinsic
        let result = solve_iv(&params, 5.0, &config);
        assert!(matches!(result, Err(IVError::PriceBelowIntrinsic { .. })));
    }

    #[test]
    fn test_solve_iv_bisection() {
        let params = IVParams::call(100.0, 100.0, 0.25, 0.05);
        let target_vol = 0.25;
        let market_price = BlackScholes::price(&params, target_vol);

        let config = SolverConfig::default();
        let (iv, _) = solve_iv_bisection(&params, market_price, &config).unwrap();

        assert!((iv - target_vol).abs() < TOLERANCE);
    }

    #[test]
    fn test_solver_config_builder() {
        let config = SolverConfig::new()
            .with_max_iterations(50)
            .with_tolerance(1e-6)
            .with_initial_guess(0.30)
            .with_bounds(0.01, 3.0);

        assert_eq!(config.max_iterations, 50);
        assert!((config.tolerance - 1e-6).abs() < 1e-10);
        assert!((config.initial_guess - 0.30).abs() < 1e-10);
        assert!((config.min_iv - 0.01).abs() < 1e-10);
        assert!((config.max_iv - 3.0).abs() < 1e-10);
    }

    #[test]
    fn test_smart_initial_guess() {
        let params = IVParams::call(100.0, 100.0, 0.25, 0.0);
        // Price for 25% vol ATM option
        let market_price = BlackScholes::price(&params, 0.25);

        let guess = smart_initial_guess(&params, market_price);
        // Should be reasonably close to actual vol
        assert!(guess > 0.1 && guess < 0.5);
    }

    #[test]
    fn test_convergence_speed() {
        let params = IVParams::call(100.0, 100.0, 0.25, 0.05);
        let target_vol = 0.25;
        let market_price = BlackScholes::price(&params, target_vol);

        let config = SolverConfig::default();
        let (_, iterations) = solve_iv(&params, market_price, &config).unwrap();

        // Newton-Raphson should converge quickly
        assert!(iterations <= 10);
    }

    #[test]
    fn test_various_maturities() {
        let target_vol = 0.25;
        let config = SolverConfig::default();

        // Test different maturities
        for days in [7, 30, 90, 180, 365] {
            let time = days as f64 / 365.0;
            let params = IVParams::call(100.0, 100.0, time, 0.05);
            let market_price = BlackScholes::price(&params, target_vol);

            let (iv, _) = solve_iv(&params, market_price, &config).unwrap();
            assert!(
                (iv - target_vol).abs() < TOLERANCE,
                "Failed for {} days maturity",
                days
            );
        }
    }

    #[test]
    fn test_various_moneyness() {
        let target_vol = 0.25;
        let config = SolverConfig::default();

        // Test different moneyness levels
        for strike in [80, 90, 100, 110, 120] {
            let params = IVParams::call(100.0, strike as f64, 0.25, 0.05);
            let market_price = BlackScholes::price(&params, target_vol);

            let (iv, _) = solve_iv(&params, market_price, &config).unwrap();
            assert!(
                (iv - target_vol).abs() < TOLERANCE,
                "Failed for strike {}",
                strike
            );
        }
    }
}
