//! Who is who in a match: the local player ("me") and the roster from `playerlist`.

use std::fmt;

use serde::Serialize;

use crate::league::models::{ActivePlayer, Player, Scores};
use crate::privacy::Redacted;

/// The names Riot uses for one player. Events carry the Riot ID *game name* (no tag);
/// `activeplayer`/`playerlist` carry the full Riot ID, the game name, and a legacy
/// `summonerName` that is now `Name#TAG`. "Me" is matched the way Riot does:
/// riotId, then gameName, then summonerName. Comparison is case-insensitive.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct PlayerIdentity {
    riot_id: Option<String>,
    /// Lower-cased names to match: the Riot ID, the game name, the summoner name, and
    /// the part of each before `#`.
    keys: Vec<String>,
    /// The same names in original case, for the log scrubber.
    names: Vec<String>,
}

impl PlayerIdentity {
    pub fn new(
        riot_id: Option<&str>,
        game_name: Option<&str>,
        summoner_name: Option<&str>,
    ) -> Self {
        let mut identity = Self {
            riot_id: riot_id
                .map(|r| r.trim().to_lowercase())
                .filter(|r| !r.is_empty()),
            ..Self::default()
        };
        for name in [riot_id, game_name, summoner_name].into_iter().flatten() {
            identity.add(name);
            if let Some((game, _tag)) = name.split_once('#') {
                identity.add(game);
            }
        }
        identity
    }

    pub fn from_active_player(player: &ActivePlayer) -> Self {
        Self::new(
            player.riot_id.as_deref(),
            player.riot_id_game_name.as_deref(),
            player.summoner_name.as_deref(),
        )
    }

    pub fn from_player(player: &Player) -> Self {
        Self::new(
            player.riot_id.as_deref(),
            player.riot_id_game_name.as_deref(),
            player.summoner_name.as_deref(),
        )
    }

    fn add(&mut self, name: &str) {
        let name = name.trim();
        let key = name.to_lowercase();
        if !key.is_empty() && !self.keys.contains(&key) {
            self.keys.push(key);
            self.names.push(name.to_owned());
        }
    }

    pub fn is_known(&self) -> bool {
        !self.keys.is_empty()
    }

    /// Whether an event name (`KillerName`, `VictimName`, an assister, …) is this player.
    pub fn matches(&self, name: &str) -> bool {
        let key = name.trim().to_lowercase();
        !key.is_empty() && self.keys.contains(&key)
    }

    pub fn matches_opt(&self, name: Option<&str>) -> bool {
        name.is_some_and(|n| self.matches(n))
    }

    /// Same player: full Riot IDs decide when both sides have one, because two players
    /// can share a game name with different tags.
    pub fn same_player(&self, other: &PlayerIdentity) -> bool {
        match (&self.riot_id, &other.riot_id) {
            (Some(a), Some(b)) => a == b,
            _ => self.keys.iter().any(|k| other.keys.contains(k)),
        }
    }

    /// The player's names in original case (for [`crate::privacy::register_names`]).
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.names.iter().map(String::as_str)
    }
}

impl fmt::Debug for PlayerIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.names.first() {
            Some(name) => write!(f, "PlayerIdentity({:?})", Redacted(name)),
            None => f.write_str("PlayerIdentity(unknown)"),
        }
    }
}

/// Whether an event name (`KillerName`, `VictimName`, …) is a turret, minion, monster or
/// other structure rather than a player, going by Riot's object naming.
pub fn is_game_object(name: &str) -> bool {
    const PREFIXES: [&str; 5] = ["Turret_", "Minion_", "SRU_", "Barracks_", "HA_"];
    PREFIXES.iter().any(|prefix| name.starts_with(prefix))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Team {
    /// Blue side.
    Order,
    /// Red side.
    Chaos,
}

impl Team {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_uppercase().as_str() {
            "ORDER" | "100" => Some(Self::Order),
            "CHAOS" | "200" => Some(Self::Chaos),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct RosterEntry {
    pub identity: PlayerIdentity,
    pub team: Option<Team>,
    pub champion: String,
    pub is_bot: bool,
    pub is_me: bool,
    pub scores: Scores,
}

/// The match's players from `playerlist`.
#[derive(Debug, Clone, Default)]
pub struct Roster {
    entries: Vec<RosterEntry>,
}

impl Roster {
    pub fn from_players(players: &[Player], me: &PlayerIdentity) -> Self {
        let entries = players
            .iter()
            .map(|player| {
                let identity = PlayerIdentity::from_player(player);
                RosterEntry {
                    is_me: me.is_known() && identity.same_player(me),
                    team: Team::parse(&player.team),
                    champion: player.champion_name.clone(),
                    is_bot: player.is_bot,
                    scores: player.scores,
                    identity,
                }
            })
            .collect();
        Self { entries }
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn entries(&self) -> &[RosterEntry] {
        &self.entries
    }

    pub fn find(&self, name: &str) -> Option<&RosterEntry> {
        self.entries.iter().find(|e| e.identity.matches(name))
    }

    pub fn me(&self) -> Option<&RosterEntry> {
        self.entries.iter().find(|e| e.is_me)
    }

    pub fn my_team(&self) -> Option<Team> {
        self.me().and_then(|e| e.team)
    }

    pub fn team_of(&self, name: &str) -> Option<Team> {
        self.find(name).and_then(|e| e.team)
    }

    /// Whether `name` is a champion in this match, as opposed to a turret, minion or
    /// monster (executions). Without a roster, falls back to Riot's object naming.
    pub fn is_champion(&self, name: &str) -> bool {
        if name.trim().is_empty() {
            return false;
        }
        if !self.entries.is_empty() {
            return self.find(name).is_some();
        }
        !is_game_object(name)
    }

    /// Every player name in the match (for [`crate::privacy::register_names`]).
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().flat_map(|e| e.identity.names())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn player(riot_id: &str, team: &str, champion: &str) -> Player {
        let (game, tag) = riot_id.split_once('#').unwrap();
        Player {
            champion_name: champion.into(),
            riot_id: Some(riot_id.into()),
            riot_id_game_name: Some(game.into()),
            riot_id_tag_line: Some(tag.into()),
            summoner_name: Some(riot_id.into()),
            team: team.into(),
            ..Player::default()
        }
    }

    #[test]
    fn identity_matches_game_name_riot_id_and_legacy_names() {
        let me = PlayerIdentity::new(
            Some("Down Player#DP1"),
            Some("Down Player"),
            Some("Down Player#DP1"),
        );
        assert!(me.matches("Down Player"));
        assert!(me.matches("down player#dp1"));
        assert!(me.matches(" DOWN PLAYER "));
        assert!(!me.matches("Down"));
        assert!(!me.matches(""));

        // Older payloads only had a plain summoner name.
        let legacy = PlayerIdentity::new(None, None, Some("OldName"));
        assert!(legacy.matches("oldname"));
    }

    #[test]
    fn same_game_name_different_tag_is_a_different_player() {
        let a = PlayerIdentity::new(Some("Twin#EUW"), Some("Twin"), None);
        let b = PlayerIdentity::new(Some("Twin#NA1"), Some("Twin"), None);
        assert!(!a.same_player(&b));
        assert!(a.same_player(&PlayerIdentity::new(Some("twin#euw"), None, None)));
    }

    #[test]
    fn roster_knows_me_teams_and_champions() {
        let me = PlayerIdentity::new(Some("Me#DP1"), Some("Me"), None);
        let roster = Roster::from_players(
            &[
                player("Me#DP1", "ORDER", "Shyvana"),
                player("Mate#DP1", "ORDER", "Ahri"),
                player("Foe#DP2", "CHAOS", "Zed"),
            ],
            &me,
        );
        assert_eq!(roster.me().unwrap().champion, "Shyvana");
        assert_eq!(roster.my_team(), Some(Team::Order));
        assert_eq!(roster.team_of("Foe"), Some(Team::Chaos));
        assert!(roster.is_champion("Mate"));
        assert!(!roster.is_champion("Turret_T2_C_05_A"));
        assert!(!roster.is_champion("SRU_Baron17.1.1"));
    }

    #[test]
    fn without_roster_non_champion_names_are_recognised() {
        let roster = Roster::default();
        assert!(roster.is_champion("Anyone"));
        assert!(!roster.is_champion("Minion_T200L0S11N0034"));
        assert!(!roster.is_champion("Turret_T1_C_05_A"));
    }
}
