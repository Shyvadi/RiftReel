//! The cleanup, or janitor (doc 03 §8): keeps the full-match videos within the age and
//! size the user set, oldest first, by sending them to the Recycle Bin. Clips are never
//! removed (their total only gets a warning), a favorite match's video never, and a video
//! RiftReel didn't record (an Outplayed import) only when the user allowed it for that
//! import. A video Windows can't recycle is kept and flagged for the user. It also clears
//! clip thumbnails without their clip and empty match folders.
//!
//! It runs only when the user has turned it on; [`Library::cleanup_overview`] is the same
//! plan as a dry run, changing nothing.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::outplayed::Import;
use super::{
    read_dir, read_json, safe_name, video_path, write_json, Library, MATCH_FILE,
    MATCH_THUMBNAIL_NAME, OUTPLAYED_SOURCE,
};
use crate::capture::clips::{CLIPS_DIR, THUMBS_DIR};

/// The janitor's notes, in the library's root: the videos it couldn't recycle, and how
/// its last run went.
pub const CLEANUP_FILE: &str = "cleanup.json";
const CLEANUP_SCHEMA: u32 = 1;
const DAY_MS: i64 = 86_400_000;
/// A clip thumbnail no clip lists is left alone this long (a clip being saved has its
/// thumbnail a moment before it is listed).
const ORPHAN_AGE: Duration = Duration::from_secs(10 * 60);
/// An empty match folder is left alone this long (a recording that is starting fills
/// its new folder).
const EMPTY_AGE: Duration = Duration::from_secs(60 * 60);

/// How sending one file to the Recycle Bin went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recycled {
    Done,
    /// Windows would have deleted it for good (too big for the bin, a drive without
    /// one), so it was left where it is.
    NotRecyclable,
    /// It wasn't moved (in use, access denied…); nothing was deleted.
    Failed(String),
}

/// The user's limits (`[storage]`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rules {
    /// Full matches older than this many days go (0: no age limit).
    pub keep_days: u32,
    /// The oldest go while the full matches take more than this (0: no size limit).
    pub max_full_bytes: u64,
    /// Clips above this total get a warning; they are never removed (0: no warning).
    pub clips_warn_bytes: u64,
}

/// What the janitor knows about one match folder.
#[derive(Debug, Clone, PartialEq)]
pub struct Holding {
    pub id: String,
    pub when_ms: i64,
    pub champion: Option<String>,
    pub favorite: bool,
    /// The full-match video and its size.
    pub video: Option<(PathBuf, u64)>,
    /// The janitor may remove the video: RiftReel recorded it, or the user allowed it
    /// for the import it came from.
    pub managed: bool,
    pub clip_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    /// Older than the days kept.
    Age,
    /// Over the size limit.
    Size,
}

/// A full-match video the janitor would remove (or did).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Removal {
    pub id: String,
    pub when_ms: i64,
    pub champion: Option<String>,
    pub path: PathBuf,
    pub bytes: u64,
    pub reason: Reason,
}

/// What the janitor would do now.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Plan {
    /// Oldest first.
    pub removals: Vec<Removal>,
    pub freed_bytes: u64,
    /// The full-match videos the janitor looks after (favorites included), and their
    /// bytes now and after the removals.
    pub managed_matches: usize,
    pub managed_bytes: u64,
    pub bytes_after: u64,
    /// Of those, the favorites (never removed).
    pub favorites: usize,
    /// Imported videos the janitor leaves alone.
    pub unmanaged_matches: usize,
    pub unmanaged_bytes: u64,
    pub clip_bytes: u64,
    pub clips_over_warning: bool,
}

/// The plan for `holdings` under `rules` at `now_ms`: every managed, non-favorite video
/// older than the days kept, then the oldest of the rest while the managed videos
/// (favorites included) take more than the size limit. Videos in `skip` (flagged before)
/// count but aren't tried again.
pub fn plan(holdings: &[Holding], rules: &Rules, now_ms: i64, skip: &HashSet<PathBuf>) -> Plan {
    let mut plan = Plan::default();
    let mut candidates = Vec::new();
    for holding in holdings {
        plan.clip_bytes += holding.clip_bytes;
        let Some((path, bytes)) = &holding.video else {
            continue;
        };
        if !holding.managed {
            plan.unmanaged_matches += 1;
            plan.unmanaged_bytes += bytes;
            continue;
        }
        plan.managed_matches += 1;
        plan.managed_bytes += bytes;
        if holding.favorite {
            plan.favorites += 1;
        } else if !skip.contains(path) {
            candidates.push((holding, path, *bytes));
        }
    }
    candidates.sort_by(|a, b| a.0.when_ms.cmp(&b.0.when_ms).then(a.0.id.cmp(&b.0.id)));

    let removal = |holding: &Holding, path: &PathBuf, bytes: u64, reason| Removal {
        id: holding.id.clone(),
        when_ms: holding.when_ms,
        champion: holding.champion.clone(),
        path: path.clone(),
        bytes,
        reason,
    };
    let mut remaining = plan.managed_bytes;
    let cutoff = (rules.keep_days > 0).then(|| now_ms - i64::from(rules.keep_days) * DAY_MS);
    // The old ones are the start of the list, so the removals stay oldest first.
    let mut rest = Vec::new();
    for (holding, path, bytes) in candidates {
        if cutoff.is_some_and(|cutoff| holding.when_ms < cutoff) {
            plan.removals
                .push(removal(holding, path, bytes, Reason::Age));
            remaining -= bytes;
        } else {
            rest.push((holding, path, bytes));
        }
    }
    if rules.max_full_bytes > 0 {
        for (holding, path, bytes) in rest {
            if remaining <= rules.max_full_bytes {
                break;
            }
            plan.removals
                .push(removal(holding, path, bytes, Reason::Size));
            remaining -= bytes;
        }
    }
    plan.freed_bytes = plan.managed_bytes - remaining;
    plan.bytes_after = remaining;
    plan.clips_over_warning =
        rules.clips_warn_bytes > 0 && plan.clip_bytes > rules.clips_warn_bytes;
    plan
}

/// A video the janitor meant to remove but kept, and why.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Kept {
    #[serde(flatten)]
    pub removal: Removal,
    pub why: String,
    pub at_ms: i64,
}

/// What a run did.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunReport {
    pub removed: Vec<Removal>,
    pub freed_bytes: u64,
    /// Couldn't be recycled: kept and flagged for the user.
    pub kept: Vec<Kept>,
    /// Couldn't be moved (in use, say): kept, and tried again next time.
    pub failed: Vec<Kept>,
    /// Clip thumbnails without their clip, recycled.
    pub thumbnails: usize,
    /// Empty match folders removed.
    pub folders: usize,
    /// A game started, so it stopped there.
    pub stopped: bool,
}

/// How the last run went, as the window shows it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LastRun {
    pub at_ms: i64,
    pub removed: usize,
    pub freed_bytes: u64,
    pub kept: usize,
    pub failed: usize,
    pub thumbnails: usize,
    pub folders: usize,
    pub stopped: bool,
}

/// The cleanup as the window shows it: the dry run and what is waiting for the user.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Overview {
    /// What a run would do now.
    pub plan: Plan,
    /// Videos a run couldn't recycle and left for the user to decide about.
    pub kept: Vec<Kept>,
    /// Clip thumbnails without their clip, and empty match folders, a run would clear.
    pub thumbnails: usize,
    pub folders: usize,
    pub last_run: Option<LastRun>,
}

/// `cleanup.json`.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Notes {
    schema: u32,
    kept: Vec<Kept>,
    last_run: Option<LastRun>,
}

/// What the janitor found in the matches folder.
#[derive(Debug, Default)]
struct Survey {
    holdings: Vec<Holding>,
    /// Clip thumbnails no clip lists (never a match's own picture).
    orphans: Vec<PathBuf>,
    /// Match folders without a single file.
    empty: Vec<PathBuf>,
}

impl Library {
    /// What cleanup would do now under `rules`, and what it has left for the user. A dry
    /// run: nothing is changed.
    pub fn cleanup_overview(&self, rules: &Rules, now_ms: i64) -> Overview {
        let survey = self.survey(now_ms);
        let notes = self.notes();
        let kept = still_kept(notes.kept, &survey.holdings);
        Overview {
            plan: plan(&survey.holdings, rules, now_ms, &paths_of(&kept)),
            kept,
            thumbnails: survey.orphans.len(),
            folders: survey.empty.len(),
            last_run: notes.last_run,
        }
    }

    /// Runs the cleanup: the plan's videos, then the stray thumbnails, go to `recycle`
    /// (the Recycle Bin, one file at a time) and the empty folders are removed. A video
    /// `recycle` can't recycle is flagged and not tried again; one it fails to move is
    /// tried again next time. `go_on` is asked before each file and stops the run when it
    /// says no (a game started). Each match whose video went has `videoRemoved` in its
    /// `match.json`, and its index entry follows.
    pub fn cleanup(
        &self,
        rules: &Rules,
        now_ms: i64,
        mut recycle: impl FnMut(&Path) -> Recycled,
        mut go_on: impl FnMut() -> bool,
    ) -> RunReport {
        let survey = self.survey(now_ms);
        let mut notes = self.notes();
        let mut kept = still_kept(std::mem::take(&mut notes.kept), &survey.holdings);
        let plan = plan(&survey.holdings, rules, now_ms, &paths_of(&kept));
        let mut report = RunReport::default();
        let mut stop = |report: &mut RunReport| {
            report.stopped = !go_on();
            report.stopped
        };

        for removal in plan.removals {
            if stop(&mut report) {
                break;
            }
            match self.remove_video(&removal, now_ms, &mut recycle) {
                Recycled::Done => {
                    report.freed_bytes += removal.bytes;
                    report.removed.push(removal);
                }
                Recycled::NotRecyclable => report.kept.push(Kept {
                    removal,
                    why: "too big for the Recycle Bin, or the drive has none".into(),
                    at_ms: now_ms,
                }),
                Recycled::Failed(why) => report.failed.push(Kept {
                    removal,
                    why,
                    at_ms: now_ms,
                }),
            }
        }
        for thumbnail in &survey.orphans {
            if report.stopped || stop(&mut report) {
                break;
            }
            if recycle(thumbnail) == Recycled::Done {
                report.thumbnails += 1;
            }
        }
        if !report.stopped {
            report.folders = survey.empty.iter().filter(|dir| remove_empty(dir)).count();
        }

        kept.extend(report.kept.iter().cloned());
        notes.kept = kept;
        notes.last_run = Some(LastRun {
            at_ms: now_ms,
            removed: report.removed.len(),
            freed_bytes: report.freed_bytes,
            kept: report.kept.len(),
            failed: report.failed.len(),
            thumbnails: report.thumbnails,
            folders: report.folders,
            stopped: report.stopped,
        });
        self.save_notes(&mut notes);
        report
    }

    /// Lets the cleanup try a flagged video again on its next run. Returns `false` if it
    /// wasn't flagged.
    pub fn retry_cleanup(&self, id: &str) -> bool {
        let mut notes = self.notes();
        let before = notes.kept.len();
        notes.kept.retain(|kept| kept.removal.id != id);
        let found = notes.kept.len() != before;
        if found {
            self.save_notes(&mut notes);
        }
        found
    }

    /// Sends one planned video to `recycle`, if its match still has it and still isn't a
    /// favorite, and notes the removal in the match.
    fn remove_video(
        &self,
        removal: &Removal,
        now_ms: i64,
        recycle: &mut impl FnMut(&Path) -> Recycled,
    ) -> Recycled {
        let Some(name) = safe_name(&removal.id) else {
            return Recycled::Failed("match not found".into());
        };
        let dir = self.matches_dir.join(name);
        let file = dir.join(MATCH_FILE);
        let Some(mut summary) = read_json(&file) else {
            return Recycled::Failed("match not found".into());
        };
        let video = video_path(&dir, &summary);
        if is_favorite(&summary) || video.as_deref() != Some(removal.path.as_path()) {
            return Recycled::Failed("the match changed since it was planned".into());
        }
        let outcome = recycle(&removal.path);
        if outcome == Recycled::Done {
            summary["videoRemoved"] = serde_json::json!({
                "atMs": now_ms,
                "reason": removal.reason,
                "bytes": removal.bytes,
            });
            if let Err(error) = write_json(&file, &summary) {
                tracing::warn!(%error, id = %removal.id, "could not note the removed video");
            }
            self.refresh(&removal.id);
        }
        outcome
    }

    fn survey(&self, now_ms: i64) -> Survey {
        let now = UNIX_EPOCH + Duration::from_millis(u64::try_from(now_ms).unwrap_or_default());
        let aged = |path: &Path, age: Duration| {
            std::fs::metadata(path)
                .and_then(|meta| meta.modified())
                .is_ok_and(|at| now.duration_since(at).is_ok_and(|elapsed| elapsed >= age))
        };
        let imports = self.imports();
        let mut survey = Survey::default();
        for dir in read_dir(&self.matches_dir) {
            if !dir.is_dir() {
                continue;
            }
            let Some(id) = dir.file_name().and_then(|n| n.to_str()).map(str::to_owned) else {
                continue;
            };
            match read_json(&dir.join(MATCH_FILE)) {
                Some(summary) => {
                    let orphans = orphan_thumbnails(&dir, &summary);
                    survey
                        .orphans
                        .extend(orphans.into_iter().filter(|p| aged(p, ORPHAN_AGE)));
                    survey.holdings.push(holding(id, &dir, &summary, &imports));
                }
                None if is_empty(&dir) && aged(&dir, EMPTY_AGE) => survey.empty.push(dir),
                None => {}
            }
        }
        survey
    }

    fn notes_path(&self) -> Option<PathBuf> {
        let root = self.matches_dir.parent()?;
        (!root.as_os_str().is_empty()).then(|| root.join(CLEANUP_FILE))
    }

    fn notes(&self) -> Notes {
        self.notes_path()
            .and_then(|path| read_json(&path))
            .and_then(|value| serde_json::from_value(value).ok())
            .unwrap_or_default()
    }

    fn save_notes(&self, notes: &mut Notes) {
        let Some(path) = self.notes_path() else {
            return;
        };
        notes.schema = CLEANUP_SCHEMA;
        if let Err(error) = write_json(&path, notes) {
            tracing::warn!(%error, "could not save the cleanup's notes");
        }
    }
}

fn is_favorite(summary: &Value) -> bool {
    summary["favorite"].as_bool().unwrap_or_default()
}

fn holding(id: String, dir: &Path, summary: &Value, imports: &[Import]) -> Holding {
    let video = video_path(dir, summary)
        .and_then(|path| Some((path.clone(), std::fs::metadata(&path).ok()?.len())));
    Holding {
        id,
        // A match without a start time is never old.
        when_ms: summary["startedWallMs"].as_i64().unwrap_or(i64::MAX),
        champion: summary["champion"].as_str().map(str::to_owned),
        favorite: is_favorite(summary),
        managed: managed(
            summary,
            video.as_ref().map(|(path, _)| path.as_path()),
            imports,
        ),
        video,
        clip_bytes: files_bytes(&dir.join(CLIPS_DIR)),
    }
}

/// Whether the cleanup may remove a match's video: RiftReel recorded it, or it was
/// imported and the user allowed the cleanup for that import, and it is still inside the
/// folder imported from.
fn managed(summary: &Value, video: Option<&Path>, imports: &[Import]) -> bool {
    if summary["source"] != OUTPLAYED_SOURCE {
        return true;
    }
    let (Some(import), Some(video)) = (summary["importId"].as_str(), video) else {
        return false;
    };
    imports
        .iter()
        .any(|i| i.id == import && i.cleanup && video.starts_with(&i.root))
}

/// The flagged videos that are still a match's video, still the janitor's to remove and
/// not a favorite since.
fn still_kept(kept: Vec<Kept>, holdings: &[Holding]) -> Vec<Kept> {
    kept.into_iter()
        .filter(|kept| {
            holdings.iter().any(|holding| {
                holding.managed
                    && !holding.favorite
                    && holding
                        .video
                        .as_ref()
                        .is_some_and(|(path, _)| *path == kept.removal.path)
            })
        })
        .collect()
}

fn paths_of(kept: &[Kept]) -> HashSet<PathBuf> {
    kept.iter().map(|kept| kept.removal.path.clone()).collect()
}

/// The bytes of the files in `dir` (not below it).
fn files_bytes(dir: &Path) -> u64 {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|entry| entry.metadata().ok())
                .filter(|meta| meta.is_file())
                .map(|meta| meta.len())
                .sum()
        })
        .unwrap_or_default()
}

/// The pictures in a match's `thumbs/` that are neither its own nor a listed clip's.
fn orphan_thumbnails(dir: &Path, summary: &Value) -> Vec<PathBuf> {
    let listed: HashSet<&str> = summary["clips"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|clip| {
            clip["thumbnail"]
                .as_str()?
                .strip_prefix(THUMBS_DIR)?
                .strip_prefix('/')
        })
        .collect();
    read_dir(&dir.join(THUMBS_DIR))
        .into_iter()
        .filter(|path| {
            let name = path.file_name().and_then(|n| n.to_str());
            path.is_file()
                && path.extension().is_some_and(|ext| ext == "jpg")
                && name.is_some_and(|name| name != MATCH_THUMBNAIL_NAME && !listed.contains(name))
        })
        .collect()
}

/// No file in it, nor in the folders in it (an empty `clips/` or `thumbs/` is fine).
fn is_empty(dir: &Path) -> bool {
    std::fs::read_dir(dir).is_ok_and(|entries| {
        entries.flatten().all(|entry| {
            entry.file_type().is_ok_and(|kind| kind.is_dir())
                && std::fs::read_dir(entry.path()).is_ok_and(|mut inner| inner.next().is_none())
        })
    })
}

/// Removes an empty match folder: only ever empty folders, so a file that appeared since
/// keeps it.
fn remove_empty(dir: &Path) -> bool {
    for inner in read_dir(dir) {
        let _ = std::fs::remove_dir(inner);
    }
    std::fs::remove_dir(dir).is_ok()
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use super::*;
    use crate::library::VIDEO_FILE;
    use crate::tracker::session::SessionConfig;

    const GB: u64 = 1_000_000_000;
    const NOW: i64 = 1_792_080_720_000;

    fn holding(id: &str, days_old: i64, gb: u64, favorite: bool, managed: bool) -> Holding {
        Holding {
            id: id.into(),
            when_ms: NOW - days_old * DAY_MS,
            champion: Some("Shyvana".into()),
            favorite,
            video: (gb > 0).then(|| (PathBuf::from(format!(r"C:\lib\{id}\match.mp4")), gb * GB)),
            managed,
            clip_bytes: GB / 10,
        }
    }

    fn rules(keep_days: u32, max_gb: u64) -> Rules {
        Rules {
            keep_days,
            max_full_bytes: max_gb * GB,
            clips_warn_bytes: 50 * GB,
        }
    }

    fn reasons(removals: &[Removal]) -> Vec<(&str, Reason)> {
        removals.iter().map(|r| (r.id.as_str(), r.reason)).collect()
    }

    #[test]
    fn old_matches_go_first_then_the_oldest_while_over_the_limit() {
        let holdings = [
            holding("new", 1, 3, false, true),
            holding("old", 45, 3, false, true),
            holding("older", 60, 3, false, true),
            holding("mid", 20, 3, false, true),
            holding("recent", 5, 3, false, true),
        ];
        // 30 days: the two past it go. Then 9 GB is over a 7 GB limit: "mid" goes too.
        let plan = plan(&holdings, &rules(30, 7), NOW, &HashSet::new());
        assert_eq!(
            reasons(&plan.removals),
            [
                ("older", Reason::Age),
                ("old", Reason::Age),
                ("mid", Reason::Size)
            ],
            "oldest first"
        );
        assert_eq!((plan.managed_bytes, plan.bytes_after), (15 * GB, 6 * GB));
        assert_eq!(plan.freed_bytes, 9 * GB);

        // No limits: nothing goes.
        let none = super::plan(&holdings, &rules(0, 0), NOW, &HashSet::new());
        assert!(none.removals.is_empty());
        // Only the size limit.
        let size = super::plan(&holdings, &rules(0, 10), NOW, &HashSet::new());
        assert_eq!(
            reasons(&size.removals),
            [("older", Reason::Size), ("old", Reason::Size)]
        );
    }

    #[test]
    fn favorites_imports_and_flagged_videos_are_never_planned() {
        let holdings = [
            holding("favorite", 90, 5, true, true),
            holding("imported", 400, 40, false, false),
            holding("flagged", 80, 5, false, true),
            holding("plain", 70, 5, false, true),
            holding("no-video", 100, 0, false, true),
        ];
        let flagged = HashSet::from([PathBuf::from(r"C:\lib\flagged\match.mp4")]);
        let plan = plan(&holdings, &rules(30, 1), NOW, &flagged);
        assert_eq!(reasons(&plan.removals), [("plain", Reason::Age)]);
        assert_eq!(plan.favorites, 1);
        assert_eq!(
            plan.managed_matches, 3,
            "favorites and flagged count toward the limit"
        );
        assert_eq!((plan.unmanaged_matches, plan.unmanaged_bytes), (1, 40 * GB));
        assert_eq!(
            plan.bytes_after,
            10 * GB,
            "over the limit: what's left can't go"
        );
    }

    #[test]
    fn clips_are_counted_and_warned_about_never_removed() {
        let mut big = holding("clips", 2, 1, false, true);
        big.clip_bytes = 60 * GB;
        let plan = plan(&[big], &rules(30, 150), NOW, &HashSet::new());
        assert!(plan.removals.is_empty());
        assert_eq!(plan.clip_bytes, 60 * GB);
        assert!(plan.clips_over_warning);
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("riftreel-janitor-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn now_ms() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap()
    }

    /// A recorded match `days_old` days before `now`, with a video of `bytes`.
    fn recorded(matches: &Path, id: &str, now: i64, days_old: i64, bytes: usize) -> PathBuf {
        let dir = matches.join(id);
        std::fs::create_dir_all(&dir).unwrap();
        let json = serde_json::json!({
            "schema": 1, "id": id, "startedWallMs": now - days_old * DAY_MS,
            "champion": "Shyvana", "recording": { "file": VIDEO_FILE, "frames": 600, "fps": 60 },
            "clips": [],
        });
        std::fs::write(dir.join(MATCH_FILE), json.to_string()).unwrap();
        std::fs::write(dir.join(VIDEO_FILE), vec![0u8; bytes]).unwrap();
        dir
    }

    fn library(root: &Path) -> Library {
        Library::new(
            root.join("matches"),
            root.join("recordings"),
            SessionConfig::default(),
        )
    }

    /// A Recycle Bin for tests: moves the file into `bin`.
    fn into(bin: &Path) -> impl FnMut(&Path) -> Recycled + '_ {
        std::fs::create_dir_all(bin).unwrap();
        move |path| {
            let name = format!(
                "{}-{}",
                path.parent()
                    .unwrap()
                    .file_name()
                    .unwrap()
                    .to_string_lossy(),
                path.file_name().unwrap().to_string_lossy()
            );
            std::fs::rename(path, bin.join(name)).unwrap();
            Recycled::Done
        }
    }

    /// Every file under `dir`, with its size.
    fn listing(dir: &Path) -> Vec<(PathBuf, u64)> {
        let mut files = Vec::new();
        for path in read_dir(dir) {
            if path.is_dir() {
                files.extend(listing(&path));
            } else {
                files.push((path.clone(), std::fs::metadata(&path).unwrap().len()));
            }
        }
        files.sort();
        files
    }

    const RULES: Rules = Rules {
        keep_days: 30,
        max_full_bytes: 0,
        clips_warn_bytes: 0,
    };

    #[test]
    fn a_run_recycles_the_plan_notes_it_and_keeps_favorites() {
        let root = temp_dir("run");
        let matches = root.join("matches");
        let now = now_ms();
        recorded(&matches, "old", now, 40, 3000);
        recorded(&matches, "older", now, 50, 2000);
        recorded(&matches, "new", now, 2, 1000);
        let favorite = recorded(&matches, "favorite", now, 60, 4000);
        let mut json = read_json(&favorite.join(MATCH_FILE)).unwrap();
        json["favorite"] = true.into();
        std::fs::write(favorite.join(MATCH_FILE), json.to_string()).unwrap();
        let library = library(&root);
        assert!(library.list().iter().all(|card| card.video));

        // The dry run changes nothing.
        let before = listing(&root);
        let overview = library.cleanup_overview(&RULES, now);
        assert_eq!(
            reasons(&overview.plan.removals),
            [("older", Reason::Age), ("old", Reason::Age)]
        );
        assert_eq!(overview.plan.freed_bytes, 5000);
        assert_eq!(overview.plan.favorites, 1);
        assert_eq!(listing(&root), before, "a dry run touches nothing");

        let bin = root.join("bin");
        let report = library.cleanup(&RULES, now, into(&bin), || true);
        assert_eq!(reasons(&report.removed), reasons(&overview.plan.removals));
        assert_eq!(report.freed_bytes, 5000);
        assert!(!report.stopped);
        assert!(bin.join("old-match.mp4").is_file() && bin.join("older-match.mp4").is_file());
        assert!(
            favorite.join(VIDEO_FILE).is_file(),
            "a favorite is never touched"
        );
        assert!(matches.join("new").join(VIDEO_FILE).is_file());

        // The match and the index know.
        let old = library.get("old").unwrap();
        assert_eq!(old["videoRemoved"]["reason"], "age");
        assert_eq!(old["videoRemoved"]["bytes"], 3000);
        assert!(old.get("videoPath").is_none());
        let cards = library.list();
        let video = |id: &str| cards.iter().find(|c| c.id == id).unwrap().video;
        assert!(!video("old") && !video("older") && video("new") && video("favorite"));

        // Nothing left to do; the run is remembered.
        let after = library.cleanup_overview(&RULES, now);
        assert!(after.plan.removals.is_empty());
        let last = after.last_run.unwrap();
        assert_eq!((last.removed, last.freed_bytes, last.kept), (2, 5000, 0));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_video_that_cannot_be_recycled_is_kept_and_flagged() {
        let root = temp_dir("flag");
        let matches = root.join("matches");
        let now = now_ms();
        let big = recorded(&matches, "big", now, 40, 5000);
        recorded(&matches, "busy", now, 45, 1000);
        recorded(&matches, "plain", now, 50, 1000);
        let library = library(&root);
        let bin = root.join("bin");
        let mut into_bin = into(&bin);
        let report = library.cleanup(
            &RULES,
            now,
            |path| {
                if path.starts_with(&big) {
                    Recycled::NotRecyclable
                } else if path.to_string_lossy().contains("busy") {
                    Recycled::Failed("in use".into())
                } else {
                    into_bin(path)
                }
            },
            || true,
        );
        assert_eq!(reasons(&report.removed), [("plain", Reason::Age)]);
        assert_eq!(report.kept.len(), 1);
        assert_eq!(report.kept[0].removal.id, "big");
        assert_eq!(report.failed.len(), 1);
        assert_eq!(report.failed[0].why, "in use");
        assert!(big.join(VIDEO_FILE).is_file(), "never deleted");
        assert!(library.get("big").unwrap().get("videoRemoved").is_none());

        // Flagged: shown for the user, not planned again. The busy one is retried.
        let overview = library.cleanup_overview(&RULES, now);
        assert_eq!(overview.kept.len(), 1);
        assert_eq!(overview.kept[0].removal.path, big.join(VIDEO_FILE));
        assert_eq!(reasons(&overview.plan.removals), [("busy", Reason::Age)]);
        let notes = std::fs::read_to_string(root.join(CLEANUP_FILE)).unwrap();
        assert!(notes.contains("\"big\""), "{notes}");

        // "Try again" puts it back in the plan.
        assert!(library.retry_cleanup("big"));
        assert!(!library.retry_cleanup("big"));
        let again = library.cleanup_overview(&RULES, now);
        assert!(again.kept.is_empty());
        assert_eq!(again.plan.removals.len(), 2);

        // Made a favorite, or removed by the user: no longer flagged.
        library.cleanup(&RULES, now, |_| Recycled::NotRecyclable, || true);
        assert_eq!(library.cleanup_overview(&RULES, now).kept.len(), 2);
        library.set_match_favorite("big", true).unwrap();
        std::fs::remove_file(matches.join("busy").join(VIDEO_FILE)).unwrap();
        assert!(library.cleanup_overview(&RULES, now).kept.is_empty());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_game_starting_stops_the_run() {
        let root = temp_dir("stop");
        let matches = root.join("matches");
        let now = now_ms();
        for (i, id) in ["a", "b", "c"].into_iter().enumerate() {
            recorded(&matches, id, now, 40 + i as i64, 100);
        }
        let library = library(&root);
        let mut asked = 0;
        let report = library.cleanup(&RULES, now, into(&root.join("bin")), || {
            asked += 1;
            asked < 2
        });
        assert_eq!(report.removed.len(), 1);
        assert!(report.stopped);
        assert_eq!(library.cleanup_overview(&RULES, now).plan.removals.len(), 2);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn stray_thumbnails_and_empty_folders_are_cleared_once_old() {
        let root = temp_dir("tidy");
        let matches = root.join("matches");
        let now = now_ms();
        let dir = recorded(&matches, "m1", now, 1, 100);
        let thumbs = dir.join(THUMBS_DIR);
        std::fs::create_dir_all(&thumbs).unwrap();
        for name in ["match.jpg", "0001.jpg", "0002.jpg", "notes.txt"] {
            std::fs::write(thumbs.join(name), b"x").unwrap();
        }
        let mut json = read_json(&dir.join(MATCH_FILE)).unwrap();
        json["clips"] =
            serde_json::json!([{ "file": "clips/0001_kill.mp4", "thumbnail": "thumbs/0001.jpg" }]);
        std::fs::write(dir.join(MATCH_FILE), json.to_string()).unwrap();
        let empty = matches.join("2026-10-15_16-12-00");
        std::fs::create_dir_all(empty.join(CLIPS_DIR)).unwrap();
        let busy = matches.join("2026-10-15_17-00-00");
        std::fs::create_dir_all(&busy).unwrap();
        std::fs::write(busy.join("match.mp4"), b"recording").unwrap();
        let library = library(&root);

        // Just made: left alone.
        let young = library.cleanup_overview(&RULES, now);
        assert_eq!((young.thumbnails, young.folders), (0, 0));

        // Two hours on: the stray picture goes to the bin and the empty folder goes.
        let later = now + 2 * 3_600_000;
        let overview = library.cleanup_overview(&RULES, later);
        assert_eq!((overview.thumbnails, overview.folders), (1, 1));
        let bin = root.join("bin");
        let report = library.cleanup(&RULES, later, into(&bin), || true);
        assert_eq!((report.thumbnails, report.folders), (1, 1));
        assert!(bin.join("thumbs-0002.jpg").is_file());
        assert!(
            thumbs.join("match.jpg").is_file(),
            "the match's own picture"
        );
        assert!(thumbs.join("0001.jpg").is_file(), "a listed clip's");
        assert!(
            thumbs.join("notes.txt").is_file(),
            "not a picture RiftReel made"
        );
        assert!(!empty.exists());
        assert!(
            busy.join("match.mp4").is_file(),
            "a folder with a file isn't empty"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn imported_videos_are_left_alone() {
        let root = temp_dir("imported");
        let matches = root.join("matches");
        let now = now_ms();
        let dir = recorded(&matches, "imported", now, 400, 100);
        let mut json = read_json(&dir.join(MATCH_FILE)).unwrap();
        json["source"] = OUTPLAYED_SOURCE.into();
        std::fs::write(dir.join(MATCH_FILE), json.to_string()).unwrap();
        let library = library(&root);
        let report = library.cleanup(&RULES, now, |_| panic!("nothing to recycle"), || true);
        assert!(report.removed.is_empty());
        let overview = library.cleanup_overview(&RULES, now);
        assert_eq!(
            (
                overview.plan.unmanaged_matches,
                overview.plan.unmanaged_bytes
            ),
            (1, 100)
        );
        std::fs::remove_dir_all(&root).ok();
    }
}
