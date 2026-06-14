use std::path::Path;

use bytes::Bytes;

use crate::errors::StorageResult;

/// File position, currently a byte offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Position {
    pub offset: u64,
}

impl From<&[u8]> for Position {
    fn from(value: &[u8]) -> Self {
        assert_eq!(value.len(), 8);
        Self {
            offset: u64::from_be_bytes(value.try_into().unwrap()),
        }
    }
}

/// Abstraction over block write targets (file, memory, etc.).
pub trait BlockWriter {
    fn write_block<T: AsRef<[u8]>>(&mut self, data: T) -> StorageResult<Position>;
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
    fn write_block<T: AsRef<[u8]>>(&mut self, data: T) -> StorageResult<Position> {
        let offset = self.next_offset;
        let size = data.as_ref().len() as u64;
        self.buf.extend_from_slice(data.as_ref());
        self.next_offset += size;
        Ok(Position { offset })
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
    fn write_block<T: AsRef<[u8]>>(&mut self, data: T) -> StorageResult<Position> {
        use std::io::Write;
        let offset = self.next_offset;
        let size = data.as_ref().len() as u64;
        self.file.write_all(data.as_ref())?;
        self.next_offset += size;
        Ok(Position { offset })
    }
}

/// Abstraction over block read sources (file, memory, etc.).
pub trait BlockReader {
    fn read_block(&self, position: &Position) -> StorageResult<Bytes>;
    fn block_size(&self) -> usize;
}

/// Allow Arc<R> to be used wherever R: BlockReader is expected.
impl<R: BlockReader> BlockReader for std::sync::Arc<R> {
    fn read_block(&self, position: &Position) -> StorageResult<Bytes> {
        (**self).read_block(position)
    }
    fn block_size(&self) -> usize {
        (**self).block_size()
    }
}

/// In-memory block reader for testing.
/// Reads blocks from a shared `Bytes` buffer using offset-only position.
pub struct InMemoryBlockReader {
    buf: Bytes,
    block_size: usize,
}

impl InMemoryBlockReader {
    pub fn new(buf: Bytes, block_size: usize) -> Self {
        Self { buf, block_size }
    }
}

impl BlockReader for InMemoryBlockReader {
    fn read_block(&self, position: &Position) -> StorageResult<Bytes> {
        let start = position.offset as usize;
        if start > self.buf.len() {
            return Err(crate::errors::StorageError::InvalidKey(
                "position out of bounds".into(),
            ));
        }
        let end = (start + self.block_size).min(self.buf.len());
        Ok(self.buf.slice(start..end))
    }

    fn block_size(&self) -> usize {
        self.block_size
    }
}

/// File-based block reader for production.
/// Reads fixed-size blocks via positional reads (`pread`) — thread-safe, no seek.
pub struct FileBlockReader {
    file: std::sync::Arc<std::fs::File>,
    block_size: usize,
}

impl FileBlockReader {
    pub fn open(path: &Path, block_size: usize) -> StorageResult<Self> {
        let file = std::fs::File::open(path)?;
        Ok(Self {
            file: std::sync::Arc::new(file),
            block_size,
        })
    }
}

impl BlockReader for FileBlockReader {
    fn read_block(&self, position: &Position) -> StorageResult<Bytes> {
        use std::os::unix::fs::FileExt;
        let mut buf = vec![0u8; self.block_size];
        self.file.read_exact_at(&mut buf, position.offset)?;
        Ok(Bytes::from(buf))
    }

    fn block_size(&self) -> usize {
        self.block_size
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Position only contains offset; block size is known by reader/sst_writer.
    #[test]
    fn test_position_is_offset_only() {
        let p = Position { offset: 42 };
        assert_eq!(p.offset, 42);
    }

    // BlockReader must report the block_size it was constructed with.
    #[test]
    fn test_reader_reports_block_size() {
        let reader = InMemoryBlockReader::new(Bytes::new(), 4096);
        assert_eq!(reader.block_size(), 4096);
    }

    // Reader reads exactly block_size bytes from the given position.
    #[test]
    fn test_reader_reads_fixed_size_block() -> StorageResult<()> {
        let block_size = 4;
        let data = vec![1u8, 2, 3, 4, 5, 6, 7, 8];
        let reader = InMemoryBlockReader::new(Bytes::from(data), block_size);

        let b0 = reader.read_block(&Position { offset: 0 })?;
        let b1 = reader.read_block(&Position { offset: 4 })?;

        assert_eq!(b0.as_ref(), &[1, 2, 3, 4]);
        assert_eq!(b1.as_ref(), &[5, 6, 7, 8]);
        Ok(())
    }

    // Last block may be shorter than block_size; reader returns what is available.
    #[test]
    fn test_reader_handles_short_last_block() -> StorageResult<()> {
        let block_size = 4;
        let data = vec![0u8, 0, 0, 0, 9, 8, 7];
        let reader = InMemoryBlockReader::new(Bytes::from(data), block_size);

        let b = reader.read_block(&Position { offset: 4 })?;
        assert_eq!(b.as_ref(), &[9, 8, 7]);
        Ok(())
    }

    // InMemoryBlockWriter is raw I/O: appends bytes, returns Position.
    #[test]
    fn test_in_memory_writer_appends_and_returns_position() -> StorageResult<()> {
        let mut writer = InMemoryBlockWriter::new();

        let p1 = writer.write_block(vec![1u8, 2, 3])?;
        let p2 = writer.write_block(vec![4u8, 5])?;

        assert_eq!(p1, Position { offset: 0 });
        assert_eq!(p2, Position { offset: 3 });
        assert_eq!(writer.as_slice(), &[1, 2, 3, 4, 5]);
        Ok(())
    }

    // Writer + reader round-trip with same-size blocks.
    #[test]
    fn test_writer_reader_round_trip() -> StorageResult<()> {
        let block_size = 4;
        let mut writer = InMemoryBlockWriter::new();

        let p1 = writer.write_block(vec![0u8, 1, 2, 3])?;
        let p2 = writer.write_block(vec![3u8, 2, 1, 0])?;

        assert_eq!(p1, Position { offset: 0 });
        assert_eq!(p2, Position { offset: 4 });

        let reader = InMemoryBlockReader::new(Bytes::from(writer.into_inner()), block_size);
        let b1 = reader.read_block(&p1)?;
        let b2 = reader.read_block(&p2)?;

        assert_eq!(b1.as_ref(), &[0, 1, 2, 3]);
        assert_eq!(b2.as_ref(), &[3, 2, 1, 0]);
        Ok(())
    }
}
