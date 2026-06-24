//! NATS JetStream trade event publisher.
//!
//! This module provides [`NatsTradePublisher`], which converts trade events
//! from the order book's [`TradeListener`] callback into NATS JetStream
//! messages. Each trade is published to two subjects:
//!
//! - `{prefix}.{symbol}` — per-symbol stream
//! - `{prefix}.all` — aggregate stream
//!
//! The listener callback is non-blocking on the matching hot path: it clones
//! the [`TradeResult`] into a bounded channel and returns immediately — no
//! serialization, no `format!`, and no per-trade task spawn happen on the
//! engine thread. A single background Tokio task drains the channel, batches
//! and (optionally) throttles, and performs the serialization, subject
//! construction, and JetStream publish with exponential-backoff retry. This
//! mirrors the sibling [`NatsBookChangePublisher`](crate::orderbook::nats_book_change::NatsBookChangePublisher)
//! so neither outbound path floods the runtime with tiny per-event tasks under
//! a burst.
//!
//! # Feature Gate
//!
//! This module is only available when the `nats` feature is enabled:
//!
//! ```toml
//! [dependencies]
//! orderbook-rs = { version = "0.6", features = ["nats"] }
//! ```

use crate::orderbook::serialization::{EventSerializer, JsonEventSerializer};
use crate::orderbook::trade::{TradeListener, TradeResult};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::mpsc;
use tracing::{error, trace, warn};

/// Default batch window in milliseconds. Trades are drained from the channel
/// for at most this duration before the accumulated batch is published.
const DEFAULT_BATCH_WINDOW_MS: u64 = 1;

/// Default maximum number of trades drained per batch. When this limit is
/// reached the batch is flushed immediately, regardless of the time window.
const DEFAULT_MAX_BATCH_SIZE: usize = 100;

/// Default bounded-channel capacity. When the channel is full, new trades are
/// dropped and `dropped_events` is incremented.
const DEFAULT_CHANNEL_CAPACITY: usize = 10_000;

/// Default minimum interval in milliseconds between consecutive flushes. Set to
/// 0 to disable throttling.
const DEFAULT_MIN_PUBLISH_INTERVAL_MS: u64 = 0;

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
/// # Batching and throttling
///
/// The listener callback pushes each trade into a bounded channel and returns
/// immediately. A single background task drains the channel, accumulating
/// trades until either the
/// [`batch_window_ms`](NatsTradePublisher::with_batch_window_ms) elapses or
/// [`max_batch_size`](NatsTradePublisher::with_max_batch_size) trades have been
/// collected, then publishes them. An optional
/// [`min_publish_interval_ms`](NatsTradePublisher::with_min_publish_interval_ms)
/// throttles consecutive flushes on a high-activity book.
///
/// # Metrics
///
/// The publisher tracks the following counters via atomic operations:
///
/// - **publish_count** — number of successfully published trades (a trade
///   counts once when both its symbol and aggregate publishes succeed)
/// - **error_count** — number of permanently failed publish/serialize attempts
/// - **events_received** — total trades received from the listener callback
/// - **batches_published** — total drain/flush cycles performed
/// - **dropped_events** — trades dropped because the channel was full
/// - **sequence** — monotonically increasing sequence number; each publish
///   (symbol-specific and aggregate) receives its own unique value
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
/// let (handle, listener) = publisher.into_listener();
/// // Use `listener` as the OrderBook's trade_listener
/// // Use `handle` to read metrics: handle.publish_count(), handle.error_count()
/// # Ok(())
/// # }
/// ```
pub struct NatsTradePublisher {
    /// JetStream context for publishing messages.
    jetstream: async_nats::jetstream::Context,

    /// Subject prefix. Messages are published to `{prefix}.{symbol}` and
    /// `{prefix}.all`.
    subject_prefix: String,

    /// The `{prefix}.all` aggregate subject, precomputed once at construction
    /// so the publish path never rebuilds it.
    all_subject: String,

    /// Handle to the Tokio runtime used for spawning the background batch task.
    runtime: tokio::runtime::Handle,

    /// Batch window duration in milliseconds.
    batch_window_ms: u64,

    /// Maximum number of trades per batch before an early flush.
    max_batch_size: usize,

    /// Bounded channel capacity for the trade buffer.
    channel_capacity: usize,

    /// Minimum interval in milliseconds between consecutive flushes.
    min_publish_interval_ms: u64,

    /// Maximum number of retry attempts for transient failures.
    max_retries: u32,

    /// Monotonically increasing sequence number embedded in each published
    /// message as a NATS header. Written exclusively by the single background
    /// `publish_task`; the `Relaxed` ordering on its `fetch_add` is correct
    /// only because no other writer exists (the `into_listener(self)` consuming
    /// signature spawns exactly one task per publisher).
    sequence: AtomicU64,

    /// Count of successfully published trades (one per trade when both subjects
    /// succeed).
    publish_count: AtomicU64,

    /// Count of permanently failed publish/serialize attempts.
    error_count: AtomicU64,

    /// Total trades received from the listener callback.
    events_received: AtomicU64,

    /// Total drain/flush cycles performed.
    batches_published: AtomicU64,

    /// Trades dropped because the bounded channel was full.
    dropped_events: AtomicU64,

    /// Pluggable event serializer. Defaults to [`JsonEventSerializer`] for
    /// backward compatibility. Can be overridden via
    /// [`with_serializer`](NatsTradePublisher::with_serializer).
    serializer: Arc<dyn EventSerializer>,
}

impl NatsTradePublisher {
    /// Create a new NATS trade publisher.
    ///
    /// # Arguments
    ///
    /// * `jetstream` — JetStream context obtained from an `async_nats` client
    /// * `subject_prefix` — prefix for NATS subjects (e.g. `"trades"`)
    /// * `runtime` — handle to the Tokio runtime for spawning the batch task
    #[inline]
    pub fn new(
        jetstream: async_nats::jetstream::Context,
        subject_prefix: String,
        runtime: tokio::runtime::Handle,
    ) -> Self {
        let all_subject = format!("{subject_prefix}.all");
        Self {
            jetstream,
            subject_prefix,
            all_subject,
            runtime,
            batch_window_ms: DEFAULT_BATCH_WINDOW_MS,
            max_batch_size: DEFAULT_MAX_BATCH_SIZE,
            channel_capacity: DEFAULT_CHANNEL_CAPACITY,
            min_publish_interval_ms: DEFAULT_MIN_PUBLISH_INTERVAL_MS,
            max_retries: DEFAULT_MAX_RETRIES,
            sequence: AtomicU64::new(0),
            publish_count: AtomicU64::new(0),
            error_count: AtomicU64::new(0),
            events_received: AtomicU64::new(0),
            batches_published: AtomicU64::new(0),
            dropped_events: AtomicU64::new(0),
            serializer: Arc::new(JsonEventSerializer),
        }
    }

    /// Set the batch window duration in milliseconds.
    ///
    /// Trades are accumulated for at most this duration before being flushed.
    /// Defaults to [`DEFAULT_BATCH_WINDOW_MS`] (1 ms).
    #[must_use = "builders do nothing unless consumed"]
    #[inline]
    pub fn with_batch_window_ms(mut self, batch_window_ms: u64) -> Self {
        self.batch_window_ms = batch_window_ms;
        self
    }

    /// Set the maximum number of trades per batch.
    ///
    /// When the batch reaches this size it is flushed immediately, regardless
    /// of the time window. Defaults to [`DEFAULT_MAX_BATCH_SIZE`] (100).
    #[must_use = "builders do nothing unless consumed"]
    #[inline]
    pub fn with_max_batch_size(mut self, max_batch_size: usize) -> Self {
        self.max_batch_size = max_batch_size;
        self
    }

    /// Set the bounded channel capacity.
    ///
    /// When the channel is full, new trades are dropped and `dropped_events`
    /// is incremented. Defaults to [`DEFAULT_CHANNEL_CAPACITY`] (10,000).
    ///
    /// # Panics
    ///
    /// Panics if `channel_capacity` is zero (Tokio mpsc requires a positive
    /// capacity).
    #[must_use = "builders do nothing unless consumed"]
    #[inline]
    pub fn with_channel_capacity(mut self, channel_capacity: usize) -> Self {
        assert!(
            channel_capacity > 0,
            "channel_capacity must be greater than zero"
        );
        self.channel_capacity = channel_capacity;
        self
    }

    /// Set the minimum interval in milliseconds between consecutive flushes.
    ///
    /// When set to a value greater than 0, the background task waits at least
    /// this long between consecutive flushes. Defaults to
    /// [`DEFAULT_MIN_PUBLISH_INTERVAL_MS`] (0, disabled).
    #[must_use = "builders do nothing unless consumed"]
    #[inline]
    pub fn with_min_publish_interval_ms(mut self, min_publish_interval_ms: u64) -> Self {
        self.min_publish_interval_ms = min_publish_interval_ms;
        self
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

    /// Set a custom event serializer.
    ///
    /// Defaults to [`JsonEventSerializer`]. Use this to switch to a more
    /// compact binary format (e.g. `BincodeEventSerializer`) for lower
    /// latency publishing.
    ///
    /// # Arguments
    ///
    /// * `serializer` — the serializer implementation to use
    #[must_use = "builders do nothing unless consumed"]
    #[inline]
    pub fn with_serializer(mut self, serializer: Arc<dyn EventSerializer>) -> Self {
        self.serializer = serializer;
        self
    }

    /// Returns the number of successfully published trades.
    #[must_use]
    #[inline]
    pub fn publish_count(&self) -> u64 {
        self.publish_count.load(Ordering::Relaxed)
    }

    /// Returns the number of permanently failed publish attempts.
    #[must_use]
    #[inline]
    pub fn error_count(&self) -> u64 {
        self.error_count.load(Ordering::Relaxed)
    }

    /// Returns the total number of trades received from the listener callback.
    #[must_use]
    #[inline]
    pub fn events_received(&self) -> u64 {
        self.events_received.load(Ordering::Relaxed)
    }

    /// Returns the total number of drain/flush cycles performed.
    #[must_use]
    #[inline]
    pub fn batches_published(&self) -> u64 {
        self.batches_published.load(Ordering::Relaxed)
    }

    /// Returns the number of trades dropped because the channel was full.
    #[must_use]
    #[inline]
    pub fn dropped_events(&self) -> u64 {
        self.dropped_events.load(Ordering::Relaxed)
    }

    /// Returns the current sequence number (next value to be assigned).
    #[must_use]
    #[inline]
    pub fn sequence(&self) -> u64 {
        self.sequence.load(Ordering::Relaxed)
    }

    /// Returns a reference to the configured event serializer.
    #[must_use]
    #[inline]
    pub fn serializer(&self) -> &dyn EventSerializer {
        self.serializer.as_ref()
    }

    /// Convert this publisher into a [`TradeListener`] callback.
    ///
    /// This method consumes `self`, wraps it in an `Arc`, spawns a single
    /// background batch task on the configured Tokio runtime, and returns both
    /// the `Arc` handle (for reading metrics) and the listener callback.
    ///
    /// The returned listener clones each [`TradeResult`] into a bounded channel
    /// and returns immediately — no serialization, no `format!`, and no task
    /// spawn happen on the matching hot path. The background task drains the
    /// channel, batches the trades, and publishes each to both
    /// `{prefix}.{symbol}` and the precomputed `{prefix}.all` subject with a
    /// unique sequence number per publish.
    ///
    /// # Returns
    ///
    /// A tuple of `(Arc<NatsTradePublisher>, TradeListener)`. The `Arc` handle
    /// allows the caller to read metrics (`publish_count`, `error_count`,
    /// `events_received`, `dropped_events`, `sequence`) after wiring the
    /// listener into the order book.
    pub fn into_listener(self) -> (Arc<Self>, TradeListener) {
        let channel_capacity = self.channel_capacity;
        let publisher = Arc::new(self);
        let handle = Arc::clone(&publisher);

        let (tx, rx) = mpsc::channel::<TradeResult>(channel_capacity);

        // Spawn the single background batch task.
        let task_publisher = Arc::clone(&publisher);
        publisher
            .runtime
            .spawn(Self::publish_task(task_publisher, rx));

        // Build the hot-path listener closure: clone + non-blocking send only.
        let listener_publisher = Arc::clone(&publisher);
        let listener = Arc::new(move |trade_result: &TradeResult| {
            listener_publisher
                .events_received
                .fetch_add(1, Ordering::Relaxed);
            if tx.try_send(trade_result.clone()).is_err() {
                listener_publisher
                    .dropped_events
                    .fetch_add(1, Ordering::Relaxed);
                warn!("trade channel full, event dropped");
            }
        });

        (handle, listener)
    }

    /// Background task that drains the trade channel, batches trades, and
    /// publishes them to NATS.
    ///
    /// The task flushes when either:
    /// - The batch window timer elapses (configurable via `batch_window_ms`)
    /// - The batch reaches `max_batch_size` trades
    ///
    /// When throttling is enabled (`min_publish_interval_ms > 0`), the task
    /// waits at least that duration between consecutive flushes.
    async fn publish_task(publisher: Arc<Self>, mut rx: mpsc::Receiver<TradeResult>) {
        let batch_window = std::time::Duration::from_millis(publisher.batch_window_ms);
        let min_interval = if publisher.min_publish_interval_ms > 0 {
            Some(std::time::Duration::from_millis(
                publisher.min_publish_interval_ms,
            ))
        } else {
            None
        };

        let mut batch: Vec<TradeResult> = Vec::with_capacity(publisher.max_batch_size);
        let mut last_publish = tokio::time::Instant::now();

        loop {
            // Wait for the first trade or channel close.
            if batch.is_empty() {
                match rx.recv().await {
                    Some(trade) => batch.push(trade),
                    None => break, // Channel closed
                }
            }

            // Collect more trades within the batch window.
            let deadline = tokio::time::Instant::now() + batch_window;
            while batch.len() < publisher.max_batch_size {
                match tokio::time::timeout_at(deadline, rx.recv()).await {
                    Ok(Some(trade)) => batch.push(trade),
                    Ok(None) => {
                        // Channel closed — flush remaining and exit.
                        Self::flush_batch(&publisher, &mut batch, &mut last_publish, min_interval)
                            .await;
                        return;
                    }
                    Err(_) => break, // Timeout — flush batch
                }
            }

            Self::flush_batch(&publisher, &mut batch, &mut last_publish, min_interval).await;
        }

        // Flush any remaining trades.
        Self::flush_batch(&publisher, &mut batch, &mut last_publish, min_interval).await;
    }

    /// Flush the accumulated batch: serialize and publish each trade to its
    /// per-symbol and aggregate subjects, then apply throttling.
    ///
    /// Serialization, subject construction, and the JetStream publish all
    /// happen here in the background task — never on the matching hot path.
    async fn flush_batch(
        publisher: &Arc<Self>,
        batch: &mut Vec<TradeResult>,
        last_publish: &mut tokio::time::Instant,
        min_interval: Option<std::time::Duration>,
    ) {
        if batch.is_empty() {
            return;
        }

        let trades = std::mem::take(batch);
        for trade in trades {
            let payload = match publisher.serializer.serialize_trade(&trade) {
                Ok(bytes) => bytes,
                Err(e) => {
                    publisher.error_count.fetch_add(1, Ordering::Relaxed);
                    error!(error = %e, "failed to serialize trade result for NATS");
                    continue;
                }
            };

            let symbol_seq = publisher.sequence.fetch_add(1, Ordering::Relaxed);
            let all_seq = publisher.sequence.fetch_add(1, Ordering::Relaxed);
            let symbol_subject = format!("{}.{}", publisher.subject_prefix, trade.symbol);
            let all_subject = publisher.all_subject.clone();
            let payload_bytes: bytes::Bytes = payload.into();

            Self::publish_with_retry(
                Arc::clone(publisher),
                symbol_subject,
                all_subject,
                payload_bytes,
                symbol_seq,
                all_seq,
            )
            .await;
        }

        publisher.batches_published.fetch_add(1, Ordering::Relaxed);

        // Throttle: wait if needed before allowing the next flush.
        if let Some(interval) = min_interval {
            let elapsed = last_publish.elapsed();
            if elapsed < interval {
                tokio::time::sleep(interval - elapsed).await;
            }
        }

        *last_publish = tokio::time::Instant::now();
    }

    /// Publish a trade event to both the symbol-specific and aggregate subjects
    /// with retry logic for transient failures.
    ///
    /// Each subject receives its own unique sequence number in the
    /// `Nats-Sequence` header so consumers can deduplicate per-stream without
    /// collisions between the symbol and aggregate streams.
    async fn publish_with_retry(
        publisher: Arc<Self>,
        symbol_subject: String,
        all_subject: String,
        payload: bytes::Bytes,
        symbol_seq: u64,
        all_seq: u64,
    ) {
        let content_type = publisher.serializer.content_type();

        let mut symbol_headers = async_nats::HeaderMap::new();
        symbol_headers.insert("Nats-Sequence", symbol_seq.to_string().as_str());
        symbol_headers.insert("Content-Type", content_type);

        let mut all_headers = async_nats::HeaderMap::new();
        all_headers.insert("Nats-Sequence", all_seq.to_string().as_str());
        all_headers.insert("Content-Type", content_type);

        // Publish to symbol-specific subject
        let symbol_ok =
            Self::publish_single(&publisher, &symbol_subject, payload.clone(), symbol_headers)
                .await;

        // Publish to aggregate subject
        let all_ok = Self::publish_single(&publisher, &all_subject, payload, all_headers).await;

        if symbol_ok && all_ok {
            publisher.publish_count.fetch_add(1, Ordering::Relaxed);
            trace!(symbol_seq, all_seq, symbol = %symbol_subject, "trade event published to NATS");
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

            // Exponential backoff: 10ms, 20ms, 40ms, ... clamped to avoid
            // panic from over-shifting when max_retries is large.
            if attempt + 1 < max_attempts {
                let shift = u32::min(attempt, 63);
                let delay_ms =
                    BASE_RETRY_DELAY_MS.saturating_mul(1u64.checked_shl(shift).unwrap_or(u64::MAX));
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
            .field("batch_window_ms", &self.batch_window_ms)
            .field("max_batch_size", &self.max_batch_size)
            .field("channel_capacity", &self.channel_capacity)
            .field("min_publish_interval_ms", &self.min_publish_interval_ms)
            .field("max_retries", &self.max_retries)
            .field("sequence", &self.sequence.load(Ordering::Relaxed))
            .field("publish_count", &self.publish_count.load(Ordering::Relaxed))
            .field("error_count", &self.error_count.load(Ordering::Relaxed))
            .field(
                "events_received",
                &self.events_received.load(Ordering::Relaxed),
            )
            .field(
                "batches_published",
                &self.batches_published.load(Ordering::Relaxed),
            )
            .field(
                "dropped_events",
                &self.dropped_events.load(Ordering::Relaxed),
            )
            .field("serializer", &self.serializer.content_type())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pricelevel::{Id, MatchResult, Quantity};

    fn make_trade_result(symbol: &str) -> TradeResult {
        let order_id = Id::new_uuid();
        let match_result = MatchResult::new(order_id, Quantity::new(100));
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
    fn test_precomputed_all_subject_matches_format() {
        // The aggregate subject is precomputed once at construction; it must
        // equal what the per-publish path would otherwise format.
        let prefix = "trades";
        let precomputed = format!("{prefix}.all");
        assert_eq!(precomputed, "trades.all");
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
    fn test_default_batch_constants() {
        assert_eq!(DEFAULT_BATCH_WINDOW_MS, 1);
        assert_eq!(DEFAULT_MAX_BATCH_SIZE, 100);
        assert_eq!(DEFAULT_CHANNEL_CAPACITY, 10_000);
        assert_eq!(DEFAULT_MIN_PUBLISH_INTERVAL_MS, 0);
    }

    #[test]
    fn test_exponential_backoff_calculation() {
        // Verify the backoff sequence: 10, 20, 40, 80, ...
        for attempt in 0u32..4 {
            let shift = u32::min(attempt, 63);
            let delay =
                BASE_RETRY_DELAY_MS.saturating_mul(1u64.checked_shl(shift).unwrap_or(u64::MAX));
            let expected = BASE_RETRY_DELAY_MS * 2u64.pow(attempt);
            assert_eq!(delay, expected);
        }
    }

    #[test]
    fn test_exponential_backoff_high_retry_count_does_not_panic() {
        // With max_retries >= 64, the shift must not panic.
        for attempt in [63u32, 64, 100, u32::MAX] {
            let shift = u32::min(attempt, 63);
            let delay =
                BASE_RETRY_DELAY_MS.saturating_mul(1u64.checked_shl(shift).unwrap_or(u64::MAX));
            // All values saturate rather than panic
            assert!(delay >= BASE_RETRY_DELAY_MS);
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
