use aes_gcm::{
    aead::{generic_array::GenericArray, Aead, AeadCore, KeyInit, Nonce, OsRng},
    Aes256Gcm, Key,
};
use arboard::{Clipboard, ImageData};
use base64::prelude::{Engine as _, BASE64_STANDARD};
use clap::{Parser, Subcommand};
use errors::ProgramError;
use futures_channel::mpsc::{unbounded, UnboundedSender};
use futures_util::{future, pin_mut, stream::TryStreamExt, SinkExt, StreamExt};
use hyper::{
    body::Incoming,
    header::{
        HeaderName, HeaderValue, CONNECTION, SEC_WEBSOCKET_ACCEPT, SEC_WEBSOCKET_KEY,
        SEC_WEBSOCKET_VERSION, UPGRADE,
    },
    server::conn::http1,
    service::service_fn,
    upgrade::Upgraded,
    Method, Request, Response, StatusCode, Version,
};
use hyper_util::rt::TokioIo;
use local_ip_address::local_ip;
use pbkdf2::{pbkdf2_hmac, pbkdf2_hmac_array};
use rand::Rng;
use sha2::Sha256;
use std::{
    collections::HashMap,
    convert::Infallible,
    net::SocketAddr,
    sync::{atomic::AtomicBool, Arc, Mutex, RwLock},
};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::{
    tungstenite::{
        handshake::derive_accept_key,
        protocol::{Message, Role},
    },
    WebSocketStream,
};

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

// client enters password which will be key
// server generates salt
// server sends salt to client and client generates key

struct EncryptedClipboardData {
    data: Vec<u8>,
    nonce: Vec<u8>,
    salt: Vec<u8>,
}

type Tx = UnboundedSender<Message>;
type PeerMap = Arc<Mutex<HashMap<SocketAddr, Tx>>>;

mod body {
    use http_body_util::{Either, Empty, Full};
    use hyper::body::Bytes;

    pub type Body = Either<Empty<Bytes>, Full<Bytes>>;

    pub fn empty() -> Body {
        Either::Left(Empty::new())
    }

    pub fn bytes<B: Into<Bytes>>(chunk: B) -> Body {
        Either::Right(Full::from(chunk.into()))
    }
}

async fn handle_connection(
    peer_map: PeerMap,
    ws_stream: WebSocketStream<TokioIo<Upgraded>>,
    addr: SocketAddr,
) {
    // let mut ws_stream = tokio_tungstenite::accept_async(raw_stream)
    //     .await
    //     .expect("Error during the websocket handshake occurred");
    println!("Connected to: {:?}", addr);
    // send salt to client
    // if let Some(salt) = salt {

    //     ws_stream
    //         .send(Message::binary(salt))
    //         .await
    //         .expect("Failed to send salt to client");
    // }

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

async fn handle_request<'a>(
    peer_map: PeerMap,
    mut req: Request<Incoming>,
    addr: SocketAddr,
    hash: Option<[u8; 32]>,
    salt: [u8; 20],
) -> Result<Response<body::Body>, Infallible> {
    // println!("Received a new, potentially ws handshake");
    // println!("The request's path is: {}", req.uri().path());
    // println!("The request's headers are:");
    // for (ref header, _value) in req.headers() {
    //     println!("* {}", header);
    // }
    let upgrade = HeaderValue::from_static("Upgrade");
    let websocket = HeaderValue::from_static("websocket");
    let headers = req.headers();
    let key = headers.get(SEC_WEBSOCKET_KEY);

    let derived = key.map(|k| derive_accept_key(k.as_bytes()));
    if req.method() != Method::GET
        || req.version() < Version::HTTP_11
        || !headers
            .get(CONNECTION)
            .and_then(|h| h.to_str().ok())
            .map(|h| {
                h.split(|c| c == ' ' || c == ',')
                    .any(|p| p.eq_ignore_ascii_case(upgrade.to_str().unwrap()))
            })
            .unwrap_or(false)
        || !headers
            .get(SEC_WEBSOCKET_VERSION)
            .map(|h| h == "13")
            .unwrap_or(false)
        || key.is_none()
        || req.uri() != "/sync"
    {
        return Ok(Response::new(body::bytes("failed")));
    }

    let ver = req.version();
    let mut res = Response::new(body::empty());
    *res.status_mut() = StatusCode::SWITCHING_PROTOCOLS;
    *res.version_mut() = ver;
    res.headers_mut().append(CONNECTION, upgrade);
    res.headers_mut().append(UPGRADE, websocket);
    res.headers_mut()
        .append(SEC_WEBSOCKET_ACCEPT, derived.unwrap().parse().unwrap());

    if let Some(hash) = hash {
        let Some(password) = req.headers().get("X-Password") else {
            return Ok(Response::new(body::bytes("failed")));
        };
        let mut new_hash = [0u8; 32];
        pbkdf2_hmac::<Sha256>(password.as_bytes(), &salt, 60_000, &mut new_hash);
        if hash != new_hash {
            println!("Password is invalid");
            return Ok(Response::new(body::bytes("failed")));
        }
        // let key = Key::<Aes256Gcm>::from_slice(&hash);
        // let cipher = Aes256Gcm::new(key);
        res.headers_mut()
            .append("X-Salt", BASE64_STANDARD.encode(hash).parse().unwrap());
    }

    tokio::task::spawn(async move {
        match hyper::upgrade::on(&mut req).await {
            Ok(upgraded) => {
                let upgraded = TokioIo::new(upgraded);
                handle_connection(
                    peer_map,
                    WebSocketStream::from_raw_socket(upgraded, Role::Server, None).await,
                    addr,
                )
                .await;
            }
            Err(e) => println!("upgrade error: {}", e),
        }
    });

    Ok(res)
}

#[tokio::main]
async fn main() -> Result<(), ProgramError> {
    let cli = Cli::parse();

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
            let mut cipher = None;

            tokio::spawn(read_clipboard(clipboard_tx, cipher.clone()));

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
            // let mut salt: Option<Vec<u8>> = None;
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

            let (write, read) = ws_stream.split();
            let clipboard_to_ws = clipboard_rx.map(Ok).forward(write);

            let ws_to_clipboard = {
                read.for_each(
                    |message: Result<Message, tokio_tungstenite::tungstenite::Error>| async {
                        let mut clipboard = Clipboard::new().unwrap();

                        match message {
                            Ok(message) => {
                                let mut data: Vec<u8> = message.into_data();

                                // decrypt if encryption is enabled
                                if let Some(cipher) = &cipher {
                                    let nonce = GenericArray::from_slice(&data[0..12]);
                                    println!("nonce: {:?}", nonce);

                                    let cipher_text = data[12..].to_vec();
                                    match cipher.decrypt(&nonce, cipher_text.as_ref()) {
                                        Ok(plaintext) => data = plaintext,
                                        Err(e) => {
                                            dbg!(e);

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
            let mut salt = [0u8; 20];
            rand::thread_rng().fill(&mut salt[..]);
            let mut key = [0u8; 32];
            if let Some(password) = &cli.password {
                pbkdf2_hmac::<Sha256>(password.as_bytes(), &salt, 60_000, &mut key);
            }

            let addr = format!("{}:{}", address, port);
            let listener: TcpListener = TcpListener::bind(&addr).await?;
            let local_ip: std::net::IpAddr = local_ip().unwrap();
            println!("Listening on: {}\nLocal IP: {}", addr, local_ip);
            let peers = PeerMap::new(Mutex::new(HashMap::new()));

            loop {
                let (stream, remote_addr) = listener.accept().await?;
                let peers = peers.clone();
                let key = cli.password.is_some().then(|| key.clone());
                let salt = salt.clone();

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
                let mut data_vec: Vec<u8> = Vec::new();
                // extend the vector first with the data type (text = 0 and image = 1)
                data_vec.extend_from_slice(&(0u8).to_be_bytes());
                data_vec.extend_from_slice(data.as_bytes());
                data_vec
            }
            Err(_) => {
                if let Ok(data) = clipboard.get_image() {
                    let mut data_vec = Vec::new();
                    data_vec.extend_from_slice(&(1u8).to_be_bytes());
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
                let nonce: GenericArray<u8, _> = Aes256Gcm::generate_nonce(&mut OsRng);
                println!("nonce: {:?}", nonce);
                let ciphertext = cipher.encrypt(&nonce, data.as_ref()).unwrap();
                // send nonce and ciphertext
                let mut data_vec = Vec::new();
                data_vec.extend_from_slice(&nonce);
                data_vec.extend_from_slice(&ciphertext);

                let nonce_c = GenericArray::from_slice(&data_vec[0..12]);
                println!("nonce_c: {:?}", nonce_c);

                let cipher_text = data_vec[12..].to_vec();

                match cipher.decrypt(&nonce_c, cipher_text.as_ref()) {
                    Ok(plaintext) => println!("{:?}", plaintext),
                    Err(e) => {
                        dbg!(e);

                        return;
                    }
                };

                tx.unbounded_send(Message::binary(data_vec)).unwrap();

                continue;
            }

            tx.unbounded_send(Message::binary(data)).unwrap();
        }
    }
}
