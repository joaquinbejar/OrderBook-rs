//! Functional-style iterators for order book analysis
//!
//! This module provides efficient, lazy iterators for analyzing order book depth
//! and structure without unnecessary allocations. All iterators support standard
//! iterator combinators and can short-circuit early.

use crossbeam_skiplist::SkipMap;
use crossbeam_skiplist::map::Iter;
use either::Either;
use pricelevel::{PriceLevel, Side};
use std::iter::Rev;
use std::sync::Arc;

/// Direction-erased iterator over price levels in a [`SkipMap`].
///
/// Wraps either a reverse (highest-to-lowest) or forward (lowest-to-highest) iterator:
/// bids ([`Side::Buy`]) iterate in descending price order, while asks ([`Side::Sell`])
/// iterate in ascending price order.
type PriceLevelIter<'a> =
    Either<Rev<Iter<'a, u128, Arc<PriceLevel>>>, Iter<'a, u128, Arc<PriceLevel>>>;

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
    iter: PriceLevelIter<'a>,
    cumulative_depth: u64,
}

impl<'a> LevelsWithCumulativeDepth<'a> {
    /// Creates a new iterator over levels with cumulative depth
    ///
    /// # Arguments
    /// - `price_levels`: Reference to the SkipMap of price levels
    /// - `side`: Side to iterate (Buy for bids, Sell for asks)
    pub fn new(price_levels: &'a SkipMap<u128, Arc<PriceLevel>>, side: Side) -> Self {
        let iter = match side {
            Side::Buy => Either::Left(price_levels.iter().rev()), // Highest to lowest
            Side::Sell => Either::Right(price_levels.iter()),     // Lowest to highest
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
    iter: PriceLevelIter<'a>,
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
        let iter = match side {
            Side::Buy => Either::Left(price_levels.iter().rev()),
            Side::Sell => Either::Right(price_levels.iter()),
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
    iter: PriceLevelIter<'a>,
    side: Side,
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
        let iter = match side {
            Side::Buy => Either::Left(price_levels.iter().rev()),
            Side::Sell => Either::Right(price_levels.iter()),
        };

        Self {
            iter,
            side,
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

            // Ordered early exit: the underlying SkipMap iteration is sorted, so
            // once a price passes the FAR edge of the band no later entry can be
            // in range. Buy iterates descending (high→low): a price below the
            // band ends it. Sell iterates ascending (low→high): a price above the
            // band ends it.
            let past_far_edge = match self.side {
                Side::Buy => price < self.min_price,
                Side::Sell => price > self.max_price,
            };
            if past_far_edge {
                self.finished = true;
                return None;
            }

            // Check if price is within range.
            if price >= self.min_price && price <= self.max_price {
                let quantity = entry.value().total_quantity().unwrap_or(0);

                return Some(LevelInfo {
                    price,
                    quantity,
                    cumulative_depth: 0, // Not tracked in range iterator
                });
            }
            // Otherwise we are still on the NEAR side of the band (Buy: above
            // max; Sell: below min) — keep scanning toward it.
        }

        self.finished = true;
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_map(prices: impl IntoIterator<Item = u128>) -> SkipMap<u128, Arc<PriceLevel>> {
        let map = SkipMap::new();
        for p in prices {
            map.insert(p, Arc::new(PriceLevel::new(p)));
        }
        map
    }

    #[test]
    fn test_levels_in_range_terminates_at_far_edge_sell() {
        // Wide ascending book 1..=1000, narrow band [10, 12] near the low end.
        let map = make_map(1..=1000u128);
        let mut it = LevelsInRange::new(&map, Side::Sell, 10, 12);
        let prices: Vec<u128> = (&mut it).map(|l| l.price).collect();
        assert_eq!(prices, vec![10, 11, 12], "only in-band levels are yielded");
        assert!(
            it.finished,
            "iterator marks itself finished at the far edge"
        );
        // Early-exit proof: the underlying iterator was NOT drained to the end —
        // entries past the far edge (13..=1000) remain. A non-short-circuiting
        // scan would have consumed all of them.
        assert!(
            it.iter.next().is_some(),
            "iteration must stop at the far edge, leaving later entries unconsumed"
        );
    }

    #[test]
    fn test_levels_in_range_terminates_at_far_edge_buy() {
        // Buy iterates descending; narrow band [988, 990] near the high end.
        let map = make_map(1..=1000u128);
        let mut it = LevelsInRange::new(&map, Side::Buy, 988, 990);
        let prices: Vec<u128> = (&mut it).map(|l| l.price).collect();
        assert_eq!(prices, vec![990, 989, 988], "descending in-band yield");
        assert!(it.finished);
        assert!(
            it.iter.next().is_some(),
            "iteration must stop below the band, leaving lower entries unconsumed"
        );
    }

    #[test]
    fn test_levels_in_range_empty_when_band_outside_book() {
        let map = make_map(1..=10u128);
        let got: Vec<u128> = LevelsInRange::new(&map, Side::Sell, 100, 200)
            .map(|l| l.price)
            .collect();
        assert!(got.is_empty());
    }
}
