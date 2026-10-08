/******************************************************************************
   Author: Joaquín Béjar García
   Email: jb@taunais.com
   Date: 15/7/25
******************************************************************************/

use crossbeam::atomic::AtomicCell;
use serde::ser::SerializeStruct;
use serde::{Serialize, Serializer};

/// Atomic cached price layout
/// bit 127          bits 0..126
/// ┌──────┬───────────────────────────────┐
/// │ valid│            price              │
/// └──────┴───────────────────────────────┘
/// mask for valid price
const VALID_MASK: u128 = 1 << 127;

/// mask for price value
const PRICE_MASK: u128 = !VALID_MASK;

/// A best bid / ask fast-path cache for an [`OrderBook`](crate::OrderBook).
///
/// Each side carries its own validity flag, so reading one side never evicts
/// the other: after a `best_bid()` then `best_ask()` with no intervening
/// mutation, both are served from cache.
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
}

impl Serialize for PriceLevelCache {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state = serializer.serialize_struct("PriceLevelCache", 4)?;
        let (bid_valid, bid_price) = Self::decode(self.best_bid_price.load());
        let (ask_valid, ask_price) = Self::decode(self.best_ask_price.load());
        state.serialize_field("best_bid_price", &bid_price)?;
        state.serialize_field("best_ask_price", &ask_price)?;
        state.serialize_field("bid_valid", &bid_valid)?;
        state.serialize_field("ask_valid", &ask_valid)?;
        state.end()
    }
}

impl PriceLevelCache {
    /// Create an empty cache with both sides invalid.
    pub fn new() -> Self {
        Self {
            best_bid_price: AtomicCell::new(0),
            best_ask_price: AtomicCell::new(0),
        }
    }

    /// Apply the validity mask on the price with bits-OR op if it is not `u128::MAX`
    fn encode(price: u128) -> u128 {
        if price == u128::MAX {
            return price;
        }
        price | VALID_MASK
    }

    /// Decodes the cached data.
    fn decode(encoded: u128) -> (bool, u128) {
        if encoded == u128::MAX {
            return (false, encoded);
        }
        if encoded & VALID_MASK == 0 {
            (false, encoded & PRICE_MASK)
        } else {
            (true, encoded & PRICE_MASK)
        }
    }

    /// Invalidate both sides. Called by every book mutation.
    pub fn invalidate(&self) {
        self.best_bid_price.fetch_and(PRICE_MASK);
        self.best_ask_price.fetch_and(PRICE_MASK);
    }

    /// Returns the cached best bid, or `None` on a cache miss (an empty or
    /// invalidated bid side). A cached price of `0` is a valid hit.
    pub fn get_cached_best_bid(&self) -> Option<u128> {
        match Self::decode(self.best_bid_price.load()) {
            (true, price) => Some(price),
            _ => None,
        }
    }

    /// Returns the cached best ask, or `None` on a cache miss (an empty or
    /// invalidated ask side). A cached price of `0` is a valid hit.
    pub fn get_cached_best_ask(&self) -> Option<u128> {
        match Self::decode(self.best_ask_price.load()) {
            (true, price) => Some(price),
            _ => None,
        }
    }

    /// Updates the best bid price atomically. If the price is `None`, the price becomes invalid
    /// while keeping the price value
    pub fn update_best_bid(&self, best_bid: Option<u128>) {
        match best_bid {
            Some(price) => {
                self.best_bid_price.store(Self::encode(price));
            }
            None => {
                self.best_bid_price.fetch_and(PRICE_MASK);
            }
        }
    }

    /// Updates the ask price atomically. If the price is `None`, the price becomes invalid
    /// while keeping the price value
    pub fn update_best_ask(&self, best_ask: Option<u128>) {
        match best_ask {
            Some(price) => {
                self.best_ask_price.store(Self::encode(price));
            }
            None => {
                // keep the price but turn off the valid bit
                self.best_ask_price.fetch_and(PRICE_MASK);
            }
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

    #[test]
    fn test_default_cache_is_invalid() {
        let cache = PriceLevelCache::new();
        assert_eq!(cache.get_cached_best_bid(), None);
        assert_eq!(cache.get_cached_best_ask(), None);
    }

    #[test]
    fn test_empty_bid_side_is_a_miss_and_does_not_touch_the_other() {
        let cache = PriceLevelCache::new();
        cache.update_best_ask(Some(110));
        cache.update_best_bid(None);
        assert_eq!(cache.get_cached_best_bid(), None);
        assert_eq!(cache.get_cached_best_ask(), Some(110));
    }

    #[test]
    fn test_u128_max_handling() {
        assert_eq!(PriceLevelCache::encode(u128::MAX), u128::MAX);
        assert_eq!(PriceLevelCache::decode(u128::MAX), (false, u128::MAX));

        let cache = PriceLevelCache::new();
        cache.update_best_bid(Some(u128::MAX));
        assert_eq!(cache.get_cached_best_bid(), None);

        cache.update_best_ask(Some(u128::MAX));
        assert_eq!(cache.get_cached_best_ask(), None);
    }

    #[test]
    fn test_price_level_cache_serialization() {
        let cache = PriceLevelCache::new();
        cache.update_best_bid(Some(100));
        cache.update_best_ask(Some(200));

        let json = serde_json::to_string(&cache).expect("serialization must succeed");
        assert!(json.contains("\"best_bid_price\":100"));
        assert!(json.contains("\"best_ask_price\":200"));
        assert!(json.contains("\"bid_valid\":true"));
        assert!(json.contains("\"ask_valid\":true"));

        cache.invalidate();
        let json_invalid = serde_json::to_string(&cache).expect("serialization must succeed");
        assert!(json_invalid.contains("\"best_bid_price\":100"));
        assert!(json_invalid.contains("\"best_ask_price\":200"));
        assert!(json_invalid.contains("\"bid_valid\":false"));
        assert!(json_invalid.contains("\"ask_valid\":false"));
    }
}
