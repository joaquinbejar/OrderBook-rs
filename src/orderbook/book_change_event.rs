//! Price-level change events emitted by the order book.
//!
//! Each `PriceLevelChangedEvent` carries an `engine_seq` minted by
//! `OrderBook::next_engine_seq` immediately before emission. The same
//! counter is shared with `TradeResult`, so the union of trade events
//! and price-level events emitted by a single `OrderBook<T>` instance
//! is **strictly monotonic** in `engine_seq`. Consumers can use this
//! cross-stream invariant for gap detection and temporal ordering
//! without correlating two independent counters.

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

    /// Strictly monotonic global engine sequence number for this event.
    /// See [`crate::orderbook::trade::TradeResult::engine_seq`] for the
    /// full cross-stream monotonicity contract.
    ///
    /// Defaults to `0` when deserializing payloads from format versions
    /// that pre-date `engine_seq` so existing consumers keep parsing.
    #[serde(default)]
    pub engine_seq: u64,
}

/// A thread-safe listener callback for price level change events.
///
/// This type alias represents a function that will be called whenever
/// a price level in the order book changes (e.g., order added, cancelled,
/// matched, or updated).
pub type PriceLevelChangedListener = Arc<dyn Fn(PriceLevelChangedEvent) + Send + Sync>;

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_event(engine_seq: u64) -> PriceLevelChangedEvent {
        PriceLevelChangedEvent {
            side: Side::Buy,
            price: 50_000,
            quantity: 250,
            engine_seq,
        }
    }

    #[test]
    fn test_engine_seq_default_zero_via_struct_literal() {
        let event = PriceLevelChangedEvent {
            side: Side::Sell,
            price: 1,
            quantity: 1,
            engine_seq: 0,
        };
        assert_eq!(event.engine_seq, 0);
    }

    #[test]
    fn test_json_roundtrip_preserves_engine_seq() {
        let event = sample_event(123);
        let bytes = serde_json::to_vec(&event).expect("serialize event");
        let decoded: PriceLevelChangedEvent =
            serde_json::from_slice(&bytes).expect("deserialize event");
        assert_eq!(decoded, event);
        assert_eq!(decoded.engine_seq, 123);
    }

    #[test]
    fn test_json_missing_engine_seq_defaults_zero() {
        // Construct a JSON payload that lacks the engine_seq field, which
        // models a payload produced by an earlier crate version.
        let json = r#"{"side":"Buy","price":42,"quantity":10}"#;
        let decoded: PriceLevelChangedEvent =
            serde_json::from_str(json).expect("deserialize legacy event");
        assert_eq!(
            decoded.engine_seq, 0,
            "missing engine_seq must default to 0 via #[serde(default)]"
        );
        assert_eq!(decoded.price, 42);
        assert_eq!(decoded.quantity, 10);
    }

    #[cfg(feature = "bincode")]
    #[test]
    fn test_bincode_roundtrip_preserves_engine_seq() {
        use bincode::config::standard;
        use bincode::serde::{decode_from_slice, encode_to_vec};

        let event = sample_event(9_000);
        let bytes = encode_to_vec(&event, standard()).expect("bincode encode");
        let (decoded, consumed): (PriceLevelChangedEvent, usize) =
            decode_from_slice(&bytes, standard()).expect("bincode decode");
        assert_eq!(consumed, bytes.len(), "no trailing bytes expected");
        assert_eq!(decoded, event);
        assert_eq!(decoded.engine_seq, 9_000);
    }
}
