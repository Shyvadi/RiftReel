//! The match list's thumbnails (doc 03 §9): one frame of each recorded match, made one
//! at a time in the background at low priority. A match gets one when it is saved; a
//! match the list shows without one (an older or imported match) is queued when it
//! scrolls into view. Nothing is made while League's game runs.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tokio::sync::mpsc;

use crate::capture::clips::MATCH_THUMBNAIL_WIDTH;
use crate::capture::ffmpeg;
use crate::league::client::scan_processes;
use crate::library::Library;

/// How often a paused queue looks again for the game having closed.
const GAME_WAIT: Duration = Duration::from_secs(30);

/// The event a finished thumbnail sends to an open window.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Made {
    id: String,
    thumbnail_path: String,
}

/// Asks for thumbnails; cheap to clone.
#[derive(Clone)]
pub struct Thumbnails {
    queue: mpsc::UnboundedSender<String>,
}

impl Thumbnails {
    /// Starts the worker. Without an ffmpeg it only drops what it is asked.
    pub fn start(app: AppHandle, library: Arc<Library>, ffmpeg: Option<PathBuf>) -> Self {
        let (queue, requests) = mpsc::unbounded_channel();
        tauri::async_runtime::spawn(work(app, library, ffmpeg, requests));
        Self { queue }
    }

    /// Queues a match; one that has a thumbnail, or no video, is skipped.
    pub fn request(&self, id: String) {
        let _ = self.queue.send(id);
    }
}

async fn work(
    app: AppHandle,
    library: Arc<Library>,
    ffmpeg: Option<PathBuf>,
    mut requests: mpsc::UnboundedReceiver<String>,
) {
    // Each match is tried once per run: a video ffmpeg can't read stays without one.
    let mut tried = HashSet::new();
    while let Some(id) = requests.recv().await {
        let Some(ffmpeg) = ffmpeg.as_deref() else {
            continue;
        };
        if !tried.insert(id.clone()) {
            continue;
        }
        while game_running().await {
            tokio::time::sleep(GAME_WAIT).await;
        }
        let lookup = library.clone();
        let wanted = id.clone();
        let job = tauri::async_runtime::spawn_blocking(move || lookup.thumbnail_job(&wanted))
            .await
            .ok()
            .flatten();
        let Some(job) = job else {
            continue;
        };
        let made = ffmpeg::make_thumbnail(
            ffmpeg,
            &job.video,
            job.at_s,
            MATCH_THUMBNAIL_WIDTH,
            &job.out,
        )
        .await;
        if let Err(error) = made {
            tracing::debug!(%id, %error, "no thumbnail for this match");
            continue;
        }
        let record = library.clone();
        let wanted = id.clone();
        let recorded =
            tauri::async_runtime::spawn_blocking(move || record.set_match_thumbnail(&wanted))
                .await
                .ok()
                .flatten();
        if let Some(path) = recorded {
            let _ = app.emit(
                "match-thumbnail",
                Made {
                    id,
                    thumbnail_path: path.display().to_string(),
                },
            );
        }
    }
}

async fn game_running() -> bool {
    tauri::async_runtime::spawn_blocking(|| scan_processes().game_running)
        .await
        .unwrap_or(true)
}
