//! NATS JetStream order book change publisher.
//!
//! This module provides [`NatsBookChangePublisher`], which converts
//! [`PriceLevelChangedEvent`]s from the order book into batched NATS JetStream
//! messages. Events are collected via a bounded channel and flushed either when
//! the batch window elapses or the batch reaches its maximum size.
//!
//! Published subjects:
//!
//! - `{prefix}.{symbol}.changes` — all changes (mixed sides)
//! - `{prefix}.{symbol}.bid` — bid-side changes only
//! - `{prefix}.{symbol}.ask` — ask-side changes only
//!
//! The listener callback is non-blocking: it sends each event into a bounded
//! channel and returns immediately. A background Tokio task drains the channel,
//! batches events, and publishes to NATS with exponential-backoff retry.
//!
//! # Feature Gate
//!
//! This module is only available when the `nats` feature is enabled:
//!
//! ```toml
//! [dependencies]
//! orderbook-rs = { version = "0.6", features = ["nats"] }
//! ```

use crate::orderbook::book_change_event::{PriceLevelChangedEvent, PriceLevelChangedListener};
use pricelevel::Side;
use serde::Serialize;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::mpsc;
use tracing::{error, trace, warn};

/// Default batch window in milliseconds. Events are accumulated for at most
/// this duration before being flushed to NATS.
const DEFAULT_BATCH_WINDOW_MS: u64 = 1;

/// Default maximum number of events per batch. When this limit is reached the
/// batch is flushed immediately, regardless of the time window.
const DEFAULT_MAX_BATCH_SIZE: usize = 100;

/// Default bounded-channel capacity. When the channel is full, new events are
/// dropped and `dropped_events` is incremented.
const DEFAULT_CHANNEL_CAPACITY: usize = 10_000;

/// Default maximum number of retry attempts for transient NATS publish failures.
const DEFAULT_MAX_RETRIES: u32 = 3;

/// Base delay in milliseconds for exponential backoff between retries.
const BASE_RETRY_DELAY_MS: u64 = 10;

/// Default minimum interval in milliseconds between consecutive publish
/// operations. Set to 0 to disable throttling.
const DEFAULT_MIN_PUBLISH_INTERVAL_MS: u64 = 0;

/// A batched order book change payload published to NATS JetStream.
///
/// Each batch contains one or more [`BookChangeEntry`] values collected within
/// the configured batch window. Consumers use the `sequence` field for ordering
/// and gap detection.
#[derive(Debug, Clone, Serialize)]
pub struct BookChangeBatch {
    /// The symbol this batch belongs to.
    pub symbol: String,

    /// Monotonically increasing sequence number for this batch.
    pub sequence: u64,

    /// Unix timestamp in milliseconds when the batch was flushed.
    pub timestamp_ms: u64,

    /// Number of individual change events in this batch.
    pub event_count: usize,

    /// The individual price level changes.
    pub changes: Vec<BookChangeEntry>,
}

/// A single price level change within a [`BookChangeBatch`].
#[derive(Debug, Clone, Serialize)]
pub struct BookChangeEntry {
    /// The order book side that changed.
    pub side: Side,

    /// The price level that changed.
    pub price: u128,

    /// The new visible quantity at this price level after the change.
    pub quantity: u64,
}

impl From<PriceLevelChangedEvent> for BookChangeEntry {
    #[inline]
    fn from(event: PriceLevelChangedEvent) -> Self {
        Self {
            side: event.side,
            price: event.price,
            quantity: event.quantity,
        }
    }
}

/// A publisher that batches [`PriceLevelChangedEvent`]s and publishes them to
/// NATS JetStream.
///
/// The publisher wraps a JetStream context and provides a non-blocking
/// [`into_listener`](NatsBookChangePublisher::into_listener) method that returns
/// a [`PriceLevelChangedListener`] suitable for use with
/// [`OrderBook::price_level_changed_listener`].
///
/// # Batching
///
/// Events are collected in a bounded channel and flushed by a background task
/// when either the [`batch_window_ms`](NatsBookChangePublisher::with_batch_window_ms)
/// elapses or [`max_batch_size`](NatsBookChangePublisher::with_max_batch_size)
/// events have been collected.
///
/// # Throttling
///
/// An optional minimum publish interval prevents flooding on high-activity
/// books. When set, the publisher enforces at least
/// [`min_publish_interval_ms`](NatsBookChangePublisher::with_min_publish_interval_ms)
/// between consecutive NATS publishes.
///
/// # Metrics
///
/// The publisher tracks the following counters via atomic operations:
///
/// - **publish_count** — number of successfully published batches
/// - **error_count** — number of permanently failed publish attempts
/// - **events_received** — total events received from the listener callback
/// - **batches_published** — total batches flushed to NATS
/// - **dropped_events** — events dropped because the channel was full
/// - **sequence** — monotonically increasing batch sequence number
///
/// # Example
///
/// ```rust,no_run
/// use orderbook_rs::orderbook::nats_book_change::NatsBookChangePublisher;
///
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let client = async_nats::connect("nats://localhost:4222").await?;
/// let jetstream = async_nats::jetstream::new(client);
/// let handle = tokio::runtime::Handle::current();
///
/// let publisher = NatsBookChangePublisher::new(
///     jetstream,
///     "BTC/USD".to_string(),
///     "book".to_string(),
///     handle,
/// );
/// let (metrics, listener) = publisher.into_listener();
/// // Wire `listener` into OrderBook::set_price_level_listener()
/// // Read metrics via `metrics.publish_count()`, `metrics.dropped_events()`, etc.
/// # Ok(())
/// # }
/// ```
pub struct NatsBookChangePublisher {
    /// JetStream context for publishing messages.
    jetstream: async_nats::jetstream::Context,

    /// The order book symbol (e.g. `"BTC/USD"`).
    symbol: String,

    /// Subject prefix. Batches are published to `{prefix}.{symbol}.changes`,
    /// `{prefix}.{symbol}.bid`, and `{prefix}.{symbol}.ask`.
    subject_prefix: String,

    /// Handle to the Tokio runtime for spawning the background batch task.
    runtime: tokio::runtime::Handle,

    /// Batch window duration in milliseconds.
    batch_window_ms: u64,

    /// Maximum number of events per batch before an early flush.
    max_batch_size: usize,

    /// Bounded channel capacity for the event buffer.
    channel_capacity: usize,

    /// Minimum interval in milliseconds between consecutive publishes.
    min_publish_interval_ms: u64,

    /// Maximum retry attempts for transient NATS failures.
    max_retries: u32,

    /// Monotonically increasing batch sequence number.
    sequence: AtomicU64,

    /// Count of successfully published batches.
    publish_count: AtomicU64,

    /// Count of permanently failed publish attempts (after all retries).
    error_count: AtomicU64,

    /// Total events received from the listener callback.
    events_received: AtomicU64,

    /// Total batches successfully flushed to NATS.
    batches_published: AtomicU64,

    /// Events dropped because the bounded channel was full.
    dropped_events: AtomicU64,
}

impl NatsBookChangePublisher {
    /// Create a new NATS book change publisher.
    ///
    /// # Arguments
    ///
    /// * `jetstream` — JetStream context obtained from an `async_nats` client
    /// * `symbol` — the order book symbol (e.g. `"BTC/USD"`)
    /// * `subject_prefix` — prefix for NATS subjects (e.g. `"book"`)
    /// * `runtime` — handle to the Tokio runtime for spawning the batch task
    #[inline]
    pub fn new(
        jetstream: async_nats::jetstream::Context,
        symbol: String,
        subject_prefix: String,
        runtime: tokio::runtime::Handle,
    ) -> Self {
        Self {
            jetstream,
            symbol,
            subject_prefix,
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
        }
    }

    /// Set the batch window duration in milliseconds.
    ///
    /// Events are accumulated for at most this duration before being flushed.
    /// Defaults to [`DEFAULT_BATCH_WINDOW_MS`] (1 ms).
    #[must_use = "builders do nothing unless consumed"]
    #[inline]
    pub fn with_batch_window_ms(mut self, batch_window_ms: u64) -> Self {
        self.batch_window_ms = batch_window_ms;
        self
    }

    /// Set the maximum number of events per batch.
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
    /// When the channel is full, new events are dropped and `dropped_events`
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

    /// Set the minimum interval in milliseconds between consecutive publishes.
    ///
    /// When set to a value greater than 0, the publisher will wait at least
    /// this long between consecutive NATS publish operations. Defaults to
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

    /// Returns the number of successfully published batches.
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

    /// Returns the total number of events received from the listener callback.
    #[must_use]
    #[inline]
    pub fn events_received(&self) -> u64 {
        self.events_received.load(Ordering::Relaxed)
    }

    /// Returns the total number of batches successfully flushed to NATS.
    #[must_use]
    #[inline]
    pub fn batches_published(&self) -> u64 {
        self.batches_published.load(Ordering::Relaxed)
    }

    /// Returns the number of events dropped because the channel was full.
    #[must_use]
    #[inline]
    pub fn dropped_events(&self) -> u64 {
        self.dropped_events.load(Ordering::Relaxed)
    }

    /// Returns the current batch sequence number (next value to be assigned).
    #[must_use]
    #[inline]
    pub fn sequence(&self) -> u64 {
        self.sequence.load(Ordering::Relaxed)
    }

    /// Convert this publisher into a [`PriceLevelChangedListener`] callback.
    ///
    /// This method consumes `self`, wraps it in an `Arc`, spawns a background
    /// batch task on the configured Tokio runtime, and returns both the `Arc`
    /// handle (for reading metrics) and the listener callback.
    ///
    /// The listener sends each [`PriceLevelChangedEvent`] into a bounded
    /// channel. The background task drains the channel, batches events, and
    /// publishes them to NATS JetStream.
    ///
    /// # Returns
    ///
    /// A tuple of `(Arc<NatsBookChangePublisher>, PriceLevelChangedListener)`.
    /// The `Arc` handle allows the caller to read metrics after wiring the
    /// listener into the order book.
    pub fn into_listener(self) -> (Arc<Self>, PriceLevelChangedListener) {
        let channel_capacity = self.channel_capacity;
        let publisher = Arc::new(self);
        let handle = Arc::clone(&publisher);

        let (tx, rx) = mpsc::channel::<PriceLevelChangedEvent>(channel_capacity);

        // Spawn the background batch task
        let batch_publisher = Arc::clone(&publisher);
        publisher
            .runtime
            .spawn(Self::batch_task(batch_publisher, rx));

        // Build the listener closure
        let listener_publisher = Arc::clone(&publisher);
        let listener = Arc::new(move |event: PriceLevelChangedEvent| {
            listener_publisher
                .events_received
                .fetch_add(1, Ordering::Relaxed);
            if tx.try_send(event).is_err() {
                listener_publisher
                    .dropped_events
                    .fetch_add(1, Ordering::Relaxed);
                warn!("book change channel full, event dropped");
            }
        });

        (handle, listener)
    }

    /// Background task that drains the event channel, batches events, and
    /// publishes them to NATS.
    ///
    /// The task flushes when either:
    /// - The batch window timer elapses (configurable via `batch_window_ms`)
    /// - The batch reaches `max_batch_size` events
    ///
    /// When throttling is enabled (`min_publish_interval_ms > 0`), the task
    /// waits at least that duration between consecutive publishes.
    async fn batch_task(publisher: Arc<Self>, mut rx: mpsc::Receiver<PriceLevelChangedEvent>) {
        let batch_window = std::time::Duration::from_millis(publisher.batch_window_ms);
        let min_interval = if publisher.min_publish_interval_ms > 0 {
            Some(std::time::Duration::from_millis(
                publisher.min_publish_interval_ms,
            ))
        } else {
            None
        };

        let mut batch: Vec<BookChangeEntry> = Vec::with_capacity(publisher.max_batch_size);
        let mut last_publish = tokio::time::Instant::now();

        loop {
            // Wait for the first event or channel close
            if batch.is_empty() {
                match rx.recv().await {
                    Some(event) => batch.push(BookChangeEntry::from(event)),
                    None => break, // Channel closed
                }
            }

            // Collect more events within the batch window
            let deadline = tokio::time::Instant::now() + batch_window;
            while batch.len() < publisher.max_batch_size {
                match tokio::time::timeout_at(deadline, rx.recv()).await {
                    Ok(Some(event)) => batch.push(BookChangeEntry::from(event)),
                    Ok(None) => {
                        // Channel closed — flush remaining and exit
                        if !batch.is_empty() {
                            Self::flush_batch(
                                &publisher,
                                &mut batch,
                                &mut last_publish,
                                min_interval,
                            )
                            .await;
                        }
                        return;
                    }
                    Err(_) => break, // Timeout — flush batch
                }
            }

            // Flush the batch (throttling is applied inside flush_batch)
            Self::flush_batch(&publisher, &mut batch, &mut last_publish, min_interval).await;
        }

        // Flush any remaining events
        if !batch.is_empty() {
            Self::flush_batch(&publisher, &mut batch, &mut last_publish, min_interval).await;
        }
    }

    /// Flush the accumulated batch to NATS JetStream.
    ///
    /// Publishes to three subjects:
    /// - `{prefix}.{symbol}.changes` — all changes
    /// - `{prefix}.{symbol}.bid` — bid-side changes only
    /// - `{prefix}.{symbol}.ask` — ask-side changes only
    ///
    /// Side-specific subjects are only published if the batch contains events
    /// for that side.
    async fn flush_batch(
        publisher: &Arc<Self>,
        batch: &mut Vec<BookChangeEntry>,
        last_publish: &mut tokio::time::Instant,
        min_interval: Option<std::time::Duration>,
    ) {
        if batch.is_empty() {
            return;
        }

        let seq = publisher.sequence.fetch_add(1, Ordering::Relaxed);
        let timestamp_ms = crate::utils::current_time_millis();
        let changes: Vec<BookChangeEntry> = std::mem::take(batch);

        let all_batch = BookChangeBatch {
            symbol: publisher.symbol.clone(),
            sequence: seq,
            timestamp_ms,
            event_count: changes.len(),
            changes: changes.clone(),
        };

        // Publish the aggregate changes subject
        let changes_subject = format!("{}.{}.changes", publisher.subject_prefix, publisher.symbol);
        let all_ok = Self::publish_batch(publisher, &changes_subject, &all_batch, seq).await;

        // Publish bid-side subject if there are bid changes
        let bid_changes: Vec<BookChangeEntry> = changes
            .iter()
            .filter(|c| c.side == Side::Buy)
            .cloned()
            .collect();
        let bid_ok = if !bid_changes.is_empty() {
            let bid_seq = publisher.sequence.fetch_add(1, Ordering::Relaxed);
            let bid_batch = BookChangeBatch {
                symbol: publisher.symbol.clone(),
                sequence: bid_seq,
                timestamp_ms,
                event_count: bid_changes.len(),
                changes: bid_changes,
            };
            let bid_subject = format!("{}.{}.bid", publisher.subject_prefix, publisher.symbol);
            Self::publish_batch(publisher, &bid_subject, &bid_batch, bid_seq).await
        } else {
            true
        };

        // Publish ask-side subject if there are ask changes
        let ask_changes: Vec<BookChangeEntry> = changes
            .iter()
            .filter(|c| c.side == Side::Sell)
            .cloned()
            .collect();
        let ask_ok = if !ask_changes.is_empty() {
            let ask_seq = publisher.sequence.fetch_add(1, Ordering::Relaxed);
            let ask_batch = BookChangeBatch {
                symbol: publisher.symbol.clone(),
                sequence: ask_seq,
                timestamp_ms,
                event_count: ask_changes.len(),
                changes: ask_changes,
            };
            let ask_subject = format!("{}.{}.ask", publisher.subject_prefix, publisher.symbol);
            Self::publish_batch(publisher, &ask_subject, &ask_batch, ask_seq).await
        } else {
            true
        };

        if all_ok && bid_ok && ask_ok {
            publisher.publish_count.fetch_add(1, Ordering::Relaxed);
            publisher.batches_published.fetch_add(1, Ordering::Relaxed);
            trace!(seq, symbol = %publisher.symbol, "book change batch published to NATS");
        }

        // Throttle: wait if needed before allowing next flush
        if let Some(interval) = min_interval {
            let elapsed = last_publish.elapsed();
            if elapsed < interval {
                tokio::time::sleep(interval - elapsed).await;
            }
        }

        *last_publish = tokio::time::Instant::now();
    }

    /// Serialize and publish a single batch to a NATS subject with retry logic.
    ///
    /// Returns `true` if the publish succeeded, `false` if all retries were
    /// exhausted.
    async fn publish_batch(
        publisher: &Arc<Self>,
        subject: &str,
        batch: &BookChangeBatch,
        seq: u64,
    ) -> bool {
        let payload = match serde_json::to_vec(batch) {
            Ok(bytes) => bytes,
            Err(e) => {
                publisher.error_count.fetch_add(1, Ordering::Relaxed);
                error!(error = %e, "failed to serialize book change batch for NATS");
                return false;
            }
        };

        let payload_bytes: bytes::Bytes = payload.into();

        let mut headers = async_nats::HeaderMap::new();
        headers.insert("Nats-Sequence", seq.to_string().as_str());

        Self::publish_single(publisher, subject, payload_bytes, headers).await
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

impl std::fmt::Debug for NatsBookChangePublisher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NatsBookChangePublisher")
            .field("symbol", &self.symbol)
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
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_book_change_entry_from_event() {
        let event = PriceLevelChangedEvent {
            side: Side::Buy,
            price: 50_000,
            quantity: 100,
        };
        let entry = BookChangeEntry::from(event);
        assert_eq!(entry.side, Side::Buy);
        assert_eq!(entry.price, 50_000);
        assert_eq!(entry.quantity, 100);
    }

    #[test]
    fn test_book_change_entry_serializes_to_json() {
        let entry = BookChangeEntry {
            side: Side::Buy,
            price: 50_000,
            quantity: 100,
        };
        let result = serde_json::to_value(&entry);
        assert!(result.is_ok());
        let value = result.unwrap_or(serde_json::Value::Null);
        assert_eq!(value.get("price").and_then(|v| v.as_u64()), Some(50_000));
        assert_eq!(value.get("quantity").and_then(|v| v.as_u64()), Some(100));
        assert!(value.get("side").is_some());
    }

    #[test]
    fn test_book_change_batch_serializes_to_json() {
        let batch = BookChangeBatch {
            symbol: "BTC/USD".to_string(),
            sequence: 42,
            timestamp_ms: 1_700_000_000_000,
            event_count: 2,
            changes: vec![
                BookChangeEntry {
                    side: Side::Buy,
                    price: 50_000,
                    quantity: 100,
                },
                BookChangeEntry {
                    side: Side::Sell,
                    price: 50_100,
                    quantity: 200,
                },
            ],
        };
        let result = serde_json::to_vec(&batch);
        assert!(result.is_ok());
        let bytes = result.unwrap_or_default();
        assert!(!bytes.is_empty());

        let json_str = String::from_utf8(bytes).unwrap_or_default();
        assert!(json_str.contains("BTC/USD"));
        assert!(json_str.contains("\"sequence\":42"));
        assert!(json_str.contains("\"event_count\":2"));
    }

    #[test]
    fn test_book_change_batch_roundtrip_fields() {
        let batch = BookChangeBatch {
            symbol: "ETH/USDT".to_string(),
            sequence: 7,
            timestamp_ms: 1_700_000_000_000,
            event_count: 1,
            changes: vec![BookChangeEntry {
                side: Side::Sell,
                price: 2_000,
                quantity: 50,
            }],
        };
        let json = serde_json::to_value(&batch);
        assert!(json.is_ok());
        let value = json.unwrap_or(serde_json::Value::Null);
        assert_eq!(
            value.get("symbol").and_then(|v| v.as_str()),
            Some("ETH/USDT")
        );
        assert_eq!(value.get("sequence").and_then(|v| v.as_u64()), Some(7));
        assert_eq!(value.get("event_count").and_then(|v| v.as_u64()), Some(1));
        let changes = value.get("changes").and_then(|v| v.as_array());
        assert!(changes.is_some());
        assert_eq!(changes.map(|c| c.len()), Some(1));
    }

    #[test]
    fn test_subject_formatting_changes() {
        let prefix = "book";
        let symbol = "BTC/USD";
        let changes_subject = format!("{prefix}.{symbol}.changes");
        let bid_subject = format!("{prefix}.{symbol}.bid");
        let ask_subject = format!("{prefix}.{symbol}.ask");

        assert_eq!(changes_subject, "book.BTC/USD.changes");
        assert_eq!(bid_subject, "book.BTC/USD.bid");
        assert_eq!(ask_subject, "book.BTC/USD.ask");
    }

    #[test]
    fn test_subject_formatting_with_custom_prefix() {
        let prefix = "orderbook.events";
        let symbol = "ETH-PERP";
        let changes_subject = format!("{prefix}.{symbol}.changes");
        let bid_subject = format!("{prefix}.{symbol}.bid");
        let ask_subject = format!("{prefix}.{symbol}.ask");

        assert_eq!(changes_subject, "orderbook.events.ETH-PERP.changes");
        assert_eq!(bid_subject, "orderbook.events.ETH-PERP.bid");
        assert_eq!(ask_subject, "orderbook.events.ETH-PERP.ask");
    }

    #[test]
    fn test_default_constants() {
        assert_eq!(DEFAULT_BATCH_WINDOW_MS, 1);
        assert_eq!(DEFAULT_MAX_BATCH_SIZE, 100);
        assert_eq!(DEFAULT_CHANNEL_CAPACITY, 10_000);
        assert_eq!(DEFAULT_MAX_RETRIES, 3);
        assert_eq!(BASE_RETRY_DELAY_MS, 10);
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
        for attempt in [63u32, 64, 100, u32::MAX] {
            let shift = u32::min(attempt, 63);
            let delay =
                BASE_RETRY_DELAY_MS.saturating_mul(1u64.checked_shl(shift).unwrap_or(u64::MAX));
            assert!(delay >= BASE_RETRY_DELAY_MS);
        }
    }

    #[test]
    fn test_empty_batch_serializes() {
        let batch = BookChangeBatch {
            symbol: "BTC/USD".to_string(),
            sequence: 0,
            timestamp_ms: 0,
            event_count: 0,
            changes: vec![],
        };
        let result = serde_json::to_vec(&batch);
        assert!(result.is_ok());
        let json_str = String::from_utf8(result.unwrap_or_default()).unwrap_or_default();
        assert!(json_str.contains("\"event_count\":0"));
        assert!(json_str.contains("\"changes\":[]"));
    }

    #[test]
    fn test_price_level_changed_event_serializes() {
        let event = PriceLevelChangedEvent {
            side: Side::Buy,
            price: 42_000,
            quantity: 500,
        };
        let result = serde_json::to_value(&event);
        assert!(result.is_ok());
        let value = result.unwrap_or(serde_json::Value::Null);
        assert_eq!(value.get("price").and_then(|v| v.as_u64()), Some(42_000));
        assert_eq!(value.get("quantity").and_then(|v| v.as_u64()), Some(500));
    }

    #[test]
    fn test_nats_publish_error_display() {
        let err = crate::orderbook::OrderBookError::NatsPublishError {
            message: "timeout".to_string(),
        };
        let display = format!("{err}");
        assert!(display.contains("nats publish error"));
        assert!(display.contains("timeout"));
    }

    #[test]
    fn test_nats_serialization_error_display() {
        let err = crate::orderbook::OrderBookError::NatsSerializationError {
            message: "invalid data".to_string(),
        };
        let display = format!("{err}");
        assert!(display.contains("nats serialization error"));
        assert!(display.contains("invalid data"));
    }
}
