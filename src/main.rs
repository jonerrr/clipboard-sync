use aes_gcm::{
    aead::{
        generic_array::GenericArray, rand_core::RngCore, Aead, AeadCore, KeyInit, Nonce, OsRng,
    },
    aes::cipher::typenum::U12,
    Aes256Gcm,
};
use arboard::{Clipboard, ImageData};
use clap::{Parser, Subcommand};
use errors::ProgramError;
use futures_channel::mpsc::{unbounded, UnboundedSender};
use futures_util::{future, pin_mut, stream::TryStreamExt, StreamExt};
use local_ip_address::local_ip;
use pbkdf2::pbkdf2_hmac_array;
use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{Arc, Mutex},
};

use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::protocol::Message;
mod errors;
mod service;

/// Sync your clipboard between all your devices
#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// Install as a background service (may require root)
    #[arg(short, long, default_value = "false")]
    service: bool,

    /// Use AES encryption, password is required
    #[arg(short, long)]
    password: Option<String>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Connect client to server
    Connect {
        /// Port to connect to
        #[arg(short, long, env, default_value = "4343")]
        port: u16,
        /// Address to connect to
        #[arg(short, long, env, default_value = "127.0.0.1")]
        address: String,
        /// Use TLS (NOT IMPLEMENTED YET)
        #[arg(short, long, env, default_value = "false")]
        tls: bool,
        // /// Install as a service (probably requires root)
        // #[arg(short, long, env, default_value = "false")]
        // service: bool,
    },
    /// Starts sync server
    Start {
        /// Port to listen on
        #[arg(short, long, env, default_value = "4343")]
        port: u16,
        /// Address to listen on
        #[arg(short, long, env, default_value = "0.0.0.0")]
        address: String,
        // /// Install as a service (probably requires root)
        // #[arg(short, long, env, default_value = "false")]
        // service: bool,
    },
}

#[derive(PartialEq, Clone)]
struct Image {
    bytes: Vec<u8>,
    width: usize,
    height: usize,
}

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
    let cli = Cli::parse();

    // let salt = SaltString::(&mut OsRng);
    // let mut salt = [0u8; 32];
    // OsRng.fill_bytes(&mut salt);

    // let key: [u8; 32] = pbkdf2_hmac_array::<sha2::Sha256, 32>(password, &salt, 600_000);
    // let cipher = Aes256Gcm::new(&key.into());
    // let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
    // let ciphertext = cipher.encrypt(&nonce, b"poop".as_ref()).unwrap();
    // let plaintext = cipher.decrypt(&nonce, ciphertext.as_ref()).unwrap();

    // let p_str = String::from_utf8(plaintext).unwrap();

    // let mut file = std::fs::File::create("key.bin").unwrap();
    // file.write_all(&key).unwrap();

    match &cli.command {
        Commands::Connect { port, address, tls } => {
            if cli.service {
                let mut env = Vec::new();
                env.push(("PORT".to_owned(), port.to_string()));
                env.push(("ADDRESS".to_owned(), address.to_owned()));
                // added to prevent clipboard errors when running as a service
                if std::env::consts::OS == "linux" {
                    env.push(("DISPLAY".to_owned(), std::env::var("DISPLAY").unwrap()));
                }
                service::create(
                    "clipboard-sync-client".to_owned(),
                    "connect".to_owned(),
                    env,
                )
                .await?;
                return Ok(());
            }

            let (clipboard_tx, clipboard_rx) = futures_channel::mpsc::unbounded();

            let cipher = match cli.password {
                Some(password) => {
                    println!("Encrypting all traffic using AES256-GCM");
                    let mut salt = [0u8; 32];
                    OsRng.fill_bytes(&mut salt);
                    let key: [u8; 32] =
                        pbkdf2_hmac_array::<sha2::Sha256, 32>(password.as_bytes(), &salt, 600_000);
                    Some(Aes256Gcm::new(&key.into()))
                }
                None => None,
            };

            tokio::spawn(read_clipboard(clipboard_tx, cipher.clone()));

            let addr = format!("ws://{}:{}", address, port);
            let (ws_stream, _) = tokio_tungstenite::connect_async(&addr).await?;
            println!("Websocket connection established with: {}", addr);

            let (write, read) = ws_stream.split();
            let clipboard_to_ws = clipboard_rx.map(Ok).forward(write);

            let ws_to_clipboard = {
                read.for_each(
                    |message: Result<Message, tokio_tungstenite::tungstenite::Error>| async {
                        let mut clipboard = Clipboard::new().unwrap();

                        // get binary message and decrypt if cipher is set
                        match message {
                            Ok(message) => {
                                let mut data = message.into_data();
                                if let Some(cipher) = &cipher {
                                    let nonce = GenericArray::from_slice(&data[0..12]);
                                    let cipher_text = data[12..].to_vec();
                                    data = cipher.decrypt(&nonce, cipher_text.as_ref()).unwrap();
                                };
                                // set clipboard data
                                match data[0] {
                                    0 => {
                                        let text = String::from_utf8(data[1..].to_vec()).unwrap();
                                        clipboard.set_text(text).unwrap();
                                    }
                                    1 => {
                                        let width =
                                            usize::from_be_bytes(data[1..9].try_into().unwrap());
                                        let height =
                                            usize::from_be_bytes(data[9..17].try_into().unwrap());
                                        let bytes = data[17..].to_vec();
                                        let img = Image {
                                            bytes,
                                            width,
                                            height,
                                        };
                                        clipboard
                                            .set_image(ImageData {
                                                bytes: img.bytes.into(),
                                                width: img.width,
                                                height: img.height,
                                            })
                                            .unwrap();
                                    }
                                    _ => (),
                                };
                            }
                            Err(_) => (),
                        };
                    },
                )
            };

            pin_mut!(clipboard_to_ws, ws_to_clipboard);
            future::select(clipboard_to_ws, ws_to_clipboard).await;
        }
        Commands::Start { port, address } => {
            if cli.service {
                // maybe use hashmap instead of vec
                let mut env = Vec::new();
                env.push(("PORT".to_owned(), port.to_string()));
                env.push(("ADDRESS".to_owned(), address.to_owned()));

                service::create("clipboard-sync-server".to_owned(), "start".to_owned(), env)
                    .await?;
                return Ok(());
            }

            let addr = format!("{}:{}", address, port);
            let listener = TcpListener::bind(&addr).await?;
            let local_ip = local_ip().unwrap();
            println!("Listening on: {}\nLocal IP: {}", addr, local_ip);
            let peers = PeerMap::new(Mutex::new(HashMap::new()));

            while let Ok((stream, addr)) = listener.accept().await {
                tokio::spawn(handle_connection(stream, addr, peers.clone()));
            }

            // let mut clipboard = Clipboard::new().unwrap();
            // let data = clipboard.get().image();

            // println!("clipboard: {:#?}", clipboard.get_image())
        }
    }

    Ok(())
}

async fn read_clipboard(
    tx: futures_channel::mpsc::UnboundedSender<Message>,
    cipher: Option<Aes256Gcm>,
) {
    let mut clipboard = Clipboard::new().unwrap();
    let mut last_clipboard: Vec<u8> = Vec::new();

    loop {
        tokio::time::sleep(tokio::time::Duration::from_millis(1000)).await;

        let data = match clipboard.get_text() {
            Ok(data) => {
                let mut data_vec = Vec::new();
                // extend the vector first with the data type (text = 0 and image = 1)
                data_vec.extend_from_slice(&(0 as u32).to_be_bytes());
                data_vec.extend_from_slice(data.as_bytes());
                data_vec
            }
            Err(_) => {
                if let Ok(data) = clipboard.get_image() {
                    let mut data_vec = Vec::new();
                    data_vec.extend_from_slice(&(1 as u32).to_be_bytes());
                    // include image data dimensions
                    data_vec.extend_from_slice(&data.width.to_be_bytes());
                    data_vec.extend_from_slice(&data.height.to_be_bytes());
                    data_vec.extend_from_slice(&data.bytes);
                    data_vec
                } else {
                    continue;
                }
            }
        };

        if last_clipboard != data {
            last_clipboard = data.clone();

            if let Some(cipher) = &cipher {
                let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
                let ciphertext = cipher.encrypt(&nonce, data.as_ref()).unwrap();
                // send nonce and ciphertext
                let mut data_vec = Vec::new();
                data_vec.extend_from_slice(&nonce);
                data_vec.extend_from_slice(&ciphertext);
                tx.unbounded_send(Message::binary(data_vec)).unwrap();

                continue;
            }

            tx.unbounded_send(Message::binary(data)).unwrap();
        }
    }

    // let mut last_clipboard: ClipboardData = ClipboardData::Text("".to_owned());

    // loop {
    //     if let Ok(data) = clipboard.get_text() {
    //         let cp_data = ClipboardData::Text(data.clone());
    //         if last_clipboard != cp_data {
    //             last_clipboard = cp_data;
    //             tx.unbounded_send(Message::text(data)).unwrap();
    //         }
    //     } else {
    //         if let Ok(data) = clipboard.get_image() {
    //             let img = Image {
    //                 bytes: data.bytes.to_vec(),
    //                 width: data.width,
    //                 height: data.height,
    //             };

    //             if last_clipboard != ClipboardData::Image(img.clone()) {
    //                 last_clipboard = ClipboardData::Image(img.clone());
    //                 tx.unbounded_send(Message::binary(img)).unwrap();
    //             }
    //         }
    //     }
    //     tokio::time::sleep(tokio::time::Duration::from_millis(1000)).await;
    // }
}

type Tx = UnboundedSender<Message>;
type PeerMap = Arc<Mutex<HashMap<SocketAddr, Tx>>>;

async fn handle_connection(raw_stream: TcpStream, addr: SocketAddr, peer_map: PeerMap) {
    // println!("Incoming TCP connection from: {:?}", addr);

    let ws_stream = tokio_tungstenite::accept_async(raw_stream)
        .await
        .expect("Error during the websocket handshake occurred");
    println!("Connected to: {:?}", addr);

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

    println!("{:?} disconnected", &addr);
    peer_map.lock().unwrap().remove(&addr);
}
