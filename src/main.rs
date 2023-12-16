use arboard::Clipboard;
use clap::{Parser, Subcommand};
use errors::ProgramError;
use futures_channel::mpsc::{unbounded, UnboundedSender};
use futures_util::{future, pin_mut, stream::TryStreamExt, StreamExt};
use std::{
    collections::HashMap,
    net::SocketAddr,
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::protocol::Message;
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
        /// Port to connect to
        #[arg(short, long, default_value = "4343")]
        port: u16,
        /// Address to connect to
        #[arg(short, long, default_value = "127.0.0.1")]
        address: String,
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

#[derive(PartialEq)]
enum ClipboardData {
    Text(String),
    Image(Vec<u8>),
}

#[tokio::main]
async fn main() -> Result<(), ProgramError> {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();

    match &cli.command {
        Commands::Client { port, address } => {
            let addr = format!("ws://{}:{}", address, port);
            let (clipboard_tx, clipboard_rx) = futures_channel::mpsc::unbounded();
            tokio::spawn(read_clipboard(clipboard_tx));

            let (ws_stream, _) = tokio_tungstenite::connect_async(&addr).await?;
            tracing::info!("Connected to: {}", addr);

            let (write, read) = ws_stream.split();
            let clipboard_to_ws = clipboard_rx.map(Ok).forward(write);

            let ws_to_clipboard = {
                read.for_each(
                    |message: Result<Message, tokio_tungstenite::tungstenite::Error>| async {
                        let mut clipboard = Clipboard::new().unwrap();

                        let _ = match message.unwrap() {
                            Message::Text(text) => clipboard.set_text(text),
                            Message::Binary(bin) => {
                                clipboard.set_image(todo!("convert to image data"))
                            }
                            _ => Ok(()),
                        };
                        // tracing::debug!("Received a message from server: {}", data);
                    },
                )
            };

            pin_mut!(clipboard_to_ws, ws_to_clipboard);
            future::select(clipboard_to_ws, ws_to_clipboard).await;
        }
        Commands::Server { port, address } => {
            let addr = format!("{}:{}", address, port);
            let listener = TcpListener::bind(&addr).await?;
            tracing::info!("Listening on: {}", addr);
            let peers = PeerMap::new(Mutex::new(HashMap::new()));

            while let Ok((stream, addr)) = listener.accept().await {
                tokio::spawn(handle_connection(stream, addr, peers.clone()));
            }

            // let mut clipboard = Clipboard::new().unwrap();
            // let data = clipboard.get().image();

            // tracing::info!("clipboard: {:#?}", clipboard.get_image())
        }
    }

    Ok(())
}

async fn read_clipboard(tx: futures_channel::mpsc::UnboundedSender<Message>) {
    let mut clipboard = Clipboard::new().unwrap();
    let mut last_clipboard: ClipboardData = ClipboardData::Text("".to_owned());

    loop {
        if let Ok(data) = clipboard.get_text() {
            if last_clipboard != ClipboardData::Text(data.clone()) {
                last_clipboard = ClipboardData::Text(data.clone());
                tracing::info!("clipboard: {:#?}", data);
                tx.unbounded_send(Message::text(data)).unwrap();
            }
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(1000)).await;
    }
}

type Tx = UnboundedSender<Message>;
type PeerMap = Arc<Mutex<HashMap<SocketAddr, Tx>>>;

async fn handle_connection(raw_stream: TcpStream, addr: SocketAddr, peer_map: PeerMap) {
    println!("Incoming TCP connection from: {:?}", addr);

    let ws_stream = tokio_tungstenite::accept_async(raw_stream)
        .await
        .expect("Error during the websocket handshake occurred");
    println!("WebSocket connection established: {:?}", addr);

    // Insert the write part of this peer to the peer map.
    let (tx, rx) = unbounded();
    peer_map.lock().unwrap().insert(addr, tx);

    let (outgoing, incoming) = ws_stream.split();

    let broadcast_incoming = incoming.try_for_each(|msg| {
        println!(
            "Received a message from {:?}: {}",
            addr,
            msg.to_text().unwrap()
        );
        let peers: std::sync::MutexGuard<'_, HashMap<SocketAddr, UnboundedSender<Message>>> =
            peer_map.lock().unwrap();

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
