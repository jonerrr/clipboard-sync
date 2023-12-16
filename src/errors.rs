use thiserror::Error;
use tokio::io;

#[derive(Error, Debug)]
pub enum ProgramError {
    #[error("Failed to bind")]
    Disconnect(#[from] io::Error),
    #[error("Failed to connect to websocket")]
    Connect(#[from] tokio_tungstenite::tungstenite::Error),
}
