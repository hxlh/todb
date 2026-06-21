use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use parking_lot::Mutex;

use crate::{
    engine::ShardId,
    errors::{StorageError, StorageResult},
    write_batch::WriteBatch,
};

use super::serialize;
use super::{Lsn, WalEntry, WalPayload, WalStore};

const DEFAULT_SEGMENT_SIZE: u64 = 64 * 1024 * 1024; // 64 MiB

/// On-disk append-only WAL for one replication group.
///
/// Layout: `{dir}/{segment_id}.wal`, segment files rotate when a write would
/// exceed `segment_size`. `append` writes into an in-memory buffer; the buffer
/// is flushed (write + fsync) on `sync()`, when it reaches `buffer_size`, or
/// by the optional background sync thread (`start_sync`). `recover` streams
/// `WalEntry`s across all segments without materializing them all at once.
pub struct FileWalStore {
    dir: PathBuf,
    buffer_size: usize,
    segment_size: u64,
    inner: Mutex<Inner>,
    next_lsn: AtomicU64,
    stop: AtomicBool,
    sync_handle: Mutex<Option<JoinHandle<()>>>,
}

struct Inner {
    buffer: Vec<u8>,
    cur_segment_id: u64,
    cur_segment_offset: u64, // bytes already flushed to current segment
    file: File,
}

impl FileWalStore {
    /// Open with the default 64 MiB segment size.
    pub fn open(dir: PathBuf, rg_id: u64, buffer_size: usize) -> Self {
        Self::open_with(dir, rg_id, buffer_size, DEFAULT_SEGMENT_SIZE)
    }

    /// Open with an explicit `buffer_size` (bytes) and `segment_size` (bytes).
    pub fn open_with(
        dir: PathBuf,
        rg_id: u64,
        buffer_size: usize,
        segment_size: u64,
    ) -> Self {
        let _ = rg_id; // reserved for future per-rg naming / debug
        std::fs::create_dir_all(&dir).expect("create wal dir");
        let (cur_segment_id, cur_segment_offset) = scan_tail(&dir);
        let next_lsn_init = first_lsn_after_replay(&dir);
        let file = open_segment(&dir, cur_segment_id);
        Self {
            dir,
            buffer_size,
            segment_size,
            inner: Mutex::new(Inner {
                buffer: Vec::new(),
                cur_segment_id,
                cur_segment_offset,
                file,
            }),
            next_lsn: AtomicU64::new(next_lsn_init),
            stop: AtomicBool::new(false),
            sync_handle: Mutex::new(None),
        }
    }

    /// Start the per-WalStore background sync thread. Idempotent. The thread
    /// holds an `Arc<Self>` clone and stops on drop / when `stop` is set.
    pub fn start_sync(self: &Arc<Self>, interval: Duration) {
        let mut handle = self.sync_handle.lock();
        if handle.is_some() {
            return;
        }
        let this = self.clone();
        let h = thread::Builder::new()
            .name(format!("wal-sync-{}", self.dir.display()))
            .spawn(move || loop {
                if this.stop.load(Ordering::SeqCst) {
                    break;
                }
                let _ = this.sync();
                thread::sleep(interval);
            })
            .expect("spawn wal sync thread");
        *handle = Some(h);
    }

    /// Flush the buffer to the current segment; rotate when it would exceed
    /// `segment_size`. Caller holds `inner` lock.
    fn flush_locked(dir: &Path, segment_size: u64, inner: &mut Inner) -> StorageResult<()> {
        if inner.buffer.is_empty() {
            return Ok(());
        }

        // switch to next segment
        if inner.cur_segment_offset + inner.buffer.len() as u64 > segment_size
            && inner.cur_segment_offset > 0
        {
            inner.file.sync_all()?;
            inner.cur_segment_id += 1;
            inner.cur_segment_offset = 0;
            inner.file = open_segment(dir, inner.cur_segment_id);
        }
        inner.file.write_all(&inner.buffer)?;
        inner.file.sync_all()?;
        inner.cur_segment_offset += inner.buffer.len() as u64;
        inner.buffer.clear();
        Ok(())
    }
}

impl WalStore for FileWalStore {
    fn append(&self, shard_id: ShardId, batch: &WriteBatch) -> StorageResult<Lsn> {
        let lsn = self.next_lsn.fetch_add(1, Ordering::SeqCst);
        let payload = serialize::encode_write_batch(batch);
        let entry = encode_entry(shard_id, lsn, WalPayloadKind::Write, &payload);
        let mut inner = self.inner.lock();
        inner.buffer.extend_from_slice(&entry);
        if inner.buffer.len() >= self.buffer_size {
            Self::flush_locked(&self.dir, self.segment_size, &mut inner)?;
        }
        Ok(lsn)
    }

    fn sync(&self) -> StorageResult<()> {
        let mut inner = self.inner.lock();
        Self::flush_locked(&self.dir, self.segment_size, &mut inner)
    }

    fn recover(
        &self,
    ) -> StorageResult<Box<dyn Iterator<Item = StorageResult<WalEntry>> + Send>> {
        // NOTE: recover does NOT sync first — it replays only what is already
        // fsync'd on disk. Crash-recovery semantics: an unsynced buffer is lost.
        let mut seg_ids = list_segment_ids(&self.dir)?;
        seg_ids.sort_unstable();
        let files: StorageResult<Vec<File>> = seg_ids
            .iter()
            .map(|id| File::open(segment_path(&self.dir, *id)).map_err(StorageError::from))
            .collect();
        Ok(Box::new(MultiSegmentIter::new(files?)))  
    }
}

impl Drop for FileWalStore {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.sync_handle.lock().take() {
            let _ = h.join();
        }
    }
}

fn open_segment(dir: &Path, seg_id: u64) -> File {
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(segment_path(dir, seg_id))
        .expect("open wal segment")
}

fn segment_path(dir: &Path, seg_id: u64) -> PathBuf {
    dir.join(format!("{seg_id}.wal"))
}

/// Return (highest existing segment_id, its byte size). Defaults to (0, 0).
fn scan_tail(dir: &Path) -> (u64, u64) {
    let mut ids = list_segment_ids(dir).unwrap_or_default();
    ids.sort_unstable();
    match ids.last() {
        None => (0, 0),
        Some(&id) => {
            let size = std::fs::metadata(segment_path(dir, id))
                .map(|m| m.len())
                .unwrap_or(0);
            (id, size)
        }
    }
}

fn list_segment_ids(dir: &Path) -> StorageResult<Vec<u64>> {
    let mut ids = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let s = name.to_string_lossy();
        if let Some(stem) = s.strip_suffix(".wal") {
            if let Ok(id) = stem.parse::<u64>() {
                ids.push(id);
            }
        }
    }
    Ok(ids)
}

/// The next lsn to assign = (max lsn seen across all segments) + 1.
fn first_lsn_after_replay(dir: &Path) -> u64 {
    let mut max_lsn: Option<u64> = None;
    let mut seg_ids = list_segment_ids(dir).unwrap_or_default();
    seg_ids.sort_unstable();
    for id in seg_ids {
        if let Ok(file) = File::open(segment_path(dir, id)) {
            let mut it = SegmentIter::new(file);
            while let Some(Ok(e)) = it.next() {
                max_lsn = Some(max_lsn.map_or(e.lsn, |m| m.max(e.lsn)));
            }
        }
    }
    max_lsn.map(|m| m + 1).unwrap_or(0)
}

// ── entry framing: `| len:u32 | shard_id:u64 | lsn:u64 | payload_type:u8 | payload |` ──

#[repr(u8)]
enum WalPayloadKind {
    Write = 0,
}

impl WalPayloadKind {
    fn from_u8(v: u8) -> StorageResult<Self> {
        match v {
            0 => Ok(WalPayloadKind::Write),
            other => Err(StorageError::WalCorrupted(format!(
                "unknown wal payload type {other}"
            ))),
        }
    }
}

fn encode_entry(
    shard_id: ShardId,
    lsn: Lsn,
    kind: WalPayloadKind,
    payload: &[u8],
) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&shard_id.to_be_bytes());
    body.extend_from_slice(&lsn.to_be_bytes());
    body.push(kind as u8);
    body.extend_from_slice(payload);
    let mut out = Vec::with_capacity(4 + body.len());
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend_from_slice(&body);
    out
}

/// Streaming iterator over one segment file: reads in 4 KiB chunks, decodes
/// one framed entry per `next()`.
struct SegmentIter {
    file: File,
    buf: Vec<u8>,
}

impl SegmentIter {
    fn new(file: File) -> Self {
        Self {
            file,
            buf: Vec::new(),
        }
    }
}

impl Iterator for SegmentIter {
    type Item = StorageResult<WalEntry>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            match try_decode_entry(&self.buf) {
                Ok(Some((entry, consumed))) => {
                    self.buf.drain(..consumed);
                    return Some(Ok(entry));
                }
                Ok(None) => {
                    let mut chunk = [0u8; 4096];
                    match self.file.read(&mut chunk) {
                        Ok(0) => return None, // EOF
                        Ok(n) => self.buf.extend_from_slice(&chunk[..n]),
                        Err(e) => return Some(Err(StorageError::IoError(e))),
                    }
                }
                Err(e) => return Some(Err(e)),
            }
        }
    }
}

/// Try to decode one entry from `buf`. `Ok(Some((entry, consumed)))` on success,
/// `Ok(None)` if not enough bytes yet, `Err` on corruption.
fn try_decode_entry(buf: &[u8]) -> StorageResult<Option<(WalEntry, usize)>> {
    if buf.len() < 4 {
        return Ok(None);
    }
    let body_len = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
    if buf.len() < 4 + body_len {
        return Ok(None); // need more bytes
    }
    let body = &buf[4..4 + body_len];
    if body.len() < 8 + 8 + 1 {
        return Err(StorageError::WalCorrupted("wal body too short".into()));
    }
    let mut pos = 0usize;
    let mut arr8 = [0u8; 8];
    arr8.copy_from_slice(&body[pos..pos + 8]);
    let shard_id = u64::from_be_bytes(arr8);
    pos += 8;
    arr8.copy_from_slice(&body[pos..pos + 8]);
    let lsn = u64::from_be_bytes(arr8);
    pos += 8;
    let kind = WalPayloadKind::from_u8(body[pos])?;
    pos += 1;
    let payload_bytes = &body[pos..];
    let payload = match kind {
        WalPayloadKind::Write => WalPayload::Write(serialize::decode_write_batch(payload_bytes)?),
    };
    Ok(Some((
        WalEntry {
            shard_id,
            lsn,
            payload,
        },
        4 + body_len,
    )))
}

/// Streaming iterator over multiple segment files in order.
struct MultiSegmentIter {
    files: Vec<File>,
    cur: Option<SegmentIter>,
}

impl MultiSegmentIter {
    fn new(mut files: Vec<File>) -> Self {
        let cur = if files.is_empty() {
            None
        } else {
            Some(SegmentIter::new(files.remove(0)))
        };
        Self { files, cur }
    }
}

impl Iterator for MultiSegmentIter {
    type Item = StorageResult<WalEntry>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(it) = &mut self.cur {
                if let Some(item) = it.next() {
                    return Some(item);
                }
            }
            if self.files.is_empty() {
                return None;
            }
            let next = self.files.remove(0);
            self.cur = Some(SegmentIter::new(next));
        }
    }
}
