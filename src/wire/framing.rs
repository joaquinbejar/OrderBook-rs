//! Length-prefixed binary frame codec.
//!
//! Every frame on the wire has the layout:
//!
//! ```text
//! [len: u32 LE][kind: u8][payload: …]
//! ```
//!
//! `len` is the byte length of `kind + payload` — it does NOT include the
//! `len` field itself. All multi-byte integers on the wire are little-endian.
//!
//! Framing is symmetric for inbound and outbound traffic; the only thing that
//! differs is which `kind` discriminants are valid in each direction (see
//! [`super::MessageKind`]).

use super::error::WireError;
use std::io::{self, Write};

/// Size in bytes of the length prefix.
const LEN_PREFIX: usize = 4;
/// Size in bytes of the kind byte.
const KIND_SIZE: usize = 1;
/// Minimum frame size: a `len` prefix plus a single `kind` byte (zero-byte
/// payload).
const MIN_FRAME_SIZE: usize = LEN_PREFIX + KIND_SIZE;

/// Encodes a frame into `out`.
///
/// Writes `len` (4 bytes, little-endian, value `1 + payload.len()`), the
/// `kind` byte, and the payload, in that order.
///
/// # Errors
///
/// Propagates any [`io::Error`] returned by the underlying writer.
///
/// # Panics
///
/// Does not panic. Payloads larger than `u32::MAX - 1` bytes are not
/// representable on this wire and would saturate to `u32::MAX`; in practice
/// no message in this protocol is anywhere near that size, so this case is
/// not validated at the framer level. Callers building unbounded payloads
/// should guard before calling.
#[inline]
pub fn encode_frame<W: Write>(kind: u8, payload: &[u8], out: &mut W) -> io::Result<()> {
    // `len` is the size of `kind + payload`. `KIND_SIZE` is a `u8`, so the
    // total fits in `u32` for any payload up to `u32::MAX - 1` bytes.
    let body_len = u32::try_from(payload.len().saturating_add(KIND_SIZE)).unwrap_or(u32::MAX);
    out.write_all(&body_len.to_le_bytes())?;
    out.write_all(&[kind])?;
    out.write_all(payload)?;
    Ok(())
}

/// Decodes a single frame from the start of `buf`.
///
/// On success returns `(kind, payload, bytes_consumed)`. `bytes_consumed`
/// includes the `len` prefix and the `kind` byte, so callers can advance
/// their read cursor by exactly that many bytes.
///
/// # Errors
///
/// Returns [`WireError::Truncated`] if `buf` is shorter than the framing
/// header or shorter than the body length declared by the header.
#[inline]
pub fn decode_frame(buf: &[u8]) -> Result<(u8, &[u8], usize), WireError> {
    if buf.len() < MIN_FRAME_SIZE {
        return Err(WireError::Truncated);
    }
    // SAFETY-style note: the bounds check above guarantees `buf[..4]` and
    // `buf[4]` are in bounds. We avoid `[..]` indexing in production by
    // using `get` everywhere; clippy::indexing_slicing is treated as a hard
    // rule in this crate.
    let len_bytes = buf.get(..LEN_PREFIX).ok_or(WireError::Truncated)?;
    let mut len_arr = [0u8; LEN_PREFIX];
    len_arr.copy_from_slice(len_bytes);
    let body_len = u32::from_le_bytes(len_arr) as usize;

    if body_len < KIND_SIZE {
        return Err(WireError::InvalidPayload("frame body shorter than kind"));
    }

    let total = LEN_PREFIX
        .checked_add(body_len)
        .ok_or(WireError::Truncated)?;
    if buf.len() < total {
        return Err(WireError::Truncated);
    }

    let kind = *buf.get(LEN_PREFIX).ok_or(WireError::Truncated)?;
    let payload_start = LEN_PREFIX + KIND_SIZE;
    let payload_end = LEN_PREFIX + body_len;
    let payload = buf
        .get(payload_start..payload_end)
        .ok_or(WireError::Truncated)?;
    Ok((kind, payload, total))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_empty_payload() {
        let mut buf = Vec::new();
        encode_frame(0x01, &[], &mut buf).expect("encode empty payload");
        let (kind, payload, consumed) = decode_frame(&buf).expect("decode empty payload");
        assert_eq!(kind, 0x01);
        assert!(payload.is_empty());
        assert_eq!(consumed, buf.len());
    }

    #[test]
    fn roundtrip_with_payload() {
        let mut buf = Vec::new();
        let payload = [1u8, 2, 3, 4, 5];
        encode_frame(0x42, &payload, &mut buf).expect("encode payload");
        let (kind, decoded, consumed) = decode_frame(&buf).expect("decode payload");
        assert_eq!(kind, 0x42);
        assert_eq!(decoded, &payload);
        assert_eq!(consumed, buf.len());
    }

    #[test]
    fn truncated_header_returns_truncated() {
        // Only 3 bytes — shorter than the 5-byte minimum frame.
        let buf = [0x05, 0x00, 0x00];
        assert_eq!(decode_frame(&buf), Err(WireError::Truncated));
    }

    #[test]
    fn truncated_payload_returns_truncated() {
        // Body length declares 10 bytes but we only have the 5-byte header.
        let buf = [0x0A, 0x00, 0x00, 0x00, 0x01];
        assert_eq!(decode_frame(&buf), Err(WireError::Truncated));
    }

    #[test]
    fn zero_body_length_is_invalid() {
        // `len = 0` means there isn't even a kind byte — protocol violation.
        let buf = [0x00, 0x00, 0x00, 0x00, 0x00];
        assert!(matches!(
            decode_frame(&buf),
            Err(WireError::InvalidPayload(_))
        ));
    }

    #[test]
    fn decode_consumes_only_one_frame_at_a_time() {
        let mut buf = Vec::new();
        encode_frame(0x01, &[0xAA, 0xBB], &mut buf).expect("encode frame 1");
        encode_frame(0x02, &[0xCC], &mut buf).expect("encode frame 2");

        let (k1, p1, used1) = decode_frame(&buf).expect("decode frame 1");
        assert_eq!(k1, 0x01);
        assert_eq!(p1, &[0xAA, 0xBB]);

        let rest = buf.get(used1..).expect("rest of buffer");
        let (k2, p2, used2) = decode_frame(rest).expect("decode frame 2");
        assert_eq!(k2, 0x02);
        assert_eq!(p2, &[0xCC]);
        assert_eq!(used1 + used2, buf.len());
    }
}
