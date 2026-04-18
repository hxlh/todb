use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum ErrorCode {
    // General: 0xxx
    Ok = 0,
    Unknown = 1,
    Internal = 2,
    NotReady = 3,
    Config = 4,

    // RPC: 1xxx
    RpcTimeout = 1000,
    RpcUnavailable = 1001,
    RpcNetworkError = 1002,

    // Storage: 2xxx
    StorageIo = 2000,
    StorageCorruption = 2001,
    StorageFull = 2002,
    KeyNotFound = 2003,
    KeyExists = 2004,

    // Transaction: 3xxx
    TxnConflict = 3000,
    TxnDeadlock = 3001,
    TxnTimeout = 3002,
    TxnAborted = 3003,

    // Raft: 4xxx
    RaftNotLeader = 4000,
    RaftProposalDropped = 4001,
    RaftLeadershipChanged = 4002,
}

impl ErrorCode {
    pub fn is_retryable(self) -> bool {
        matches!(
            self,
            ErrorCode::RpcTimeout
                | ErrorCode::RpcUnavailable
                | ErrorCode::TxnConflict
                | ErrorCode::TxnDeadlock
                | ErrorCode::RaftNotLeader
                | ErrorCode::RaftLeadershipChanged
        )
    }
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("[{code:?}] {message}")]
    Known { code: ErrorCode, message: String },

    #[error("{0}")]
    Other(#[from] anyhow::Error),
}

impl Error {
    pub fn code(&self) -> ErrorCode {
        match self {
            Error::Known { code, .. } => *code,
            Error::Other(_) => ErrorCode::Unknown,
        }
    }

    pub fn is_retryable(&self) -> bool {
        self.code().is_retryable()
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[macro_export]
macro_rules! bail {
    ($code:expr, $msg:expr) => {
        return Err($crate::Error::Known {
            code: $code,
            message: $msg.to_string(),
        })
    };
    ($code:expr, $fmt:expr, $($arg:expr),+ $(,)?) => {
        return Err($crate::Error::Known {
            code: $code,
            message: format!($fmt, $($arg),+),
        })
    };
}

#[macro_export]
macro_rules! ensure {
    ($cond:expr, $code:expr, $msg:expr) => {
        if !$cond {
            $crate::bail!($code, $msg);
        }
    };
}
