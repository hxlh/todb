#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    #[error("unsupported statement: {0}")]
    UnsupportedStatement(String),

    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}
