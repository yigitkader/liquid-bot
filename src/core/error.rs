use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("Configuration error: {0}")]
    Config(String),

    #[error("RPC error: {0}")]
    Rpc(String),

    #[error("Protocol error: {0}")]
    Protocol(String),

    #[error("Validation error: {0}")]
    Validation(String),

    #[error("Transaction error: {0}")]
    Transaction(String),

    #[error("Balance error: {0}")]
    Balance(String),
}

pub type Result<T> = std::result::Result<T, Error>;
