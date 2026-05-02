#[derive(Debug, Clone)]
pub struct ClientSession {
    current_database: String,
}

impl ClientSession {
    pub fn new(current_database: impl Into<String>) -> Self {
        Self {
            current_database: current_database.into(),
        }
    }

    pub fn current_database(&self) -> &str {
        &self.current_database
    }
}
