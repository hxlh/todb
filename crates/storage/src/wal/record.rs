use std::ops::Range;
use std::sync::Arc;

use crate::wal::{AlignedMem, Lsn};

/// A log record: an LSN and its payload bytes. `term` lives inside the payload
/// (the WAL is term-agnostic — see `docs/architecture/wal-design.md` §LSN And Term).
#[derive(Debug, Clone)]
pub struct Record {
    pub lsn: Lsn,
    pub payload: Vec<u8>,
}

/// Owned handle to a record's payload backed by an `Arc<AlignedMem>` block.
#[derive(Debug)]
pub struct RecordRef {
    pub lsn: Lsn,
    block: Arc<AlignedMem>,
    payload: Range<usize>,
}

impl RecordRef {
    pub fn new(lsn: Lsn, block: Arc<AlignedMem>, payload: Range<usize>) -> Self {
        Self {
            lsn,
            block,
            payload,
        }
    }

    pub fn payload(&self) -> &[u8] {
        &self.block.as_bytes()[self.payload.clone()]
    }

    pub fn lsn(&self) -> Lsn {
        self.lsn
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_ref_payload_slices_block() {
        let mut block = AlignedMem::zeroed(4096, 4096).unwrap();
        block.as_bytes_mut()[100..103].copy_from_slice(b"xyz");
        let r = RecordRef::new(Lsn(5), Arc::new(block), 100..103);
        assert_eq!(r.lsn(), Lsn(5));
        assert_eq!(r.payload(), b"xyz");
    }
}
