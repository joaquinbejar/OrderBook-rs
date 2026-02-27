//! Pluggable event serialization for NATS publishers and consumers.
//!
//! This module provides the [`EventSerializer`] trait and two built-in
//! implementations:
//!
//! - [`JsonEventSerializer`] — human-readable JSON (always available)
//! - `BincodeEventSerializer` — compact binary format (requires the
//!   `bincode` feature)
//!
//! Publishers such as `NatsTradePublisher` (requires the `nats` feature)
//! accept any `Arc<dyn EventSerializer>` so the serialization format can be
//! chosen at construction time without changing downstream code.
//!
//! # Feature Gate
//!
//! The `BincodeEventSerializer` requires the `bincode` feature:
//!
//! ```toml
//! [dependencies]
//! orderbook-rs = { version = "0.6", features = ["bincode"] }
//! ```

use crate::orderbook::book_change_event::PriceLevelChangedEvent;
use crate::orderbook::trade::TradeResult;

/// Errors that can occur during event serialization or deserialization.
#[derive(Debug)]
pub struct SerializationError {
    /// Human-readable description of the failure.
    pub message: String,
}

impl std::fmt::Display for SerializationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "event serialization error: {}", self.message)
    }
}

impl std::error::Error for SerializationError {}

/// A pluggable serializer for order book events.
///
/// Implementations convert [`TradeResult`] and [`PriceLevelChangedEvent`]
/// to and from byte buffers. The format (JSON, Bincode, etc.) is an
/// implementation detail, allowing publishers and consumers to negotiate
/// the most efficient wire format.
///
/// # Thread Safety
///
/// Implementations must be `Send + Sync` so they can be shared across
/// async task boundaries via `Arc<dyn EventSerializer>`.
pub trait EventSerializer: Send + Sync + std::fmt::Debug {
    /// Serialize a [`TradeResult`] into a byte buffer.
    ///
    /// # Errors
    ///
    /// Returns [`SerializationError`] if the event cannot be serialized.
    fn serialize_trade(&self, trade: &TradeResult) -> Result<Vec<u8>, SerializationError>;

    /// Serialize a [`PriceLevelChangedEvent`] into a byte buffer.
    ///
    /// # Errors
    ///
    /// Returns [`SerializationError`] if the event cannot be serialized.
    fn serialize_book_change(
        &self,
        event: &PriceLevelChangedEvent,
    ) -> Result<Vec<u8>, SerializationError>;

    /// Deserialize a [`TradeResult`] from a byte buffer.
    ///
    /// # Errors
    ///
    /// Returns [`SerializationError`] if the bytes are malformed or
    /// incompatible with the expected format.
    fn deserialize_trade(&self, data: &[u8]) -> Result<TradeResult, SerializationError>;

    /// Deserialize a [`PriceLevelChangedEvent`] from a byte buffer.
    ///
    /// # Errors
    ///
    /// Returns [`SerializationError`] if the bytes are malformed or
    /// incompatible with the expected format.
    fn deserialize_book_change(
        &self,
        data: &[u8],
    ) -> Result<PriceLevelChangedEvent, SerializationError>;

    /// Returns the MIME-like content type identifier for this format.
    ///
    /// Consumers can use this value to select the correct deserializer.
    /// Examples: `"application/json"`, `"application/x-bincode"`.
    #[must_use]
    fn content_type(&self) -> &'static str;
}

// ─── JSON ───────────────────────────────────────────────────────────────────

/// JSON event serializer using `serde_json`.
///
/// This is the default serializer, producing human-readable JSON payloads.
/// It is always available (no feature gate) since `serde_json` is a
/// required dependency.
///
/// # Content Type
///
/// `"application/json"`
#[derive(Debug, Clone, Copy, Default)]
pub struct JsonEventSerializer;

impl JsonEventSerializer {
    /// Create a new JSON event serializer.
    #[must_use]
    #[inline]
    pub fn new() -> Self {
        Self
    }
}

impl EventSerializer for JsonEventSerializer {
    fn serialize_trade(&self, trade: &TradeResult) -> Result<Vec<u8>, SerializationError> {
        serde_json::to_vec(trade).map_err(|e| SerializationError {
            message: e.to_string(),
        })
    }

    fn serialize_book_change(
        &self,
        event: &PriceLevelChangedEvent,
    ) -> Result<Vec<u8>, SerializationError> {
        serde_json::to_vec(event).map_err(|e| SerializationError {
            message: e.to_string(),
        })
    }

    fn deserialize_trade(&self, data: &[u8]) -> Result<TradeResult, SerializationError> {
        serde_json::from_slice(data).map_err(|e| SerializationError {
            message: e.to_string(),
        })
    }

    fn deserialize_book_change(
        &self,
        data: &[u8],
    ) -> Result<PriceLevelChangedEvent, SerializationError> {
        serde_json::from_slice(data).map_err(|e| SerializationError {
            message: e.to_string(),
        })
    }

    #[inline]
    fn content_type(&self) -> &'static str {
        "application/json"
    }
}

// ─── Bincode ────────────────────────────────────────────────────────────────

/// Bincode event serializer for compact binary payloads.
///
/// Produces significantly smaller payloads than JSON with much lower
/// serialization latency (typically < 500 ns per event). The trade-off
/// is that the output is not human-readable.
///
/// # Feature Gate
///
/// Requires the `bincode` feature:
///
/// ```toml
/// [dependencies]
/// orderbook-rs = { version = "0.6", features = ["bincode"] }
/// ```
///
/// # Content Type
///
/// `"application/x-bincode"`
#[cfg(feature = "bincode")]
#[derive(Debug, Clone, Copy, Default)]
pub struct BincodeEventSerializer;

#[cfg(feature = "bincode")]
impl BincodeEventSerializer {
    /// Create a new Bincode event serializer.
    #[must_use]
    #[inline]
    pub fn new() -> Self {
        Self
    }
}

#[cfg(feature = "bincode")]
impl EventSerializer for BincodeEventSerializer {
    fn serialize_trade(&self, trade: &TradeResult) -> Result<Vec<u8>, SerializationError> {
        bincode::serialize(trade).map_err(|e| SerializationError {
            message: e.to_string(),
        })
    }

    fn serialize_book_change(
        &self,
        event: &PriceLevelChangedEvent,
    ) -> Result<Vec<u8>, SerializationError> {
        bincode::serialize(event).map_err(|e| SerializationError {
            message: e.to_string(),
        })
    }

    fn deserialize_trade(&self, data: &[u8]) -> Result<TradeResult, SerializationError> {
        bincode::deserialize(data).map_err(|e| SerializationError {
            message: e.to_string(),
        })
    }

    fn deserialize_book_change(
        &self,
        data: &[u8],
    ) -> Result<PriceLevelChangedEvent, SerializationError> {
        bincode::deserialize(data).map_err(|e| SerializationError {
            message: e.to_string(),
        })
    }

    #[inline]
    fn content_type(&self) -> &'static str {
        "application/x-bincode"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pricelevel::{Id, MatchResult, Side};

    fn make_trade_result() -> TradeResult {
        let order_id = Id::new_uuid();
        let match_result = MatchResult::new(order_id, 100);
        TradeResult::new("BTC/USD".to_string(), match_result)
    }

    fn make_book_change() -> PriceLevelChangedEvent {
        PriceLevelChangedEvent {
            side: Side::Buy,
            price: 50_000_000,
            quantity: 1_000,
        }
    }

    // ─── JSON tests ─────────────────────────────────────────────────────

    #[test]
    fn test_json_serialize_trade() {
        let serializer = JsonEventSerializer::new();
        let trade = make_trade_result();
        let result = serializer.serialize_trade(&trade);
        assert!(result.is_ok());
        let bytes = result.unwrap_or_default();
        assert!(!bytes.is_empty());

        let json_str = String::from_utf8(bytes).unwrap_or_default();
        assert!(json_str.contains("BTC/USD"));
    }

    #[test]
    fn test_json_roundtrip_trade() {
        let serializer = JsonEventSerializer::new();
        let trade = make_trade_result();
        let bytes = serializer.serialize_trade(&trade);
        assert!(bytes.is_ok());
        let bytes = bytes.unwrap_or_default();

        let decoded = serializer.deserialize_trade(&bytes);
        assert!(decoded.is_ok());
        let decoded = decoded.unwrap_or_else(|_| make_trade_result());
        assert_eq!(decoded.symbol, trade.symbol);
        assert_eq!(decoded.total_maker_fees, trade.total_maker_fees);
        assert_eq!(decoded.total_taker_fees, trade.total_taker_fees);
    }

    #[test]
    fn test_json_serialize_book_change() {
        let serializer = JsonEventSerializer::new();
        let event = make_book_change();
        let result = serializer.serialize_book_change(&event);
        assert!(result.is_ok());
        let bytes = result.unwrap_or_default();
        assert!(!bytes.is_empty());
    }

    #[test]
    fn test_json_roundtrip_book_change() {
        let serializer = JsonEventSerializer::new();
        let event = make_book_change();
        let bytes = serializer.serialize_book_change(&event);
        assert!(bytes.is_ok());
        let bytes = bytes.unwrap_or_default();

        let decoded = serializer.deserialize_book_change(&bytes);
        assert!(decoded.is_ok());
        let decoded = decoded.unwrap_or_else(|_| make_book_change());
        assert_eq!(decoded, event);
    }

    #[test]
    fn test_json_content_type() {
        let serializer = JsonEventSerializer::new();
        assert_eq!(serializer.content_type(), "application/json");
    }

    #[test]
    fn test_json_deserialize_trade_error() {
        let serializer = JsonEventSerializer::new();
        let result = serializer.deserialize_trade(b"not valid json");
        assert!(result.is_err());
    }

    #[test]
    fn test_json_deserialize_book_change_error() {
        let serializer = JsonEventSerializer::new();
        let result = serializer.deserialize_book_change(b"not valid json");
        assert!(result.is_err());
    }

    #[test]
    fn test_serialization_error_display() {
        let err = SerializationError {
            message: "test error".to_string(),
        };
        let display = format!("{err}");
        assert!(display.contains("event serialization error"));
        assert!(display.contains("test error"));
    }

    // ─── Bincode tests ──────────────────────────────────────────────────

    #[cfg(feature = "bincode")]
    mod bincode_tests {
        use super::*;

        #[test]
        fn test_bincode_serialize_trade() {
            let serializer = BincodeEventSerializer::new();
            let trade = make_trade_result();
            let result = serializer.serialize_trade(&trade);
            assert!(result.is_ok());
            let bytes = result.unwrap_or_default();
            assert!(!bytes.is_empty());

            // Bincode should be more compact than JSON
            let json_serializer = JsonEventSerializer::new();
            let json_bytes = json_serializer.serialize_trade(&trade).unwrap_or_default();
            assert!(
                bytes.len() < json_bytes.len(),
                "bincode ({}) should be smaller than json ({})",
                bytes.len(),
                json_bytes.len()
            );
        }

        #[test]
        fn test_bincode_roundtrip_trade() {
            let serializer = BincodeEventSerializer::new();
            let trade = make_trade_result();
            let bytes = serializer.serialize_trade(&trade);
            assert!(bytes.is_ok());
            let bytes = bytes.unwrap_or_default();

            let decoded = serializer.deserialize_trade(&bytes);
            assert!(decoded.is_ok());
            let decoded = decoded.unwrap_or_else(|_| make_trade_result());
            assert_eq!(decoded.symbol, trade.symbol);
            assert_eq!(decoded.total_maker_fees, trade.total_maker_fees);
            assert_eq!(decoded.total_taker_fees, trade.total_taker_fees);
        }

        #[test]
        fn test_bincode_serialize_book_change() {
            let serializer = BincodeEventSerializer::new();
            let event = make_book_change();
            let result = serializer.serialize_book_change(&event);
            assert!(result.is_ok());
            let bytes = result.unwrap_or_default();
            assert!(!bytes.is_empty());
        }

        #[test]
        fn test_bincode_roundtrip_book_change() {
            let serializer = BincodeEventSerializer::new();
            let event = make_book_change();
            let bytes = serializer.serialize_book_change(&event);
            assert!(bytes.is_ok());
            let bytes = bytes.unwrap_or_default();

            let decoded = serializer.deserialize_book_change(&bytes);
            assert!(decoded.is_ok());
            let decoded = decoded.unwrap_or_else(|_| make_book_change());
            assert_eq!(decoded, event);
        }

        #[test]
        fn test_bincode_content_type() {
            let serializer = BincodeEventSerializer::new();
            assert_eq!(serializer.content_type(), "application/x-bincode");
        }

        #[test]
        fn test_bincode_deserialize_trade_error() {
            let serializer = BincodeEventSerializer::new();
            let result = serializer.deserialize_trade(b"\x00\x01");
            assert!(result.is_err());
        }

        #[test]
        fn test_bincode_deserialize_book_change_error() {
            let serializer = BincodeEventSerializer::new();
            let result = serializer.deserialize_book_change(b"\x00\x01");
            assert!(result.is_err());
        }

        #[test]
        fn test_bincode_smaller_than_json_book_change() {
            let event = make_book_change();
            let bincode_ser = BincodeEventSerializer::new();
            let json_ser = JsonEventSerializer::new();

            let bin_bytes = bincode_ser
                .serialize_book_change(&event)
                .unwrap_or_default();
            let json_bytes = json_ser.serialize_book_change(&event).unwrap_or_default();

            assert!(
                bin_bytes.len() < json_bytes.len(),
                "bincode ({}) should be smaller than json ({})",
                bin_bytes.len(),
                json_bytes.len()
            );
        }
    }
}
