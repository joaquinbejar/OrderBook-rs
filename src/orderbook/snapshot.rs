//! Order book snapshot for market data

use bitflags::bitflags;
use pricelevel::PriceLevelSnapshot;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracing::trace;

use super::error::OrderBookError;

/// A snapshot of the order book state at a specific point in time
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderBookSnapshot {
    /// The symbol or identifier for this order book
    pub symbol: String,

    /// Timestamp when the snapshot was created (milliseconds since epoch)
    pub timestamp: u64,

    /// Snapshot of bid price levels
    pub bids: Vec<PriceLevelSnapshot>,

    /// Snapshot of ask price levels
    pub asks: Vec<PriceLevelSnapshot>,
}

impl OrderBookSnapshot {
    /// Recomputes aggregate values for all included price levels.
    pub fn refresh_aggregates(&mut self) {
        for level in &mut self.bids {
            let _ = level.refresh_aggregates();
        }

        for level in &mut self.asks {
            let _ = level.refresh_aggregates();
        }
    }

    /// Get the best bid price and quantity
    pub fn best_bid(&self) -> Option<(u128, u64)> {
        let bids = self
            .bids
            .iter()
            .map(|level| (level.price(), level.visible_quantity()))
            .max_by_key(|&(price, _)| price);
        trace!("best_bid: {:?}", bids);
        bids
    }

    /// Get the best ask price and quantity
    pub fn best_ask(&self) -> Option<(u128, u64)> {
        let ask = self
            .asks
            .iter()
            .map(|level| (level.price(), level.visible_quantity()))
            .min_by_key(|&(price, _)| price);
        trace!("best_ask: {:?}", ask);
        ask
    }

    /// Get the mid price (average of best bid and best ask)
    pub fn mid_price(&self) -> Option<f64> {
        let mid_price = match (self.best_bid(), self.best_ask()) {
            (Some((bid_price, _)), Some((ask_price, _))) => {
                Some((bid_price as f64 + ask_price as f64) / 2.0)
            }
            _ => None,
        };
        trace!("mid_price: {:?}", mid_price);
        mid_price
    }

    /// Get the spread (best ask - best bid)
    pub fn spread(&self) -> Option<u128> {
        let spread = match (self.best_bid(), self.best_ask()) {
            (Some((bid_price, _)), Some((ask_price, _))) => {
                Some(ask_price.saturating_sub(bid_price))
            }
            _ => None,
        };
        trace!("spread: {:?}", spread);
        spread
    }

    /// Calculate the total volume on the bid side
    pub fn total_bid_volume(&self) -> u64 {
        let volume = self
            .bids
            .iter()
            .map(|level| level.total_quantity().unwrap_or(0))
            .sum();
        trace!("total_bid_volume: {:?}", volume);
        volume
    }

    /// Calculate the total volume on the ask side
    pub fn total_ask_volume(&self) -> u64 {
        let volume = self
            .asks
            .iter()
            .map(|level| level.total_quantity().unwrap_or(0))
            .sum();
        trace!("total_ask_volume: {:?}", volume);
        volume
    }

    /// Calculate the total value on the bid side (price * quantity)
    pub fn total_bid_value(&self) -> u128 {
        let value = self
            .bids
            .iter()
            .map(|level| {
                level
                    .price()
                    .saturating_mul(level.total_quantity().unwrap_or(0) as u128)
            })
            .sum();
        trace!("total_bid_value: {:?}", value);
        value
    }

    /// Calculate the total value on the ask side (price * quantity)
    pub fn total_ask_value(&self) -> u128 {
        let value = self
            .asks
            .iter()
            .map(|level| {
                level
                    .price()
                    .saturating_mul(level.total_quantity().unwrap_or(0) as u128)
            })
            .sum();
        trace!("total_ask_value: {:?}", value);
        value
    }
}

/// Format version used for checksum-enabled order book snapshots.
pub const ORDERBOOK_SNAPSHOT_FORMAT_VERSION: u32 = 1;

/// Wrapper that provides checksum validation for `OrderBookSnapshot` instances.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderBookSnapshotPackage {
    /// Version of the snapshot schema for forward compatibility.
    pub version: u32,
    /// Snapshot payload.
    pub snapshot: OrderBookSnapshot,
    /// Hex-encoded checksum of the serialized snapshot.
    pub checksum: String,
}

impl OrderBookSnapshotPackage {
    /// Creates a new snapshot package computing the checksum of the snapshot contents.
    pub fn new(mut snapshot: OrderBookSnapshot) -> Result<Self, OrderBookError> {
        snapshot.refresh_aggregates();

        let checksum = Self::compute_checksum(&snapshot)?;

        Ok(Self {
            version: ORDERBOOK_SNAPSHOT_FORMAT_VERSION,
            snapshot,
            checksum,
        })
    }

    /// Serializes the package to JSON.
    pub fn to_json(&self) -> Result<String, OrderBookError> {
        serde_json::to_string(self).map_err(|error| OrderBookError::SerializationError {
            message: error.to_string(),
        })
    }

    /// Deserializes the package from JSON.
    pub fn from_json(data: &str) -> Result<Self, OrderBookError> {
        serde_json::from_str(data).map_err(|error| OrderBookError::DeserializationError {
            message: error.to_string(),
        })
    }

    /// Validates the checksum and version.
    pub fn validate(&self) -> Result<(), OrderBookError> {
        if self.version != ORDERBOOK_SNAPSHOT_FORMAT_VERSION {
            return Err(OrderBookError::InvalidOperation {
                message: format!(
                    "Unsupported snapshot version: {} (expected {})",
                    self.version, ORDERBOOK_SNAPSHOT_FORMAT_VERSION
                ),
            });
        }

        let computed = Self::compute_checksum(&self.snapshot)?;
        if computed != self.checksum {
            return Err(OrderBookError::ChecksumMismatch {
                expected: self.checksum.clone(),
                actual: computed,
            });
        }

        Ok(())
    }

    /// Consumes the package and returns the validated snapshot.
    pub fn into_snapshot(self) -> Result<OrderBookSnapshot, OrderBookError> {
        self.validate()?;
        Ok(self.snapshot)
    }

    fn compute_checksum(snapshot: &OrderBookSnapshot) -> Result<String, OrderBookError> {
        let payload =
            serde_json::to_vec(snapshot).map_err(|error| OrderBookError::SerializationError {
                message: error.to_string(),
            })?;

        let mut hasher = Sha256::new();
        hasher.update(payload);

        let checksum_bytes = hasher.finalize();
        Ok(format!("{:x}", checksum_bytes))
    }
}

bitflags! {
    /// Flags for selecting which metrics to calculate in enriched snapshots
    ///
    /// Use these flags to optimize performance by calculating only the metrics
    /// you need. Multiple flags can be combined using bitwise OR.
    ///
    /// # Examples
    /// ```
    /// use orderbook_rs::MetricFlags;
    ///
    /// // Calculate only mid price and spread
    /// let flags = MetricFlags::MID_PRICE | MetricFlags::SPREAD;
    ///
    /// // Calculate all metrics
    /// let flags = MetricFlags::ALL;
    /// ```
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub struct MetricFlags: u32 {
        /// Calculate mid price (average of best bid and ask)
        const MID_PRICE = 1 << 0;

        /// Calculate spread in basis points
        const SPREAD = 1 << 1;

        /// Calculate total depth on each side
        const DEPTH = 1 << 2;

        /// Calculate VWAP for top N levels
        const VWAP = 1 << 3;

        /// Calculate order book imbalance
        const IMBALANCE = 1 << 4;

        /// Calculate all metrics
        const ALL = Self::MID_PRICE.bits() | Self::SPREAD.bits()
                  | Self::DEPTH.bits() | Self::VWAP.bits() | Self::IMBALANCE.bits();
    }
}

/// An enriched snapshot with pre-calculated metrics
///
/// This provides better performance than creating a snapshot and calculating
/// metrics separately, as it computes everything in a single pass through the data.
/// This is particularly beneficial for high-frequency trading applications.
///
/// # Performance
/// - Single pass through data vs multiple passes
/// - Better cache locality
/// - Optional metric selection for optimization
///
/// # Examples
/// ```
/// use orderbook_rs::OrderBook;
/// use pricelevel::{Id, Side, TimeInForce};
///
/// let book = OrderBook::<()>::new("BTC/USD");
/// let _ = book.add_limit_order(Id::new(), 100, 10, Side::Buy, TimeInForce::Gtc, None);
/// let _ = book.add_limit_order(Id::new(), 101, 10, Side::Sell, TimeInForce::Gtc, None);
///
/// let snapshot = book.enriched_snapshot(10);
///
/// if let Some(mid) = snapshot.mid_price {
///     println!("Mid price: {}", mid);
/// }
/// if let Some(spread) = snapshot.spread_bps {
///     println!("Spread: {} bps", spread);
/// }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnrichedSnapshot {
    /// The symbol or identifier for this order book
    pub symbol: String,

    /// Timestamp when the snapshot was created (milliseconds since epoch)
    pub timestamp: u64,

    /// Snapshot of bid price levels
    pub bids: Vec<PriceLevelSnapshot>,

    /// Snapshot of ask price levels
    pub asks: Vec<PriceLevelSnapshot>,

    /// Mid price (average of best bid and best ask)
    pub mid_price: Option<f64>,

    /// Spread in basis points
    pub spread_bps: Option<f64>,

    /// Total depth on bid side (in units)
    pub bid_depth_total: u64,

    /// Total depth on ask side (in units)
    pub ask_depth_total: u64,

    /// Order book imbalance (-1.0 to 1.0)
    pub order_book_imbalance: f64,

    /// VWAP for top N bid levels
    pub vwap_bid: Option<f64>,

    /// VWAP for top N ask levels
    pub vwap_ask: Option<f64>,
}

impl EnrichedSnapshot {
    /// Creates a new enriched snapshot with all metrics calculated
    ///
    /// # Arguments
    /// - `symbol`: Symbol identifier
    /// - `timestamp`: Timestamp in milliseconds
    /// - `bids`: Bid price levels
    /// - `asks`: Ask price levels
    /// - `vwap_levels`: Number of levels to use for VWAP calculation
    /// - `imbalance_levels`: Number of levels to use for imbalance calculation
    pub fn new(
        symbol: String,
        timestamp: u64,
        bids: Vec<PriceLevelSnapshot>,
        asks: Vec<PriceLevelSnapshot>,
        vwap_levels: usize,
        imbalance_levels: usize,
    ) -> Self {
        Self::with_metrics(
            symbol,
            timestamp,
            bids,
            asks,
            vwap_levels,
            imbalance_levels,
            MetricFlags::ALL,
        )
    }

    /// Creates a new enriched snapshot with custom metric selection
    ///
    /// # Arguments
    /// - `symbol`: Symbol identifier
    /// - `timestamp`: Timestamp in milliseconds
    /// - `bids`: Bid price levels
    /// - `asks`: Ask price levels
    /// - `vwap_levels`: Number of levels to use for VWAP calculation
    /// - `imbalance_levels`: Number of levels to use for imbalance calculation
    /// - `flags`: Metrics to calculate
    pub fn with_metrics(
        symbol: String,
        timestamp: u64,
        bids: Vec<PriceLevelSnapshot>,
        asks: Vec<PriceLevelSnapshot>,
        vwap_levels: usize,
        imbalance_levels: usize,
        flags: MetricFlags,
    ) -> Self {
        // Calculate mid price if needed
        let mid_price = if flags.contains(MetricFlags::MID_PRICE) {
            Self::calculate_mid_price(&bids, &asks)
        } else {
            None
        };

        // Calculate spread if needed
        let spread_bps = if flags.contains(MetricFlags::SPREAD) {
            Self::calculate_spread_bps(&bids, &asks)
        } else {
            None
        };

        // Calculate depths if needed
        let (bid_depth_total, ask_depth_total) = if flags.contains(MetricFlags::DEPTH) {
            (
                Self::calculate_total_depth(&bids),
                Self::calculate_total_depth(&asks),
            )
        } else {
            (0, 0)
        };

        // Calculate VWAP if needed
        let (vwap_bid, vwap_ask) = if flags.contains(MetricFlags::VWAP) {
            (
                Self::calculate_vwap(&bids, vwap_levels),
                Self::calculate_vwap(&asks, vwap_levels),
            )
        } else {
            (None, None)
        };

        // Calculate imbalance if needed
        let order_book_imbalance = if flags.contains(MetricFlags::IMBALANCE) {
            Self::calculate_imbalance(&bids, &asks, imbalance_levels)
        } else {
            0.0
        };

        Self {
            symbol,
            timestamp,
            bids,
            asks,
            mid_price,
            spread_bps,
            bid_depth_total,
            ask_depth_total,
            order_book_imbalance,
            vwap_bid,
            vwap_ask,
        }
    }

    fn calculate_mid_price(
        bids: &[PriceLevelSnapshot],
        asks: &[PriceLevelSnapshot],
    ) -> Option<f64> {
        let best_bid = bids.first().map(|l| l.price())?;
        let best_ask = asks.first().map(|l| l.price())?;
        Some((best_bid as f64 + best_ask as f64) / 2.0)
    }

    fn calculate_spread_bps(
        bids: &[PriceLevelSnapshot],
        asks: &[PriceLevelSnapshot],
    ) -> Option<f64> {
        let best_bid = bids.first().map(|l| l.price())? as f64;
        let best_ask = asks.first().map(|l| l.price())? as f64;
        let mid_price = (best_bid + best_ask) / 2.0;

        if mid_price == 0.0 {
            return None;
        }

        let spread = best_ask - best_bid;
        Some((spread / mid_price) * 10000.0)
    }

    fn calculate_total_depth(levels: &[PriceLevelSnapshot]) -> u64 {
        levels.iter().map(|l| l.total_quantity().unwrap_or(0)).sum()
    }

    fn calculate_vwap(levels: &[PriceLevelSnapshot], max_levels: usize) -> Option<f64> {
        let levels_to_use = levels.iter().take(max_levels);

        let mut total_value = 0u128;
        let mut total_quantity = 0u64;

        for level in levels_to_use {
            let quantity = level.total_quantity().unwrap_or(0);
            if quantity > 0 {
                total_value =
                    total_value.saturating_add(level.price().saturating_mul(quantity as u128));
                total_quantity = total_quantity.saturating_add(quantity);
            }
        }

        if total_quantity == 0 {
            None
        } else {
            Some(total_value as f64 / total_quantity as f64)
        }
    }

    fn calculate_imbalance(
        bids: &[PriceLevelSnapshot],
        asks: &[PriceLevelSnapshot],
        max_levels: usize,
    ) -> f64 {
        let bid_volume: u64 = bids
            .iter()
            .take(max_levels)
            .map(|l| l.total_quantity().unwrap_or(0))
            .sum();

        let ask_volume: u64 = asks
            .iter()
            .take(max_levels)
            .map(|l| l.total_quantity().unwrap_or(0))
            .sum();

        let total = bid_volume + ask_volume;

        if total == 0 {
            0.0
        } else {
            (bid_volume as f64 - ask_volume as f64) / total as f64
        }
    }
}
