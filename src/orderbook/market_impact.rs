//! Market impact simulation and liquidity analysis
//!
//! This module provides tools for analyzing the impact of large orders
//! before execution, helping traders understand:
//! - Average execution price
//! - Expected slippage
//! - Number of price levels consumed
//! - Available liquidity in price ranges

use serde::{Deserialize, Serialize};

/// Represents the market impact analysis of an order
///
/// Provides detailed metrics about how an order would affect the market,
/// including price impact, slippage, and liquidity consumption.
///
/// The fields use two reference frames: `avg_price`, `worst_price`,
/// `slippage`, `slippage_bps`, and `levels_consumed` describe only the
/// portion the analyzed order would **consume**, whereas
/// [`total_quantity_available`](Self::total_quantity_available) reports the
/// **whole side's** resting depth (so [`fill_ratio`](Self::fill_ratio) can
/// exceed `1.0`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MarketImpact {
    /// Average execution price across all fills (in price units)
    pub avg_price: f64,

    /// Worst (furthest from best price) execution price (in price units)
    pub worst_price: u128,

    /// Absolute slippage from best price (in price units)
    pub slippage: u128,

    /// Slippage in basis points
    pub slippage_bps: f64,

    /// Number of price levels that would be consumed
    pub levels_consumed: usize,

    /// Total resting depth available on the side being hit (in units),
    /// summed across **every** non-empty level — not capped at the
    /// requested quantity. This is what makes [`Self::can_fill`] and
    /// [`Self::fill_ratio`] meaningful: a value greater than the requested
    /// quantity means the order would fully fill with depth to spare.
    pub total_quantity_available: u64,
}

/// Represents a simulated order execution
///
/// Provides step-by-step details of how an order would be filled,
/// including all individual fills at different price levels.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrderSimulation {
    /// Vector of fills as (price, quantity) pairs
    pub fills: Vec<(u128, u64)>,

    /// Average execution price across all fills (in price units)
    pub avg_price: f64,

    /// Total quantity that would be filled (in units)
    pub total_filled: u64,

    /// Quantity that could not be filled due to insufficient liquidity (in units)
    pub remaining_quantity: u64,
}

impl MarketImpact {
    /// Creates a new MarketImpact with all fields set to zero/empty
    ///
    /// # Returns
    /// A MarketImpact instance with default values indicating no impact
    #[must_use]
    pub fn empty() -> Self {
        Self {
            avg_price: 0.0,
            worst_price: 0,
            slippage: 0,
            slippage_bps: 0.0,
            levels_consumed: 0,
            total_quantity_available: 0,
        }
    }

    /// Checks if the order can be fully filled
    ///
    /// # Arguments
    /// - `requested_quantity`: The quantity originally requested (in units)
    ///
    /// # Returns
    /// `true` if sufficient liquidity exists to fill the entire order
    #[must_use]
    pub fn can_fill(&self, requested_quantity: u64) -> bool {
        self.total_quantity_available >= requested_quantity
    }

    /// Returns the fill ratio of available depth to requested quantity.
    ///
    /// Because [`Self::total_quantity_available`] now reflects the true
    /// resting depth (not the capped fill quantity), this ratio **can
    /// exceed 1.0** when the book holds more depth than requested — a value
    /// of `2.0` means twice the requested quantity is resting. Returns
    /// `0.0` for a zero `requested_quantity`.
    ///
    /// # Arguments
    /// - `requested_quantity`: The quantity originally requested (in units)
    ///
    /// # Returns
    /// `total_quantity_available / requested_quantity`
    #[must_use]
    pub fn fill_ratio(&self, requested_quantity: u64) -> f64 {
        if requested_quantity == 0 {
            return 0.0;
        }
        (self.total_quantity_available as f64) / (requested_quantity as f64)
    }
}

impl OrderSimulation {
    /// Creates a new OrderSimulation with empty fills
    ///
    /// # Returns
    /// An OrderSimulation instance with no fills
    #[must_use]
    pub fn empty() -> Self {
        Self {
            fills: Vec::new(),
            avg_price: 0.0,
            total_filled: 0,
            remaining_quantity: 0,
        }
    }

    /// Checks if the order was fully filled
    ///
    /// # Returns
    /// `true` if no quantity remains unfilled
    #[must_use]
    pub fn is_fully_filled(&self) -> bool {
        self.remaining_quantity == 0
    }

    /// Returns the number of price levels used in the simulation
    ///
    /// # Returns
    /// The count of distinct price levels in the fills
    #[must_use]
    pub fn levels_count(&self) -> usize {
        self.fills.len()
    }

    /// Calculates the total cost of the simulated order
    ///
    /// # Returns
    /// The total cost (price × quantity summed across all fills)
    #[must_use]
    pub fn total_cost(&self) -> u128 {
        // Saturating fold (matching the simulate path) so an extreme
        // price × quantity product or running total caps at u128::MAX rather
        // than panicking in debug / wrapping in release.
        self.fills.iter().fold(0u128, |acc, (price, qty)| {
            acc.saturating_add(price.saturating_mul(u128::from(*qty)))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_market_impact_empty() {
        let impact = MarketImpact::empty();
        assert_eq!(impact.avg_price, 0.0);
        assert_eq!(impact.worst_price, 0);
        assert_eq!(impact.levels_consumed, 0);
    }

    #[test]
    fn test_total_cost_saturates_on_extreme_fills() {
        // A price × quantity product and running total beyond u128::MAX must
        // saturate, not panic in debug / wrap in release.
        let sim = OrderSimulation {
            fills: vec![(u128::MAX, 2), (u128::MAX, 3)],
            avg_price: 0.0,
            total_filled: 5,
            remaining_quantity: 0,
        };
        assert_eq!(sim.total_cost(), u128::MAX);
    }

    #[test]
    fn test_total_cost_unchanged_for_realistic_fills() {
        let sim = OrderSimulation {
            fills: vec![(100, 5), (101, 3)],
            avg_price: 0.0,
            total_filled: 8,
            remaining_quantity: 0,
        };
        assert_eq!(sim.total_cost(), 100 * 5 + 101 * 3);
    }

    #[test]
    fn test_market_impact_can_fill() {
        let impact = MarketImpact {
            avg_price: 100.0,
            worst_price: 105,
            slippage: 5,
            slippage_bps: 50.0,
            levels_consumed: 3,
            total_quantity_available: 100,
        };

        assert!(impact.can_fill(100));
        assert!(impact.can_fill(50));
        assert!(!impact.can_fill(101));
    }

    #[test]
    fn test_market_impact_fill_ratio() {
        let impact = MarketImpact {
            avg_price: 100.0,
            worst_price: 105,
            slippage: 5,
            slippage_bps: 50.0,
            levels_consumed: 3,
            total_quantity_available: 75,
        };

        assert_eq!(impact.fill_ratio(100), 0.75);
        assert_eq!(impact.fill_ratio(75), 1.0);
        assert_eq!(impact.fill_ratio(0), 0.0);
    }

    #[test]
    fn test_order_simulation_empty() {
        let sim = OrderSimulation::empty();
        assert!(sim.fills.is_empty());
        assert_eq!(sim.total_filled, 0);
        assert!(sim.is_fully_filled());
    }

    #[test]
    fn test_order_simulation_is_fully_filled() {
        let sim = OrderSimulation {
            fills: vec![(100, 50), (105, 50)],
            avg_price: 102.5,
            total_filled: 100,
            remaining_quantity: 0,
        };
        assert!(sim.is_fully_filled());

        let sim_partial = OrderSimulation {
            fills: vec![(100, 50)],
            avg_price: 100.0,
            total_filled: 50,
            remaining_quantity: 50,
        };
        assert!(!sim_partial.is_fully_filled());
    }

    #[test]
    fn test_order_simulation_levels_count() {
        let sim = OrderSimulation {
            fills: vec![(100, 30), (105, 40), (110, 30)],
            avg_price: 105.0,
            total_filled: 100,
            remaining_quantity: 0,
        };
        assert_eq!(sim.levels_count(), 3);
    }

    #[test]
    fn test_order_simulation_total_cost() {
        let sim = OrderSimulation {
            fills: vec![(100, 10), (105, 10)],
            avg_price: 102.5,
            total_filled: 20,
            remaining_quantity: 0,
        };
        // (100 * 10) + (105 * 10) = 1000 + 1050 = 2050
        assert_eq!(sim.total_cost(), 2050);
    }
}
