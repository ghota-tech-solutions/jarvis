use thiserror::Error;

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("provider error: {0}")]
    Provider(String),

    #[error("provider not found: {0}")]
    ProviderNotFound(String),

    #[error("capability not satisfied: {required}")]
    Capability { required: String },

    #[error("budget exceeded: {0}")]
    BudgetExceeded(String),

    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("serde: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("config: {0}")]
    Config(String),

    #[error("ledger: {0}")]
    Ledger(String),

    #[error("cancelled")]
    Cancelled,

    #[error("invalid input: {0}")]
    Invalid(String),

    #[error("internal: {0}")]
    Internal(String),
}
