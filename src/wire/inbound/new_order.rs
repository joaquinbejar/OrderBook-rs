//! `NewOrder` inbound message.
//!
//! See `doc/wire-protocol.md` for the canonical layout.

use crate::wire::error::WireError;
use pricelevel::{Hash32, Id, OrderType, Price, Quantity, Side, TimeInForce, TimestampMs};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

/// Wire codes for the `side` field of [`NewOrderWire`].
pub const SIDE_BUY: u8 = 0;
/// Wire codes for the `side` field of [`NewOrderWire`].
pub const SIDE_SELL: u8 = 1;

/// Wire codes for the `time_in_force` field.
pub const TIF_GTC: u8 = 0;
/// Wire codes for the `time_in_force` field.
pub const TIF_IOC: u8 = 1;
/// Wire codes for the `time_in_force` field.
pub const TIF_FOK: u8 = 2;
/// Wire codes for the `time_in_force` field.
pub const TIF_DAY: u8 = 3;

/// Wire codes for the `order_type` field.
pub const ORDER_TYPE_STANDARD: u8 = 0;

/// Inbound `NewOrder` packed wire layout.
///
/// All fields are little-endian primitives. Total size: **48 bytes**.
///
/// | Offset | Size | Field           | Type | Notes                          |
/// |-------:|-----:|-----------------|------|--------------------------------|
/// |      0 |    8 | `client_ts`     | u64  | client-side timestamp (ms)     |
/// |      8 |    8 | `order_id`      | u64  | unique order id                |
/// |     16 |    8 | `account_id`    | u64  | numeric account id             |
/// |     24 |    8 | `price`         | i64  | tick-scaled limit price        |
/// |     32 |    8 | `qty`           | u64  | quantity                       |
/// |     40 |    1 | `side`          | u8   | `0` Buy, `1` Sell              |
/// |     41 |    1 | `time_in_force` | u8   | `0` GTC, `1` IOC, `2` FOK, `3` DAY |
/// |     42 |    1 | `order_type`    | u8   | `0` Standard (only one in MVP) |
/// |     43 |    5 | `_pad`          | u8×5 | reserved, must be zero         |
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout,
)]
#[repr(C, packed)]
pub struct NewOrderWire {
    /// Client-supplied timestamp (milliseconds since the Unix epoch).
    pub client_ts: u64,
    /// Unique order identifier supplied by the client.
    pub order_id: u64,
    /// Numeric account identifier supplied by the client.
    pub account_id: u64,
    /// Tick-scaled limit price.
    pub price: i64,
    /// Order quantity.
    pub qty: u64,
    /// Side: `0` = Buy, `1` = Sell.
    pub side: u8,
    /// Time-in-force: `0` GTC / `1` IOC / `2` FOK / `3` DAY.
    pub time_in_force: u8,
    /// Order type: `0` Standard (only Standard is supported in the MVP).
    pub order_type: u8,
    /// Reserved padding. Must be zero.
    pub _pad: [u8; 5],
}

const _: () = assert!(core::mem::size_of::<NewOrderWire>() == 48);

impl NewOrderWire {
    /// Returns the packed byte representation of `self`.
    ///
    /// Equivalent to `<Self as zerocopy::IntoBytes>::as_bytes(&self)` but
    /// callable without importing `zerocopy` at the call site.
    #[must_use]
    #[inline]
    pub fn as_payload_bytes(&self) -> &[u8] {
        <Self as zerocopy::IntoBytes>::as_bytes(self)
    }
}

/// Decodes a `NewOrder` payload (48 bytes).
///
/// # Errors
///
/// Returns [`WireError::InvalidPayload`] when the buffer length differs from
/// 48 bytes.
#[inline]
pub fn decode_new_order(payload: &[u8]) -> Result<NewOrderWire, WireError> {
    let view = NewOrderWire::ref_from_bytes(payload)
        .map_err(|_| WireError::InvalidPayload("NewOrder: payload size mismatch"))?;
    Ok(*view)
}

impl TryFrom<&NewOrderWire> for OrderType<()> {
    type Error = WireError;

    fn try_from(value: &NewOrderWire) -> Result<Self, Self::Error> {
        // Copy each packed field into a local first — taking a reference to a
        // packed field is undefined behavior. The `{ value.field }` syntax
        // forces a copy.
        let order_id = { value.order_id };
        let account_id = { value.account_id };
        let client_ts = { value.client_ts };
        let price_raw = { value.price };
        let qty = { value.qty };
        let side_byte = { value.side };
        let tif_byte = { value.time_in_force };
        let kind_byte = { value.order_type };

        if price_raw < 0 {
            return Err(WireError::InvalidPayload("NewOrder: negative price"));
        }

        let side = match side_byte {
            SIDE_BUY => Side::Buy,
            SIDE_SELL => Side::Sell,
            _ => return Err(WireError::InvalidPayload("NewOrder: unknown side")),
        };
        let time_in_force = match tif_byte {
            TIF_GTC => TimeInForce::Gtc,
            TIF_IOC => TimeInForce::Ioc,
            TIF_FOK => TimeInForce::Fok,
            TIF_DAY => TimeInForce::Day,
            _ => {
                return Err(WireError::InvalidPayload("NewOrder: unknown time_in_force"));
            }
        };
        if kind_byte != ORDER_TYPE_STANDARD {
            return Err(WireError::InvalidPayload(
                "NewOrder: unsupported order_type",
            ));
        }

        // Encode the numeric account_id into the high 8 bytes of a Hash32 so
        // it is preserved across the wire/domain boundary without colliding
        // with `Hash32::zero()` (which is the "no STP" sentinel).
        let mut user_bytes = [0u8; 32];
        if let Some(slot) = user_bytes.get_mut(0..8) {
            slot.copy_from_slice(&account_id.to_le_bytes());
        }
        let user_id = Hash32::new(user_bytes);

        Ok(OrderType::Standard {
            id: Id::from_u64(order_id),
            price: Price::new(u128::from(price_raw as u64)),
            quantity: Quantity::new(qty),
            side,
            user_id,
            timestamp: TimestampMs::new(client_ts),
            time_in_force,
            extra_fields: (),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::framing::{decode_frame, encode_frame};
    use proptest::prelude::*;
    use zerocopy::IntoBytes;

    proptest! {
        #[test]
        fn roundtrip_through_frame(
            client_ts in any::<u64>(),
            order_id in any::<u64>(),
            account_id in any::<u64>(),
            price in 0i64..i64::MAX,
            qty in any::<u64>(),
            side in 0u8..=1u8,
            tif in 0u8..=3u8,
        ) {
            let original = NewOrderWire {
                client_ts,
                order_id,
                account_id,
                price,
                qty,
                side,
                time_in_force: tif,
                order_type: ORDER_TYPE_STANDARD,
                _pad: [0u8; 5],
            };

            let mut framed = Vec::new();
            encode_frame(0x01, original.as_bytes(), &mut framed).expect("encode_frame");

            let (kind, payload, used) = decode_frame(&framed).expect("decode_frame");
            prop_assert_eq!(kind, 0x01u8);
            prop_assert_eq!(used, framed.len());

            let decoded = decode_new_order(payload).expect("decode_new_order");
            // Read packed fields via copy.
            prop_assert_eq!({ decoded.client_ts }, client_ts);
            prop_assert_eq!({ decoded.order_id }, order_id);
            prop_assert_eq!({ decoded.account_id }, account_id);
            prop_assert_eq!({ decoded.price }, price);
            prop_assert_eq!({ decoded.qty }, qty);
            prop_assert_eq!({ decoded.side }, side);
            prop_assert_eq!({ decoded.time_in_force }, tif);
            prop_assert_eq!({ decoded.order_type }, ORDER_TYPE_STANDARD);
            prop_assert_eq!({ decoded._pad }, [0u8; 5]);
        }
    }

    #[test]
    fn rejects_short_payload() {
        let buf = [0u8; 47];
        assert!(matches!(
            decode_new_order(&buf),
            Err(WireError::InvalidPayload(_))
        ));
    }

    #[test]
    fn rejects_long_payload() {
        let buf = [0u8; 49];
        assert!(matches!(
            decode_new_order(&buf),
            Err(WireError::InvalidPayload(_))
        ));
    }

    #[test]
    fn try_from_rejects_unknown_side() {
        let wire = NewOrderWire {
            client_ts: 0,
            order_id: 1,
            account_id: 2,
            price: 100,
            qty: 5,
            side: 9,
            time_in_force: TIF_GTC,
            order_type: ORDER_TYPE_STANDARD,
            _pad: [0u8; 5],
        };
        let res: Result<OrderType<()>, _> = (&wire).try_into();
        assert!(matches!(res, Err(WireError::InvalidPayload(_))));
    }

    #[test]
    fn try_from_rejects_negative_price() {
        let wire = NewOrderWire {
            client_ts: 0,
            order_id: 1,
            account_id: 2,
            price: -1,
            qty: 5,
            side: SIDE_BUY,
            time_in_force: TIF_GTC,
            order_type: ORDER_TYPE_STANDARD,
            _pad: [0u8; 5],
        };
        let res: Result<OrderType<()>, _> = (&wire).try_into();
        assert!(matches!(res, Err(WireError::InvalidPayload(_))));
    }

    #[test]
    fn try_from_builds_standard_order() {
        let wire = NewOrderWire {
            client_ts: 1_700_000_000_000,
            order_id: 42,
            account_id: 7,
            price: 9_999,
            qty: 10,
            side: SIDE_SELL,
            time_in_force: TIF_IOC,
            order_type: ORDER_TYPE_STANDARD,
            _pad: [0u8; 5],
        };
        let order: OrderType<()> = (&wire).try_into().expect("convert to OrderType");
        match order {
            OrderType::Standard {
                price,
                quantity,
                side,
                time_in_force,
                ..
            } => {
                assert_eq!(price.as_u128(), 9_999);
                assert_eq!(quantity.as_u64(), 10);
                assert_eq!(side, Side::Sell);
                assert_eq!(time_in_force, TimeInForce::Ioc);
            }
            _ => panic!("expected Standard variant"),
        }
    }
}
