use base64::prelude::{Engine as _, BASE64_STANDARD};
use futures_channel::mpsc::{unbounded, UnboundedSender};
use futures_util::{future, pin_mut, stream::TryStreamExt, StreamExt};
use hyper::{
    body::Incoming,
    header::{
        HeaderValue, CONNECTION, SEC_WEBSOCKET_ACCEPT, SEC_WEBSOCKET_KEY, SEC_WEBSOCKET_VERSION,
        UPGRADE,
    },
    upgrade::Upgraded,
    Method, Request, Response, StatusCode, Version,
};
use hyper_util::rt::TokioIo;
use pbkdf2::pbkdf2_hmac;
use sha2::Sha256;
use std::{
    collections::HashMap,
    convert::Infallible,
    net::SocketAddr,
    sync::{Arc, Mutex},
};
use tokio_tungstenite::{
    tungstenite::{
        handshake::derive_accept_key,
        protocol::{Message, Role},
    },
    WebSocketStream,
};

type Tx = UnboundedSender<Message>;
pub type PeerMap = Arc<Mutex<HashMap<SocketAddr, Tx>>>;

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

pub async fn handle_request<'a>(
    peer_map: PeerMap,
    mut req: Request<Incoming>,
    addr: SocketAddr,
    hash: Option<[u8; 32]>,
    salt: [u8; 20],
) -> Result<Response<body::Body>, Infallible> {
    // check if its a valid websocket connection
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
        return Ok(Response::new(body::bytes(
            "failed to establish websocket connection",
        )));
    }

    // add headers to response
    let ver = req.version();
    let mut res = Response::new(body::empty());
    *res.status_mut() = StatusCode::SWITCHING_PROTOCOLS;
    *res.version_mut() = ver;
    res.headers_mut().append(CONNECTION, upgrade);
    res.headers_mut().append(UPGRADE, websocket);
    res.headers_mut()
        .append(SEC_WEBSOCKET_ACCEPT, derived.unwrap().parse().unwrap());

    // if encryption is enabled, check password and send salt
    //TODO: password should not be sent in plaintext (should prob use tls or switch to a different encryption system)
    if let Some(hash) = hash {
        let Some(password) = req.headers().get("X-Password") else {
            return Ok(Response::new(body::bytes("no password")));
        };
        let mut new_hash = [0u8; 32];
        pbkdf2_hmac::<Sha256>(password.as_bytes(), &salt, 60_000, &mut new_hash);
        if hash != new_hash {
            return Ok(Response::new(body::bytes("invalid password")));
        }
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

async fn handle_connection(
    peer_map: PeerMap,
    ws_stream: WebSocketStream<TokioIo<Upgraded>>,
    addr: SocketAddr,
) {
    println!("Connected to: {:?}", addr);

    let (tx, rx) = unbounded();
    peer_map.lock().unwrap().insert(addr, tx);

    let (outgoing, incoming) = ws_stream.split();

    let broadcast_incoming = incoming.try_for_each(|msg| {
        let peers: std::sync::MutexGuard<'_, HashMap<SocketAddr, UnboundedSender<Message>>> =
            peer_map.lock().unwrap();

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
