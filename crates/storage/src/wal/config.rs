//! `WalConfig` + validation. See `wal-design.md` §Config.

use crate::wal::WalError;

/// Creation-time WAL configuration. All sizes are byte counts.
///
/// - `segment_size`: per-segment `.log` preallocation (rolled over when filled).
/// - `buffer_size`: one in-memory append buffer; must be `< segment_size` so a
///   single buffer never spans more than two segments on flush.
/// - `block_size`: I/O alignment unit for `O_DIRECT` (default 4096).
/// - `buffer_count`: free-pool capacity (spares beyond the active buffer); `>= 2`.
/// - `read_cache_blocks`: `DiskManager` CLOCK frame-pool size, `>= 1` (read path).
/// - `o_direct`: production `true`; `false` lets tests run on tmpfs/CI.
#[derive(Debug, Clone)]
pub struct WalConfig {
    pub segment_size: usize,
    pub buffer_size: usize,
    pub block_size: usize,
    pub buffer_count: usize,
    pub read_cache_blocks: usize,
    pub o_direct: bool,
}

impl Default for WalConfig {
    fn default() -> Self {
        Self {
            segment_size: 1 << 30, // 1 GiB
            buffer_size: 1 << 24,  // 16 MiB
            block_size: 4096,
            buffer_count: 2,
            read_cache_blocks: 64,
            o_direct: true,
        }
    }
}

impl WalConfig {
    /// Validate on open. Rules per `wal-design.md` §Config.
    pub fn validate(&self) -> Result<(), WalError> {
        if self.block_size < 512 || !self.block_size.is_power_of_two() {
            return Err(WalError::InvalidConfig(format!(
                "block_size must be a power of two >= 512 (got {})",
                self.block_size
            )));
        }
        let misaligned = |v: usize, what: &str| {
            if !v.is_multiple_of(self.block_size) {
                Err(WalError::InvalidConfig(format!(
                    "{what} ({v}) must be a multiple of block_size ({})",
                    self.block_size
                )))
            } else {
                Ok(())
            }
        };
        misaligned(self.segment_size, "segment_size")?;
        misaligned(self.buffer_size, "buffer_size")?;
        if self.buffer_size >= self.segment_size {
            return Err(WalError::InvalidConfig(format!(
                "buffer_size ({}) must be < segment_size ({})",
                self.buffer_size, self.segment_size
            )));
        }
        if self.buffer_count < 2 {
            return Err(WalError::InvalidConfig(format!(
                "buffer_count must be >= 2 (got {})",
                self.buffer_count
            )));
        }
        if self.read_cache_blocks == 0 {
            return Err(WalError::InvalidConfig(
                "read_cache_blocks must be >= 1 (empty pool has no victim and no unpinner → deadlock)".into(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid() -> WalConfig {
        WalConfig {
            segment_size: 1 << 16,
            buffer_size: 4096,
            block_size: 4096,
            buffer_count: 2,
            read_cache_blocks: 8,
            o_direct: false,
        }
    }

    #[test]
    fn read_cache_blocks_zero_rejected() {
        let mut cfg = valid();
        cfg.read_cache_blocks = 0;
        assert!(
            matches!(cfg.validate(), Err(WalError::InvalidConfig(_))),
            "read_cache_blocks == 0 must be rejected (empty pool deadlocks)"
        );
    }

    #[test]
    fn read_cache_blocks_one_accepted() {
        let mut cfg = valid();
        cfg.read_cache_blocks = 1;
        assert!(cfg.validate().is_ok());
    }
}
