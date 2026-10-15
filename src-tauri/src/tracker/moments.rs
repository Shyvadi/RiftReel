//! Merging marker windows into moments (doc 02 §B4, doc 03 §4.3).

use serde::Serialize;

use super::normalize::{Marker, MarkerLabel};

/// Default cap on a moment's length (doc 03 §4.3).
pub const DEFAULT_MAX_MOMENT_S: f64 = 60.0;

/// A stretch of the match worth a clip: one or more overlapping marker windows.
/// Times are game seconds.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Moment {
    pub start_s: f64,
    pub end_s: f64,
    /// The highest-priority label among the moment's markers.
    pub label: MarkerLabel,
    pub marker_ids: Vec<u32>,
}

impl Moment {
    pub fn duration_s(&self) -> f64 {
        self.end_s - self.start_s
    }
}

/// Merges overlapping or touching windows into moments, each labelled by its
/// highest-priority marker.
///
/// A moment is capped at `max_len_s`. A marker that would stretch it past the cap starts
/// a new moment with its full window, so a long fight becomes consecutive clips that
/// may overlap slightly; every clip keeps its pre-roll. A single marker longer than the
/// cap (from a user's custom pre/post-roll) is kept whole.
pub fn build_moments(markers: &[Marker], max_len_s: f64) -> Vec<Moment> {
    let mut windows: Vec<(f64, f64, &Marker)> = markers
        .iter()
        .map(|m| (m.start_s().max(0.0), m.end_s(), m))
        .collect();
    windows.sort_by(|a, b| {
        a.0.total_cmp(&b.0)
            .then(a.1.total_cmp(&b.1))
            .then(a.2.id.cmp(&b.2.id))
    });

    let mut moments: Vec<Moment> = Vec::new();
    for (start, end, marker) in windows {
        if let Some(current) = moments.last_mut() {
            let overlaps = start <= current.end_s;
            let merged_len = end.max(current.end_s) - current.start_s;
            if overlaps && merged_len <= max_len_s {
                current.end_s = current.end_s.max(end);
                current.marker_ids.push(marker.id);
                if marker.label.priority() > current.label.priority() {
                    current.label = marker.label;
                }
                continue;
            }
        }
        moments.push(Moment {
            start_s: start,
            end_s: end,
            label: marker.label,
            marker_ids: vec![marker.id],
        });
    }
    moments
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tracker::normalize::MarkerKind;

    fn marker(id: u32, label: MarkerLabel, time: f64, pre: f64, post: f64) -> Marker {
        Marker {
            id,
            kind: MarkerKind::Kill,
            label,
            game_time_s: time,
            video_time_s: None,
            pre_s: pre,
            post_s: post,
            stolen: false,
            by_team: false,
            riot_event_ids: vec![u64::from(id)],
        }
    }

    #[test]
    fn overlapping_windows_merge_under_the_best_label() {
        let markers = [
            marker(1, MarkerLabel::Assist, 100.0, 12.0, 8.0), // 88..108
            marker(2, MarkerLabel::DoubleKill, 104.0, 12.0, 8.0), // 92..112
            marker(3, MarkerLabel::Death, 110.0, 12.0, 3.0),  // 98..113
            marker(4, MarkerLabel::Kill, 300.0, 12.0, 8.0),   // separate
        ];
        let moments = build_moments(&markers, DEFAULT_MAX_MOMENT_S);
        assert_eq!(moments.len(), 2);
        assert_eq!((moments[0].start_s, moments[0].end_s), (88.0, 113.0));
        assert_eq!(moments[0].label, MarkerLabel::DoubleKill);
        assert_eq!(moments[0].marker_ids, [1, 2, 3]);
        assert_eq!(moments[1].label, MarkerLabel::Kill);
    }

    #[test]
    fn touching_windows_merge_and_gaps_split() {
        let touching = [
            marker(1, MarkerLabel::Kill, 100.0, 10.0, 5.0),
            marker(2, MarkerLabel::Assist, 115.0, 10.0, 5.0),
        ];
        assert_eq!(build_moments(&touching, 60.0).len(), 1);
        let apart = [
            marker(1, MarkerLabel::Kill, 100.0, 10.0, 5.0),
            marker(2, MarkerLabel::Assist, 115.1, 10.0, 5.0),
        ];
        assert_eq!(build_moments(&apart, 60.0).len(), 2);
    }

    #[test]
    fn input_order_does_not_matter_and_start_clamps_to_zero() {
        let markers = [
            marker(2, MarkerLabel::Kill, 50.0, 12.0, 8.0),
            marker(1, MarkerLabel::Death, 5.0, 12.0, 3.0),
        ];
        let moments = build_moments(&markers, 60.0);
        assert_eq!(moments[0].start_s, 0.0);
        assert_eq!(moments[0].marker_ids, [1]);
        assert_eq!(moments[1].marker_ids, [2]);
    }

    #[test]
    fn long_chains_are_split_at_the_cap() {
        // Assists every 10 s from 1200 to 1300: windows chain from 1188 to 1308.
        let markers: Vec<_> = (0..11)
            .map(|i| {
                marker(
                    i + 1,
                    MarkerLabel::Assist,
                    1200.0 + 10.0 * f64::from(i),
                    12.0,
                    8.0,
                )
            })
            .collect();
        let moments = build_moments(&markers, 60.0);
        let spans: Vec<_> = moments
            .iter()
            .map(|m| (m.start_s, m.end_s, m.marker_ids.len()))
            .collect();
        assert_eq!(
            spans,
            [
                (1188.0, 1248.0, 5),
                (1238.0, 1298.0, 5),
                (1288.0, 1308.0, 1)
            ]
        );
        assert!(moments.iter().all(|m| m.duration_s() <= 60.0));
    }

    #[test]
    fn a_single_oversized_window_is_kept_whole() {
        let markers = [marker(1, MarkerLabel::Kill, 100.0, 50.0, 30.0)];
        let moments = build_moments(&markers, 60.0);
        assert_eq!((moments[0].start_s, moments[0].end_s), (50.0, 130.0));
    }
}
