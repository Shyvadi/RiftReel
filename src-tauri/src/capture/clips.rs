//! Highlight clips (doc 03 §6–7): each moment cut from the match video with a stream copy
//! into `clips/NNNN_<label>.mp4`, with a JPEG thumbnail in `thumbs/NNNN.jpg`.
//!
//! A stream copy can only start on a keyframe. Recordings have one every second, so a
//! clip begins up to a second before its moment's pre-roll and nothing is re-encoded:
//! cutting a match's clips reads only their own bytes.

use std::ffi::OsString;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::tracker::moments::Moment;
use crate::tracker::normalize::Marker;

pub const CLIPS_DIR: &str = "clips";
pub const THUMBS_DIR: &str = "thumbs";
/// The label of a clip saved with the Save replay hotkey.
pub const REPLAY_LABEL: &str = "replay";
/// Shorter spans aren't worth a file.
const MIN_CLIP_S: f64 = 1.0;
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
    /// The moment's label, or `replay` for a Save replay.
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
    /// The frame the thumbnail shows (the moment's main event).
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

/// The keyframe a stream copy starting at `s` really starts on.
pub fn keyframe_at_or_before(s: f64, keyframe_s: f64) -> f64 {
    if keyframe_s <= 0.0 {
        return s.max(0.0);
    }
    ((s.max(0.0) / keyframe_s) + 1e-9).floor() * keyframe_s
}

/// A span of the video as a clip numbered `index`: clamped to the video, its start moved
/// back to a keyframe, the thumbnail at `main_s`. `None` if too little of it was recorded.
pub fn plan_span(
    index: usize,
    label: &str,
    span: (f64, f64),
    main_s: f64,
    video_s: f64,
    keyframe_s: f64,
) -> Option<ClipPlan> {
    let to_s = span.1.min(video_s);
    let from_s = keyframe_at_or_before(span.0, keyframe_s);
    if to_s - from_s < MIN_CLIP_S || to_s <= 0.0 {
        return None;
    }
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

/// The clips for a match's moments, in video time: from each moment's earliest pre-roll
/// to its latest post-roll (its markers' video times), within the `video_s` recorded.
/// Moments the video doesn't cover, or with markers that couldn't be placed on it, are
/// left out.
pub fn plan_clips(
    moments: &[Moment],
    markers: &[Marker],
    video_s: f64,
    keyframe_s: f64,
) -> Vec<ClipPlan> {
    let mut plans = Vec::new();
    for moment in moments {
        let members: Vec<&Marker> = moment
            .marker_ids
            .iter()
            .filter_map(|id| markers.iter().find(|m| m.id == *id))
            .collect();
        let Some(times) = members
            .iter()
            .map(|m| {
                m.video_time_s
                    .map(|t| (t - m.pre_s, t + m.post_s, t, m.label))
            })
            .collect::<Option<Vec<_>>>()
        else {
            continue;
        };
        if times.is_empty() {
            continue;
        }
        let from = times.iter().map(|t| t.0).fold(f64::INFINITY, f64::min);
        let to = times.iter().map(|t| t.1).fold(f64::NEG_INFINITY, f64::max);
        // The thumbnail shows the event the moment is named after.
        let main = times
            .iter()
            .find(|t| t.3 == moment.label)
            .map_or(times[0].2, |t| t.2);
        if let Some(plan) = plan_span(
            plans.len() + 1,
            moment.label.as_str(),
            (from, to),
            main,
            video_s,
            keyframe_s,
        ) {
            plans.push(plan);
        }
    }
    plans
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
    use crate::tracker::normalize::{MarkerKind, MarkerLabel};

    fn marker(id: u32, label: MarkerLabel, video: Option<f64>, pre: f64, post: f64) -> Marker {
        Marker {
            id,
            kind: MarkerKind::Kill,
            label,
            game_time_s: video.unwrap_or(0.0) - 40.0,
            video_time_s: video,
            pre_s: pre,
            post_s: post,
            stolen: false,
            by_team: false,
            riot_event_ids: vec![u64::from(id)],
        }
    }

    fn moment(label: MarkerLabel, ids: &[u32]) -> Moment {
        Moment {
            start_s: 0.0,
            end_s: 0.0,
            label,
            marker_ids: ids.to_vec(),
        }
    }

    fn text(args: &[OsString]) -> Vec<String> {
        args.iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn a_moment_spans_its_markers_pre_and_post_roll_from_a_keyframe() {
        let markers = [
            marker(1, MarkerLabel::Kill, Some(100.4), 12.0, 8.0),
            marker(2, MarkerLabel::DoubleKill, Some(104.0), 12.0, 8.0),
        ];
        let plans = plan_clips(
            &[moment(MarkerLabel::DoubleKill, &[1, 2])],
            &markers,
            1800.0,
            1.0,
        );
        assert_eq!(plans.len(), 1);
        let plan = &plans[0];
        // Earliest pre-roll 88.4 s: the copy starts on the keyframe at 88 s.
        assert_eq!(plan.from_s, 88.0);
        assert!((plan.to_s - 112.0).abs() < 1e-9, "latest post-roll");
        assert_eq!(plan.thumbnail_at_s, 104.0, "the double kill itself");
        assert_eq!(plan.file, "clips/0001_double_kill.mp4");
        assert_eq!(plan.thumbnail, "thumbs/0001.jpg");
        assert_eq!(plan.label, "double_kill");
        let clip = plan.clip(true);
        assert_eq!(
            (clip.from_s, clip.thumbnail.as_deref(), clip.favorite),
            (88.0, Some("thumbs/0001.jpg"), false)
        );
        assert_eq!(plan.clip(false).thumbnail, None);
    }

    #[test]
    fn clips_stay_within_the_recording() {
        let markers = [
            // Before the recording began (it started during the loading screen).
            marker(1, MarkerLabel::Kill, Some(5.0), 12.0, 8.0),
            // The victory, its post-roll past the end of the video.
            marker(2, MarkerLabel::Victory, Some(1795.0), 10.0, 10.0),
            // Never placed on the video (no clock sync).
            marker(3, MarkerLabel::Assist, None, 12.0, 8.0),
            // Entirely after the video ended.
            marker(4, MarkerLabel::Kill, Some(1900.0), 12.0, 8.0),
        ];
        let moments = [
            moment(MarkerLabel::Kill, &[1]),
            moment(MarkerLabel::Victory, &[2]),
            moment(MarkerLabel::Assist, &[3]),
            moment(MarkerLabel::Kill, &[4]),
        ];
        let plans = plan_clips(&moments, &markers, 1800.0, 1.0);
        assert_eq!(plans.len(), 2, "{plans:?}");
        assert_eq!((plans[0].from_s, plans[0].to_s), (0.0, 13.0));
        assert_eq!(plans[1].file, "clips/0002_victory.mp4", "numbered as cut");
        assert_eq!((plans[1].from_s, plans[1].to_s), (1785.0, 1800.0));
        assert!(plans[1].thumbnail_at_s < 1800.0);
    }

    #[test]
    fn keyframes() {
        assert_eq!(keyframe_at_or_before(88.4, 1.0), 88.0);
        assert_eq!(keyframe_at_or_before(88.0, 1.0), 88.0);
        assert_eq!(keyframe_at_or_before(-3.0, 1.0), 0.0);
        assert_eq!(keyframe_at_or_before(5.0, 2.0), 4.0);
        assert!(
            plan_span(1, "replay", (10.0, 10.5), 10.2, 60.0, 1.0).is_none(),
            "too short"
        );
        let replay = plan_span(7, REPLAY_LABEL, (40.3, 60.3), 60.3, 60.0, 1.0).unwrap();
        assert_eq!((replay.from_s, replay.to_s), (40.0, 60.0));
        assert_eq!(replay.file, "clips/0007_replay.mp4");
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
