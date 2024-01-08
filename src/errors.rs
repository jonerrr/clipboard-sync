use thiserror::Error;

#[derive(Error, Debug)]
pub enum ProgramError {
    #[error("Failed to bind")]
    Disconnect(#[from] tokio::io::Error),
    #[error("Failed to connect to websocket")]
    Connect(#[from] tokio_tungstenite::tungstenite::Error),
    #[error("Error occurred: {0}")]
    Custom(String),
}
