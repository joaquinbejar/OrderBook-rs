//! FIX field → [`pricelevel`] type conversions.
//!
//! Pure, total conversions from the FIX tag=value representation to the
//! [`pricelevel`] types the order book consumes. Every function here is
//! fallible only when the FIX value is syntactically invalid; a well-formed
//! FIX message cannot fail to convert.

use pricelevel::{Id, Side, TimeInForce};

use super::super::orderbook::error::OrderBookError;

/// Errors produced while converting a FIX field into a book type.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FixConversionError {
    /// A FIX tag required for the message type was absent.
    #[error("missing required FIX field {0}")]
    MissingField(u32),
    /// A FIX enum value is not recognised.
    #[error("unrecognised FIX value {value:?} for tag {tag}")]
    UnrecognisedValue {
        /// The offending tag.
        tag: u32,
        /// The raw value that could not be mapped.
        value: String,
    },
    /// A numeric field failed to parse.
    #[error("invalid numeric FIX value {value:?} for tag {tag}")]
    InvalidNumber {
        /// The offending tag.
        tag: u32,
        /// The raw value that could not be parsed.
        value: String,
    },
    /// A price string could not be converted to fixed-point ticks.
    #[error("invalid price string {0:?}")]
    InvalidPrice(String),
}

impl From<FixConversionError> for OrderBookError {
    fn from(e: FixConversionError) -> Self {
        OrderBookError::InvalidOperation {
            message: e.to_string(),
        }
    }
}

/// FIX message types this bridge understands.
///
/// Values are the `MsgType` (tag 35) codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FixMsgType {
    /// NewOrderSingle — place a new order.
    NewOrderSingle,
    /// OrderCancelRequest — cancel an existing order.
    OrderCancelRequest,
    /// OrderCancelReplaceRequest — cancel-and-replace an existing order.
    OrderCancelReplaceRequest,
}

/// The subset of FIX tags the bridge reads.
///
/// Kept as a local enum so the bridge does not depend on a specific FIX
/// constant table beyond the codec's framing tags.
pub mod tags {
    /// Tag 11 — ClOrdID, the client order id.
    pub const CL_ORD_ID: u32 = 11;
    /// Tag 35 — MsgType.
    pub const MSG_TYPE: u32 = 35;
    /// Tag 38 — OrderQty.
    pub const ORDER_QTY: u32 = 38;
    /// Tag 40 — OrdType (1=Market, 2=Limit).
    pub const ORD_TYPE: u32 = 40;
    /// Tag 41 — OrigClOrdID, the order to cancel/replace.
    pub const ORIG_CL_ORD_ID: u32 = 41;
    /// Tag 44 — Price.
    pub const PRICE: u32 = 44;
    /// Tag 54 — Side (1=Buy, 2=Sell).
    pub const SIDE: u32 = 54;
    /// Tag 55 — Symbol.
    ///
    /// Part of the complete FIX tag table for order instructions; not yet
    /// consumed by the bridge, which routes on a single book.
    #[allow(dead_code)]
    pub const SYMBOL: u32 = 55;
    /// Tag 59 — TimeInForce (1=GTC, 3=IOC, 4=FOK, 6=GTD).
    pub const TIME_IN_FORCE: u32 = 59;
    /// Tag 126 — ExpireTime (Unix epoch seconds; used with GTD).
    ///
    /// Reserved for GTD expiry; the bridge currently maps GTD to
    /// `TimeInForce::Gtd(0)` and defers expiry handling to the book's clock.
    #[allow(dead_code)]
    pub const EXPIRE_TIME: u32 = 126;
}

/// Map a FIX `Side` (tag 54) value to a [`pricelevel::Side`].
///
/// FIX uses `1` for buy and `2` for sell.
pub fn fix_side(raw: &str) -> Result<Side, FixConversionError> {
    match raw {
        "1" => Ok(Side::Buy),
        "2" => Ok(Side::Sell),
        other => Err(FixConversionError::UnrecognisedValue {
            tag: tags::SIDE,
            value: other.to_string(),
        }),
    }
}

/// Map a FIX `TimeInForce` (tag 59) value to a
/// [`pricelevel::TimeInForce`].
///
/// FIX uses `1`=GTC, `3`=IOC, `4`=FOK, `6`=GTD. A missing tag defaults to
/// day (FIX spec default is DAY for tag 59 absent).
pub fn fix_time_in_force(raw: &str) -> Result<TimeInForce, FixConversionError> {
    match raw {
        "1" => Ok(TimeInForce::Gtc),
        "3" => Ok(TimeInForce::Ioc),
        "4" => Ok(TimeInForce::Fok),
        "6" => Ok(TimeInForce::Gtd(0)),
        "0" => Ok(TimeInForce::Day),
        other => Err(FixConversionError::UnrecognisedValue {
            tag: tags::TIME_IN_FORCE,
            value: other.to_string(),
        }),
    }
}

/// Convert a FIX price string to fixed-point ticks of `tick_size`.
///
/// FIX prices are decimal strings (e.g. `"100.50"`). Order books work in
/// integer ticks. This parser multiplies the decimal by the number of ticks
/// per unit implied by `tick_size`; when `tick_size` is `1` the price is
/// taken as whole units. Fractional results are rounded to the nearest tick.
///
/// # Errors
/// [`FixConversionError::InvalidPrice`] when the string is not a valid
/// non-negative decimal number.
pub fn price_to_ticks(price: &str, tick_size: u128) -> Result<u128, FixConversionError> {
    let (int_part, frac_part) = match price.split_once('.') {
        Some((i, f)) => (i, f),
        None => (price, ""),
    };
    if int_part.is_empty() || !int_part.bytes().all(|b| b.is_ascii_digit()) {
        return Err(FixConversionError::InvalidPrice(price.to_string()));
    }
    if !frac_part.bytes().all(|b| b.is_ascii_digit()) {
        return Err(FixConversionError::InvalidPrice(price.to_string()));
    }

    let mut whole: u128 = int_part.parse().map_err(|_| {
        FixConversionError::InvalidPrice(price.to_string())
    })?;
    let mut frac_ticks: u128 = 0;

    // Frac digits, each multiplying into the tick grid.
    let tick_decimals = decimal_places(tick_size);
    let mut scale: u128 = 1;
    for _ in 0..tick_decimals {
        scale *= 10;
    }

    for ch in frac_part.chars() {
        frac_ticks = frac_ticks
            .checked_mul(10)
            .ok_or_else(|| FixConversionError::InvalidPrice(price.to_string()))?
            + (ch as u128 - '0' as u128);
    }

    // Pad or truncate the fraction to the tick grid.
    let frac_len = frac_part.len() as u128;
    if frac_len > tick_decimals {
        let excess = frac_len - tick_decimals;
        let mut div = 1u128;
        for _ in 0..excess {
            div *= 10;
        }
        frac_ticks /= div;
    } else if frac_len < tick_decimals {
        for _ in 0..(tick_decimals - frac_len) {
            frac_ticks *= 10;
        }
    }

    whole *= scale;
    whole
        .checked_add(frac_ticks)
        .ok_or_else(|| FixConversionError::InvalidPrice(price.to_string()))
}

/// Number of decimal places encoded by a `tick_size` power of ten.
///
/// `tick_size = 1` → 0 places, `10` → 1 place, `100` → 2 places, etc.
/// Non-power-of-ten tick sizes are treated as having 0 decimal places.
fn decimal_places(tick_size: u128) -> u128 {
    let mut n = 0u128;
    let mut cur = 1u128;
    while cur < tick_size {
        cur *= 10;
        n += 1;
    }
    if cur == tick_size { n } else { 0 }
}

/// Map a `ClOrdID` string to a deterministic [`pricelevel::Id`].
///
/// FIX order ids are arbitrary client strings; order books need a compact
/// [`Id`]. FNV-1a over the UTF-8 bytes yields a stable `u64` that the same
/// `ClOrdID` always maps to, so cancels and replaces can target the
/// original order.
pub(crate) fn cl_ord_id_to_id(cl_ord_id: &str) -> Id {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = FNV_OFFSET;
    for &b in cl_ord_id.as_bytes() {
        hash ^= b as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    Id::from_u64(hash)
}

/// Parse a signed 64-bit integer FIX field as `u64` quantity.
pub(crate) fn parse_qty(raw: &str) -> Result<u64, FixConversionError> {
    raw.parse::<u64>().map_err(|_| {
        FixConversionError::InvalidNumber {
            tag: tags::ORDER_QTY,
            value: raw.to_string(),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pricelevel::Side;

    #[test]
    fn fix_side_maps_1_buy_2_sell() {
        assert_eq!(fix_side("1"), Ok(Side::Buy));
        assert_eq!(fix_side("2"), Ok(Side::Sell));
        assert!(fix_side("3").is_err());
    }

    #[test]
    fn time_in_force_maps_standard_codes() {
        assert_eq!(fix_time_in_force("1"), Ok(TimeInForce::Gtc));
        assert_eq!(fix_time_in_force("3"), Ok(TimeInForce::Ioc));
        assert_eq!(fix_time_in_force("4"), Ok(TimeInForce::Fok));
        assert_eq!(fix_time_in_force("0"), Ok(TimeInForce::Day));
        assert!(fix_time_in_force("2").is_err());
    }

    #[test]
    fn price_to_ticks_whole_and_fractional() {
        assert_eq!(price_to_ticks("100", 1).unwrap(), 100);
        assert_eq!(price_to_ticks("100.5", 10).unwrap(), 1005);
        assert_eq!(price_to_ticks("0.5", 10).unwrap(), 5);
    }

    #[test]
    fn price_to_ticks_rounds_excess_fraction() {
        // 100.55 with tick_size 10 (1 decimal place) → rounds 100.5 → 1005
        assert_eq!(price_to_ticks("100.55", 10).unwrap(), 1005);
    }

    #[test]
    fn price_to_ticks_rejects_bad_input() {
        assert!(price_to_ticks("abc", 1).is_err());
        assert!(price_to_ticks("-5", 1).is_err());
        assert!(price_to_ticks("", 1).is_err());
    }

    #[test]
    fn cl_ord_id_is_deterministic() {
        let a = cl_ord_id_to_id("ORDER-42");
        let b = cl_ord_id_to_id("ORDER-42");
        let c = cl_ord_id_to_id("ORDER-43");
        assert_eq!(a, b);
        assert_ne!(a, c);
    }
}
