//! League client (LCU) access: match lifecycle, queue/mode and end-of-game stats.
//!
//! Riot says the LCU API is "not officially supported" for third parties, so the
//! tracker only sees it through the [`LeagueClient`] trait and must keep working
//! without it (degraded mode, see `tracker::machine`).
//!
//! Connection details come from `<install dir>\lockfile`
//! (`name:pid:port:password:protocol`), next to `LeagueClientUx.exe`, or from that
//! process's `--app-port` / `--remoting-auth-token` arguments. Requests use HTTPS and
//! WSS on `127.0.0.1` with basic auth `riot:<password>`, verified against Riot's root
//! certificate.

use std::ffi::OsStr;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use futures_util::future::BoxFuture;
use futures_util::{SinkExt, StreamExt};
use serde::de::DeserializeOwned;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::{HeaderValue, AUTHORIZATION};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::Connector;

use super::models::{EogStatsBlock, GameflowPhase, GameflowSession};
use super::tls::{riot_client_config, TlsSetupError};

/// The client UI process; the lockfile sits in the same folder.
pub const CLIENT_PROCESS: &str = "LeagueClientUx.exe";

/// WAMP topic for gameflow phase changes.
pub const GAMEFLOW_PHASE_EVENT: &str = "OnJsonApiEvent_lol-gameflow_v1_gameflow-phase";

const WAMP_SUBSCRIBE: u8 = 5;
const WAMP_EVENT: u8 = 8;

#[derive(Debug, thiserror::Error)]
pub enum LcuError {
    #[error("League client not running")]
    NotRunning,
    #[error("League client credentials not found: {0}")]
    Credentials(String),
    #[error("League client request failed: {0}")]
    Transport(String),
    #[error("League client returned HTTP {status} for {path}")]
    Status { status: u16, path: String },
    #[error("unexpected League client response for {path}: {detail}")]
    Decode { path: String, detail: String },
    #[error("League client WebSocket error: {0}")]
    WebSocket(String),
    #[error(transparent)]
    Setup(#[from] TlsSetupError),
}

/// What the tracker needs from the League client. Absent in degraded mode.
pub trait LeagueClient: Send + Sync {
    fn gameflow_phase(&self) -> BoxFuture<'_, Result<GameflowPhase, LcuError>>;
    fn gameflow_session(&self) -> BoxFuture<'_, Result<GameflowSession, LcuError>>;
    fn eog_stats_block(&self) -> BoxFuture<'_, Result<EogStatsBlock, LcuError>>;
}

// ---------------------------------------------------------------------------
// Credentials and discovery
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialSource {
    Lockfile,
    CommandLine,
}

#[derive(Clone, PartialEq, Eq)]
pub struct LcuCredentials {
    pub port: u16,
    password: String,
    pub pid: Option<u32>,
    pub source: CredentialSource,
}

impl fmt::Debug for LcuCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LcuCredentials")
            .field("port", &self.port)
            .field("password", &"<redacted>")
            .field("pid", &self.pid)
            .field("source", &self.source)
            .finish()
    }
}

impl LcuCredentials {
    pub fn new(port: u16, password: impl Into<String>, source: CredentialSource) -> Self {
        Self {
            port,
            password: password.into(),
            pid: None,
            source,
        }
    }

    /// Parses `LeagueClient:<pid>:<port>:<password>:<protocol>`.
    pub fn parse_lockfile(contents: &str) -> Result<Self, LcuError> {
        let bad = |what: &str| LcuError::Credentials(format!("malformed lockfile ({what})"));
        let mut head = contents.trim().splitn(4, ':');
        let _name = head.next().ok_or_else(|| bad("name"))?;
        let pid = head.next().ok_or_else(|| bad("pid"))?;
        let port = head.next().ok_or_else(|| bad("port"))?;
        // The remainder is "<password>:<protocol>". Split from the right in case the
        // password ever contains a colon.
        let (password, protocol) = head
            .next()
            .and_then(|rest| rest.rsplit_once(':'))
            .ok_or_else(|| bad("password"))?;
        if !protocol.eq_ignore_ascii_case("https") {
            return Err(bad("protocol"));
        }
        let port: u16 = port.parse().map_err(|_| bad("port"))?;
        if password.is_empty() {
            return Err(bad("password"));
        }
        Ok(Self {
            port,
            password: password.to_owned(),
            pid: pid.parse().ok(),
            source: CredentialSource::Lockfile,
        })
    }

    /// Reads `--app-port=` and `--remoting-auth-token=` from `LeagueClientUx.exe`'s
    /// arguments.
    pub fn from_args<I, S>(args: I) -> Option<Self>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut port = None;
        let mut token = None;
        for arg in args {
            let arg = arg.as_ref().trim().trim_matches('"');
            if let Some(value) = arg.strip_prefix("--app-port=") {
                port = value.trim_matches('"').parse::<u16>().ok();
            } else if let Some(value) = arg.strip_prefix("--remoting-auth-token=") {
                let value = value.trim_matches('"');
                if !value.is_empty() {
                    token = Some(value.to_owned());
                }
            }
        }
        Some(Self::new(port?, token?, CredentialSource::CommandLine))
    }

    /// `Authorization` header value: `Basic base64("riot:<password>")`.
    pub fn basic_auth(&self) -> String {
        let encoded =
            base64::engine::general_purpose::STANDARD.encode(format!("riot:{}", self.password));
        format!("Basic {encoded}")
    }

    pub fn password(&self) -> &str {
        &self.password
    }
}

/// The game itself (its window is what gets recorded).
pub const GAME_PROCESS: &str = "League of Legends.exe";

/// A running `LeagueClientUx.exe`, as far as discovery cares.
#[derive(Debug, Clone)]
pub struct ClientProcess {
    pub pid: u32,
    pub exe: Option<PathBuf>,
    pub args: Vec<String>,
}

/// What a process-table scan found.
#[derive(Debug, Clone, Default)]
pub struct ProcessScan {
    pub client: Option<ClientProcess>,
    pub game_running: bool,
}

/// One pass over the process table: names only, plus the executable path and arguments
/// of `LeagueClientUx.exe` when it's there. Blocking; takes a few milliseconds.
pub fn scan_processes() -> ProcessScan {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

    let named = |process: &sysinfo::Process, name: &str| {
        process.name().eq_ignore_ascii_case(OsStr::new(name))
    };

    let mut system = System::new();
    system.refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::nothing());
    let game_running = system.processes().values().any(|p| named(p, GAME_PROCESS));
    let client_pid: Option<Pid> = system
        .processes()
        .values()
        .find(|p| named(p, CLIENT_PROCESS))
        .map(|p| p.pid());

    let client = client_pid.and_then(|pid| {
        system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[pid]),
            false,
            ProcessRefreshKind::nothing()
                .with_exe(UpdateKind::Always)
                .with_cmd(UpdateKind::Always),
        );
        let process = system.process(pid)?;
        Some(ClientProcess {
            pid: pid.as_u32(),
            exe: process.exe().map(Path::to_path_buf),
            args: process
                .cmd()
                .iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect(),
        })
    });
    ProcessScan {
        client,
        game_running,
    }
}

/// Finds `LeagueClientUx.exe` in the process table.
pub fn find_client_process() -> Option<ClientProcess> {
    scan_processes().client
}

/// Credentials for a running client: the lockfile next to the executable first, then
/// the command line.
pub fn credentials_for(process: &ClientProcess) -> Result<LcuCredentials, LcuError> {
    let lockfile_error = match process.exe.as_deref().and_then(Path::parent) {
        Some(dir) => match std::fs::read_to_string(dir.join("lockfile")) {
            Ok(contents) => match LcuCredentials::parse_lockfile(&contents) {
                Ok(creds) => return Ok(creds),
                Err(e) => e.to_string(),
            },
            Err(e) => format!("lockfile unreadable: {e}"),
        },
        None => "client executable path unavailable".to_owned(),
    };
    LcuCredentials::from_args(&process.args).ok_or_else(|| {
        LcuError::Credentials(format!(
            "{lockfile_error}; no --app-port/--remoting-auth-token arguments"
        ))
    })
}

/// Finds the running client and its credentials.
pub fn discover() -> Result<LcuCredentials, LcuError> {
    let process = find_client_process().ok_or(LcuError::NotRunning)?;
    credentials_for(&process)
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

/// HTTPS + WSS client for one League client instance.
#[derive(Clone)]
pub struct LcuClient {
    creds: LcuCredentials,
    http: reqwest::Client,
    tls: Arc<rustls::ClientConfig>,
    host: String,
}

impl fmt::Debug for LcuClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LcuClient")
            .field("creds", &self.creds)
            .finish_non_exhaustive()
    }
}

impl LcuClient {
    /// Trusting only Riot's root certificate.
    pub fn new(creds: LcuCredentials) -> Result<Self, LcuError> {
        Self::with_tls(creds, riot_client_config()?)
    }

    pub fn with_tls(creds: LcuCredentials, tls: rustls::ClientConfig) -> Result<Self, LcuError> {
        let http = reqwest::Client::builder()
            .tls_backend_preconfigured(tls.clone())
            .no_proxy()
            .connect_timeout(Duration::from_secs(2))
            .timeout(Duration::from_secs(5))
            .build()
            .map_err(|e| LcuError::Transport(e.to_string()))?;
        Ok(Self {
            host: format!("127.0.0.1:{}", creds.port),
            creds,
            http,
            tls: Arc::new(tls),
        })
    }

    pub fn credentials(&self) -> &LcuCredentials {
        &self.creds
    }

    async fn get_json<T: DeserializeOwned>(&self, path: &str) -> Result<T, LcuError> {
        let response = self
            .http
            .get(format!("https://{}{path}", self.host))
            .header(AUTHORIZATION, self.creds.basic_auth())
            .send()
            .await
            .map_err(|e| LcuError::Transport(e.to_string()))?;
        let status = response.status();
        if !status.is_success() {
            return Err(LcuError::Status {
                status: status.as_u16(),
                path: path.to_owned(),
            });
        }
        let body = response
            .bytes()
            .await
            .map_err(|e| LcuError::Transport(e.to_string()))?;
        serde_json::from_slice(&body).map_err(|e| LcuError::Decode {
            path: path.to_owned(),
            detail: e.to_string(),
        })
    }

    pub async fn get_gameflow_phase(&self) -> Result<GameflowPhase, LcuError> {
        self.get_json("/lol-gameflow/v1/gameflow-phase").await
    }

    pub async fn get_gameflow_session(&self) -> Result<GameflowSession, LcuError> {
        self.get_json("/lol-gameflow/v1/session").await
    }

    pub async fn get_eog_stats_block(&self) -> Result<EogStatsBlock, LcuError> {
        self.get_json("/lol-end-of-game/v1/eog-stats-block").await
    }

    /// Opens the client's WebSocket and subscribes to gameflow phase changes. Only
    /// changes are pushed; read the current phase with [`Self::get_gameflow_phase`].
    pub async fn subscribe_gameflow_phase(&self) -> Result<PhaseSubscription, LcuError> {
        let ws_err = |e: tokio_tungstenite::tungstenite::Error| LcuError::WebSocket(e.to_string());
        let mut request = format!("wss://{}/", self.host)
            .into_client_request()
            .map_err(ws_err)?;
        let auth = HeaderValue::from_str(&self.creds.basic_auth())
            .map_err(|e| LcuError::WebSocket(e.to_string()))?;
        request.headers_mut().insert(AUTHORIZATION, auth);

        let (mut socket, _response) = tokio_tungstenite::connect_async_tls_with_config(
            request,
            None,
            true,
            Some(Connector::Rustls(self.tls.clone())),
        )
        .await
        .map_err(ws_err)?;
        socket
            .send(Message::text(format!(
                "[{WAMP_SUBSCRIBE},\"{GAMEFLOW_PHASE_EVENT}\"]"
            )))
            .await
            .map_err(ws_err)?;

        let (tx, rx) = mpsc::channel(16);
        let task = tokio::spawn(async move {
            while let Some(message) = socket.next().await {
                match message {
                    Ok(Message::Text(text)) => {
                        if let Some(phase) = parse_phase_event(text.as_str()) {
                            if tx.send(phase).await.is_err() {
                                break;
                            }
                        }
                    }
                    Ok(Message::Close(_)) | Err(_) => break,
                    // Pings are answered by tungstenite while we keep reading.
                    Ok(_) => {}
                }
            }
            tracing::debug!("LCU WebSocket closed");
        });
        Ok(PhaseSubscription { rx, task })
    }
}

impl LeagueClient for LcuClient {
    fn gameflow_phase(&self) -> BoxFuture<'_, Result<GameflowPhase, LcuError>> {
        Box::pin(self.get_gameflow_phase())
    }

    fn gameflow_session(&self) -> BoxFuture<'_, Result<GameflowSession, LcuError>> {
        Box::pin(self.get_gameflow_session())
    }

    fn eog_stats_block(&self) -> BoxFuture<'_, Result<EogStatsBlock, LcuError>> {
        Box::pin(self.get_eog_stats_block())
    }
}

/// Gameflow phase changes pushed over the client's WebSocket.
pub struct PhaseSubscription {
    rx: mpsc::Receiver<GameflowPhase>,
    task: JoinHandle<()>,
}

impl PhaseSubscription {
    /// The next phase change, or `None` once the socket closed (client exited/restarted).
    pub async fn next(&mut self) -> Option<GameflowPhase> {
        self.rx.recv().await
    }
}

impl Drop for PhaseSubscription {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Parses a WAMP event frame for the gameflow phase topic:
/// `[8, "OnJsonApiEvent_lol-gameflow_v1_gameflow-phase", {"data": "InProgress", "eventType": "Update", "uri": "…"}]`.
/// A `Delete` event means the phase went back to `None`.
pub fn parse_phase_event(frame: &str) -> Option<GameflowPhase> {
    let value: serde_json::Value = serde_json::from_str(frame).ok()?;
    let parts = value.as_array()?;
    if parts.first()?.as_u64()? != u64::from(WAMP_EVENT)
        || parts.get(1)?.as_str()? != GAMEFLOW_PHASE_EVENT
    {
        return None;
    }
    let payload = parts.get(2)?;
    if payload.get("eventType").and_then(|t| t.as_str()) == Some("Delete") {
        return Some(GameflowPhase::None);
    }
    Some(match payload.get("data")?.as_str() {
        Some(phase) => GameflowPhase::parse(phase),
        None => GameflowPhase::None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_lockfile() {
        let creds =
            LcuCredentials::parse_lockfile("LeagueClient:21456:51234:s3cr3t-T0ken_x:https\r\n")
                .unwrap();
        assert_eq!(creds.port, 51234);
        assert_eq!(creds.pid, Some(21456));
        assert_eq!(creds.password(), "s3cr3t-T0ken_x");
        assert_eq!(creds.source, CredentialSource::Lockfile);
    }

    #[test]
    fn lockfile_password_may_contain_colons() {
        let creds = LcuCredentials::parse_lockfile("LeagueClient:1:2999:pa:ss:https").unwrap();
        assert_eq!(creds.password(), "pa:ss");
    }

    #[test]
    fn rejects_malformed_lockfiles() {
        for bad in [
            "",
            "LeagueClient",
            "LeagueClient:1:notaport:pw:https",
            "LeagueClient:1:123::https",
            "LeagueClient:1:123:pw:http",
        ] {
            assert!(LcuCredentials::parse_lockfile(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn reads_credentials_from_arguments() {
        let args = [
            r"C:\Riot Games\League of Legends\LeagueClientUx.exe",
            "--riotclient-auth-token=other",
            "\"--remoting-auth-token=tok3n\"",
            "--respawn-command=LeagueClient.exe",
            "--app-port=54321",
        ];
        let creds = LcuCredentials::from_args(args).unwrap();
        assert_eq!(creds.port, 54321);
        assert_eq!(creds.password(), "tok3n");
        assert_eq!(creds.source, CredentialSource::CommandLine);
        assert!(LcuCredentials::from_args(["--app-port=1"]).is_none());
    }

    #[test]
    fn basic_auth_and_debug_redaction() {
        let creds = LcuCredentials::new(1, "secret", CredentialSource::Lockfile);
        assert_eq!(creds.basic_auth(), "Basic cmlvdDpzZWNyZXQ=");
        assert!(!format!("{creds:?}").contains("secret"));
    }

    #[test]
    fn credentials_prefer_lockfile_then_arguments() {
        let dir = std::env::temp_dir().join(format!("downplayed-lcu-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let process = ClientProcess {
            pid: 1,
            exe: Some(dir.join(CLIENT_PROCESS)),
            args: vec![
                "--app-port=2000".into(),
                "--remoting-auth-token=fromargs".into(),
            ],
        };
        // No lockfile yet: falls back to the arguments.
        assert_eq!(
            credentials_for(&process).unwrap().source,
            CredentialSource::CommandLine
        );

        std::fs::write(dir.join("lockfile"), "LeagueClient:9:3000:fromlock:https").unwrap();
        let creds = credentials_for(&process).unwrap();
        assert_eq!(
            (creds.port, creds.password(), creds.source),
            (3000, "fromlock", CredentialSource::Lockfile)
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn parses_wamp_phase_events() {
        let frame = r#"[8,"OnJsonApiEvent_lol-gameflow_v1_gameflow-phase",{"data":"ChampSelect","eventType":"Update","uri":"/lol-gameflow/v1/gameflow-phase"}]"#;
        assert_eq!(parse_phase_event(frame), Some(GameflowPhase::ChampSelect));
        let delete = r#"[8,"OnJsonApiEvent_lol-gameflow_v1_gameflow-phase",{"data":null,"eventType":"Delete","uri":"/lol-gameflow/v1/gameflow-phase"}]"#;
        assert_eq!(parse_phase_event(delete), Some(GameflowPhase::None));
        // Welcome frame, other topics and junk are ignored.
        assert_eq!(parse_phase_event(r#"[0,"abc",1,"server"]"#), None);
        assert_eq!(
            parse_phase_event(r#"[8,"OnJsonApiEvent_lol-chat",{"data":"x"}]"#),
            None
        );
        assert_eq!(parse_phase_event("not json"), None);
    }
}
