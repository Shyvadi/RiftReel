//! Riot Live Client events → highlight markers (doc 02 §B2, doc 03 §4.3).
//!
//! Every marker is stamped with Riot's `EventTime`, not the time we received it.
//! Pre/post-roll windows default to Outplayed's observed values; the Downplayed
//! additions (Voidgrubs, first blood, ace, shutdown, sprees) are off by default.

use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};

use super::roster::{PlayerIdentity, Roster, Team};
use super::streaks::{SpreeLevel, StreakTracker};
use crate::league::models::RawEvent;

/// Marker types. Each one has its own on/off switch and pre/post-roll in settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MarkerKind {
    Kill,
    Assist,
    Death,
    TurretKill,
    InhibitorKill,
    DragonKill,
    RiftHerald,
    /// Riot's `HordeKill` event, one per Voidgrub.
    Voidgrubs,
    BaronKill,
    Victory,
    FirstBlood,
    Ace,
    Shutdown,
    Spree,
}

impl MarkerKind {
    pub const ALL: [MarkerKind; 14] = [
        Self::Kill,
        Self::Assist,
        Self::Death,
        Self::TurretKill,
        Self::InhibitorKill,
        Self::DragonKill,
        Self::RiftHerald,
        Self::Voidgrubs,
        Self::BaronKill,
        Self::Victory,
        Self::FirstBlood,
        Self::Ace,
        Self::Shutdown,
        Self::Spree,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Kill => "kill",
            Self::Assist => "assist",
            Self::Death => "death",
            Self::TurretKill => "turret_kill",
            Self::InhibitorKill => "inhibitor_kill",
            Self::DragonKill => "dragon_kill",
            Self::RiftHerald => "rift_herald",
            Self::Voidgrubs => "voidgrubs",
            Self::BaronKill => "baron_kill",
            Self::Victory => "victory",
            Self::FirstBlood => "first_blood",
            Self::Ace => "ace",
            Self::Shutdown => "shutdown",
            Self::Spree => "spree",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.as_str() == value)
    }

    /// Outplayed's windows for its highlight set (doc 02 §A3); Downplayed's additions
    /// are 10 s / 5 s and off by default.
    pub fn default_window(self) -> MarkerWindow {
        match self {
            Self::Kill | Self::Assist => MarkerWindow::on(12.0, 8.0),
            Self::Death => MarkerWindow::on(12.0, 3.0),
            Self::TurretKill
            | Self::InhibitorKill
            | Self::DragonKill
            | Self::RiftHerald
            | Self::BaronKill => MarkerWindow::on(10.0, 5.0),
            Self::Victory => MarkerWindow::on(10.0, 1.0),
            Self::Voidgrubs | Self::FirstBlood | Self::Ace | Self::Shutdown | Self::Spree => {
                MarkerWindow::off(10.0, 5.0)
            }
        }
    }
}

/// What a marker or moment is called. A kill marker is relabelled when a `Multikill`
/// follows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MarkerLabel {
    PentaKill,
    QuadraKill,
    TripleKill,
    DoubleKill,
    Ace,
    Shutdown,
    Legendary,
    Godlike,
    Dominating,
    Unstoppable,
    Rampage,
    KillingSpree,
    FirstBlood,
    Kill,
    Victory,
    BaronKill,
    ElderDragon,
    DragonKill,
    RiftHerald,
    Voidgrubs,
    InhibitorKill,
    TurretKill,
    Assist,
    Death,
}

impl MarkerLabel {
    /// Moment labelling priority (doc 02 §B4): penta > quadra > triple > double > kill >
    /// objective > assist > death. The opt-in extras rank between multikills and kills,
    /// victory ranks between kills and objectives, and Voidgrubs just below the Herald.
    pub fn priority(self) -> u8 {
        match self {
            Self::PentaKill => 100,
            Self::QuadraKill => 95,
            Self::TripleKill => 90,
            Self::DoubleKill => 85,
            Self::Ace => 80,
            Self::Shutdown => 78,
            Self::Legendary => 76,
            Self::Godlike => 75,
            Self::Dominating => 74,
            Self::Unstoppable => 73,
            Self::Rampage => 72,
            Self::KillingSpree => 71,
            Self::FirstBlood => 70,
            Self::Kill => 60,
            Self::Victory => 55,
            Self::BaronKill => 50,
            Self::ElderDragon => 48,
            Self::DragonKill => 46,
            Self::RiftHerald => 44,
            Self::Voidgrubs => 43,
            Self::InhibitorKill => 42,
            Self::TurretKill => 40,
            Self::Assist => 30,
            Self::Death => 20,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::PentaKill => "penta_kill",
            Self::QuadraKill => "quadra_kill",
            Self::TripleKill => "triple_kill",
            Self::DoubleKill => "double_kill",
            Self::Ace => "ace",
            Self::Shutdown => "shutdown",
            Self::Legendary => "legendary",
            Self::Godlike => "godlike",
            Self::Dominating => "dominating",
            Self::Unstoppable => "unstoppable",
            Self::Rampage => "rampage",
            Self::KillingSpree => "killing_spree",
            Self::FirstBlood => "first_blood",
            Self::Kill => "kill",
            Self::Victory => "victory",
            Self::BaronKill => "baron_kill",
            Self::ElderDragon => "elder_dragon",
            Self::DragonKill => "dragon_kill",
            Self::RiftHerald => "rift_herald",
            Self::Voidgrubs => "voidgrubs",
            Self::InhibitorKill => "inhibitor_kill",
            Self::TurretKill => "turret_kill",
            Self::Assist => "assist",
            Self::Death => "death",
        }
    }

    /// `KillStreak` 2–5 from a `Multikill` event (anything above 5 counts as a penta).
    pub fn multikill(kill_streak: u32) -> Option<Self> {
        match kill_streak {
            2 => Some(Self::DoubleKill),
            3 => Some(Self::TripleKill),
            4 => Some(Self::QuadraKill),
            n if n >= 5 => Some(Self::PentaKill),
            _ => None,
        }
    }
}

impl From<SpreeLevel> for MarkerLabel {
    fn from(level: SpreeLevel) -> Self {
        match level {
            SpreeLevel::KillingSpree => Self::KillingSpree,
            SpreeLevel::Rampage => Self::Rampage,
            SpreeLevel::Unstoppable => Self::Unstoppable,
            SpreeLevel::Dominating => Self::Dominating,
            SpreeLevel::Godlike => Self::Godlike,
            SpreeLevel::Legendary => Self::Legendary,
        }
    }
}

/// On/off and pre/post-roll for one marker kind, in seconds.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MarkerWindow {
    pub enabled: bool,
    pub pre_s: f64,
    pub post_s: f64,
}

impl MarkerWindow {
    pub const fn on(pre_s: f64, post_s: f64) -> Self {
        Self {
            enabled: true,
            pre_s,
            post_s,
        }
    }

    pub const fn off(pre_s: f64, post_s: f64) -> Self {
        Self {
            enabled: false,
            pre_s,
            post_s,
        }
    }
}

/// Marker settings: a window per kind, plus whether teammates' objectives count.
#[derive(Debug, Clone, PartialEq)]
pub struct MarkerConfig {
    windows: BTreeMap<MarkerKind, MarkerWindow>,
    /// Also mark objectives and aces your team took, flagged `by_team`. Off by default,
    /// which matches Outplayed: only the local player's own objectives.
    pub include_team: bool,
}

impl Default for MarkerConfig {
    fn default() -> Self {
        Self {
            windows: MarkerKind::ALL
                .into_iter()
                .map(|k| (k, k.default_window()))
                .collect(),
            include_team: false,
        }
    }
}

impl MarkerConfig {
    pub fn window(&self, kind: MarkerKind) -> MarkerWindow {
        self.windows
            .get(&kind)
            .copied()
            .unwrap_or_else(|| kind.default_window())
    }

    pub fn set_window(&mut self, kind: MarkerKind, window: MarkerWindow) {
        self.windows.insert(kind, window);
    }

    pub fn set_enabled(&mut self, kind: MarkerKind, enabled: bool) {
        let mut window = self.window(kind);
        window.enabled = enabled;
        self.windows.insert(kind, window);
    }

    /// Turns on the Downplayed additions: Voidgrubs, first blood, ace, shutdown and
    /// spree markers.
    pub fn with_extras(mut self) -> Self {
        for kind in [
            MarkerKind::Voidgrubs,
            MarkerKind::FirstBlood,
            MarkerKind::Ace,
            MarkerKind::Shutdown,
            MarkerKind::Spree,
        ] {
            self.set_enabled(kind, true);
        }
        self
    }

    pub fn windows(&self) -> impl Iterator<Item = (MarkerKind, MarkerWindow)> + '_ {
        MarkerKind::ALL.into_iter().map(|k| (k, self.window(k)))
    }
}

/// A highlight marker. `game_time_s` is Riot's `EventTime`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Marker {
    pub id: u32,
    #[serde(rename = "type")]
    pub kind: MarkerKind,
    pub label: MarkerLabel,
    pub game_time_s: f64,
    /// Seconds into the recording, once the clock is aligned (see `tracker::clock`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub video_time_s: Option<f64>,
    pub pre_s: f64,
    pub post_s: f64,
    #[serde(skip_serializing_if = "is_false")]
    pub stolen: bool,
    /// A teammate took this objective/ace (only with `include_team`).
    #[serde(skip_serializing_if = "is_false")]
    pub by_team: bool,
    pub riot_event_ids: Vec<u64>,
}

fn is_false(value: &bool) -> bool {
    !*value
}

impl Marker {
    pub fn start_s(&self) -> f64 {
        self.game_time_s - self.pre_s
    }

    pub fn end_s(&self) -> f64 {
        self.game_time_s + self.post_s
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GameResult {
    Win,
    Lose,
}

impl GameResult {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "win" | "victory" => Some(Self::Win),
            "lose" | "loss" | "defeat" => Some(Self::Lose),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Kda {
    pub kills: u32,
    pub deaths: u32,
    pub assists: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkerChange {
    Added(u32),
    Relabeled(u32),
}

/// A `Multikill` relabels the kill it completes; Riot sends both at the same time.
const MULTIKILL_TOLERANCE_S: f64 = 2.0;

/// Stateful event → marker conversion for one match. Feed events in `EventID` order.
#[derive(Debug)]
pub struct Normalizer {
    config: MarkerConfig,
    me: PlayerIdentity,
    roster: Roster,
    streaks: StreakTracker,
    markers: Vec<Marker>,
    next_id: u32,
    counted: Kda,
    result: Option<GameResult>,
    game_end_time: Option<f64>,
    unmapped: HashSet<String>,
}

impl Normalizer {
    pub fn new(config: MarkerConfig, me: PlayerIdentity, roster: Roster) -> Self {
        Self {
            config,
            me,
            roster,
            streaks: StreakTracker::new(),
            markers: Vec::new(),
            next_id: 0,
            counted: Kda::default(),
            result: None,
            game_end_time: None,
            unmapped: HashSet::new(),
        }
    }

    /// Updates the roster (e.g. `playerlist` arrived after the first events).
    pub fn set_roster(&mut self, roster: Roster) {
        self.roster = roster;
    }

    pub fn me(&self) -> &PlayerIdentity {
        &self.me
    }

    pub fn roster(&self) -> &Roster {
        &self.roster
    }

    pub fn markers(&self) -> &[Marker] {
        &self.markers
    }

    /// Result from Riot's `GameEnd` event.
    pub fn result(&self) -> Option<GameResult> {
        self.result
    }

    pub fn game_end_time(&self) -> Option<f64> {
        self.game_end_time
    }

    /// K/D/A counted from the event stream (cross-check for `playerlist` scores).
    pub fn counted_kda(&self) -> Kda {
        self.counted
    }

    /// Processes one event. Returns the markers it added or relabelled.
    pub fn push(&mut self, event: &RawEvent) -> Vec<MarkerChange> {
        let mut changes = Vec::new();
        match event.event_name.as_str() {
            "ChampionKill" => self.on_champion_kill(event, &mut changes),
            "Multikill" => self.on_multikill(event, &mut changes),
            "FirstBlood" => {
                if self.me.matches_opt(event.recipient.as_deref()) {
                    self.add(
                        MarkerKind::FirstBlood,
                        MarkerLabel::FirstBlood,
                        event,
                        false,
                        false,
                        &mut changes,
                    );
                }
            }
            "Ace" => self.on_ace(event, &mut changes),
            "TurretKilled" => self.on_objective(
                event,
                MarkerKind::TurretKill,
                MarkerLabel::TurretKill,
                &mut changes,
            ),
            "InhibKilled" => self.on_objective(
                event,
                MarkerKind::InhibitorKill,
                MarkerLabel::InhibitorKill,
                &mut changes,
            ),
            "DragonKill" => {
                let elder = event
                    .dragon_type
                    .as_deref()
                    .is_some_and(|t| t.eq_ignore_ascii_case("Elder"));
                let label = if elder {
                    MarkerLabel::ElderDragon
                } else {
                    MarkerLabel::DragonKill
                };
                self.on_objective(event, MarkerKind::DragonKill, label, &mut changes);
            }
            "HeraldKill" => self.on_objective(
                event,
                MarkerKind::RiftHerald,
                MarkerLabel::RiftHerald,
                &mut changes,
            ),
            // Same shape as HeraldKill (KillerName, Assisters, Stolen), one per grub.
            "HordeKill" => self.on_objective(
                event,
                MarkerKind::Voidgrubs,
                MarkerLabel::Voidgrubs,
                &mut changes,
            ),
            "BaronKill" => self.on_objective(
                event,
                MarkerKind::BaronKill,
                MarkerLabel::BaronKill,
                &mut changes,
            ),
            "GameEnd" => self.on_game_end(event, &mut changes),
            "GameStart"
            | "MinionsSpawning"
            | "FirstBrick"
            | "InhibRespawningSoon"
            | "InhibRespawned" => {}
            // Event names not confirmed from a live dump stay unmapped (doc 03 §4.2).
            other => {
                if self.unmapped.insert(other.to_owned()) {
                    tracing::debug!(event = other, "unmapped Live Client event");
                }
            }
        }
        changes
    }

    fn on_champion_kill(&mut self, event: &RawEvent, changes: &mut Vec<MarkerChange>) {
        let killer = event.killer_name.as_deref().unwrap_or_default();
        let victim = event.victim_name.as_deref().unwrap_or_default();
        let outcome =
            self.streaks
                .on_champion_kill(killer, self.roster.is_champion(killer), victim);

        if self.me.matches(killer) {
            self.counted.kills += 1;
            self.add(
                MarkerKind::Kill,
                MarkerLabel::Kill,
                event,
                false,
                false,
                changes,
            );
            if outcome.shutdown {
                self.add(
                    MarkerKind::Shutdown,
                    MarkerLabel::Shutdown,
                    event,
                    false,
                    false,
                    changes,
                );
            }
            if let Some(level) = outcome.killer_new_spree {
                self.add(
                    MarkerKind::Spree,
                    level.into(),
                    event,
                    false,
                    false,
                    changes,
                );
            }
        }
        if event.assisters.iter().any(|a| self.me.matches(a)) {
            self.counted.assists += 1;
            self.add(
                MarkerKind::Assist,
                MarkerLabel::Assist,
                event,
                false,
                false,
                changes,
            );
        }
        // The killer may be a turret, minion or monster (an execution); still a death.
        if self.me.matches(victim) {
            self.counted.deaths += 1;
            self.add(
                MarkerKind::Death,
                MarkerLabel::Death,
                event,
                false,
                false,
                changes,
            );
        }
    }

    fn on_multikill(&mut self, event: &RawEvent, changes: &mut Vec<MarkerChange>) {
        if !self.me.matches_opt(event.killer_name.as_deref()) {
            return;
        }
        let Some(label) = event.kill_streak.and_then(MarkerLabel::multikill) else {
            return;
        };
        let completed_kill = self
            .markers
            .iter_mut()
            .rev()
            .find(|m| m.kind == MarkerKind::Kill)
            .filter(|m| (m.game_time_s - event.event_time).abs() <= MULTIKILL_TOLERANCE_S);
        if let Some(marker) = completed_kill {
            if label.priority() > marker.label.priority() {
                tracing::debug!(id = marker.id, label = label.as_str(), "marker relabelled");
                marker.label = label;
                marker.riot_event_ids.push(event.event_id);
                changes.push(MarkerChange::Relabeled(marker.id));
            }
        }
    }

    fn on_ace(&mut self, event: &RawEvent, changes: &mut Vec<MarkerChange>) {
        if self.me.matches_opt(event.acer.as_deref()) {
            self.add(
                MarkerKind::Ace,
                MarkerLabel::Ace,
                event,
                false,
                false,
                changes,
            );
        } else if self.config.include_team {
            let acing_team = event.acing_team.as_deref().and_then(Team::parse);
            if acing_team.is_some() && acing_team == self.roster.my_team() {
                self.add(
                    MarkerKind::Ace,
                    MarkerLabel::Ace,
                    event,
                    false,
                    true,
                    changes,
                );
            }
        }
    }

    fn on_objective(
        &mut self,
        event: &RawEvent,
        kind: MarkerKind,
        label: MarkerLabel,
        changes: &mut Vec<MarkerChange>,
    ) {
        let stolen = event.stolen.unwrap_or(false);
        let killer = event.killer_name.as_deref();
        if self.me.matches_opt(killer) {
            self.add(kind, label, event, stolen, false, changes);
        } else if self.config.include_team && killer.is_some_and(|k| self.is_teammate(k)) {
            self.add(kind, label, event, stolen, true, changes);
        }
    }

    fn on_game_end(&mut self, event: &RawEvent, changes: &mut Vec<MarkerChange>) {
        let result = event.result.as_deref().and_then(GameResult::parse);
        if result.is_some() {
            self.result = result;
        }
        self.game_end_time = Some(event.event_time);
        if result == Some(GameResult::Win) {
            self.add(
                MarkerKind::Victory,
                MarkerLabel::Victory,
                event,
                false,
                false,
                changes,
            );
        }
    }

    fn is_teammate(&self, name: &str) -> bool {
        let mine = self.roster.my_team();
        mine.is_some() && self.roster.team_of(name) == mine && !self.me.matches(name)
    }

    fn add(
        &mut self,
        kind: MarkerKind,
        label: MarkerLabel,
        event: &RawEvent,
        stolen: bool,
        by_team: bool,
        changes: &mut Vec<MarkerChange>,
    ) {
        let window = self.config.window(kind);
        if !window.enabled {
            return;
        }
        self.next_id += 1;
        tracing::debug!(
            id = self.next_id,
            label = label.as_str(),
            game_time = event.event_time,
            "marker"
        );
        self.markers.push(Marker {
            id: self.next_id,
            kind,
            label,
            game_time_s: event.event_time,
            video_time_s: None,
            pre_s: window.pre_s,
            post_s: window.post_s,
            stolen,
            by_team,
            riot_event_ids: vec![event.event_id],
        });
        changes.push(MarkerChange::Added(self.next_id));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::league::models::Player;

    const ME: &str = "Downplayer";

    fn me() -> PlayerIdentity {
        PlayerIdentity::new(Some("Downplayer#DP1"), Some(ME), Some("Downplayer#DP1"))
    }

    fn roster() -> Roster {
        let player = |game: &str, team: &str| Player {
            riot_id: Some(format!("{game}#DP1")),
            riot_id_game_name: Some(game.into()),
            team: team.into(),
            champion_name: "Champ".into(),
            ..Player::default()
        };
        let players = [
            player(ME, "ORDER"),
            player("Mate", "ORDER"),
            player("Foe", "CHAOS"),
            player("Foe2", "CHAOS"),
            player("Foe3", "CHAOS"),
        ];
        Roster::from_players(&players, &me())
    }

    fn normalizer(config: MarkerConfig) -> Normalizer {
        Normalizer::new(config, me(), roster())
    }

    fn event(id: u64, name: &str, time: f64) -> RawEvent {
        RawEvent {
            event_id: id,
            event_name: name.into(),
            event_time: time,
            ..RawEvent::default()
        }
    }

    fn kill(id: u64, time: f64, killer: &str, victim: &str, assisters: &[&str]) -> RawEvent {
        RawEvent {
            killer_name: Some(killer.into()),
            victim_name: Some(victim.into()),
            assisters: assisters.iter().map(|a| a.to_string()).collect(),
            ..event(id, "ChampionKill", time)
        }
    }

    fn objective(id: u64, name: &str, time: f64, killer: &str) -> RawEvent {
        RawEvent {
            killer_name: Some(killer.into()),
            ..event(id, name, time)
        }
    }

    fn labels(n: &Normalizer) -> Vec<&'static str> {
        n.markers().iter().map(|m| m.label.as_str()).collect()
    }

    #[test]
    fn kills_assists_and_deaths_use_outplayed_windows() {
        let mut n = normalizer(MarkerConfig::default());
        n.push(&kill(1, 100.0, ME, "Foe", &[]));
        n.push(&kill(2, 200.0, "Mate", "Foe", &[ME]));
        n.push(&kill(3, 300.0, "Foe", ME, &["Foe2"]));
        assert_eq!(labels(&n), ["kill", "assist", "death"]);
        let windows: Vec<_> = n.markers().iter().map(|m| (m.pre_s, m.post_s)).collect();
        assert_eq!(windows, [(12.0, 8.0), (12.0, 8.0), (12.0, 3.0)]);
        assert_eq!(
            n.counted_kda(),
            Kda {
                kills: 1,
                deaths: 1,
                assists: 1
            }
        );
    }

    #[test]
    fn executions_count_as_deaths() {
        let mut n = normalizer(MarkerConfig::default());
        n.push(&kill(1, 50.0, "Turret_T2_C_05_A", ME, &[]));
        assert_eq!(labels(&n), ["death"]);
    }

    #[test]
    fn other_players_kills_are_ignored() {
        let mut n = normalizer(MarkerConfig::default());
        n.push(&kill(1, 50.0, "Foe", "Mate", &["Foe2"]));
        assert!(n.markers().is_empty());
    }

    #[test]
    fn multikill_relabels_the_kill_it_completes() {
        let mut n = normalizer(MarkerConfig::default());
        n.push(&kill(1, 100.0, ME, "Foe", &[]));
        n.push(&kill(2, 103.0, ME, "Foe2", &[]));
        let changes = n.push(&RawEvent {
            killer_name: Some(ME.into()),
            kill_streak: Some(2),
            ..event(3, "Multikill", 103.0)
        });
        assert_eq!(changes, [MarkerChange::Relabeled(2)]);
        n.push(&kill(4, 106.0, ME, "Foe3", &[]));
        n.push(&RawEvent {
            killer_name: Some(ME.into()),
            kill_streak: Some(3),
            ..event(5, "Multikill", 106.0)
        });
        assert_eq!(labels(&n), ["kill", "double_kill", "triple_kill"]);
        assert_eq!(n.markers()[2].riot_event_ids, [4, 5]);
    }

    #[test]
    fn someone_elses_multikill_or_a_stale_one_changes_nothing() {
        let mut n = normalizer(MarkerConfig::default());
        n.push(&kill(1, 100.0, ME, "Foe", &[]));
        n.push(&RawEvent {
            killer_name: Some("Foe".into()),
            kill_streak: Some(2),
            ..event(2, "Multikill", 100.0)
        });
        n.push(&RawEvent {
            killer_name: Some(ME.into()),
            kill_streak: Some(2),
            ..event(3, "Multikill", 160.0)
        });
        assert_eq!(labels(&n), ["kill"]);
    }

    #[test]
    fn objectives_are_mine_only_unless_team_is_enabled() {
        let events = [
            RawEvent {
                dragon_type: Some("Fire".into()),
                stolen: Some(false),
                ..objective(1, "DragonKill", 600.0, ME)
            },
            RawEvent {
                stolen: Some(true),
                ..objective(2, "BaronKill", 1300.0, ME)
            },
            objective(3, "HeraldKill", 800.0, "Mate"),
            RawEvent {
                turret_killed: Some("Turret_T2_L_03_A".into()),
                ..objective(4, "TurretKilled", 900.0, ME)
            },
            RawEvent {
                inhib_killed: Some("Barracks_T2_L1".into()),
                ..objective(5, "InhibKilled", 1400.0, "Mate")
            },
            objective(6, "TurretKilled", 950.0, "Minion_T100L1S12N0123"),
            RawEvent {
                dragon_type: Some("Elder".into()),
                ..objective(7, "DragonKill", 1900.0, ME)
            },
            objective(8, "BaronKill", 2000.0, "Foe"),
        ];

        let mut solo = normalizer(MarkerConfig::default());
        events.iter().for_each(|e| {
            solo.push(e);
        });
        assert_eq!(
            labels(&solo),
            ["dragon_kill", "baron_kill", "turret_kill", "elder_dragon"]
        );
        assert!(solo.markers()[1].stolen);
        assert!(solo
            .markers()
            .iter()
            .all(|m| (m.pre_s, m.post_s) == (10.0, 5.0)));

        let mut team = normalizer(MarkerConfig {
            include_team: true,
            ..MarkerConfig::default()
        });
        events.iter().for_each(|e| {
            team.push(e);
        });
        assert_eq!(
            labels(&team),
            [
                "dragon_kill",
                "baron_kill",
                "rift_herald",
                "turret_kill",
                "inhibitor_kill",
                "elder_dragon"
            ]
        );
        let by_team: Vec<_> = team
            .markers()
            .iter()
            .filter(|m| m.by_team)
            .map(|m| m.label.as_str())
            .collect();
        assert_eq!(by_team, ["rift_herald", "inhibitor_kill"]);
    }

    #[test]
    fn victory_is_marked_and_the_result_recorded() {
        let mut n = normalizer(MarkerConfig::default());
        n.push(&RawEvent {
            result: Some("Win".into()),
            ..event(9, "GameEnd", 1800.0)
        });
        assert_eq!(labels(&n), ["victory"]);
        assert_eq!((n.markers()[0].pre_s, n.markers()[0].post_s), (10.0, 1.0));
        assert_eq!(n.result(), Some(GameResult::Win));

        let mut lost = normalizer(MarkerConfig::default());
        lost.push(&RawEvent {
            result: Some("Lose".into()),
            ..event(9, "GameEnd", 1800.0)
        });
        assert!(lost.markers().is_empty());
        assert_eq!(lost.result(), Some(GameResult::Lose));
        assert_eq!(lost.game_end_time(), Some(1800.0));
    }

    #[test]
    fn extras_are_opt_in() {
        let feed = |n: &mut Normalizer| {
            n.push(&kill(1, 90.0, ME, "Foe", &[]));
            n.push(&RawEvent {
                recipient: Some(ME.into()),
                ..event(2, "FirstBlood", 90.0)
            });
            for (i, victim) in ["A", "B", "C"].iter().enumerate() {
                n.push(&kill(3 + i as u64, 100.0 + i as f64, "Foe2", victim, &[]));
            }
            n.push(&kill(6, 200.0, ME, "Foe2", &[])); // shutdown; my streak is 2
            n.push(&kill(7, 210.0, ME, "Foe3", &[])); // my streak 3: killing spree
            n.push(&RawEvent {
                acer: Some(ME.into()),
                acing_team: Some("ORDER".into()),
                ..event(8, "Ace", 211.0)
            });
        };
        let mut defaults = normalizer(MarkerConfig::default());
        feed(&mut defaults);
        assert_eq!(labels(&defaults), ["kill", "kill", "kill"]);

        let mut extras = normalizer(MarkerConfig::default().with_extras());
        feed(&mut extras);
        assert_eq!(
            labels(&extras),
            [
                "kill",
                "first_blood",
                "kill",
                "shutdown",
                "kill",
                "killing_spree",
                "ace"
            ]
        );
    }

    #[test]
    fn disabled_kinds_produce_no_markers_but_still_count() {
        let mut config = MarkerConfig::default();
        config.set_enabled(MarkerKind::Death, false);
        let mut n = normalizer(config);
        n.push(&kill(1, 300.0, "Foe", ME, &[]));
        assert!(n.markers().is_empty());
        assert_eq!(n.counted_kda().deaths, 1);
    }

    #[test]
    fn unknown_events_are_ignored() {
        let mut n = normalizer(MarkerConfig::default());
        assert!(n.push(&objective(1, "SomethingNew", 400.0, ME)).is_empty());
        assert!(n.push(&event(2, "InhibRespawned", 500.0)).is_empty());
    }

    /// Live shape (2026-10-15): `{"EventName":"HordeKill","KillerName":…,"Assisters":[…],"Stolen":"False"}`.
    fn grub(id: u64, time: f64, killer: &str, stolen: bool) -> RawEvent {
        RawEvent {
            stolen: Some(stolen),
            assisters: vec!["Mate".into()],
            ..objective(id, "HordeKill", time, killer)
        }
    }

    #[test]
    fn voidgrubs_are_opt_in() {
        let feed = |n: &mut Normalizer| {
            n.push(&grub(1, 361.0, ME, false));
            n.push(&grub(2, 364.5, "Mate", false));
            n.push(&grub(3, 368.0, ME, true));
            n.push(&grub(4, 600.0, "Foe", false));
        };
        let mut defaults = normalizer(MarkerConfig::default());
        feed(&mut defaults);
        assert!(defaults.markers().is_empty(), "off by default");
        assert!(!MarkerKind::Voidgrubs.default_window().enabled);

        let mut config = MarkerConfig::default();
        config.set_enabled(MarkerKind::Voidgrubs, true);
        let mut mine = normalizer(config.clone());
        feed(&mut mine);
        assert_eq!(labels(&mine), ["voidgrubs", "voidgrubs"]);
        assert!(mine
            .markers()
            .iter()
            .all(|m| m.kind == MarkerKind::Voidgrubs && (m.pre_s, m.post_s) == (10.0, 5.0)));
        assert_eq!(
            mine.markers()
                .iter()
                .map(|m| (m.riot_event_ids[0], m.stolen))
                .collect::<Vec<_>>(),
            [(1, false), (3, true)]
        );

        config.include_team = true;
        let mut team = normalizer(config);
        feed(&mut team);
        let by_team: Vec<_> = team.markers().iter().map(|m| m.by_team).collect();
        assert_eq!(
            by_team,
            [false, true, false],
            "the enemy's grub is never marked"
        );

        let mut extras = normalizer(MarkerConfig::default().with_extras());
        feed(&mut extras);
        assert_eq!(extras.markers().len(), 2);
    }

    #[test]
    fn label_priorities_follow_the_spec() {
        use MarkerLabel::*;
        let ordered = [
            PentaKill, QuadraKill, TripleKill, DoubleKill, Kill, BaronKill, TurretKill, Assist,
            Death,
        ];
        assert!(ordered
            .windows(2)
            .all(|w| w[0].priority() > w[1].priority()));
        assert!(Kill.priority() > Victory.priority() && Victory.priority() > BaronKill.priority());
        // Just below the Herald, above every lower objective.
        assert_eq!(Voidgrubs.priority(), RiftHerald.priority() - 1);
        assert!(Voidgrubs.priority() > InhibitorKill.priority());
    }
}
