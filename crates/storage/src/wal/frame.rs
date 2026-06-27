//! Streaming container frame: `len | crc32 | lsn | payload` (16 B header, little-endian).
//!
//! Frames are self-describing and torn-tail detectable. A frame may span
//! multiple 4 KiB blocks — there is no per-block framing.

use crate::wal::{Lsn, WalError};

/// 16 B frame header: `len: u32 | crc32: u32 | lsn: u64` (little-endian).
pub const HEADER_LEN: usize = 16;

/// A successfully decoded frame: its LSN and total byte length (header + payload).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodedFrame {
    pub lsn: Lsn,
    pub total_len: usize,
}

impl DecodedFrame {
    /// Decode a frame starting at `offset` within `buf`.
    ///
    /// - `Ok(Some(frame))` — valid frame.
    /// - `Ok(None)` — header incomplete or `len` runs past `buf` (torn tail / truncation;
    ///   recovery treats this as the segment's valid end).
    /// - `Err(CrcMismatch)` — header looks complete but the crc check failed.
    pub fn decode_at(buf: &[u8], offset: usize) -> Result<Option<DecodedFrame>, WalError> {
        if offset + HEADER_LEN > buf.len() {
            return Ok(None);
        }
        let len = u32::from_le_bytes(
            buf[offset..offset + 4]
                .try_into()
                .expect("header len checked"),
        );
        let crc = u32::from_le_bytes(
            buf[offset + 4..offset + 8]
                .try_into()
                .expect("header len checked"),
        );
        let lsn = u64::from_le_bytes(
            buf[offset + 8..offset + 16]
                .try_into()
                .expect("header len checked"),
        );
        let frame_end = offset + HEADER_LEN + len as usize;
        if frame_end > buf.len() {
            return Ok(None);
        }
        let mut h = crc32fast::Hasher::new();
        h.update(&lsn.to_le_bytes());
        h.update(&buf[offset + HEADER_LEN..frame_end]);
        if h.finalize() != crc {
            return Err(WalError::CrcMismatch {
                lsn,
                offset: offset as u64,
            });
        }
        Ok(Some(DecodedFrame {
            lsn: Lsn(lsn),
            total_len: HEADER_LEN + len as usize,
        }))
    }
}

/// Encode `payload` with its `lsn` into a self-describing frame.
pub fn encode(lsn: Lsn, payload: &[u8]) -> Vec<u8> {
    let len = payload.len() as u32;
    let crc = crc32_of(lsn, payload);
    let mut buf = Vec::with_capacity(HEADER_LEN + payload.len());
    buf.extend_from_slice(&len.to_le_bytes());
    buf.extend_from_slice(&crc.to_le_bytes());
    buf.extend_from_slice(&lsn.get().to_le_bytes());
    buf.extend_from_slice(payload);
    buf
}

fn crc32_of(lsn: Lsn, payload: &[u8]) -> u32 {
    let mut h = crc32fast::Hasher::new();
    h.update(&lsn.get().to_le_bytes());
    h.update(payload);
    h.finalize()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let payload = b"hello world";
        let frame = encode(Lsn(42), payload);
        let decoded = DecodedFrame::decode_at(&frame, 0).unwrap().unwrap();
        assert_eq!(decoded.lsn, Lsn(42));
        assert_eq!(decoded.total_len, HEADER_LEN + payload.len());
    }

    #[test]
    fn payload_preserved() {
        let payload = b"the quick brown fox";
        let frame = encode(Lsn(7), payload);
        let decoded = DecodedFrame::decode_at(&frame, 0).unwrap().unwrap();
        assert_eq!(
            &frame[HEADER_LEN..HEADER_LEN + decoded.total_len - HEADER_LEN],
            payload
        );
    }

    #[test]
    fn crc_tamper_detected() {
        let mut frame = encode(Lsn(1), b"data");
        frame[HEADER_LEN] ^= 0xff; // 篡改 payload
        let err = DecodedFrame::decode_at(&frame, 0).unwrap_err();
        assert!(matches!(err, WalError::CrcMismatch { lsn: 1, .. }));
    }

    #[test]
    fn crc_header_tamper_detected() {
        let mut frame = encode(Lsn(2), b"another");
        frame[6] ^= 0xff; // 篡改 header 内的 crc 字段
        assert!(DecodedFrame::decode_at(&frame, 0).is_err());
    }

    #[test]
    fn truncated_header_is_none() {
        let frame = encode(Lsn(1), b"x");
        let short = &frame[..HEADER_LEN - 1];
        assert!(DecodedFrame::decode_at(short, 0).unwrap().is_none());
    }

    #[test]
    fn truncated_payload_is_none() {
        let frame = encode(Lsn(1), b"abcdef");
        let short = &frame[..HEADER_LEN + 3]; // payload 不全
        assert!(DecodedFrame::decode_at(short, 0).unwrap().is_none());
    }

    #[test]
    fn empty_payload_round_trips() {
        let frame = encode(Lsn(0), b"");
        let decoded = DecodedFrame::decode_at(&frame, 0).unwrap().unwrap();
        assert_eq!(decoded.lsn, Lsn(0));
        assert_eq!(decoded.total_len, HEADER_LEN);
    }

    #[test]
    fn spanning_header_decodes() {
        // frame header 从 offset 4092 起 → [4092, 4108) 横跨 4096 block 边界
        let block = 4096usize;
        let mut buf = vec![0u8; block * 2];
        let payload = b"spanning frame payload bytes";
        let frame = encode(Lsn(99), payload);
        let offset = block - 4;
        buf[offset..offset + frame.len()].copy_from_slice(&frame);
        let decoded = DecodedFrame::decode_at(&buf, offset).unwrap().unwrap();
        assert_eq!(decoded.lsn, Lsn(99));
        assert_eq!(decoded.total_len, HEADER_LEN + payload.len());
        assert_eq!(
            &buf[offset + HEADER_LEN..offset + decoded.total_len],
            payload
        );
    }

    #[test]
    fn sequential_decode_advances_offset() {
        let payload = b"seq";
        let mut buf = encode(Lsn(10), payload);
        let frame2 = encode(Lsn(11), payload);
        buf.extend_from_slice(&frame2);
        let first = DecodedFrame::decode_at(&buf, 0).unwrap().unwrap();
        let second = DecodedFrame::decode_at(&buf, first.total_len)
            .unwrap()
            .unwrap();
        assert_eq!(first.lsn, Lsn(10));
        assert_eq!(second.lsn, Lsn(11));
    }
}
