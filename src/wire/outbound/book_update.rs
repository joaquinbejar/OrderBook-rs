//! `BookUpdate` outbound message.
//!
//! See `doc/wire-protocol.md` for the canonical layout.

use crate::wire::error::WireError;

/// Wire codes for the `side` field.
pub const SIDE_BUY: u8 = 0;
/// Wire codes for the `side` field.
pub const SIDE_SELL: u8 = 1;

/// Fixed payload size in bytes for a `BookUpdateWire` (with trailing pad).
pub const BOOK_UPDATE_SIZE: usize = 32;

/// Outbound `BookUpdate` message body.
///
/// Total payload size: **32 bytes** (26 bytes of fields + 6 bytes of trailing
/// pad to round to a 32-byte block — keeps the message a comfortable
/// cache-line slice and leaves room for forward-compatible additions).
///
/// | Offset | Size | Field        | Type | Notes                       |
/// |-------:|-----:|--------------|------|-----------------------------|
/// |      0 |    8 | `engine_seq` | u64  | global engine sequence      |
/// |      8 |    1 | `side`       | u8   | `0` Buy, `1` Sell           |
/// |      9 |    8 | `price`      | i64  | tick-scaled level price     |
/// |     17 |    8 | `qty`        | u64  | new total quantity at level |
/// |     25 |    1 | `_pad0`      | u8   | reserved                    |
/// |     26 |    6 | `_pad`       | u8×6 | reserved, must be zero      |
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BookUpdateWire {
    /// Global engine sequence (monotonic across outbound streams).
    pub engine_seq: u64,
    /// Side of the level: `0` = Buy, `1` = Sell.
    pub side: u8,
    /// Tick-scaled level price.
    pub price: i64,
    /// New total quantity resting at this level (`0` if the level was wiped).
    pub qty: u64,
}

/// Encodes a `BookUpdate` payload (32 bytes) into `out`. The trailing 7-byte
/// pad is zero-filled.
#[inline]
pub fn encode_book_update(update: &BookUpdateWire, out: &mut Vec<u8>) {
    out.reserve(BOOK_UPDATE_SIZE);
    out.extend_from_slice(&update.engine_seq.to_le_bytes());
    out.push(update.side);
    out.extend_from_slice(&update.price.to_le_bytes());
    out.extend_from_slice(&update.qty.to_le_bytes());
    // 7 bytes of trailing pad to round to 32.
    out.extend_from_slice(&[0u8; 7]);
}

/// Decodes a `BookUpdate` payload.
///
/// # Errors
///
/// Returns [`WireError::InvalidPayload`] when the buffer length differs from
/// [`BOOK_UPDATE_SIZE`].
#[inline]
pub fn decode_book_update(payload: &[u8]) -> Result<BookUpdateWire, WireError> {
    if payload.len() != BOOK_UPDATE_SIZE {
        return Err(WireError::InvalidPayload(
            "BookUpdate: payload size mismatch",
        ));
    }
    let read_u64 = |offset: usize| -> Result<u64, WireError> {
        let slot = payload
            .get(offset..offset + 8)
            .ok_or(WireError::Truncated)?;
        let mut arr = [0u8; 8];
        arr.copy_from_slice(slot);
        Ok(u64::from_le_bytes(arr))
    };
    let read_i64 = |offset: usize| -> Result<i64, WireError> {
        let slot = payload
            .get(offset..offset + 8)
            .ok_or(WireError::Truncated)?;
        let mut arr = [0u8; 8];
        arr.copy_from_slice(slot);
        Ok(i64::from_le_bytes(arr))
    };

    let engine_seq = read_u64(0)?;
    let side = *payload.get(8).ok_or(WireError::Truncated)?;
    if side != SIDE_BUY && side != SIDE_SELL {
        return Err(WireError::InvalidPayload("BookUpdate: unknown side"));
    }
    let price = read_i64(9)?;
    let qty = read_u64(17)?;
    let pad = payload.get(25..32).ok_or(WireError::Truncated)?;
    if pad.iter().any(|&byte| byte != 0) {
        return Err(WireError::InvalidPayload(
            "BookUpdate: non-zero reserved padding",
        ));
    }
    Ok(BookUpdateWire {
        engine_seq,
        side,
        price,
        qty,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::framing::{decode_frame, encode_frame};
    use proptest::prelude::*;

    #[test]
    fn payload_size_constant() {
        let upd = BookUpdateWire {
            engine_seq: 0,
            side: SIDE_BUY,
            price: 0,
            qty: 0,
        };
        let mut buf = Vec::new();
        encode_book_update(&upd, &mut buf);
        assert_eq!(buf.len(), BOOK_UPDATE_SIZE);
    }

    proptest! {
        #[test]
        fn roundtrip_through_frame(
            engine_seq in any::<u64>(),
            side in 0u8..=1u8,
            price in any::<i64>(),
            qty in any::<u64>(),
        ) {
            let original = BookUpdateWire {
                engine_seq,
                side,
                price,
                qty,
            };
            let mut payload = Vec::new();
            encode_book_update(&original, &mut payload);
            let mut framed = Vec::new();
            encode_frame(0x83, &payload, &mut framed).expect("encode_frame");

            let (kind, decoded_payload, _) = decode_frame(&framed).expect("decode_frame");
            prop_assert_eq!(kind, 0x83u8);
            let decoded = decode_book_update(decoded_payload).expect("decode_book_update");
            prop_assert_eq!(decoded, original);
        }
    }

    #[test]
    fn rejects_wrong_size() {
        let buf = [0u8; BOOK_UPDATE_SIZE - 1];
        assert!(matches!(
            decode_book_update(&buf),
            Err(WireError::InvalidPayload(_))
        ));
    }
}
