//! The tray application. It starts with no window and opens one on demand, which is
//! destroyed when closed. The tracker runs in the background the whole time and records
//! each match; each match it finishes is saved to the library and, by default, opened
//! for review once the game has closed.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, RunEvent};
use tokio::sync::{mpsc, watch};

use crate::capture::{ffmpeg, CaptureConfig, CaptureController};
use crate::league::client::scan_processes;
use crate::league::live::LiveClient;
use crate::library::{Library, MatchCard};
use crate::logging;
use crate::settings::{AppPaths, LoadOutcome, Settings};
use crate::tracker::runtime::{watch_league, Timings, TrackerRuntime, TrackerStatus};
use crate::tracker::session::MatchSummary;
use crate::tray;

/// How often the process table is scanned for the League client and the game.
const PROCESS_SCAN_INTERVAL: Duration = Duration::from_secs(3);

/// After a match, how often to check whether the game has closed before opening its
/// review, and for how long.
const REVIEW_WAIT_POLL: Duration = Duration::from_secs(2);
const REVIEW_WAIT_MAX: Duration = Duration::from_secs(15 * 60);

/// Set to open the window at startup (development; normally only the tray opens it).
/// `hidden` creates it invisible, for smoke tests.
const OPEN_WINDOW_ENV: &str = "DOWNPLAYED_OPEN_WINDOW";

struct AppState {
    status: watch::Receiver<TrackerStatus>,
    paths: AppPaths,
    library: Arc<Library>,
    /// A match the window should open on when it next asks (it was just created for it).
    review_request: Mutex<Option<String>>,
    /// The ffmpeg that cuts clips (the one capture uses).
    ffmpeg: Option<PathBuf>,
    /// Edits to a saved `match.json` (a clip saved, a favorite) go one at a time; a clip
    /// is numbered after the ones already there.
    match_edits: tokio::sync::Mutex<()>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AppStatus {
    version: &'static str,
    tracker: TrackerStatus,
    settings_file: String,
    log_dir: String,
    matches_dir: String,
    recordings_dir: String,
    /// Open this match's review (set when the window was opened for one).
    open_match: Option<String>,
}

#[tauri::command]
fn get_status(state: tauri::State<'_, AppState>) -> AppStatus {
    AppStatus {
        version: env!("CARGO_PKG_VERSION"),
        tracker: state.status.borrow().clone(),
        settings_file: state.paths.settings_file.display().to_string(),
        log_dir: state.paths.log_dir.display().to_string(),
        matches_dir: state.paths.matches_dir.display().to_string(),
        recordings_dir: state.paths.recordings_dir.display().to_string(),
        open_match: state
            .review_request
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take(),
    }
}

/// Saved matches and recordings, newest first.
#[tauri::command]
async fn list_matches(state: tauri::State<'_, AppState>) -> Result<Vec<MatchCard>, String> {
    let library = state.library.clone();
    tauri::async_runtime::spawn_blocking(move || library.list())
        .await
        .map_err(|e| e.to_string())
}

/// One match or recording for the review view.
#[tauri::command]
async fn get_match(
    id: String,
    state: tauri::State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    let library = state.library.clone();
    tauri::async_runtime::spawn_blocking(move || library.get(&id))
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "match not found".to_owned())
}

/// Cuts a clip out of a saved match's recording and lists it in its `match.json`: a
/// stream copy (a second or two, nothing re-encoded) of `fromS`–`toS` in video seconds,
/// starting on the keyframe at or before `fromS`. Returns the clip as `get_match` lists
/// clips.
#[tauri::command]
async fn save_clip(
    id: String,
    from_s: f64,
    to_s: f64,
    label: Option<String>,
    thumbnail_s: Option<f64>,
    state: tauri::State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    let binary = state
        .ffmpeg
        .clone()
        .ok_or("ffmpeg not found (run tools/fetch-ffmpeg.ps1, or set [capture] ffmpeg_path)")?;
    let _one_at_a_time = state.match_edits.lock().await;
    let library = state.library.clone();
    let match_id = id.clone();
    let label = label.unwrap_or_default();
    let job = tauri::async_runtime::spawn_blocking(move || {
        library.plan_clip(&match_id, (from_s, to_s), &label, thumbnail_s)
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())?;
    let thumbnail = ffmpeg::cut_clip(&binary, &job.video, &job.folder, &job.plan).await?;
    let clip = job.plan.clip(thumbnail);
    tracing::info!(
        file = %clip.file,
        seconds = (clip.to_s - clip.from_s).round() as u64,
        "clip saved"
    );
    let library = state.library.clone();
    tauri::async_runtime::spawn_blocking(move || library.add_clip(&id, &clip))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

/// Marks a clip of a saved match as a favorite, or not.
#[tauri::command]
async fn set_clip_favorite(
    id: String,
    file: String,
    favorite: bool,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    let _one_at_a_time = state.match_edits.lock().await;
    let library = state.library.clone();
    let changed = tauri::async_runtime::spawn_blocking(move || {
        library.set_clip_favorite(&id, &file, favorite)
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())?;
    changed
        .then_some(())
        .ok_or_else(|| "clip not found".to_owned())
}

/// Opens Explorer on a clip of a saved match, selected.
#[tauri::command]
async fn reveal_clip(
    id: String,
    file: String,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    let library = state.library.clone();
    let path = tauri::async_runtime::spawn_blocking(move || library.clip_file(&id, &file))
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "clip not found".to_owned())?;
    reveal_in_explorer(&path)
}

fn reveal_in_explorer(path: &std::path::Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // Explorer takes `/select,"<path>"` as written; a path can't contain a quote.
        std::process::Command::new("explorer.exe")
            .raw_arg(format!("/select,\"{}\"", path.display()))
            .spawn()
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        Err("showing files is only supported on Windows".into())
    }
}

pub fn run() {
    let paths = AppPaths::resolve();
    let loaded = Settings::load_or_create(&paths.settings_file);
    if let Err(e) = logging::init(
        Some(&paths.log_dir),
        &loaded.settings.logging.level,
        cfg!(debug_assertions),
    ) {
        eprintln!("Downplayed: file logging unavailable: {e}");
    }
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "Downplayed starting");
    match &loaded.outcome {
        LoadOutcome::Loaded => {}
        LoadOutcome::Created => {
            tracing::info!(path = %paths.settings_file.display(), "wrote default settings")
        }
        LoadOutcome::Invalid(error) => {
            tracing::warn!(%error, "settings file unusable; using defaults for this run")
        }
    }
    for warning in &loaded.warnings {
        tracing::warn!("{warning}");
    }
    let settings = loaded.settings;
    let library = Arc::new(Library::new(
        paths.matches_dir.clone(),
        paths.recordings_dir.clone(),
        settings.tracker_config().session,
    ));
    let capture = settings.capture_config(&paths);
    let matches_dir = paths.matches_dir.clone();

    let (status_tx, status_rx) = watch::channel(TrackerStatus::default());
    let app = tauri::Builder::default()
        .manage(AppState {
            status: status_rx.clone(),
            paths,
            library: library.clone(),
            review_request: Mutex::new(None),
            ffmpeg: ffmpeg::locate(capture.ffmpeg.as_deref()),
            match_edits: tokio::sync::Mutex::new(()),
        })
        .invoke_handler(tauri::generate_handler![
            get_status,
            list_matches,
            get_match,
            save_clip,
            set_clip_favorite,
            reveal_clip
        ])
        .setup(move |app| {
            // The review's <video> streams match.mp4 from here, in ranged reads; the
            // asset protocol serves nothing outside the matches folder.
            app.asset_protocol_scope()
                .allow_directory(&matches_dir, true)?;
            tray::create(app.handle())?;
            start_tracker(
                app.handle(),
                &settings,
                capture,
                library,
                status_tx,
                status_rx,
            );
            if let Some(value) = std::env::var_os(OPEN_WINDOW_ENV) {
                tray::create_main_window(app.handle(), value != "hidden");
            }
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("failed to build the Downplayed app");

    app.run(|_app, event| {
        // Closing the last window must not quit: Downplayed lives in the tray. Tray
        // "Quit" calls `exit(0)`, which carries an exit code and goes through.
        if let RunEvent::ExitRequested {
            code: None, api, ..
        } = event
        {
            api.prevent_exit();
        }
    });
}

fn start_tracker(
    app: &AppHandle,
    settings: &Settings,
    capture: CaptureConfig,
    library: Arc<Library>,
    status_tx: watch::Sender<TrackerStatus>,
    mut status_rx: watch::Receiver<TrackerStatus>,
) {
    match LiveClient::new() {
        Ok(live) => {
            let (events_tx, events_rx) = mpsc::channel(64);
            let (summary_tx, summary_rx) = mpsc::unbounded_channel();
            if !capture.enabled {
                tracing::info!("capture is off ([capture] enabled = false)");
            }
            let runtime = TrackerRuntime::new(
                settings.tracker_config(),
                live,
                Timings::default(),
                status_tx,
            )
            .with_summaries(summary_tx)
            .with_capture(CaptureController::with_ffmpeg(capture));
            tauri::async_runtime::spawn(runtime.run(events_rx));
            tauri::async_runtime::spawn(watch_league(
                events_tx,
                settings.tracker.use_league_client,
                PROCESS_SCAN_INTERVAL,
            ));
            tauri::async_runtime::spawn(save_matches(
                app.clone(),
                library,
                summary_rx,
                settings.ui.open_review_after_game,
            ));
        }
        Err(error) => tracing::error!(%error, "Live Client API setup failed; tracker disabled"),
    }

    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        while status_rx.changed().await.is_ok() {
            let status = status_rx.borrow_and_update().clone();
            tray::show_status(&app, &status);
            // Only reaches a window if one is open.
            let _ = app.emit("status", &status);
        }
    });
}

/// Saves each finished match to the library, then (if enabled) opens its review.
async fn save_matches(
    app: AppHandle,
    library: Arc<Library>,
    mut summaries: mpsc::UnboundedReceiver<MatchSummary>,
    open_review: bool,
) {
    while let Some(summary) = summaries.recv().await {
        let library = library.clone();
        let saved = tauri::async_runtime::spawn_blocking(move || library.save(&summary)).await;
        let id = match saved {
            Ok(Ok(id)) => id,
            Ok(Err(error)) => {
                tracing::warn!(%error, "could not save the match");
                continue;
            }
            Err(error) => {
                tracing::warn!(%error, "could not save the match");
                continue;
            }
        };
        tracing::info!(%id, "match saved");
        let _ = app.emit("matches-changed", &id);
        if open_review {
            tauri::async_runtime::spawn(open_review_after_game(app.clone(), id));
        }
    }
}

/// Opens the window on a match once `League of Legends.exe` is gone: never over a game.
/// Without the League client the tracker finishes on `GameEnd`, while the game still
/// shows its end screen.
async fn open_review_after_game(app: AppHandle, id: String) {
    let started = tokio::time::Instant::now();
    loop {
        let game_running = tauri::async_runtime::spawn_blocking(|| scan_processes().game_running)
            .await
            .unwrap_or(true);
        if !game_running {
            break;
        }
        if started.elapsed() >= REVIEW_WAIT_MAX {
            tracing::debug!("game still running; not opening the review");
            return;
        }
        tokio::time::sleep(REVIEW_WAIT_POLL).await;
    }
    // An open window switches on the event; a new one asks for its match on load.
    if !tray::show_match(&app, &id) {
        if let Some(state) = app.try_state::<AppState>() {
            *state
                .review_request
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = Some(id);
        }
        tray::create_main_window(&app, true);
    }
}
