//! Developer commands (not part of the app).
//!
//! ```text
//! downplayed-dev replay <fixture.json> [--extras] [--team] [--json]
//!     Run a Live Client fixture through the tracker and print markers and moments.
//!     League client files recorded with it (`lcu/<name>.session.json`, `.eog.json`)
//!     are used too, so the result comes from the end-of-game stats when they exist.
//! downplayed-dev record [out-dir]
//!     Wait for the next game and record it as a fixture in <out-dir> (default: the
//!     `recordings` folder the Downplayed window lists): `<name>.json` plus
//!     `lcu/<name>.{session,eog,frames}.json` from the League client. Player names are
//!     pseudonymised (`Downplayer#DP1` is you). Files are rewritten every few seconds;
//!     it stops by itself after the end-of-game screen.
//! downplayed-dev lcu
//!     Find the running League client, print its gameflow phase and queue, then
//!     follow phase changes over its WebSocket (Ctrl+C to stop).
//! downplayed-dev live [--extras] [--team]
//!     Poll the real Live Client API (start a Practice Tool game) and print markers
//!     as they happen.
//! downplayed-dev eog
//!     Read the League client's end-of-game stats for the last match and print what
//!     Downplayed takes from them (result, K/D/A, length). Compare with the EOG screen.
//! ```
//!
//! `--extras` enables the Voidgrubs/first blood/ace/shutdown/spree markers; `--team` also
//! marks your team's objectives. Set `DOWNPLAYED_LOG=debug` for (scrubbed) diagnostics
//! on stderr.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use downplayed_lib::league::client::{self, LcuClient};
use downplayed_lib::league::live::{LiveClient, EVENT_POLL_INTERVAL};
use downplayed_lib::logging;
use downplayed_lib::recorder::{self, RecorderTimings};
use downplayed_lib::replay::{self, clock, render_text, Fixture, LcuRecording};
use downplayed_lib::settings::AppPaths;
use downplayed_lib::tracker::machine::EndReason;
use downplayed_lib::tracker::normalize::{MarkerChange, MarkerConfig};
use downplayed_lib::tracker::session::{MatchSession, SessionConfig};

const USAGE: &str = "usage:
  downplayed-dev replay <fixture.json> [--extras] [--team] [--json]
  downplayed-dev record [out-dir]
  downplayed-dev lcu
  downplayed-dev live [--extras] [--team]
  downplayed-dev eog";

#[derive(Default)]
struct Flags {
    extras: bool,
    team: bool,
    json: bool,
    positional: Vec<String>,
}

impl Flags {
    fn parse(args: &[String]) -> Result<Self, String> {
        let mut flags = Self::default();
        for arg in args {
            match arg.as_str() {
                "--extras" => flags.extras = true,
                "--team" => flags.team = true,
                "--json" => flags.json = true,
                other if other.starts_with("--") => return Err(format!("unknown flag {other}")),
                other => flags.positional.push(other.to_owned()),
            }
        }
        Ok(flags)
    }

    fn session_config(&self) -> SessionConfig {
        let mut markers = if self.extras {
            MarkerConfig::default().with_extras()
        } else {
            MarkerConfig::default()
        };
        markers.include_team = self.team;
        SessionConfig {
            markers,
            ..SessionConfig::default()
        }
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let _ = logging::init(None, "warn", true);
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some((command, rest)) = args.split_first() else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    let result = match Flags::parse(rest) {
        Err(e) => Err(e),
        Ok(flags) => match command.as_str() {
            "replay" => run_replay(&flags),
            "record" => run_record(&flags).await,
            "lcu" => run_lcu().await,
            "live" => run_live(&flags).await,
            "eog" => run_eog().await,
            "help" | "-h" | "--help" => {
                println!("{USAGE}");
                Ok(())
            }
            other => Err(format!("unknown command {other:?}\n{USAGE}")),
        },
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run_replay(flags: &Flags) -> Result<(), String> {
    let path = flags
        .positional
        .first()
        .ok_or("replay needs a fixture path")?;
    let fixture = Fixture::load(path).map_err(|e| e.to_string())?;
    let lcu = LcuRecording::load_beside(path).map_err(|e| e.to_string())?;
    let report =
        replay::replay_with(&fixture, &lcu, &flags.session_config()).map_err(|e| e.to_string())?;
    if flags.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report.summary).map_err(|e| e.to_string())?
        );
    } else {
        print!("{}", render_text(&report));
    }
    Ok(())
}

async fn run_record(flags: &Flags) -> Result<(), String> {
    let out_dir = match flags.positional.first() {
        Some(dir) => PathBuf::from(dir),
        None => AppPaths::resolve().recordings_dir,
    };
    let live = LiveClient::new().map_err(|e| e.to_string())?;
    let lcu = match client::discover() {
        Ok(creds) => Some(LcuClient::new(creds).map_err(|e| e.to_string())?),
        Err(e) => {
            println!("no League client ({e}): recording the Live Client API only");
            None
        }
    };
    let start = Instant::now();
    let recorded = recorder::record(
        &live,
        lcu.as_ref(),
        &out_dir,
        &RecorderTimings::default(),
        |line| println!("[{:>6.1}s] {line}", start.elapsed().as_secs_f64()),
    )
    .await
    .map_err(|e| e.to_string())?;
    println!(
        "\nsaved {} events{}:",
        recorded.events,
        if recorded.has_eog {
            " and the end-of-game stats"
        } else {
            ""
        }
    );
    for file in &recorded.files {
        println!("  {}", file.display());
    }
    if !recorded.redacted.is_empty() {
        println!(
            "redacted where pseudonymisation missed identifiers (please report): {}",
            recorded.redacted.join("; ")
        );
    }
    println!(
        "replay: cargo run --bin downplayed-dev -- replay {}",
        recorded.fixture.display()
    );
    Ok(())
}

async fn run_eog() -> Result<(), String> {
    let lcu = LcuClient::new(client::discover().map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let eog = lcu.get_eog_stats_block().await.map_err(|e| e.to_string())?;
    let result = match eog.win() {
        Some(true) => "Win",
        Some(false) => "Lose",
        None => "unknown (no WIN/LOSE stat or team flag)",
    };
    let kda = eog
        .kda()
        .map_or("unknown (stat keys missing)".to_owned(), |(k, d, a)| {
            format!("{k}/{d}/{a}")
        });
    let champion = eog
        .local_player
        .as_ref()
        .and_then(|p| p.champion_name.clone());
    println!(
        "game {} · {} · {} · {} s{}",
        eog.game_id,
        if eog.game_mode.is_empty() {
            "-"
        } else {
            &eog.game_mode
        },
        if eog.queue_type.is_empty() {
            "-"
        } else {
            &eog.queue_type
        },
        eog.game_length,
        if eog.game_ended_in_surrender {
            " · surrender"
        } else {
            ""
        }
    );
    println!(
        "result {result} · K/D/A {kda} · champion {}",
        champion.as_deref().unwrap_or("?")
    );
    Ok(())
}

async fn run_lcu() -> Result<(), String> {
    let creds = client::discover().map_err(|e| e.to_string())?;
    println!(
        "League client on 127.0.0.1:{} (credentials from {:?})",
        creds.port, creds.source
    );
    let lcu = LcuClient::new(creds).map_err(|e| e.to_string())?;
    let phase = lcu.get_gameflow_phase().await.map_err(|e| e.to_string())?;
    println!("phase {phase}");
    match lcu.get_gameflow_session().await {
        Ok(session) => {
            let queue = &session.game_data.queue;
            println!(
                "queue {} ({}) · mode {} · map {} · custom {}",
                queue.id,
                if queue.description.is_empty() {
                    "-"
                } else {
                    &queue.description
                },
                if queue.game_mode.is_empty() {
                    "-"
                } else {
                    &queue.game_mode
                },
                if session.map.name.is_empty() {
                    "-"
                } else {
                    &session.map.name
                },
                session.game_data.is_custom_game
            );
        }
        Err(e) => println!("no gameflow session ({e})"),
    }
    let mut phases = lcu
        .subscribe_gameflow_phase()
        .await
        .map_err(|e| e.to_string())?;
    println!("following phase changes over the WebSocket (Ctrl+C to stop)");
    while let Some(phase) = phases.next().await {
        println!("phase {phase}");
    }
    println!("League client WebSocket closed");
    Ok(())
}

async fn run_live(flags: &Flags) -> Result<(), String> {
    let live = LiveClient::new().map_err(|e| e.to_string())?;
    let mut session = MatchSession::new(flags.session_config());
    println!("waiting for a game on {} (Ctrl+C to stop)", live.base_url());
    let mut ticker = tokio::time::interval(EVENT_POLL_INTERVAL);
    loop {
        ticker.tick().await;
        if !session.has_identity() {
            let Ok(stats) = live.game_stats().await else {
                continue;
            };
            session.set_game_stats(&stats);
            match live.active_player().await {
                Ok(player) => session.set_identity(&player),
                Err(e) => {
                    println!("no active player ({e}): spectator or replay, nothing to track");
                    return Ok(());
                }
            }
            if let Ok(players) = live.player_list().await {
                session.set_players(players);
            }
            println!(
                "in game: mode {} · champion {} · game time {}",
                session.game_mode().unwrap_or("?"),
                session.champion().unwrap_or("?"),
                clock(stats.game_time)
            );
        }
        let data = match live.event_data().await {
            Ok(data) => data,
            Err(e) if e.is_unavailable() => continue,
            Err(e) => {
                eprintln!("{e}");
                continue;
            }
        };
        let outcome = session.ingest(data.events);
        for change in outcome.changes {
            let (MarkerChange::Added(id) | MarkerChange::Relabeled(id)) = change;
            let Some(marker) = session.markers().iter().find(|m| m.id == id) else {
                continue;
            };
            let verb = if matches!(change, MarkerChange::Added(_)) {
                "marker"
            } else {
                "relabel"
            };
            println!(
                "{verb:>7} #{id:<3} {}  {:<15} {}{}",
                clock(marker.game_time_s),
                marker.kind.as_str(),
                marker.label.as_str(),
                if marker.stolen { " (stolen)" } else { "" }
            );
        }
        if outcome.game_ended {
            if let Ok(players) = live.player_list().await {
                session.set_final_players(players);
            }
            let summary = session.summary(EndReason::GameEnd);
            println!(
                "\ngame over: {:?} · {} moments",
                summary.result,
                summary.moments.len()
            );
            for moment in &summary.moments {
                println!(
                    "  {}–{}  {}",
                    clock(moment.start_s),
                    clock(moment.end_s),
                    moment.label.as_str()
                );
            }
            return Ok(());
        }
    }
}
