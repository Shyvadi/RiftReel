//! Developer commands (not part of the app).
//!
//! ```text
//! riftreel-dev replay <fixture.json> [--extras] [--team] [--json]
//!     Run a Live Client fixture through the tracker and print markers and moments.
//!     League client files recorded with it (`lcu/<name>.session.json`, `.eog.json`)
//!     are used too, so the result comes from the end-of-game stats when they exist.
//! riftreel-dev record [out-dir]
//!     Wait for the next game and record it as a fixture in <out-dir> (default: the
//!     `recordings` folder the RiftReel window lists): `<name>.json` plus
//!     `lcu/<name>.{session,eog,frames}.json` from the League client. Player names are
//!     pseudonymised (`Reeler#RR1` is you). Files are rewritten every few seconds;
//!     it stops by itself after the end-of-game screen.
//! riftreel-dev lcu
//!     Find the running League client, print its gameflow phase and queue, then
//!     follow phase changes over its WebSocket (Ctrl+C to stop).
//! riftreel-dev live [--extras] [--team]
//!     Poll the real Live Client API (start a Practice Tool game) and print markers
//!     as they happen.
//! riftreel-dev eog
//!     Read the League client's end-of-game stats for the last match and print what
//!     RiftReel takes from them (result, K/D/A, length). Compare with the EOG screen.
//! riftreel-dev probe [--force]
//!     Find ffmpeg, run the encoder probe the app uses (NVENC → AMF → QSV → CPU; the
//!     stored answer unless --force) and list the monitors capture can record.
//! riftreel-dev capture [seconds] [out-dir] [--stop-on-game]
//!     Record like the app does for a match (the monitor League's game window is on,
//!     else the primary one) for <seconds> (default 10), then print the file, frame
//!     counts and dropped/duplicated frames. <out-dir> defaults to `capture-test` in
//!     RiftReel's data folder. --stop-on-game ends it early if a League game starts.
//!     The sound comes from `[capture.audio]` in the settings.
//! riftreel-dev audio
//!     List the audio output and input devices (what `[capture.audio]` device names
//!     match against). Nothing is recorded.
//! riftreel-dev library [--cold]
//!     Time the library index (rebuilt from scratch with --cold) and a few list queries.
//! riftreel-dev cleanup
//!     The cleanup's dry run under the settings' `[storage]` limits: each full-match
//!     video it would send to the Recycle Bin, and the space freed. Changes nothing.
//! riftreel-dev outplayed <outplayed-folder> [--write]
//!     What importing an Outplayed library would add: its League videos, those joined to
//!     a record of its localStorage backup, the markers recovered, the results known.
//!     Reads only, unless --write: then it imports them, by reference, into the library.
//!     The Outplayed folder is never written to either way.
//! ```
//!
//! `--extras` enables the Voidgrubs/first blood/ace/shutdown/spree markers; `--team` also
//! marks your team's objectives. Set `RIFTREEL_LOG=debug` for (scrubbed) diagnostics
//! on stderr.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use riftreel_lib::capture::controller::CaptureTimings;
use riftreel_lib::capture::{
    display, ffmpeg, probe, wasapi, CaptureController, CaptureState, StartRequest,
};
use riftreel_lib::disk;
use riftreel_lib::league::client::{self, LcuClient};
use riftreel_lib::league::live::{LiveClient, EVENT_POLL_INTERVAL};
use riftreel_lib::library::{self, Library, MatchQuery, Sort};
use riftreel_lib::logging;
use riftreel_lib::recorder::{self, RecorderTimings};
use riftreel_lib::replay::{self, clock, render_text, Fixture, LcuRecording};
use riftreel_lib::settings::{AppPaths, Settings};
use riftreel_lib::tracker::machine::EndReason;
use riftreel_lib::tracker::normalize::{MarkerChange, MarkerConfig};
use riftreel_lib::tracker::session::{MatchSession, SessionConfig};

const USAGE: &str = "usage:
  riftreel-dev replay <fixture.json> [--extras] [--team] [--json]
  riftreel-dev record [out-dir]
  riftreel-dev lcu
  riftreel-dev live [--extras] [--team]
  riftreel-dev eog
  riftreel-dev probe [--force]
  riftreel-dev capture [seconds] [out-dir] [--stop-on-game]
  riftreel-dev audio
  riftreel-dev library [--cold]
  riftreel-dev cleanup
  riftreel-dev outplayed <outplayed-folder> [--write]";

#[derive(Default)]
struct Flags {
    extras: bool,
    team: bool,
    json: bool,
    force: bool,
    stop_on_game: bool,
    cold: bool,
    write: bool,
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
                "--force" => flags.force = true,
                "--stop-on-game" => flags.stop_on_game = true,
                "--cold" => flags.cold = true,
                "--write" => flags.write = true,
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
            "probe" => run_probe(&flags).await,
            "audio" => run_audio(),
            "capture" => run_capture(&flags).await,
            "library" => run_library(&flags),
            "cleanup" => run_cleanup(),
            "outplayed" => run_outplayed(&flags),
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
        "replay: cargo run --bin riftreel-dev -- replay {}",
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

/// The user's settings as the app reads them (defaults if there is no file).
/// Indexes the library (`RIFTREEL_HOME`, or the real one) and times it: the sync, then
/// a few list queries. `--cold` deletes the index first (it's a cache of the folders).
fn run_library(flags: &Flags) -> Result<(), String> {
    let (paths, settings) = app_paths();
    let index = paths
        .matches_dir
        .parent()
        .map(|root| root.join(library::INDEX_FILE))
        .ok_or("no library root")?;
    if flags.cold {
        let _ = std::fs::remove_file(&index);
    }
    let started = Instant::now();
    let library = Library::new(
        paths.matches_dir.clone(),
        paths.recordings_dir.clone(),
        settings.tracker_config().session,
    );
    let opened = started.elapsed();
    let report = library.sync();
    println!(
        "index {} ({:?}) opened in {:.1} ms",
        index.display(),
        library.index_opened(),
        opened.as_secs_f64() * 1000.0
    );
    println!(
        "sync: {} matches, {} read, {} removed, in {:.1} ms",
        report.total,
        report.read,
        report.removed,
        report.elapsed.as_secs_f64() * 1000.0
    );
    let again = library.sync();
    println!(
        "sync again (nothing changed): {:.1} ms",
        again.elapsed.as_secs_f64() * 1000.0
    );
    let queries = [
        ("first page", MatchQuery::default()),
        (
            "page 6",
            MatchQuery {
                offset: 500,
                ..MatchQuery::default()
            },
        ),
        (
            "wins with a multikill, most kills first",
            MatchQuery {
                result: Some("win".into()),
                has_multikill: true,
                sort: Sort::MostKills,
                ..MatchQuery::default()
            },
        ),
        (
            "has clips, oldest first",
            MatchQuery {
                offset: 1,
                has_clips: true,
                sort: Sort::Oldest,
                ..MatchQuery::default()
            },
        ),
    ];
    for (name, query) in queries {
        let started = Instant::now();
        let page = library.query(&query).map_err(|e| e.to_string())?;
        println!(
            "{name}: {} of {} in {:.2} ms",
            page.items.len(),
            page.total,
            started.elapsed().as_secs_f64() * 1000.0
        );
    }
    let started = Instant::now();
    let facets = library.facets().map_err(|e| e.to_string())?;
    println!(
        "filters: {} champions, {} queues in {:.2} ms",
        facets.champions.len(),
        facets.queues.len(),
        started.elapsed().as_secs_f64() * 1000.0
    );
    Ok(())
}

/// The cleanup's dry run: nothing is recycled or written.
fn run_cleanup() -> Result<(), String> {
    let (paths, settings) = app_paths();
    let storage = &settings.storage;
    let library = Library::new(
        paths.matches_dir.clone(),
        paths.recordings_dir.clone(),
        settings.tracker_config().session,
    );
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_default();
    let started = Instant::now();
    let overview = library.cleanup_overview(&storage.janitor_rules(), now_ms);
    let elapsed = started.elapsed();
    let plan = &overview.plan;
    println!("library {}", paths.matches_dir.display());
    println!(
        "cleanup is {}: keep {} days, at most {} GB of full matches (0: no limit)",
        if storage.cleanup { "on" } else { "off" },
        storage.keep_days,
        storage.full_matches_gb
    );
    println!(
        "full matches {} in {} ({} favorites); imported, left alone, {} in {}; clips {}{}",
        disk::gb(plan.managed_bytes),
        plan.managed_matches,
        plan.favorites,
        disk::gb(plan.unmanaged_bytes),
        plan.unmanaged_matches,
        disk::gb(plan.clip_bytes),
        if plan.clips_over_warning {
            " (over the warning level)"
        } else {
            ""
        }
    );
    println!(
        "dry run in {:.0} ms: {} videos to the Recycle Bin, {} freed, {} left",
        elapsed.as_secs_f64() * 1000.0,
        plan.removals.len(),
        disk::gb(plan.freed_bytes),
        disk::gb(plan.bytes_after)
    );
    // A match's id is its start (UTC), queue and champion.
    for removal in &plan.removals {
        println!(
            "  {:<48} {:>9}  {:?}",
            removal.id,
            disk::gb(removal.bytes),
            removal.reason
        );
    }
    if overview.thumbnails + overview.folders > 0 {
        println!(
            "also {} clip thumbnails without their clip and {} empty match folders",
            overview.thumbnails, overview.folders
        );
    }
    for kept in &overview.kept {
        println!(
            "  kept (couldn't recycle): {} {}  {}",
            kept.removal.id,
            disk::gb(kept.removal.bytes),
            kept.why
        );
    }
    Ok(())
}

/// The Outplayed import's dry run (or, with --write, the import).
fn run_outplayed(flags: &Flags) -> Result<(), String> {
    let root = PathBuf::from(
        flags
            .positional
            .first()
            .ok_or("outplayed needs the Outplayed folder, like D:\\Outplayed")?,
    );
    let (paths, settings) = app_paths();
    let library = Library::new(
        paths.matches_dir.clone(),
        paths.recordings_dir.clone(),
        settings.tracker_config().session,
    );
    let started = Instant::now();
    let survey = if flags.write {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or_default();
        library.import_outplayed(&root, now_ms)
    } else {
        library.outplayed_survey(&root)
    }
    .map_err(|e| format!("{}: {e}", root.display()))?;
    println!("Outplayed library {}", survey.root.display());
    println!(
        "League videos: {} ({}); imported before: {}; new: {} ({})",
        survey.videos,
        disk::gb(survey.bytes),
        survey.already,
        survey.new,
        disk::gb(survey.new_bytes)
    );
    match &survey.export {
        Some(export) => println!(
            "localStorage backup {}: {} League records",
            export.display(),
            survey.records
        ),
        None => println!("no localStorage backup (temp\\outplayed.json): names and lengths only"),
    }
    println!(
        "of the new: {} joined to a record, {} markers recovered, {} with a champion, {} won, {} with the result unknown",
        survey.joined, survey.markers, survey.champions, survey.wins, survey.unknown_results
    );
    println!(
        "lengths read from {} of {}; {} dated by the file (no time in the name)",
        survey.lengths, survey.new, survey.undated
    );
    println!(
        "{} in {:.1} s{}",
        if flags.write { "imported" } else { "dry run" },
        started.elapsed().as_secs_f64(),
        if flags.write {
            format!(" into {}", paths.matches_dir.display())
        } else {
            ": nothing written".into()
        }
    );
    Ok(())
}

fn load_settings(paths: &AppPaths) -> Settings {
    let mut settings = Settings::load(&paths.settings_file).unwrap_or_default();
    for warning in settings.validate() {
        eprintln!("settings: {warning}");
    }
    settings
}

/// The app's paths and settings, with the library where `[storage] library_dir` puts it.
fn app_paths() -> (AppPaths, Settings) {
    let paths = AppPaths::resolve();
    let settings = load_settings(&paths);
    let (paths, problem) = paths.with_library_dir(&settings.storage.library_dir);
    if let Some(problem) = problem {
        eprintln!("settings: {problem}");
    }
    (paths, settings)
}

async fn run_probe(flags: &Flags) -> Result<(), String> {
    let (paths, settings) = app_paths();
    let config = settings.capture_config(&paths);
    let ffmpeg = ffmpeg::locate(config.ffmpeg.as_deref())
        .ok_or("ffmpeg not found: run tools/fetch-ffmpeg.ps1 or set [capture] ffmpeg_path")?;
    println!("ffmpeg {}", ffmpeg.display());
    let result = probe::cached_or_probe(&ffmpeg, &config.probe_file, flags.force)
        .await
        .map_err(|e| e.to_string())?;
    println!("version {}", result.version.as_deref().unwrap_or("?"));
    println!("ddagrab {}", if result.ddagrab { "yes" } else { "missing" });
    match &result.encoder {
        Some(encoder) if result.is_software() => {
            println!("encoder {encoder} (on the CPU: no hardware encoder works here)")
        }
        Some(encoder) => println!("encoder {encoder}"),
        None => println!("encoder none: capture can't work with this ffmpeg"),
    }
    for rejected in &result.rejected {
        println!("  skipped {}: {}", rejected.encoder, rejected.reason);
    }
    println!(
        "stored in {}{}",
        config.probe_file.display(),
        if flags.force {
            ""
        } else {
            " (--force probes again)"
        }
    );
    for monitor in display::displays() {
        println!(
            "monitor {} {}x{} on {} (adapter {}, output {})",
            monitor.device_name,
            monitor.width,
            monitor.height,
            monitor.adapter_name,
            monitor.adapter,
            monitor.output
        );
    }
    match display::game_display() {
        Some(monitor) => println!("League's game window is on {}", monitor.device_name),
        None => println!("League's game window isn't open"),
    }
    Ok(())
}

async fn run_capture(flags: &Flags) -> Result<(), String> {
    let seconds: u64 = match flags.positional.first() {
        Some(text) => text
            .parse()
            .map_err(|_| format!("not a number of seconds: {text}"))?,
        None => 10,
    };
    let paths = AppPaths::resolve();
    let out = flags
        .positional
        .get(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| paths.recordings_dir.with_file_name("capture-test"));
    let mut config = load_settings(&paths).capture_config(&paths);
    config.enabled = true;
    config.matches_dir = out.clone();
    let mut capture = CaptureController::with_ffmpeg(config).with_timings(CaptureTimings {
        // No game window to wait for when testing on the desktop.
        window_wait: Duration::from_secs(2),
        ..CaptureTimings::default()
    });
    let started_wall_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or_default();
    let mut status = capture.status();
    capture.start(StartRequest {
        started_wall_ms,
        queue: Some("capture-test".into()),
    });
    println!("recording for {seconds} s into {}", out.display());
    let began = Instant::now();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(seconds);
    let mut game_check = tokio::time::interval(Duration::from_secs(2));
    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => break,
            changed = status.changed() => {
                if changed.is_err() {
                    break;
                }
                let now = status.borrow_and_update().clone();
                println!(
                    "[{:>7.1}s] {:?}{}{}",
                    began.elapsed().as_secs_f64(),
                    now.state,
                    now.encoder.map(|e| format!(" · {e}")).unwrap_or_default(),
                    now.error.map(|e| format!(" · {e}")).unwrap_or_default()
                );
                if now.state == CaptureState::Failed {
                    break;
                }
            }
            _ = game_check.tick(), if flags.stop_on_game => {
                let game = tokio::task::spawn_blocking(|| client::scan_processes().game_running)
                    .await
                    .unwrap_or(false);
                if game {
                    println!("a League game started; stopping");
                    break;
                }
            }
        }
    }
    let Some(recording) = capture.finish(true).await else {
        return Err("nothing was recorded".into());
    };
    let info = &recording.info;
    let file = info.folder.join(&info.file);
    let size = std::fs::metadata(&file)
        .map(|m| m.len())
        .unwrap_or_default();
    let recorded_s = info.end_wall_ms.saturating_sub(info.start_wall_ms) as f64 / 1000.0;
    println!("saved {} ({:.1} MB)", file.display(), size as f64 / 1e6);
    println!(
        "{}x{} at {} fps · {} · {} kbps",
        info.width, info.height, info.fps, info.encoder, info.bitrate_kbps
    );
    println!(
        "frames {} over {:.1} s ({:.2} fps) · dropped {} · duplicated {}",
        info.frames,
        recorded_s,
        info.frames as f64 / recorded_s.max(0.001),
        info.dropped_frames,
        info.duplicated_frames
    );
    if let Some(error) = &info.error {
        println!("ended early: {error}");
    }
    match &info.audio {
        Some(audio) => {
            println!(
                "audio tracks {} · started {:.1} ms before the video ({})",
                audio.tracks.join(", "),
                audio.lead_s * 1000.0,
                if audio.aligned {
                    "moved into sync"
                } else {
                    "not moved: the remux failed"
                }
            );
            for source in &audio.sources {
                let stats = &source.stats;
                println!(
                    "  {:?} {}: {} frames, {:.1} s of silence filled, {} dropped / {} repeated{}",
                    source.kind,
                    source.device,
                    stats.frames,
                    stats.silence_frames as f64 / 48_000.0,
                    stats.dropped_frames,
                    stats.repeated_frames,
                    source
                        .error
                        .as_deref()
                        .map(|e| format!(" · {e}"))
                        .unwrap_or_default()
                );
            }
        }
        None => println!("no audio"),
    }
    Ok(())
}

fn run_audio() -> Result<(), String> {
    let devices = wasapi::devices();
    if devices.is_empty() {
        return Err("no audio devices found".into());
    }
    for (input, heading) in [
        (false, "output devices (loopback)"),
        (true, "input devices"),
    ] {
        println!("{heading}:");
        for device in devices.iter().filter(|d| d.input == input) {
            println!(
                "  {}{}",
                device.name,
                if device.default { "  (default)" } else { "" }
            );
        }
    }
    Ok(())
}
