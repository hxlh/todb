//! 4 KiB-aligned byte buffer for `O_DIRECT` I/O. Owns its allocation.

use std::alloc::{self, Layout};
use std::io;

use crate::wal::WalError;

/// A heap buffer with a power-of-two alignment (typically 4 KiB for `O_DIRECT`).
#[derive(Debug)]
pub struct AlignedMem {
    ptr: *mut u8,
    layout: Layout,
    len: usize,
}

impl AlignedMem {
    /// Allocate `len` bytes with `align` alignment (uninitialized).
    pub fn new(len: usize, align: usize) -> Result<Self, WalError> {
        let layout = Layout::from_size_align(len, align)
            .map_err(|e| WalError::Io(io::Error::other(format!("bad layout: {e}"))))?;

        let ptr = unsafe { alloc::alloc(layout) };
        if ptr.is_null() {
            return Err(WalError::Io(io::Error::other("allocation failed (OOM)")));
        }
        Ok(Self { ptr, layout, len })
    }

    /// Allocate `len` zero-initialized bytes with `align` alignment.
    pub fn zeroed(len: usize, align: usize) -> Result<Self, WalError> {
        let layout = Layout::from_size_align(len, align)
            .map_err(|e| WalError::Io(io::Error::other(format!("bad layout: {e}"))))?;

        let ptr = unsafe { alloc::alloc_zeroed(layout) };
        if ptr.is_null() {
            return Err(WalError::Io(io::Error::other("allocation failed (OOM)")));
        }
        Ok(Self { ptr, layout, len })
    }

    pub fn as_bytes(&self) -> &[u8] {
        // SAFETY: `ptr` is valid for `len` bytes for self's lifetime.
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }

    pub fn as_bytes_mut(&mut self) -> &mut [u8] {
        // SAFETY: `&mut self` excludes other access; ptr valid for `len`.
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }

    /// Raw mutable pointer to the buffer start. For the WAL lock-free append path,
    /// where multiple writers write **disjoint** frame ranges into the same buffer
    /// concurrently through a shared `&Arc<WalBuffer>`. The caller is responsible
    /// for ensuring no two writes (and no overlapping read) touch the same bytes —
    /// the append path guarantees this via `fetch_add` byte-range claims, and the
    /// flush thread only reads after the `in_flight` barrier has drained all writers.
    pub fn as_mut_ptr(&self) -> *mut u8 {
        self.ptr
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl Drop for AlignedMem {
    fn drop(&mut self) {
        // SAFETY: `ptr` was allocated with exactly this `layout`; `dealloc` matches.
        unsafe {
            alloc::dealloc(self.ptr, self.layout);
        }
    }
}

// SAFETY: AlignedMem owns a heap buffer with no interior mutability beyond `&mut self`.
// Sharing across threads is safe (read path uses `&[u8]`); mutation requires `&mut`.
unsafe impl Send for AlignedMem {}
unsafe impl Sync for AlignedMem {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alloc_is_aligned() {
        let m = AlignedMem::zeroed(8192, 4096).unwrap();
        assert_eq!(m.len(), 8192);
        assert_eq!(m.as_bytes().as_ptr() as usize % 4096, 0);
        assert!(m.as_bytes().iter().all(|&b| b == 0));
    }

    #[test]
    fn write_read_roundtrip() {
        let mut m = AlignedMem::new(4096, 4096).unwrap();
        m.as_bytes_mut()[..5].copy_from_slice(b"hello");
        assert_eq!(&m.as_bytes()[..5], b"hello");
    }

    #[test]
    fn drop_does_not_abort() {
        let _ = AlignedMem::zeroed(4096, 4096).unwrap();
    }
}
