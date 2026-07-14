//! Property tests for the queue-priority contract of
//! `OrderBook::update_order(OrderUpdate::UpdateQuantity { .. })` (issue #203):
//!
//! - A quantity **decrease** (or unchanged total) keeps the maker's queue
//!   position — reducing size never forfeits time priority.
//! - A quantity **increase** demotes the order to the back of its price
//!   level's queue.
//!
//! Both properties are asserted through the public API only: build a level of
//! resting standard orders, resize one, then sweep the level with an
//! aggressive market order and check the maker-id sequence of the fills.

use orderbook_rs::OrderBook;
use pricelevel::{Hash32, Id, MatchResult, OrderUpdate, Quantity, Side, TimeInForce};
use proptest::prelude::*;

/// Price level shared by every resting order; the market order sweeps it.
const LEVEL_PRICE: u128 = 100;

/// Id used by the aggressive market order — outside the resting-id range.
const TAKER_ID: u64 = 9_999;

/// Resting order `i` (zero-based insertion position) gets id `i + 1`.
fn resting_id(position: usize) -> Id {
    Id::from_u64(position as u64 + 1)
}

/// Builds a book with one ask level holding `quantities.len()` standard GTC
/// sell orders admitted in index order, so insertion position `i` has id
/// `i + 1` and quantity `quantities[i]`.
fn book_with_ask_level(quantities: &[u64]) -> Result<OrderBook<()>, TestCaseError> {
    let book = OrderBook::<()>::new("PROPS");
    for (position, quantity) in quantities.iter().enumerate() {
        if let Err(error) = book.add_limit_order_with_user(
            resting_id(position),
            LEVEL_PRICE,
            *quantity,
            Side::Sell,
            TimeInForce::Gtc,
            Hash32::zero(),
            None,
        ) {
            return Err(TestCaseError::fail(format!(
                "failed to rest order at position {position}: {error}"
            )));
        }
    }
    Ok(book)
}

/// Resizes the resting order at `position` to `new_quantity` and asserts the
/// update was applied to a live order.
fn resize_order(
    book: &OrderBook<()>,
    position: usize,
    new_quantity: u64,
) -> Result<(), TestCaseError> {
    match book.update_order(OrderUpdate::UpdateQuantity {
        order_id: resting_id(position),
        new_quantity: Quantity::new(new_quantity),
    }) {
        Ok(Some(_)) => Ok(()),
        Ok(None) => Err(TestCaseError::fail(format!(
            "order at position {position} not found for quantity update"
        ))),
        Err(error) => Err(TestCaseError::fail(format!(
            "quantity update at position {position} failed: {error}"
        ))),
    }
}

/// Sweeps the ask level with a market buy of `quantity` units and returns the
/// match result.
fn sweep(book: &OrderBook<()>, quantity: u64) -> Result<MatchResult, TestCaseError> {
    match book.submit_market_order_with_user(
        Id::from_u64(TAKER_ID),
        quantity,
        Side::Buy,
        Hash32::zero(),
    ) {
        Ok(result) => Ok(result),
        Err(error) => Err(TestCaseError::fail(format!(
            "market sweep of {quantity} units failed: {error}"
        ))),
    }
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 256,
        max_shrink_iters: 50_000,
        ..ProptestConfig::default()
    })]

    /// Reducing (or keeping) an order's quantity must preserve its queue
    /// position: a sweep that reaches exactly one unit into the resized
    /// order's original position must fill the orders ahead of it in
    /// insertion order and then partially fill the resized order itself.
    #[test]
    fn test_update_quantity_decrease_preserves_queue_position(
        quantities in proptest::collection::vec(1u64..=50, 3..8),
        position_index in any::<prop::sample::Index>(),
        target_index in any::<prop::sample::Index>(),
    ) {
        // Pick a position with at least one order resting behind it, so a
        // lost queue position would visibly reroute the final fill.
        let position = position_index.index(quantities.len() - 1);
        // New total in 1..=original: strict decrease or unchanged, both of
        // which the contract requires to keep the queue position.
        let new_quantity = 1 + target_index.index(quantities[position] as usize) as u64;

        let book = book_with_ask_level(&quantities)?;
        resize_order(&book, position, new_quantity)?;

        // One unit past the (unchanged) orders ahead of the resized order.
        let ahead: u64 = quantities[..position].iter().sum();
        let result = sweep(&book, ahead + 1)?;

        let trades = result.trades().as_vec();
        prop_assert_eq!(
            trades.len(),
            position + 1,
            "sweep must fill every order ahead plus the resized order"
        );
        for (fill_index, trade) in trades.iter().enumerate() {
            prop_assert_eq!(
                trade.maker_order_id(),
                resting_id(fill_index),
                "fill {} must consume insertion position {}",
                fill_index,
                fill_index
            );
        }
    }

    /// Increasing an order's quantity must demote it to the back of the
    /// queue: a sweep that consumes every other order plus one unit must
    /// fill all other orders in insertion order first and hit the resized
    /// order last.
    #[test]
    fn test_update_quantity_increase_demotes_to_back_of_queue(
        quantities in proptest::collection::vec(1u64..=50, 3..8),
        position_index in any::<prop::sample::Index>(),
        delta_index in any::<prop::sample::Index>(),
    ) {
        // Pick a position that is not already last, so the demotion is
        // observable in the fill sequence.
        let position = position_index.index(quantities.len() - 1);
        let delta = 1 + delta_index.index(50) as u64;
        let new_quantity = quantities[position] + delta;

        let book = book_with_ask_level(&quantities)?;
        resize_order(&book, position, new_quantity)?;

        // Every unit not belonging to the resized order, plus one unit that
        // must come from it — and can only be the final fill if the resize
        // demoted it behind every other resting order.
        let others: u64 = quantities
            .iter()
            .enumerate()
            .filter(|(index, _)| *index != position)
            .map(|(_, quantity)| *quantity)
            .sum();
        let result = sweep(&book, others + 1)?;

        let trades = result.trades().as_vec();
        prop_assert_eq!(
            trades.len(),
            quantities.len(),
            "sweep must fill every other order fully and the resized order once"
        );

        let expected_makers: Vec<Id> = (0..quantities.len())
            .filter(|index| *index != position)
            .map(resting_id)
            .chain(std::iter::once(resting_id(position)))
            .collect();
        let actual_makers: Vec<Id> = trades.iter().map(|trade| trade.maker_order_id()).collect();
        prop_assert_eq!(
            actual_makers,
            expected_makers,
            "demoted order must fill last, all others in insertion order"
        );
    }
}
