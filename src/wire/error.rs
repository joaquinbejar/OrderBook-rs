//! Errors raised by the binary wire protocol codec.
//!
//! Manual `Display` implementation to avoid pulling in `thiserror` for the
//! `wire` feature surface — keeps the dependency footprint minimal.

/// Errors that can be raised when framing, decoding, or validating a binary
/// wire message.
///
/// `WireError` is `#[non_exhaustive]` — additional variants may be added in
/// future minor releases without a breaking change.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum WireError {
    /// The buffer is shorter than the framing or payload length declares.
    Truncated,
    /// The frame's `kind` byte does not map to a known [`MessageKind`].
    ///
    /// The raw byte is preserved for telemetry and rejection reporting.
    ///
    /// [`MessageKind`]: super::MessageKind
    UnknownKind(u8),
    /// The payload's length does not match the fixed size required by the
    /// declared `MessageKind`, or a packed field carries an invalid
    /// discriminant.
    ///
    /// The static string is a stable, tracing-friendly description of the
    /// failure site (e.g. `"NewOrder: payload size mismatch"`).
    InvalidPayload(&'static str),
}

impl std::fmt::Display for WireError {
    #[cold]
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WireError::Truncated => f.write_str("wire frame truncated"),
            WireError::UnknownKind(byte) => write!(f, "wire unknown kind: 0x{byte:02x}"),
            WireError::InvalidPayload(reason) => write!(f, "wire invalid payload: {reason}"),
        }
    }
}

impl std::error::Error for WireError {}
