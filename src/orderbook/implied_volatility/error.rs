//! Error types for implied volatility calculation.

use std::fmt;

/// Errors specific to IV calculation.
#[derive(Debug, Clone)]
pub enum IVError {
    /// No valid price available (empty book or no bid/ask).
    NoPriceAvailable,

    /// Spread too wide for reliable calculation.
    SpreadTooWide {
        /// Current spread in basis points.
        spread_bps: f64,
        /// Maximum allowed spread in basis points.
        threshold_bps: f64,
    },

    /// The book is crossed (best ask below best bid) or locked (best ask equal
    /// to best bid), so no meaningful mid price exists for IV calculation.
    CrossedBook {
        /// Best bid price (scaled to f64).
        bid: f64,
        /// Best ask price (scaled to f64).
        ask: f64,
    },

    /// Newton-Raphson solver did not converge within max iterations.
    ConvergenceFailure {
        /// Number of iterations attempted.
        iterations: u32,
        /// Last IV estimate before giving up.
        last_iv: f64,
    },

    /// Invalid input parameters for IV calculation.
    InvalidParams {
        /// Description of the invalid parameter.
        message: String,
    },

    /// Price is below intrinsic value (indicates arbitrage opportunity).
    PriceBelowIntrinsic {
        /// Market price observed.
        price: f64,
        /// Calculated intrinsic value.
        intrinsic: f64,
    },

    /// Time to expiry is too small for reliable calculation.
    TimeToExpiryTooSmall {
        /// Time to expiry in years.
        time_to_expiry: f64,
        /// Minimum required time in years.
        min_time: f64,
    },

    /// Volatility is outside reasonable bounds.
    VolatilityOutOfBounds {
        /// Calculated volatility.
        volatility: f64,
        /// Minimum bound.
        min_bound: f64,
        /// Maximum bound.
        max_bound: f64,
    },
}

impl fmt::Display for IVError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IVError::NoPriceAvailable => {
                write!(f, "no valid price available from order book")
            }
            IVError::SpreadTooWide {
                spread_bps,
                threshold_bps,
            } => {
                write!(
                    f,
                    "spread too wide: {spread_bps:.1} bps exceeds threshold of {threshold_bps:.1} bps"
                )
            }
            IVError::CrossedBook { bid, ask } => {
                write!(
                    f,
                    "book is crossed or locked: best bid {bid:.4} is not below best ask {ask:.4}"
                )
            }
            IVError::ConvergenceFailure {
                iterations,
                last_iv,
            } => {
                write!(
                    f,
                    "solver did not converge after {iterations} iterations, last IV: {last_iv:.4}"
                )
            }
            IVError::InvalidParams { message } => {
                write!(f, "invalid parameters: {message}")
            }
            IVError::PriceBelowIntrinsic { price, intrinsic } => {
                write!(
                    f,
                    "price {price:.4} is below intrinsic value {intrinsic:.4}"
                )
            }
            IVError::TimeToExpiryTooSmall {
                time_to_expiry,
                min_time,
            } => {
                write!(
                    f,
                    "time to expiry {time_to_expiry:.6} years is below minimum {min_time:.6} years"
                )
            }
            IVError::VolatilityOutOfBounds {
                volatility,
                min_bound,
                max_bound,
            } => {
                write!(
                    f,
                    "volatility {volatility:.4} is outside bounds [{min_bound:.4}, {max_bound:.4}]"
                )
            }
        }
    }
}

impl std::error::Error for IVError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_display() {
        let err = IVError::NoPriceAvailable;
        assert_eq!(err.to_string(), "no valid price available from order book");

        let err = IVError::SpreadTooWide {
            spread_bps: 600.0,
            threshold_bps: 500.0,
        };
        assert!(err.to_string().contains("600.0 bps"));

        let err = IVError::CrossedBook {
            bid: 10.5,
            ask: 10.0,
        };
        assert!(err.to_string().contains("crossed or locked"));

        let err = IVError::ConvergenceFailure {
            iterations: 100,
            last_iv: 0.25,
        };
        assert!(err.to_string().contains("100 iterations"));

        let err = IVError::InvalidParams {
            message: "negative spot price".to_string(),
        };
        assert!(err.to_string().contains("negative spot price"));

        let err = IVError::PriceBelowIntrinsic {
            price: 5.0,
            intrinsic: 10.0,
        };
        assert!(err.to_string().contains("below intrinsic"));

        let err = IVError::TimeToExpiryTooSmall {
            time_to_expiry: 0.0001,
            min_time: 0.001,
        };
        assert!(err.to_string().contains("time to expiry"));

        let err = IVError::VolatilityOutOfBounds {
            volatility: 6.0,
            min_bound: 0.001,
            max_bound: 5.0,
        };
        assert!(err.to_string().contains("outside bounds"));
    }
}
