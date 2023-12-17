use arboard::{Clipboard, ImageData};
use clap::{Parser, Subcommand};
use errors::ProgramError;
use futures_channel::mpsc::{unbounded, UnboundedSender};
use futures_util::{future, pin_mut, stream::TryStreamExt, StreamExt};
use local_ip_address::local_ip;
// use native_tls::TlsConnector;
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
        /// Use TLS (NOT IMPLEMENTED YET)
        #[arg(short, long, default_value = "false")]
        tls: bool,
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

#[derive(PartialEq, Clone)]
struct Image {
    bytes: Vec<u8>,
    width: usize,
    height: usize,
}

// convert Image to Vec<u8> and include the width and hieght in the vec
impl Into<Vec<u8>> for Image {
    fn into(self) -> Vec<u8> {
        let mut vec = Vec::new();
        vec.extend_from_slice(&self.width.to_be_bytes());
        vec.extend_from_slice(&self.height.to_be_bytes());
        vec.extend_from_slice(&self.bytes);
        vec
    }
}

#[derive(PartialEq)]
enum ClipboardData {
    Text(String),
    Image(Image),
}

#[tokio::main]
async fn main() -> Result<(), ProgramError> {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();

    match &cli.command {
        Commands::Client { port, address, tls } => {
            let (clipboard_tx, clipboard_rx) = futures_channel::mpsc::unbounded();
            tokio::spawn(read_clipboard(clipboard_tx));

            let protocol: &str = if *tls { "wss" } else { "ws" };
            // let connector = if *tls {
            //     let mut builder = TlsConnector::builder();
            //     builder.danger_accept_invalid_certs(true);
            //     Connector::NativeTls(builder.build().unwrap())
            // } else {
            //     Connector::Plain
            // };
            let addr = format!("{}://{}:{}", protocol, address, port);
            let (ws_stream, _) = tokio_tungstenite::connect_async(&addr).await?;
            tracing::info!("Websocket connection established with: {}", addr);

            let (write, read) = ws_stream.split();
            let clipboard_to_ws = clipboard_rx.map(Ok).forward(write);

            let ws_to_clipboard = {
                read.for_each(
                    |message: Result<Message, tokio_tungstenite::tungstenite::Error>| async {
                        let mut clipboard = Clipboard::new().unwrap();

                        let _ = match message.unwrap() {
                            Message::Text(text) => clipboard.set_text(text),
                            Message::Binary(img) => {
                                let width = usize::from_be_bytes(img[0..8].try_into().unwrap());
                                let height = usize::from_be_bytes(img[8..16].try_into().unwrap());
                                let bytes = img[16..].to_vec();
                                let img = Image {
                                    bytes,
                                    width,
                                    height,
                                };
                                clipboard.set_image(ImageData {
                                    bytes: img.bytes.into(),
                                    width: img.width,
                                    height: img.height,
                                })
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
            let local_ip = local_ip().unwrap();
            tracing::info!("Listening on: {}\nLocal IP: {}", addr, local_ip);
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

type Tx = UnboundedSender<Message>;
type PeerMap = Arc<Mutex<HashMap<SocketAddr, Tx>>>;

async fn handle_connection(raw_stream: TcpStream, addr: SocketAddr, peer_map: PeerMap) {
    // tracing::info!("Incoming TCP connection from: {:?}", addr);

    let ws_stream = tokio_tungstenite::accept_async(raw_stream)
        .await
        .expect("Error during the websocket handshake occurred");
    tracing::info!("Connected to: {:?}", addr);

    // Insert the write part of this peer to the peer map.
    let (tx, rx) = unbounded();
    peer_map.lock().unwrap().insert(addr, tx);

    let (outgoing, incoming) = ws_stream.split();

    let broadcast_incoming = incoming.try_for_each(|msg| {
        // println!(
        //     "Received a message from {:?}: {}",
        //     addr,
        //     msg.to_text().unwrap()
        // );
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

    tracing::info!("{:?} disconnected", &addr);
    peer_map.lock().unwrap().remove(&addr);
}

async fn read_clipboard(tx: futures_channel::mpsc::UnboundedSender<Message>) {
    let mut clipboard = Clipboard::new().unwrap();
    let mut last_clipboard: ClipboardData = ClipboardData::Text("".to_owned());

    //TODO: use less clones
    loop {
        if let Ok(data) = clipboard.get_text() {
            if last_clipboard != ClipboardData::Text(data.clone()) {
                last_clipboard = ClipboardData::Text(data.clone());
                // tracing::info!("clipboard: {:#?}", data);
                tx.unbounded_send(Message::text(data)).unwrap();
            }
        } else {
            if let Ok(data) = clipboard.get_image() {
                let img = Image {
                    bytes: data.bytes.to_vec(),
                    width: data.width,
                    height: data.height,
                };

                if last_clipboard != ClipboardData::Image(img.clone()) {
                    last_clipboard = ClipboardData::Image(img.clone());
                    tracing::info!("clipboard img: {:#?}", data.width);
                    //TODO impl into vec<u8> for Image
                    tx.unbounded_send(Message::binary(img)).unwrap();
                }
            }
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(1000)).await;
    }
}
