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
    first_key: Option<Bytes>,
    last_key: Option<Bytes>,
}

impl<S: SstWriter> SstBuilder<S> {
    pub fn new(writer: S, option: SstOption) -> Self {
        Self {
            writer,
            data_builder: DataBlockBuilder::new(&option),
            index_builders: Vec::new(),
            option,
            first_key: None,
            last_key: None,
        }
    }
    /// Add one sorted entry. Caller must guarantee keys are in ascending order.
    pub fn add(&mut self, key: Bytes, value: Bytes) -> StorageResult<()> {
        if self.first_key.is_none() {
            self.first_key = Some(key.clone());
        }
        self.last_key = Some(key.clone());
        if self.data_builder.would_exceed(&key, &value) {
            self.flush_data_block()?;
        }
        self.data_builder.add(key, value);
        Ok(())
    }

    /// Add a delete tombstone. Caller must guarantee keys are in ascending order.
    pub fn add_delete(&mut self, key: Bytes) -> StorageResult<()> {
        if self.first_key.is_none() {
            self.first_key = Some(key.clone());
        }
        self.last_key = Some(key.clone());
        if self.data_builder.would_exceed(&key, &Bytes::new()) {
            self.flush_data_block()?;
        }
        self.data_builder.add_delete(key);
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

        // Flush every index level bottom-up. The loop range is fixed to the
        // pre-flush builder count; flush_index_block promotes a summary entry to
        // the next level and may recursively create new higher levels, which are
        // flushed in turn by that recursion.
        let pre_flush_levels = self.index_builders.len();
        for level in 0..pre_flush_levels {
            if !self.index_builders[level].is_empty() {
                self.flush_index_block(level)?;
            }
        }

        // The loop above never visits a level created during flushing (its range
        // was fixed to the pre-flush count). If that new topmost level
        // accumulated more than one entry, those entries are sibling top-level
        // blocks with no parent block above them — `root_position` below would
        // reach only the last, orphaning the rest and corrupting any tree of
        // height >= 4. Flush the topmost into a single root block; the promoted
        // summary becomes the sole entry of a new topmost, so one flush suffices
        // (the topmost never exceeds block capacity — add_index_entry flushes on
        // would_exceed).
        if self
            .index_builders
            .last()
            .map_or(false, |b| b.entry_count() > 1)
        {
            let top = self.index_builders.len() - 1;
            self.flush_index_block(top)?;
        }

        let height = self.index_builders.len() as u32;

        let root_position = self
            .index_builders
            .last()
            .and_then(|b| b.last_entry())
            .map(|e| e.child)
            .unwrap_or(Position { offset: 0 });

        let footer = SstFooter {
            root_index_block_position: root_position,
            tree_height: height,
            first_key: self.first_key.clone().unwrap_or_default(),
            last_key: self.last_key.clone().unwrap_or_default(),
        };

        self.writer.write_footer(&footer)?;

        Ok((footer, self.writer))
    }
}

/// Trailing metadata of an SST file. Self-describing length: the body is
/// followed by a `body_len:u32` trailer so the reader locates the footer by
/// reading the last 4 bytes first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SstFooter {
    pub root_index_block_position: Position,
    pub tree_height: u32,
    /// Smallest key in the SST (empty for an empty SST).
    pub first_key: Bytes,
    /// Largest key in the SST (empty for an empty SST).
    pub last_key: Bytes,
}

impl SstFooter {
    /// Minimum footer size: body with empty keys (20B) + trailer (4B).
    pub const MIN_LEN: usize = 24;

    /// Encode as `body ++ [body_len:u32]`.
    pub fn encode(&self) -> Vec<u8> {
        let mut body = Vec::with_capacity(20 + self.first_key.len() + self.last_key.len());
        body.extend_from_slice(&self.root_index_block_position.offset.to_be_bytes());
        body.extend_from_slice(&self.tree_height.to_be_bytes());
        body.extend_from_slice(&(self.first_key.len() as u32).to_be_bytes());
        body.extend_from_slice(&(self.last_key.len() as u32).to_be_bytes());
        body.extend_from_slice(&self.first_key);
        body.extend_from_slice(&self.last_key);
        let mut out = body;
        out.extend_from_slice(&(out.len() as u32).to_be_bytes()); // trailer = body_len
        out
    }

    /// Decode from a buffer that is `body ++ [body_len:u32]` (the full footer
    /// region read from the file tail).
    pub fn decode(buf: &[u8]) -> StorageResult<Self> {
        if buf.len() < 4 {
            return Err(StorageError::InvalidValue(
                "footer too short for trailer".into(),
            ));
        }
        let body_len = u32::from_be_bytes(buf[buf.len() - 4..].try_into().unwrap()) as usize;
        if buf.len() < body_len + 4 {
            return Err(StorageError::InvalidValue("footer truncated".into()));
        }
        let body = &buf[..body_len];
        if body.len() < 20 {
            return Err(StorageError::InvalidValue("footer body too short".into()));
        }
        let root_offset = u64::from_be_bytes(body[0..8].try_into().unwrap());
        let tree_height = u32::from_be_bytes(body[8..12].try_into().unwrap());
        let first_key_len = u32::from_be_bytes(body[12..16].try_into().unwrap()) as usize;
        let last_key_len = u32::from_be_bytes(body[16..20].try_into().unwrap()) as usize;
        if 20 + first_key_len + last_key_len != body_len {
            return Err(StorageError::InvalidValue(
                "footer key length mismatch".into(),
            ));
        }
        let first_key = Bytes::copy_from_slice(&body[20..20 + first_key_len]);
        let last_key = Bytes::copy_from_slice(
            &body[20 + first_key_len..20 + first_key_len + last_key_len],
        );
        Ok(Self {
            root_index_block_position: Position { offset: root_offset },
            tree_height,
            first_key,
            last_key,
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
        assert!(footer.root_index_block_position.offset < buf.len() as u64);
    }

    #[test]
    fn test_footer_encodes_fields_and_keys() {
        let footer = SstFooter {
            root_index_block_position: Position {
                offset: 0x0102_0304_0506_0708,
            },
            tree_height: 0x1112_1314,
            first_key: Bytes::from_static(b"aaa"),
            last_key: Bytes::from_static(b"zzz"),
        };
        let encoded = footer.encode();
        assert!(encoded.len() >= SstFooter::MIN_LEN);
        let decoded = SstFooter::decode(&encoded).unwrap();
        assert_eq!(decoded, footer);
    }

    #[test]
    fn test_footer_empty_keys() {
        let footer = SstFooter {
            root_index_block_position: Position { offset: 0 },
            tree_height: 0,
            first_key: Bytes::new(),
            last_key: Bytes::new(),
        };
        let encoded = footer.encode();
        assert_eq!(encoded.len(), SstFooter::MIN_LEN);
        let decoded = SstFooter::decode(&encoded).unwrap();
        assert_eq!(decoded, footer);
    }

    #[test]
    fn test_footer_rejects_truncated_trailer() {
        assert!(SstFooter::decode(&[0u8; 3]).is_err());
    }

    #[test]
    fn test_builder_writes_footer_after_blocks() {
        let block_size = 256;
        let mut builder = fixed_builder(block_size);
        for i in 0..100u64 {
            builder.add(make_key(i), make_value(i)).unwrap();
        }

        let (footer, sst_writer) = builder.finish().unwrap();
        let writer = sst_writer.into_inner();
        let buf = writer.into_inner();

        // footer body_len trailer is the last 4 bytes
        let body_len = u32::from_be_bytes(buf[buf.len() - 4..].try_into().unwrap()) as usize;
        let footer_start = buf.len() - body_len - 4;
        assert_eq!(footer_start % block_size, 0);
        assert_eq!(footer.root_index_block_position.offset as usize % block_size, 0);

        let decoded = SstFooter::decode(&buf[footer_start..]).unwrap();
        assert_eq!(decoded, footer);
    }

    #[test]
    fn test_empty_sst_writes_only_footer() {
        let (footer, sst_writer) = fixed_builder(256).finish().unwrap();
        let buf = sst_writer.into_inner().into_inner();
        assert_eq!(buf.len(), SstFooter::MIN_LEN);
        assert_eq!(footer.root_index_block_position, Position { offset: 0 });
        assert_eq!(footer.tree_height, 0);
        assert!(footer.first_key.is_empty());
        assert!(footer.last_key.is_empty());
        assert_eq!(SstFooter::decode(&buf).unwrap(), footer);
    }

    #[test]
    fn test_build_empty() {
        let builder = fixed_builder(256);
        let (footer, _) = builder.finish().unwrap();
        assert_eq!(footer.root_index_block_position, Position { offset: 0 });
    }

    #[test]
    fn test_builder_footer_records_first_and_last_key() {
        let mut builder = fixed_builder(4096);
        for i in 0..100u64 {
            builder.add(make_key(i), make_value(i)).unwrap();
        }
        let (footer, _) = builder.finish().unwrap();
        assert_eq!(footer.first_key, make_key(0));
        assert_eq!(footer.last_key, make_key(99));
    }

    #[test]
    fn test_builder_footer_empty_when_no_entries() {
        let (footer, _) = fixed_builder(4096).finish().unwrap();
        assert!(footer.first_key.is_empty());
        assert!(footer.last_key.is_empty());
    }
}
