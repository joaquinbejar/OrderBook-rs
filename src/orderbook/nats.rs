//! NATS JetStream trade event publisher.
//!
//! This module provides [`NatsTradePublisher`], which converts trade events
//! from the order book's [`TradeListener`] callback into NATS JetStream
//! messages. Each trade is published to two subjects:
//!
//! - `{prefix}.{symbol}` — per-symbol stream
//! - `{prefix}.all` — aggregate stream
//!
//! The publisher is non-blocking on the hot path: serialization and sequence
//! numbering happen synchronously, while the actual NATS publish is spawned
//! onto a Tokio runtime. Transient failures are retried with exponential
//! backoff.
//!
//! # Feature Gate
//!
//! This module is only available when the `nats` feature is enabled:
//!
//! ```toml
//! [dependencies]
//! orderbook-rs = { version = "0.6", features = ["nats"] }
//! ```

use crate::orderbook::trade::{TradeListener, TradeResult};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tracing::{error, trace, warn};

/// Default maximum number of retry attempts for transient NATS publish failures.
const DEFAULT_MAX_RETRIES: u32 = 3;

/// Base delay in milliseconds for exponential backoff between retries.
const BASE_RETRY_DELAY_MS: u64 = 10;

/// A trade event publisher that sends [`TradeResult`] events to NATS JetStream.
///
/// The publisher wraps a JetStream context and provides a non-blocking
/// [`into_listener`](NatsTradePublisher::into_listener) method that returns a
/// [`TradeListener`] suitable for use with [`OrderBook::trade_listener`].
///
/// # Metrics
///
/// The publisher tracks the following counters via atomic operations:
///
/// - **publish_count** — number of successfully published messages
/// - **error_count** — number of permanently failed publish attempts
/// - **sequence** — monotonically increasing sequence number included in each message
///
/// # Example
///
/// ```rust,no_run
/// use orderbook_rs::orderbook::nats::NatsTradePublisher;
///
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let client = async_nats::connect("nats://localhost:4222").await?;
/// let jetstream = async_nats::jetstream::new(client);
/// let handle = tokio::runtime::Handle::current();
///
/// let publisher = NatsTradePublisher::new(jetstream, "trades".to_string(), handle);
/// let listener = publisher.into_listener();
/// // Use `listener` as the OrderBook's trade_listener
/// # Ok(())
/// # }
/// ```
pub struct NatsTradePublisher {
    /// JetStream context for publishing messages.
    jetstream: async_nats::jetstream::Context,

    /// Subject prefix. Messages are published to `{prefix}.{symbol}` and
    /// `{prefix}.all`.
    subject_prefix: String,

    /// Handle to the Tokio runtime used for spawning async publish tasks.
    runtime: tokio::runtime::Handle,

    /// Monotonically increasing sequence number embedded in each published
    /// message as a NATS header.
    sequence: AtomicU64,

    /// Count of successfully published messages (across both subjects).
    publish_count: AtomicU64,

    /// Count of permanently failed publish attempts (after all retries
    /// exhausted).
    error_count: AtomicU64,

    /// Maximum number of retry attempts for transient failures.
    max_retries: u32,
}

impl NatsTradePublisher {
    /// Create a new NATS trade publisher.
    ///
    /// # Arguments
    ///
    /// * `jetstream` — JetStream context obtained from an `async_nats` client
    /// * `subject_prefix` — prefix for NATS subjects (e.g. `"trades"`)
    /// * `runtime` — handle to the Tokio runtime for spawning publish tasks
    #[inline]
    pub fn new(
        jetstream: async_nats::jetstream::Context,
        subject_prefix: String,
        runtime: tokio::runtime::Handle,
    ) -> Self {
        Self {
            jetstream,
            subject_prefix,
            runtime,
            sequence: AtomicU64::new(0),
            publish_count: AtomicU64::new(0),
            error_count: AtomicU64::new(0),
            max_retries: DEFAULT_MAX_RETRIES,
        }
    }

    /// Set the maximum number of retry attempts for transient NATS failures.
    ///
    /// Defaults to [`DEFAULT_MAX_RETRIES`] (3). Set to 0 to disable retries.
    #[must_use = "builders do nothing unless consumed"]
    #[inline]
    pub fn with_max_retries(mut self, max_retries: u32) -> Self {
        self.max_retries = max_retries;
        self
    }

    /// Returns the number of successfully published messages.
    #[must_use]
    #[inline]
    pub fn publish_count(self: &Arc<Self>) -> u64 {
        self.publish_count.load(Ordering::Relaxed)
    }

    /// Returns the number of permanently failed publish attempts.
    #[must_use]
    #[inline]
    pub fn error_count(self: &Arc<Self>) -> u64 {
        self.error_count.load(Ordering::Relaxed)
    }

    /// Returns the current sequence number (next value to be assigned).
    #[must_use]
    #[inline]
    pub fn sequence(self: &Arc<Self>) -> u64 {
        self.sequence.load(Ordering::Relaxed)
    }

    /// Convert this publisher into a [`TradeListener`] callback.
    ///
    /// The returned listener serializes each [`TradeResult`] to JSON, assigns
    /// a sequence number, and spawns an async task that publishes to both
    /// `{prefix}.{symbol}` and `{prefix}.all` subjects on the configured
    /// JetStream context.
    ///
    /// Publishing is non-blocking: the listener returns immediately after
    /// spawning the async task, keeping the matching engine hot path fast.
    ///
    /// # Returns
    ///
    /// An `Arc<dyn Fn(&TradeResult) + Send + Sync>` suitable for use as
    /// [`OrderBook::trade_listener`].
    pub fn into_listener(self) -> TradeListener {
        let publisher = Arc::new(self);
        Arc::new(move |trade_result: &TradeResult| {
            let payload = match serde_json::to_vec(trade_result) {
                Ok(bytes) => bytes,
                Err(e) => {
                    publisher.error_count.fetch_add(1, Ordering::Relaxed);
                    error!(error = %e, "failed to serialize trade result for NATS");
                    return;
                }
            };

            let seq = publisher.sequence.fetch_add(1, Ordering::Relaxed);
            let symbol_subject = format!("{}.{}", publisher.subject_prefix, trade_result.symbol);
            let all_subject = format!("{}.all", publisher.subject_prefix);

            let pub_clone = Arc::clone(&publisher);
            let payload_bytes: bytes::Bytes = payload.into();

            pub_clone.runtime.spawn(Self::publish_with_retry(
                Arc::clone(&pub_clone),
                symbol_subject,
                all_subject,
                payload_bytes,
                seq,
            ));
        })
    }

    /// Publish a trade event to both the symbol-specific and aggregate subjects
    /// with retry logic for transient failures.
    async fn publish_with_retry(
        publisher: Arc<Self>,
        symbol_subject: String,
        all_subject: String,
        payload: bytes::Bytes,
        seq: u64,
    ) {
        let mut headers = async_nats::HeaderMap::new();
        headers.insert("Nats-Sequence", seq.to_string().as_str());

        // Publish to symbol-specific subject
        let symbol_ok = Self::publish_single(
            &publisher,
            &symbol_subject,
            payload.clone(),
            headers.clone(),
        )
        .await;

        // Publish to aggregate subject
        let all_ok = Self::publish_single(&publisher, &all_subject, payload, headers).await;

        if symbol_ok && all_ok {
            publisher.publish_count.fetch_add(1, Ordering::Relaxed);
            trace!(seq, symbol = %symbol_subject, "trade event published to NATS");
        }
    }

    /// Publish a single message to a subject with exponential backoff retry.
    ///
    /// Returns `true` if the publish succeeded, `false` if all retries were
    /// exhausted.
    async fn publish_single(
        publisher: &Arc<Self>,
        subject: &str,
        payload: bytes::Bytes,
        headers: async_nats::HeaderMap,
    ) -> bool {
        let max_attempts = publisher.max_retries.saturating_add(1);

        for attempt in 0..max_attempts {
            let publish_result = publisher
                .jetstream
                .publish_with_headers(subject.to_string(), headers.clone(), payload.clone())
                .await;

            match publish_result {
                Ok(ack_future) => {
                    // Wait for the server acknowledgement
                    match ack_future.await {
                        Ok(_) => return true,
                        Err(e) => {
                            warn!(
                                attempt = attempt + 1,
                                max = max_attempts,
                                subject,
                                error = %e,
                                "NATS ack failed, retrying"
                            );
                        }
                    }
                }
                Err(e) => {
                    warn!(
                        attempt = attempt + 1,
                        max = max_attempts,
                        subject,
                        error = %e,
                        "NATS publish failed, retrying"
                    );
                }
            }

            // Exponential backoff: 10ms, 20ms, 40ms, ...
            if attempt + 1 < max_attempts {
                let delay_ms = BASE_RETRY_DELAY_MS.saturating_mul(1u64 << attempt);
                tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            }
        }

        publisher.error_count.fetch_add(1, Ordering::Relaxed);
        error!(subject, "NATS publish failed after all retries");
        false
    }
}

impl std::fmt::Debug for NatsTradePublisher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NatsTradePublisher")
            .field("subject_prefix", &self.subject_prefix)
            .field("sequence", &self.sequence.load(Ordering::Relaxed))
            .field("publish_count", &self.publish_count.load(Ordering::Relaxed))
            .field("error_count", &self.error_count.load(Ordering::Relaxed))
            .field("max_retries", &self.max_retries)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pricelevel::{MatchResult, OrderId};

    fn make_trade_result(symbol: &str) -> TradeResult {
        let order_id = OrderId::new_uuid();
        let match_result = MatchResult::new(order_id, 100);
        TradeResult::new(symbol.to_string(), match_result)
    }

    #[test]
    fn test_trade_result_serializes_to_json() {
        let tr = make_trade_result("BTC/USD");
        let result = serde_json::to_vec(&tr);
        assert!(result.is_ok());
        let bytes = result.unwrap_or_default();
        assert!(!bytes.is_empty());

        // Verify it contains expected fields
        let json_str = String::from_utf8(bytes).unwrap_or_default();
        assert!(json_str.contains("BTC/USD"));
        assert!(json_str.contains("match_result"));
    }

    #[test]
    fn test_trade_result_serialize_roundtrip_fields() {
        let tr = make_trade_result("ETH/USDT");
        let json = serde_json::to_value(&tr);
        assert!(json.is_ok());
        let value = json.unwrap_or(serde_json::Value::Null);
        assert_eq!(
            value.get("symbol").and_then(|v| v.as_str()),
            Some("ETH/USDT")
        );
        assert_eq!(
            value.get("total_maker_fees").and_then(|v| v.as_i64()),
            Some(0)
        );
        assert_eq!(
            value.get("total_taker_fees").and_then(|v| v.as_i64()),
            Some(0)
        );
    }

    #[test]
    fn test_subject_formatting() {
        let prefix = "trades";
        let symbol = "BTC/USD";
        let symbol_subject = format!("{prefix}.{symbol}");
        let all_subject = format!("{prefix}.all");

        assert_eq!(symbol_subject, "trades.BTC/USD");
        assert_eq!(all_subject, "trades.all");
    }

    #[test]
    fn test_subject_formatting_with_custom_prefix() {
        let prefix = "orderbook.events.trades";
        let symbol = "ETH-PERP";
        let symbol_subject = format!("{prefix}.{symbol}");
        let all_subject = format!("{prefix}.all");

        assert_eq!(symbol_subject, "orderbook.events.trades.ETH-PERP");
        assert_eq!(all_subject, "orderbook.events.trades.all");
    }

    #[test]
    fn test_default_max_retries() {
        assert_eq!(DEFAULT_MAX_RETRIES, 3);
    }

    #[test]
    fn test_base_retry_delay() {
        assert_eq!(BASE_RETRY_DELAY_MS, 10);
    }

    #[test]
    fn test_exponential_backoff_calculation() {
        // Verify the backoff sequence: 10, 20, 40, 80, ...
        for attempt in 0u32..4 {
            let delay = BASE_RETRY_DELAY_MS.saturating_mul(1u64 << attempt);
            let expected = BASE_RETRY_DELAY_MS * 2u64.pow(attempt);
            assert_eq!(delay, expected);
        }
    }

    #[test]
    fn test_nats_publish_error_display() {
        let err = crate::orderbook::OrderBookError::NatsPublishError {
            message: "connection refused".to_string(),
        };
        let display = format!("{err}");
        assert!(display.contains("nats publish error"));
        assert!(display.contains("connection refused"));
    }

    #[test]
    fn test_nats_serialization_error_display() {
        let err = crate::orderbook::OrderBookError::NatsSerializationError {
            message: "invalid utf-8".to_string(),
        };
        let display = format!("{err}");
        assert!(display.contains("nats serialization error"));
        assert!(display.contains("invalid utf-8"));
    }
}
