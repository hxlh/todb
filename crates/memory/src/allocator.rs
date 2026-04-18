use tikv_jemalloc_ctl::{epoch, stats};

pub struct Allocator;

impl Allocator {
    pub fn stats() -> AllocStats {
        let _ = epoch::advance();

        let allocated = stats::allocated::read().unwrap_or(0) as u64;
        let active = stats::active::read().unwrap_or(0) as u64;
        let resident = stats::resident::read().unwrap_or(0) as u64;
        let mapped = stats::mapped::read().unwrap_or(0) as u64;
        let metadata = stats::metadata::read().unwrap_or(0) as u64;
        let retained = stats::retained::read().unwrap_or(0) as u64;

        AllocStats {
            allocated,
            active,
            resident,
            mapped,
            metadata,
            retained,
        }
    }
}

#[derive(Debug, Clone)]
pub struct AllocStats {
    pub allocated: u64,
    pub active: u64,
    pub resident: u64,
    pub mapped: u64,
    pub metadata: u64,
    pub retained: u64,
}

impl std::fmt::Display for AllocStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "allocated={:.1}MB active={:.1}MB resident={:.1}MB mapped={:.1}MB metadata={:.1}MB retained={:.1}MB",
            self.allocated as f64 / 1_048_576.0,
            self.active as f64 / 1_048_576.0,
            self.resident as f64 / 1_048_576.0,
            self.mapped as f64 / 1_048_576.0,
            self.metadata as f64 / 1_048_576.0,
            self.retained as f64 / 1_048_576.0,
        )
    }
}
