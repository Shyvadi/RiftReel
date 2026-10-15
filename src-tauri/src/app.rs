//! The tray application. It starts with no window and opens one on demand, which is
//! destroyed when closed. The tracker runs in the background the whole time.

use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter, RunEvent};
use tokio::sync::{mpsc, watch};

use crate::league::live::LiveClient;
use crate::logging;
use crate::settings::{AppPaths, LoadOutcome, Settings};
use crate::tracker::runtime::{watch_league, Timings, TrackerRuntime, TrackerStatus};
use crate::tray;

/// How often the process table is scanned for the League client and the game.
const PROCESS_SCAN_INTERVAL: Duration = Duration::from_secs(3);

/// Set to open the window at startup (development; normally only the tray opens it).
/// `hidden` creates it invisible, for smoke tests.
const OPEN_WINDOW_ENV: &str = "DOWNPLAYED_OPEN_WINDOW";

struct AppState {
    status: watch::Receiver<TrackerStatus>,
    paths: AppPaths,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AppStatus {
    version: &'static str,
    tracker: TrackerStatus,
    settings_file: String,
    log_dir: String,
}

#[tauri::command]
fn get_status(state: tauri::State<'_, AppState>) -> AppStatus {
    AppStatus {
        version: env!("CARGO_PKG_VERSION"),
        tracker: state.status.borrow().clone(),
        settings_file: state.paths.settings_file.display().to_string(),
        log_dir: state.paths.log_dir.display().to_string(),
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

    let (status_tx, status_rx) = watch::channel(TrackerStatus::default());
    let app = tauri::Builder::default()
        .manage(AppState {
            status: status_rx.clone(),
            paths,
        })
        .invoke_handler(tauri::generate_handler![get_status])
        .setup(move |app| {
            tray::create(app.handle())?;
            start_tracker(app.handle(), &settings, status_tx, status_rx);
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
    status_tx: watch::Sender<TrackerStatus>,
    mut status_rx: watch::Receiver<TrackerStatus>,
) {
    match LiveClient::new() {
        Ok(live) => {
            let (events_tx, events_rx) = mpsc::channel(64);
            let runtime = TrackerRuntime::new(
                settings.tracker_config(),
                live,
                Timings::default(),
                status_tx,
            );
            tauri::async_runtime::spawn(runtime.run(events_rx));
            tauri::async_runtime::spawn(watch_league(
                events_tx,
                settings.tracker.use_league_client,
                PROCESS_SCAN_INTERVAL,
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
