//! `downplayed-dev record` against the fake Live Client API and League client: the
//! recording is a fixture that replays exactly like the match it recorded, and holds no
//! real name, Riot ID, puuid, game ID or League client password.

mod support;

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};

use downplayed_lib::league::client::{CredentialSource, LcuClient, LcuCredentials};
use downplayed_lib::league::live::LiveClient;
use downplayed_lib::recorder::{record, RecordError, Recorded, RecorderTimings};
use downplayed_lib::replay::{replay, replay_with, Fixture, LcuRecording};
use downplayed_lib::tracker::normalize::MarkerConfig;
use downplayed_lib::tracker::session::{ResultSource, SessionConfig};
use support::{eventually, fixture_path, FakeLcuServer, FakeLiveServer, TestPki};

const PASSWORD: &str = "Rec0rder-test-Pa55";
const PUUID: &str = "1e0e2c1a-5d8e-4bc4-9f0a-recorder-test-local-puuid-000000000000000000000000";
/// Shaped like the post-game chat token in the real end-of-game block.
const CHAT_JWT: &str = "eyJraWQiOiIxIn0.eyJzdWIiOiJyZWFsLWFjY291bnQifQ.c2lnbmF0dXJlLWJ5dGVz";

/// ranked_win with real-looking players instead of the synthetic names.
const RENAMES: [(&str, &str); 12] = [
    ("Downplayer", "Faker Fan"),
    ("AllyTop", "Top Laner Joe"),
    ("AllyMid", "Midnight Owl"),
    ("AllyBot", "Bottom Text"),
    ("AllySup", "Warding Wendy"),
    ("EnemyTop", "Grumpy Otter"),
    ("EnemyJgl", "Gank Plank"),
    ("EnemyMid", "Mid Or Feed"),
    ("EnemyBot", "Crit Happens"),
    ("EnemySup", "Hook Line"),
    ("#DP1", "#XQZ7"),
    ("#DP2", "#QWV3"),
];

fn real_fixture() -> Fixture {
    let mut text = std::fs::read_to_string(fixture_path("lol/ranked_win.json")).unwrap();
    for (synthetic, real) in RENAMES {
        text = text.replace(synthetic, real);
    }
    text = text
        .replace("\"DP1\"", "\"XQZ7\"")
        .replace("\"DP2\"", "\"QWV3\"");
    Fixture::parse(&text).unwrap()
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("downplayed-record-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            files.extend(files_under(&path));
        } else {
            files.push(path);
        }
    }
    files.sort();
    files
}

/// Runs the recorder while the fake match plays out: the game reaches its end, closes,
/// and the client shows its end-of-game screen.
async fn record_match(
    pki: &TestPki,
    live_server: &FakeLiveServer,
    lcu_server: &FakeLcuServer,
    dir: &Path,
    eog_wait: Duration,
) -> (Result<Recorded, RecordError>, Vec<String>) {
    let live = LiveClient::with_tls(live_server.url(), pki.client_config()).unwrap();
    let creds = LcuCredentials::new(lcu_server.port(), PASSWORD, CredentialSource::Lockfile);
    let lcu = LcuClient::with_tls(creds, pki.client_config()).unwrap();
    let timings = RecorderTimings {
        poll: Duration::from_millis(10),
        roster_every: 3,
        live_gone: Duration::from_millis(150),
        eog_wait,
        checkpoint: Duration::from_millis(50),
    };
    let out = dir.to_path_buf();
    let recorder = tokio::spawn(async move {
        let mut lines = Vec::new();
        let recorded = record(&live, Some(&lcu), &out, &timings, |line| lines.push(line)).await;
        (recorded, lines)
    });

    live_server.set_game_time(600.0);
    tokio::time::sleep(Duration::from_millis(100)).await;
    live_server.set_game_time(1810.0);
    let polls = |s: &FakeLiveServer| {
        s.with(|state| {
            state
                .requests
                .iter()
                .filter(|r| r.as_str() == "/liveclientdata/eventdata")
                .count()
        })
    };
    let seen = polls(live_server);
    assert!(eventually(Duration::from_secs(5), || polls(live_server) >= seen + 3).await);
    live_server.set_up(false);
    lcu_server.with(|s| s.phase = "EndOfGame".into());

    tokio::time::timeout(Duration::from_secs(10), recorder)
        .await
        .expect("recorder finished")
        .unwrap()
}

#[tokio::test]
async fn records_a_match_as_a_pseudonymised_fixture() {
    let pki = TestPki::new();
    let original = real_fixture();
    let live_server = FakeLiveServer::start(&pki, original.clone()).await;
    let lcu_server = FakeLcuServer::start(&pki, PASSWORD).await;
    lcu_server.with(|s| {
        s.session["gameData"]["teamOne"] = json!([
            {"championId": 102, "puuid": PUUID, "summonerId": 77, "summonerName": "", "gameName": "Faker Fan", "tagLine": "XQZ7"}
        ]);
        let eog = s.eog.as_mut().unwrap();
        let local = &mut eog["localPlayer"];
        local["puuid"] = PUUID.into();
        local["summonerId"] = 77.into();
        local["summonerName"] = "Faker Fan".into();
        local["riotIdGameName"] = "Faker Fan".into();
        local["riotIdTagLine"] = "XQZ7".into();
        eog["accountId"] = 4242.into();
        eog["mucJwtDto"] = json!({"channelClaim": "5123456789-eog", "jwt": CHAT_JWT});
        eog["multiUserChatId"] = "5123456789-eog".into();
        eog["multiUserChatPassword"] = CHAT_JWT.into();
    });
    let dir = temp_dir("match");
    let (recorded, lines) = record_match(
        &pki,
        &live_server,
        &lcu_server,
        &dir,
        Duration::from_secs(5),
    )
    .await;
    let recorded = recorded.unwrap();
    assert!(recorded.has_eog, "{lines:#?}");
    assert_eq!(recorded.events, original.events.len());
    assert!(lines.iter().any(|l| l.contains("HordeKill")), "{lines:#?}");

    let names: Vec<String> = files_under(&dir)
        .iter()
        .map(|p| {
            p.strip_prefix(&dir)
                .unwrap()
                .display()
                .to_string()
                .replace('\\', "/")
        })
        .collect();
    let stem = recorded.fixture.file_stem().unwrap().to_string_lossy();
    assert!(stem.starts_with("classic-"), "{stem}");
    assert_eq!(
        names,
        [
            format!("{stem}.json"),
            format!("lcu/{stem}.eog.json"),
            format!("lcu/{stem}.frames.json"),
            format!("lcu/{stem}.session.json"),
        ]
    );

    // Nothing identifying in any file.
    for path in files_under(&dir) {
        let text = std::fs::read_to_string(&path).unwrap();
        let lower = text.to_lowercase();
        for (_, real) in RENAMES {
            assert!(
                !lower.contains(&real.trim_start_matches('#').to_lowercase()),
                "{real} leaked into {}",
                path.display()
            );
        }
        for secret in [PASSWORD, PUUID, "5123456789", CHAT_JWT] {
            assert!(
                !text.contains(secret),
                "{secret} leaked into {}",
                path.display()
            );
        }
        for key in ["\"puuid\"", "\"summonerid\"", "\"accountid\""] {
            assert!(!lower.contains(key), "{key} kept in {}", path.display());
        }
    }

    // The recording replays exactly like the match, and the League client files give
    // the result from the end-of-game stats.
    let fixture = Fixture::load(&recorded.fixture).unwrap();
    assert_eq!(fixture.activeplayer["riotId"], "Downplayer#DP1");
    let config = SessionConfig {
        markers: MarkerConfig::default().with_extras(),
        ..SessionConfig::default()
    };
    let expected = replay(&original, &config).unwrap().summary;
    let lcu = LcuRecording::load_beside(&recorded.fixture).unwrap();
    let summary = replay_with(&fixture, &lcu, &config).unwrap().summary;
    assert_eq!(summary.markers, expected.markers);
    assert_eq!(summary.moments, expected.moments);
    assert_eq!(summary.result, expected.result);
    assert_eq!(summary.result_source, Some(ResultSource::EndOfGameStats));
    assert_eq!(summary.queue_id, Some(420));
    assert!(summary
        .markers
        .iter()
        .any(|m| m.kind.as_str() == "voidgrubs"));

    let frames: Vec<String> = serde_json::from_str(
        &std::fs::read_to_string(dir.join(format!("lcu/{stem}.frames.json"))).unwrap(),
    )
    .unwrap();
    let phases: Vec<Value> = frames
        .iter()
        .filter_map(|f| serde_json::from_str::<Value>(f).ok())
        .filter(|f| f[0] == 8)
        .map(|f| f[2]["data"].clone())
        .collect();
    assert_eq!(phases.first(), Some(&json!("Lobby")));
    assert!(phases.contains(&json!("EndOfGame")));
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn end_of_game_stats_of_another_game_are_ignored() {
    let pki = TestPki::new();
    let live_server = FakeLiveServer::start(&pki, real_fixture()).await;
    let lcu_server = FakeLcuServer::start(&pki, PASSWORD).await;
    // The session names this game; the client still serves the previous one's stats.
    lcu_server.with(|s| s.eog.as_mut().unwrap()["gameId"] = 5_000_000_001u64.into());
    let dir = temp_dir("stale");
    let (recorded, lines) = record_match(
        &pki,
        &live_server,
        &lcu_server,
        &dir,
        Duration::from_millis(500),
    )
    .await;
    let recorded = recorded.unwrap();
    assert!(!recorded.has_eog, "{lines:#?}");
    assert!(
        lines.iter().any(|l| l.contains("another game")),
        "{lines:#?}"
    );
    let stem = recorded.fixture.file_stem().unwrap().to_string_lossy();
    assert!(!dir.join(format!("lcu/{stem}.eog.json")).exists());
    std::fs::remove_dir_all(&dir).ok();
}
