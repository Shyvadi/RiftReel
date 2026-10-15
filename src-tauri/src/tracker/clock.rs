//! Aligning Riot's game clock with the wall clock (doc 02 §B4, doc 03 §4.4).
//!
//! `EventTime` is seconds of game time. Every ~10 s the tracker samples
//! `gamestats.gameTime` together with a monotonic timestamp (`Instant`, which is QPC on
//! Windows, the same clock capture will use). A marker's video time is then
//! `wall(EventTime) - recording_start`.

use std::time::{Duration, Instant};

/// How often to sample `gamestats.gameTime` during a match.
pub const SAMPLE_INTERVAL: Duration = Duration::from_secs(10);

/// Readings whose request took longer than this are too imprecise to keep.
pub const MAX_SAMPLE_RTT: Duration = Duration::from_millis(250);

/// Enough for ~5.5 h at one sample per 10 s; the oldest are dropped beyond that.
const MAX_SAMPLES: usize = 2000;

/// One reading: at wall-clock `wall`, the game clock read `game_time`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClockSample {
    pub game_time: f64,
    pub wall: Instant,
}

/// Maps game time to wall-clock time.
///
/// A game-time point maps through the latest sample taken at or before it, assuming
/// game time runs at 1 s per second from there. A pause (custom/tournament games)
/// then only affects events until the next sample re-anchors the clock. Before the
/// first sample there's no mapping; after an API outage the last good sample keeps
/// being used.
#[derive(Debug, Clone, Default)]
pub struct GameClock {
    /// Sorted by `game_time`.
    samples: Vec<ClockSample>,
}

impl GameClock {
    pub fn new() -> Self {
        Self::default()
    }

    /// Records a `gamestats` reading taken between `sent` and `received`, stamped at the
    /// midpoint. Returns `false` if the round trip was too slow to trust.
    pub fn record(&mut self, game_time: f64, sent: Instant, received: Instant) -> bool {
        let rtt = received.saturating_duration_since(sent);
        if rtt > MAX_SAMPLE_RTT || !game_time.is_finite() || game_time < 0.0 {
            return false;
        }
        self.record_sample(ClockSample {
            game_time,
            wall: sent + rtt / 2,
        });
        true
    }

    /// Records an exact sample. When the game time repeats (a paused game), the later
    /// reading wins, since it anchors what happens after the pause.
    pub fn record_sample(&mut self, sample: ClockSample) {
        let index = self
            .samples
            .partition_point(|s| s.game_time < sample.game_time);
        match self.samples.get_mut(index) {
            Some(existing) if existing.game_time == sample.game_time => {
                if sample.wall > existing.wall {
                    *existing = sample;
                }
            }
            _ => self.samples.insert(index, sample),
        }
        if self.samples.len() > MAX_SAMPLES {
            self.samples.remove(0);
        }
    }

    pub fn is_synced(&self) -> bool {
        !self.samples.is_empty()
    }

    pub fn samples(&self) -> &[ClockSample] {
        &self.samples
    }

    /// Wall-clock instant of a game-time point.
    pub fn wall_at(&self, game_time: f64) -> Option<Instant> {
        let anchor = match self.samples.partition_point(|s| s.game_time <= game_time) {
            0 => self.samples.first()?,
            after => &self.samples[after - 1],
        };
        let delta = game_time - anchor.game_time;
        if delta >= 0.0 {
            anchor
                .wall
                .checked_add(Duration::try_from_secs_f64(delta).ok()?)
        } else {
            anchor
                .wall
                .checked_sub(Duration::try_from_secs_f64(-delta).ok()?)
        }
    }

    /// Seconds into a recording that started at `recording_start`; negative if the
    /// moment happened before the recording began.
    pub fn video_time(&self, game_time: f64, recording_start: Instant) -> Option<f64> {
        let wall = self.wall_at(game_time)?;
        Some(if wall >= recording_start {
            (wall - recording_start).as_secs_f64()
        } else {
            -(recording_start - wall).as_secs_f64()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(s: f64) -> Duration {
        Duration::from_secs_f64(s)
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-6
    }

    #[test]
    fn no_samples_no_mapping() {
        let clock = GameClock::new();
        assert!(!clock.is_synced());
        assert_eq!(clock.wall_at(10.0), None);
    }

    #[test]
    fn maps_game_time_through_the_latest_sample() {
        let t0 = Instant::now();
        let mut clock = GameClock::new();
        // The game had been running 30 s when we first looked.
        clock.record_sample(ClockSample {
            game_time: 30.0,
            wall: t0,
        });
        clock.record_sample(ClockSample {
            game_time: 40.0,
            wall: t0 + secs(10.0),
        });

        assert_eq!(clock.wall_at(45.0), Some(t0 + secs(15.0)));
        // Before the first sample: extrapolate backwards from it.
        assert_eq!(clock.wall_at(20.0), Some(t0 - secs(10.0)));

        // Recording started 2 s after our first sample.
        let recording = t0 + secs(2.0);
        assert!(close(clock.video_time(45.0, recording).unwrap(), 13.0));
        assert!(close(clock.video_time(31.0, recording).unwrap(), -1.0));
    }

    #[test]
    fn a_pause_shifts_only_later_events() {
        let t0 = Instant::now();
        let mut clock = GameClock::new();
        clock.record_sample(ClockSample {
            game_time: 100.0,
            wall: t0,
        });
        // Game paused for 60 s at game time 105.
        clock.record_sample(ClockSample {
            game_time: 105.0,
            wall: t0 + secs(5.0),
        });
        clock.record_sample(ClockSample {
            game_time: 105.0,
            wall: t0 + secs(65.0),
        });
        clock.record_sample(ClockSample {
            game_time: 110.0,
            wall: t0 + secs(70.0),
        });

        assert_eq!(
            clock.samples().len(),
            3,
            "repeated game time keeps the later reading"
        );
        assert_eq!(clock.wall_at(102.0), Some(t0 + secs(2.0)));
        assert_eq!(clock.wall_at(107.0), Some(t0 + secs(67.0)));
    }

    #[test]
    fn keeps_last_good_offset_across_outages_and_rejects_slow_samples() {
        let t0 = Instant::now();
        let mut clock = GameClock::new();
        assert!(clock.record(200.0, t0, t0 + Duration::from_millis(20)));
        // A sluggish reading during a hiccup is ignored.
        assert!(!clock.record(260.0, t0 + secs(60.0), t0 + secs(61.0)));
        assert_eq!(clock.samples().len(), 1);
        // Midpoint stamping: wall = t0 + 10 ms.
        assert_eq!(clock.wall_at(200.0), Some(t0 + Duration::from_millis(10)));
        // Long after the last sample, the last offset still applies.
        assert_eq!(
            clock.wall_at(500.0),
            Some(t0 + Duration::from_millis(10) + secs(300.0))
        );
    }

    #[test]
    fn out_of_order_samples_are_sorted() {
        let t0 = Instant::now();
        let mut clock = GameClock::new();
        clock.record_sample(ClockSample {
            game_time: 50.0,
            wall: t0 + secs(50.0),
        });
        clock.record_sample(ClockSample {
            game_time: 10.0,
            wall: t0 + secs(10.0),
        });
        clock.record_sample(ClockSample {
            game_time: 30.0,
            wall: t0 + secs(30.5),
        });
        let times: Vec<_> = clock.samples().iter().map(|s| s.game_time).collect();
        assert_eq!(times, [10.0, 30.0, 50.0]);
        assert_eq!(clock.wall_at(35.0), Some(t0 + secs(35.5)));
    }
}
