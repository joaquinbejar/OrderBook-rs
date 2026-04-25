//! `ExecReport` outbound message.
//!
//! Outbound encoders use an explicit byte-cursor (`Vec<u8>::extend_from_slice`)
//! rather than `#[repr(C, packed)]`. This is I/O-dominated traffic — the cost
//! of a few dozen bytes of explicit copying is dwarfed by socket overhead, and
//! we get freedom to evolve the layout without exposing a packed type.
//!
//! See `doc/wire-protocol.md` for the canonical layout.

use crate::orderbook::order_state::OrderStatus;
use crate::wire::error::WireError;

/// Wire code for `OrderStatus::Open`.
pub const STATUS_OPEN: u8 = 0;
/// Wire code for `OrderStatus::PartiallyFilled`.
pub const STATUS_PARTIALLY_FILLED: u8 = 1;
/// Wire code for `OrderStatus::Filled`.
pub const STATUS_FILLED: u8 = 2;
/// Wire code for `OrderStatus::Cancelled`.
pub const STATUS_CANCELLED: u8 = 3;
/// Wire code for `OrderStatus::Rejected`.
pub const STATUS_REJECTED: u8 = 4;

/// Fixed payload size in bytes for an `ExecReport`.
pub const EXEC_REPORT_SIZE: usize = 44;

/// Outbound `ExecReport` message body.
///
/// Total payload size: **44 bytes**.
///
/// | Offset | Size | Field            | Type | Notes                            |
/// |-------:|-----:|------------------|------|----------------------------------|
/// |      0 |    8 | `engine_seq`     | u64  | global engine sequence           |
/// |      8 |    8 | `order_id`       | u64  | order id                         |
/// |     16 |    1 | `status`         | u8   | see `STATUS_*` constants          |
/// |     17 |    8 | `filled_qty`     | u64  | cumulative filled quantity       |
/// |     25 |    8 | `remaining_qty`  | u64  | quantity still resting           |
/// |     33 |    8 | `price`          | i64  | tick-scaled price                |
/// |     41 |    2 | `reject_reason`  | u16  | reject code, `0` if not rejected |
/// |     43 |    1 | `_pad`           | u8   | reserved, must be zero           |
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecReport {
    /// Global engine sequence (monotonic across outbound streams).
    pub engine_seq: u64,
    /// Order id.
    pub order_id: u64,
    /// Status discriminant — see `STATUS_*` constants.
    pub status: u8,
    /// Cumulative filled quantity for this order.
    pub filled_qty: u64,
    /// Quantity still resting on the book.
    pub remaining_qty: u64,
    /// Tick-scaled price.
    pub price: i64,
    /// Numeric reject code. `0` when the report is not a rejection.
    pub reject_reason: u16,
    /// Reserved. Must be zero.
    pub _pad: u8,
}

/// Maps an [`OrderStatus`] to its wire-side discriminant.
///
/// The mapping is stable across `0.7.x` patch releases.
#[must_use]
#[inline]
pub fn status_to_wire(status: &OrderStatus) -> u8 {
    match status {
        OrderStatus::Open => STATUS_OPEN,
        OrderStatus::PartiallyFilled { .. } => STATUS_PARTIALLY_FILLED,
        OrderStatus::Filled { .. } => STATUS_FILLED,
        OrderStatus::Cancelled { .. } => STATUS_CANCELLED,
        OrderStatus::Rejected { .. } => STATUS_REJECTED,
    }
}

/// Encodes an `ExecReport` payload (44 bytes) into `out`.
#[inline]
pub fn encode_exec_report(report: &ExecReport, out: &mut Vec<u8>) {
    out.reserve(EXEC_REPORT_SIZE);
    out.extend_from_slice(&report.engine_seq.to_le_bytes());
    out.extend_from_slice(&report.order_id.to_le_bytes());
    out.push(report.status);
    out.extend_from_slice(&report.filled_qty.to_le_bytes());
    out.extend_from_slice(&report.remaining_qty.to_le_bytes());
    out.extend_from_slice(&report.price.to_le_bytes());
    out.extend_from_slice(&report.reject_reason.to_le_bytes());
    out.push(report._pad);
}

/// Decodes an `ExecReport` payload.
///
/// # Errors
///
/// Returns [`WireError::InvalidPayload`] when the buffer length differs from
/// [`EXEC_REPORT_SIZE`].
#[inline]
pub fn decode_exec_report(payload: &[u8]) -> Result<ExecReport, WireError> {
    if payload.len() != EXEC_REPORT_SIZE {
        return Err(WireError::InvalidPayload(
            "ExecReport: payload size mismatch",
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
    let read_u16 = |offset: usize| -> Result<u16, WireError> {
        let slot = payload
            .get(offset..offset + 2)
            .ok_or(WireError::Truncated)?;
        let mut arr = [0u8; 2];
        arr.copy_from_slice(slot);
        Ok(u16::from_le_bytes(arr))
    };

    let engine_seq = read_u64(0)?;
    let order_id = read_u64(8)?;
    let status = *payload.get(16).ok_or(WireError::Truncated)?;
    let filled_qty = read_u64(17)?;
    let remaining_qty = read_u64(25)?;
    let price = read_i64(33)?;
    let reject_reason = read_u16(41)?;
    let pad = *payload.get(43).ok_or(WireError::Truncated)?;
    Ok(ExecReport {
        engine_seq,
        order_id,
        status,
        filled_qty,
        remaining_qty,
        price,
        reject_reason,
        _pad: pad,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orderbook::reject_reason::RejectReason;
    use crate::wire::framing::{decode_frame, encode_frame};
    use proptest::prelude::*;

    #[test]
    fn payload_size_constant() {
        let report = ExecReport {
            engine_seq: 0,
            order_id: 0,
            status: STATUS_OPEN,
            filled_qty: 0,
            remaining_qty: 0,
            price: 0,
            reject_reason: 0,
            _pad: 0,
        };
        let mut buf = Vec::new();
        encode_exec_report(&report, &mut buf);
        assert_eq!(buf.len(), EXEC_REPORT_SIZE);
    }

    #[test]
    fn status_to_wire_covers_all_variants() {
        assert_eq!(status_to_wire(&OrderStatus::Open), STATUS_OPEN);
        assert_eq!(
            status_to_wire(&OrderStatus::PartiallyFilled {
                original_quantity: 10,
                filled_quantity: 4
            }),
            STATUS_PARTIALLY_FILLED
        );
        assert_eq!(
            status_to_wire(&OrderStatus::Filled {
                filled_quantity: 10
            }),
            STATUS_FILLED
        );
        assert_eq!(
            status_to_wire(&OrderStatus::Cancelled {
                filled_quantity: 0,
                reason: crate::orderbook::order_state::CancelReason::UserRequested,
            }),
            STATUS_CANCELLED
        );
        assert_eq!(
            status_to_wire(&OrderStatus::Rejected {
                reason: RejectReason::KillSwitchActive
            }),
            STATUS_REJECTED
        );
    }

    proptest! {
        #[test]
        fn roundtrip_through_frame(
            engine_seq in any::<u64>(),
            order_id in any::<u64>(),
            status in 0u8..=4u8,
            filled_qty in any::<u64>(),
            remaining_qty in any::<u64>(),
            price in any::<i64>(),
            reject_reason in any::<u16>(),
        ) {
            let original = ExecReport {
                engine_seq,
                order_id,
                status,
                filled_qty,
                remaining_qty,
                price,
                reject_reason,
                _pad: 0,
            };
            let mut payload = Vec::new();
            encode_exec_report(&original, &mut payload);
            let mut framed = Vec::new();
            encode_frame(0x81, &payload, &mut framed).expect("encode_frame");

            let (kind, decoded_payload, _) = decode_frame(&framed).expect("decode_frame");
            prop_assert_eq!(kind, 0x81u8);
            let decoded = decode_exec_report(decoded_payload).expect("decode_exec_report");
            prop_assert_eq!(decoded, original);
        }
    }

    #[test]
    fn rejects_short_payload() {
        let buf = [0u8; EXEC_REPORT_SIZE - 1];
        assert!(matches!(
            decode_exec_report(&buf),
            Err(WireError::InvalidPayload(_))
        ));
    }
}
