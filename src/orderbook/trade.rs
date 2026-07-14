/******************************************************************************
   Author: Joaquín Béjar García
   Email: jb@taunais.com
   Date: 2/10/25
******************************************************************************/
use crate::orderbook::fees::FeeSchedule;
use pricelevel::MatchResult;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Enhanced trade result that includes symbol information and fee details
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeResult {
    /// The symbol this trade result belongs to
    pub symbol: String,
    /// The underlying match result from the pricelevel crate
    pub match_result: MatchResult,
    /// Total maker fees across all transactions in this trade, in the same
    /// unit as the notional (price × quantity). Negative values represent
    /// rebates. Zero when no `FeeSchedule` is configured.
    pub total_maker_fees: i128,
    /// Total taker fees across all transactions in this trade, in the same
    /// unit as the notional (price × quantity). Zero when no `FeeSchedule`
    /// is configured.
    pub total_taker_fees: i128,
    /// Strictly monotonic global sequence number across every outbound
    /// stream of this `OrderBook<T>` instance: the `TradeListener`
    /// callback, the `PriceLevelChangedListener` callback, and the NATS
    /// publishers. Use it for cross-stream gap detection and temporal
    /// ordering. Always strictly increasing within a single book; replay
    /// into a fresh book yields fresh seqs, not the originals. Stamped at
    /// emission time by `OrderBook::next_engine_seq`.
    ///
    /// Defaults to `0` when deserializing payloads from format versions
    /// that pre-date `engine_seq` so existing consumers keep parsing.
    #[serde(default)]
    pub engine_seq: u64,
    /// Total quote-asset notional consumed by this trade, computed as
    /// `Σ price × quantity` across every transaction. Populated for both
    /// base-quantity (`match_market_order`) and quote-notional
    /// (`match_market_order_by_amount`) market-order paths so consumers
    /// have the field uniformly available without recomputing per-trade.
    ///
    /// Defaults to `0` when deserializing payloads from format versions
    /// that pre-date `quote_notional` so existing consumers keep parsing.
    #[serde(default)]
    pub quote_notional: u128,
}

impl TradeResult {
    /// Create a new `TradeResult` with zero fees
    ///
    /// Use this constructor when no `FeeSchedule` is configured.
    /// Fees default to zero for backward compatibility. The
    /// `quote_notional` field is populated from the supplied
    /// `match_result` (sum of `price × quantity` across every trade).
    pub fn new(symbol: String, match_result: MatchResult) -> Self {
        let quote_notional = compute_quote_notional(&match_result);
        Self {
            symbol,
            match_result,
            total_maker_fees: 0,
            total_taker_fees: 0,
            engine_seq: 0,
            quote_notional,
        }
    }

    /// Create a new `TradeResult` with fees calculated from the given schedule
    ///
    /// For each transaction in the match result, the maker and taker fees
    /// are computed using `FeeSchedule::calculate_fee` with the transaction
    /// notional value (price × quantity).
    ///
    /// # Arguments
    ///
    /// * `symbol` - The trading symbol
    /// * `match_result` - The matching engine result containing transactions
    /// * `fee_schedule` - Optional fee schedule; `None` results in zero fees
    pub fn with_fees(
        symbol: String,
        match_result: MatchResult,
        fee_schedule: Option<FeeSchedule>,
    ) -> Self {
        let (total_maker_fees, total_taker_fees) = match fee_schedule {
            Some(schedule) if !schedule.is_zero_fee() => {
                let mut maker_sum: i128 = 0;
                let mut taker_sum: i128 = 0;
                for tx in match_result.trades().as_vec() {
                    let notional = tx
                        .price()
                        .as_u128()
                        .saturating_mul(tx.quantity().as_u64() as u128);
                    maker_sum = maker_sum
                        .checked_add(schedule.calculate_fee(notional, true))
                        .unwrap_or(maker_sum);
                    taker_sum = taker_sum
                        .checked_add(schedule.calculate_fee(notional, false))
                        .unwrap_or(taker_sum);
                }
                (maker_sum, taker_sum)
            }
            _ => (0, 0),
        };

        let quote_notional = compute_quote_notional(&match_result);
        Self {
            symbol,
            match_result,
            total_maker_fees,
            total_taker_fees,
            engine_seq: 0,
            quote_notional,
        }
    }

    /// Returns the sum of all fees (maker + taker) for this trade
    ///
    /// A positive value means net fees charged; a negative value means
    /// the maker rebate exceeds the taker fee (unusual but possible).
    #[must_use]
    #[inline]
    pub fn total_fees(&self) -> i128 {
        self.total_maker_fees
            .checked_add(self.total_taker_fees)
            .unwrap_or(i128::MAX)
    }
}

/// Sum of `price × quantity` across every trade in `match_result`.
///
/// Saturates on overflow rather than panicking — overflow on `u128` can
/// only occur in adversarial fixtures with prices near `u128::MAX`, and
/// the matching path already saturates equivalent multiplications.
#[inline]
#[must_use]
fn compute_quote_notional(match_result: &MatchResult) -> u128 {
    let mut total: u128 = 0;
    for tx in match_result.trades().as_vec() {
        let notional = tx
            .price()
            .as_u128()
            .saturating_mul(u128::from(tx.quantity().as_u64()));
        total = total.saturating_add(notional);
    }
    total
}

/// Trade listener specification using Arc for shared ownership
pub type TradeListener = Arc<dyn Fn(&TradeResult) + Send + Sync>;

/// A trade event that includes additional metadata for processing
#[derive(Debug, Clone)]
pub struct TradeEvent {
    /// The trading symbol for this event
    pub symbol: String,
    /// The trade result containing match details
    pub trade_result: TradeResult,
    /// Unix timestamp in milliseconds when the trade occurred
    pub timestamp: u64,
    /// Mirrors [`TradeResult::engine_seq`]. Exposed on the envelope so the
    /// outbound payload carries the engine sequence directly without
    /// forcing consumers to reach into `trade_result`.
    pub engine_seq: u64,
}

/// Structure to store trade information for later display
#[derive(Debug, Clone)]
pub struct TradeInfo {
    /// The trading symbol
    pub symbol: String,
    /// The order identifier as a string
    pub order_id: String,
    /// Total quantity executed in this trade
    pub executed_quantity: u64,
    /// Remaining quantity not yet filled
    pub remaining_quantity: u64,
    /// Whether the order was completely filled
    pub is_complete: bool,
    /// Number of individual transactions that occurred
    pub transaction_count: usize,
    /// Detailed information about each transaction
    pub transactions: Vec<TransactionInfo>,
}

/// Information about a single transaction within a trade
#[derive(Debug, Clone)]
pub struct TransactionInfo {
    /// The price at which the transaction occurred
    pub price: u128,
    /// The quantity traded in this transaction
    pub quantity: u64,
    /// Unique identifier for this transaction
    pub transaction_id: String,
    /// Order ID of the maker (passive) side
    pub maker_order_id: String,
    /// Order ID of the taker (aggressive) side
    pub taker_order_id: String,
    /// Fee charged to the maker for this transaction, in the same unit
    /// as the notional (price × quantity). Negative values represent
    /// rebates.
    pub maker_fee: i128,
    /// Fee charged to the taker for this transaction, in the same unit
    /// as the notional (price × quantity). Always non-negative.
    pub taker_fee: i128,
}

impl TradeInfo {
    /// Build a display-oriented [`TradeInfo`] from a [`TradeResult`],
    /// populating each [`TransactionInfo`]'s per-transaction maker/taker
    /// fees from `fee_schedule`.
    ///
    /// For every transaction the fee is what the configured [`FeeSchedule`]
    /// charges on that transaction's notional (`price × quantity`):
    /// `maker_fee = calculate_fee(notional, true)` (negative for a rebate)
    /// and `taker_fee = calculate_fee(notional, false)`. When `fee_schedule`
    /// is `None` (or a zero-fee schedule) every per-transaction fee is `0`.
    ///
    /// The per-transaction fees sum to the aggregate
    /// [`TradeResult::total_maker_fees`] / [`TradeResult::total_taker_fees`]
    /// produced by [`TradeResult::with_fees`] for the same schedule, so the
    /// detailed and aggregate views are consistent. This is the
    /// authoritative engine-side population path for the `TransactionInfo`
    /// fee fields; consumers should prefer it to constructing
    /// `TransactionInfo` by hand (which historically left the fees at `0`).
    #[must_use]
    pub fn from_trade_result(
        trade_result: &TradeResult,
        fee_schedule: Option<&FeeSchedule>,
    ) -> Self {
        let match_result = &trade_result.match_result;
        let schedule = fee_schedule.filter(|s| !s.is_zero_fee());

        let transactions: Vec<TransactionInfo> = match_result
            .trades()
            .as_vec()
            .iter()
            .map(|tx| {
                let notional = tx
                    .price()
                    .as_u128()
                    .saturating_mul(u128::from(tx.quantity().as_u64()));
                let (maker_fee, taker_fee) = match schedule {
                    Some(s) => (
                        s.calculate_fee(notional, true),
                        s.calculate_fee(notional, false),
                    ),
                    None => (0, 0),
                };
                TransactionInfo {
                    price: tx.price().as_u128(),
                    quantity: tx.quantity().as_u64(),
                    transaction_id: tx.trade_id().to_string(),
                    maker_order_id: tx.maker_order_id().to_string(),
                    taker_order_id: tx.taker_order_id().to_string(),
                    maker_fee,
                    taker_fee,
                }
            })
            .collect();

        Self {
            symbol: trade_result.symbol.clone(),
            order_id: match_result.order_id().to_string(),
            executed_quantity: match_result
                .executed_quantity()
                .map(|q| q.as_u64())
                .unwrap_or(0),
            remaining_quantity: match_result.remaining_quantity().as_u64(),
            is_complete: match_result.is_complete(),
            transaction_count: match_result.trades().len(),
            transactions,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pricelevel::{Id, MatchResult, Price, Quantity, Trade};

    /// Taker order id shared by every fixture trade: since pricelevel 0.9
    /// `MatchResult::add_trade` validates that each trade's taker order id
    /// matches the result's incoming order id, so the fixture threads one
    /// id through both.
    fn make_match_result_with_trades(trades: Vec<Trade>) -> MatchResult {
        let taker_order_id = trades
            .first()
            .map(|t| t.taker_order_id())
            .unwrap_or_else(Id::new_uuid);
        let total_qty: u64 = trades.iter().map(|t| t.quantity().as_u64()).sum();
        let initial_qty = if trades.is_empty() { 100 } else { total_qty };
        let mut mr = MatchResult::new(taker_order_id, Quantity::new(initial_qty));
        for trade in trades {
            assert!(
                mr.add_trade(trade).is_ok(),
                "fixture trade must satisfy the match-result invariants"
            );
        }
        mr
    }

    /// Taker order id for fixture trades — one shared id so
    /// `make_match_result_with_trades` can satisfy the taker-identity
    /// invariant.
    const FIXTURE_TAKER: u64 = 424_242;

    fn make_trade(price: u128, quantity: u64) -> Trade {
        Trade::new(
            Id::new_uuid(),
            Id::from_u64(FIXTURE_TAKER),
            Id::new_uuid(),
            Price::new(price),
            Quantity::new(quantity),
            pricelevel::Side::Buy,
        )
    }

    #[test]
    fn test_trade_result_new_has_zero_fees() {
        let mr = make_match_result_with_trades(vec![make_trade(1000, 10)]);
        let tr = TradeResult::new("BTC/USD".to_string(), mr);

        assert_eq!(tr.total_maker_fees, 0);
        assert_eq!(tr.total_taker_fees, 0);
        assert_eq!(tr.total_fees(), 0);
    }

    #[test]
    fn test_trade_result_with_fees_none_schedule() {
        let mr = make_match_result_with_trades(vec![make_trade(1000, 10)]);
        let tr = TradeResult::with_fees("BTC/USD".to_string(), mr, None);

        assert_eq!(tr.total_maker_fees, 0);
        assert_eq!(tr.total_taker_fees, 0);
        assert_eq!(tr.total_fees(), 0);
    }

    #[test]
    fn test_trade_result_with_fees_zero_schedule() {
        let schedule = FeeSchedule::zero_fee();
        let mr = make_match_result_with_trades(vec![make_trade(1000, 10)]);
        let tr = TradeResult::with_fees("BTC/USD".to_string(), mr, Some(schedule));

        assert_eq!(tr.total_maker_fees, 0);
        assert_eq!(tr.total_taker_fees, 0);
        assert_eq!(tr.total_fees(), 0);
    }

    #[test]
    fn test_trade_result_with_fees_single_transaction() {
        // 5 bps taker, -2 bps maker rebate
        let schedule = FeeSchedule::new(-2, 5);
        // notional = 1000 * 10 = 10_000
        let mr = make_match_result_with_trades(vec![make_trade(1000, 10)]);
        let tr = TradeResult::with_fees("BTC/USD".to_string(), mr, Some(schedule));

        // maker fee: 10_000 * -2 / 10_000 = -2
        assert_eq!(tr.total_maker_fees, -2);
        // taker fee: 10_000 * 5 / 10_000 = 5
        assert_eq!(tr.total_taker_fees, 5);
        // total = -2 + 5 = 3
        assert_eq!(tr.total_fees(), 3);
    }

    #[test]
    fn test_trade_result_with_fees_multiple_transactions() {
        let schedule = FeeSchedule::new(-2, 5);
        let mr = make_match_result_with_trades(vec![
            make_trade(1000, 10), // notional = 10_000
            make_trade(2000, 20), // notional = 40_000
        ]);
        let tr = TradeResult::with_fees("BTC/USD".to_string(), mr, Some(schedule));

        // maker fees: (-2 * 10_000 / 10_000) + (-2 * 40_000 / 10_000) = -2 + -8 = -10
        assert_eq!(tr.total_maker_fees, -10);
        // taker fees: (5 * 10_000 / 10_000) + (5 * 40_000 / 10_000) = 5 + 20 = 25
        assert_eq!(tr.total_taker_fees, 25);
        assert_eq!(tr.total_fees(), 15);
    }

    #[test]
    fn test_trade_result_with_fees_no_transactions() {
        let schedule = FeeSchedule::new(-2, 5);
        let mr = make_match_result_with_trades(vec![]);
        let tr = TradeResult::with_fees("BTC/USD".to_string(), mr, Some(schedule));

        assert_eq!(tr.total_maker_fees, 0);
        assert_eq!(tr.total_taker_fees, 0);
        assert_eq!(tr.total_fees(), 0);
    }

    #[test]
    fn test_trade_result_with_maker_rebate() {
        let schedule = FeeSchedule::with_maker_rebate(5, 10);
        // notional = 100_000 * 50 = 5_000_000
        let mr = make_match_result_with_trades(vec![make_trade(100_000, 50)]);
        let tr = TradeResult::with_fees("BTC/USD".to_string(), mr, Some(schedule));

        // maker: -5 * 5_000_000 / 10_000 = -2_500
        assert_eq!(tr.total_maker_fees, -2_500);
        // taker: 10 * 5_000_000 / 10_000 = 5_000
        assert_eq!(tr.total_taker_fees, 5_000);
        assert_eq!(tr.total_fees(), 2_500);
        assert!(tr.total_maker_fees < 0); // rebate
    }

    #[test]
    fn test_trade_info_from_result_populates_per_transaction_fees_issue_119() {
        let schedule = FeeSchedule::new(-2, 5); // -2 bps maker rebate, 5 bps taker
        let mr = make_match_result_with_trades(vec![
            make_trade(1000, 10), // notional 10_000 → maker -2, taker 5
            make_trade(2000, 20), // notional 40_000 → maker -8, taker 20
        ]);
        let tr = TradeResult::with_fees("BTC/USD".to_string(), mr, Some(schedule));

        let info = TradeInfo::from_trade_result(&tr, Some(&schedule));

        assert_eq!(info.symbol, "BTC/USD");
        assert_eq!(info.transaction_count, 2);
        assert_eq!(info.transactions.len(), 2);

        // Per-transaction fees are populated (no longer hard-zero).
        assert_eq!(info.transactions[0].maker_fee, -2);
        assert_eq!(info.transactions[0].taker_fee, 5);
        assert_eq!(info.transactions[1].maker_fee, -8);
        assert_eq!(info.transactions[1].taker_fee, 20);

        // The detailed view sums to the aggregate TradeResult fees.
        let maker_sum: i128 = info.transactions.iter().map(|t| t.maker_fee).sum();
        let taker_sum: i128 = info.transactions.iter().map(|t| t.taker_fee).sum();
        assert_eq!(maker_sum, tr.total_maker_fees);
        assert_eq!(taker_sum, tr.total_taker_fees);
    }

    #[test]
    fn test_trade_info_from_result_none_schedule_zero_fees_issue_119() {
        let mr = make_match_result_with_trades(vec![make_trade(1000, 10)]);
        let tr = TradeResult::new("ETH/USD".to_string(), mr);

        // No schedule → per-transaction fees are zero, metadata still populated.
        let info = TradeInfo::from_trade_result(&tr, None);
        assert_eq!(info.transaction_count, 1);
        assert_eq!(info.transactions.len(), 1);
        assert_eq!(info.transactions[0].maker_fee, 0);
        assert_eq!(info.transactions[0].taker_fee, 0);
        assert_eq!(info.transactions[0].price, 1000);
        assert_eq!(info.transactions[0].quantity, 10);

        // A zero-fee schedule is treated the same as no schedule.
        let zero = FeeSchedule::zero_fee();
        let info_zero = TradeInfo::from_trade_result(&tr, Some(&zero));
        assert_eq!(info_zero.transactions[0].maker_fee, 0);
        assert_eq!(info_zero.transactions[0].taker_fee, 0);
    }

    #[test]
    fn test_trade_result_symbol_preserved() {
        let mr = make_match_result_with_trades(vec![]);
        let tr = TradeResult::with_fees("ETH/USDT".to_string(), mr, None);
        assert_eq!(tr.symbol, "ETH/USDT");
    }

    #[test]
    fn test_transaction_info_fee_fields() {
        let info = TransactionInfo {
            price: 50_000,
            quantity: 10,
            transaction_id: "tx-1".to_string(),
            maker_order_id: "maker-1".to_string(),
            taker_order_id: "taker-1".to_string(),
            maker_fee: -25,
            taker_fee: 50,
        };

        assert_eq!(info.maker_fee, -25);
        assert_eq!(info.taker_fee, 50);
    }

    #[test]
    fn test_trade_result_engine_seq_default_zero() {
        let mr = make_match_result_with_trades(vec![make_trade(1000, 10)]);
        let tr = TradeResult::new("BTC/USD".to_string(), mr);
        assert_eq!(tr.engine_seq, 0);
    }

    #[test]
    fn test_trade_result_json_roundtrip_preserves_engine_seq() {
        let mr = make_match_result_with_trades(vec![make_trade(1000, 10)]);
        let mut tr = TradeResult::new("BTC/USD".to_string(), mr);
        tr.engine_seq = 42;

        let json = serde_json::to_vec(&tr).expect("serialize trade");
        let decoded: TradeResult = serde_json::from_slice(&json).expect("deserialize trade");

        assert_eq!(decoded.engine_seq, 42);
        assert_eq!(decoded.symbol, tr.symbol);
        assert_eq!(decoded.total_maker_fees, tr.total_maker_fees);
        assert_eq!(decoded.total_taker_fees, tr.total_taker_fees);
    }

    #[test]
    fn test_trade_result_json_missing_engine_seq_defaults_zero() {
        // Build a JSON payload that mirrors the pre-engine_seq schema by
        // first serializing a TradeResult and then stripping the field.
        let mr = make_match_result_with_trades(vec![make_trade(1000, 10)]);
        let mut tr = TradeResult::new("BTC/USD".to_string(), mr);
        tr.engine_seq = 99;

        let mut value: serde_json::Value =
            serde_json::to_value(&tr).expect("serialize trade to value");
        // Remove the field to simulate a payload from before engine_seq.
        if let Some(map) = value.as_object_mut() {
            map.remove("engine_seq");
        }
        let bytes = serde_json::to_vec(&value).expect("serialize stripped value");

        let decoded: TradeResult =
            serde_json::from_slice(&bytes).expect("deserialize stripped trade");
        assert_eq!(
            decoded.engine_seq, 0,
            "missing engine_seq must default to 0 via #[serde(default)]"
        );
    }

    #[test]
    fn test_trade_result_new_populates_quote_notional() {
        // single trade: 1000 * 10 = 10_000
        let mr = make_match_result_with_trades(vec![make_trade(1000, 10)]);
        let tr = TradeResult::new("BTC/USD".to_string(), mr);
        assert_eq!(tr.quote_notional, 10_000);
    }

    #[test]
    fn test_trade_result_with_fees_populates_quote_notional_multi_trade() {
        // 1000*10 + 2000*20 = 10_000 + 40_000 = 50_000
        let mr = make_match_result_with_trades(vec![make_trade(1000, 10), make_trade(2000, 20)]);
        let tr = TradeResult::with_fees("BTC/USD".to_string(), mr, None);
        assert_eq!(tr.quote_notional, 50_000);
    }

    #[test]
    fn test_trade_result_quote_notional_zero_when_no_trades() {
        let mr = make_match_result_with_trades(vec![]);
        let tr = TradeResult::new("BTC/USD".to_string(), mr);
        assert_eq!(tr.quote_notional, 0);
    }

    #[test]
    fn test_trade_result_json_missing_quote_notional_defaults_zero() {
        // Pre-quote_notional payload: serialize, strip the field, decode.
        let mr = make_match_result_with_trades(vec![make_trade(1000, 10)]);
        let tr = TradeResult::new("BTC/USD".to_string(), mr);

        let mut value: serde_json::Value =
            serde_json::to_value(&tr).expect("serialize trade to value");
        if let Some(map) = value.as_object_mut() {
            map.remove("quote_notional");
        }
        let bytes = serde_json::to_vec(&value).expect("serialize stripped value");

        let decoded: TradeResult =
            serde_json::from_slice(&bytes).expect("deserialize stripped trade");
        assert_eq!(
            decoded.quote_notional, 0,
            "missing quote_notional must default to 0 via #[serde(default)]"
        );
    }

    #[test]
    fn test_trade_result_json_roundtrip_preserves_quote_notional() {
        let mr = make_match_result_with_trades(vec![make_trade(1000, 10), make_trade(2000, 5)]);
        let tr = TradeResult::new("BTC/USD".to_string(), mr);
        let original = tr.quote_notional;
        assert_eq!(original, 20_000);

        let json = serde_json::to_vec(&tr).expect("serialize trade");
        let decoded: TradeResult = serde_json::from_slice(&json).expect("deserialize trade");
        assert_eq!(decoded.quote_notional, original);
    }

    #[cfg(feature = "bincode")]
    #[test]
    fn test_trade_result_bincode_roundtrip_preserves_quote_notional() {
        use bincode::config::standard;
        use bincode::serde::{decode_from_slice, encode_to_vec};

        let mr = make_match_result_with_trades(vec![make_trade(1234, 7)]);
        let tr = TradeResult::new("BTC/USD".to_string(), mr);
        let original = tr.quote_notional;
        assert_eq!(original, 8_638);

        let bytes = encode_to_vec(&tr, standard()).expect("bincode encode");
        let (decoded, consumed): (TradeResult, usize) =
            decode_from_slice(&bytes, standard()).expect("bincode decode");
        assert_eq!(consumed, bytes.len());
        assert_eq!(decoded.quote_notional, original);
    }

    #[cfg(feature = "bincode")]
    #[test]
    fn test_trade_result_bincode_roundtrip_preserves_engine_seq() {
        use bincode::config::standard;
        use bincode::serde::{decode_from_slice, encode_to_vec};

        let mr = make_match_result_with_trades(vec![make_trade(1000, 10)]);
        let mut tr = TradeResult::new("BTC/USD".to_string(), mr);
        tr.engine_seq = 7;

        let bytes = encode_to_vec(&tr, standard()).expect("bincode encode");
        let (decoded, consumed): (TradeResult, usize) =
            decode_from_slice(&bytes, standard()).expect("bincode decode");
        assert_eq!(consumed, bytes.len(), "no trailing bytes expected");
        assert_eq!(decoded.engine_seq, 7);
        assert_eq!(decoded.symbol, tr.symbol);
    }
}
