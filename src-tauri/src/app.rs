//! The tray application. It starts with no window and opens one on demand, which is
//! destroyed when closed. The tracker runs in the background the whole time and records
//! each match; each match it finishes is saved to the library and, by default, opened
//! for review once the game has closed. The cleanup, when the user has turned it on,
//! runs after each match and every hour, never during a game.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, RunEvent};
use tokio::sync::{mpsc, watch, Notify};

use crate::capture::{ffmpeg, CaptureConfig, CaptureController};
use crate::league::client::scan_processes;
use crate::league::live::LiveClient;
use crate::library::janitor::{Overview, RunReport};
use crate::library::{Facets, Library, MatchPage, MatchQuery, Removal};
use crate::logging;
use crate::settings::{AppPaths, LoadOutcome, Settings, SettingsError, StorageSettings};
use crate::shell;
use crate::thumbnails::Thumbnails;
use crate::tracker::runtime::{watch_league, Timings, TrackerRuntime, TrackerStatus};
use crate::tracker::session::MatchSummary;
use crate::tray;

/// Most thumbnails one request may queue (a screenful is a dozen).
const THUMBNAILS_PER_REQUEST: usize = 100;

/// How often the process table is scanned for the League client and the game.
const PROCESS_SCAN_INTERVAL: Duration = Duration::from_secs(3);

/// After a match, how often to check whether the game has closed before opening its
/// review, and for how long.
const REVIEW_WAIT_POLL: Duration = Duration::from_secs(2);
const REVIEW_WAIT_MAX: Duration = Duration::from_secs(15 * 60);
/// How long a message waits for the game to close before it is dropped.
const NOTICE_WAIT_MAX: Duration = Duration::from_secs(6 * 60 * 60);

/// The cleanup's first look after startup, then how often it looks again.
const CLEANUP_FIRST: Duration = Duration::from_secs(5 * 60);
const CLEANUP_EVERY: Duration = Duration::from_secs(60 * 60);
/// While the cleanup runs, how often it checks that no game has started.
const CLEANUP_GAME_CHECK: Duration = Duration::from_secs(2);

/// Set to open the window at startup (development; normally only the tray opens it).
/// `hidden` creates it invisible, for smoke tests.
const OPEN_WINDOW_ENV: &str = "DOWNPLAYED_OPEN_WINDOW";

struct AppState {
    status: watch::Receiver<TrackerStatus>,
    paths: AppPaths,
    library: Arc<Library>,
    /// A match the window should open on when it next asks (it was just created for it).
    review_request: Mutex<Option<String>>,
    /// A message for the window (low disk), until it has been shown once.
    notice: Mutex<Option<String>>,
    /// The ffmpeg that cuts clips (the one capture uses).
    ffmpeg: Option<PathBuf>,
    /// Edits to a saved `match.json` (a clip saved, a favorite, a cleanup run) go one at
    /// a time; a clip is numbered after the ones already there.
    match_edits: tokio::sync::Mutex<()>,
    /// `[storage]` as it is now: the window can change the cleanup's part.
    storage: Mutex<StorageSettings>,
    /// The settings file loaded, or was created. An unusable one is never rewritten.
    settings_writable: bool,
    /// Wakes the cleanup: a match was saved, or the user turned it on.
    cleanup_wake: Notify,
}

impl AppState {
    fn storage(&self) -> StorageSettings {
        self.storage
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
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
    /// Something to tell the user (once).
    notice: Option<String>,
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
        notice: state
            .notice
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take(),
    }
}

/// A page of saved matches and recordings, filtered and sorted (newest first by default).
#[tauri::command]
async fn list_matches(
    query: Option<MatchQuery>,
    state: tauri::State<'_, AppState>,
) -> Result<MatchPage, String> {
    let library = state.library.clone();
    let query = query.unwrap_or_default();
    tauri::async_runtime::spawn_blocking(move || library.query(&query))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

/// The champions and queues the list's filters offer.
#[tauri::command]
async fn match_filters(state: tauri::State<'_, AppState>) -> Result<Facets, String> {
    let library = state.library.clone();
    tauri::async_runtime::spawn_blocking(move || library.facets())
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

/// Queues list thumbnails for matches shown without one (they arrive as
/// `match-thumbnail` events).
#[tauri::command]
fn request_thumbnails(ids: Vec<String>, thumbnails: tauri::State<'_, Thumbnails>) {
    for id in ids.into_iter().take(THUMBNAILS_PER_REQUEST) {
        thumbnails.request(id);
    }
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
    app: AppHandle,
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
    let match_id = id.clone();
    let shown = tauri::async_runtime::spawn_blocking(move || library.add_clip(&match_id, &clip))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
    let _ = app.emit("matches-changed", &id);
    Ok(shown)
}

/// Marks a clip of a saved match as a favorite, or not.
#[tauri::command]
async fn set_clip_favorite(
    id: String,
    file: String,
    favorite: bool,
    app: AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    let _one_at_a_time = state.match_edits.lock().await;
    let library = state.library.clone();
    let match_id = id.clone();
    let changed = tauri::async_runtime::spawn_blocking(move || {
        library.set_clip_favorite(&match_id, &file, favorite)
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())?;
    if !changed {
        return Err("clip not found".to_owned());
    }
    let _ = app.emit("matches-changed", &id);
    Ok(())
}

/// A saved match's listed clip file.
async fn clip_path(state: &AppState, id: String, file: String) -> Result<PathBuf, String> {
    let library = state.library.clone();
    tauri::async_runtime::spawn_blocking(move || library.clip_file(&id, &file))
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "clip not found".to_owned())
}

/// The window's handle, to own the shell's dialogs.
fn window_handle(app: &AppHandle) -> Option<isize> {
    #[cfg(windows)]
    {
        let window = app.get_webview_window(tray::MAIN_WINDOW)?;
        window.hwnd().ok().map(|hwnd| hwnd.0 as isize)
    }
    #[cfg(not(windows))]
    {
        let _ = app;
        None
    }
}

/// Opens Explorer on a clip of a saved match, selected.
#[tauri::command]
async fn reveal_clip(
    id: String,
    file: String,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    let path = clip_path(&state, id, file).await?;
    shell::reveal(&path)
}

/// Sends a clip of a saved match (and its thumbnail) to the Recycle Bin and takes it off
/// the match. Returns `false` when the recycling was cancelled (nothing changed).
#[tauri::command]
async fn delete_clip(
    id: String,
    file: String,
    app: AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<bool, String> {
    let _one_at_a_time = state.match_edits.lock().await;
    let owner = window_handle(&app);
    let library = state.library.clone();
    let match_id = id.clone();
    let removal = tauri::async_runtime::spawn_blocking(move || {
        library.remove_clip(&match_id, &file, |files| shell::recycle(files, owner))
    })
    .await
    .map_err(|e| e.to_string())??;
    let removed = removal == Removal::Removed;
    if removed {
        tracing::info!(%id, "clip sent to the Recycle Bin");
        let _ = app.emit("matches-changed", &id);
    }
    Ok(removed)
}

/// Puts a clip of a saved match on the clipboard as a file, to paste anywhere.
#[tauri::command]
async fn copy_clip(
    id: String,
    file: String,
    app: AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    let path = clip_path(&state, id, file).await?;
    let owner = window_handle(&app);
    tauri::async_runtime::spawn_blocking(move || shell::copy_files(&[path], owner))
        .await
        .map_err(|e| e.to_string())?
}

/// Saves a copy of a clip of a saved match where the user picks ("Save as"). Returns
/// where it went, or nothing when cancelled.
#[tauri::command]
async fn export_clip(
    id: String,
    file: String,
    app: AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<Option<String>, String> {
    let path = clip_path(&state, id.clone(), file).await?;
    let name = format!(
        "{id}_{}",
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("clip.mp4")
    );
    let owner = window_handle(&app);
    let chosen = tauri::async_runtime::spawn_blocking(move || shell::save_as(owner, &name))
        .await
        .map_err(|e| e.to_string())??;
    let Some(target) = chosen else {
        return Ok(None);
    };
    if target == path {
        return Ok(Some(target.display().to_string()));
    }
    tokio::fs::copy(&path, &target)
        .await
        .map_err(|e| format!("could not save {}: {e}", target.display()))?;
    tracing::info!("clip saved as a copy");
    Ok(Some(target.display().to_string()))
}

/// Marks a saved match as a favorite, or not: the cleanup never removes a favorite's
/// video.
#[tauri::command]
async fn set_match_favorite(
    id: String,
    favorite: bool,
    app: AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    let _one_at_a_time = state.match_edits.lock().await;
    let library = state.library.clone();
    let match_id = id.clone();
    tauri::async_runtime::spawn_blocking(move || library.set_match_favorite(&match_id, favorite))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
    let _ = app.emit("matches-changed", &id);
    let _ = app.emit("storage-changed", ());
    Ok(())
}

/// Opens Explorer on a saved match's video, selected.
#[tauri::command]
async fn reveal_video(id: String, state: tauri::State<'_, AppState>) -> Result<(), String> {
    let library = state.library.clone();
    let path = tauri::async_runtime::spawn_blocking(move || library.video_file(&id))
        .await
        .map_err(|e| e.to_string())?
        .ok_or("this match has no video")?;
    shell::reveal(&path)
}

/// The cleanup's settings, as the window edits them.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CleanupSettings {
    cleanup: bool,
    keep_days: u32,
    full_matches_gb: f64,
    clips_warn_gb: f64,
}

impl CleanupSettings {
    fn of(storage: &StorageSettings) -> Self {
        Self {
            cleanup: storage.cleanup,
            keep_days: storage.keep_days,
            full_matches_gb: storage.full_matches_gb,
            clips_warn_gb: storage.clips_warn_gb,
        }
    }

    fn apply(self, storage: &mut StorageSettings) {
        storage.cleanup = self.cleanup;
        storage.keep_days = self.keep_days;
        storage.full_matches_gb = self.full_matches_gb;
        storage.clips_warn_gb = self.clips_warn_gb;
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StorageOverview {
    /// As saved.
    settings: CleanupSettings,
    /// The window may change them: the settings file is usable.
    settings_writable: bool,
    /// Free space on the library's drive.
    free_bytes: Option<u64>,
    game_running: bool,
    #[serde(flatten)]
    cleanup: Overview,
}

/// The storage view: what the library takes, the cleanup's settings, what it left for
/// the user, and its dry run under `draft` (settings being edited; else the saved ones).
/// Changes nothing.
#[tauri::command]
async fn storage_overview(
    draft: Option<CleanupSettings>,
    state: tauri::State<'_, AppState>,
) -> Result<StorageOverview, String> {
    let saved = state.storage();
    let mut storage = saved.clone();
    if let Some(draft) = draft {
        draft.apply(&mut storage);
        storage.validate();
    }
    let rules = storage.janitor_rules();
    let library = state.library.clone();
    let matches_dir = state.paths.matches_dir.clone();
    let (cleanup, free_bytes, game_running) = tauri::async_runtime::spawn_blocking(move || {
        (
            library.cleanup_overview(&rules, now_ms()),
            crate::disk::free_space(&matches_dir),
            scan_processes().game_running,
        )
    })
    .await
    .map_err(|e| e.to_string())?;
    Ok(StorageOverview {
        settings: CleanupSettings::of(&saved),
        settings_writable: state.settings_writable,
        free_bytes,
        game_running,
        cleanup,
    })
}

/// Saves the cleanup's settings in `settings.toml`, the rest of the file as it is now.
/// Turning the cleanup on runs it as soon as no game is running.
#[tauri::command]
async fn set_cleanup(
    settings: CleanupSettings,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    if !state.settings_writable {
        return Err(format!(
            "{} couldn't be read when Downplayed started; fix it (or remove it for the defaults), then restart Downplayed.",
            state.paths.settings_file.display()
        ));
    }
    let before = state.storage();
    let mut storage = before.clone();
    settings.apply(&mut storage);
    let problems = storage.validate();
    if !problems.is_empty() {
        return Err(problems.join("; "));
    }
    let path = state.paths.settings_file.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let mut file = match Settings::load(&path) {
            Ok(file) => file,
            Err(SettingsError::Io { source, .. })
                if source.kind() == std::io::ErrorKind::NotFound =>
            {
                Settings::default()
            }
            Err(e) => return Err(format!("{e}; fix it, then try again")),
        };
        settings.apply(&mut file.storage);
        file.save(&path)
            .map_err(|e| format!("could not save {}: {e}", path.display()))
    })
    .await
    .map_err(|e| e.to_string())??;
    *state.storage.lock().unwrap_or_else(PoisonError::into_inner) = storage;
    tracing::info!(
        cleanup = settings.cleanup,
        keep_days = settings.keep_days,
        full_matches_gb = settings.full_matches_gb,
        clips_warn_gb = settings.clips_warn_gb,
        "cleanup settings saved"
    );
    if settings.cleanup && !before.cleanup {
        state.cleanup_wake.notify_one();
    }
    Ok(())
}

/// Runs the cleanup now: it must be on, and League not running.
#[tauri::command]
async fn run_cleanup(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<RunReport, String> {
    if !state.storage().cleanup {
        return Err("The cleanup is off.".into());
    }
    if game_running().await {
        return Err("Not while League is running.".into());
    }
    clean_up(&app).await
}

/// Lets the cleanup try a video it couldn't recycle again, on its next run.
#[tauri::command]
async fn retry_cleanup(
    id: String,
    app: AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    let library = state.library.clone();
    tauri::async_runtime::spawn_blocking(move || library.retry_cleanup(&id))
        .await
        .map_err(|e| e.to_string())?;
    let _ = app.emit("storage-changed", ());
    Ok(())
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
    let settings_writable = !matches!(loaded.outcome, LoadOutcome::Invalid(_));
    let settings = loaded.settings;
    if settings.storage.cleanup {
        tracing::info!(
            keep_days = settings.storage.keep_days,
            full_matches_gb = settings.storage.full_matches_gb,
            "cleanup is on"
        );
    }
    let library = Arc::new(Library::new(
        paths.matches_dir.clone(),
        paths.recordings_dir.clone(),
        settings.tracker_config().session,
    ));
    let capture = settings.capture_config(&paths);
    let matches_dir = paths.matches_dir.clone();
    let ffmpeg = ffmpeg::locate(capture.ffmpeg.as_deref());

    let (status_tx, status_rx) = watch::channel(TrackerStatus::default());
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .manage(AppState {
            status: status_rx.clone(),
            paths,
            library: library.clone(),
            review_request: Mutex::new(None),
            notice: Mutex::new(None),
            ffmpeg: ffmpeg.clone(),
            match_edits: tokio::sync::Mutex::new(()),
            storage: Mutex::new(settings.storage.clone()),
            settings_writable,
            cleanup_wake: Notify::new(),
        })
        .invoke_handler(tauri::generate_handler![
            get_status,
            list_matches,
            match_filters,
            request_thumbnails,
            get_match,
            save_clip,
            set_clip_favorite,
            reveal_clip,
            delete_clip,
            copy_clip,
            export_clip,
            set_match_favorite,
            reveal_video,
            storage_overview,
            set_cleanup,
            run_cleanup,
            retry_cleanup
        ])
        .setup(move |app| {
            // The review's <video> streams match.mp4 from here, in ranged reads; the
            // asset protocol serves nothing outside the matches folder.
            app.asset_protocol_scope()
                .allow_directory(&matches_dir, true)?;
            tray::create(app.handle())?;
            let thumbnails = Thumbnails::start(app.handle().clone(), library.clone(), ffmpeg);
            app.manage(thumbnails.clone());
            // Matches changed while Downplayed wasn't running (or a new index) are read
            // now, so the window's first page doesn't wait for them.
            let indexed = library.clone();
            tauri::async_runtime::spawn_blocking(move || {
                let report = indexed.sync();
                tracing::info!(
                    matches = report.total,
                    read = report.read,
                    removed = report.removed,
                    ms = report.elapsed.as_millis() as u64,
                    index = ?indexed.index_opened(),
                    path = ?indexed.index_path(),
                    "library index ready"
                );
            });
            start_tracker(
                app.handle(),
                &settings,
                capture,
                library,
                thumbnails,
                status_tx,
                status_rx,
            );
            tauri::async_runtime::spawn(janitor(app.handle().clone()));
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
    thumbnails: Thumbnails,
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
                thumbnails,
                summary_rx,
                settings.ui.open_review_after_game,
            ));
        }
        Err(error) => tracing::error!(%error, "Live Client API setup failed; tracker disabled"),
    }

    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let mut told: Option<String> = None;
        while status_rx.changed().await.is_ok() {
            let status = status_rx.borrow_and_update().clone();
            tray::show_status(&app, &status);
            // Only reaches a window if one is open.
            let _ = app.emit("status", &status);
            // The drive ran short of space this match: say so once, after the game.
            let low_disk = status.capture.as_ref().and_then(|c| c.low_disk.clone());
            if low_disk != told {
                if let Some(message) = low_disk.clone() {
                    tauri::async_runtime::spawn(tell_after_game(app.clone(), message));
                }
                told = low_disk;
            }
        }
    });
}

/// Whether `League of Legends.exe` is running (a game is being played).
async fn game_running() -> bool {
    tauri::async_runtime::spawn_blocking(|| scan_processes().game_running)
        .await
        .unwrap_or(true)
}

/// Waits until `League of Legends.exe` isn't running, checking every few seconds. Returns
/// `false` if it still is after `max`.
async fn game_closed(max: Duration) -> bool {
    let started = tokio::time::Instant::now();
    loop {
        if !game_running().await {
            return true;
        }
        if started.elapsed() >= max {
            return false;
        }
        tokio::time::sleep(REVIEW_WAIT_POLL).await;
    }
}

/// Shows a message in the window: now if one is open, else when it next opens.
/// Returns whether one is open.
fn tell_window(app: &AppHandle, message: &str) -> bool {
    if let Some(state) = app.try_state::<AppState>() {
        // A window still loading (or opened later) asks for it; one already up gets
        // the event.
        *state.notice.lock().unwrap_or_else(PoisonError::into_inner) = Some(message.to_owned());
    }
    let open = app.get_webview_window(tray::MAIN_WINDOW).is_some();
    if open {
        let _ = app.emit("notice", message);
    }
    open
}

/// Tells the user something once the game has closed (never during it): in the window
/// if one is open, else as a Windows notification.
async fn tell_after_game(app: AppHandle, message: String) {
    if !game_closed(NOTICE_WAIT_MAX).await {
        return;
    }
    if app.get_webview_window(tray::MAIN_WINDOW).is_some() {
        tell_window(&app, &message);
        return;
    }
    use tauri_plugin_notification::NotificationExt;
    let shown = app
        .notification()
        .builder()
        .title("Downplayed")
        .body(&message)
        .show();
    if let Err(error) = shown {
        tracing::warn!(%error, "could not show a notification");
    }
}

/// Saves each finished match to the library, queues its list thumbnail, then (if
/// enabled) opens its review.
async fn save_matches(
    app: AppHandle,
    library: Arc<Library>,
    thumbnails: Thumbnails,
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
        thumbnails.request(id.clone());
        let _ = app.emit("matches-changed", &id);
        if let Some(state) = app.try_state::<AppState>() {
            state.cleanup_wake.notify_one();
        }
        if open_review {
            tauri::async_runtime::spawn(open_review_after_game(app.clone(), id));
        }
    }
}

/// Opens the window on a match once `League of Legends.exe` is gone: never over a game.
/// Without the League client the tracker finishes on `GameEnd`, while the game still
/// shows its end screen.
async fn open_review_after_game(app: AppHandle, id: String) {
    if !game_closed(REVIEW_WAIT_MAX).await {
        tracing::debug!("game still running; not opening the review");
        return;
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

/// The cleanup, while it is on: after each match once its game has closed, and every
/// hour, never while a game is running.
async fn janitor(app: AppHandle) {
    let mut hourly =
        tokio::time::interval_at(tokio::time::Instant::now() + CLEANUP_FIRST, CLEANUP_EVERY);
    hourly.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        let Some(state) = app.try_state::<AppState>() else {
            return;
        };
        tokio::select! {
            _ = hourly.tick() => {}
            () = state.cleanup_wake.notified() => {
                if !game_closed(REVIEW_WAIT_MAX).await {
                    continue;
                }
            }
        }
        let on = state.storage().cleanup;
        if !on || game_running().await {
            continue;
        }
        if let Err(error) = clean_up(&app).await {
            tracing::warn!(%error, "the cleanup failed");
        }
    }
}

/// One cleanup run, between the window's edits to matches. It stops if a game starts.
async fn clean_up(app: &AppHandle) -> Result<RunReport, String> {
    let state = app.state::<AppState>();
    let rules = state.storage().janitor_rules();
    let _one_at_a_time = state.match_edits.lock().await;
    let library = state.library.clone();
    let report = tauri::async_runtime::spawn_blocking(move || {
        let mut checked: Option<Instant> = None;
        let mut playing = false;
        library.cleanup(&rules, now_ms(), shell::recycle_strict, || {
            if checked.is_none_or(|at| at.elapsed() >= CLEANUP_GAME_CHECK) {
                playing = scan_processes().game_running;
                checked = Some(Instant::now());
            }
            !playing
        })
    })
    .await
    .map_err(|e| e.to_string())?;
    tracing::info!(
        removed = report.removed.len(),
        freed_mb = report.freed_bytes / 1_000_000,
        kept = report.kept.len(),
        failed = report.failed.len(),
        thumbnails = report.thumbnails,
        folders = report.folders,
        stopped = report.stopped,
        "cleanup ran"
    );
    for failed in &report.failed {
        tracing::debug!(why = %failed.why, "a video wasn't moved; trying again next time");
    }
    if !report.removed.is_empty() {
        let _ = app.emit("matches-changed", "");
    }
    let _ = app.emit("storage-changed", ());
    if !report.kept.is_empty() {
        let count = report.kept.len();
        let videos = if count == 1 { "video" } else { "videos" };
        tell_window(
            app,
            &format!(
                "The cleanup couldn't send {count} {videos} to the Recycle Bin (too big for it), so nothing was deleted. Storage has them for you to decide."
            ),
        );
    }
    Ok(report)
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or_default()
}
