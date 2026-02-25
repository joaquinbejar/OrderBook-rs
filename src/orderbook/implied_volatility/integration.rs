//! Integration of implied volatility calculation with OrderBook.
//!
//! This module provides methods on the OrderBook struct to calculate
//! implied volatility from order book prices.

use super::black_scholes::BlackScholes;
use super::error::IVError;
use super::solver::{SolverConfig, solve_iv};
use super::types::{IVParams, IVQuality, IVResult, PriceSource};
use crate::orderbook::book::OrderBook;
use pricelevel::Side;

/// Threshold for high quality IV calculation (spread < 100 bps = 1%).
const HIGH_QUALITY_SPREAD_BPS: f64 = 100.0;

/// Threshold for medium quality IV calculation (spread < 500 bps = 5%).
const MEDIUM_QUALITY_SPREAD_BPS: f64 = 500.0;

/// Configuration for IV calculation from order book.
#[derive(Debug, Clone)]
pub struct IVConfig {
    /// Solver configuration for Newton-Raphson.
    pub solver: SolverConfig,
    /// Maximum allowed spread in basis points (default: 1000 bps = 10%).
    pub max_spread_bps: f64,
    /// Price scale factor to convert u64 prices to f64.
    /// For example, if prices are in cents, use 100.0 to get dollars.
    pub price_scale: f64,
}

impl Default for IVConfig {
    fn default() -> Self {
        Self {
            solver: SolverConfig::default(),
            max_spread_bps: 1000.0,
            price_scale: 1.0,
        }
    }
}

impl IVConfig {
    /// Creates a new IV configuration with default values.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the maximum allowed spread in basis points.
    #[must_use]
    pub fn with_max_spread(mut self, max_spread_bps: f64) -> Self {
        self.max_spread_bps = max_spread_bps;
        self
    }

    /// Sets the price scale factor.
    #[must_use]
    pub fn with_price_scale(mut self, price_scale: f64) -> Self {
        self.price_scale = price_scale;
        self
    }

    /// Sets the solver configuration.
    #[must_use]
    pub fn with_solver(mut self, solver: SolverConfig) -> Self {
        self.solver = solver;
        self
    }
}

impl<T> OrderBook<T>
where
    T: Clone + Send + Sync + Default + 'static,
{
    /// Calculates implied volatility for an option from order book prices.
    ///
    /// This method extracts the market price from the order book based on the
    /// specified price source, then uses Newton-Raphson to find the implied
    /// volatility that makes the Black-Scholes price equal to the market price.
    ///
    /// # Arguments
    /// - `params`: Option parameters (spot, strike, time, rate, type)
    /// - `price_source`: How to derive price from bid/ask (MidPrice, WeightedMid, LastTrade)
    ///
    /// # Returns
    /// - `Ok(IVResult)` with calculated IV and metadata
    /// - `Err(IVError)` if calculation fails
    ///
    /// # Example
    /// ```ignore
    /// use orderbook_rs::OrderBook;
    /// use orderbook_rs::implied_volatility::{IVParams, OptionType, PriceSource};
    ///
    /// let book = OrderBook::<()>::new("AAPL-C-150");
    /// // Add orders to the book...
    ///
    /// let params = IVParams {
    ///     spot: 150.0,
    ///     strike: 155.0,
    ///     time_to_expiry: 30.0 / 365.0,
    ///     risk_free_rate: 0.05,
    ///     option_type: OptionType::Call,
    /// };
    ///
    /// match book.implied_volatility(&params, PriceSource::MidPrice) {
    ///     Ok(result) => println!("IV: {:.2}%", result.iv_percent()),
    ///     Err(e) => eprintln!("Failed to calculate IV: {}", e),
    /// }
    /// ```
    pub fn implied_volatility(
        &self,
        params: &IVParams,
        price_source: PriceSource,
    ) -> Result<IVResult, IVError> {
        self.implied_volatility_with_config(params, price_source, &IVConfig::default())
    }

    /// Calculates implied volatility with custom configuration.
    ///
    /// # Arguments
    /// - `params`: Option parameters
    /// - `price_source`: Price extraction method
    /// - `config`: Custom IV calculation configuration
    ///
    /// # Returns
    /// - `Ok(IVResult)` with calculated IV and metadata
    /// - `Err(IVError)` if calculation fails
    pub fn implied_volatility_with_config(
        &self,
        params: &IVParams,
        price_source: PriceSource,
        config: &IVConfig,
    ) -> Result<IVResult, IVError> {
        // Extract price from order book
        let (price, spread_bps) = self.extract_price_for_iv(price_source, config.price_scale)?;

        // Check spread threshold
        if spread_bps > config.max_spread_bps {
            return Err(IVError::SpreadTooWide {
                spread_bps,
                threshold_bps: config.max_spread_bps,
            });
        }

        // Check if price is below intrinsic value
        let intrinsic = params.intrinsic_value();
        if price < intrinsic - config.solver.tolerance {
            return Err(IVError::PriceBelowIntrinsic { price, intrinsic });
        }

        // Determine quality based on spread
        let quality = spread_to_quality(spread_bps);

        // Solve for IV using Newton-Raphson
        let (iv, iterations) = solve_iv(params, price, &config.solver)?;

        Ok(IVResult::new(iv, price, spread_bps, iterations, quality))
    }

    /// Extracts the market price from the order book.
    ///
    /// # Arguments
    /// - `source`: Price extraction method
    /// - `price_scale`: Scale factor to convert u64 to f64
    ///
    /// # Returns
    /// - `Ok((price, spread_bps))`: Extracted price and spread in basis points
    /// - `Err(IVError::NoPriceAvailable)`: If no valid price can be extracted
    fn extract_price_for_iv(
        &self,
        source: PriceSource,
        price_scale: f64,
    ) -> Result<(f64, f64), IVError> {
        let best_bid = self.best_bid();
        let best_ask = self.best_ask();

        match (best_bid, best_ask) {
            (Some(bid), Some(ask)) => {
                let bid_f = bid as f64 / price_scale;
                let ask_f = ask as f64 / price_scale;
                let mid = (bid_f + ask_f) / 2.0;

                // Calculate spread in basis points
                let spread_bps = if mid > 0.0 {
                    ((ask_f - bid_f) / mid) * 10_000.0
                } else {
                    0.0
                };

                let price = match source {
                    PriceSource::MidPrice => mid,
                    PriceSource::WeightedMid => {
                        self.weighted_mid_price_for_iv(bid, ask, price_scale)
                    }
                    PriceSource::LastTrade => self
                        .last_trade_price()
                        .map(|p| p as f64 / price_scale)
                        .unwrap_or(mid),
                };

                Ok((price, spread_bps))
            }
            (Some(bid), None) => {
                // Only bid available - use bid price with high spread indicator
                let price = bid as f64 / price_scale;
                Ok((price, 10_000.0)) // 100% spread indicates one-sided market
            }
            (None, Some(ask)) => {
                // Only ask available - use ask price with high spread indicator
                let price = ask as f64 / price_scale;
                Ok((price, 10_000.0)) // 100% spread indicates one-sided market
            }
            (None, None) => Err(IVError::NoPriceAvailable),
        }
    }

    /// Calculates volume-weighted mid price.
    ///
    /// Weights the mid price by the quantities available at best bid and ask.
    /// This gives more weight to the side with more liquidity.
    fn weighted_mid_price_for_iv(&self, bid: u128, ask: u128, price_scale: f64) -> f64 {
        let bid_f = bid as f64 / price_scale;
        let ask_f = ask as f64 / price_scale;

        // Get quantities at best bid and ask
        let bid_qty = self.quantity_at_price(bid, Side::Buy);
        let ask_qty = self.quantity_at_price(ask, Side::Sell);

        let total_qty = bid_qty + ask_qty;

        if total_qty == 0 {
            // Fall back to simple mid if no quantities
            (bid_f + ask_f) / 2.0
        } else {
            // Weight by quantities: more weight to the side with more liquidity
            let bid_weight = ask_qty as f64 / total_qty as f64;
            let ask_weight = bid_qty as f64 / total_qty as f64;
            bid_f * bid_weight + ask_f * ask_weight
        }
    }

    /// Gets the total quantity at a specific price level.
    fn quantity_at_price(&self, price: u128, side: Side) -> u64 {
        let price_levels = match side {
            Side::Buy => &self.bids,
            Side::Sell => &self.asks,
        };

        price_levels
            .get(&price)
            .and_then(|entry| entry.value().total_quantity().ok())
            .unwrap_or(0)
    }

    /// Calculates the theoretical option price using Black-Scholes.
    ///
    /// This is a convenience method that uses the Black-Scholes model
    /// to price an option given the parameters and volatility.
    ///
    /// # Arguments
    /// - `params`: Option parameters
    /// - `volatility`: Implied or historical volatility
    ///
    /// # Returns
    /// Theoretical option price
    #[must_use]
    pub fn theoretical_price(params: &IVParams, volatility: f64) -> f64 {
        BlackScholes::price(params, volatility)
    }

    /// Calculates vega (sensitivity to volatility) for an option.
    ///
    /// # Arguments
    /// - `params`: Option parameters
    /// - `volatility`: Current volatility estimate
    ///
    /// # Returns
    /// Vega value (change in price per unit change in volatility)
    #[must_use]
    pub fn option_vega(params: &IVParams, volatility: f64) -> f64 {
        BlackScholes::vega(params, volatility)
    }

    /// Calculates delta (sensitivity to underlying price) for an option.
    ///
    /// # Arguments
    /// - `params`: Option parameters
    /// - `volatility`: Current volatility estimate
    ///
    /// # Returns
    /// Delta value
    #[must_use]
    pub fn option_delta(params: &IVParams, volatility: f64) -> f64 {
        BlackScholes::delta(params, volatility)
    }

    /// Calculates gamma (rate of change of delta) for an option.
    ///
    /// # Arguments
    /// - `params`: Option parameters
    /// - `volatility`: Current volatility estimate
    ///
    /// # Returns
    /// Gamma value
    #[must_use]
    pub fn option_gamma(params: &IVParams, volatility: f64) -> f64 {
        BlackScholes::gamma(params, volatility)
    }

    /// Calculates theta (time decay) for an option.
    ///
    /// # Arguments
    /// - `params`: Option parameters
    /// - `volatility`: Current volatility estimate
    ///
    /// # Returns
    /// Theta value (daily time decay, negative for long positions)
    #[must_use]
    pub fn option_theta(params: &IVParams, volatility: f64) -> f64 {
        BlackScholes::theta(params, volatility)
    }
}

/// Converts spread in basis points to IV quality indicator.
fn spread_to_quality(spread_bps: f64) -> IVQuality {
    if spread_bps < HIGH_QUALITY_SPREAD_BPS {
        IVQuality::High
    } else if spread_bps < MEDIUM_QUALITY_SPREAD_BPS {
        IVQuality::Medium
    } else {
        IVQuality::Low
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use pricelevel::{Id, TimeInForce};

    fn create_test_book() -> OrderBook<()> {
        let book = OrderBook::<()>::new("TEST-OPT");

        // Add bid orders
        let _ = book.add_limit_order(
            Id::new(),
            450, // $4.50
            100,
            Side::Buy,
            TimeInForce::Gtc,
            None,
        );

        // Add ask orders
        let _ = book.add_limit_order(
            Id::new(),
            470, // $4.70
            100,
            Side::Sell,
            TimeInForce::Gtc,
            None,
        );

        book
    }

    #[test]
    fn test_extract_price_mid() {
        let book = create_test_book();
        let config = IVConfig::default().with_price_scale(100.0);

        let (price, spread_bps) = book
            .extract_price_for_iv(PriceSource::MidPrice, config.price_scale)
            .unwrap();

        // Mid price should be (4.50 + 4.70) / 2 = 4.60
        assert!((price - 4.60).abs() < 0.01);
        // Spread should be (4.70 - 4.50) / 4.60 * 10000 ≈ 434.78 bps
        assert!(spread_bps > 400.0 && spread_bps < 500.0);
    }

    #[test]
    fn test_extract_price_weighted_mid() {
        let book = OrderBook::<()>::new("TEST-OPT");

        // Add bid with large quantity
        let _ = book.add_limit_order(Id::new(), 450, 1000, Side::Buy, TimeInForce::Gtc, None);

        // Add ask with small quantity
        let _ = book.add_limit_order(Id::new(), 470, 100, Side::Sell, TimeInForce::Gtc, None);

        let config = IVConfig::default().with_price_scale(100.0);

        let (price, _) = book
            .extract_price_for_iv(PriceSource::WeightedMid, config.price_scale)
            .unwrap();

        // Weighted mid should be closer to bid (more liquidity there)
        // bid_weight = ask_qty / total = 100 / 1100 ≈ 0.09
        // ask_weight = bid_qty / total = 1000 / 1100 ≈ 0.91
        // weighted = 4.50 * 0.09 + 4.70 * 0.91 ≈ 4.68
        assert!(price > 4.60); // Should be closer to ask due to more bid liquidity
    }

    #[test]
    fn test_extract_price_last_trade() {
        let book = create_test_book();

        // Execute a trade to set last trade price
        let _ = book.match_market_order(Id::new(), 50, Side::Buy);

        let config = IVConfig::default().with_price_scale(100.0);

        let (price, _) = book
            .extract_price_for_iv(PriceSource::LastTrade, config.price_scale)
            .unwrap();

        // Last trade should be at ask price (4.70)
        assert!((price - 4.70).abs() < 0.01);
    }

    #[test]
    fn test_extract_price_no_orders() {
        let book = OrderBook::<()>::new("EMPTY");

        let result = book.extract_price_for_iv(PriceSource::MidPrice, 1.0);
        assert!(matches!(result, Err(IVError::NoPriceAvailable)));
    }

    #[test]
    fn test_implied_volatility_calculation() {
        let book = OrderBook::<()>::new("TEST-OPT");

        // Create a book with prices that correspond to ~25% IV
        // For ATM option with S=100, K=100, T=0.25, r=0.05, σ=0.25
        // BS price ≈ 5.45
        let _ = book.add_limit_order(Id::new(), 540, 100, Side::Buy, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 550, 100, Side::Sell, TimeInForce::Gtc, None);

        let params = IVParams::call(100.0, 100.0, 0.25, 0.05);
        let config = IVConfig::default().with_price_scale(100.0);

        let result = book
            .implied_volatility_with_config(&params, PriceSource::MidPrice, &config)
            .unwrap();

        // IV should be close to 25%
        assert!(result.iv > 0.20 && result.iv < 0.30);
        assert!(result.iterations < 20);
    }

    #[test]
    fn test_implied_volatility_spread_too_wide() {
        let book = OrderBook::<()>::new("TEST-OPT");

        // Create a book with very wide spread
        let _ = book.add_limit_order(Id::new(), 100, 100, Side::Buy, TimeInForce::Gtc, None);
        let _ = book.add_limit_order(Id::new(), 500, 100, Side::Sell, TimeInForce::Gtc, None);

        let params = IVParams::call(100.0, 100.0, 0.25, 0.05);
        let config = IVConfig::default()
            .with_price_scale(100.0)
            .with_max_spread(500.0); // 5% max spread

        let result = book.implied_volatility_with_config(&params, PriceSource::MidPrice, &config);

        assert!(matches!(result, Err(IVError::SpreadTooWide { .. })));
    }

    #[test]
    fn test_spread_to_quality() {
        assert_eq!(spread_to_quality(50.0), IVQuality::High);
        assert_eq!(spread_to_quality(99.0), IVQuality::High);
        assert_eq!(spread_to_quality(100.0), IVQuality::Medium);
        assert_eq!(spread_to_quality(300.0), IVQuality::Medium);
        assert_eq!(spread_to_quality(499.0), IVQuality::Medium);
        assert_eq!(spread_to_quality(500.0), IVQuality::Low);
        assert_eq!(spread_to_quality(1000.0), IVQuality::Low);
    }

    #[test]
    fn test_iv_config_builder() {
        let config = IVConfig::new()
            .with_max_spread(2000.0)
            .with_price_scale(100.0)
            .with_solver(SolverConfig::default().with_max_iterations(50));

        assert!((config.max_spread_bps - 2000.0).abs() < 1e-10);
        assert!((config.price_scale - 100.0).abs() < 1e-10);
        assert_eq!(config.solver.max_iterations, 50);
    }

    #[test]
    fn test_theoretical_price() {
        let params = IVParams::call(100.0, 100.0, 0.25, 0.05);
        let price = OrderBook::<()>::theoretical_price(&params, 0.25);

        // ATM call with 25% vol should be around 5-6
        assert!(price > 4.0 && price < 7.0);
    }

    #[test]
    fn test_option_greeks() {
        let params = IVParams::call(100.0, 100.0, 0.25, 0.05);
        let vol = 0.25;

        let delta = OrderBook::<()>::option_delta(&params, vol);
        let gamma = OrderBook::<()>::option_gamma(&params, vol);
        let vega = OrderBook::<()>::option_vega(&params, vol);
        let theta = OrderBook::<()>::option_theta(&params, vol);

        // ATM call delta should be around 0.5
        assert!(delta > 0.4 && delta < 0.6);
        // Gamma should be positive
        assert!(gamma > 0.0);
        // Vega should be positive
        assert!(vega > 0.0);
        // Theta should be negative (time decay)
        assert!(theta < 0.0);
    }

    #[test]
    fn test_one_sided_market_bid_only() {
        let book = OrderBook::<()>::new("TEST-OPT");

        // Only bid, no ask
        let _ = book.add_limit_order(Id::new(), 450, 100, Side::Buy, TimeInForce::Gtc, None);

        let (price, spread_bps) = book
            .extract_price_for_iv(PriceSource::MidPrice, 100.0)
            .unwrap();

        assert!((price - 4.50).abs() < 0.01);
        assert!((spread_bps - 10_000.0).abs() < 1.0); // 100% spread indicator
    }

    #[test]
    fn test_one_sided_market_ask_only() {
        let book = OrderBook::<()>::new("TEST-OPT");

        // Only ask, no bid
        let _ = book.add_limit_order(Id::new(), 470, 100, Side::Sell, TimeInForce::Gtc, None);

        let (price, spread_bps) = book
            .extract_price_for_iv(PriceSource::MidPrice, 100.0)
            .unwrap();

        assert!((price - 4.70).abs() < 0.01);
        assert!((spread_bps - 10_000.0).abs() < 1.0); // 100% spread indicator
    }
}
