//! `league::live` against a fake Live Client Data API on 127.0.0.1 that replays the
//! fixtures over verified HTTPS, polled the way the tracker polls it.

mod support;

use std::collections::HashSet;

use downplayed_lib::league::live::{LiveClient, LiveError};
use downplayed_lib::league::tls::riot_client_config;
use downplayed_lib::replay::{replay, Fixture};
use downplayed_lib::tracker::machine::EndReason;
use downplayed_lib::tracker::normalize::{GameResult, Marker};
use downplayed_lib::tracker::session::{MatchSession, SessionConfig};
use support::{load_fixture, FakeLiveServer, TestPki};

/// Default markers for `ranked_win.json`: (label, EventTime, riot event IDs).
const RANKED_WIN_MARKERS: &[(&str, f64, &[u64])] = &[
    ("kill", 185.24, &[2]),
    ("death", 412.71, &[5]),
    ("dragon_kill", 540.33, &[6]),
    ("assist", 610.05, &[7]),
    ("turret_kill", 905.1, &[12]),
    ("kill", 1203.4, &[13]),
    ("double_kill", 1206.9, &[14, 15]),
    ("assist", 1210.0, &[16]),
    ("baron_kill", 1505.8, &[17]),
    ("kill", 1512.2, &[18]),
    ("inhibitor_kill", 1580.4, &[19]),
    ("death", 1700.1, &[27]),
    ("victory", 1802.6, &[29]),
];

fn summary_markers(markers: &[Marker]) -> Vec<(&'static str, f64, Vec<u64>)> {
    markers
        .iter()
        .map(|m| (m.label.as_str(), m.game_time_s, m.riot_event_ids.clone()))
        .collect()
}

fn expected_markers() -> Vec<(&'static str, f64, Vec<u64>)> {
    RANKED_WIN_MARKERS
        .iter()
        .map(|(l, t, ids)| (*l, *t, ids.to_vec()))
        .collect()
}

async fn start(fixture: &str) -> (TestPki, FakeLiveServer, LiveClient) {
    let pki = TestPki::new();
    let server = FakeLiveServer::start(&pki, load_fixture(fixture)).await;
    let live = LiveClient::with_tls(server.url(), pki.client_config()).unwrap();
    (pki, server, live)
}

/// What the runtime does when a match starts: game stats, "me", the roster.
async fn bootstrap(live: &LiveClient) -> MatchSession {
    let mut session = MatchSession::new(SessionConfig::default());
    session.set_game_stats(&live.game_stats().await.unwrap());
    session.set_identity(&live.active_player().await.unwrap());
    session.set_players(live.player_list().await.unwrap());
    session
}

/// Advances game time `step` seconds at a time, reading `eventdata` at each step.
async fn poll_through(
    server: &FakeLiveServer,
    live: &LiveClient,
    session: &mut MatchSession,
    from: f64,
    to: f64,
    step: f64,
) {
    let mut game_time = from;
    while game_time <= to {
        server.set_game_time(game_time);
        let data = live.event_data().await.expect("eventdata");
        session.ingest(data.events);
        game_time += step;
    }
}

fn assert_no_duplicates(markers: &[Marker]) {
    let mut seen = HashSet::new();
    for marker in markers {
        for id in &marker.riot_event_ids {
            assert!(seen.insert(*id), "Riot event {id} produced two markers");
        }
    }
}

#[tokio::test]
async fn reads_every_endpoint_over_verified_https() {
    let (_pki, server, live) = start("ranked_win").await;
    server.set_game_time(1300.0);

    let me = live.active_player().await.unwrap();
    assert_eq!(me.riot_id.as_deref(), Some("Downplayer#DP1"));
    assert_eq!(live.active_player_name().await.unwrap(), "Downplayer#DP1");
    assert_eq!(live.player_list().await.unwrap().len(), 10);
    let stats = live.game_stats().await.unwrap();
    assert_eq!(
        (stats.game_mode.as_str(), stats.game_time),
        ("CLASSIC", 1300.0)
    );
    let events = live.event_data().await.unwrap().events;
    assert_eq!(events.len(), 17, "history up to 1300 s");
    assert!(events.iter().all(|e| e.event_time <= 1300.0));
}

#[tokio::test]
async fn riot_root_rejects_a_server_it_did_not_sign() {
    let (_pki, server, _) = start("ranked_win").await;
    let pinned = LiveClient::with_tls(server.url(), riot_client_config().unwrap()).unwrap();
    let err = pinned.game_stats().await.unwrap_err();
    assert!(
        matches!(err, LiveError::Tls(_)),
        "expected a TLS rejection, got {err:?}"
    );
    assert!(
        !err.is_unavailable(),
        "a bad certificate must not look like 'game not running'"
    );
}

#[tokio::test]
async fn no_game_running_is_reported_as_unavailable() {
    let pki = TestPki::new();
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let live =
        LiveClient::with_tls(format!("https://127.0.0.1:{port}"), pki.client_config()).unwrap();
    let err = live.event_data().await.unwrap_err();
    assert!(err.is_unavailable(), "{err:?}");
}

#[tokio::test]
async fn spectators_get_an_error_from_activeplayer() {
    let (_pki, _server, live) = start("spectator").await;
    assert!(live.game_stats().await.is_ok());
    match live.active_player().await.unwrap_err() {
        LiveError::Status { status, code } => {
            assert_eq!(status, 404);
            assert_eq!(code.as_deref(), Some("RESOURCE_NOT_FOUND"));
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[tokio::test]
async fn polling_a_match_yields_the_fixture_markers() {
    let (_pki, server, live) = start("ranked_win").await;
    let mut session = bootstrap(&live).await;
    poll_through(&server, &live, &mut session, 0.0, 1810.0, 0.5).await;

    let summary = session.summary(EndReason::GameEnd);
    assert_eq!(summary_markers(&summary.markers), expected_markers());
    assert_eq!(summary.result, Some(GameResult::Win));
    assert_no_duplicates(&summary.markers);
    assert_eq!(session.cursor().last_event_id(), Some(29));

    // Same result as the offline replay of the fixture.
    let offline = replay(&load_fixture("ranked_win"), &SessionConfig::default()).unwrap();
    assert_eq!(
        summary_markers(&offline.summary.markers),
        expected_markers()
    );
    assert_eq!(offline.summary.moments, summary.moments);
}

#[tokio::test]
async fn reconnect_keeps_the_cursor_and_emits_nothing_twice() {
    let (_pki, server, live) = start("ranked_win").await;
    let mut session = bootstrap(&live).await;
    poll_through(&server, &live, &mut session, 0.0, 1000.0, 1.0).await;
    assert_eq!(session.cursor().last_event_id(), Some(12));
    let before = session.markers().len();

    // The game crashes. The client reconnects 100 s of game time later.
    server.set_up(false);
    for _ in 0..3 {
        let err = live.event_data().await.unwrap_err();
        assert!(err.is_unavailable(), "{err:?}");
    }
    server.set_game_time(1100.0);
    server.set_up(true);
    // While loading, the restarted game serves a partial history.
    server.with(|s| s.max_events = Some(4));
    session.ingest(live.event_data().await.unwrap().events);
    assert_eq!(
        session.markers().len(),
        before,
        "old events must not come back as new markers"
    );
    server.with(|s| s.max_events = None);

    poll_through(&server, &live, &mut session, 1100.0, 1810.0, 1.0).await;
    let summary = session.summary(EndReason::GameEnd);
    assert_eq!(summary_markers(&summary.markers), expected_markers());
    assert_no_duplicates(&summary.markers);
    assert_eq!(summary.result, Some(GameResult::Win));
}

#[tokio::test]
async fn reconnect_with_renumbered_history_still_finds_new_events() {
    let (_pki, server, live) = start("ranked_win").await;
    let mut session = bootstrap(&live).await;
    poll_through(&server, &live, &mut session, 0.0, 1000.0, 1.0).await;

    // The restarted game lost the first 400 s of history and numbers events from 0, so
    // the double kill gets an EventID at or below the old cursor.
    server.set_up(false);
    assert!(live.event_data().await.unwrap_err().is_unavailable());
    server.with(|s| {
        s.history_from = 400.0;
        s.renumber = true;
    });
    server.set_game_time(1100.0);
    server.set_up(true);
    poll_through(&server, &live, &mut session, 1100.0, 1810.0, 1.0).await;

    let summary = session.summary(EndReason::GameEnd);
    let labels: Vec<_> = summary
        .markers
        .iter()
        .map(|m| (m.label.as_str(), m.game_time_s))
        .collect();
    let expected: Vec<_> = RANKED_WIN_MARKERS
        .iter()
        .map(|(l, t, _)| (*l, *t))
        .collect();
    assert_eq!(labels, expected);
    assert!(session.cursor().renumber_count() >= 1);
    assert_eq!(summary.result, Some(GameResult::Win));
}

#[tokio::test]
async fn fixture_round_trips_through_the_fake_server() {
    let fixture: Fixture = load_fixture("aram_loss");
    let pki = TestPki::new();
    let server = FakeLiveServer::start(&pki, fixture.clone()).await;
    let live = LiveClient::with_tls(server.url(), pki.client_config()).unwrap();
    server.set_game_time(fixture.end_time());
    let served = live.event_data().await.unwrap().events;
    assert_eq!(served, fixture.events().unwrap());
}
