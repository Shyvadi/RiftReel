//! Client for Riot's Live Client Data API, `https://127.0.0.1:2999/liveclientdata/*`.
//!
//! The API is only up while a match runs, and it is official and documented. RiftReel
//! reads `eventdata` (every [`EVENT_POLL_INTERVAL`]), `activeplayer`, `playerlist` and
//! `gamestats`. [`EventCursor`] turns the full history `eventdata` returns into a
//! stream of new events.

use std::collections::HashSet;
use std::error::Error as _;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io;
use std::time::Duration;

use serde::de::DeserializeOwned;

use super::models::{ActivePlayer, ApiErrorBody, EventData, GameStats, Player, RawEvent};
use super::tls::{riot_client_config, TlsSetupError};

pub const DEFAULT_BASE_URL: &str = "https://127.0.0.1:2999";

/// How often `eventdata` is polled during a match.
pub const EVENT_POLL_INTERVAL: Duration = Duration::from_millis(500);

#[derive(Debug, thiserror::Error)]
pub enum LiveError {
    /// Nothing answered: no game running, still loading, or the game exited/crashed.
    #[error("live client API not reachable")]
    Unreachable,
    /// The API answered with an error status: 404 during loading, or in spectator and
    /// replay sessions for `activeplayer`.
    #[error("live client API returned HTTP {status}")]
    Status { status: u16, code: Option<String> },
    /// TLS handshake or certificate verification failed.
    #[error("TLS failure talking to the live client API: {0}")]
    Tls(String),
    #[error("live client API timed out")]
    Timeout,
    #[error("unexpected live client API response for {endpoint}: {detail}")]
    Decode {
        endpoint: &'static str,
        detail: String,
    },
    #[error("live client API request failed: {0}")]
    Transport(String),
    #[error(transparent)]
    Setup(#[from] TlsSetupError),
}

impl LiveError {
    /// The game simply isn't there (as opposed to a protocol or TLS problem).
    pub fn is_unavailable(&self) -> bool {
        matches!(self, Self::Unreachable | Self::Timeout)
    }
}

/// A response as the API sent it: status and JSON body (`Null` if the body isn't JSON).
#[derive(Debug, Clone)]
pub struct RawResponse {
    pub status: u16,
    pub body: serde_json::Value,
}

/// HTTP client for the Live Client Data API.
#[derive(Clone)]
pub struct LiveClient {
    http: reqwest::Client,
    base: String,
}

impl LiveClient {
    /// `https://127.0.0.1:2999`, trusting only Riot's root certificate.
    pub fn new() -> Result<Self, LiveError> {
        Self::with_tls(DEFAULT_BASE_URL, riot_client_config()?)
    }

    /// A client for another base URL and trust root (tests use a fake server).
    pub fn with_tls(
        base_url: impl Into<String>,
        tls: rustls::ClientConfig,
    ) -> Result<Self, LiveError> {
        let http = reqwest::Client::builder()
            .tls_backend_preconfigured(tls)
            .no_proxy()
            .connect_timeout(Duration::from_secs(1))
            .timeout(Duration::from_secs(3))
            .pool_max_idle_per_host(2)
            .build()
            .map_err(|e| LiveError::Transport(e.to_string()))?;
        let base = base_url.into().trim_end_matches('/').to_owned();
        Ok(Self { http, base })
    }

    pub fn base_url(&self) -> &str {
        &self.base
    }

    async fn get<T: DeserializeOwned>(&self, endpoint: &'static str) -> Result<T, LiveError> {
        let url = format!("{}/liveclientdata/{endpoint}", self.base);
        let response = self.http.get(url).send().await.map_err(classify)?;
        let status = response.status();
        let body = response.bytes().await.map_err(classify)?;
        if !status.is_success() {
            let error: ApiErrorBody = serde_json::from_slice(&body).unwrap_or_default();
            return Err(LiveError::Status {
                status: status.as_u16(),
                code: error.error_code,
            });
        }
        serde_json::from_slice(&body).map_err(|e| LiveError::Decode {
            endpoint,
            detail: e.to_string(),
        })
    }

    /// `GET /liveclientdata/<endpoint>` without interpreting it; error statuses are
    /// returned too (the fixture recorder keeps payloads verbatim).
    pub async fn get_raw(&self, endpoint: &str) -> Result<RawResponse, LiveError> {
        let url = format!("{}/liveclientdata/{endpoint}", self.base);
        let response = self.http.get(url).send().await.map_err(classify)?;
        let status = response.status().as_u16();
        let body = response.bytes().await.map_err(classify)?;
        Ok(RawResponse {
            status,
            body: serde_json::from_slice(&body).unwrap_or_default(),
        })
    }

    pub async fn event_data(&self) -> Result<EventData, LiveError> {
        self.get("eventdata").await
    }

    pub async fn active_player(&self) -> Result<ActivePlayer, LiveError> {
        self.get("activeplayer").await
    }

    /// The local player's Riot ID (`Name#TAG`).
    pub async fn active_player_name(&self) -> Result<String, LiveError> {
        self.get("activeplayername").await
    }

    pub async fn player_list(&self) -> Result<Vec<Player>, LiveError> {
        self.get("playerlist").await
    }

    pub async fn game_stats(&self) -> Result<GameStats, LiveError> {
        self.get("gamestats").await
    }
}

/// Splits reqwest errors into "game not there" versus real failures. A TLS rejection
/// must not be mistaken for "not running".
fn classify(err: reqwest::Error) -> LiveError {
    if err.is_timeout() {
        return LiveError::Timeout;
    }
    let mut cause: Option<&(dyn std::error::Error + 'static)> = err.source();
    while let Some(current) = cause {
        if let Some(tls) = current.downcast_ref::<rustls::Error>() {
            return LiveError::Tls(tls.to_string());
        }
        // A reused keep-alive connection whose process died mid-request.
        if let Some(http) = current.downcast_ref::<hyper::Error>() {
            if http.is_incomplete_message() || http.is_closed() || http.is_canceled() {
                return LiveError::Unreachable;
            }
        }
        if let Some(io) = current.downcast_ref::<io::Error>() {
            if matches!(
                io.kind(),
                io::ErrorKind::ConnectionRefused
                    | io::ErrorKind::ConnectionReset
                    | io::ErrorKind::ConnectionAborted
                    | io::ErrorKind::UnexpectedEof
                    | io::ErrorKind::BrokenPipe
            ) {
                return LiveError::Unreachable;
            }
            // `io::Error::source()` skips the error it wraps, and hyper-rustls nests the
            // rustls error two `io::Error`s deep, so unwrap explicitly.
            if let Some(inner) = io.get_ref() {
                cause = Some(inner);
                continue;
            }
        }
        cause = current.source();
    }
    if err.is_connect() {
        LiveError::Unreachable
    } else {
        LiveError::Transport(err.to_string())
    }
}

/// Turns the full `eventdata` history returned by every poll into just the new events.
///
/// The cursor always moves to the newest `EventID` the API reported, whether or not that
/// event matters to the tracker. Outplayed only advanced its cursor on the local
/// player's objective events and re-scanned everything else on every update.
///
/// Reconnect safety: the cursor survives a game crash/reconnect. If the restarted game
/// numbers its history differently, which is detected because the event at the cursor's
/// ID no longer matches, new events are found by content instead of by ID. That way
/// nothing is emitted twice or skipped.
#[derive(Debug, Default, Clone)]
pub struct EventCursor {
    last_event_id: Option<u64>,
    /// Fingerprint of the event at `last_event_id`, used to check the numbering is unchanged.
    anchor: Option<u64>,
    seen: HashSet<u64>,
    renumbered: u32,
}

impl EventCursor {
    pub fn new() -> Self {
        Self::default()
    }

    /// Highest `EventID` seen so far.
    pub fn last_event_id(&self) -> Option<u64> {
        self.last_event_id
    }

    /// How many times the history came back renumbered (reconnects that reset IDs).
    pub fn renumber_count(&self) -> u32 {
        self.renumbered
    }

    /// Returns the events not seen before, in `EventID` order, and advances the cursor.
    pub fn take_new(&mut self, mut events: Vec<RawEvent>) -> Vec<RawEvent> {
        if events.is_empty() {
            return events;
        }
        events.sort_by_key(|e| e.event_id);
        let consistent = match (self.last_event_id, self.anchor) {
            (Some(last), Some(anchor)) => events
                .iter()
                .find(|e| e.event_id == last)
                .is_some_and(|e| fingerprint(e) == anchor),
            _ => true,
        };
        if !consistent {
            self.renumbered += 1;
            tracing::debug!(
                previous_last_id = ?self.last_event_id,
                "eventdata history renumbered (reconnect?); matching new events by content"
            );
        }

        let newest = events.last().map(|e| (e.event_id, fingerprint(e)));
        let fresh: Vec<RawEvent> = events
            .into_iter()
            .filter(|e| !consistent || self.last_event_id.is_none_or(|last| e.event_id > last))
            .filter(|e| self.seen.insert(fingerprint(e)))
            .collect();

        if let Some((id, print)) = newest {
            self.last_event_id = Some(id);
            self.anchor = Some(print);
        }
        fresh
    }
}

/// Identifies an event by content, independent of its `EventID`.
fn fingerprint(event: &RawEvent) -> u64 {
    let mut hasher = DefaultHasher::new();
    event.event_name.hash(&mut hasher);
    // Millisecond resolution: the same event always reports the same float.
    ((event.event_time * 1000.0).round() as i64).hash(&mut hasher);
    event.killer_name.hash(&mut hasher);
    event.victim_name.hash(&mut hasher);
    event.kill_streak.hash(&mut hasher);
    event.dragon_type.hash(&mut hasher);
    event.result.hash(&mut hasher);
    event.acer.hash(&mut hasher);
    event.recipient.hash(&mut hasher);
    event.turret_killed.hash(&mut hasher);
    event.inhib_killed.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(id: u64, name: &str, time: f64) -> RawEvent {
        RawEvent {
            event_id: id,
            event_name: name.into(),
            event_time: time,
            ..Default::default()
        }
    }

    fn kill(id: u64, time: f64, killer: &str, victim: &str) -> RawEvent {
        RawEvent {
            killer_name: Some(killer.into()),
            victim_name: Some(victim.into()),
            ..ev(id, "ChampionKill", time)
        }
    }

    fn ids(events: &[RawEvent]) -> Vec<u64> {
        events.iter().map(|e| e.event_id).collect()
    }

    #[test]
    fn returns_only_new_events_and_always_advances() {
        let mut cursor = EventCursor::new();
        let history = vec![ev(0, "GameStart", 0.0), ev(1, "MinionsSpawning", 65.0)];
        assert_eq!(ids(&cursor.take_new(history.clone())), [0, 1]);
        assert_eq!(cursor.last_event_id(), Some(1));

        // Same history again: nothing new, even though neither event is "interesting".
        assert!(cursor.take_new(history.clone()).is_empty());

        let mut longer = history;
        longer.push(kill(2, 100.0, "A", "B"));
        longer.push(ev(3, "FirstBlood", 100.0));
        assert_eq!(ids(&cursor.take_new(longer)), [2, 3]);
        assert_eq!(cursor.last_event_id(), Some(3));
    }

    #[test]
    fn tolerates_unsorted_input_and_empty_polls() {
        let mut cursor = EventCursor::new();
        assert!(cursor.take_new(vec![]).is_empty());
        let got = cursor.take_new(vec![ev(2, "C", 3.0), ev(0, "A", 1.0), ev(1, "B", 2.0)]);
        assert_eq!(ids(&got), [0, 1, 2]);
        assert!(cursor.take_new(vec![]).is_empty());
        assert_eq!(cursor.last_event_id(), Some(2));
    }

    #[test]
    fn reconnect_with_same_history_continues_after_cursor() {
        let mut cursor = EventCursor::new();
        let before: Vec<_> = (0..5)
            .map(|i| kill(i, 100.0 + i as f64, "A", "B"))
            .collect();
        cursor.take_new(before.clone());

        // The game process restarts: first a partial history while it loads...
        assert!(cursor.take_new(before[..2].to_vec()).is_empty());
        // ...then the full history plus what happened since.
        let mut after = before;
        after.push(kill(5, 200.0, "A", "C"));
        assert_eq!(ids(&cursor.take_new(after)), [5]);
        assert_eq!(
            cursor.renumber_count(),
            1,
            "the partial history looked renumbered"
        );
    }

    #[test]
    fn reconnect_with_renumbered_history_dedupes_by_content() {
        let mut cursor = EventCursor::new();
        let before = vec![
            ev(0, "GameStart", 0.0),
            kill(1, 100.0, "A", "B"),
            kill(2, 150.0, "A", "C"),
        ];
        cursor.take_new(before);

        // Restarted game dropped GameStart and numbers from 0 again, so old events have
        // new IDs and a new event gets an ID <= the old cursor.
        let after = vec![
            kill(0, 100.0, "A", "B"),
            kill(1, 150.0, "A", "C"),
            kill(2, 180.0, "B", "A"),
        ];
        let fresh = cursor.take_new(after.clone());
        assert_eq!(fresh.len(), 1);
        assert_eq!(fresh[0].event_time, 180.0);
        assert_eq!(cursor.last_event_id(), Some(2));

        // Back to normal ID-based dedupe afterwards.
        let mut more = after;
        more.push(kill(3, 190.0, "A", "D"));
        assert_eq!(ids(&cursor.take_new(more)), [3]);
    }
}
