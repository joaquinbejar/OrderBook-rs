/******************************************************************************
   Author: Joaquín Béjar García
   Email: jb@taunais.com
   Date: 2/10/25
******************************************************************************/
use crate::orderbook::fees::FeeSchedule;
use pricelevel::MatchResult;
use serde::Serialize;
use std::sync::Arc;

/// Enhanced trade result that includes symbol information and fee details
#[derive(Debug, Clone, Serialize)]
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
}

impl TradeResult {
    /// Create a new `TradeResult` with zero fees
    ///
    /// Use this constructor when no `FeeSchedule` is configured.
    /// Fees default to zero for backward compatibility.
    pub fn new(symbol: String, match_result: MatchResult) -> Self {
        Self {
            symbol,
            match_result,
            total_maker_fees: 0,
            total_taker_fees: 0,
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
                for tx in match_result.transactions.as_vec() {
                    let notional = tx.price.saturating_mul(tx.quantity as u128);
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

        Self {
            symbol,
            match_result,
            total_maker_fees,
            total_taker_fees,
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

#[cfg(test)]
mod tests {
    use super::*;
    use pricelevel::{MatchResult, OrderId, Transaction};
    use uuid::Uuid;

    fn make_match_result_with_transactions(txs: Vec<Transaction>) -> MatchResult {
        let order_id = OrderId::new_uuid();
        let mut mr = MatchResult::new(order_id, 100);
        for tx in txs {
            mr.add_transaction(tx);
        }
        mr.remaining_quantity = 0;
        mr.is_complete = true;
        mr
    }

    fn make_transaction(price: u128, quantity: u64) -> Transaction {
        Transaction::new(
            Uuid::new_v4(),
            OrderId::new_uuid(),
            OrderId::new_uuid(),
            price,
            quantity,
            pricelevel::Side::Buy,
        )
    }

    #[test]
    fn test_trade_result_new_has_zero_fees() {
        let mr = make_match_result_with_transactions(vec![make_transaction(1000, 10)]);
        let tr = TradeResult::new("BTC/USD".to_string(), mr);

        assert_eq!(tr.total_maker_fees, 0);
        assert_eq!(tr.total_taker_fees, 0);
        assert_eq!(tr.total_fees(), 0);
    }

    #[test]
    fn test_trade_result_with_fees_none_schedule() {
        let mr = make_match_result_with_transactions(vec![make_transaction(1000, 10)]);
        let tr = TradeResult::with_fees("BTC/USD".to_string(), mr, None);

        assert_eq!(tr.total_maker_fees, 0);
        assert_eq!(tr.total_taker_fees, 0);
        assert_eq!(tr.total_fees(), 0);
    }

    #[test]
    fn test_trade_result_with_fees_zero_schedule() {
        let schedule = FeeSchedule::zero_fee();
        let mr = make_match_result_with_transactions(vec![make_transaction(1000, 10)]);
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
        let mr = make_match_result_with_transactions(vec![make_transaction(1000, 10)]);
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
        let mr = make_match_result_with_transactions(vec![
            make_transaction(1000, 10), // notional = 10_000
            make_transaction(2000, 20), // notional = 40_000
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
        let mr = make_match_result_with_transactions(vec![]);
        let tr = TradeResult::with_fees("BTC/USD".to_string(), mr, Some(schedule));

        assert_eq!(tr.total_maker_fees, 0);
        assert_eq!(tr.total_taker_fees, 0);
        assert_eq!(tr.total_fees(), 0);
    }

    #[test]
    fn test_trade_result_with_maker_rebate() {
        let schedule = FeeSchedule::with_maker_rebate(5, 10);
        // notional = 100_000 * 50 = 5_000_000
        let mr = make_match_result_with_transactions(vec![make_transaction(100_000, 50)]);
        let tr = TradeResult::with_fees("BTC/USD".to_string(), mr, Some(schedule));

        // maker: -5 * 5_000_000 / 10_000 = -2_500
        assert_eq!(tr.total_maker_fees, -2_500);
        // taker: 10 * 5_000_000 / 10_000 = 5_000
        assert_eq!(tr.total_taker_fees, 5_000);
        assert_eq!(tr.total_fees(), 2_500);
        assert!(tr.total_maker_fees < 0); // rebate
    }

    #[test]
    fn test_trade_result_symbol_preserved() {
        let mr = make_match_result_with_transactions(vec![]);
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
}
