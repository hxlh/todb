use bytes::Bytes;

/// A batch of writes applied atomically to the storage engine.
#[derive(Debug, Default, Clone)]
pub struct WriteBatch {
    pub entries: Vec<WriteEntry>,
}

/// A single entry in a [`WriteBatch`].
#[derive(Debug, Clone)]
pub enum WriteEntry {
    Put { key: Bytes, value: Bytes },
    Delete { key: Bytes },
}

impl WriteBatch {
    pub fn new() -> Self {
        Self { entries: Vec::new() }
    }

    pub fn put(&mut self, key: Bytes, value: Bytes) {
        self.entries.push(WriteEntry::Put { key, value });
    }

    pub fn delete(&mut self, key: Bytes) {
        self.entries.push(WriteEntry::Delete { key });
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }
}
