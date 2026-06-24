/******************************************************************************
   Author: Joaquín Béjar García
   Email: jb@taunais.com
   Date: 15/7/25
******************************************************************************/

use crossbeam::atomic::AtomicCell;
use serde::ser::SerializeStruct;
use serde::{Serialize, Serializer};
use std::sync::atomic::{AtomicBool, Ordering};

/// A best bid / ask fast-path cache for an [`OrderBook`](crate::OrderBook).
///
/// Each side carries its own validity flag, so reading one side never evicts
/// the other: after a `best_bid()` then `best_ask()` with no intervening
/// mutation, both are served from cache. Validity is tracked by a dedicated
/// `AtomicBool` per side rather than overloading price `0` as an "absent"
/// sentinel, so a genuine best level at price `0` is representable and
/// cacheable.
///
/// The cache is advisory: any book mutation calls [`invalidate`](Self::invalidate)
/// to clear both sides, and a missing side is recomputed from the skiplist. Only
/// non-empty sides are cached — an empty side leaves its flag clear and is
/// recomputed (an O(1) skiplist probe) on the next read.
#[derive(Debug, Default)]
pub struct PriceLevelCache {
    /// Cached best bid price. Meaningful only when `bid_valid` is set.
    best_bid_price: AtomicCell<u128>,
    /// Cached best ask price. Meaningful only when `ask_valid` is set.
    best_ask_price: AtomicCell<u128>,
    /// Whether `best_bid_price` currently holds a trustworthy value.
    bid_valid: AtomicBool,
    /// Whether `best_ask_price` currently holds a trustworthy value.
    ask_valid: AtomicBool,
}

impl Serialize for PriceLevelCache {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state = serializer.serialize_struct("PriceLevelCache", 4)?;
        state.serialize_field("best_bid_price", &self.best_bid_price.load())?;
        state.serialize_field("best_ask_price", &self.best_ask_price.load())?;
        state.serialize_field("bid_valid", &self.bid_valid.load(Ordering::Relaxed))?;
        state.serialize_field("ask_valid", &self.ask_valid.load(Ordering::Relaxed))?;
        state.end()
    }
}

impl PriceLevelCache {
    /// Create an empty cache with both sides invalid.
    pub fn new() -> Self {
        Self {
            best_bid_price: AtomicCell::new(0),
            best_ask_price: AtomicCell::new(0),
            bid_valid: AtomicBool::new(false),
            ask_valid: AtomicBool::new(false),
        }
    }

    /// Invalidate both sides. Called by every book mutation.
    pub fn invalidate(&self) {
        self.bid_valid.store(false, Ordering::Relaxed);
        self.ask_valid.store(false, Ordering::Relaxed);
    }

    /// Returns the cached best bid, or `None` on a cache miss (an empty or
    /// invalidated bid side). A cached price of `0` is a valid hit.
    pub fn get_cached_best_bid(&self) -> Option<u128> {
        // Acquire pairs with the Release in `update_best_bid`, so a reader that
        // observes `bid_valid == true` also observes the price stored before it.
        if self.bid_valid.load(Ordering::Acquire) {
            Some(self.best_bid_price.load())
        } else {
            None
        }
    }

    /// Returns the cached best ask, or `None` on a cache miss (an empty or
    /// invalidated ask side). A cached price of `0` is a valid hit.
    pub fn get_cached_best_ask(&self) -> Option<u128> {
        if self.ask_valid.load(Ordering::Acquire) {
            Some(self.best_ask_price.load())
        } else {
            None
        }
    }

    /// Update only the bid slot. `Some(price)` caches the price (including `0`);
    /// `None` (an empty side) leaves the slot invalid so the next read
    /// recomputes. The ask slot is never touched.
    pub fn update_best_bid(&self, best_bid: Option<u128>) {
        match best_bid {
            Some(price) => {
                self.best_bid_price.store(price);
                // Release so the price store above is visible to any reader that
                // sees `bid_valid == true`.
                self.bid_valid.store(true, Ordering::Release);
            }
            None => self.bid_valid.store(false, Ordering::Relaxed),
        }
    }

    /// Update only the ask slot. `Some(price)` caches the price (including `0`);
    /// `None` (an empty side) leaves the slot invalid so the next read
    /// recomputes. The bid slot is never touched.
    pub fn update_best_ask(&self, best_ask: Option<u128>) {
        match best_ask {
            Some(price) => {
                self.best_ask_price.store(price);
                self.ask_valid.store(true, Ordering::Release);
            }
            None => self.ask_valid.store(false, Ordering::Relaxed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_reading_one_side_does_not_evict_the_other() {
        let cache = PriceLevelCache::new();
        // Prime the bid side only (mirrors OrderBook::best_bid).
        cache.update_best_bid(Some(100));
        assert_eq!(cache.get_cached_best_bid(), Some(100));

        // Prime the ask side only (mirrors OrderBook::best_ask). This must NOT
        // clear the bid slot — the pre-fix shared flag + zero sentinel did.
        cache.update_best_ask(Some(110));
        assert_eq!(
            cache.get_cached_best_bid(),
            Some(100),
            "bid slot must survive an ask-side update"
        );
        assert_eq!(cache.get_cached_best_ask(), Some(110));
    }

    #[test]
    fn test_price_zero_is_cacheable() {
        let cache = PriceLevelCache::new();
        cache.update_best_bid(Some(0));
        assert_eq!(
            cache.get_cached_best_bid(),
            Some(0),
            "a genuine best level at price 0 must be a cache hit, not treated as absent"
        );
        cache.update_best_ask(Some(0));
        assert_eq!(cache.get_cached_best_ask(), Some(0));
    }

    #[test]
    fn test_empty_side_is_a_miss_and_does_not_touch_the_other() {
        let cache = PriceLevelCache::new();
        cache.update_best_bid(Some(100));
        // Ask side is empty: leaves the ask slot invalid (a miss → recompute),
        // and must not disturb the cached bid.
        cache.update_best_ask(None);
        assert_eq!(cache.get_cached_best_ask(), None);
        assert_eq!(cache.get_cached_best_bid(), Some(100));
    }

    #[test]
    fn test_invalidate_clears_both_sides() {
        let cache = PriceLevelCache::new();
        cache.update_best_bid(Some(100));
        cache.update_best_ask(Some(110));
        cache.invalidate();
        assert_eq!(cache.get_cached_best_bid(), None);
        assert_eq!(cache.get_cached_best_ask(), None);
    }
}
