use std::path::Path;

use bytes::Bytes;

use crate::errors::StorageResult;

pub const BLOCK_SIZE: usize = 64 * 1024;

/// Handle to a block written to the SST file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockHandle {
    pub offset: u64,
    pub size: u32,
}

/// Abstraction over block write targets (file, memory, etc.).
pub trait BlockWriter {
    fn write_block(&mut self, data: Bytes) -> StorageResult<BlockHandle>;
}

/// In-memory block writer for testing.
/// Appends blocks to a Vec<u8>, returns offset/size handles.
pub struct InMemoryBlockWriter {
    buf: Vec<u8>,
    next_offset: u64,
}

impl InMemoryBlockWriter {
    pub fn new() -> Self {
        Self {
            buf: Vec::new(),
            next_offset: 0,
        }
    }

    pub fn into_inner(self) -> Vec<u8> {
        self.buf
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.buf
    }
}

impl BlockWriter for InMemoryBlockWriter {
    fn write_block(&mut self, data: Bytes) -> StorageResult<BlockHandle> {
        let offset = self.next_offset;
        let size = data.len() as u32;
        self.buf.extend_from_slice(&data);
        self.next_offset += size as u64;
        Ok(BlockHandle { offset, size })
    }
}

/// File-based block writer for production.
pub struct FileBlockWriter {
    file: std::fs::File,
    next_offset: u64,
}

impl FileBlockWriter {
    pub fn create(path: &Path) -> StorageResult<Self> {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)?;
        Ok(Self { file, next_offset: 0 })
    }
}

impl BlockWriter for FileBlockWriter {
    fn write_block(&mut self, data: Bytes) -> StorageResult<BlockHandle> {
        use std::io::Write;
        let offset = self.next_offset;
        let size = data.len() as u32;
        self.file.write_all(&data)?;
        self.next_offset += size as u64;
        Ok(BlockHandle { offset, size })
    }
}

/// Abstraction over block read sources (file, memory, etc.).
pub trait BlockReader {
    fn read_block(&self, handle: BlockHandle) -> StorageResult<Bytes>;
}

/// In-memory block reader for testing.
/// Reads blocks from a shared `Bytes` buffer using offset/size handles.
pub struct InMemoryBlockReader {
    buf: Bytes,
}

impl InMemoryBlockReader {
    pub fn new(buf: Bytes) -> Self {
        Self { buf }
    }
}

impl BlockReader for InMemoryBlockReader {
    fn read_block(&self, handle: BlockHandle) -> StorageResult<Bytes> {
        let start = handle.offset as usize;
        let end = start + handle.size as usize;
        if end > self.buf.len() {
            return Err(crate::errors::StorageError::InvalidKey(
                "block handle out of bounds".into(),
            ));
        }
        Ok(self.buf.slice(start..end))
    }
}

/// File-based block reader for production.
pub struct FileBlockReader {
    file: std::fs::File,
}

impl FileBlockReader {
    pub fn open(path: &Path) -> StorageResult<Self> {
        let file = std::fs::OpenOptions::new().read(true).open(path)?;
        Ok(Self { file })
    }
}

impl BlockReader for FileBlockReader {
    fn read_block(&self, handle: BlockHandle) -> StorageResult<Bytes> {
        use std::io::{Read, Seek};
        let mut file = &self.file;
        file.seek(std::io::SeekFrom::Start(handle.offset))?;
        let mut buf = vec![0u8; handle.size as usize];
        file.read_exact(&mut buf)?;
        Ok(Bytes::from(buf))
    }
}
