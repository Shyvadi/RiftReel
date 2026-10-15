//! Fixture replays (`downplayed-dev replay`) produce the expected markers.
//!
//! The rendered report of every fixture is compared with `fixtures/lol/*.expected.txt`.
//! After an intended change, regenerate them with `UPDATE_EXPECTED=1 cargo test --test
//! fixture_replay` and review the diff. Like the command, replays use the League client
//! files recorded with a fixture (`lcu/<name>.*.json`) when there are any.

mod support;

use downplayed_lib::replay::{render_text, replay_with, LcuRecording, ReplayReport};
use downplayed_lib::tracker::normalize::{GameResult, Kda, MarkerConfig, MarkerKind, MarkerLabel};
use downplayed_lib::tracker::session::SessionConfig;
use support::{fixture_path, load_fixture};

fn config(extras: bool, team: bool) -> SessionConfig {
    let mut markers = if extras {
        MarkerConfig::default().with_extras()
    } else {
        MarkerConfig::default()
    };
    markers.include_team = team;
    SessionConfig {
        markers,
        ..SessionConfig::default()
    }
}

fn run(name: &str, extras: bool) -> ReplayReport {
    let lcu = LcuRecording::load_beside(fixture_path(&format!("lol/{name}.json"))).unwrap();
    replay_with(&load_fixture(name), &lcu, &config(extras, extras)).unwrap()
}

#[test]
fn rendered_replays_match_expected_output() {
    let cases = [
        ("ranked_win", false),
        ("ranked_win", true),
        ("aram_loss", false),
        ("penta_cap", false),
        ("spectator", false),
    ];
    let update = std::env::var_os("UPDATE_EXPECTED").is_some();
    let mut mismatches = Vec::new();
    for (name, extras) in cases {
        let rendered = render_text(&run(name, extras));
        let file = fixture_path(&format!(
            "lol/{name}{}.expected.txt",
            if extras { "_extras" } else { "" }
        ));
        if update {
            std::fs::write(&file, &rendered).unwrap();
            continue;
        }
        let expected = std::fs::read_to_string(&file)
            .unwrap_or_else(|e| {
                panic!(
                    "{}: {e} (run with UPDATE_EXPECTED=1 to create it)",
                    file.display()
                )
            })
            .replace("\r\n", "\n");
        if expected != rendered {
            mismatches.push(format!(
                "--- {}\n{expected}\n+++ actual\n{rendered}",
                file.display()
            ));
        }
    }
    assert!(
        mismatches.is_empty(),
        "replay output changed:\n{}",
        mismatches.join("\n")
    );
}

#[test]
fn ranked_win_markers_follow_the_spec() {
    let summary = run("ranked_win", false).summary;
    let marker = |label: MarkerLabel| summary.markers.iter().find(|m| m.label == label).unwrap();

    // Outplayed's default windows.
    assert_eq!(
        (
            marker(MarkerLabel::Kill).pre_s,
            marker(MarkerLabel::Kill).post_s
        ),
        (12.0, 8.0)
    );
    assert_eq!(
        (
            marker(MarkerLabel::Death).pre_s,
            marker(MarkerLabel::Death).post_s
        ),
        (12.0, 3.0)
    );
    assert_eq!(
        (
            marker(MarkerLabel::BaronKill).pre_s,
            marker(MarkerLabel::BaronKill).post_s
        ),
        (10.0, 5.0)
    );
    assert_eq!(
        (
            marker(MarkerLabel::Victory).pre_s,
            marker(MarkerLabel::Victory).post_s
        ),
        (10.0, 1.0)
    );

    // Exact Riot EventTime, multikill relabelling, steals, the win.
    assert_eq!(marker(MarkerLabel::Victory).game_time_s, 1802.6);
    assert_eq!(marker(MarkerLabel::DoubleKill).riot_event_ids, [14, 15]);
    assert!(marker(MarkerLabel::BaronKill).stolen);
    assert_eq!(summary.result, Some(GameResult::Win));
    assert_eq!(
        summary.kda,
        Some(Kda {
            kills: 4,
            deaths: 2,
            assists: 2
        })
    );

    // Defaults are Outplayed's highlight set: no Voidgrubs, first blood, ace, spree,
    // shutdown, and only my own objectives.
    assert!(summary.markers.iter().all(|m| !matches!(
        m.kind,
        MarkerKind::Voidgrubs
            | MarkerKind::FirstBlood
            | MarkerKind::Ace
            | MarkerKind::Shutdown
            | MarkerKind::Spree
    )));
    assert!(summary.markers.iter().all(|m| !m.by_team));

    // Moments: the double-kill fight merges three markers under the best label.
    let fight = summary
        .moments
        .iter()
        .find(|m| m.label == MarkerLabel::DoubleKill)
        .unwrap();
    assert_eq!(fight.marker_ids.len(), 3);
    assert_eq!(summary.moments.len(), 10);
}

#[test]
fn ranked_win_extras_add_first_blood_shutdown_and_team_plays() {
    let summary = run("ranked_win", true).summary;
    let labels: Vec<_> = summary.moments.iter().map(|m| m.label.as_str()).collect();
    assert_eq!(labels.first(), Some(&"first_blood"));
    assert!(
        labels.contains(&"shutdown"),
        "my kill ended a killing spree: {labels:?}"
    );
    assert!(
        labels.contains(&"rift_herald") && labels.contains(&"ace"),
        "team plays: {labels:?}"
    );
    assert!(labels.contains(&"voidgrubs"), "{labels:?}");
    let team_ace = summary
        .markers
        .iter()
        .find(|m| m.label == MarkerLabel::Ace)
        .unwrap();
    assert!(team_ace.by_team);
}

#[test]
fn penta_is_one_moment_and_long_fights_respect_the_cap() {
    let summary = run("penta_cap", false).summary;
    let penta = &summary.moments[0];
    assert_eq!(penta.label, MarkerLabel::PentaKill);
    assert_eq!(penta.marker_ids.len(), 5);
    assert_eq!((penta.start_s, penta.end_s), (988.0, 1044.0));
    let assist_moments: Vec<_> = summary
        .moments
        .iter()
        .filter(|m| m.label == MarkerLabel::Assist)
        .map(|m| (m.start_s, m.end_s))
        .collect();
    assert_eq!(
        assist_moments,
        [(1188.0, 1248.0), (1238.0, 1298.0), (1288.0, 1308.0)]
    );
    assert!(summary.moments.iter().all(|m| m.duration_s() <= 60.0));
}

#[test]
fn aram_loss_records_the_defeat_without_a_victory_marker() {
    let summary = run("aram_loss", false).summary;
    assert_eq!(summary.result, Some(GameResult::Lose));
    assert!(summary
        .markers
        .iter()
        .all(|m| m.kind != MarkerKind::Victory));
    assert!(summary
        .moments
        .iter()
        .any(|m| m.label == MarkerLabel::TripleKill));
    assert_eq!(
        summary.kda,
        Some(Kda {
            kills: 4,
            deaths: 3,
            assists: 1
        })
    );
}

#[test]
fn spectator_fixture_produces_nothing() {
    let report = run("spectator", false);
    assert!(report.spectator);
    assert!(report.summary.markers.is_empty());
}
