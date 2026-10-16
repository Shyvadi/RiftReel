//! Clips (doc 03 §7, §9), made in the review after the game: a span of the recorded match
//! cut out of `match.mp4` with a stream copy into `clips/NNNN_<label>.mp4`, with a JPEG
//! thumbnail in `thumbs/NNNN.jpg`. Nothing is cut during the game, and nothing until the
//! user asks: a highlight plays straight from the recording.
//!
//! A stream copy can only start on a keyframe. Recordings have one every second, so a
//! clip begins up to a second before the span asked for, and nothing is re-encoded:
//! cutting reads only the clip's own bytes.

use std::ffi::OsString;
use std::path::Path;

use serde::{Deserialize, Serialize};

pub const CLIPS_DIR: &str = "clips";
pub const THUMBS_DIR: &str = "thumbs";
/// Seconds between keyframes in a recording (`-g` is the frame rate).
pub const KEYFRAME_S: f64 = 1.0;
/// The label of a clip that isn't a highlight's.
pub const DEFAULT_LABEL: &str = "clip";
/// Shorter spans aren't worth a file.
const MIN_CLIP_S: f64 = 1.0;
const MAX_LABEL_LEN: usize = 40;
/// Thumbnails are this wide (height follows the video's shape).
const THUMBNAIL_WIDTH: u32 = 480;

/// One entry of `match.json`'s `clips`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Clip {
    /// The clip's file, inside the match folder (`clips/0001_double_kill.mp4`).
    pub file: String,
    /// Its span on the match's video timeline, in seconds; it starts on the keyframe at
    /// `from_s`.
    pub from_s: f64,
    pub to_s: f64,
    /// The highlight's label (`double_kill`), or `clip`.
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumbnail: Option<String>,
    #[serde(default)]
    pub favorite: bool,
}

/// One clip to cut.
#[derive(Debug, Clone, PartialEq)]
pub struct ClipPlan {
    pub from_s: f64,
    pub to_s: f64,
    /// The frame the thumbnail shows (a highlight's main event).
    pub thumbnail_at_s: f64,
    pub label: String,
    /// Inside the match folder.
    pub file: String,
    pub thumbnail: String,
}

impl ClipPlan {
    /// The clip as `match.json` lists it, once cut.
    pub fn clip(&self, has_thumbnail: bool) -> Clip {
        Clip {
            file: self.file.clone(),
            from_s: self.from_s,
            to_s: self.to_s,
            label: self.label.clone(),
            thumbnail: has_thumbnail.then(|| self.thumbnail.clone()),
            favorite: false,
        }
    }
}

/// The files for clip number `index` (from 1).
pub fn clip_paths(index: usize, label: &str) -> (String, String) {
    (
        format!("{CLIPS_DIR}/{index:04}_{label}.mp4"),
        format!("{THUMBS_DIR}/{index:04}.jpg"),
    )
}

/// A label fit for a file name: lowercase letters, digits and `_`; `clip` if nothing is
/// left.
pub fn clip_label(label: &str) -> String {
    let mut slug = String::new();
    for c in label.trim().chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.ends_with('_') {
            slug.push('_');
        }
    }
    let slug: String = slug.trim_matches('_').chars().take(MAX_LABEL_LEN).collect();
    let slug = slug.trim_end_matches('_');
    if slug.is_empty() {
        DEFAULT_LABEL.to_owned()
    } else {
        slug.to_owned()
    }
}

/// The keyframe a stream copy starting at `s` really starts on.
pub fn keyframe_at_or_before(s: f64, keyframe_s: f64) -> f64 {
    if keyframe_s <= 0.0 {
        return s.max(0.0);
    }
    ((s.max(0.0) / keyframe_s) + 1e-9).floor() * keyframe_s
}

/// A span of the video as clip number `index`: clamped to the `video_s` recorded, its
/// start moved back to a keyframe, the thumbnail at `main_s`. `None` if too little of it
/// was recorded.
pub fn plan_span(
    index: usize,
    label: &str,
    span: (f64, f64),
    main_s: f64,
    video_s: f64,
    keyframe_s: f64,
) -> Option<ClipPlan> {
    if !(span.0.is_finite() && span.1.is_finite()) {
        return None;
    }
    let to_s = span.1.min(video_s);
    let from_s = keyframe_at_or_before(span.0, keyframe_s);
    if to_s - from_s < MIN_CLIP_S || to_s <= 0.0 {
        return None;
    }
    let main_s = if main_s.is_finite() {
        main_s
    } else {
        (from_s + to_s) / 2.0
    };
    let (file, thumbnail) = clip_paths(index, label);
    Some(ClipPlan {
        from_s,
        to_s,
        thumbnail_at_s: main_s.clamp(from_s, (to_s - 0.05).max(from_s)),
        label: label.to_owned(),
        file,
        thumbnail,
    })
}

fn args<const N: usize>(items: [&str; N]) -> Vec<OsString> {
    items.into_iter().map(OsString::from).collect()
}

/// Cuts `[from_s, to_s]` out of `video` with a stream copy (every track). The output
/// starts at 0 and has its index up front, ready to share.
pub fn cut_args(video: &Path, from_s: f64, to_s: f64, out: &Path) -> Vec<OsString> {
    let mut out_args = args(["-hide_banner", "-nostdin", "-loglevel", "error", "-ss"]);
    out_args.push(format!("{from_s:.3}").into());
    out_args.extend(args(["-to"]));
    out_args.push(format!("{to_s:.3}").into());
    out_args.extend(args(["-i"]));
    out_args.push(video.into());
    out_args.extend(args([
        "-map",
        "0",
        "-c",
        "copy",
        "-avoid_negative_ts",
        "make_zero",
        "-movflags",
        "+faststart",
        "-f",
        "mp4",
        "-n",
    ]));
    out_args.push(out.into());
    out_args
}

/// One frame of `video` at `at_s` as a small JPEG.
pub fn thumbnail_args(video: &Path, at_s: f64, out: &Path) -> Vec<OsString> {
    let mut out_args = args(["-hide_banner", "-nostdin", "-loglevel", "error", "-ss"]);
    out_args.push(format!("{at_s:.3}").into());
    out_args.extend(args(["-i"]));
    out_args.push(video.into());
    out_args.extend(args(["-frames:v", "1", "-vf"]));
    out_args.push(format!("scale={THUMBNAIL_WIDTH}:-2").into());
    out_args.extend(args(["-q:v", "4", "-f", "image2", "-update", "1", "-n"]));
    out_args.push(out.into());
    out_args
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(args: &[OsString]) -> Vec<String> {
        args.iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn a_span_starts_on_the_keyframe_before_it() {
        // A double kill's window: 12 s before the first kill, 8 s after the second.
        let plan = plan_span(1, "double_kill", (88.4, 112.0), 104.0, 1800.0, KEYFRAME_S).unwrap();
        assert_eq!(plan.from_s, 88.0, "the copy starts on the keyframe at 88 s");
        assert_eq!(plan.to_s, 112.0);
        assert_eq!(plan.thumbnail_at_s, 104.0, "the double kill itself");
        assert_eq!(plan.file, "clips/0001_double_kill.mp4");
        assert_eq!(plan.thumbnail, "thumbs/0001.jpg");
        let clip = plan.clip(true);
        assert_eq!(
            (clip.from_s, clip.thumbnail.as_deref(), clip.favorite),
            (88.0, Some("thumbs/0001.jpg"), false)
        );
        assert_eq!(plan.clip(false).thumbnail, None);
    }

    #[test]
    fn spans_stay_within_the_recording() {
        // Before the recording began (it started during the loading screen).
        let early = plan_span(1, "kill", (-7.0, 13.0), 5.0, 1800.0, KEYFRAME_S).unwrap();
        assert_eq!((early.from_s, early.to_s), (0.0, 13.0));
        // The victory, its post-roll past the end of the video.
        let late = plan_span(2, "victory", (1785.0, 1806.0), 1795.0, 1800.0, KEYFRAME_S).unwrap();
        assert_eq!((late.from_s, late.to_s), (1785.0, 1800.0));
        assert!(late.thumbnail_at_s < 1800.0);
        // Entirely after the video ended, or too short to keep.
        assert!(plan_span(3, "kill", (1888.0, 1908.0), 1900.0, 1800.0, KEYFRAME_S).is_none());
        assert!(plan_span(4, "clip", (10.0, 10.5), 10.2, 60.0, KEYFRAME_S).is_none());
        // Nonsense from the window is refused, not clamped into a clip.
        assert!(plan_span(5, "clip", (f64::NAN, 10.5), 10.2, 60.0, KEYFRAME_S).is_none());
        assert!(plan_span(5, "clip", (2.0, f64::INFINITY), 10.2, 60.0, KEYFRAME_S).is_none());
        let odd = plan_span(5, "clip", (2.0, 10.0), f64::NAN, 60.0, KEYFRAME_S).unwrap();
        assert_eq!(
            odd.thumbnail_at_s, 6.0,
            "the middle, when the frame asked for is nonsense"
        );
    }

    #[test]
    fn keyframes() {
        assert_eq!(keyframe_at_or_before(88.4, 1.0), 88.0);
        assert_eq!(keyframe_at_or_before(88.0, 1.0), 88.0);
        assert_eq!(keyframe_at_or_before(-3.0, 1.0), 0.0);
        assert_eq!(keyframe_at_or_before(5.0, 2.0), 4.0);
    }

    #[test]
    fn labels_are_made_safe_for_file_names() {
        assert_eq!(clip_label("double_kill"), "double_kill");
        assert_eq!(clip_label("  Baron steal!! "), "baron_steal");
        assert_eq!(clip_label("../../evil"), "evil");
        assert_eq!(clip_label(""), "clip");
        assert_eq!(clip_label("***"), "clip");
        assert_eq!(clip_label(&"a".repeat(80)).len(), 40);
    }

    #[test]
    fn cut_and_thumbnail_command_lines() {
        let video = Path::new(r"C:\m\match.mp4");
        let args = text(&cut_args(
            video,
            88.0,
            112.0,
            Path::new(r"C:\m\clips\0001_kill.mp4"),
        ));
        assert_eq!(
            args,
            [
                "-hide_banner",
                "-nostdin",
                "-loglevel",
                "error",
                "-ss",
                "88.000",
                "-to",
                "112.000",
                "-i",
                r"C:\m\match.mp4",
                "-map",
                "0",
                "-c",
                "copy",
                "-avoid_negative_ts",
                "make_zero",
                "-movflags",
                "+faststart",
                "-f",
                "mp4",
                "-n",
                r"C:\m\clips\0001_kill.mp4"
            ]
        );
        let thumb = text(&thumbnail_args(
            video,
            104.0,
            Path::new(r"C:\m\thumbs\0001.jpg"),
        ));
        assert!(thumb.windows(2).any(|w| w == ["-ss", "104.000"]));
        assert!(thumb.windows(2).any(|w| w == ["-frames:v", "1"]));
        assert!(thumb.windows(2).any(|w| w == ["-vf", "scale=480:-2"]));
        assert_eq!(thumb.last().unwrap(), r"C:\m\thumbs\0001.jpg");
    }

    #[test]
    fn clips_round_trip_through_match_json() {
        let clip = Clip {
            file: "clips/0001_kill.mp4".into(),
            from_s: 88.0,
            to_s: 112.0,
            label: "kill".into(),
            thumbnail: Some("thumbs/0001.jpg".into()),
            favorite: true,
        };
        let json = serde_json::to_value(&clip).unwrap();
        assert_eq!(json["fromS"], 88.0);
        assert_eq!(json["thumbnail"], "thumbs/0001.jpg");
        assert_eq!(serde_json::from_value::<Clip>(json).unwrap(), clip);
        // Older or hand-edited entries: no thumbnail, no favorite flag.
        let bare: Clip = serde_json::from_str(
            r#"{"file":"clips/0002_death.mp4","fromS":1,"toS":5,"label":"death"}"#,
        )
        .unwrap();
        assert_eq!((bare.thumbnail, bare.favorite), (None, false));
    }
}
