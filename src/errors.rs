use thiserror::Error;
use tokio::io;

#[derive(Error, Debug)]
pub enum ProgramError {
    #[error("Failed to bind")]
    Disconnect(#[from] io::Error),
}
