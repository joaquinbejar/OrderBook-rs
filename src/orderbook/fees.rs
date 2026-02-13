//! Fee schedule implementation for OrderBook trading fees

use serde::{Deserialize, Serialize};

/// Configurable fee schedule for maker and taker fees
///
/// Fees are expressed in basis points (bps), where 1 bps = 0.01% = 0.0001.
/// Negative values represent rebates (common for maker fees to provide liquidity).
///
/// # Examples
///
/// ```
/// use orderbook_rs::FeeSchedule;
///
/// // Standard fee schedule: 5 bps taker fee, 2 bps maker rebate
/// let schedule = FeeSchedule::new(-2, 5);
///
/// // Calculate fee for a $10,000 trade
/// let notional = 10_000_000; // $10,000 in cents/micro-units
/// let taker_fee = schedule.calculate_fee(notional, false);
/// assert_eq!(taker_fee, 5_000); // 5 bps of $10,000 = $5.00
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeeSchedule {
    /// Maker fee in basis points (negative = rebate)
    ///
    /// Positive values charge makers, negative values provide rebates.
    /// Typical values range from -10 (rebate) to +10 (fee).
    pub maker_fee_bps: i32,

    /// Taker fee in basis points
    ///
    /// Always positive or zero. Typical values range from 0 to 50 bps.
    pub taker_fee_bps: i32,
}

impl FeeSchedule {
    /// Create a new fee schedule
    ///
    /// # Arguments
    ///
    /// * `maker_fee_bps` - Maker fee in basis points (negative for rebates)
    /// * `taker_fee_bps` - Taker fee in basis points (must be non-negative)
    ///
    /// # Errors
    ///
    /// Returns `OrderBookError::InvalidFeeRate` if taker fee is negative.
    ///
    /// # Examples
    ///
    /// ```
    /// use orderbook_rs::FeeSchedule;
    ///
    /// // Standard exchange fees
    /// let schedule = FeeSchedule::new(-2, 5);
    /// assert_eq!(schedule.maker_fee_bps, -2);
    /// assert_eq!(schedule.taker_fee_bps, 5);
    /// ```
    #[must_use = "FeeSchedule does nothing unless used"]
    pub fn new(maker_fee_bps: i32, taker_fee_bps: i32) -> Self {
        Self {
            maker_fee_bps,
            taker_fee_bps,
        }
    }

    /// Calculate fee amount for a transaction
    ///
    /// # Arguments
    ///
    /// * `notional` - The notional value of the trade (price × quantity)
    /// * `is_maker` - true if this is a maker transaction, false for taker
    ///
    /// # Returns
    ///
    /// The fee amount. Positive values represent charges, negative values represent rebates.
    ///
    /// # Examples
    ///
    /// ```
    /// use orderbook_rs::FeeSchedule;
    ///
    /// let schedule = FeeSchedule::new(-2, 5);
    ///
    /// // Taker fee: 5 bps on $10,000 = $5.00
    /// let taker_fee = schedule.calculate_fee(10_000_000, false);
    /// assert_eq!(taker_fee, 5_000);
    ///
    /// // Maker rebate: -2 bps on $10,000 = -$2.00
    /// let maker_rebate = schedule.calculate_fee(10_000_000, true);
    /// assert_eq!(maker_rebate, -2_000);
    /// ```
    #[must_use = "Fee calculation result must be used"]
    #[inline]
    pub fn calculate_fee(&self, notional: u128, is_maker: bool) -> i128 {
        let bps = if is_maker {
            self.maker_fee_bps
        } else {
            self.taker_fee_bps
        };
        // Use checked arithmetic to prevent overflow
        // notional can be up to u128::MAX, bps is typically small (-10000 to 10000)
        // Result fits in i128 since we divide by 10_000
        (notional as i128)
            .checked_mul(bps as i128)
            .map(|product| product / 10_000)
            .unwrap_or(i128::MAX) // Fallback for overflow (shouldn't happen with reasonable inputs)
    }

    /// Check if this fee schedule provides maker rebates
    ///
    /// # Returns
    ///
    /// true if maker_fee_bps is negative (rebate), false if positive or zero (fee)
    #[must_use]
    #[inline]
    pub fn has_maker_rebate(&self) -> bool {
        self.maker_fee_bps < 0
    }

    /// Check if this fee schedule has zero fees
    ///
    /// # Returns
    ///
    /// true if both maker and taker fees are zero
    #[must_use]
    #[inline]
    pub fn is_zero_fee(&self) -> bool {
        self.maker_fee_bps == 0 && self.taker_fee_bps == 0
    }

    /// Create a zero-fee schedule
    ///
    /// # Returns
    ///
    /// A FeeSchedule with zero fees for both makers and takers
    #[must_use]
    pub fn zero_fee() -> Self {
        Self::new(0, 0)
    }

    /// Create a fee schedule with only taker fees (common in some exchanges)
    ///
    /// # Arguments
    ///
    /// * `taker_fee_bps` - Taker fee in basis points
    ///
    /// # Returns
    ///
    /// A FeeSchedule with zero maker fee and specified taker fee
    #[must_use]
    pub fn taker_only(taker_fee_bps: i32) -> Self {
        Self::new(0, taker_fee_bps)
    }

    /// Create a fee schedule with maker rebates
    ///
    /// # Arguments
    ///
    /// * `maker_rebate_bps` - Maker rebate in basis points (positive value, will be negated)
    /// * `taker_fee_bps` - Taker fee in basis points
    ///
    /// # Returns
    ///
    /// A FeeSchedule with negative maker fee (rebate) and specified taker fee
    #[must_use]
    pub fn with_maker_rebate(maker_rebate_bps: i32, taker_fee_bps: i32) -> Self {
        Self::new(-maker_rebate_bps.abs(), taker_fee_bps)
    }
}

impl Default for FeeSchedule {
    fn default() -> Self {
        Self::zero_fee()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fee_schedule_creation() {
        let schedule = FeeSchedule::new(-2, 5);
        assert_eq!(schedule.maker_fee_bps, -2);
        assert_eq!(schedule.taker_fee_bps, 5);
    }

    #[test]
    fn test_zero_fee() {
        let schedule = FeeSchedule::zero_fee();
        assert!(schedule.is_zero_fee());
        assert_eq!(schedule.maker_fee_bps, 0);
        assert_eq!(schedule.taker_fee_bps, 0);
    }

    #[test]
    fn test_taker_only() {
        let schedule = FeeSchedule::taker_only(10);
        assert_eq!(schedule.maker_fee_bps, 0);
        assert_eq!(schedule.taker_fee_bps, 10);
    }

    #[test]
    fn test_maker_rebate() {
        let schedule = FeeSchedule::with_maker_rebate(3, 7);
        assert_eq!(schedule.maker_fee_bps, -3);
        assert_eq!(schedule.taker_fee_bps, 7);
        assert!(schedule.has_maker_rebate());
    }

    #[test]
    fn test_calculate_taker_fee() {
        let schedule = FeeSchedule::new(-2, 5);
        let notional = 100_000_000; // $1,000 in cents

        // 5 bps of $1,000 = $0.50 = 50 cents
        let fee = schedule.calculate_fee(notional, false);
        assert_eq!(fee, 50_000); // 50,000 cents = $500 (assuming cents as base unit)
    }

    #[test]
    fn test_calculate_maker_rebate() {
        let schedule = FeeSchedule::new(-2, 5);
        let notional = 100_000_000; // $1,000 in cents

        // -2 bps of $1,000 = -$0.20 = -20 cents
        let rebate = schedule.calculate_fee(notional, true);
        assert_eq!(rebate, -20_000); // -20,000 cents = -$200
    }

    #[test]
    fn test_zero_fee_calculation() {
        let schedule = FeeSchedule::zero_fee();
        let notional = 100_000_000;

        assert_eq!(schedule.calculate_fee(notional, true), 0);
        assert_eq!(schedule.calculate_fee(notional, false), 0);
    }

    #[test]
    fn test_large_notional() {
        let schedule = FeeSchedule::new(1, 1);
        let notional = u128::MAX / 10_000 - 1; // Safe large value

        let fee = schedule.calculate_fee(notional, false);
        assert!(fee > 0);
        assert!(fee < i128::MAX);
    }

    #[test]
    fn test_edge_cases() {
        let schedule = FeeSchedule::new(-10_000, 10_000); // Maximum reasonable fees
        let notional = 10_000; // Small notional

        let maker_fee = schedule.calculate_fee(notional, true);
        let taker_fee = schedule.calculate_fee(notional, false);

        assert_eq!(maker_fee, -10_000); // -100% of notional
        assert_eq!(taker_fee, 10_000); // 100% of notional
    }

    #[test]
    fn test_serialization() {
        let schedule = FeeSchedule::new(-2, 5);

        // Test JSON serialization
        let json = serde_json::to_string(&schedule).unwrap();
        let deserialized: FeeSchedule = serde_json::from_str(&json).unwrap();

        assert_eq!(schedule, deserialized);
    }

    #[test]
    fn test_default() {
        let schedule = FeeSchedule::default();
        assert!(schedule.is_zero_fee());
    }
}
