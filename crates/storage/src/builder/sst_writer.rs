use crate::{
    block::{BlockWriter, Position},
    builder::{SstFooter, SstOption},
    errors::StorageResult,
};

/// SST file format writer.
///
/// Wraps a raw [`BlockWriter`] and adds SST-specific semantics:
/// footer metadata writing and block alignment assertions.
///
/// Builder is responsible for padding output to `block_size` before calling.
/// This writer only asserts the invariant and delegates to [`BlockWriter`].
pub trait SstWriter {
    /// Write a fixed-size block slot.
    fn write_block<T: AsRef<[u8]>>(&mut self, data: T) -> StorageResult<Position>;

    /// Write SST footer. Encoding is handled internally.
    fn write_footer(&mut self, footer: &SstFooter) -> StorageResult<Position>;
}

/// Default SST writer implementation.
///
/// Generic over the raw [`BlockWriter`] so it works with any storage backend
/// (memory for testing, file for production).
pub struct DefaultSstWriter<W: BlockWriter> {
    inner: W,
    option: SstOption,
}

impl<W: BlockWriter> DefaultSstWriter<W> {
    pub fn new(inner: W, option: &SstOption) -> Self {
        Self { inner, option: option.clone() }
    }

    pub fn into_inner(self) -> W {
        self.inner
    }
}

impl<W: BlockWriter> SstWriter for DefaultSstWriter<W> {
    fn write_block<T: AsRef<[u8]>>(&mut self, data: T) -> StorageResult<Position> {
        debug_assert_eq!(data.as_ref().len(), self.option.block_size);
        self.inner.write_block(data)
    }

    fn write_footer(&mut self, footer: &SstFooter) -> StorageResult<Position> {
        // Footer is not a data/index block, so the block-size assertion does not
        // apply here; we delegate directly to the underlying BlockWriter.
        self.inner.write_block(footer.encode())
    }
}
