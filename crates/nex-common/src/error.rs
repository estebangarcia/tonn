use thiserror::Error;

pub type Result<T> = std::result::Result<T, NexError>;

#[derive(Error, Debug)]
pub enum NexError {
    #[error("PTY error: {0}")]
    Pty(String),

    #[error("Rendering error: {0}")]
    Render(String),

    #[error("Configuration error: {0}")]
    Config(String),

    #[error("MCP server error: {0}")]
    Mcp(String),

    #[error("Multiplexer error: {0}")]
    Mux(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("{0}")]
    Other(String),
}
