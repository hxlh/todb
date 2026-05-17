use bytes::Bytes;
use tracing::debug;

use crate::{
    block::{BlockHandle, BlockWriter},
    builder::{data_block::DataBlockBuilder, index_block::IndexBlockBuilder},
    errors::{StorageError, StorageResult},
};

/// Options for SST construction. Use `SstOption::default()` for sensible defaults,
/// then override fields via builder-style methods.
#[derive(Debug, Clone)]
pub struct SstOption {
    pub block_size: usize,
}

impl Default for SstOption {
    fn default() -> Self {
        Self {
            block_size: 64 * 1024,
        }
    }
}

impl SstOption {
    pub fn block_size(mut self, size: usize) -> Self {
        self.block_size = size;
        self
    }
}

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
    option: SstOption,
}

impl<W: BlockWriter> SstBuilder<W> {
    pub fn new(writer: W, option: SstOption) -> Self {
        Self {
            data_builder: DataBlockBuilder::new(&option),
            index_builders: Vec::new(),
            writer,
            option,
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
        let handle = self.writer.write_block(&block)?;
        debug!("new data block");
        self.add_index_entry(0, end_key, handle)
    }

    fn add_index_entry(
        &mut self,
        level: usize,
        end_key: Bytes,
        handle: BlockHandle,
    ) -> StorageResult<()> {
        if level >= self.index_builders.len() {
            self.index_builders
                .push(IndexBlockBuilder::new(&self.option));
        }
        if self.index_builders[level].would_exceed(&end_key, &handle) {
            self.flush_index_block(level)?;
        }
        let builder = &mut self.index_builders[level];
        builder.add(end_key, handle);

        debug!("add_index_entry: level={}", level);
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
        let handle = self.writer.write_block(&block)?;

        debug!("flush_index_block: level={}", level);
        self.add_index_entry(level + 1, end_key, handle)
    }

    /// Finalize the SST and return `(footer, writer)`.
    pub fn finish(mut self) -> StorageResult<(SstFooter, W)> {
        debug!("sst builder finish");

        self.flush_data_block()?;

        // Flush all pending index levels bottom-up
        for level in 0..self.index_builders.len() {
            if !self.index_builders[level].is_empty() {
                self.flush_index_block(level)?;
            }
        }

        let height = self.index_builders.len() as u32;

        let Some(root_handle) = self
            .index_builders
            .last()
            .and_then(|b| b.last_entry())
            .map(|e| e.child)
        else {
            return Ok((
                SstFooter {
                    tree_height: height,
                    root_handle: BlockHandle { offset: 0 },
                },
                self.writer,
            ));
        };

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

    fn default_builder() -> SstBuilder<InMemoryBlockWriter> {
        SstBuilder::new(InMemoryBlockWriter::new(), SstOption::default())
    }

    #[test]
    fn test_build_single_data_block() {
        let mut builder = default_builder();
        for i in 0..10u64 {
            builder.add(make_key(i), make_value(i)).unwrap();
        }
        let (footer, _writer) = builder.finish().unwrap();
        assert!(footer.tree_height >= 1);
    }

    #[test]
    fn test_build_multi_block_with_index() {
        let mut builder = default_builder();
        for i in 0..10000u64 {
            builder.add(make_key(i), make_value(i)).unwrap();
        }
        let (footer, _writer) = builder.finish().unwrap();
        assert!(footer.tree_height >= 2);
    }

    #[test]
    fn test_build_produces_bytes() {
        let mut builder = default_builder();
        for i in 0..100u64 {
            builder.add(make_key(i), make_value(i)).unwrap();
        }
        let (footer, writer) = builder.finish().unwrap();
        let buf = writer.into_inner();
        assert!(!buf.is_empty());
        assert!(footer.root_handle.offset < buf.len() as u64);
    }

    #[test]
    fn test_build_empty() {
        let builder = default_builder();
        let (footer, _writer) = builder.finish().unwrap();
        assert_eq!(footer.root_handle, BlockHandle { offset: 0 });
    }

    #[test]
    fn test_custom_block_size() {
        // Small block_size forces multiple data blocks with fewer entries.
        let option = SstOption::default().block_size(256);
        let mut builder = SstBuilder::new(InMemoryBlockWriter::new(), option);
        for i in 0..200u64 {
            builder.add(make_key(i), make_value(i)).unwrap();
        }
        let (footer, _writer) = builder.finish().unwrap();
        assert!(footer.tree_height >= 2);
    }
}
