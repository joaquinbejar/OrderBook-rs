//! Inbound (gateway → engine) wire messages.
//!
//! Each message is a fixed-size, `#[repr(C, packed)]` struct that derives
//! the `zerocopy` traits needed to validate-and-cast `&[u8]` into a typed
//! reference without copying. The decoder helpers (`decode_*`) verify the
//! payload length and return an owned, packed copy of the wire struct.
//!
//! All fields are little-endian primitives. See `doc/wire-protocol.md` for
//! the canonical layout tables.

pub mod cancel;
pub mod cancel_replace;
pub mod mass_cancel;
pub mod new_order;

pub use cancel::{CancelOrderWire, decode_cancel_order};
pub use cancel_replace::{CancelReplaceWire, decode_cancel_replace};
pub use mass_cancel::{
    MassCancelWire, SCOPE_ALL, SCOPE_BY_ACCOUNT, SCOPE_BY_SIDE, decode_mass_cancel,
};
pub use new_order::{
    NewOrderWire, ORDER_TYPE_STANDARD, SIDE_BUY, SIDE_SELL, TIF_DAY, TIF_FOK, TIF_GTC, TIF_IOC,
    decode_new_order,
};
