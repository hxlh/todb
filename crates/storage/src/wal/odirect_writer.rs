//! O_DIRECT SST writers for the WAL index `.idx` — a dumb [`BlockWriter`] paired
//! with an [`SstWriter`] that owns the one piece of SST encoding O_DIRECT needs.
//!
//! `ODirectBlockWriter` is a plain block writer: it asserts the builder-padded
//! `block_size` invariant, copies into an aligned buffer, and `pwrite`s — nothing
//! more (per the `SstWriter` contract: the builder/writer layer encodes, the raw
//! writer only writes).
//!
//! `ODirectSstWriter` wraps it and implements [`SstWriter`]. Data/index blocks
//! (`block_size`) delegate untouched. The variable-length SST footer
//! (`body ++ [body_len:u32]`) can't go through `O_DIRECT` raw (unaligned length),
//! so `write_footer` lays it out in one block as `[body][padding][trailer]` — the
//! `body_len` trailer at the block's last 4 bytes, body at the block start — so a
//! block-aligned read of the final block recovers it
//! (`trailer = block[block_size-4..]`, `body = block[..body_len]`). The padding
//! between them is never read back.
//!
//! `o_direct = false` is the tmpfs/CI mode (matches `Segment` / `WalConfig`);
//! production uses `true`.

use std::os::unix::io::RawFd;
use std::path::Path;

use crate::block::{BlockWriter, Position};
use crate::builder::{SstFooter, SstOption, SstWriter};
use crate::errors::{StorageError, StorageResult};
use crate::wal::segment::{fdatasync_fd, open_fd, pwrite_all};
use crate::wal::AlignedMem;

/// Dumb `BlockWriter` over an `O_DIRECT` fd. Copies each block into an aligned
/// buffer and `pwrite`s it at the next block-aligned offset. No SST/footer
/// knowledge — callers must hand it `block_size`-padded input (the [`SstWriter`]
/// contract).
pub struct ODirectBlockWriter {
    fd: RawFd,
    block_size: usize,
    next_offset: u64,
}

impl ODirectBlockWriter {
    /// `o_direct = true` in production (4 KiB-aligned I/O bypassing the page
    /// cache); `false` for tmpfs/CI.
    pub fn create(path: &Path, block_size: usize, o_direct: bool) -> Result<Self, StorageError> {
        let fd = open_fd(path, o_direct).map_err(StorageError::from)?;
        Ok(Self {
            fd,
            block_size,
            next_offset: 0,
        })
    }

    /// Bytes written so far (== current file length; always a block multiple).
    pub fn file_size(&self) -> u64 {
        self.next_offset
    }

    /// Flush data + size metadata to disk. Call before the `.meta` commit so the
    /// `.idx` SST is durable (the finalize commit invariant).
    pub fn sync_all(&self) -> Result<(), StorageError> {
        fdatasync_fd(self.fd).map_err(StorageError::from)
    }
}

impl BlockWriter for ODirectBlockWriter {
    fn write_block<T: AsRef<[u8]>>(&mut self, data: T) -> StorageResult<Position> {
        let data = data.as_ref();
        debug_assert_eq!(
            data.len(),
            self.block_size,
            "ODirectBlockWriter expects block_size-padded input; ODirectSstWriter pads the footer"
        );
        let mut block = AlignedMem::zeroed(self.block_size, self.block_size)?;
        block.as_bytes_mut().copy_from_slice(data);
        let pos = Position {
            offset: self.next_offset,
        };
        pwrite_all(self.fd, block.as_bytes(), self.next_offset as i64)?;
        self.next_offset += self.block_size as u64;
        Ok(pos)
    }
}

impl Drop for ODirectBlockWriter {
    fn drop(&mut self) {
        // SAFETY: `fd` is a valid open fd owned by `self`; close once on drop.
        // Errors ignored (best-effort cleanup).
        unsafe {
            libc::close(self.fd);
        }
    }
}

/// [`SstWriter`] over [`ODirectBlockWriter`]. Data/index blocks delegate
/// verbatim; the footer is padded to one `block_size` block as
/// `[body][padding][trailer]` so it survives O_DIRECT's alignment requirement.
pub struct ODirectSstWriter {
    inner: ODirectBlockWriter,
    block_size: usize,
}

impl ODirectSstWriter {
    pub fn new(inner: ODirectBlockWriter, option: &SstOption) -> Self {
        Self {
            inner,
            block_size: option.block_size,
        }
    }

    pub fn into_inner(self) -> ODirectBlockWriter {
        self.inner
    }
}

impl SstWriter for ODirectSstWriter {
    fn write_block<T: AsRef<[u8]>>(&mut self, data: T) -> StorageResult<Position> {
        self.inner.write_block(data)
    }

    fn write_footer(&mut self, footer: &SstFooter) -> StorageResult<Position> {
        // Footer encoding for O_DIRECT: `footer.encode()` is `body ++ [body_len:u32]`,
        // sub-block_size. Pack it into one block as [body][padding][trailer] — the
        // trailer (body_len) at the block's last 4 bytes so the read path recovers
        // it without a buffered file-tail access (incompatible with O_DIRECT).
        let encoded = footer.encode();
        let n = encoded.len();
        assert!(
            n >= 4 && n <= self.block_size,
            "footer ({n}B) must carry the 4-byte trailer and fit in one {}B block",
            self.block_size
        );
        let body_len = n - 4;
        let mut block = AlignedMem::zeroed(self.block_size, self.block_size)?;
        block.as_bytes_mut()[..body_len].copy_from_slice(&encoded[..body_len]);
        let trailer_off = self.block_size - 4;
        block.as_bytes_mut()[trailer_off..].copy_from_slice(&encoded[body_len..]);
        self.inner.write_block(block.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builder::SstOption;
    use tempfile::tempdir;

    fn footer() -> SstFooter {
        SstFooter {
            root_index_block_position: Position { offset: 0x100 },
            tree_height: 3,
            first_key: bytes::Bytes::from_static(b"aaaa"),
            last_key: bytes::Bytes::from_static(b"zzzz"),
        }
    }

    // `write_footer` lays the block out [body][padding][trailer]; a read of the
    // final block must reconstruct `body ++ trailer` and decode it.
    #[test]
    fn footer_round_trips_via_padded_layout() {
        let block_size = 64usize;
        let dir = tempdir().unwrap();
        let path = dir.path().join("f.idx");
        let f = footer();

        let w = ODirectBlockWriter::create(&path, block_size, false).unwrap();
        let mut sst = ODirectSstWriter::new(w, &SstOption::default().block_size(block_size));
        sst.write_footer(&f).unwrap();
        let written = sst.into_inner().file_size();
        assert_eq!(written, block_size as u64, "footer must pad up to one block");

        // Reconstruct from the padded block: trailer = last 4 bytes, body = first
        // body_len bytes (matches how the index read path decodes).
        let buf = std::fs::read(&path).unwrap();
        let body_len = u32::from_be_bytes(buf[block_size - 4..block_size].try_into().unwrap()) as usize;
        let mut reconstructed = buf[..body_len].to_vec();
        reconstructed.extend_from_slice(&buf[block_size - 4..block_size]);
        assert_eq!(reconstructed, f.encode());
        assert_eq!(SstFooter::decode(&reconstructed).unwrap(), f);
    }
}

#[cfg(test)]
mod odirect_true_tests {
    use super::*;
    use crate::builder::SstOption;
    use std::path::PathBuf;

    // One-shot: O_DIRECT=true (real kernel O_DIRECT) on an ext4 tempdir.
    #[test]
    fn odirect_true_round_trips_on_ext4() {
        let Some(dir) = std::env::var_os("TODB_ODIRECT_DIR").map(PathBuf::from) else {
            eprintln!("skipping (set TODB_ODIRECT_DIR to an ext4 dir to run)");
            return;
        };
        let path = dir.join("odirect_true.idx");
        let block_size = 4096usize;
        let f = SstFooter {
            root_index_block_position: Position { offset: 0x200 },
            tree_height: 1,
            first_key: bytes::Bytes::from_static(b"key0"),
            last_key: bytes::Bytes::from_static(b"key9"),
        };
        let w = ODirectBlockWriter::create(&path, block_size, true).unwrap();
        let mut sst = ODirectSstWriter::new(w, &SstOption::default().block_size(block_size));
        sst.write_footer(&f).unwrap();
        sst.into_inner().sync_all().unwrap();

        let buf = std::fs::read(&path).unwrap();
        assert_eq!(buf.len(), block_size);
        let body_len = u32::from_be_bytes(buf[block_size - 4..block_size].try_into().unwrap()) as usize;
        let mut recon = buf[..body_len].to_vec();
        recon.extend_from_slice(&buf[block_size - 4..block_size]);
        assert_eq!(recon, f.encode(), "O_DIRECT=true footer must round-trip");
        assert_eq!(SstFooter::decode(&recon).unwrap(), f);
        let _ = std::fs::remove_file(&path);
    }
}
