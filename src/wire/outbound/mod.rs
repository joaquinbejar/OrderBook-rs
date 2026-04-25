//! Outbound (engine → gateway) wire messages.
//!
//! Outbound messages use byte-cursor encoders rather than packed structs.
//! Outbound traffic is I/O-dominated, so the cost of explicit field-by-field
//! copying into a `Vec<u8>` is negligible compared to the socket overhead,
//! and we keep the layout free to evolve without exposing a packed type to
//! callers.
//!
//! All fields are little-endian primitives. See `doc/wire-protocol.md` for
//! the canonical layout tables.

pub mod book_update;
pub mod exec_report;
pub mod trade_print;

pub use book_update::{BOOK_UPDATE_SIZE, BookUpdateWire, decode_book_update, encode_book_update};
pub use exec_report::{
    EXEC_REPORT_SIZE, ExecReport, STATUS_CANCELLED, STATUS_FILLED, STATUS_OPEN,
    STATUS_PARTIALLY_FILLED, STATUS_REJECTED, decode_exec_report, encode_exec_report,
    status_to_wire,
};
pub use trade_print::{TRADE_PRINT_SIZE, TradePrintWire, decode_trade_print, encode_trade_print};
