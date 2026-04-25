//! Feature-gated binary wire protocol.
//!
//! Enabled via `--features wire`. The protocol is **additive** — `JSON`
//! and `bincode` paths are unchanged; existing callers see no behaviour
//! change.
//!
//! # Framing
//!
//! Every frame is `[len:u32 LE][kind:u8][payload …]`. `len` is the byte
//! length of `kind + payload` (it does NOT include the 4-byte `len` prefix
//! itself). All multi-byte integers are little-endian.
//!
//! # Direction
//!
//! Inbound (`0x01..=0x7F`) is gateway → engine. Outbound (`0x80..=0xFF`)
//! is engine → gateway.
//!
//! | Code    | Direction | Message         | Fixed payload size |
//! |---------|-----------|-----------------|-------------------:|
//! | `0x01`  | inbound   | `NewOrder`      | 48 B               |
//! | `0x02`  | inbound   | `CancelOrder`   | 24 B               |
//! | `0x03`  | inbound   | `CancelReplace` | 40 B               |
//! | `0x04`  | inbound   | `MassCancel`    | 24 B               |
//! | `0x81`  | outbound  | `ExecReport`    | 44 B               |
//! | `0x82`  | outbound  | `TradePrint`    | 48 B               |
//! | `0x83`  | outbound  | `BookUpdate`    | 32 B               |
//!
//! # Inbound zero-copy
//!
//! Inbound messages are `#[repr(C, packed)]` and derive the `zerocopy`
//! traits required to validate-and-cast a `&[u8]` into a typed reference
//! without copying. Decoding is safe (no `unsafe` is required at this
//! layer); it returns [`WireError::InvalidPayload`] on size mismatch.
//!
//! # Outbound byte-cursor
//!
//! Outbound messages are encoded via explicit byte-cursor (`Vec<u8>` +
//! `extend_from_slice`). Outbound is I/O-dominated, so the marginal cost
//! of copying a few dozen bytes is negligible compared to socket
//! overhead, and we keep the layout free to evolve without exposing a
//! packed type to callers.
//!
//! See `doc/wire-protocol.md` for the canonical layout tables.

pub mod error;
pub mod framing;
pub mod inbound;
pub mod outbound;

pub use error::WireError;
pub use framing::{decode_frame, encode_frame};
pub use inbound::{
    CancelOrderWire, CancelReplaceWire, MassCancelWire, NewOrderWire, decode_cancel_order,
    decode_cancel_replace, decode_mass_cancel, decode_new_order,
};
pub use outbound::{
    BookUpdateWire, ExecReport, TradePrintWire, decode_book_update, decode_exec_report,
    decode_trade_print, encode_book_update, encode_exec_report, encode_trade_print, status_to_wire,
};

/// Kind discriminants for every binary wire message.
///
/// Wire codes are stable across `0.7.x` patch releases. Inbound messages
/// occupy the low half of the byte (`0x01..=0x7F`); outbound messages
/// occupy the high half (`0x80..=0xFF`). Variant `0x00` is reserved as a
/// "no-message" sentinel and is intentionally absent from the enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
#[non_exhaustive]
pub enum MessageKind {
    /// Inbound: submit a new order. Payload: [`NewOrderWire`] (48 B).
    NewOrder = 0x01,
    /// Inbound: cancel an existing order. Payload: [`CancelOrderWire`] (24 B).
    CancelOrder = 0x02,
    /// Inbound: cancel-and-replace an existing order. Payload:
    /// [`CancelReplaceWire`] (40 B).
    CancelReplace = 0x03,
    /// Inbound: mass cancel by scope. Payload: [`MassCancelWire`] (24 B).
    MassCancel = 0x04,
    /// Outbound: execution report for an order's lifecycle event. Payload:
    /// [`ExecReport`] (44 B).
    ExecReport = 0x81,
    /// Outbound: trade print announcing a fill. Payload: [`TradePrintWire`]
    /// (48 B).
    TradePrint = 0x82,
    /// Outbound: incremental book level update. Payload: [`BookUpdateWire`]
    /// (32 B).
    BookUpdate = 0x83,
}

impl MessageKind {
    /// Resolves a raw kind byte to a typed [`MessageKind`].
    ///
    /// # Errors
    ///
    /// Returns [`WireError::UnknownKind`] for any byte outside the
    /// documented set.
    #[inline]
    pub fn from_u8(byte: u8) -> Result<Self, WireError> {
        match byte {
            0x01 => Ok(Self::NewOrder),
            0x02 => Ok(Self::CancelOrder),
            0x03 => Ok(Self::CancelReplace),
            0x04 => Ok(Self::MassCancel),
            0x81 => Ok(Self::ExecReport),
            0x82 => Ok(Self::TradePrint),
            0x83 => Ok(Self::BookUpdate),
            other => Err(WireError::UnknownKind(other)),
        }
    }

    /// Returns the raw kind byte for this variant.
    #[must_use]
    #[inline]
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    /// Returns `true` if this is an inbound (gateway → engine) message.
    #[must_use]
    #[inline]
    pub const fn is_inbound(self) -> bool {
        (self as u8) < 0x80
    }

    /// Returns `true` if this is an outbound (engine → gateway) message.
    #[must_use]
    #[inline]
    pub const fn is_outbound(self) -> bool {
        (self as u8) >= 0x80
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_u8_round_trip() {
        for kind in [
            MessageKind::NewOrder,
            MessageKind::CancelOrder,
            MessageKind::CancelReplace,
            MessageKind::MassCancel,
            MessageKind::ExecReport,
            MessageKind::TradePrint,
            MessageKind::BookUpdate,
        ] {
            let byte = kind.as_u8();
            let resolved = MessageKind::from_u8(byte).expect("resolve known kind");
            assert_eq!(resolved, kind);
        }
    }

    #[test]
    fn from_u8_rejects_unknown() {
        assert_eq!(
            MessageKind::from_u8(0x00),
            Err(WireError::UnknownKind(0x00))
        );
        assert_eq!(
            MessageKind::from_u8(0x05),
            Err(WireError::UnknownKind(0x05))
        );
        assert_eq!(
            MessageKind::from_u8(0xFF),
            Err(WireError::UnknownKind(0xFF))
        );
    }

    #[test]
    fn direction_classification() {
        assert!(MessageKind::NewOrder.is_inbound());
        assert!(!MessageKind::NewOrder.is_outbound());
        assert!(MessageKind::ExecReport.is_outbound());
        assert!(!MessageKind::ExecReport.is_inbound());
    }
}
