use bytes::Bytes;
use tracing::debug;

use crate::{
    block::Position,
    builder::{
        data_block::DataBlockBuilder,
        index_block::IndexBlockBuilder,
        sst_writer::SstWriter,
    },
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
/// Output: SST file with a B+tree index, written via `SstWriter`.
///
/// Generic over `SstWriter` so it works with both file and memory targets.
pub struct SstBuilder<S: SstWriter> {
    writer: S,
    data_builder: DataBlockBuilder,
    index_builders: Vec<IndexBlockBuilder>,
    option: SstOption,
}

impl<S: SstWriter> SstBuilder<S> {
    pub fn new(writer: S, option: SstOption) -> Self {
        Self {
            writer,
            data_builder: DataBlockBuilder::new(&option),
            index_builders: Vec::new(),
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
        let handle = self.writer.write_block(block)?;
        debug!("new data block");
        self.add_index_entry(0, end_key, handle)
    }

    fn add_index_entry(
        &mut self,
        level: usize,
        end_key: Bytes,
        position: Position,
    ) -> StorageResult<()> {
        if level >= self.index_builders.len() {
            self.index_builders
                .push(IndexBlockBuilder::new(&self.option));
        }
        if self.index_builders[level].would_exceed(&end_key, &position) {
            self.flush_index_block(level)?;
        }
        let builder = &mut self.index_builders[level];
        builder.add(end_key, position);

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
        let handle = self.writer.write_block(block)?;

        debug!("flush_index_block: level={}", level);
        self.add_index_entry(level + 1, end_key, handle)
    }

    /// Finalize the SST and return `(footer, writer)`.
    pub fn finish(mut self) -> StorageResult<(SstFooter, S)> {
        debug!("sst builder finish");

        self.flush_data_block()?;

        // Flush every existing index level bottom-up. flush_index_block may push
        // new higher-level builders, but the loop range is intentionally fixed to
        // the pre-flush snapshot — those new builders are already flushed
        // recursively and their last entry becomes the new root.
        for level in 0..self.index_builders.len() {
            if !self.index_builders[level].is_empty() {
                self.flush_index_block(level)?;
            }
        }

        let height = self.index_builders.len() as u32;

        let root_position = self
            .index_builders
            .last()
            .and_then(|b| b.last_entry())
            .map(|e| e.child)
            .unwrap_or(Position { offset: 0 });

        let footer = SstFooter {
            root_position,
            tree_height: height,
        };

        self.writer.write_footer(&footer)?;

        Ok((footer, self.writer))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SstFooter {
    pub root_position: Position,
    pub tree_height: u32,
}

impl SstFooter {
    pub const ENCODED_LEN: usize = 12;

    pub fn encode(&self) -> [u8; Self::ENCODED_LEN] {
        let mut buf = [0u8; Self::ENCODED_LEN];
        buf[0..8].copy_from_slice(&self.root_position.offset.to_be_bytes());
        buf[8..12].copy_from_slice(&self.tree_height.to_be_bytes());
        buf
    }

    pub fn decode(buf: &[u8]) -> StorageResult<Self> {
        if buf.len() < Self::ENCODED_LEN {
            return Err(StorageError::InvalidValue(
                "footer too short".into(),
            ));
        }
        Ok(Self {
            root_position: Position {
                offset: u64::from_be_bytes(buf[0..8].try_into().unwrap()),
            },
            tree_height: u32::from_be_bytes(buf[8..12].try_into().unwrap()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::InMemoryBlockWriter;
    use crate::builder::DefaultSstWriter;

    fn fixed_builder(block_size: usize) -> SstBuilder<DefaultSstWriter<InMemoryBlockWriter>> {
        let option = SstOption::default().block_size(block_size);
        SstBuilder::new(DefaultSstWriter::new(InMemoryBlockWriter::new(), &option), option)
    }

    fn make_key(i: u64) -> Bytes {
        Bytes::copy_from_slice(&i.to_be_bytes())
    }

    fn make_value(i: u64) -> Bytes {
        Bytes::from(format!("value_{}", i))
    }

    #[test]
    fn test_build_single_data_block() {
        let mut builder = fixed_builder(4096);
        for i in 0..10u64 {
            builder.add(make_key(i), make_value(i)).unwrap();
        }
        let (footer, _) = builder.finish().unwrap();
        assert!(footer.tree_height >= 1);
    }

    #[test]
    fn test_build_multi_block_with_index() {
        let mut builder = fixed_builder(256);
        for i in 0..10000u64 {
            builder.add(make_key(i), make_value(i)).unwrap();
        }
        let (footer, _) = builder.finish().unwrap();
        assert!(footer.tree_height >= 2);
    }

    #[test]
    fn test_build_produces_bytes() {
        let mut builder = fixed_builder(256);
        for i in 0..100u64 {
            builder.add(make_key(i), make_value(i)).unwrap();
        }
        let (footer, sst_writer) = builder.finish().unwrap();
        let writer = sst_writer.into_inner();
        let buf = writer.into_inner();
        assert!(!buf.is_empty());
        assert!(footer.root_position.offset < buf.len() as u64);
    }

    #[test]
    fn test_footer_encodes_root_offset_and_tree_height() {
        let footer = SstFooter {
            root_position: Position {
                offset: 0x0102_0304_0506_0708,
            },
            tree_height: 0x1112_1314,
        };

        let encoded = footer.encode();

        assert_eq!(encoded.len(), SstFooter::ENCODED_LEN);
        assert_eq!(&encoded[0..8], &0x0102_0304_0506_0708u64.to_be_bytes());
        assert_eq!(&encoded[8..12], &0x1112_1314u32.to_be_bytes());
        assert_eq!(SstFooter::decode(&encoded).unwrap(), footer);
    }

    #[test]
    fn test_builder_writes_footer_after_fixed_size_block_slots() {
        let block_size = 256;
        let mut builder = fixed_builder(block_size);
        for i in 0..100u64 {
            builder.add(make_key(i), make_value(i)).unwrap();
        }

        let (footer, sst_writer) = builder.finish().unwrap();
        let writer = sst_writer.into_inner();
        let buf = writer.into_inner();
        assert!(buf.len() > SstFooter::ENCODED_LEN);

        let footer_start = buf.len() - SstFooter::ENCODED_LEN;
        assert_eq!(footer_start % block_size, 0);
        assert_eq!(footer.root_position.offset as usize % block_size, 0);

        let decoded = SstFooter::decode(&buf[footer_start..]).unwrap();
        assert_eq!(decoded, footer);
    }

    #[test]
    fn test_empty_sst_writes_only_footer() {
        let builder = fixed_builder(256);
        let (footer, sst_writer) = builder.finish().unwrap();
        let writer = sst_writer.into_inner();
        let buf = writer.into_inner();
        assert_eq!(buf.len(), SstFooter::ENCODED_LEN);
        assert_eq!(footer.root_position, Position { offset: 0 });
        assert_eq!(footer.tree_height, 0);
        assert_eq!(SstFooter::decode(&buf).unwrap(), footer);
    }

    #[test]
    fn test_build_empty() {
        let builder = fixed_builder(256);
        let (footer, _) = builder.finish().unwrap();
        assert_eq!(footer.root_position, Position { offset: 0 });
    }
}
