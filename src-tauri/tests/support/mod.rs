//! Test doubles for Riot's local servers: a throwaway CA standing in for Riot's root,
//! a fake Live Client Data API (`127.0.0.1:<port>/liveclientdata/*`) that replays a
//! fixture with a controllable game clock, and a fake League client (HTTPS + WSS).
#![allow(dead_code)] // each test crate uses a different subset

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, Issuer, KeyPair};
use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
use serde_json::{json, Value};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;
use tokio_tungstenite::tungstenite::handshake::derive_accept_key;
use tokio_tungstenite::tungstenite::protocol::Role;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

use riftreel_lib::replay::Fixture;

pub fn fixture_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../fixtures")
        .join(relative)
}

pub fn load_fixture(name: &str) -> Fixture {
    Fixture::load(fixture_path(&format!("lol/{name}.json"))).unwrap()
}

pub fn load_json(relative: &str) -> Value {
    serde_json::from_str(&std::fs::read_to_string(fixture_path(relative)).unwrap()).unwrap()
}

/// Polls `condition` until it holds or `timeout` passes.
pub async fn eventually(timeout: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    while tokio::time::Instant::now() < deadline {
        if condition() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    condition()
}

// ---------------------------------------------------------------------------
// PKI
// ---------------------------------------------------------------------------

/// A throwaway CA and a `127.0.0.1`/`localhost` server certificate it signed, in
/// place of Riot's root and the game's certificate.
pub struct TestPki {
    pub ca_pem: String,
    pub server: Arc<rustls::ServerConfig>,
}

impl TestPki {
    pub fn new() -> Self {
        let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
        ca_params
            .distinguished_name
            .push(DnType::CommonName, "RiftReel Test CA");
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        let ca_key = KeyPair::generate().unwrap();
        let ca_cert = ca_params.self_signed(&ca_key).unwrap();
        let issuer = Issuer::new(ca_params, ca_key);

        let mut leaf_params =
            CertificateParams::new(vec!["127.0.0.1".to_owned(), "localhost".to_owned()]).unwrap();
        leaf_params
            .distinguished_name
            .push(DnType::CommonName, "rclient");
        let leaf_key = KeyPair::generate().unwrap();
        let leaf = leaf_params.signed_by(&leaf_key, &issuer).unwrap();

        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(leaf_key.serialize_der()));
        let server = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![leaf.der().clone()], key)
        .unwrap();
        Self {
            ca_pem: ca_cert.pem(),
            server: Arc::new(server),
        }
    }

    /// A client config trusting only this test CA.
    pub fn client_config(&self) -> rustls::ClientConfig {
        riftreel_lib::league::tls::client_config_for_root(self.ca_pem.as_bytes()).unwrap()
    }
}

// ---------------------------------------------------------------------------
// Minimal HTTP/1.1
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Request {
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// Reads one request head (GET requests carry no body).
async fn read_request<S: AsyncRead + Unpin>(stream: &mut S) -> Option<Request> {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if stream.read(&mut byte).await.ok()? == 0 {
            return None;
        }
        head.push(byte[0]);
        if head.len() > 64 * 1024 {
            return None;
        }
    }
    let text = String::from_utf8_lossy(&head);
    let mut lines = text.split("\r\n");
    let mut request_line = lines.next()?.split(' ');
    let method = request_line.next()?.to_owned();
    let path = request_line.next()?.to_owned();
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned()))
        .collect();
    Some(Request {
        method,
        path,
        headers,
    })
}

fn http_response(status: u16, body: &str) -> String {
    let reason = match status {
        200 => "OK",
        401 => "Unauthorized",
        404 => "Not Found",
        _ => "Error",
    };
    format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
}

fn not_found() -> (u16, String) {
    (
        404,
        json!({"errorCode": "RESOURCE_NOT_FOUND", "httpStatus": 404, "message": "Not found"})
            .to_string(),
    )
}

// ---------------------------------------------------------------------------
// Fake Live Client Data API
// ---------------------------------------------------------------------------

pub struct LiveState {
    pub fixture: Fixture,
    /// Current game time; `eventdata` returns the history up to here.
    pub game_time: f64,
    /// `false`: the game isn't running (connections are dropped).
    pub up: bool,
    /// Serve only the first N events (a restarted game still loading its history).
    pub max_events: Option<usize>,
    /// Leave out events before this game time (a restarted game that lost history)...
    pub history_from: f64,
    /// ...and number what's left from 0, as a fresh game process would.
    pub renumber: bool,
    pub requests: Vec<String>,
}

pub struct FakeLiveServer {
    pub addr: SocketAddr,
    pub state: Arc<Mutex<LiveState>>,
    up: tokio::sync::watch::Sender<bool>,
}

impl FakeLiveServer {
    pub async fn start(pki: &TestPki, fixture: Fixture) -> Self {
        let state = Arc::new(Mutex::new(LiveState {
            fixture,
            game_time: 0.0,
            up: true,
            max_events: None,
            history_from: 0.0,
            renumber: false,
            requests: Vec::new(),
        }));
        let (up, _) = tokio::sync::watch::channel(true);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let acceptor = TlsAcceptor::from(pki.server.clone());
        let shared = state.clone();
        let up_signal = up.clone();
        tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                if !shared.lock().unwrap().up {
                    drop(tcp); // game not running: the client sees the connection die
                    continue;
                }
                let acceptor = acceptor.clone();
                let shared = shared.clone();
                let mut up_rx = up_signal.subscribe();
                tokio::spawn(async move {
                    let Ok(mut tls) = acceptor.accept(tcp).await else {
                        return;
                    };
                    // Keep-alive: serve requests until the client hangs up or the game
                    // "dies", which closes open connections like a crashing process.
                    loop {
                        tokio::select! {
                            request = read_request(&mut tls) => {
                                let Some(request) = request else { return };
                                let Some((status, body)) = live_route(&shared, &request) else { return };
                                if tls.write_all(http_response(status, &body).as_bytes()).await.is_err() {
                                    return;
                                }
                            }
                            changed = up_rx.changed() => {
                                if changed.is_err() || !*up_rx.borrow() {
                                    return;
                                }
                            }
                        }
                    }
                });
            }
        });
        Self { addr, state, up }
    }

    pub fn url(&self) -> String {
        format!("https://127.0.0.1:{}", self.addr.port())
    }

    pub fn set_game_time(&self, game_time: f64) {
        self.state.lock().unwrap().game_time = game_time;
    }

    pub fn set_up(&self, up: bool) {
        self.state.lock().unwrap().up = up;
        self.up.send_replace(up);
    }

    pub fn with<R>(&self, f: impl FnOnce(&mut LiveState) -> R) -> R {
        f(&mut self.state.lock().unwrap())
    }
}

fn live_route(shared: &Mutex<LiveState>, request: &Request) -> Option<(u16, String)> {
    let mut state = shared.lock().unwrap();
    if !state.up {
        return None;
    }
    state.requests.push(request.path.clone());
    let fixture = &state.fixture;
    Some(match request.path.as_str() {
        "/liveclientdata/eventdata" => {
            let mut events: Vec<Value> = fixture.eventdata_at(state.game_time)["Events"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|e| e["EventTime"].as_f64().unwrap_or(0.0) >= state.history_from)
                .cloned()
                .collect();
            if let Some(max) = state.max_events {
                events.truncate(max);
            }
            if state.renumber {
                for (index, event) in events.iter_mut().enumerate() {
                    event["EventID"] = json!(index);
                }
            }
            (200, json!({ "Events": events }).to_string())
        }
        "/liveclientdata/activeplayer" => match &fixture.activeplayer {
            Value::Null => (
                404,
                fixture
                    .activeplayer_error
                    .clone()
                    .unwrap_or_else(|| json!({}))
                    .to_string(),
            ),
            player => (200, player.to_string()),
        },
        "/liveclientdata/activeplayername" => match fixture.activeplayer["riotId"].as_str() {
            Some(name) => (200, json!(name).to_string()),
            None => not_found(),
        },
        "/liveclientdata/playerlist" => (200, fixture.playerlist.to_string()),
        "/liveclientdata/gamestats" => (200, fixture.gamestats_at(state.game_time).to_string()),
        _ => not_found(),
    })
}

// ---------------------------------------------------------------------------
// Fake League client (LCU)
// ---------------------------------------------------------------------------

pub struct LcuState {
    pub password: String,
    pub phase: String,
    pub session: Value,
    pub eog: Option<Value>,
    /// Frames pushed to every WebSocket client after it subscribes; then the socket closes.
    pub frames: Vec<String>,
    /// Text messages received from WebSocket clients.
    pub received: Vec<String>,
    pub unauthorized: usize,
}

pub struct FakeLcuServer {
    pub addr: SocketAddr,
    pub state: Arc<Mutex<LcuState>>,
}

impl FakeLcuServer {
    pub async fn start(pki: &TestPki, password: &str) -> Self {
        let frames: Vec<String> =
            serde_json::from_value(load_json("lol/lcu/gameflow_phase_frames.json")).unwrap();
        let state = Arc::new(Mutex::new(LcuState {
            password: password.to_owned(),
            phase: "InProgress".into(),
            session: load_json("lol/lcu/ranked_win.session.json"),
            eog: Some(load_json("lol/lcu/ranked_win.eog.json")),
            frames,
            received: Vec::new(),
            unauthorized: 0,
        }));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let acceptor = TlsAcceptor::from(pki.server.clone());
        let shared = state.clone();
        tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                let acceptor = acceptor.clone();
                let shared = shared.clone();
                tokio::spawn(async move {
                    let Ok(tls) = acceptor.accept(tcp).await else {
                        return;
                    };
                    serve_lcu_connection(tls, shared).await;
                });
            }
        });
        Self { addr, state }
    }

    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    pub fn with<R>(&self, f: impl FnOnce(&mut LcuState) -> R) -> R {
        f(&mut self.state.lock().unwrap())
    }
}

async fn serve_lcu_connection(
    mut tls: tokio_rustls::server::TlsStream<tokio::net::TcpStream>,
    shared: Arc<Mutex<LcuState>>,
) {
    while let Some(request) = read_request(&mut tls).await {
        let expected = {
            use base64::Engine as _;
            let password = shared.lock().unwrap().password.clone();
            format!(
                "Basic {}",
                base64::engine::general_purpose::STANDARD.encode(format!("riot:{password}"))
            )
        };
        if request.header("authorization") != Some(expected.as_str()) {
            shared.lock().unwrap().unauthorized += 1;
            let _ = tls.write_all(http_response(401, "{}").as_bytes()).await;
            return;
        }

        if request
            .header("upgrade")
            .is_some_and(|u| u.eq_ignore_ascii_case("websocket"))
        {
            let key = request.header("sec-websocket-key").unwrap_or_default();
            let handshake = format!(
                "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {}\r\n\r\n",
                derive_accept_key(key.as_bytes())
            );
            if tls.write_all(handshake.as_bytes()).await.is_err() {
                return;
            }
            let mut socket = WebSocketStream::from_raw_socket(tls, Role::Server, None).await;
            // Wait for the subscription, then push the scripted phase changes.
            if let Some(Ok(Message::Text(text))) = socket.next().await {
                shared
                    .lock()
                    .unwrap()
                    .received
                    .push(text.as_str().to_owned());
            }
            let frames = shared.lock().unwrap().frames.clone();
            for frame in frames {
                if socket.send(Message::text(frame)).await.is_err() {
                    return;
                }
            }
            let _ = socket.close(None).await;
            return;
        }

        let (status, body) = {
            let state = shared.lock().unwrap();
            match request.path.as_str() {
                "/lol-gameflow/v1/gameflow-phase" => (200, json!(state.phase).to_string()),
                "/lol-gameflow/v1/session" => (200, state.session.to_string()),
                "/lol-end-of-game/v1/eog-stats-block" => match &state.eog {
                    Some(eog) => (200, eog.to_string()),
                    None => not_found(),
                },
                _ => not_found(),
            }
        };
        if tls
            .write_all(http_response(status, &body).as_bytes())
            .await
            .is_err()
        {
            return;
        }
    }
}
