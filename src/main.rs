use arboard::Clipboard;
use clap::{Parser, Subcommand};
use errors::ProgramError;
use std::path::PathBuf;
use tokio::net::TcpListener;

mod errors;

/// Sync your clipboard between all your devices
#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// Sets a custom config file
    #[arg(short, long, value_name = "FILE")]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// starts sync client
    Client {
        #[command(subcommand)]
        client_commands: ClientCommands,
    },
    /// Starts sync server
    Server {
        /// Port to listen on
        #[arg(short, long, default_value = "4343")]
        port: u16,
        /// Address to listen on
        #[arg(short, long, default_value = "127.0.0.1")]
        address: String,
    },
}

#[derive(Subcommand)]
enum ClientCommands {
    /// Starts sync client
    Start {
        /// Port to listen on
        #[arg(short, long, default_value = "4343")]
        port: u16,
        /// Address to listen on
        #[arg(short, long, default_value = "127.0.0.1")]
        address: String,
    },
}

#[tokio::main]
async fn main() -> Result<(), ProgramError> {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();

    match &cli.command {
        Commands::Client { client_commands } => todo!(),
        Commands::Server { port, address } => {
            let addr = format!("{}{}", address, port);
            let listener = TcpListener::bind(&addr).await?;
            tracing::info!("Listening on: {}", addr);

            // let mut clipboard = Clipboard::new().unwrap();
            // let data = clipboard.get().image();

            // tracing::info!("clipboard: {:#?}", clipboard.get_image())
        }
    }

    Ok(())
}
