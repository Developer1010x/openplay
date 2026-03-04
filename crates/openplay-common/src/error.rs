use thiserror::Error;

#[derive(Error, Debug)]
pub enum OpenPlayError {
    #[error("Configuration error: {0}")]
    Config(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Certificate error: {0}")]
    Certificate(String),

    #[error("Discovery error: {0}")]
    Discovery(String),

    #[error("Signaling error: {0}")]
    Signaling(String),

    #[error("Pipeline error: {0}")]
    Pipeline(String),

    #[error("Capture error: {0}")]
    Capture(String),

    #[error("Protocol error: {0}")]
    Protocol(String),

    #[error("Timeout: {0}")]
    Timeout(String),
}
