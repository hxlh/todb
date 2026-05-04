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
