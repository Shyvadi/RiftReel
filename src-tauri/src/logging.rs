//! Logging: `tracing` into a size-rotated file (5 × 5 MB), plus stderr in debug builds.
//!
//! Every formatted line passes through [`crate::privacy::scrub`] before it's written,
//! so Riot identifiers don't reach disk even if something formats one. At the default
//! `info` level only state changes are logged; per-event detail is `debug`/`trace`.
//! Dependencies are capped at `warn`.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

use crate::privacy;

pub const LOG_FILE: &str = "downplayed.log";
pub const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;
pub const MAX_LOG_FILES: usize = 5;

/// Overrides the configured level, e.g. `DOWNPLAYED_LOG=debug`.
pub const LOG_ENV: &str = "DOWNPLAYED_LOG";

#[derive(Debug, thiserror::Error)]
pub enum LogInitError {
    #[error("cannot open log file: {0}")]
    File(#[from] io::Error),
    #[error("logging already initialised: {0}")]
    AlreadySet(#[from] tracing_subscriber::util::TryInitError),
}

/// Filter directives for `level`: our crate at that level, chatty dependencies at warn.
pub fn directives(level: &str) -> String {
    let quiet = [
        "hyper",
        "hyper_util",
        "reqwest",
        "rustls",
        "tungstenite",
        "tokio_tungstenite",
        "tao",
        "wry",
        "tauri",
        "sysinfo",
    ];
    let mut directives = level.to_owned();
    for target in quiet {
        directives.push_str(&format!(",{target}=warn"));
    }
    directives
}

/// Installs the global subscriber: scrubbed, rotated file logs in `log_dir` (if any)
/// and, with `stderr`, the console.
pub fn init(log_dir: Option<&Path>, level: &str, stderr: bool) -> Result<(), LogInitError> {
    let filter = std::env::var(LOG_ENV)
        .ok()
        .and_then(|level| EnvFilter::try_new(directives(&level)).ok())
        .or_else(|| EnvFilter::try_new(directives(level)).ok())
        .unwrap_or_else(|| EnvFilter::new(directives("info")));

    let file_layer = match log_dir {
        Some(dir) => {
            let file = RotatingFile::open(dir, LOG_FILE, MAX_LOG_BYTES, MAX_LOG_FILES)?;
            let writer = ScrubbingWriter {
                sink: Sink::File(Arc::new(Mutex::new(file))),
            };
            Some(
                tracing_subscriber::fmt::layer()
                    .with_ansi(false)
                    .with_writer(writer),
            )
        }
        None => None,
    };
    let stderr_layer = stderr.then(|| {
        tracing_subscriber::fmt::layer().with_writer(ScrubbingWriter { sink: Sink::Stderr })
    });

    tracing_subscriber::registry()
        .with(filter)
        .with(file_layer)
        .with(stderr_layer)
        .try_init()?;
    install_panic_hook();
    Ok(())
}

fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        tracing::error!(panic = %info, "panic");
        previous(info);
    }));
}

#[derive(Clone)]
enum Sink {
    File(Arc<Mutex<RotatingFile>>),
    Stderr,
}

/// `MakeWriter` that scrubs identifiers from each formatted line.
#[derive(Clone)]
pub struct ScrubbingWriter {
    sink: Sink,
}

pub struct ScrubbingHandle {
    sink: Sink,
}

impl<'a> MakeWriter<'a> for ScrubbingWriter {
    type Writer = ScrubbingHandle;

    fn make_writer(&'a self) -> Self::Writer {
        ScrubbingHandle {
            sink: self.sink.clone(),
        }
    }
}

impl Write for ScrubbingHandle {
    /// tracing-subscriber hands over one complete formatted event per call.
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let text = String::from_utf8_lossy(buf);
        let clean = privacy::scrub(&text);
        match &self.sink {
            Sink::File(file) => file
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .write_all(clean.as_bytes())?,
            Sink::Stderr => io::stderr().write_all(clean.as_bytes())?,
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        match &self.sink {
            Sink::File(file) => file.lock().unwrap_or_else(PoisonError::into_inner).flush(),
            Sink::Stderr => io::stderr().flush(),
        }
    }
}

/// Size-based rotation: `downplayed.log`, then `downplayed.1.log` … `downplayed.{n-1}.log`.
pub struct RotatingFile {
    dir: PathBuf,
    stem: String,
    extension: String,
    max_bytes: u64,
    max_files: usize,
    file: Option<File>,
    size: u64,
}

impl RotatingFile {
    pub fn open(dir: &Path, file_name: &str, max_bytes: u64, max_files: usize) -> io::Result<Self> {
        fs::create_dir_all(dir)?;
        let name = Path::new(file_name);
        let stem = name.file_stem().map_or_else(
            || file_name.to_owned(),
            |s| s.to_string_lossy().into_owned(),
        );
        let extension = name
            .extension()
            .map_or_else(String::new, |e| e.to_string_lossy().into_owned());
        let mut rotating = Self {
            dir: dir.to_path_buf(),
            stem,
            extension,
            max_bytes,
            max_files: max_files.max(1),
            file: None,
            size: 0,
        };
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(rotating.path(0))?;
        rotating.size = file.metadata()?.len();
        rotating.file = Some(file);
        Ok(rotating)
    }

    fn path(&self, index: usize) -> PathBuf {
        let name = match (index, self.extension.is_empty()) {
            (0, true) => self.stem.clone(),
            (0, false) => format!("{}.{}", self.stem, self.extension),
            (n, true) => format!("{}.{n}", self.stem),
            (n, false) => format!("{}.{n}.{}", self.stem, self.extension),
        };
        self.dir.join(name)
    }

    fn rotate(&mut self) -> io::Result<()> {
        self.file = None;
        let _ = fs::remove_file(self.path(self.max_files - 1));
        for index in (1..self.max_files).rev() {
            let from = self.path(index - 1);
            if from.exists() {
                fs::rename(&from, self.path(index))?;
            }
        }
        self.file = Some(File::create(self.path(0))?);
        self.size = 0;
        Ok(())
    }
}

impl Write for RotatingFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.size > 0 && self.size + buf.len() as u64 > self.max_bytes {
            self.rotate()?;
        }
        if self.file.is_none() {
            self.file = Some(
                OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(self.path(0))?,
            );
        }
        let file = self.file.as_mut().expect("log file was just opened");
        let written = file.write(buf)?;
        self.size += written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.as_mut().map_or(Ok(()), Write::flush)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("downplayed-log-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn rotates_by_size_and_keeps_n_files() {
        let dir = temp_dir("rotate");
        let mut file = RotatingFile::open(&dir, "test.log", 100, 3).unwrap();
        // One write per line, like tracing-subscriber's one write per event.
        for i in 0..20 {
            file.write_all(format!("line {i:02} padding padding padding\n").as_bytes())
                .unwrap();
        }
        let mut names: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        assert_eq!(names, ["test.1.log", "test.2.log", "test.log"]);
        for name in &names {
            assert!(fs::metadata(dir.join(name)).unwrap().len() <= 100);
        }
        let newest = fs::read_to_string(dir.join("test.log")).unwrap();
        assert!(newest.contains("line 19"));
        drop(file);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn writer_scrubs_identifiers() {
        let dir = temp_dir("scrub");
        let file = RotatingFile::open(&dir, "scrub.log", MAX_LOG_BYTES, 2).unwrap();
        let writer = ScrubbingWriter {
            sink: Sink::File(Arc::new(Mutex::new(file))),
        };
        let puuid = "x".repeat(78);
        writer
            .make_writer()
            .write_all(
                format!(
                    "event body {{\"summonerName\":\"Some Body#EUW\",\"puuid\":\"{puuid}\"}}\n"
                )
                .as_bytes(),
            )
            .unwrap();
        let written = fs::read_to_string(dir.join("scrub.log")).unwrap();
        assert!(!written.contains("Some Body"), "{written}");
        assert!(!written.contains(&puuid), "{written}");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn dependencies_are_capped_at_warn() {
        let directives = directives("debug");
        assert!(directives.starts_with("debug,"));
        assert!(directives.contains("rustls=warn"));
        assert!(EnvFilter::try_new(&directives).is_ok());
    }
}
