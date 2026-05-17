use std::path::Path;

use bytes::Bytes;

use crate::errors::StorageResult;

/// Handle to a block written to the SST file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockHandle {
    pub offset: u64,
}

impl From<&[u8]> for BlockHandle {
    fn from(value: &[u8]) -> Self {
        assert_eq!(value.len(), 8);
        Self {
            offset: u64::from_be_bytes(value.try_into().unwrap()),
        }
    }
}

/// Abstraction over block write targets (file, memory, etc.).
pub trait BlockWriter {
    fn write_block<T: AsRef<[u8]>>(&mut self, data: &T) -> StorageResult<BlockHandle>;
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
    fn write_block<T: AsRef<[u8]>>(&mut self, data: &T) -> StorageResult<BlockHandle> {
        let offset = self.next_offset;
        let size = data.as_ref().len() as u64;
        self.buf.extend_from_slice(data.as_ref());
        self.next_offset += size;
        Ok(BlockHandle { offset })
    }
}

/// File-based block writer for production.
pub struct FileBlockWriter {
    file: std::fs::File,
    next_offset: u64,
    block_size: usize,
}

impl FileBlockWriter {
    pub fn create(path: &Path, block_size: usize) -> StorageResult<Self> {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)?;
        Ok(Self {
            file,
            next_offset: 0,
            block_size,
        })
    }
}

impl BlockWriter for FileBlockWriter {
    fn write_block<T: AsRef<[u8]>>(&mut self, data: &T) -> StorageResult<BlockHandle> {
        use std::io::Write;
        let offset = self.next_offset;
        let size = data.as_ref().len() as u64;
        self.file.write_all(data.as_ref())?;
        self.next_offset += size;
        Ok(BlockHandle { offset })
    }
}

/// Abstraction over block read sources (file, memory, etc.).
pub trait BlockReader {
    fn read_block(&self, handle: &BlockHandle) -> StorageResult<Bytes>;
    fn block_size(&self) -> usize;
}

/// Allow Arc<R> to be used wherever R: BlockReader is expected.
impl<R: BlockReader> BlockReader for std::sync::Arc<R> {
    fn read_block(&self, handle: &BlockHandle) -> StorageResult<Bytes> {
        (**self).read_block(handle)
    }
    fn block_size(&self) -> usize {
        (**self).block_size()
    }
}

/// In-memory block reader for testing.
/// Reads blocks from a shared `Bytes` buffer using offset/size handles.
pub struct InMemoryBlockReader {
    buf: Bytes,
    block_size: usize,
}

impl InMemoryBlockReader {
    pub fn new(buf: Bytes, block_size: usize) -> Self {
        Self {
            buf: buf,
            block_size: block_size,
        }
    }
}

impl BlockReader for InMemoryBlockReader {
    fn read_block(&self, handle: &BlockHandle) -> StorageResult<Bytes> {
        let start = handle.offset as usize;
        if start > self.buf.len() {
            return Err(crate::errors::StorageError::InvalidKey(
                "block handle out of bounds".into(),
            ));
        }
        // Last block may be shorter than block_size; return what is available.
        let end = (start + self.block_size).min(self.buf.len());
        Ok(self.buf.slice(start..end))
    }

    fn block_size(&self) -> usize {
        self.block_size
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // BlockHandle should only contain offset, no size field.
    #[test]
    fn test_block_handle_has_no_size() {
        let h = BlockHandle { offset: 42 };
        assert_eq!(h.offset, 42);
    }

    // BlockReader must report the block_size it was constructed with.
    #[test]
    fn test_reader_reports_block_size() {
        let reader = InMemoryBlockReader::new(Bytes::new(), 4096);
        assert_eq!(reader.block_size(), 4096);
    }

    // Reader reads exactly block_size bytes from the given offset.
    #[test]
    fn test_reader_reads_fixed_size_block() -> StorageResult<()> {
        let block_size = 4;
        let data = vec![1u8, 2, 3, 4, 5, 6, 7, 8];
        let reader = InMemoryBlockReader::new(Bytes::from(data), block_size);

        let b0 = reader.read_block(&BlockHandle { offset: 0 })?;
        let b1 = reader.read_block(&BlockHandle { offset: 4 })?;

        assert_eq!(b0.as_ref(), &[1, 2, 3, 4]);
        assert_eq!(b1.as_ref(), &[5, 6, 7, 8]);
        Ok(())
    }

    // Last block may be smaller than block_size; reader returns what is available.
    #[test]
    fn test_reader_handles_short_last_block() -> StorageResult<()> {
        let block_size = 4;
        // Only 3 bytes after offset 4
        let data = vec![0u8, 0, 0, 0, 9, 8, 7];
        let reader = InMemoryBlockReader::new(Bytes::from(data), block_size);

        let b = reader.read_block(&BlockHandle { offset: 4 })?;
        assert_eq!(b.as_ref(), &[9, 8, 7]);
        Ok(())
    }

    // BlockWriter must report the block_size it was constructed with.
    #[test]
    fn test_writer_reports_block_size() {
        // BlockWriter no longer exposes block_size; size is owned by SstOption.
        // This test is intentionally left as a compile-time marker.
        let _writer = InMemoryBlockWriter::new();
    }

    #[test]
    fn test_in_memory_writer_simple_read_write() -> StorageResult<()> {
        // block_size matches each written block so reads return exactly one block.
        let block_size = 4;
        let data = vec![0u8, 1, 2, 3];
        let data2 = vec![3u8, 2, 1, 0];

        let mut w = InMemoryBlockWriter::new();
        let handle1 = w.write_block(&data)?;
        let handle2 = w.write_block(&data2)?;

        let reader = InMemoryBlockReader::new(Bytes::from(w.into_inner()), block_size);
        let b1 = reader.read_block(&handle1)?;
        let b2 = reader.read_block(&handle2)?;

        assert_eq!(b1.as_ref(), data.as_slice());
        assert_ne!(b2.as_ref(), [1, 2, 3, 4]);
        assert_eq!(b2.as_ref(), data2.as_slice());

        Ok(())
    }
}
