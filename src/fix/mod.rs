//! FIX protocol bridge — map FIX messages onto [`OrderBook`](crate::OrderBook)
//! instructions.
//!
//! This module connects a wire-level FIX codec to the matching engine. It is
//! deliberately protocol-only: [`fix_codec::decode`] turns wire bytes into a
//! [`fix_codec::Message`], and this bridge turns that message into an order
//! book instruction. No session state, sequence numbers, or transport live
//! here — that is the caller's responsibility (e.g. a `fix-session` layer).
//!
//! Supported message types:
//!
//! - `D` (NewOrderSingle) — limit, market, post-only and iceberg orders.
//! - `F` (OrderCancelRequest) — cancel by `OrigClOrdID` (tag 41).
//! - `G` (OrderCancelReplaceRequest) — cancel-and-replace by `OrigClOrdID`
//!   (tag 41), re-adding the new order.
//!
//! `ClOrdID` (tag 11) is mapped deterministically to an
//! [`Id`](pricelevel::Id) via FNV-1a: the same `ClOrdID` always yields the
//! same order id, so cancels and replaces can target the original order.
//!
//! Feature `fix` is required.
//!
//! License: MIT.

mod convert;
mod engine;

pub use convert::{
    FixConversionError, FixMsgType, fix_side, fix_time_in_force, price_to_ticks,
};
pub use engine::{FixBridgeError, apply_fix_message};
