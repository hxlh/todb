use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_CONTEXT_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MemoryContextId(u64);

impl MemoryContextId {
    fn next() -> Self {
        Self(NEXT_CONTEXT_ID.fetch_add(1, Ordering::Relaxed))
    }
}

#[derive(Debug)]
pub struct MemoryContext {
    id: MemoryContextId,
    name: String,
    _parent: Option<MemoryContextId>,
    allocated: AtomicU64,
}

impl MemoryContext {
    pub fn new(name: impl Into<String>, parent: Option<MemoryContextId>) -> Self {
        Self {
            id: MemoryContextId::next(),
            name: name.into(),
            _parent: parent,
            allocated: AtomicU64::new(0),
        }
    }

    pub fn id(&self) -> MemoryContextId {
        self.id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn allocated(&self) -> u64 {
        self.allocated.load(Ordering::Relaxed)
    }

    pub fn record_alloc(&self, size: u64) {
        self.allocated.fetch_add(size, Ordering::Relaxed);
    }

    pub fn record_dealloc(&self, size: u64) {
        self.allocated.fetch_sub(size, Ordering::Relaxed);
    }

    pub fn reset(&self) {
        self.allocated.store(0, Ordering::Relaxed);
    }
}

pub struct MemoryContextTree {
    root: Arc<MemoryContext>,
    contexts: std::collections::HashMap<MemoryContextId, Arc<MemoryContext>>,
}

impl MemoryContextTree {
    pub fn new() -> Self {
        let root = Arc::new(MemoryContext::new("Top", None));
        let mut contexts = std::collections::HashMap::new();
        contexts.insert(root.id(), root.clone());
        Self { root, contexts }
    }

    pub fn root(&self) -> &Arc<MemoryContext> {
        &self.root
    }

    pub fn create_child(
        &mut self,
        name: impl Into<String>,
        parent: MemoryContextId,
    ) -> Option<Arc<MemoryContext>> {
        if !self.contexts.contains_key(&parent) {
            return None;
        }
        let ctx = Arc::new(MemoryContext::new(name, Some(parent)));
        self.contexts.insert(ctx.id(), ctx.clone());
        Some(ctx)
    }

    pub fn get(&self, id: MemoryContextId) -> Option<&Arc<MemoryContext>> {
        self.contexts.get(&id)
    }

    pub fn total_allocated(&self) -> u64 {
        self.contexts.values().map(|c| c.allocated()).sum()
    }

    pub fn reset_all(&self) {
        for ctx in self.contexts.values() {
            ctx.reset();
        }
    }
}

impl Default for MemoryContextTree {
    fn default() -> Self {
        Self::new()
    }
}
