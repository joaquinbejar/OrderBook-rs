//! Functional-style iterators for order book analysis
//!
//! This module provides efficient, lazy iterators for analyzing order book depth
//! and structure without unnecessary allocations. All iterators support standard
//! iterator combinators and can short-circuit early.

use crossbeam_skiplist::SkipMap;
use pricelevel::{PriceLevel, Side};
use std::sync::Arc;

/// Information about a price level including price, quantity, and cumulative depth
#[derive(Debug, Clone)]
pub struct LevelInfo {
    /// The price of this level (in price units)
    pub price: u128,

    /// Total quantity at this price level (in units)
    pub quantity: u64,

    /// Cumulative depth up to and including this level (in units)
    pub cumulative_depth: u64,
}

/// Iterator over price levels with cumulative depth tracking
///
/// Iterates through price levels in price-priority order (best to worst),
/// maintaining cumulative depth as it goes. This is useful for analyzing
/// market depth distribution and finding liquidity thresholds.
pub struct LevelsWithCumulativeDepth<'a> {
    iter: Box<dyn Iterator<Item = crossbeam_skiplist::map::Entry<'a, u128, Arc<PriceLevel>>> + 'a>,
    cumulative_depth: u64,
}

impl<'a> LevelsWithCumulativeDepth<'a> {
    /// Creates a new iterator over levels with cumulative depth
    ///
    /// # Arguments
    /// - `price_levels`: Reference to the SkipMap of price levels
    /// - `side`: Side to iterate (Buy for bids, Sell for asks)
    pub fn new(price_levels: &'a SkipMap<u128, Arc<PriceLevel>>, side: Side) -> Self {
        let iter: Box<dyn Iterator<Item = _> + 'a> = match side {
            Side::Buy => Box::new(price_levels.iter().rev()), // Highest to lowest
            Side::Sell => Box::new(price_levels.iter()),      // Lowest to highest
        };

        Self {
            iter,
            cumulative_depth: 0,
        }
    }
}

impl<'a> Iterator for LevelsWithCumulativeDepth<'a> {
    type Item = LevelInfo;

    fn next(&mut self) -> Option<Self::Item> {
        self.iter.next().map(|entry| {
            let price = *entry.key();
            let quantity = entry.value().total_quantity().unwrap_or(0);
            self.cumulative_depth = self.cumulative_depth.saturating_add(quantity);

            LevelInfo {
                price,
                quantity,
                cumulative_depth: self.cumulative_depth,
            }
        })
    }
}

/// Iterator over price levels until a target depth is reached
///
/// Stops automatically when the cumulative depth reaches or exceeds the target.
/// Useful for analyzing how many levels are needed to fill a specific quantity.
pub struct LevelsUntilDepth<'a> {
    iter: Box<dyn Iterator<Item = crossbeam_skiplist::map::Entry<'a, u128, Arc<PriceLevel>>> + 'a>,
    target_depth: u64,
    cumulative_depth: u64,
    finished: bool,
}

impl<'a> LevelsUntilDepth<'a> {
    /// Creates a new iterator that stops at target depth
    ///
    /// # Arguments
    /// - `price_levels`: Reference to the SkipMap of price levels
    /// - `side`: Side to iterate (Buy for bids, Sell for asks)
    /// - `target_depth`: Target cumulative depth (in units)
    pub fn new(
        price_levels: &'a SkipMap<u128, Arc<PriceLevel>>,
        side: Side,
        target_depth: u64,
    ) -> Self {
        let iter: Box<dyn Iterator<Item = _> + 'a> = match side {
            Side::Buy => Box::new(price_levels.iter().rev()),
            Side::Sell => Box::new(price_levels.iter()),
        };

        Self {
            iter,
            target_depth,
            cumulative_depth: 0,
            finished: false,
        }
    }
}

impl<'a> Iterator for LevelsUntilDepth<'a> {
    type Item = LevelInfo;

    fn next(&mut self) -> Option<Self::Item> {
        if self.finished {
            return None;
        }

        self.iter.next().map(|entry| {
            let price = *entry.key();
            let quantity = entry.value().total_quantity().unwrap_or(0);
            self.cumulative_depth = self.cumulative_depth.saturating_add(quantity);

            let level_info = LevelInfo {
                price,
                quantity,
                cumulative_depth: self.cumulative_depth,
            };

            // Check if we've reached target depth
            if self.cumulative_depth >= self.target_depth {
                self.finished = true;
            }

            level_info
        })
    }
}

/// Iterator over price levels within a specific price range
///
/// Only yields levels where the price falls within [min_price, max_price] inclusive.
/// Useful for analyzing liquidity in specific price bands.
pub struct LevelsInRange<'a> {
    iter: Box<dyn Iterator<Item = crossbeam_skiplist::map::Entry<'a, u128, Arc<PriceLevel>>> + 'a>,
    min_price: u128,
    max_price: u128,
    finished: bool,
}

impl<'a> LevelsInRange<'a> {
    /// Creates a new iterator over levels in a price range
    ///
    /// # Arguments
    /// - `price_levels`: Reference to the SkipMap of price levels
    /// - `side`: Side to iterate (Buy for bids, Sell for asks)
    /// - `min_price`: Minimum price (inclusive, in price units)
    /// - `max_price`: Maximum price (inclusive, in price units)
    pub fn new(
        price_levels: &'a SkipMap<u128, Arc<PriceLevel>>,
        side: Side,
        min_price: u128,
        max_price: u128,
    ) -> Self {
        let iter: Box<dyn Iterator<Item = _> + 'a> = match side {
            Side::Buy => Box::new(price_levels.iter().rev()),
            Side::Sell => Box::new(price_levels.iter()),
        };

        Self {
            iter,
            min_price,
            max_price,
            finished: false,
        }
    }
}

impl<'a> Iterator for LevelsInRange<'a> {
    type Item = LevelInfo;

    fn next(&mut self) -> Option<Self::Item> {
        if self.finished {
            return None;
        }

        for entry in self.iter.by_ref() {
            let price = *entry.key();

            // Check if price is within range
            if price >= self.min_price && price <= self.max_price {
                let quantity = entry.value().total_quantity().unwrap_or(0);

                return Some(LevelInfo {
                    price,
                    quantity,
                    cumulative_depth: 0, // Not tracked in range iterator
                });
            }

            // For efficiency, we can stop early if we've passed the range
            // This works because skipmap iteration is ordered
        }

        self.finished = true;
        None
    }
}
