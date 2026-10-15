//! User settings: one local TOML file, `%APPDATA%\Downplayed\settings.toml`. There is no
//! remote configuration.

use std::collections::BTreeMap;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use serde::de::{Deserializer, MapAccess, Visitor};
use serde::ser::{SerializeMap, Serializer};
use serde::{Deserialize, Serialize};

use crate::tracker::moments::DEFAULT_MAX_MOMENT_S;
use crate::tracker::normalize::{MarkerConfig, MarkerKind, MarkerWindow};
use crate::tracker::runtime::TrackerConfig;
use crate::tracker::session::{MatchFilters, SessionConfig};

pub const SETTINGS_SCHEMA: u32 = 1;

/// Outplayed's per-event sliders run 1–180 s.
const MAX_WINDOW_S: f64 = 180.0;

const HEADER: &str = "\
# Downplayed settings. Restart Downplayed after editing.
# Every value is optional; anything missing falls back to its default.
# [markers.<type>]: pre_s / post_s are seconds kept before / after the event.
";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub schema: u32,
    pub logging: LoggingSettings,
    pub tracker: TrackerSettings,
    pub ui: UiSettings,
    pub markers: MarkerSettings,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            schema: SETTINGS_SCHEMA,
            logging: LoggingSettings::default(),
            tracker: TrackerSettings::default(),
            ui: UiSettings::default(),
            markers: MarkerSettings::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UiSettings {
    /// Open the window on a match's review when it ends (never while the game runs).
    pub open_review_after_game: bool,
}

impl Default for UiSettings {
    fn default() -> Self {
        Self {
            open_review_after_game: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LoggingSettings {
    /// `error`, `warn`, `info` (default: state changes only), `debug` or `trace`.
    pub level: String,
}

impl Default for LoggingSettings {
    fn default() -> Self {
        Self {
            level: "info".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TrackerSettings {
    /// Use the League client (LCU) for the match lifecycle, queue and end-of-game stats.
    /// Off means the Live Client API and the game process only.
    pub use_league_client: bool,
    /// Also mark objectives and aces your team took, not only your own.
    pub include_team_events: bool,
    /// Longest moment (clip), in seconds.
    pub max_moment_s: f64,
    pub filters: MatchFilters,
}

impl Default for TrackerSettings {
    fn default() -> Self {
        Self {
            use_league_client: true,
            include_team_events: false,
            max_moment_s: DEFAULT_MAX_MOMENT_S,
            filters: MatchFilters::default(),
        }
    }
}

/// The `[markers.<type>]` tables. A missing type or field keeps its default.
#[derive(Debug, Clone, PartialEq)]
pub struct MarkerSettings {
    pub windows: BTreeMap<MarkerKind, MarkerWindow>,
    /// Table names that aren't a marker type; reported when loading.
    pub unknown: Vec<String>,
}

impl Default for MarkerSettings {
    fn default() -> Self {
        Self {
            windows: MarkerKind::ALL
                .into_iter()
                .map(|k| (k, k.default_window()))
                .collect(),
            unknown: Vec::new(),
        }
    }
}

impl Serialize for MarkerSettings {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.windows.len()))?;
        for kind in MarkerKind::ALL {
            let window = self
                .windows
                .get(&kind)
                .copied()
                .unwrap_or_else(|| kind.default_window());
            map.serialize_entry(kind.as_str(), &window)?;
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for MarkerSettings {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct PartialWindow {
            enabled: Option<bool>,
            pre_s: Option<f64>,
            post_s: Option<f64>,
        }

        struct MarkersVisitor;

        impl<'de> Visitor<'de> for MarkersVisitor {
            type Value = MarkerSettings;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a table of marker types")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<MarkerSettings, A::Error> {
                let mut settings = MarkerSettings::default();
                while let Some((name, partial)) = map.next_entry::<String, PartialWindow>()? {
                    let Some(kind) = MarkerKind::parse(&name) else {
                        settings.unknown.push(name);
                        continue;
                    };
                    let window = settings
                        .windows
                        .entry(kind)
                        .or_insert_with(|| kind.default_window());
                    window.enabled = partial.enabled.unwrap_or(window.enabled);
                    window.pre_s = partial.pre_s.unwrap_or(window.pre_s);
                    window.post_s = partial.post_s.unwrap_or(window.post_s);
                }
                Ok(settings)
            }
        }

        deserializer.deserialize_map(MarkersVisitor)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error("cannot read {path}: {source}")]
    Io { path: String, source: io::Error },
    #[error("invalid settings file: {0}")]
    Parse(#[from] toml::de::Error),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadOutcome {
    Loaded,
    /// No file yet: defaults were written.
    Created,
    /// The file couldn't be used; defaults are in effect and the file is left alone.
    Invalid(String),
}

#[derive(Debug, Clone)]
pub struct LoadedSettings {
    pub settings: Settings,
    pub outcome: LoadOutcome,
    /// Values that were ignored or corrected.
    pub warnings: Vec<String>,
}

impl Settings {
    pub fn from_toml(text: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(text)
    }

    pub fn to_toml(&self) -> String {
        let body = toml::to_string_pretty(self).expect("settings always serialize");
        format!("{HEADER}\n{body}")
    }

    pub fn load(path: &Path) -> Result<Self, SettingsError> {
        let text = std::fs::read_to_string(path).map_err(|source| SettingsError::Io {
            path: path.display().to_string(),
            source,
        })?;
        Ok(Self::from_toml(&text)?)
    }

    /// Loads the settings file, creating it with defaults if it doesn't exist. An
    /// unreadable or invalid file is never overwritten: defaults apply for this run and
    /// the problem is reported.
    pub fn load_or_create(path: &Path) -> LoadedSettings {
        let (mut settings, outcome, mut warnings) = match Self::load(path) {
            Ok(settings) => (settings, LoadOutcome::Loaded, Vec::new()),
            Err(SettingsError::Io { source, .. }) if source.kind() == io::ErrorKind::NotFound => {
                let settings = Self::default();
                let mut warnings = Vec::new();
                if let Err(e) = settings.save(path) {
                    warnings.push(format!(
                        "could not write default settings to {}: {e}",
                        path.display()
                    ));
                }
                (settings, LoadOutcome::Created, warnings)
            }
            Err(e) => (
                Self::default(),
                LoadOutcome::Invalid(e.to_string()),
                Vec::new(),
            ),
        };
        warnings.extend(settings.validate());
        LoadedSettings {
            settings,
            outcome,
            warnings,
        }
    }

    /// Writes the file atomically (temp file + rename).
    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let temp = path.with_extension("toml.tmp");
        std::fs::write(&temp, self.to_toml())?;
        std::fs::rename(&temp, path)
    }

    /// Clamps out-of-range values to something usable. Returns what was changed.
    pub fn validate(&mut self) -> Vec<String> {
        let mut warnings = Vec::new();
        for name in self.markers.unknown.drain(..) {
            warnings.push(format!(
                "[markers.{name}] is not a marker type and was ignored"
            ));
        }
        for (kind, window) in &mut self.markers.windows {
            for (field, value) in [("pre_s", &mut window.pre_s), ("post_s", &mut window.post_s)] {
                let clamped = if value.is_finite() {
                    value.clamp(0.0, MAX_WINDOW_S)
                } else {
                    0.0
                };
                if clamped != *value {
                    warnings.push(format!(
                        "markers.{}.{field} = {value} is out of range; using {clamped}",
                        kind.as_str()
                    ));
                    *value = clamped;
                }
            }
        }
        if !(self.tracker.max_moment_s.is_finite() && self.tracker.max_moment_s >= 1.0) {
            warnings.push(format!(
                "tracker.max_moment_s = {} is invalid; using {DEFAULT_MAX_MOMENT_S}",
                self.tracker.max_moment_s
            ));
            self.tracker.max_moment_s = DEFAULT_MAX_MOMENT_S;
        }
        let level = self.logging.level.trim().to_ascii_lowercase();
        if !["error", "warn", "info", "debug", "trace"].contains(&level.as_str()) {
            warnings.push(format!(
                "logging.level = {:?} is not a level; using info",
                self.logging.level
            ));
            self.logging.level = "info".into();
        } else {
            self.logging.level = level;
        }
        warnings
    }

    pub fn tracker_config(&self) -> TrackerConfig {
        let mut markers = MarkerConfig::default();
        for (kind, window) in &self.markers.windows {
            markers.set_window(*kind, *window);
        }
        markers.include_team = self.tracker.include_team_events;
        TrackerConfig {
            session: SessionConfig {
                markers,
                max_moment_s: self.tracker.max_moment_s,
            },
            filters: self.tracker.filters,
        }
    }
}

/// Where Downplayed keeps its files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPaths {
    pub settings_file: PathBuf,
    pub log_dir: PathBuf,
    /// One folder per finished match, each with its `match.json` (doc 03 §7).
    pub matches_dir: PathBuf,
    /// Where `downplayed-dev record` saves recordings by default; the window lists them.
    pub recordings_dir: PathBuf,
}

impl AppPaths {
    /// `%APPDATA%\Downplayed\settings.toml`; logs, matches and recordings under
    /// `%LOCALAPPDATA%\Downplayed`. `DOWNPLAYED_HOME` puts everything under one folder
    /// instead (development).
    pub fn resolve() -> Self {
        if let Some(home) = std::env::var_os("DOWNPLAYED_HOME").filter(|h| !h.is_empty()) {
            return Self::under(PathBuf::from(home));
        }
        let fallback = || PathBuf::from(".");
        let config_dir = dirs::config_dir()
            .unwrap_or_else(fallback)
            .join("Downplayed");
        let data_dir = dirs::data_local_dir()
            .unwrap_or_else(fallback)
            .join("Downplayed");
        Self {
            settings_file: config_dir.join("settings.toml"),
            ..Self::data_under(&data_dir)
        }
    }

    pub fn under(root: PathBuf) -> Self {
        Self {
            settings_file: root.join("settings.toml"),
            ..Self::data_under(&root)
        }
    }

    fn data_under(data_dir: &Path) -> Self {
        Self {
            settings_file: PathBuf::new(),
            log_dir: data_dir.join("logs"),
            matches_dir: data_dir.join("library").join("League of Legends"),
            recordings_dir: data_dir.join("recordings"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("downplayed-settings-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn defaults_match_the_spec() {
        let config = Settings::default().tracker_config();
        let window = |kind| config.session.markers.window(kind);
        assert_eq!(window(MarkerKind::Kill), MarkerWindow::on(12.0, 8.0));
        assert_eq!(window(MarkerKind::Assist), MarkerWindow::on(12.0, 8.0));
        assert_eq!(window(MarkerKind::Death), MarkerWindow::on(12.0, 3.0));
        assert_eq!(window(MarkerKind::BaronKill), MarkerWindow::on(10.0, 5.0));
        assert_eq!(window(MarkerKind::Victory), MarkerWindow::on(10.0, 1.0));
        assert!(!window(MarkerKind::FirstBlood).enabled);
        assert!(!config.session.markers.include_team);
        assert_eq!(config.session.max_moment_s, 60.0);
    }

    #[test]
    fn round_trips_through_toml() {
        let settings = Settings::default();
        let text = settings.to_toml();
        assert!(text.starts_with("# Downplayed settings"));
        assert!(text.contains("[markers.kill]"), "{text}");
        assert_eq!(Settings::from_toml(&text).unwrap(), settings);
    }

    #[test]
    fn partial_files_keep_per_type_defaults() {
        let mut settings = Settings::from_toml(
            "[markers.death]\npost_s = 5\n[markers.first_blood]\nenabled = true\n[markers.ultimate]\nenabled = true\n[tracker]\ninclude_team_events = true\n",
        )
        .unwrap();
        let warnings = settings.validate();
        assert_eq!(
            warnings,
            ["[markers.ultimate] is not a marker type and was ignored"]
        );
        let config = settings.tracker_config();
        assert_eq!(
            config.session.markers.window(MarkerKind::Death),
            MarkerWindow::on(12.0, 5.0)
        );
        assert_eq!(
            config.session.markers.window(MarkerKind::FirstBlood),
            MarkerWindow::on(10.0, 5.0)
        );
        assert_eq!(
            config.session.markers.window(MarkerKind::Kill),
            MarkerWindow::on(12.0, 8.0)
        );
        assert!(config.session.markers.include_team);
        assert!(settings.tracker.use_league_client);
    }

    #[test]
    fn the_review_opens_after_games_unless_turned_off() {
        assert!(Settings::default().ui.open_review_after_game);
        assert!(Settings::default().to_toml().contains("[ui]"));
        let settings = Settings::from_toml("[ui]\nopen_review_after_game = false\n").unwrap();
        assert!(!settings.ui.open_review_after_game);
    }

    #[test]
    fn voidgrubs_are_off_until_enabled() {
        let defaults = Settings::default();
        assert_eq!(
            defaults
                .tracker_config()
                .session
                .markers
                .window(MarkerKind::Voidgrubs),
            MarkerWindow::off(10.0, 5.0)
        );
        assert!(defaults.to_toml().contains("[markers.voidgrubs]"));

        let mut settings =
            Settings::from_toml("[markers.voidgrubs]\nenabled = true\npost_s = 8\n").unwrap();
        assert!(settings.validate().is_empty());
        assert_eq!(
            settings
                .tracker_config()
                .session
                .markers
                .window(MarkerKind::Voidgrubs),
            MarkerWindow::on(10.0, 8.0)
        );
    }

    #[test]
    fn out_of_range_values_are_clamped() {
        let mut settings =
            Settings::from_toml("[logging]\nlevel = \"loud\"\n[tracker]\nmax_moment_s = 0\n[markers.kill]\npre_s = 999\npost_s = -1\n").unwrap();
        assert_eq!(settings.validate().len(), 4);
        assert_eq!(settings.logging.level, "info");
        assert_eq!(settings.tracker.max_moment_s, 60.0);
        assert_eq!(
            settings.markers.windows[&MarkerKind::Kill],
            MarkerWindow::on(180.0, 0.0)
        );
    }

    #[test]
    fn load_or_create_writes_defaults_once_and_never_clobbers() {
        let dir = temp_dir("create");
        let path = dir.join("settings.toml");
        let first = Settings::load_or_create(&path);
        assert_eq!(first.outcome, LoadOutcome::Created);
        assert!(path.exists());
        assert_eq!(Settings::load_or_create(&path).outcome, LoadOutcome::Loaded);

        std::fs::write(&path, "this is not = = toml").unwrap();
        let broken = Settings::load_or_create(&path);
        assert!(matches!(broken.outcome, LoadOutcome::Invalid(_)));
        assert_eq!(broken.settings, Settings::default());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "this is not = = toml"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
