//! `TradePrint` outbound message.
//!
//! See `doc/wire-protocol.md` for the canonical layout.

use crate::wire::error::WireError;

/// Fixed payload size in bytes for a `TradePrintWire`.
pub const TRADE_PRINT_SIZE: usize = 48;

/// Outbound `TradePrint` message body.
///
/// Total payload size: **48 bytes**.
///
/// | Offset | Size | Field         | Type | Notes                        |
/// |-------:|-----:|---------------|------|------------------------------|
/// |      0 |    8 | `engine_seq`  | u64  | global engine sequence       |
/// |      8 |    8 | `maker_id`    | u64  | maker order id               |
/// |     16 |    8 | `taker_id`    | u64  | taker order id               |
/// |     24 |    8 | `price`       | i64  | tick-scaled fill price       |
/// |     32 |    8 | `qty`         | u64  | matched quantity             |
/// |     40 |    8 | `ts`          | u64  | engine timestamp (ms)        |
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TradePrintWire {
    /// Global engine sequence (monotonic across outbound streams).
    pub engine_seq: u64,
    /// Maker order id (the resting side of the match).
    pub maker_id: u64,
    /// Taker order id (the incoming side of the match).
    pub taker_id: u64,
    /// Tick-scaled fill price.
    pub price: i64,
    /// Matched quantity.
    pub qty: u64,
    /// Engine timestamp in milliseconds.
    pub ts: u64,
}

/// Encodes a `TradePrint` payload (48 bytes) into `out`.
#[inline]
pub fn encode_trade_print(trade: &TradePrintWire, out: &mut Vec<u8>) {
    out.reserve(TRADE_PRINT_SIZE);
    out.extend_from_slice(&trade.engine_seq.to_le_bytes());
    out.extend_from_slice(&trade.maker_id.to_le_bytes());
    out.extend_from_slice(&trade.taker_id.to_le_bytes());
    out.extend_from_slice(&trade.price.to_le_bytes());
    out.extend_from_slice(&trade.qty.to_le_bytes());
    out.extend_from_slice(&trade.ts.to_le_bytes());
}

/// Decodes a `TradePrint` payload.
///
/// # Errors
///
/// Returns [`WireError::InvalidPayload`] when the buffer length differs from
/// [`TRADE_PRINT_SIZE`].
#[inline]
pub fn decode_trade_print(payload: &[u8]) -> Result<TradePrintWire, WireError> {
    if payload.len() != TRADE_PRINT_SIZE {
        return Err(WireError::InvalidPayload(
            "TradePrint: payload size mismatch",
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

    Ok(TradePrintWire {
        engine_seq: read_u64(0)?,
        maker_id: read_u64(8)?,
        taker_id: read_u64(16)?,
        price: read_i64(24)?,
        qty: read_u64(32)?,
        ts: read_u64(40)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::framing::{decode_frame, encode_frame};
    use proptest::prelude::*;

    #[test]
    fn payload_size_constant() {
        let trade = TradePrintWire {
            engine_seq: 0,
            maker_id: 0,
            taker_id: 0,
            price: 0,
            qty: 0,
            ts: 0,
        };
        let mut buf = Vec::new();
        encode_trade_print(&trade, &mut buf);
        assert_eq!(buf.len(), TRADE_PRINT_SIZE);
    }

    proptest! {
        #[test]
        fn roundtrip_through_frame(
            engine_seq in any::<u64>(),
            maker_id in any::<u64>(),
            taker_id in any::<u64>(),
            price in any::<i64>(),
            qty in any::<u64>(),
            ts in any::<u64>(),
        ) {
            let original = TradePrintWire {
                engine_seq,
                maker_id,
                taker_id,
                price,
                qty,
                ts,
            };
            let mut payload = Vec::new();
            encode_trade_print(&original, &mut payload);
            let mut framed = Vec::new();
            encode_frame(0x82, &payload, &mut framed).expect("encode_frame");

            let (kind, decoded_payload, _) = decode_frame(&framed).expect("decode_frame");
            prop_assert_eq!(kind, 0x82u8);
            let decoded = decode_trade_print(decoded_payload).expect("decode_trade_print");
            prop_assert_eq!(decoded, original);
        }
    }

    #[test]
    fn rejects_wrong_size() {
        let buf = [0u8; TRADE_PRINT_SIZE - 1];
        assert!(matches!(
            decode_trade_print(&buf),
            Err(WireError::InvalidPayload(_))
        ));
    }
}
