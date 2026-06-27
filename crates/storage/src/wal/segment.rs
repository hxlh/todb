//! On-disk segment: `O_DIRECT` `.log` + `.idx` files with block-aligned I/O,
//! `fallocate` preallocation, `fdatasync`, and `.idx` header double-write.

use std::io;
use std::os::unix::io::RawFd;
use std::path::Path;

use crate::wal::{AlignedMem, IDX_HEADER_LEN, IdxHeader, WalError, select_valid_header};

/// A segment pair: `wal-{seg_id}.log` (preallocated) + `wal-{seg_id}.idx` (append-grown).
/// Exposes the raw fds + log-specific helpers; generic block I/O goes through the
/// `pub(crate)` free functions (`pwrite_all` / `pread_all` / `fdatasync_fd`).
pub struct Segment {
    log_fd: RawFd,
    idx_fd: RawFd,
    seg_id: u32,
    segment_size: usize,
    block_size: usize,
}

const MODE_644: libc::mode_t = 0o644;

fn open_fd(path: &Path, o_direct: bool) -> Result<RawFd, WalError> {
    let s = path
        .to_str()
        .ok_or_else(|| WalError::Io(io::Error::other("non-utf8 path")))?;
    let c_path = std::ffi::CString::new(s)
        .map_err(|e| WalError::Io(io::Error::other(format!("path nul: {e}"))))?;
    let mut flags = libc::O_RDWR | libc::O_CREAT;
    if o_direct {
        flags |= libc::O_DIRECT;
    }
    // SAFETY: `c_path` is a valid NUL-terminated C string; flags/mode are constants.
    let fd = unsafe { libc::open(c_path.as_ptr(), flags, MODE_644 as libc::c_uint) };
    if fd < 0 {
        return Err(WalError::Io(io::Error::last_os_error()));
    }
    Ok(fd)
}

impl Segment {
    /// Create + open a segment. `.log` is preallocated to `segment_size`; `.idx` is
    /// append-grown. `o_direct = false` lets tests run on tmpfs/CI; production uses
    /// `true` (4 KiB-aligned I/O bypassing the page cache).
    pub fn create(
        dir: &Path,
        seg_id: u32,
        segment_size: usize,
        block_size: usize,
        o_direct: bool,
    ) -> Result<Self, WalError> {
        let log_path = dir.join(format!("wal-{seg_id}.log"));
        let idx_path = dir.join(format!("wal-{seg_id}.idx"));
        let log_fd = open_fd(&log_path, o_direct)?;
        let idx_fd = open_fd(&idx_path, o_direct)?;
        let seg = Self {
            log_fd,
            idx_fd,
            seg_id,
            segment_size,
            block_size,
        };
        seg.fallocate_log(0, segment_size as i64)?;
        Ok(seg)
    }

    pub fn seg_id(&self) -> u32 {
        self.seg_id
    }
    pub fn segment_size(&self) -> usize {
        self.segment_size
    }
    pub fn block_size(&self) -> usize {
        self.block_size
    }
    pub fn log_fd(&self) -> RawFd {
        self.log_fd
    }
    pub fn idx_fd(&self) -> RawFd {
        self.idx_fd
    }

    /// `fallocate` a range on `.log` (mode 0 = allocate + zero on ext4/xfs default).
    pub fn fallocate_log(&self, offset: i64, len: i64) -> Result<(), WalError> {
        // SAFETY: `log_fd` is a valid open fd.
        if unsafe { libc::fallocate(self.log_fd, 0, offset, len) } != 0 {
            return Err(WalError::Io(io::Error::last_os_error()));
        }
        Ok(())
    }

    /// Truncate `.log` to `len` bytes (physical). Caller ensures block alignment.
    pub fn truncate_log(&self, len: u64) -> Result<(), WalError> {
        // SAFETY: `log_fd` is a valid open fd.
        if unsafe { libc::ftruncate(self.log_fd, len as i64) } != 0 {
            return Err(WalError::Io(io::Error::last_os_error()));
        }
        Ok(())
    }

    /// Double-write the `.idx` header: copy A (block 0) → `fdatasync` → copy B
    /// (block 1) → `fdatasync`. Encapsulates the crash-consistency ordering
    /// (`tradeoffs §21`) so callers cannot accidentally skip a step.
    pub fn write_idx_header_double(&self, header: &IdxHeader) -> Result<(), WalError> {
        let mut block = AlignedMem::zeroed(self.block_size, self.block_size)?;
        block.as_bytes_mut()[..IDX_HEADER_LEN].copy_from_slice(&header.serialize());
        pwrite_all(self.idx_fd, block.as_bytes(), 0)?;
        fdatasync_fd(self.idx_fd)?;
        pwrite_all(self.idx_fd, block.as_bytes(), self.block_size as i64)?;
        fdatasync_fd(self.idx_fd)?;
        Ok(())
    }

    /// Read the `.idx` header via double-copy selection: pread block 0 (copy A) and
    /// block 1 (copy B), return whichever passes `header_crc`. `Err(HeaderCorrupt)`
    /// only if both copies fail. See `tradeoffs §21`.
    pub fn read_idx_header(&self) -> Result<IdxHeader, WalError> {
        let mut a = AlignedMem::zeroed(self.block_size, self.block_size)?;
        let mut b = AlignedMem::zeroed(self.block_size, self.block_size)?;
        pread_all(self.idx_fd, a.as_bytes_mut(), 0)?;
        pread_all(self.idx_fd, b.as_bytes_mut(), self.block_size as i64)?;
        select_valid_header(a.as_bytes(), b.as_bytes())
    }
}

impl Drop for Segment {
    fn drop(&mut self) {
        // SAFETY: both fds are valid open fds owned by `self`; close once on drop.
        // Errors ignored (best-effort cleanup).
        unsafe {
            libc::close(self.log_fd);
            libc::close(self.idx_fd);
        }
    }
}

/// `pwrite` loop — writes until the whole buffer is flushed. For `O_DIRECT`, `buf`
/// must be `block_size`-aligned (use `AlignedMem::as_bytes()`).
pub(crate) fn pwrite_all(fd: RawFd, mut buf: &[u8], mut offset: i64) -> Result<(), WalError> {
    while !buf.is_empty() {
        // SAFETY: `fd` valid; `buf.as_ptr()`/`buf.len()` valid for the slice lifetime.
        let n = unsafe { libc::pwrite(fd, buf.as_ptr() as *const _, buf.len(), offset) };
        if n < 0 {
            return Err(WalError::Io(io::Error::last_os_error()));
        }
        let n = n as usize;
        if n == 0 {
            return Err(WalError::Io(io::Error::other("pwrite wrote 0 bytes")));
        }
        buf = &buf[n..];
        offset += n as i64;
    }
    Ok(())
}

/// `pread` loop. Stops at EOF; the caller sees a zero-padded tail if the buffer was
/// pre-zeroed (e.g. `AlignedMem::zeroed`).
pub(crate) fn pread_all(fd: RawFd, mut buf: &mut [u8], mut offset: i64) -> Result<(), WalError> {
    while !buf.is_empty() {
        // SAFETY: `fd` valid; `buf.as_mut_ptr()`/`buf.len()` valid; no aliasing during call.
        let n = unsafe { libc::pread(fd, buf.as_mut_ptr() as *mut _, buf.len(), offset) };
        if n < 0 {
            return Err(WalError::Io(io::Error::last_os_error()));
        }
        let n = n as usize;
        if n == 0 {
            break;
        }
        buf = &mut buf[n..];
        offset += n as i64;
    }
    Ok(())
}

/// `fdatasync` — flush file data + necessary metadata to disk.
pub(crate) fn fdatasync_fd(fd: RawFd) -> Result<(), WalError> {
    // SAFETY: `fd` is a valid open fd.
    if unsafe { libc::fdatasync(fd) } != 0 {
        return Err(WalError::Io(io::Error::last_os_error()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wal::IdxHeader;
    use crate::wal::index::select_valid_header;
    use tempfile::tempdir;

    // 测试用 o_direct=false（兼容 tmpfs / CI）；production 用 true。
    fn make_seg(dir: &Path, segment_size: usize) -> Segment {
        Segment::create(dir, 0, segment_size, 4096, false).unwrap()
    }

    #[test]
    fn create_preallocates_log() {
        let dir = tempdir().unwrap();
        let seg = make_seg(dir.path(), 1 << 16);
        let size = std::fs::metadata(dir.path().join("wal-0.log"))
            .unwrap()
            .len();
        assert_eq!(size, 1 << 16);
        assert_eq!(seg.segment_size(), 1 << 16);
    }

    #[test]
    fn pwrite_pread_roundtrip() {
        let dir = tempdir().unwrap();
        let seg = make_seg(dir.path(), 4096 * 4);
        let mut block = AlignedMem::zeroed(4096, 4096).unwrap();
        block.as_bytes_mut()[..5].copy_from_slice(b"hello");
        pwrite_all(seg.log_fd(), block.as_bytes(), 0).unwrap();
        fdatasync_fd(seg.log_fd()).unwrap();

        let mut read = AlignedMem::zeroed(4096, 4096).unwrap();
        pread_all(seg.log_fd(), read.as_bytes_mut(), 0).unwrap();
        assert_eq!(&read.as_bytes()[..5], b"hello");
    }

    #[test]
    fn truncate_then_refallocate_restores_size() {
        let dir = tempdir().unwrap();
        let seg = make_seg(dir.path(), 1 << 16);
        seg.truncate_log(8192).unwrap();
        assert_eq!(
            std::fs::metadata(dir.path().join("wal-0.log"))
                .unwrap()
                .len(),
            8192
        );
        // re-fallocate tail（truncate_after 路径）
        seg.fallocate_log(8192, (1 << 16) - 8192).unwrap();
        assert_eq!(
            std::fs::metadata(dir.path().join("wal-0.log"))
                .unwrap()
                .len(),
            1 << 16
        );
    }

    #[test]
    fn idx_header_double_write_roundtrip() {
        let dir = tempdir().unwrap();
        let seg = make_seg(dir.path(), 4096 * 4);
        let header = IdxHeader::new(0, 100, 200, 50);
        seg.write_idx_header_double(&header).unwrap();

        let mut a = AlignedMem::zeroed(4096, 4096).unwrap();
        let mut b = AlignedMem::zeroed(4096, 4096).unwrap();
        pread_all(seg.idx_fd(), a.as_bytes_mut(), 0).unwrap();
        pread_all(seg.idx_fd(), b.as_bytes_mut(), 4096).unwrap();
        let selected = select_valid_header(a.as_bytes(), b.as_bytes()).unwrap();
        assert_eq!(selected, header);
    }

    #[test]
    fn idx_header_double_write_survives_one_corrupt_copy() {
        let dir = tempdir().unwrap();
        let seg = make_seg(dir.path(), 4096 * 4);
        let header = IdxHeader::new(3, 10, 90, 8);
        seg.write_idx_header_double(&header).unwrap();

        // 损坏 copy B（block 1, offset 4096；篡改 min_live_lsn @ 4096+12）
        let mut corrupt = AlignedMem::zeroed(4096, 4096).unwrap();
        pread_all(seg.idx_fd(), corrupt.as_bytes_mut(), 4096).unwrap();
        corrupt.as_bytes_mut()[12] ^= 0xff;
        pwrite_all(seg.idx_fd(), corrupt.as_bytes(), 4096).unwrap();

        let mut a = AlignedMem::zeroed(4096, 4096).unwrap();
        let mut b = AlignedMem::zeroed(4096, 4096).unwrap();
        pread_all(seg.idx_fd(), a.as_bytes_mut(), 0).unwrap();
        pread_all(seg.idx_fd(), b.as_bytes_mut(), 4096).unwrap();
        let selected = select_valid_header(a.as_bytes(), b.as_bytes()).unwrap();
        assert_eq!(selected, header); // copy A 完好 → 用 A
    }
}
