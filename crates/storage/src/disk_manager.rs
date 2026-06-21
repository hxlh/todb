use crate::{
    block::{BlockWriter, FileBlockReader, FileBlockWriter, Position},
    builder::SstFooter,
    errors::{StorageError, StorageResult},
};
use std::os::unix::fs::FileExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

pub type SstId = u64;

/// A `BlockWriter` that also carries its `sst_id` and tracks file size.
/// Produced by [`DiskManager::create_sst`]; fed to `SstBuilder`.
pub struct SstFileWriter {
    sst_id: SstId,
    inner: FileBlockWriter,
}

impl SstFileWriter {
    pub fn new(sst_id: SstId, inner: FileBlockWriter) -> Self {
        Self { sst_id, inner }
    }

    pub fn sst_id(&self) -> SstId {
        self.sst_id
    }

    pub fn file_size(&self) -> u64 {
        self.inner.file_size()
    }
}

impl BlockWriter for SstFileWriter {
    fn write_block<T: AsRef<[u8]>>(&mut self, data: T) -> StorageResult<Position> {
        self.inner.write_block(data)
    }
}

/// An SST opened for reading: its block reader plus the footer decoded from
/// the file tail. Symmetric to [`SstFileWriter`].
pub struct SstFileReader {
    pub reader: FileBlockReader,
    pub footer: SstFooter,
}

/// Manages SST files globally: monotonic `sst_id` allocation, path layout
/// (`{data_dir}/{sst_id}.sst`), Writer/Reader factory, and lifecycle. Owned
/// by [`LsmEngine`](crate::lsm_engine::LsmEngine); `sst_id`s are globally
/// unique and shared across all shards.
pub struct DiskManager {
    data_dir: PathBuf,
    block_size: usize,
    next_sst_id: AtomicU64,
}

impl DiskManager {
    pub fn new(data_dir: PathBuf, block_size: usize) -> Self {
        Self {
            data_dir,
            block_size,
            next_sst_id: AtomicU64::new(1),
        }
    }

    pub fn block_size(&self) -> usize {
        self.block_size
    }

    fn sst_path(&self, sst_id: SstId) -> PathBuf {
        self.data_dir.join(format!("{sst_id}.sst"))
    }

    /// Allocate a new SST and return a Writer over it.
    pub fn create_sst(&self) -> StorageResult<SstFileWriter> {
        let sst_id = self.next_sst_id.fetch_add(1, Ordering::SeqCst);
        let path = self.sst_path(sst_id);
        std::fs::create_dir_all(path.parent().expect("sst path has a parent"))?;
        let inner = FileBlockWriter::create(&path, self.block_size)?;
        Ok(SstFileWriter::new(sst_id, inner))
    }

    /// Open an SST by id: locate its file, decode the footer from the tail,
    /// and return the reader + footer. The footer is self-describing length
    /// (a `body_len:u32` trailer), so we read the last 4 bytes first.
    pub fn open(&self, sst_id: SstId) -> StorageResult<SstFileReader> {
        let path = self.sst_path(sst_id);
        let file = std::fs::File::open(&path)
            .map_err(|_| StorageError::NotFound(format!("sst {sst_id}")))?;
        let file_size = file.metadata()?.len() as usize;
        if file_size < 4 {
            return Err(StorageError::InvalidValue(format!(
                "sst {sst_id}: file too small for footer trailer"
            )));
        }
        // Read the body_len trailer (last 4 bytes), then the body.
        let mut trailer = [0u8; 4];
        file.read_exact_at(&mut trailer, (file_size - 4) as u64)?;
        let body_len = u32::from_be_bytes(trailer) as usize;
        let footer_total = body_len + 4;
        if file_size < footer_total {
            return Err(StorageError::InvalidValue(format!(
                "sst {sst_id}: file too small for footer body"
            )));
        }
        let mut fbuf = vec![0u8; footer_total];
        file.read_exact_at(&mut fbuf, (file_size - footer_total) as u64)?;
        let footer = SstFooter::decode(&fbuf)?;
        let reader = FileBlockReader::from_file(file, self.block_size);
        Ok(SstFileReader { reader, footer })
    }

    /// List all sst_ids (scans the data dir).
    pub fn list_ssts(&self) -> StorageResult<Vec<SstId>> {
        let mut ids = Vec::new();
        let Ok(entries) = std::fs::read_dir(&self.data_dir) else {
            return Ok(ids); // data dir absent -> empty
        };
        for entry in entries {
            let path = entry?.path();
            if path.extension().and_then(|s| s.to_str()) != Some("sst") {
                continue;
            }
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                if let Ok(id) = stem.parse::<SstId>() {
                    ids.push(id);
                }
            }
        }
        Ok(ids)
    }

    /// Delete an SST by id (idempotent: a missing file is not an error).
    pub fn remove(&self, sst_id: SstId) -> StorageResult<()> {
        let _ = std::fs::remove_file(self.sst_path(sst_id));
        Ok(())
    }
}
