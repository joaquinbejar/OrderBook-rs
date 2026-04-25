//! `CancelReplace` inbound message.
//!
//! See `doc/wire-protocol.md` for the canonical layout.

use crate::wire::error::WireError;
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

/// Inbound `CancelReplace` packed wire layout.
///
/// All fields are little-endian primitives. Total size: **40 bytes**.
///
/// | Offset | Size | Field        | Type | Notes                       |
/// |-------:|-----:|--------------|------|-----------------------------|
/// |      0 |    8 | `client_ts`  | u64  | client-side timestamp (ms)  |
/// |      8 |    8 | `order_id`   | u64  | original order id           |
/// |     16 |    8 | `account_id` | u64  | numeric account id          |
/// |     24 |    8 | `new_price`  | i64  | replacement limit price     |
/// |     32 |    8 | `new_qty`    | u64  | replacement quantity        |
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout,
)]
#[repr(C, packed)]
pub struct CancelReplaceWire {
    /// Client-supplied timestamp (milliseconds since the Unix epoch).
    pub client_ts: u64,
    /// Original order id to replace.
    pub order_id: u64,
    /// Numeric account identifier supplied by the client.
    pub account_id: u64,
    /// Replacement limit price (tick-scaled).
    pub new_price: i64,
    /// Replacement quantity.
    pub new_qty: u64,
}

const _: () = assert!(core::mem::size_of::<CancelReplaceWire>() == 40);

impl CancelReplaceWire {
    /// Returns the packed byte representation of `self`.
    #[must_use]
    #[inline]
    pub fn as_payload_bytes(&self) -> &[u8] {
        <Self as zerocopy::IntoBytes>::as_bytes(self)
    }
}

/// Decodes a `CancelReplace` payload (40 bytes).
///
/// # Errors
///
/// Returns [`WireError::InvalidPayload`] when the buffer length differs from
/// 40 bytes.
#[inline]
pub fn decode_cancel_replace(payload: &[u8]) -> Result<CancelReplaceWire, WireError> {
    let view = CancelReplaceWire::ref_from_bytes(payload)
        .map_err(|_| WireError::InvalidPayload("CancelReplace: payload size mismatch"))?;
    Ok(*view)
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
            new_price in any::<i64>(),
            new_qty in any::<u64>(),
        ) {
            let original = CancelReplaceWire {
                client_ts,
                order_id,
                account_id,
                new_price,
                new_qty,
            };
            let mut framed = Vec::new();
            encode_frame(0x03, original.as_bytes(), &mut framed).expect("encode_frame");

            let (kind, payload, _) = decode_frame(&framed).expect("decode_frame");
            prop_assert_eq!(kind, 0x03u8);
            let decoded = decode_cancel_replace(payload).expect("decode_cancel_replace");
            prop_assert_eq!({ decoded.client_ts }, client_ts);
            prop_assert_eq!({ decoded.order_id }, order_id);
            prop_assert_eq!({ decoded.account_id }, account_id);
            prop_assert_eq!({ decoded.new_price }, new_price);
            prop_assert_eq!({ decoded.new_qty }, new_qty);
        }
    }

    #[test]
    fn rejects_wrong_size() {
        let buf = [0u8; 39];
        assert!(matches!(
            decode_cancel_replace(&buf),
            Err(WireError::InvalidPayload(_))
        ));
    }
}
