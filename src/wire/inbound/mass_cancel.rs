//! `MassCancel` inbound message.
//!
//! See `doc/wire-protocol.md` for the canonical layout.

use crate::wire::error::WireError;
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

/// Cancel-all scope. Wire code `0x00`.
pub const SCOPE_ALL: u8 = 0;
/// Cancel by account scope. Wire code `0x01`.
pub const SCOPE_BY_ACCOUNT: u8 = 1;
/// Cancel by side scope. Wire code `0x02`. The side itself is encoded in the
/// low bit of `_pad[0]` — `0` = Buy, `1` = Sell.
pub const SCOPE_BY_SIDE: u8 = 2;

/// Inbound `MassCancel` packed wire layout.
///
/// All fields are little-endian primitives. Total size: **24 bytes**.
///
/// | Offset | Size | Field        | Type   | Notes                              |
/// |-------:|-----:|--------------|--------|------------------------------------|
/// |      0 |    8 | `client_ts`  | u64    | client-side timestamp (ms)         |
/// |      8 |    8 | `account_id` | u64    | numeric account id                 |
/// |     16 |    1 | `scope`      | u8     | `0` All, `1` ByAccount, `2` BySide |
/// |     17 |    7 | `_pad`       | u8×7   | for `BySide`, `_pad[0] & 1` = side |
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout,
)]
#[repr(C, packed)]
pub struct MassCancelWire {
    /// Client-supplied timestamp (milliseconds since the Unix epoch).
    pub client_ts: u64,
    /// Numeric account identifier supplied by the client.
    pub account_id: u64,
    /// Cancellation scope: `0` All, `1` ByAccount, `2` BySide.
    pub scope: u8,
    /// Reserved padding. For `scope == BySide`, the low bit of `_pad[0]`
    /// encodes the side (`0` = Buy, `1` = Sell). Other bits must be zero.
    pub _pad: [u8; 7],
}

const _: () = assert!(core::mem::size_of::<MassCancelWire>() == 24);

impl MassCancelWire {
    /// Returns the packed byte representation of `self`.
    #[must_use]
    #[inline]
    pub fn as_payload_bytes(&self) -> &[u8] {
        <Self as zerocopy::IntoBytes>::as_bytes(self)
    }
}

/// Decodes a `MassCancel` payload (24 bytes).
///
/// # Errors
///
/// Returns [`WireError::InvalidPayload`] when the buffer length differs from
/// 24 bytes, when the `scope` byte is outside the documented range, or when
/// reserved padding bits are non-zero (for `BySide` only the low bit of
/// `_pad[0]` is allowed).
#[inline]
pub fn decode_mass_cancel(payload: &[u8]) -> Result<MassCancelWire, WireError> {
    let view = MassCancelWire::ref_from_bytes(payload)
        .map_err(|_| WireError::InvalidPayload("MassCancel: payload size mismatch"))?;
    let scope = { view.scope };
    let pad = { view._pad };
    match scope {
        SCOPE_ALL | SCOPE_BY_ACCOUNT => {
            if pad.iter().any(|&byte| byte != 0) {
                return Err(WireError::InvalidPayload(
                    "MassCancel: non-zero reserved padding",
                ));
            }
        }
        SCOPE_BY_SIDE => {
            // Only the low bit of `_pad[0]` carries the side; every other
            // padding bit must be zero.
            let head = *pad.first().ok_or(WireError::Truncated)?;
            if head & !1 != 0 {
                return Err(WireError::InvalidPayload(
                    "MassCancel: reserved bits set in BySide pad[0]",
                ));
            }
            if pad.iter().skip(1).any(|&byte| byte != 0) {
                return Err(WireError::InvalidPayload(
                    "MassCancel: non-zero reserved padding",
                ));
            }
        }
        _ => return Err(WireError::InvalidPayload("MassCancel: unknown scope")),
    }
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
            account_id in any::<u64>(),
            scope in 0u8..=2u8,
            side_bit in 0u8..=1u8,
        ) {
            let mut pad = [0u8; 7];
            if scope == SCOPE_BY_SIDE
                && let Some(slot) = pad.get_mut(0)
            {
                *slot = side_bit;
            }
            let original = MassCancelWire {
                client_ts,
                account_id,
                scope,
                _pad: pad,
            };

            let mut framed = Vec::new();
            encode_frame(0x04, original.as_bytes(), &mut framed).expect("encode_frame");

            let (kind, payload, _) = decode_frame(&framed).expect("decode_frame");
            prop_assert_eq!(kind, 0x04u8);
            let decoded = decode_mass_cancel(payload).expect("decode_mass_cancel");
            prop_assert_eq!({ decoded.client_ts }, client_ts);
            prop_assert_eq!({ decoded.account_id }, account_id);
            prop_assert_eq!({ decoded.scope }, scope);
            prop_assert_eq!({ decoded._pad }, pad);
        }
    }

    #[test]
    fn rejects_unknown_scope() {
        let bad = MassCancelWire {
            client_ts: 0,
            account_id: 0,
            scope: 9,
            _pad: [0u8; 7],
        };
        let bytes = bad.as_bytes();
        assert!(matches!(
            decode_mass_cancel(bytes),
            Err(WireError::InvalidPayload(_))
        ));
    }

    #[test]
    fn rejects_wrong_size() {
        let buf = [0u8; 23];
        assert!(matches!(
            decode_mass_cancel(&buf),
            Err(WireError::InvalidPayload(_))
        ));
    }
}
