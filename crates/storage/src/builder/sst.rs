use bytes::Bytes;

use crate::{
    block::{BlockHandle, BlockWriter},
    builder::{data_block::DataBlockBuilder, index_block::IndexBlockBuilder},
    errors::StorageResult,
};

/// Streaming SST builder.
///
/// Input: sorted `(key, value)` byte pairs.
/// Output: SST file with a B+tree index, written via `BlockWriter`.
///
/// Generic over `BlockWriter` so it works with both file and memory targets.
pub struct SstBuilder<W: BlockWriter> {
    writer: W,
    data_builder: DataBlockBuilder,
    index_builders: Vec<IndexBlockBuilder>,
}

impl<W: BlockWriter> SstBuilder<W> {
    pub fn new(writer: W) -> Self {
        Self {
            writer,
            data_builder: DataBlockBuilder::new(),
            index_builders: Vec::new(),
        }
    }

    /// Add one sorted entry. Caller must guarantee keys are in ascending order.
    pub fn add(&mut self, key: Bytes, value: Bytes) -> StorageResult<()> {
        if self.data_builder.would_exceed(&key, &value) {
            self.flush_data_block()?;
        }
        self.data_builder.add(key, value);
        Ok(())
    }

    fn flush_data_block(&mut self) -> StorageResult<()> {
        if self.data_builder.is_empty() {
            return Ok(());
        }
        let end_key = self
            .data_builder
            .last_key()
            .cloned()
            .unwrap_or_else(|| Bytes::new());
        let block = self.data_builder.finish();
        let handle = self.writer.write_block(block)?;
        self.add_index_entry(0, end_key, handle)
    }

    fn add_index_entry(
        &mut self,
        level: usize,
        end_key: Bytes,
        handle: BlockHandle,
    ) -> StorageResult<()> {
        if level >= self.index_builders.len() {
            self.index_builders.push(IndexBlockBuilder::new());
        }
        if self.index_builders[level].would_exceed(&end_key, &handle) {
            self.flush_index_block(level)?;
        }
        let builder = &mut self.index_builders[level];
        builder.add(end_key, handle);
        Ok(())
    }

    fn flush_index_block(&mut self, level: usize) -> StorageResult<()> {
        let builder = &mut self.index_builders[level];
        if builder.is_empty() {
            return Ok(());
        }
        let end_key = builder
            .last_entry()
            .map(|e| e.end_key.clone())
            .unwrap_or_else(|| Bytes::new());
        let block = builder.finish();
        let handle = self.writer.write_block(block)?;
        self.add_index_entry(level + 1, end_key, handle)
    }

    /// Finalize the SST and return `(footer, writer)`.
    pub fn finish(mut self) -> StorageResult<(SstFooter, W)> {
        self.flush_data_block()?;

        // Flush all pending index levels bottom-up
        for level in 0..self.index_builders.len() {
            if !self.index_builders[level].is_empty() {
                self.flush_index_block(level)?;
            }
        }

        if let Some(builder) = self.index_builders.last() {
            if !builder.is_empty() {
                self.flush_index_block(self.index_builders.len() - 1)?;
            }
        }

        let root_handle = self
            .index_builders
            .last()
            .and_then(|b| b.last_entry())
            .map(|e| e.child)
            .unwrap_or(BlockHandle { offset: 0, size: 0 });

        let height = self.index_builders.len() as u32 + 1;

        let footer = SstFooter {
            root_handle,
            tree_height: height,
        };

        Ok((footer, self.writer))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SstFooter {
    pub root_handle: BlockHandle,
    pub tree_height: u32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::InMemoryBlockWriter;

    fn make_key(i: u64) -> Bytes {
        Bytes::copy_from_slice(&i.to_be_bytes())
    }

    fn make_value(i: u64) -> Bytes {
        Bytes::from(format!("value_{}", i))
    }

    #[test]
    fn test_build_single_data_block() {
        let writer = InMemoryBlockWriter::new();
        let mut builder = SstBuilder::new(writer);

        for i in 0..10u64 {
            builder.add(make_key(i), make_value(i)).unwrap();
        }

        let (footer, _writer) = builder.finish().unwrap();
        assert!(footer.tree_height >= 1);
    }

    #[test]
    fn test_build_multi_block_with_index() {
        let writer = InMemoryBlockWriter::new();
        let mut builder = SstBuilder::new(writer);

        // Write enough entries to trigger multiple data blocks
        for i in 0..10000u64 {
            builder.add(make_key(i), make_value(i)).unwrap();
        }

        let (footer, _writer) = builder.finish().unwrap();
        assert!(footer.tree_height >= 2);
    }

    #[test]
    fn test_build_produces_bytes() {
        let writer = InMemoryBlockWriter::new();
        let mut builder = SstBuilder::new(writer);

        for i in 0..100u64 {
            builder.add(make_key(i), make_value(i)).unwrap();
        }

        let (footer, writer) = builder.finish().unwrap();
        let buf = writer.into_inner();
        assert!(!buf.is_empty());
        // Root block should be within the written bytes
        let root_end = footer.root_handle.offset as usize + footer.root_handle.size as usize;
        assert!(root_end <= buf.len());
    }

    #[test]
    fn test_build_empty() {
        let writer = InMemoryBlockWriter::new();
        let builder = SstBuilder::new(writer);
        let (footer, _writer) = builder.finish().unwrap();
        assert_eq!(footer.root_handle, BlockHandle { offset: 0, size: 0 });
    }
}
