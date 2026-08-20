//! FIX message → order book instruction execution.
//!
//! [`apply_fix_message`] is the bridge's single entry point: given a
//! codec-decoded [`fix_codec::Message`] and an [`OrderBook`], it maps the
//! message onto the matching engine. `MsgType` `D` (NewOrderSingle) places a
//! new order, `F` (OrderCancelRequest) cancels by `OrigClOrdID`, and `G`
//! (OrderCancelReplaceRequest) cancels-and-replaces.

use fix_codec::Message;
use pricelevel::{Id, TimeInForce};

use crate::OrderBook;
use crate::orderbook::error::OrderBookError;

use super::convert::{
    FixConversionError, cl_ord_id_to_id, fix_side, fix_time_in_force, parse_qty, price_to_ticks,
};
use super::convert::tags::{self, MSG_TYPE};

/// Errors returned by [`apply_fix_message`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FixBridgeError {
    /// A required tag was missing or could not be converted.
    #[error("fix conversion error: {0}")]
    Conversion(#[from] FixConversionError),
    /// The `MsgType` (tag 35) is not one the bridge handles.
    #[error("unsupported FIX MsgType {0:?}")]
    UnsupportedMsgType(String),
    /// The order book rejected the mapped instruction.
    #[error("order book error: {0}")]
    OrderBook(String),
}

impl From<OrderBookError> for FixBridgeError {
    fn from(e: OrderBookError) -> Self {
        FixBridgeError::OrderBook(e.to_string())
    }
}

/// Outcome of applying a FIX message to an order book.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FixOutcome {
    /// A new order was placed; carries the book's order id.
    Placed {
        /// The order id the book assigned.
        order_id: Id,
    },
    /// An existing order was cancelled.
    Cancelled {
        /// The order id that was cancelled.
        order_id: Id,
    },
    /// A cancel-and-replace landed; carries the replacement's id.
    Replaced {
        /// The replacement order's id.
        order_id: Id,
    },
}

/// Apply a codec-decoded FIX message to an order book.
///
/// # Message types
///
/// - `D` (NewOrderSingle) — requires tags 54 (Side), 38 (OrderQty),
///   40 (OrdType), 44 (Price for limit orders); optional 59 (TimeInForce).
/// - `F` (OrderCancelRequest) — requires tag 41 (OrigClOrdID).
/// - `G` (OrderCancelReplaceRequest) — requires tag 41 plus the tags of a
///   new `D`.
///
/// Prices are interpreted as integer ticks when the book's `tick_size` is
/// `1`, otherwise scaled by the book's tick size (see
/// [`price_to_ticks`]).
///
/// # Errors
/// Returns [`FixBridgeError`] on missing/unconvertible tags, unsupported
/// `MsgType`, or when the order book rejects the mapped instruction.
pub fn apply_fix_message<T>(
    book: &OrderBook<T>,
    message: &Message,
    tick_size: u128,
) -> Result<FixOutcome, FixBridgeError>
where
    T: Clone + Send + Sync + Default + 'static,
{
    let msg_type = message
        .get(MSG_TYPE)
        .and_then(|f| f.as_str())
        .ok_or(FixConversionError::MissingField(MSG_TYPE))?;

    match msg_type {
        "D" => apply_new_order_single(book, message, tick_size),
        "F" => apply_order_cancel(book, message),
        "G" => apply_order_cancel_replace(book, message, tick_size),
        other => Err(FixBridgeError::UnsupportedMsgType(other.to_string())),
    }
}

/// Map `OrdType` (tag 40) plus side/qty/price onto the right order call.
fn apply_new_order_single<T>(
    book: &OrderBook<T>,
    message: &Message,
    tick_size: u128,
) -> Result<FixOutcome, FixBridgeError>
where
    T: Clone + Send + Sync + Default + 'static,
{
    let cl_ord_id = required_str(message, tags::CL_ORD_ID)?;
    let order_id = cl_ord_id_to_id(cl_ord_id);
    let side = required_str(message, tags::SIDE).and_then(fix_side)?;
    let qty = required_str(message, tags::ORDER_QTY).and_then(parse_qty)?;
    let ord_type = required_str(message, tags::ORD_TYPE)?;
    let time_in_force = message
        .get(tags::TIME_IN_FORCE)
        .and_then(|f| f.as_str())
        .map(fix_time_in_force)
        .transpose()?
        .unwrap_or(TimeInForce::Day);

    match ord_type {
        "2" => {
            // Limit order.
            let price_raw = required_str(message, tags::PRICE)?;
            let price = price_to_ticks(price_raw, tick_size)?;
            book.add_limit_order(order_id, price, qty, side, time_in_force, None)?;
        }
        "1" => {
            // Market order. time_in_force is irrelevant for a market order;
            // the book only takes qty + side.
            book.submit_market_order(order_id, qty, side)?;
        }
        other => {
            return Err(FixBridgeError::UnsupportedMsgType(format!(
                "OrdType {other}"
            )));
        }
    }

    Ok(FixOutcome::Placed { order_id })
}

/// Cancel by `OrigClOrdID` (tag 41).
fn apply_order_cancel<T>(
    book: &OrderBook<T>,
    message: &Message,
) -> Result<FixOutcome, FixBridgeError>
where
    T: Clone + Send + Sync + Default + 'static,
{
    let orig = required_str(message, tags::ORIG_CL_ORD_ID)?;
    let order_id = cl_ord_id_to_id(orig);
    let removed = book.cancel_order(order_id)?;
    match removed {
        Some(_) => Ok(FixOutcome::Cancelled { order_id }),
        None => Err(FixBridgeError::OrderBook(format!(
            "order {order_id} not found"
        ))),
    }
}

/// Cancel-and-replace: cancel the original, then place the replacement.
///
/// `OrigClOrdID` (tag 41) targets the order to cancel; the replacement is
/// read from the same message's `ClOrdID` (tag 11) and order tags.
fn apply_order_cancel_replace<T>(
    book: &OrderBook<T>,
    message: &Message,
    tick_size: u128,
) -> Result<FixOutcome, FixBridgeError>
where
    T: Clone + Send + Sync + Default + 'static,
{
    // Cancel first (tag 41), then route the rest as a fresh NewOrderSingle.
    apply_order_cancel(book, message)?;

    apply_new_order_single(book, message, tick_size)?;
    let new_id = cl_ord_id_to_id(required_str(message, tags::CL_ORD_ID)?);
    Ok(FixOutcome::Replaced { order_id: new_id })
}

fn required_str(message: &Message, tag: u32) -> Result<&str, FixConversionError> {
    message
        .get(tag)
        .and_then(|f| f.as_str())
        .ok_or(FixConversionError::MissingField(tag))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orderbook::book::OrderBook;
    use fix_codec::{Message as FixMessage, tags as fix_tags};

    fn new_book() -> OrderBook<()> {
        let mut book = OrderBook::new("TEST");
        book.set_order_state_tracker(crate::orderbook::order_state::OrderStateTracker::new());
        book
    }

    fn new_order_msg(side: &str, qty: &str, ord_type: &str, price: &str) -> FixMessage {
        let mut m = FixMessage::new();
        m.push(fix_tags::BEGIN_STRING, "FIX.4.4");
        m.push(MSG_TYPE, "D");
        m.push(tags::CL_ORD_ID, "ORD-1");
        m.push(tags::SIDE, side);
        m.push(tags::ORDER_QTY, qty);
        m.push(tags::ORD_TYPE, ord_type);
        m.push(tags::PRICE, price);
        m.push(tags::TIME_IN_FORCE, "1"); // GTC
        m
    }

    #[test]
    fn places_limit_order() {
        let book = new_book();
        let msg = new_order_msg("1", "100", "2", "100.5");
        let outcome = apply_fix_message(&book, &msg, 10).unwrap();
        match outcome {
            FixOutcome::Placed { order_id } => {
                let status = book.order_status(order_id);
                assert!(status.is_some());
            }
            _ => panic!("expected Placed"),
        }
    }

    #[test]
    fn places_market_order() {
        let book = new_book();
        // Seed a resting sell so the market buy has a counterparty. The seed
        // must carry its own ClOrdID (the helper defaults to "ORD-1", which
        // the market order would otherwise collide with).
        let mut seed = new_order_msg("2", "50", "2", "100"); // sell 50 @ 100
        seed.set(tags::CL_ORD_ID, "ORD-SELL");
        let seed_outcome = apply_fix_message(&book, &seed, 1);
        assert!(
            seed_outcome.is_ok(),
            "seed sell should rest, got {:?}",
            seed_outcome
        );

        let mut msg = new_order_msg("1", "50", "1", "0"); // buy 50 market
        msg.set(tags::CL_ORD_ID, "ORD-MKT");
        let outcome = apply_fix_message(&book, &msg, 1).unwrap();
        assert!(matches!(outcome, FixOutcome::Placed { .. }));
    }

    #[test]
    fn cancels_existing_order() {
        let book = new_book();
        let msg = new_order_msg("1", "100", "2", "100");
        apply_fix_message(&book, &msg, 1).unwrap();

        let mut cancel = FixMessage::new();
        cancel.push(fix_tags::BEGIN_STRING, "FIX.4.4");
        cancel.push(MSG_TYPE, "F");
        cancel.push(tags::ORIG_CL_ORD_ID, "ORD-1");

        let outcome = apply_fix_message(&book, &cancel, 1).unwrap();
        assert!(matches!(outcome, FixOutcome::Cancelled { .. }));
    }

    #[test]
    fn cancel_missing_order_errors() {
        let book = new_book();
        let mut cancel = FixMessage::new();
        cancel.push(MSG_TYPE, "F");
        cancel.push(tags::ORIG_CL_ORD_ID, "DOES-NOT-EXIST");
        assert!(apply_fix_message(&book, &cancel, 1).is_err());
    }

    #[test]
    fn cancel_replace_updates_price() {
        let book = new_book();
        let msg = new_order_msg("1", "100", "2", "100");
        apply_fix_message(&book, &msg, 1).unwrap();

        let mut replace = FixMessage::new();
        replace.push(fix_tags::BEGIN_STRING, "FIX.4.4");
        replace.push(MSG_TYPE, "G");
        replace.push(tags::ORIG_CL_ORD_ID, "ORD-1");
        replace.push(tags::CL_ORD_ID, "ORD-2");
        replace.push(tags::SIDE, "1");
        replace.push(tags::ORDER_QTY, "100");
        replace.push(tags::ORD_TYPE, "2");
        replace.push(tags::PRICE, "101");
        replace.push(tags::TIME_IN_FORCE, "1");

        let outcome = apply_fix_message(&book, &replace, 1).unwrap();
        assert!(matches!(outcome, FixOutcome::Replaced { .. }));

        // Original is cancelled (terminal state retained by the tracker);
        // the replacement is resting.
        let orig = cl_ord_id_to_id("ORD-1");
        assert!(matches!(
            book.order_status(orig),
            Some(crate::orderbook::order_state::OrderStatus::Cancelled { .. })
        ));
        let new_id = cl_ord_id_to_id("ORD-2");
        assert!(matches!(
            book.order_status(new_id),
            Some(crate::orderbook::order_state::OrderStatus::Open)
        ));
    }

    #[test]
    fn unsupported_msg_type_errors() {
        let book = new_book();
        let mut msg = FixMessage::new();
        msg.push(MSG_TYPE, "8"); // ExecutionReport — not an order instruction
        assert!(apply_fix_message(&book, &msg, 1).is_err());
    }

    #[test]
    fn missing_required_field_errors() {
        let book = new_book();
        let mut msg = FixMessage::new();
        msg.push(MSG_TYPE, "D");
        msg.push(tags::CL_ORD_ID, "ORD-1");
        // No Side → MissingField(54)
        assert!(matches!(
            apply_fix_message(&book, &msg, 1),
            Err(FixBridgeError::Conversion(FixConversionError::MissingField(
                tags::SIDE
            )))
        ));
    }
}
