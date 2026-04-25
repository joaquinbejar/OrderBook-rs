//! `CancelOrder` inbound message.
//!
//! See `doc/wire-protocol.md` for the canonical layout.

use crate::wire::error::WireError;
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

/// Inbound `CancelOrder` packed wire layout.
///
/// All fields are little-endian primitives. Total size: **24 bytes**.
///
/// | Offset | Size | Field        | Type | Notes                      |
/// |-------:|-----:|--------------|------|----------------------------|
/// |      0 |    8 | `client_ts`  | u64  | client-side timestamp (ms) |
/// |      8 |    8 | `order_id`   | u64  | order id to cancel         |
/// |     16 |    8 | `account_id` | u64  | numeric account id         |
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout,
)]
#[repr(C, packed)]
pub struct CancelOrderWire {
    /// Client-supplied timestamp (milliseconds since the Unix epoch).
    pub client_ts: u64,
    /// Order id to cancel.
    pub order_id: u64,
    /// Numeric account identifier supplied by the client.
    pub account_id: u64,
}

const _: () = assert!(core::mem::size_of::<CancelOrderWire>() == 24);

impl CancelOrderWire {
    /// Returns the packed byte representation of `self`.
    #[must_use]
    #[inline]
    pub fn as_payload_bytes(&self) -> &[u8] {
        <Self as zerocopy::IntoBytes>::as_bytes(self)
    }
}

/// Decodes a `CancelOrder` payload (24 bytes).
///
/// # Errors
///
/// Returns [`WireError::InvalidPayload`] when the buffer length differs from
/// 24 bytes.
#[inline]
pub fn decode_cancel_order(payload: &[u8]) -> Result<CancelOrderWire, WireError> {
    let view = CancelOrderWire::ref_from_bytes(payload)
        .map_err(|_| WireError::InvalidPayload("CancelOrder: payload size mismatch"))?;
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
        ) {
            let original = CancelOrderWire { client_ts, order_id, account_id };
            let mut framed = Vec::new();
            encode_frame(0x02, original.as_bytes(), &mut framed).expect("encode_frame");

            let (kind, payload, _) = decode_frame(&framed).expect("decode_frame");
            prop_assert_eq!(kind, 0x02u8);
            let decoded = decode_cancel_order(payload).expect("decode_cancel_order");
            prop_assert_eq!({ decoded.client_ts }, client_ts);
            prop_assert_eq!({ decoded.order_id }, order_id);
            prop_assert_eq!({ decoded.account_id }, account_id);
        }
    }

    #[test]
    fn rejects_wrong_size() {
        let buf = [0u8; 23];
        assert!(matches!(
            decode_cancel_order(&buf),
            Err(WireError::InvalidPayload(_))
        ));
        let buf = [0u8; 25];
        assert!(matches!(
            decode_cancel_order(&buf),
            Err(WireError::InvalidPayload(_))
        ));
    }
}
