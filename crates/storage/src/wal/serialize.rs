use bytes::Bytes;

use crate::errors::{StorageError, StorageResult};
use crate::write_batch::{WriteBatch, WriteEntry};

/// Serialize a [`WriteBatch`] to bytes for a WAL payload.
///
/// Framing: `entry_count:u32`, then per entry `tag:u8` (0=Put, 1=Delete) +
/// `key_len:u32 + key` (+ `value_len:u32 + value` for Put).
pub fn encode_write_batch(batch: &WriteBatch) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.extend_from_slice(&(batch.entries.len() as u32).to_be_bytes());
    for entry in &batch.entries {
        match entry {
            WriteEntry::Put { key, value } => {
                buf.push(0u8);
                encode_bytes(&mut buf, key);
                encode_bytes(&mut buf, value);
            }
            WriteEntry::Delete { key } => {
                buf.push(1u8);
                encode_bytes(&mut buf, key);
            }
        }
    }
    buf
}

/// Deserialize a [`WriteBatch`] from bytes (inverse of [`encode_write_batch`]).
pub fn decode_write_batch(buf: &[u8]) -> StorageResult<WriteBatch> {
    let mut pos = 0usize;
    let count = read_u32_be(buf, &mut pos)? as usize;
    let mut entries = Vec::with_capacity(count);
    for _ in 0..count {
        let tag = read_u8(buf, &mut pos)?;
        match tag {
            0 => {
                let key = read_bytes(buf, &mut pos)?;
                let value = read_bytes(buf, &mut pos)?;
                entries.push(WriteEntry::Put { key, value });
            }
            1 => {
                let key = read_bytes(buf, &mut pos)?;
                entries.push(WriteEntry::Delete { key });
            }
            other => {
                return Err(StorageError::WalCorrupted(format!(
                    "unknown write entry tag {other}"
                )));
            }
        }
    }
    Ok(WriteBatch { entries })
}

fn encode_bytes(out: &mut Vec<u8>, b: &[u8]) {
    out.extend_from_slice(&(b.len() as u32).to_be_bytes());
    out.extend_from_slice(b);
}

fn read_u32_be(buf: &[u8], pos: &mut usize) -> StorageResult<u32> {
    if *pos + 4 > buf.len() {
        return Err(StorageError::WalCorrupted("truncated u32".into()));
    }
    let mut arr = [0u8; 4];
    arr.copy_from_slice(&buf[*pos..*pos + 4]);
    *pos += 4;
    Ok(u32::from_be_bytes(arr))
}

fn read_u8(buf: &[u8], pos: &mut usize) -> StorageResult<u8> {
    if *pos >= buf.len() {
        return Err(StorageError::WalCorrupted("truncated u8".into()));
    }
    let v = buf[*pos];
    *pos += 1;
    Ok(v)
}

fn read_bytes(buf: &[u8], pos: &mut usize) -> StorageResult<Bytes> {
    let len = read_u32_be(buf, pos)? as usize;
    if *pos + len > buf.len() {
        return Err(StorageError::WalCorrupted("truncated bytes".into()));
    }
    let b = Bytes::copy_from_slice(&buf[*pos..*pos + len]);
    *pos += len;
    Ok(b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    #[test]
    fn encode_decode_roundtrip_mixed() {
        let mut b = WriteBatch::new();
        b.put(Bytes::from_static(b"k1"), Bytes::from_static(b"v1"));
        b.delete(Bytes::from_static(b"k2"));
        b.put(Bytes::from_static(b"k3"), Bytes::from_static(b""));
        let encoded = encode_write_batch(&b);
        let decoded = decode_write_batch(&encoded).unwrap();
        assert_eq!(decoded.entries.len(), 3);
        assert!(matches!(&decoded.entries[0], WriteEntry::Put { key, value } if key == &Bytes::from_static(b"k1") && value == &Bytes::from_static(b"v1")));
        assert!(matches!(&decoded.entries[1], WriteEntry::Delete { key } if key == &Bytes::from_static(b"k2")));
        assert!(matches!(&decoded.entries[2], WriteEntry::Put { key, value } if key == &Bytes::from_static(b"k3") && value.is_empty()));
    }

    #[test]
    fn encode_decode_empty() {
        let b = WriteBatch::new();
        let encoded = encode_write_batch(&b);
        let decoded = decode_write_batch(&encoded).unwrap();
        assert!(decoded.entries.is_empty());
    }

    #[test]
    fn decode_truncated_returns_err() {
        let mut b = WriteBatch::new();
        b.put(Bytes::from_static(b"k"), Bytes::from_static(b"v"));
        let mut encoded = encode_write_batch(&b);
        encoded.truncate(encoded.len() - 1); // chop last byte
        assert!(decode_write_batch(&encoded).is_err());
    }
}
