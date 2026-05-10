use bytes::Bytes;
use std::cmp::Ordering;

/// Length of the InternalKey trailer: start_ts(8) + commit_ts(8) + seq(4) + op(1).
pub const INTERNAL_KEY_TRAILER_LEN: usize = 21;

/// MVCC operation type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpType {
    Put = 0,
    Delete = 1,
}

/// Transaction metadata embedded in an InternalKey.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TxInfo {
    pub(crate) start_ts: u64,
    pub(crate) commit_ts: u64,
    pub(crate) seq: u32,
    pub(crate) op: OpType,
}

// format: | primary key(u8 array) | start_ts(u64) | commit_ts(u64) | seq(u32) | op(u8) |
#[derive(Debug, Clone)]
pub struct InternalKey {
    data: Bytes,
    tx_info: TxInfo,
}

impl InternalKey {
    /// Construct from raw encoded bytes. `tx_info` is parsed from the trailer.
    pub fn new(data: Bytes) -> Self {
        let tx_info = Self::parse_tx_info(&data);
        Self { data, tx_info }
    }

    fn parse_tx_info(data: &Bytes) -> TxInfo {
        let len = data.len();
        let trailer_start = len.saturating_sub(INTERNAL_KEY_TRAILER_LEN);
        let t = &data[trailer_start..];

        let start_ts = u64::from_be_bytes([
            t[0], t[1], t[2], t[3], t[4], t[5], t[6], t[7],
        ]);
        let commit_ts = u64::from_be_bytes([
            t[8], t[9], t[10], t[11], t[12], t[13], t[14], t[15],
        ]);
        let seq = u32::from_be_bytes([t[16], t[17], t[18], t[19]]);
        let op = if t[20] == 0 { OpType::Put } else { OpType::Delete };

        TxInfo { start_ts, commit_ts, seq, op }
    }

    /// Build an InternalKey from a user key + sequence + op.
    /// `start_ts` and `commit_ts` are initialized to 0.
    pub fn from_user_key(user_key: Bytes, seq: u64, op: OpType) -> Self {
        let mut data = Vec::with_capacity(user_key.len() + INTERNAL_KEY_TRAILER_LEN);
        data.extend_from_slice(&user_key);
        data.extend_from_slice(&0u64.to_be_bytes()); // start_ts
        data.extend_from_slice(&0u64.to_be_bytes()); // commit_ts
        data.extend_from_slice(&(seq as u32).to_be_bytes());
        data.push(op as u8);
        let data = Bytes::from(data);
        let tx_info = Self::parse_tx_info(&data);
        Self { data, tx_info }
    }

    /// Return the user key portion (everything before the trailer).
    pub fn raw_key(&self) -> Bytes {
        let key_len = self.data.len().saturating_sub(INTERNAL_KEY_TRAILER_LEN);
        self.data.slice(0..key_len)
    }

    /// Return the per-write sequence number.
    pub fn sequence(&self) -> u64 {
        self.tx_info.seq as u64
    }

    /// Return the operation type (Put / Delete).
    pub fn op_type(&self) -> OpType {
        self.tx_info.op
    }

    /// Return the full encoded bytes (including trailer).
    pub fn as_bytes(&self) -> &Bytes {
        &self.data
    }
}

impl PartialEq for InternalKey {
    fn eq(&self, other: &Self) -> bool {
        self.data == other.data
    }
}

impl Eq for InternalKey {}

impl PartialOrd for InternalKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for InternalKey {
    fn cmp(&self, other: &Self) -> Ordering {
        // 1. Compare user key ascending.
        let user_cmp = self.raw_key().cmp(&other.raw_key());
        if user_cmp != Ordering::Equal {
            return user_cmp;
        }
        // 2. Same user key: compare sequence descending (newer first).
        other.sequence().cmp(&self.sequence())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_decode() {
        let user_key = Bytes::from("hello");
        let ik = InternalKey::from_user_key(user_key, 42, OpType::Put);
        assert_eq!(ik.raw_key(), Bytes::from("hello"));
        assert_eq!(ik.sequence(), 42);
        assert_eq!(ik.op_type(), OpType::Put);
    }

    #[test]
    fn test_ordering_user_key() {
        let a = InternalKey::from_user_key(Bytes::from("a"), 100, OpType::Put);
        let b = InternalKey::from_user_key(Bytes::from("b"), 1, OpType::Put);
        assert!(a < b);
    }

    #[test]
    fn test_ordering_sequence_desc() {
        let old = InternalKey::from_user_key(Bytes::from("k"), 10, OpType::Put);
        let new = InternalKey::from_user_key(Bytes::from("k"), 20, OpType::Put);
        // Newer (higher sequence) comes first.
        assert!(new < old);
    }

    #[test]
    fn test_delete_type() {
        let ik = InternalKey::from_user_key(Bytes::from("x"), 1, OpType::Delete);
        assert_eq!(ik.op_type(), OpType::Delete);
    }
}
