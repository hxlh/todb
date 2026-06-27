//! Lock-free write buffer: byte-stream + per-slot entry index, claimed via `fetch_add`.

use std::sync::atomic::{AtomicU8, AtomicU32, AtomicU64, AtomicUsize, Ordering};

use crate::wal::{AlignedMem, HEADER_LEN, WalError};

pub const STATE_ACTIVE: u8 = 0;
pub const STATE_FULL: u8 = 1;

/// Pack `(pos, frame_len)` into the per-slot `AtomicU64`. `pos < 2^32`, `frame_len < 2^32`.
pub fn pack(pos: usize, frame_len: u32) -> u64 {
    debug_assert!(pos < (1u64 << 32) as usize);
    ((pos as u64) << 32) | u64::from(frame_len)
}

/// Unpack a per-slot `AtomicU64` into `(pos, frame_len)`.
pub fn unpack(v: u64) -> (usize, u32) {
    ((v >> 32) as usize, (v & 0xFFFF_FFFF) as u32)
}

/// Lock-free append buffer. Multiple writers claim disjoint byte ranges and entry
/// slots concurrently; exactly one swapper transitions `state` Active→Full.
pub struct WalBuffer {
    pub data: AlignedMem,
    pub write_pos: AtomicUsize,
    pub entries_allocated: AtomicUsize,
    pub entries: Box<[AtomicU64]>,
    pub in_flight: AtomicU32,
    pub state: AtomicU8,
    /// LSN of slot 0. Carried across swaps to keep lsn continuous.
    pub min_lsn: AtomicU64,
    /// Frozen at swap time (number of valid entries).
    pub count: AtomicUsize,
    pub seg_id: u32,
    pub capacity: usize,
    pub max_entries: usize,
}

impl WalBuffer {
    pub fn new(
        buffer_size: usize,
        block_size: usize,
        min_lsn: u64,
        seg_id: u32,
    ) -> Result<Self, WalError> {
        let data = AlignedMem::new(buffer_size, block_size)?;
        let max_entries = buffer_size / HEADER_LEN;
        let entries = (0..max_entries)
            .map(|_| AtomicU64::new(0))
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Ok(Self {
            data,
            write_pos: AtomicUsize::new(0),
            entries_allocated: AtomicUsize::new(0),
            entries,
            in_flight: AtomicU32::new(0),
            state: AtomicU8::new(STATE_ACTIVE),
            min_lsn: AtomicU64::new(min_lsn),
            count: AtomicUsize::new(0),
            seg_id,
            capacity: buffer_size,
            max_entries,
        })
    }

    /// Reset for reuse with the next `min_lsn`. Caller MUST hold the buffer exclusively
    /// (post-swap, pre-reuse); concurrent appends are not allowed during reset.
    pub fn reset(&self, next_min_lsn: u64) {
        self.write_pos.store(0, Ordering::Relaxed);
        self.entries_allocated.store(0, Ordering::Relaxed);
        self.in_flight.store(0, Ordering::Relaxed);
        self.count.store(0, Ordering::Relaxed);
        self.min_lsn.store(next_min_lsn, Ordering::Relaxed);
        self.state.store(STATE_ACTIVE, Ordering::Relaxed);
        // `entries` slots are overwritten on alloc; no need to clear.
    }

    /// Claim a contiguous `frame_len`-byte range. Returns the start offset, or `None`
    /// if it would exceed `capacity` (caller triggers swap).
    pub fn claim_buffer_range(&self, frame_len: usize) -> Option<usize> {
        let pos = self.write_pos.fetch_add(frame_len, Ordering::Relaxed);
        if pos.checked_add(frame_len)? > self.capacity {
            return None;
        }
        Some(pos)
    }

    /// Claim the next entry slot. Returns the slot index.
    ///
    /// Invariant: in the `append` path (Phase 4a) the byte range is claimed FIRST.
    /// Since every frame is `>= HEADER_LEN` (16 B), the number of slots fits within
    /// `max_entries = buffer_size / HEADER_LEN` — slot exhaustion cannot happen if
    /// the caller claimed a byte range first (byte overflow triggers swap first).
    /// Slot exhaustion here is therefore a caller bug (byte range not claimed, or
    /// buffer mis-sized); we panic rather than return an error.
    pub fn claim_slot(&self) -> usize {
        let slot = self.entries_allocated.fetch_add(1, Ordering::Relaxed);
        assert!(
            slot < self.max_entries,
            "claim_slot exhausted (slot {} >= max_entries {}): append must claim a byte range first",
            slot,
            self.max_entries
        );
        slot
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn pack_unpack_roundtrip() {
        for (pos, len) in [(0, 0), (1, 16), (4096, 100), (1_000_000, 65535)] {
            assert_eq!(unpack(pack(pos, len)), (pos, len));
        }
    }

    #[test]
    fn claim_byte_ranges_are_disjoint_single_thread() {
        let buf = WalBuffer::new(8192, 4096, 0, 0).unwrap();
        let mut positions = Vec::new();
        for len in [16, 32, 8, 64] {
            positions.push((buf.claim_buffer_range(len).unwrap(), len));
        }
        let mut offset = 0;
        for (pos, len) in positions {
            assert_eq!(pos, offset);
            offset += len;
        }
        assert_eq!(offset, buf.write_pos.load(Ordering::Relaxed));
    }

    #[test]
    fn claim_byte_range_overflow_returns_none() {
        let buf = WalBuffer::new(64, 4096, 0, 0).unwrap();
        assert!(buf.claim_buffer_range(32).is_some());
        assert!(buf.claim_buffer_range(32).is_some());
        assert!(buf.claim_buffer_range(1).is_none()); // 64 已满
    }

    #[test]
    fn claim_slots_are_distinct() {
        let buf = WalBuffer::new(16 * 8, 4096, 0, 0).unwrap();
        let mut slots: Vec<usize> = (0..buf.max_entries).map(|_| buf.claim_slot()).collect();
        slots.sort_unstable();
        slots.dedup();
        assert_eq!(slots.len(), buf.max_entries);
    }

    #[test]
    #[should_panic(expected = "claim_slot exhausted")]
    fn claim_slot_exhaustion_panics() {
        let buf = WalBuffer::new(16 * 8, 4096, 0, 0).unwrap();
        for _ in 0..buf.max_entries {
            buf.claim_slot();
        }
        buf.claim_slot(); // 违反不变式（byte range 未先 claim）→ panic
    }

    #[test]
    fn claim_byte_ranges_disjoint_multi_thread() {
        let buf = Arc::new(WalBuffer::new(1 << 20, 4096, 0, 0).unwrap());
        let mut handles = Vec::new();
        for _ in 0..8 {
            let b = Arc::clone(&buf);
            handles.push(std::thread::spawn(move || {
                let mut claimed = Vec::new();
                for _ in 0..100 {
                    if let Some(pos) = b.claim_buffer_range(64) {
                        claimed.push(pos);
                    }
                }
                claimed
            }));
        }
        let mut all: Vec<usize> = Vec::new();
        for h in handles {
            all.extend(h.join().unwrap());
        }
        all.sort_unstable();
        let mut deduped = all.clone();
        deduped.dedup();
        assert_eq!(all.len(), deduped.len(), "positions must not overlap");
    }

    #[test]
    fn reset_clears_state() {
        let buf = WalBuffer::new(4096, 4096, 0, 0).unwrap();
        buf.claim_buffer_range(64);
        buf.claim_slot();
        buf.reset(100);
        assert_eq!(buf.write_pos.load(Ordering::Relaxed), 0);
        assert_eq!(buf.entries_allocated.load(Ordering::Relaxed), 0);
        assert_eq!(buf.min_lsn.load(Ordering::Relaxed), 100);
        assert_eq!(buf.state.load(Ordering::Relaxed), STATE_ACTIVE);
    }
}
