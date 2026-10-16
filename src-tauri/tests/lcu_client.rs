//! `league::client` against a fake League client: basic auth, REST endpoints and the
//! gameflow-phase WebSocket subscription. The recorded phase frames also drive the
//! tracker state machine.

mod support;

use std::time::Duration;

use riftreel_lib::league::client::{
    parse_phase_event, CredentialSource, LcuClient, LcuCredentials, LcuError,
};
use riftreel_lib::league::models::GameflowPhase as P;
use riftreel_lib::tracker::machine::{Effect, EndReason, Machine, Signal, TrackerState};
use support::{load_json, FakeLcuServer, TestPki};

const PASSWORD: &str = "Zx9-test-password_Q";

fn client(pki: &TestPki, server: &FakeLcuServer, password: &str) -> LcuClient {
    let creds = LcuCredentials::new(server.port(), password, CredentialSource::Lockfile);
    LcuClient::with_tls(creds, pki.client_config()).unwrap()
}

#[tokio::test]
async fn rest_endpoints_with_basic_auth() {
    let pki = TestPki::new();
    let server = FakeLcuServer::start(&pki, PASSWORD).await;
    let lcu = client(&pki, &server, PASSWORD);

    assert_eq!(lcu.get_gameflow_phase().await.unwrap(), P::InProgress);
    let session = lcu.get_gameflow_session().await.unwrap();
    assert_eq!(session.game_data.queue.id, 420);
    assert_eq!(session.game_data.queue.description, "Ranked Solo/Duo");
    assert_eq!(session.map.name, "Summoner's Rift");
    let eog = lcu.get_eog_stats_block().await.unwrap();
    assert_eq!(eog.win(), Some(true));
    assert_eq!(eog.kda(), Some((4, 2, 2)));

    server.with(|s| s.eog = None);
    assert!(matches!(
        lcu.get_eog_stats_block().await,
        Err(LcuError::Status { status: 404, .. })
    ));
    assert_eq!(server.with(|s| s.unauthorized), 0);
}

#[tokio::test]
async fn wrong_password_is_refused() {
    let pki = TestPki::new();
    let server = FakeLcuServer::start(&pki, PASSWORD).await;
    let lcu = client(&pki, &server, "not-the-password");
    assert!(matches!(
        lcu.get_gameflow_phase().await,
        Err(LcuError::Status { status: 401, .. })
    ));
    assert!(matches!(
        lcu.subscribe_gameflow_phase().await,
        Err(LcuError::WebSocket(_))
    ));
    assert_eq!(server.with(|s| s.unauthorized), 2);
}

#[tokio::test]
async fn websocket_subscription_streams_phase_changes() {
    let pki = TestPki::new();
    let server = FakeLcuServer::start(&pki, PASSWORD).await;
    let lcu = client(&pki, &server, PASSWORD);

    let mut phases = lcu.subscribe_gameflow_phase().await.unwrap();
    let mut seen = Vec::new();
    while let Some(phase) = tokio::time::timeout(Duration::from_secs(5), phases.next())
        .await
        .expect("phase stream stalled")
    {
        seen.push(phase);
    }
    assert_eq!(
        seen,
        [
            P::Lobby,
            P::Matchmaking,
            P::ReadyCheck,
            P::ChampSelect,
            P::GameStart,
            P::InProgress,
            P::Reconnect,
            P::InProgress,
            P::WaitingForStats,
            P::PreEndOfGame,
            P::EndOfGame,
            P::Lobby,
        ]
    );
    assert_eq!(
        server.with(|s| s.received.clone()),
        [r#"[5,"OnJsonApiEvent_lol-gameflow_v1_gameflow-phase"]"#]
    );
}

#[test]
fn recorded_phase_frames_drive_the_state_machine_through_a_reconnect() {
    let frames: Vec<String> =
        serde_json::from_value(load_json("lol/lcu/gameflow_phase_frames.json")).unwrap();
    let mut machine = Machine::new();
    machine.handle(Signal::LcuConnected);
    let mut trace = Vec::new();
    for phase in frames.iter().filter_map(|frame| parse_phase_event(frame)) {
        let effects = machine.handle(Signal::Phase(phase.clone()));
        trace.push((phase, machine.state(), effects));
    }
    use TrackerState::*;
    let expected = [
        (P::Lobby, Idle, vec![]),
        (P::Matchmaking, Idle, vec![]),
        (P::ReadyCheck, Idle, vec![]),
        (P::ChampSelect, Armed, vec![Effect::PrewarmCapture]),
        (P::GameStart, Armed, vec![]),
        (P::InProgress, InGame, vec![Effect::StartMatch]),
        (P::Reconnect, InGame, vec![]),
        (P::InProgress, InGame, vec![Effect::ResumeMatch]),
        (
            P::WaitingForStats,
            Finalizing,
            vec![Effect::Finalize(EndReason::ClientPhase)],
        ),
        (P::PreEndOfGame, Finalizing, vec![]),
        (P::EndOfGame, Finalizing, vec![]),
        (P::Lobby, Finalizing, vec![]),
    ];
    assert_eq!(trace, expected);
    assert!(machine.handle(Signal::Finalized).is_empty());
    assert_eq!(machine.state(), Idle);
}
