//! Fee schedule implementation for OrderBook trading fees

use serde::{Deserialize, Serialize};

/// Denominator for basis-point fee math: 1 bps = 1 / 10_000 of the notional.
const BPS_DENOMINATOR: u128 = 10_000;

/// Fee computation would overflow its `u128` intermediate product.
///
/// Returned by [`FeeSchedule::try_calculate_fee`] when
/// `notional × |bps|` overflows `u128` — exactly the case in which
/// [`FeeSchedule::calculate_fee`] saturates the product and clamps instead
/// of computing the fee from the true product. The clamped fee is not
/// guaranteed exact (at isolated notionals it can still coincide with the
/// exact value), so venues that must guarantee exact integer fees
/// (journaled, replayable systems) can treat this error as "the engine
/// would journal a clamped fee" and reject the input, or enforce
/// [`FeeSchedule::max_guaranteed_exact_notional`] at admission time so this
/// error is provably unreachable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error(
    "fee overflow: notional {notional} × |{bps}| bps exceeds the u128 domain; max guaranteed-exact notional at this rate is {max_guaranteed_exact_notional}"
)]
pub struct FeeOverflow {
    /// The notional value (price × quantity) that was passed in.
    pub notional: u128,
    /// The signed fee rate in basis points that applied (maker or taker).
    pub bps: i32,
    /// Largest notional guaranteed exact at this rate (the
    /// multiplication-safety bound) — equal to
    /// [`FeeSchedule::max_guaranteed_exact_notional_for_bps`]`(bps)`.
    pub max_guaranteed_exact_notional: u128,
}

impl FeeOverflow {
    #[cold]
    fn new(notional: u128, bps: i32) -> Self {
        Self {
            notional,
            bps,
            max_guaranteed_exact_notional: FeeSchedule::max_guaranteed_exact_notional_for_bps(bps),
        }
    }
}

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
    /// # Rounding
    ///
    /// The fee is `notional × bps / 10_000` computed with integer division, which
    /// **truncates toward zero** — i.e. it rounds toward `0`, not toward `−∞`. The
    /// magnitude is computed in the unsigned domain and the sign of `bps` is
    /// applied afterward, so the rounding is symmetric in magnitude for a taker
    /// fee (positive `bps`) and a maker rebate (negative `bps`): both drop the
    /// fractional part rather than rounding it. For example a `notional` of
    /// `15_003` yields `+7` at `+5` bps and `−3` at `−2` bps (each `floor` of the
    /// magnitude `7.5015` / `3.0006`, then signed). External fee reconciliation
    /// should therefore truncate-toward-zero, not round-half-up.
    ///
    /// # Saturation
    ///
    /// The result is **guaranteed exact** when
    /// `notional <= Self::max_guaranteed_exact_notional_for_bps(bps)`
    /// (equivalently, when `notional × |bps|` fits in `u128`). Beyond that
    /// bound the intermediate product saturates and the fee clamps to a
    /// magnitude of `u128::MAX / 10_000` (signed per `bps`) instead of
    /// panicking; the clamped value is **not guaranteed exact**, although at
    /// isolated notionals it can still coincide with the true
    /// `⌊notional × |bps| / 10_000⌋`. Callers that must distinguish a
    /// clamped fee from a guaranteed-exact one should use
    /// [`Self::try_calculate_fee`], or enforce
    /// [`Self::max_guaranteed_exact_notional`] at admission time so this
    /// saturating branch is provably unreachable.
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
    ///
    /// // Fractional bps × notional truncates toward zero in both directions.
    /// assert_eq!(schedule.calculate_fee(15_003, false), 7); // floor(7.5015)
    /// assert_eq!(schedule.calculate_fee(15_003, true), -3); // -floor(3.0006)
    /// ```
    #[must_use = "Fee calculation result must be used"]
    #[inline]
    pub fn calculate_fee(&self, notional: u128, is_maker: bool) -> i128 {
        match self.try_calculate_fee(notional, is_maker) {
            Ok(fee) => fee,
            Err(overflow) => {
                // Saturated clamp, bit-identical to the historical
                // `saturating_mul` behavior: the product caps at u128::MAX, so
                // after `/ 10_000` the magnitude is u128::MAX / 10_000 (which
                // always fits i128). The sign is applied afterward so a maker
                // rebate (negative bps) is preserved.
                let magnitude = i128::try_from(u128::MAX / BPS_DENOMINATOR).unwrap_or(i128::MAX);
                if overflow.bps < 0 {
                    -magnitude
                } else {
                    magnitude
                }
            }
        }
    }

    /// Calculate the fee for a transaction, or fail instead of clamping
    ///
    /// Fallible variant of [`Self::calculate_fee`]: identical inputs,
    /// identical rounding (truncation toward zero, sign applied after the
    /// unsigned-domain magnitude), but where `calculate_fee` saturates its
    /// intermediate product this returns [`FeeOverflow`]. An `Ok` value is
    /// therefore always the mathematically exact
    /// `sign(bps) × ⌊notional × |bps| / 10_000⌋` **and** equal to what
    /// [`Self::calculate_fee`] returns for the same inputs, suitable for
    /// journaled / replayable venues whose fee contract requires exact
    /// integer fees.
    ///
    /// The error condition is the multiplication-safety bound, not exactness
    /// itself: an `Err` means `calculate_fee` would clamp, whose result is
    /// merely not *guaranteed* exact — at isolated notionals the clamp can
    /// still coincide with the true fee. This variant rejects conservatively
    /// there, because a venue admitting such an input would have the engine
    /// journal a clamped, unverifiable fee.
    ///
    /// # Arguments
    ///
    /// * `notional` - The notional value of the trade (price × quantity)
    /// * `is_maker` - true if this is a maker transaction, false for taker
    ///
    /// # Errors
    ///
    /// Returns [`FeeOverflow`] if and only if `notional × |bps|` does not
    /// fit in `u128`, i.e. iff
    /// `notional > Self::max_guaranteed_exact_notional_for_bps(bps)`.
    /// A zero-bps rate never errors (the fee is exactly `0` for any
    /// notional).
    ///
    /// # Examples
    ///
    /// ```
    /// use orderbook_rs::FeeSchedule;
    ///
    /// let schedule = FeeSchedule::new(-2, 5);
    ///
    /// // Exact taker fee: 5 bps on $10,000 = $5.00
    /// let fee = schedule.try_calculate_fee(10_000_000, false)?;
    /// assert_eq!(fee, 5_000);
    ///
    /// // Beyond the guaranteed-exact bound the computation refuses to clamp.
    /// assert!(schedule.try_calculate_fee(u128::MAX, false).is_err());
    /// # Ok::<(), orderbook_rs::FeeOverflow>(())
    /// ```
    #[inline]
    pub fn try_calculate_fee(&self, notional: u128, is_maker: bool) -> Result<i128, FeeOverflow> {
        let bps = if is_maker {
            self.maker_fee_bps
        } else {
            self.taker_fee_bps
        };
        // Compute the magnitude in the u128 domain: notional * |bps| / 10_000.
        // Doing this as u128 (not i128) avoids truncating a notional above
        // i128::MAX into a negative value. If the product fits in u128, the
        // post-division magnitude is at most u128::MAX / 10_000 < i128::MAX,
        // so the i128 conversion below cannot fail — mapping it to the same
        // error keeps exactness a checked guarantee rather than a comment.
        let product = notional
            .checked_mul(u128::from(bps.unsigned_abs()))
            .ok_or_else(|| FeeOverflow::new(notional, bps))?;
        let magnitude = i128::try_from(product / BPS_DENOMINATOR)
            .map_err(|_| FeeOverflow::new(notional, bps))?;
        Ok(if bps < 0 { -magnitude } else { magnitude })
    }

    /// Largest notional whose fee at `bps` is guaranteed exact
    ///
    /// This is the **multiplication-safety bound** `u128::MAX / |bps|`
    /// (`u128::MAX` for a zero rate): at or below it the intermediate
    /// product `notional × |bps|` cannot overflow, so both
    /// [`Self::calculate_fee`] and [`Self::try_calculate_fee`] return the
    /// exact `⌊notional × |bps| / 10_000⌋` (signed). Callers can enforce it
    /// at admission time, making the saturating branch of
    /// [`Self::calculate_fee`] provably unreachable.
    ///
    /// The guarantee is sufficient, not tight: above the bound the
    /// multiplication overflows, so [`Self::try_calculate_fee`] always
    /// rejects and [`Self::calculate_fee`] clamps — even though at isolated
    /// notionals the clamped value can still coincide with the exact fee
    /// (e.g. `1 << 127` at 2 bps). "Guaranteed exact" is therefore a
    /// property of inputs at or below the bound, not a claim that every
    /// input above it is inexact.
    ///
    /// # Arguments
    ///
    /// * `bps` - Fee rate in basis points; only its magnitude matters
    ///
    /// # Examples
    ///
    /// ```
    /// use orderbook_rs::FeeSchedule;
    ///
    /// assert_eq!(
    ///     FeeSchedule::max_guaranteed_exact_notional_for_bps(5),
    ///     u128::MAX / 5
    /// );
    /// assert_eq!(
    ///     FeeSchedule::max_guaranteed_exact_notional_for_bps(-2),
    ///     u128::MAX / 2
    /// );
    /// assert_eq!(
    ///     FeeSchedule::max_guaranteed_exact_notional_for_bps(0),
    ///     u128::MAX
    /// );
    /// ```
    #[must_use]
    #[inline]
    pub const fn max_guaranteed_exact_notional_for_bps(bps: i32) -> u128 {
        match bps.unsigned_abs() {
            0 => u128::MAX,
            b => u128::MAX / b as u128,
        }
    }

    /// Largest notional guaranteed exact for both legs of this schedule
    ///
    /// The minimum of [`Self::max_guaranteed_exact_notional_for_bps`] over
    /// the maker and taker rates — a single venue-level admission bound: any
    /// notional at or below it produces exact maker **and** taker fees from
    /// [`Self::calculate_fee`] / [`Self::try_calculate_fee`]. Like the
    /// per-rate bound, it is a sufficient guarantee, not a tight exactness
    /// frontier.
    ///
    /// # Examples
    ///
    /// ```
    /// use orderbook_rs::FeeSchedule;
    ///
    /// let schedule = FeeSchedule::new(-2, 5);
    /// assert_eq!(schedule.max_guaranteed_exact_notional(), u128::MAX / 5);
    ///
    /// // A zero-fee schedule never saturates.
    /// assert_eq!(
    ///     FeeSchedule::zero_fee().max_guaranteed_exact_notional(),
    ///     u128::MAX
    /// );
    /// ```
    #[must_use]
    #[inline]
    pub const fn max_guaranteed_exact_notional(&self) -> u128 {
        let maker = Self::max_guaranteed_exact_notional_for_bps(self.maker_fee_bps);
        let taker = Self::max_guaranteed_exact_notional_for_bps(self.taker_fee_bps);
        if maker < taker { maker } else { taker }
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
    fn test_calculate_fee_notional_above_i128_max_keeps_sign_and_magnitude() {
        // A notional above i128::MAX previously cast to a negative i128, so a
        // taker fee came out negative (wrong sign) and a maker rebate positive.
        // Compute the magnitude in the u128 domain so the sign stays correct.
        let notional = u128::MAX; // far above i128::MAX
        let taker = FeeSchedule::new(0, 5);
        let taker_fee = taker.calculate_fee(notional, false);
        assert!(
            taker_fee > 0,
            "taker fee must stay positive, got {taker_fee}"
        );
        // Magnitude is u128::MAX * 5 / 10_000 (saturating mul caps at u128::MAX,
        // which still fits i128 after the divide).
        let expected = i128::try_from(u128::MAX.saturating_mul(5) / 10_000).unwrap_or(i128::MAX);
        assert_eq!(taker_fee, expected);

        let maker = FeeSchedule::new(-2, 5);
        let rebate = maker.calculate_fee(notional, true);
        assert!(rebate < 0, "maker rebate must stay negative, got {rebate}");
    }

    #[test]
    fn test_calculate_fee_unchanged_for_realistic_inputs() {
        // The u128-domain computation must match the old behaviour for normal
        // notionals (both taker fee and maker rebate, including truncation).
        let schedule = FeeSchedule::new(-2, 5);
        assert_eq!(schedule.calculate_fee(100_000_000, false), 50_000);
        assert_eq!(schedule.calculate_fee(100_000_000, true), -20_000);
        // Non-multiple-of-10_000 notional: floor(15_003 * 5 / 10_000) = 7.
        assert_eq!(schedule.calculate_fee(15_003, false), 7);
        // Maker rebate truncates toward zero just like the old i128 path.
        assert_eq!(schedule.calculate_fee(15_003, true), -3);
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
    fn test_try_calculate_fee_realistic_inputs_matches_calculate_fee() {
        // The fallible variant is exact on realistic inputs and agrees with
        // the infallible path, including truncation-toward-zero rounding.
        let schedule = FeeSchedule::new(-2, 5);
        for notional in [0u128, 1, 15_003, 100_000_000, u128::MAX / 5] {
            for is_maker in [true, false] {
                let exact = schedule.try_calculate_fee(notional, is_maker);
                assert_eq!(exact, Ok(schedule.calculate_fee(notional, is_maker)));
            }
        }
        assert_eq!(schedule.try_calculate_fee(15_003, false), Ok(7));
        assert_eq!(schedule.try_calculate_fee(15_003, true), Ok(-3));
    }

    #[test]
    fn test_try_calculate_fee_overflow_returns_error_with_fields() {
        let schedule = FeeSchedule::new(-2, 5);

        let taker_err = schedule.try_calculate_fee(u128::MAX, false);
        assert_eq!(
            taker_err,
            Err(FeeOverflow {
                notional: u128::MAX,
                bps: 5,
                max_guaranteed_exact_notional: u128::MAX / 5,
            })
        );

        // The maker leg reports the signed rate (-2), not its magnitude.
        let maker_err = schedule.try_calculate_fee(u128::MAX, true);
        assert_eq!(
            maker_err,
            Err(FeeOverflow {
                notional: u128::MAX,
                bps: -2,
                max_guaranteed_exact_notional: u128::MAX / 2,
            })
        );
    }

    #[test]
    fn test_try_calculate_fee_ok_at_bound_err_above_bound() {
        let schedule = FeeSchedule::new(-2, 5);
        let taker_bound = FeeSchedule::max_guaranteed_exact_notional_for_bps(5);
        let maker_bound = FeeSchedule::max_guaranteed_exact_notional_for_bps(-2);

        // Exactly at the bound the fee is still exact and agrees with the
        // saturating path (which does not saturate there).
        let at_bound = schedule.try_calculate_fee(taker_bound, false);
        assert_eq!(at_bound, Ok(schedule.calculate_fee(taker_bound, false)));
        assert!(schedule.try_calculate_fee(taker_bound + 1, false).is_err());

        let at_maker_bound = schedule.try_calculate_fee(maker_bound, true);
        assert_eq!(
            at_maker_bound,
            Ok(schedule.calculate_fee(maker_bound, true))
        );
        assert!(schedule.try_calculate_fee(maker_bound + 1, true).is_err());
    }

    #[test]
    fn test_try_calculate_fee_zero_bps_never_overflows() {
        let schedule = FeeSchedule::zero_fee();
        assert_eq!(schedule.try_calculate_fee(u128::MAX, false), Ok(0));
        assert_eq!(schedule.try_calculate_fee(u128::MAX, true), Ok(0));
    }

    #[test]
    fn test_max_guaranteed_exact_notional_for_bps_values() {
        assert_eq!(
            FeeSchedule::max_guaranteed_exact_notional_for_bps(0),
            u128::MAX
        );
        assert_eq!(
            FeeSchedule::max_guaranteed_exact_notional_for_bps(1),
            u128::MAX
        );
        assert_eq!(
            FeeSchedule::max_guaranteed_exact_notional_for_bps(5),
            u128::MAX / 5
        );
        assert_eq!(
            FeeSchedule::max_guaranteed_exact_notional_for_bps(-2),
            u128::MAX / 2
        );
        assert_eq!(
            FeeSchedule::max_guaranteed_exact_notional_for_bps(i32::MIN),
            u128::MAX / 2_147_483_648
        );
    }

    #[test]
    fn test_max_guaranteed_exact_notional_takes_min_of_both_legs() {
        assert_eq!(
            FeeSchedule::new(-2, 5).max_guaranteed_exact_notional(),
            u128::MAX / 5
        );
        assert_eq!(
            FeeSchedule::new(-7, 5).max_guaranteed_exact_notional(),
            u128::MAX / 7
        );
        assert_eq!(
            FeeSchedule::zero_fee().max_guaranteed_exact_notional(),
            u128::MAX
        );
    }

    #[test]
    fn test_calculate_fee_saturates_to_documented_clamp() {
        // Above the guaranteed-exact bound, calculate_fee clamps to the
        // documented magnitude u128::MAX / 10_000, signed per bps.
        let schedule = FeeSchedule::new(-2, 5);
        let clamp = i128::try_from(u128::MAX / 10_000).unwrap_or(i128::MAX);
        assert_eq!(schedule.calculate_fee(u128::MAX, false), clamp);
        assert_eq!(schedule.calculate_fee(u128::MAX, true), -clamp);
    }

    #[test]
    fn test_try_calculate_fee_rejects_conservatively_even_where_clamp_coincides() {
        // The bound is multiplication-safety, not a tight exactness frontier:
        // at notional 2^127 with 2 bps the product 2^128 overflows u128, so
        // try_calculate_fee rejects — yet the legacy clamp happens to equal
        // the true fee there, because floor((2^128 - 1) / 10_000) ==
        // floor(2^128 / 10_000) (2^128 mod 10_000 = 1456 >= 1). This pins the
        // documented "guaranteed exact is sufficient, not necessary" contract.
        let schedule = FeeSchedule::new(0, 2);
        let notional = 1u128 << 127;
        assert!(notional > FeeSchedule::max_guaranteed_exact_notional_for_bps(2));
        assert!(schedule.try_calculate_fee(notional, false).is_err());

        // Exact fee via a split that avoids the overflow: with
        // notional = q*10_000 + r, floor(notional*2/10_000) = q*2 +
        // floor(r*2/10_000).
        let q = notional / 10_000;
        let r = notional % 10_000;
        let exact = i128::try_from(q * 2 + (r * 2) / 10_000).unwrap_or(i128::MAX);
        // The clamp coincides with the exact value at this isolated point.
        assert_eq!(schedule.calculate_fee(notional, false), exact);
    }

    #[test]
    fn test_fee_overflow_display_contains_values() {
        let schedule = FeeSchedule::new(0, 5);
        let Err(err) = schedule.try_calculate_fee(u128::MAX, false) else {
            panic!("expected overflow error");
        };
        let msg = err.to_string();
        assert!(msg.contains("fee overflow"), "message was: {msg}");
        assert!(msg.contains(&u128::MAX.to_string()), "message was: {msg}");
        assert!(msg.contains('5'), "message was: {msg}");
        assert!(
            msg.contains(&(u128::MAX / 5).to_string()),
            "message was: {msg}"
        );
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
