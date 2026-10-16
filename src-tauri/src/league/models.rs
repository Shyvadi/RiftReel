//! Wire types for Riot's local APIs.
//!
//! Only the fields RiftReel uses are modelled. Unknown fields are ignored, so Riot
//! adding fields doesn't break parsing. Types that hold Riot identifiers print
//! pseudonyms in `Debug` (see [`crate::privacy`]).

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};

use crate::privacy::{redact_opt, Redacted};

// ---------------------------------------------------------------------------
// Live Client Data API (https://127.0.0.1:2999/liveclientdata/*)
// ---------------------------------------------------------------------------

/// `GET /liveclientdata/eventdata`. Every call returns the whole event history.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct EventData {
    #[serde(rename = "Events", default, deserialize_with = "null_as_default")]
    pub events: Vec<RawEvent>,
}

/// One entry of `eventdata.Events`. Each event type fills a different subset of the
/// optional fields; `KillerName`, `VictimName`, `Assisters`, `Acer` and `Recipient`
/// hold the Riot ID *game name* (no `#TAG`), or a structure/monster/minion name.
#[derive(Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct RawEvent {
    #[serde(rename = "EventID")]
    pub event_id: u64,
    pub event_name: String,
    /// Game time in seconds.
    #[serde(default)]
    pub event_time: f64,
    #[serde(default)]
    pub killer_name: Option<String>,
    #[serde(default)]
    pub victim_name: Option<String>,
    #[serde(default, deserialize_with = "null_as_default")]
    pub assisters: Vec<String>,
    /// `Multikill`: 2 (double) … 5 (penta).
    #[serde(default)]
    pub kill_streak: Option<u32>,
    /// `DragonKill`: "Fire", "Water", "Earth", "Air", "Hextech", "Chemtech", "Elder".
    #[serde(default)]
    pub dragon_type: Option<String>,
    /// Objective steals. Riot sends the strings "True"/"False".
    #[serde(default, deserialize_with = "riot_bool")]
    pub stolen: Option<bool>,
    /// `GameEnd`: "Win" or "Lose".
    #[serde(default)]
    pub result: Option<String>,
    #[serde(default)]
    pub acer: Option<String>,
    #[serde(default)]
    pub acing_team: Option<String>,
    /// `FirstBlood`: who got it.
    #[serde(default)]
    pub recipient: Option<String>,
    #[serde(default)]
    pub turret_killed: Option<String>,
    #[serde(default)]
    pub inhib_killed: Option<String>,
}

impl fmt::Debug for RawEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut d = f.debug_struct("RawEvent");
        d.field("id", &self.event_id)
            .field("name", &self.event_name)
            .field("time", &self.event_time);
        if let Some(killer) = redact_opt(&self.killer_name) {
            d.field("killer", &killer);
        }
        if let Some(victim) = redact_opt(&self.victim_name) {
            d.field("victim", &victim);
        }
        if !self.assisters.is_empty() {
            let assisters: Vec<_> = self.assisters.iter().map(|a| Redacted(a)).collect();
            d.field("assisters", &assisters);
        }
        if let Some(streak) = self.kill_streak {
            d.field("kill_streak", &streak);
        }
        if let Some(dragon) = &self.dragon_type {
            d.field("dragon_type", dragon);
        }
        if let Some(stolen) = self.stolen {
            d.field("stolen", &stolen);
        }
        if let Some(result) = &self.result {
            d.field("result", result);
        }
        if let Some(acer) = redact_opt(&self.acer) {
            d.field("acer", &acer);
        }
        if let Some(team) = &self.acing_team {
            d.field("acing_team", team);
        }
        if let Some(recipient) = redact_opt(&self.recipient) {
            d.field("recipient", &recipient);
        }
        if let Some(turret) = &self.turret_killed {
            d.field("turret", turret);
        }
        if let Some(inhib) = &self.inhib_killed {
            d.field("inhibitor", inhib);
        }
        d.finish()
    }
}

/// `GET /liveclientdata/activeplayer`: the local player. In spectator and replay
/// sessions this endpoint answers with an error instead.
#[derive(Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivePlayer {
    #[serde(default)]
    pub riot_id: Option<String>,
    #[serde(default)]
    pub riot_id_game_name: Option<String>,
    #[serde(default)]
    pub riot_id_tag_line: Option<String>,
    /// Legacy field; now `Name#TAG`.
    #[serde(default)]
    pub summoner_name: Option<String>,
    #[serde(default)]
    pub level: Option<u32>,
}

impl fmt::Debug for ActivePlayer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ActivePlayer")
            .field("riot_id", &redact_opt(&self.riot_id))
            .field("level", &self.level)
            .finish_non_exhaustive()
    }
}

/// One entry of `GET /liveclientdata/playerlist`.
#[derive(Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Player {
    #[serde(default)]
    pub champion_name: String,
    #[serde(default)]
    pub riot_id: Option<String>,
    #[serde(default)]
    pub riot_id_game_name: Option<String>,
    #[serde(default)]
    pub riot_id_tag_line: Option<String>,
    #[serde(default)]
    pub summoner_name: Option<String>,
    /// "ORDER" (blue) or "CHAOS" (red).
    #[serde(default)]
    pub team: String,
    #[serde(default)]
    pub is_bot: bool,
    #[serde(default)]
    pub position: Option<String>,
    #[serde(default)]
    pub scores: Scores,
}

impl fmt::Debug for Player {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Player")
            .field("riot_id", &redact_opt(&self.riot_id))
            .field("champion", &self.champion_name)
            .field("team", &self.team)
            .field("scores", &self.scores)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Scores {
    #[serde(default)]
    pub kills: u32,
    #[serde(default)]
    pub deaths: u32,
    #[serde(default)]
    pub assists: u32,
    #[serde(default)]
    pub creep_score: u32,
}

/// `GET /liveclientdata/gamestats`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GameStats {
    /// "CLASSIC", "ARAM", "PRACTICETOOL", "CHERRY" (Arena), …
    #[serde(default)]
    pub game_mode: String,
    /// Seconds since the game started.
    #[serde(default)]
    pub game_time: f64,
    #[serde(default)]
    pub map_name: String,
    #[serde(default)]
    pub map_number: i64,
    #[serde(default)]
    pub map_terrain: String,
}

/// Error body the Live Client API returns with non-2xx statuses.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiErrorBody {
    #[serde(default)]
    pub error_code: Option<String>,
    #[serde(default)]
    pub http_status: Option<u16>,
    #[serde(default)]
    pub message: Option<String>,
}

// ---------------------------------------------------------------------------
// League client (LCU)
// ---------------------------------------------------------------------------

/// `/lol-gameflow/v1/gameflow-phase`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum GameflowPhase {
    #[default]
    None,
    Lobby,
    Matchmaking,
    CheckedIntoTournament,
    ReadyCheck,
    ChampSelect,
    GameStart,
    FailedToLaunch,
    InProgress,
    Reconnect,
    WaitingForStats,
    PreEndOfGame,
    EndOfGame,
    TerminatedInError,
    Other(String),
}

impl GameflowPhase {
    pub fn parse(value: &str) -> Self {
        match value {
            "None" | "" => Self::None,
            "Lobby" => Self::Lobby,
            "Matchmaking" => Self::Matchmaking,
            "CheckedIntoTournament" => Self::CheckedIntoTournament,
            "ReadyCheck" => Self::ReadyCheck,
            "ChampSelect" => Self::ChampSelect,
            "GameStart" => Self::GameStart,
            "FailedToLaunch" => Self::FailedToLaunch,
            "InProgress" => Self::InProgress,
            "Reconnect" => Self::Reconnect,
            "WaitingForStats" => Self::WaitingForStats,
            "PreEndOfGame" => Self::PreEndOfGame,
            "EndOfGame" => Self::EndOfGame,
            "TerminatedInError" => Self::TerminatedInError,
            other => Self::Other(other.to_owned()),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::None => "None",
            Self::Lobby => "Lobby",
            Self::Matchmaking => "Matchmaking",
            Self::CheckedIntoTournament => "CheckedIntoTournament",
            Self::ReadyCheck => "ReadyCheck",
            Self::ChampSelect => "ChampSelect",
            Self::GameStart => "GameStart",
            Self::FailedToLaunch => "FailedToLaunch",
            Self::InProgress => "InProgress",
            Self::Reconnect => "Reconnect",
            Self::WaitingForStats => "WaitingForStats",
            Self::PreEndOfGame => "PreEndOfGame",
            Self::EndOfGame => "EndOfGame",
            Self::TerminatedInError => "TerminatedInError",
            Self::Other(other) => other,
        }
    }

    /// Phases after the game itself ended, while the client gathers or shows stats.
    pub fn is_post_game(&self) -> bool {
        matches!(
            self,
            Self::WaitingForStats | Self::PreEndOfGame | Self::EndOfGame
        )
    }
}

impl fmt::Display for GameflowPhase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for GameflowPhase {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Option::<String>::deserialize(deserializer)?;
        Ok(value.map_or(Self::None, |v| Self::parse(&v)))
    }
}

impl Serialize for GameflowPhase {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

/// `GET /lol-gameflow/v1/session`, reduced to the match description. The real payload
/// also lists every player with their identifiers; those fields are never parsed.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GameflowSession {
    #[serde(default)]
    pub phase: GameflowPhase,
    #[serde(default)]
    pub game_data: GameData,
    #[serde(default)]
    pub map: MapInfo,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GameData {
    #[serde(default)]
    pub game_id: u64,
    #[serde(default)]
    pub is_custom_game: bool,
    #[serde(default)]
    pub queue: QueueInfo,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueInfo {
    /// e.g. 420 Ranked Solo/Duo, 440 Flex, 450 ARAM, 1700 Arena; -1 for custom games.
    #[serde(default)]
    pub id: i64,
    #[serde(default)]
    pub game_mode: String,
    #[serde(default, rename = "type")]
    pub queue_type: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub is_ranked: bool,
    #[serde(default)]
    pub map_id: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MapInfo {
    #[serde(default)]
    pub id: i64,
    #[serde(default)]
    pub game_mode: String,
    #[serde(default)]
    pub name: String,
}

/// `GET /lol-end-of-game/v1/eog-stats-block`, reduced to the local player's result.
/// Player identifiers in the payload are never parsed.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EogStatsBlock {
    #[serde(default)]
    pub game_id: u64,
    /// Seconds.
    #[serde(default)]
    pub game_length: u64,
    #[serde(default)]
    pub game_mode: String,
    #[serde(default)]
    pub queue_type: String,
    #[serde(default)]
    pub game_ended_in_early_surrender: bool,
    #[serde(default)]
    pub game_ended_in_surrender: bool,
    #[serde(default)]
    pub local_player: Option<EogPlayer>,
    #[serde(default, deserialize_with = "null_as_default")]
    pub teams: Vec<EogTeam>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EogPlayer {
    #[serde(default)]
    pub champion_name: Option<String>,
    #[serde(default)]
    pub champion_id: Option<i64>,
    #[serde(default)]
    pub team_id: Option<i64>,
    /// Upper-case stat keys such as `CHAMPIONS_KILLED`, `NUM_DEATHS`, `ASSISTS`, `WIN`.
    #[serde(default)]
    pub stats: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EogTeam {
    #[serde(default)]
    pub team_id: i64,
    #[serde(default)]
    pub is_winning_team: bool,
    #[serde(default)]
    pub is_player_team: bool,
}

impl EogStatsBlock {
    fn stat(&self, key: &str) -> Option<i64> {
        let value = self.local_player.as_ref()?.stats.get(key)?;
        value.as_i64().or_else(|| value.as_f64().map(|f| f as i64))
    }

    /// Whether the local player's team won.
    pub fn win(&self) -> Option<bool> {
        if let Some(win) = self.stat("WIN") {
            return Some(win > 0);
        }
        if let Some(lose) = self.stat("LOSE") {
            return Some(lose == 0);
        }
        self.teams
            .iter()
            .find(|t| t.is_player_team)
            .map(|t| t.is_winning_team)
    }

    /// Kills, deaths, assists of the local player.
    pub fn kda(&self) -> Option<(u32, u32, u32)> {
        let as_u32 = |v: i64| u32::try_from(v).unwrap_or(0);
        Some((
            as_u32(self.stat("CHAMPIONS_KILLED")?),
            as_u32(self.stat("NUM_DEATHS")?),
            as_u32(self.stat("ASSISTS")?),
        ))
    }
}

// ---------------------------------------------------------------------------
// serde helpers
// ---------------------------------------------------------------------------

/// Accepts `null` where a collection is expected.
fn null_as_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

/// Riot encodes some booleans as "True"/"False" strings.
fn riot_bool<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<bool>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum BoolOrString {
        Bool(bool),
        Str(String),
    }
    Ok(match Option::<BoolOrString>::deserialize(deserializer)? {
        None => None,
        Some(BoolOrString::Bool(b)) => Some(b),
        Some(BoolOrString::Str(s)) if s.eq_ignore_ascii_case("true") => Some(true),
        Some(BoolOrString::Str(s)) if s.eq_ignore_ascii_case("false") => Some(false),
        Some(BoolOrString::Str(_)) => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sample payload from Riot's Game Client API documentation (names replaced).
    const RIOT_SAMPLE_EVENTS: &str = r#"{"Events":[
        {"EventID":0,"EventName":"GameStart","EventTime":0.0563616007566452},
        {"EventID":1,"EventName":"MinionsSpawning","EventTime":65.0},
        {"EventID":2,"EventName":"FirstBrick","EventTime":1023.0,"KillerName":"Player A"},
        {"EventID":3,"EventName":"TurretKilled","EventTime":1023.0,"TurretKilled":"Turret_T2_L_03_A","KillerName":"Player A","Assisters":[]},
        {"EventID":4,"EventName":"InhibKilled","EventTime":1203.0,"InhibKilled":"Barracks_T2_L1","KillerName":"Player A","Assisters":["Player B"]},
        {"EventID":5,"EventName":"DragonKill","EventTime":1210.0,"DragonType":"Earth","Stolen":"False","KillerName":"Player A","Assisters":[]},
        {"EventID":6,"EventName":"HeraldKill","EventTime":1220.0,"Stolen":"True","KillerName":"Player A","Assisters":[]},
        {"EventID":7,"EventName":"BaronKill","EventTime":1300.0,"Stolen":"False","KillerName":"Player A","Assisters":null},
        {"EventID":8,"EventName":"ChampionKill","EventTime":1310.0,"VictimName":"Player C","KillerName":"Player A","Assisters":["Player B"]},
        {"EventID":9,"EventName":"Multikill","EventTime":1310.0,"KillerName":"Player A","KillStreak":2},
        {"EventID":10,"EventName":"Ace","EventTime":1320.0,"Acer":"Player A","AcingTeam":"ORDER"},
        {"EventID":11,"EventName":"SomethingNew","EventTime":1330.0,"Whatever":{"x":1}}
    ]}"#;

    #[test]
    fn parses_riot_sample_eventdata() {
        let data: EventData = serde_json::from_str(RIOT_SAMPLE_EVENTS).unwrap();
        assert_eq!(data.events.len(), 12);
        assert_eq!(
            data.events[3].turret_killed.as_deref(),
            Some("Turret_T2_L_03_A")
        );
        assert_eq!(data.events[5].stolen, Some(false));
        assert_eq!(data.events[5].dragon_type.as_deref(), Some("Earth"));
        assert_eq!(data.events[6].stolen, Some(true));
        assert!(
            data.events[7].assisters.is_empty(),
            "null Assisters becomes empty"
        );
        assert_eq!(data.events[9].kill_streak, Some(2));
        assert_eq!(data.events[10].acing_team.as_deref(), Some("ORDER"));
        assert_eq!(data.events[11].event_name, "SomethingNew");
    }

    #[test]
    fn debug_output_hides_player_names() {
        let data: EventData = serde_json::from_str(RIOT_SAMPLE_EVENTS).unwrap();
        let debug = format!("{:?}", data.events[8]);
        assert!(!debug.contains("Player"), "{debug}");
        assert!(debug.contains("ChampionKill"));
    }

    #[test]
    fn parses_gameflow_phases() {
        let phase: GameflowPhase = serde_json::from_str("\"InProgress\"").unwrap();
        assert_eq!(phase, GameflowPhase::InProgress);
        let phase: GameflowPhase = serde_json::from_str("\"BrandNewPhase\"").unwrap();
        assert_eq!(phase, GameflowPhase::Other("BrandNewPhase".into()));
        let phase: GameflowPhase = serde_json::from_str("null").unwrap();
        assert_eq!(phase, GameflowPhase::None);
        assert!(GameflowPhase::EndOfGame.is_post_game());
    }

    #[test]
    fn eog_result_prefers_stats_then_team_flags() {
        let block: EogStatsBlock = serde_json::from_str(
            r#"{"gameId":1,"localPlayer":{"stats":{"WIN":1,"CHAMPIONS_KILLED":7,"NUM_DEATHS":2,"ASSISTS":9}},
                "teams":[{"teamId":100,"isPlayerTeam":true,"isWinningTeam":false}]}"#,
        )
        .unwrap();
        assert_eq!(block.win(), Some(true));
        assert_eq!(block.kda(), Some((7, 2, 9)));

        let block: EogStatsBlock = serde_json::from_str(
            r#"{"teams":[{"teamId":200,"isPlayerTeam":true,"isWinningTeam":true}],"localPlayer":{"stats":{}}}"#,
        )
        .unwrap();
        assert_eq!(block.win(), Some(true));
        assert_eq!(block.kda(), None);
    }
}
