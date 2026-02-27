use pricelevel::Side;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Event data for orderbook price level changes.
/// It is assumed that the listener is aware of the
/// order book context so we are not adding symbol here.
/// This event is sent on operations that update the order book price levels
/// e.g. adding, cancelling, updating or matching order
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PriceLevelChangedEvent {
    /// the order book side of the price level
    pub side: Side,

    /// price level price
    pub price: u128,

    /// latest visible quantity of the order book at this price level
    pub quantity: u64,
}

/// A thread-safe listener callback for price level change events.
///
/// This type alias represents a function that will be called whenever
/// a price level in the order book changes (e.g., order added, cancelled,
/// matched, or updated).
pub type PriceLevelChangedListener = Arc<dyn Fn(PriceLevelChangedEvent) + Send + Sync>;
