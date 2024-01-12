use crate::{
    connect::read_clipboard,
    server::{handle_request, PeerMap},
};
use aes_gcm::{
    aead::{generic_array::GenericArray, Aead, KeyInit},
    Aes256Gcm, Key,
};
use arboard::{Clipboard, ImageData};
use base64::prelude::{Engine as _, BASE64_STANDARD};
use clap::{Parser, Subcommand};
use errors::ProgramError;
use futures_util::{future, pin_mut, StreamExt};
use hyper::{server::conn::http1, service::service_fn, Request};
use hyper_util::rt::TokioIo;
use local_ip_address::local_ip;
use pbkdf2::pbkdf2_hmac;
use rand::Rng;
use sha2::Sha256;
use std::{collections::HashMap, sync::Mutex};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::protocol::Message;

mod connect;
mod errors;
mod server;
mod service;

/// Sync your clipboard between all your devices
#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// Install as a background service (may require root)
    #[arg(short, long, default_value = "false")]
    service: bool,

    /// Encrypt clipboard data with a password (using AES-256-GCM)
    #[arg(short, long, env)]
    password: Option<String>,

    #[command(subcommand)]
    command: Commands,
}

//TODO: make address not require -a flag
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
        // /// Install as a service (probably requires root)
        // #[arg(short, long, env, default_value = "false")]
        // service: bool,
    },
    /// Starts sync server
    Start {
        // maybe combine port and address into one arg
        /// Port to listen on
        #[arg(env, default_value = "4343")]
        port: u16,
        /// Address to listen on
        #[arg(env, default_value = "0.0.0.0")]
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

impl From<Image> for Vec<u8> {
    fn from(val: Image) -> Self {
        let mut vec = Vec::new();
        vec.extend_from_slice(&val.width.to_be_bytes());
        vec.extend_from_slice(&val.height.to_be_bytes());
        vec.extend_from_slice(&val.bytes);
        vec
    }
}

#[tokio::main]
async fn main() -> Result<(), ProgramError> {
    let cli = Cli::parse();

    match &cli.command {
        Commands::Connect { port, address } => {
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

            let addr = format!("ws://{}:{}/sync", address, port);
            let mut rng = rand::thread_rng();
            let random_bytes: Vec<u8> = (0..16).map(|_| rng.gen()).collect();
            let sec_websocket_key = BASE64_STANDARD.encode(&random_bytes);

            let mut request = Request::builder()
                .uri(&addr)
                .header("sec-websocket-key", sec_websocket_key)
                .header("host", address)
                .header("upgrade", "websocket")
                .header("connection", "upgrade")
                .header("sec-websocket-version", "13");
            if let Some(password) = &cli.password {
                request = request.header("x-password", password);
            }
            let request = request.body(()).unwrap();

            let (ws_stream, res) = tokio_tungstenite::connect_async(request).await?;
            println!("Websocket connection established with: {}", addr);

            let mut cipher = None;
            if let Some(password) = &cli.password {
                let Some(salt) = res.headers().get("x-salt") else {
                    return Err(errors::ProgramError::Custom(
                        "Failed to get salt".to_string(),
                    ));
                };
                let salt = BASE64_STANDARD.decode(salt.as_bytes()).unwrap();
                let mut hash = [0u8; 32];
                pbkdf2_hmac::<Sha256>(password.as_bytes(), &salt, 60_000, &mut hash);
                cipher = Some(Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&hash)));
            }

            // read clipboard and send to server
            let (clipboard_tx, clipboard_rx) = futures_channel::mpsc::unbounded();
            tokio::spawn(read_clipboard(clipboard_tx, cipher.clone()));
            let (write, read) = ws_stream.split();
            let clipboard_to_ws = clipboard_rx.map(Ok).forward(write);

            //TODO: move this to a function
            let ws_to_clipboard = {
                read.for_each(
                    |message: Result<Message, tokio_tungstenite::tungstenite::Error>| async {
                        let mut clipboard = Clipboard::new().unwrap();
                        if let Ok(message) = message {
                            let mut data: Vec<u8> = message.into_data();

                            // decrypt if encryption is enabled
                            if let Some(cipher) = &cipher {
                                let nonce = GenericArray::from_slice(&data[0..12]);
                                let cipher_text = data[12..].to_vec();
                                match cipher.decrypt(nonce, cipher_text.as_ref()) {
                                    Ok(plaintext) => data = plaintext,
                                    Err(e) => {
                                        eprintln!("{:?}", e);
                                        return;
                                    }
                                };
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
                    },
                )
            };

            pin_mut!(clipboard_to_ws, ws_to_clipboard);
            future::select(clipboard_to_ws, ws_to_clipboard).await;
        }
        Commands::Start { port, address } => {
            let mut salt = [0u8; 20];
            rand::thread_rng().fill(&mut salt[..]);
            let mut key = [0u8; 32];
            if let Some(password) = &cli.password {
                pbkdf2_hmac::<Sha256>(password.as_bytes(), &salt, 60_000, &mut key);
            }

            if cli.service {
                // maybe use hashmap instead of vec
                let env = vec![
                    ("PORT".to_owned(), port.to_string()),
                    ("ADDRESS".to_owned(), address.to_owned()),
                    (
                        "PASSWORD".to_owned(),
                        cli.password.clone().unwrap_or_default(),
                    ),
                ];

                service::create("clipboard-sync-server".to_owned(), "start".to_owned(), env)
                    .await?;
                return Ok(());
            }
            let addr = format!("{}:{}", address, port);
            let listener: TcpListener = TcpListener::bind(&addr).await?;
            let local_ip: std::net::IpAddr = local_ip().unwrap();
            println!("Listening on: {}\nLocal IP: {}", addr, local_ip);
            let peers = PeerMap::new(Mutex::new(HashMap::new()));

            loop {
                let (stream, remote_addr) = listener.accept().await?;
                let peers = peers.clone();
                let key = cli.password.is_some().then_some(key);

                tokio::spawn(async move {
                    let io = TokioIo::new(stream);

                    let service = service_fn(move |req| {
                        handle_request(peers.clone(), req, remote_addr, key, salt)
                    });

                    let conn = http1::Builder::new()
                        .serve_connection(io, service)
                        .with_upgrades();

                    if let Err(err) = conn.await {
                        eprintln!("failed to serve connection: {err:?}");
                    }
                });
            }
        }
    }

    Ok(())
}
