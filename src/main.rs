use arboard::Clipboard;
use clap::{Parser, Subcommand};
use errors::ProgramError;
use futures_util::{future, pin_mut, stream::TryStreamExt, StreamExt};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tokio::net::{unix::SocketAddr, TcpListener, TcpStream};

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

enum ClipboardData {
    Text(String),
    Image(Vec<u8>),
}

type Peers = Arc<Mutex<Vec<(SocketAddr, tokio_tungstenite::WebSocketStream<TcpStream>)>>>;

async fn handle_connection(state: State, raw_stream: TcpStream, addr: SocketAddr) {
    println!("Incoming TCP connection from: {:?}", addr);

    let ws_stream = tokio_tungstenite::accept_async(raw_stream)
        .await
        .expect("Error during the websocket handshake occurred");
    println!("WebSocket connection established: {:?}", addr);

    // Insert the write part of this peer to the peer map.
    // let (tx, rx) = unbounded();
    // peer_map.lock().unwrap().insert(addr, tx);

    let (outgoing, incoming) = ws_stream.split();

    let broadcast_incoming = incoming.try_for_each(|msg| {
        println!(
            "Received a message from {:?}: {}",
            addr,
            msg.to_text().unwrap()
        );
        // let peers = peer_map.lock().unwrap();

        // We want to broadcast the message to everyone except ourselves.
        let broadcast_recipients = peers
            .iter()
            .filter(|(peer_addr, _)| peer_addr != &&addr)
            .map(|(_, ws_sink)| ws_sink);

        for recp in broadcast_recipients {
            recp.unbounded_send(msg.clone()).unwrap();
        }

        future::ok(())
    });

    let receive_from_others = rx.map(Ok).forward(outgoing);

    pin_mut!(broadcast_incoming, receive_from_others);
    future::select(broadcast_incoming, receive_from_others).await;

    println!("{:?} disconnected", &addr);
    peer_map.lock().unwrap().remove(&addr);
}

type State = Arc<Mutex<ClipboardData>>;

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
            let state = State::new(Mutex::new(ClipboardData::Text("".to_string())));

            while let Ok((stream, addr)) = listener.accept().await {
                tokio::spawn(handle_connection(state.clone(), stream, addr));
            }

            // let mut clipboard = Clipboard::new().unwrap();
            // let data = clipboard.get().image();

            // tracing::info!("clipboard: {:#?}", clipboard.get_image())
        }
    }

    Ok(())
}
