//! Fixture recorder (doc 02 §B5.1): saves one match from Riot's local APIs in the
//! fixture format of `fixtures/lol/README.md`, the backend of `riftreel-dev record`.
//!
//! Kept from the Live Client Data API: `activeplayer` (or the error a spectator gets),
//! the last `playerlist` read (end-of-game scores), `gamestats` and the complete
//! `eventdata` history. From the League client: the gameflow session, the end-of-game
//! stats block and the gameflow-phase WebSocket frames.
//!
//! Raw payloads only live in memory. Everything written goes through
//! [`Pseudonymizer`] first: Riot IDs and names become `Reeler#RR1` (the local
//! player) and `Player1#RR1`, `Player2#RR1`, … consistently across all files; puuids,
//! summoner and account IDs are dropped; game IDs (which would look the match up,
//! names included) are replaced. The League client password is never part of a
//! payload, and every file is checked for it before it is written.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use regex::Regex;
use serde_json::{Map, Value};
use tokio::time::{self, MissedTickBehavior};

use crate::league::client::{
    parse_phase_event, FrameSubscription, LcuClient, GAMEFLOW_PHASE_EVENT,
};
use crate::league::live::LiveClient;
use crate::league::models::GameflowPhase;
use crate::tracker::roster::is_game_object;

/// Pseudonym of the local player (the synthetic fixtures use it too).
pub const LOCAL_PLAYER: &str = "Reeler";

/// Everything else becomes `Player<n>`.
const OTHER_PLAYER: &str = "Player";

/// Names in name fields that match no player and no game object.
const UNKNOWN_NAME: &str = "Unknown";

/// Pseudonymous tag lines are `RR1`, `RR2`, … (`RR0`: tag of an unmatched player).
const TAG_PREFIX: &str = "RR";

/// Replacement for game IDs.
const PSEUDO_GAME_ID: u64 = 1;

/// Keys (lower-cased) whose values are player names.
const NAME_KEYS: [&str; 12] = [
    "killername",
    "victimname",
    "assisters",
    "acer",
    "recipient",
    "riotid",
    "riotidgamename",
    "summonername",
    "gamename",
    "summonerinternalname",
    "internalname",
    "playername",
];

/// Keys (lower-cased) whose values are tag lines.
const TAG_KEYS: [&str; 2] = ["riotidtagline", "tagline"];

/// Keys (lower-cased) whose values are game vocabulary, never player names, even when
/// a player happens to be called the same (a player named "Fire" vs `DragonType`).
const VOCABULARY_KEYS: [&str; 22] = [
    "eventname",
    "dragontype",
    "result",
    "acingteam",
    "turretkilled",
    "inhibkilled",
    "team",
    "position",
    "selectedposition",
    "detectedteamposition",
    "gamemode",
    "gametype",
    "queuetype",
    "type",
    "phase",
    "mapname",
    "mapterrain",
    "displayname",
    "rawdisplayname",
    "rawdescription",
    "description",
    "name",
];

/// Summoner (account) progression in the end-of-game block, as opposed to gameplay.
const ACCOUNT_KEYS: [&str; 8] = [
    "summonerlevel",
    "currentlevel",
    "previouslevel",
    "experiencetotal",
    "previousxptotal",
    "nextlevelxp",
    "prelevelupexperiencetotal",
    "prelevelupnextlevelxp",
];

fn is_dropped_key(key: &str) -> bool {
    key.ends_with("puuid")
        || key.ends_with("summonerid")
        || key.ends_with("accountid")
        || key == "playerid"
        // Credentials, e.g. the post-game chat's JWT (`mucJwtDto`, `multiUserChatPassword`).
        || ["password", "token", "jwt", "secret"].iter().any(|word| key.contains(word))
        // Account details that single a player out: profile icon, summoner level and XP.
        || key.ends_with("iconid")
        || ACCOUNT_KEYS.contains(&key)
}

/// A JSON Web Token anywhere in a string.
static JWT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"eyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]*").expect("valid JWT regex")
});

fn is_game_id_key(key: &str) -> bool {
    key.ends_with("gameid")
}

fn is_vocabulary_key(key: &str) -> bool {
    VOCABULARY_KEYS.contains(&key)
        || key.contains("champion")
        || key.contains("skin")
        || key.contains("path")
}

/// Case-insensitive lookup of an object member.
fn member<'a>(object: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    object
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(key))
        .and_then(|(_, v)| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
}

fn fold(name: &str) -> String {
    name.trim().to_lowercase()
}

/// `summonerInternalName` style: lower case without spaces.
fn internal(name: &str) -> String {
    fold(name).replace(' ', "")
}

/// One real player, as far as a payload describes them.
#[derive(Debug, Clone, Default)]
struct RealPlayer {
    game_name: Option<String>,
    tag_line: Option<String>,
    summoner_name: Option<String>,
    internal_name: Option<String>,
    puuid: Option<String>,
    local: bool,
}

impl RealPlayer {
    /// The player an object describes, if it has identity members.
    fn from_object(object: &Map<String, Value>) -> Option<Self> {
        let riot_id = member(object, "riotId");
        let mut game_name = member(object, "riotIdGameName");
        let mut tag_line = member(object, "riotIdTagLine").or(member(object, "tagLine"));
        // `gameName` alone is also the lobby name of a custom game; only a tag line
        // next to it makes it a Riot ID.
        if game_name.is_none() && member(object, "tagLine").is_some() {
            game_name = member(object, "gameName");
        }
        let summoner_name = member(object, "summonerName");
        for full in [riot_id, summoner_name].into_iter().flatten() {
            if let Some((game, tag)) = full.split_once('#') {
                game_name = game_name.or(Some(game));
                tag_line = tag_line.or(Some(tag));
            }
        }
        let game_name = game_name.or(summoner_name);
        let internal_name =
            member(object, "summonerInternalName").or(member(object, "internalName"));
        let puuid = member(object, "puuid");
        if game_name.is_none() && internal_name.is_none() && puuid.is_none() {
            return None;
        }
        let local = object
            .get("isLocalPlayer")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        Some(Self {
            game_name: game_name.map(str::to_owned),
            tag_line: tag_line.map(str::to_owned),
            summoner_name: summoner_name.map(str::to_owned),
            internal_name: internal_name.map(str::to_owned),
            puuid: puuid.map(str::to_owned),
            local,
        })
    }

    fn same_as(&self, other: &RealPlayer) -> bool {
        if let (Some(a), Some(b)) = (&self.puuid, &other.puuid) {
            return a == b;
        }
        let name = |p: &RealPlayer| p.game_name.as_deref().map(fold);
        match (name(self), name(other)) {
            (Some(a), Some(b)) if a == b => match (&self.tag_line, &other.tag_line) {
                (Some(x), Some(y)) => fold(x) == fold(y),
                _ => true,
            },
            (Some(_), Some(_)) => false,
            _ => {
                let key = |p: &RealPlayer| {
                    p.internal_name
                        .as_deref()
                        .or(p.game_name.as_deref())
                        .map(internal)
                };
                key(self).is_some() && key(self) == key(other)
            }
        }
    }

    fn merge(&mut self, other: RealPlayer) {
        self.game_name = self.game_name.take().or(other.game_name);
        self.tag_line = self.tag_line.take().or(other.tag_line);
        self.summoner_name = self.summoner_name.take().or(other.summoner_name);
        self.internal_name = self.internal_name.take().or(other.internal_name);
        self.puuid = self.puuid.take().or(other.puuid);
        self.local |= other.local;
    }
}

/// What a recording would leak if it were written as is.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("refusing to write {file}: it would contain {what}")]
pub struct Leak {
    pub file: String,
    pub what: &'static str,
}

/// Replaces player identities in Riot payloads, consistently across every payload of
/// one match. Built from all the payloads first, so the numbering doesn't depend on
/// which file is written first.
#[derive(Debug)]
pub struct Pseudonymizer {
    /// Folded `name#tag` → pseudonymous Riot ID.
    riot_ids: HashMap<String, String>,
    /// Folded game/summoner name → pseudonymous game name.
    names: HashMap<String, String>,
    /// Internal (folded, no spaces) name → pseudonymous internal name.
    internal_names: HashMap<String, String>,
    /// Every player found, with their pseudonymous tag line (`RR0` for a puuid-only
    /// entry, which has no name to replace).
    players: Vec<(RealPlayer, String)>,
    /// Names in name fields that matched nobody.
    unknown: HashMap<String, String>,
    /// Real game IDs (from `gameId` members). They also turn up inside other strings,
    /// such as the post-game chat's `"<gameId>-eog"`.
    game_ids: Vec<String>,
    /// Real Riot IDs, puuids, game IDs and secrets: replaced wherever they appear inside
    /// a string.
    sweep: Option<Regex>,
    sweep_with: HashMap<String, String>,
    secrets: Vec<String>,
}

impl Pseudonymizer {
    /// `local` is the `activeplayer` body (if any); `payloads` are every other body in
    /// the order players should be numbered (`playerlist` first). `secrets` (the League
    /// client password) must never appear in the output.
    pub fn new<'a>(
        local: Option<&Value>,
        payloads: impl IntoIterator<Item = &'a Value>,
        secrets: &[&str],
    ) -> Self {
        let mut found: Vec<RealPlayer> = Vec::new();
        let mut game_ids: Vec<String> = Vec::new();
        let mut add = |mut player: RealPlayer, is_local: bool| {
            player.local |= is_local;
            match found.iter_mut().find(|p| p.same_as(&player)) {
                Some(existing) => existing.merge(player),
                None => found.push(player),
            }
        };
        if let Some(Value::Object(object)) = local {
            if let Some(player) = RealPlayer::from_object(object) {
                add(player, true);
            }
        }
        for payload in payloads {
            visit_objects(payload, &mut |object| {
                if let Some(player) = RealPlayer::from_object(object) {
                    add(player, false);
                }
                for (key, value) in object {
                    if !is_game_id_key(&key.to_ascii_lowercase()) {
                        continue;
                    }
                    let id = match value {
                        Value::Number(n) => n.to_string(),
                        Value::String(s) => s.trim().to_owned(),
                        _ => continue,
                    };
                    // Too short to be a real one, and would match everywhere.
                    if id.len() >= 4 && !game_ids.contains(&id) {
                        game_ids.push(id);
                    }
                }
            });
        }
        // The local player first, everyone else in the order the payloads list them.
        found.sort_by_key(|p| !p.local);

        let mut this = Self {
            riot_ids: HashMap::new(),
            names: HashMap::new(),
            internal_names: HashMap::new(),
            players: Vec::new(),
            unknown: HashMap::new(),
            game_ids,
            sweep: None,
            sweep_with: HashMap::new(),
            secrets: secrets
                .iter()
                .filter(|s| !s.is_empty())
                .map(|s| (*s).to_owned())
                .collect(),
        };
        let mut numbered = 0;
        // Riot IDs as written. Lower-casing can change a name's length (Turkish `İ`
        // becomes `i̇`), and then a case-insensitive search for the lower-cased form
        // misses the original, so the sweep looks for both.
        let mut as_written: Vec<(String, String)> = Vec::new();
        // Players sharing a game name share the pseudonym and differ by tag, as Riot's
        // events (game name only) can't tell them apart either.
        let mut groups: Vec<(String, String, Vec<String>)> = Vec::new();
        for player in found {
            let key = player
                .game_name
                .as_deref()
                .or(player.internal_name.as_deref())
                .map(internal);
            let tag = player.tag_line.as_deref().map(fold).unwrap_or_default();
            let pseudo_tag = match key {
                None => format!("{TAG_PREFIX}0"),
                Some(key) => {
                    let index = match groups.iter().position(|(k, _, _)| *k == key) {
                        Some(index) => index,
                        None => {
                            let name = if player.local {
                                LOCAL_PLAYER.to_owned()
                            } else {
                                numbered += 1;
                                format!("{OTHER_PLAYER}{numbered}")
                            };
                            groups.push((key, name, Vec::new()));
                            groups.len() - 1
                        }
                    };
                    let (_, name, tags) = &mut groups[index];
                    let n = match tags.iter().position(|t| *t == tag) {
                        Some(n) => n + 1,
                        None => {
                            tags.push(tag.clone());
                            tags.len()
                        }
                    };
                    let pseudo_tag = format!("{TAG_PREFIX}{n}");
                    let name = name.clone();
                    if let Some(game) = &player.game_name {
                        this.names.insert(fold(game), name.clone());
                        if let Some(real_tag) = &player.tag_line {
                            let pseudo = format!("{name}#{pseudo_tag}");
                            as_written.push((format!("{game}#{real_tag}"), pseudo.clone()));
                            this.riot_ids
                                .insert(format!("{}#{}", fold(game), fold(real_tag)), pseudo);
                        }
                    }
                    if let Some(summoner) = &player.summoner_name {
                        if !summoner.contains('#') {
                            this.names.insert(fold(summoner), name.clone());
                        }
                    }
                    for real in [&player.internal_name, &player.game_name]
                        .into_iter()
                        .flatten()
                    {
                        this.internal_names.insert(internal(real), internal(&name));
                    }
                    pseudo_tag
                }
            };
            this.players.push((player, pseudo_tag));
        }

        let mut needles: Vec<(String, String)> = this
            .riot_ids
            .iter()
            .map(|(real, pseudo)| (real.clone(), pseudo.clone()))
            .collect();
        needles.extend(as_written);
        for (player, _) in &this.players {
            if let Some(puuid) = &player.puuid {
                needles.push((fold(puuid), "<puuid>".to_owned()));
            }
        }
        for id in &this.game_ids {
            needles.push((fold(id), PSEUDO_GAME_ID.to_string()));
        }
        for secret in &this.secrets {
            needles.push((fold(secret), "<redacted>".to_owned()));
        }
        // Longest first, so `Name#TAG` wins over anything it contains.
        needles.sort_by_key(|(real, _)| std::cmp::Reverse(real.len()));
        if !needles.is_empty() {
            let pattern = needles
                .iter()
                .map(|(real, _)| regex::escape(real))
                .collect::<Vec<_>>()
                .join("|");
            this.sweep = Regex::new(&format!("(?i){pattern}")).ok();
        }
        this.sweep_with = needles.into_iter().collect();
        this
    }

    /// The pseudonymous Riot ID of the local player, if the payloads named one.
    pub fn local_riot_id(&self) -> Option<String> {
        self.players
            .iter()
            .find(|(p, _)| p.local)
            .map(|(_, tag)| format!("{LOCAL_PLAYER}#{tag}"))
    }

    /// Pseudonymises one payload in place.
    pub fn apply(&mut self, value: &mut Value) {
        self.walk(value, None);
    }

    /// A pseudonymised copy of a WebSocket frame (JSON text).
    pub fn apply_frame(&mut self, frame: &str) -> String {
        match serde_json::from_str::<Value>(frame) {
            Ok(mut value) => {
                self.apply(&mut value);
                value.to_string()
            }
            Err(_) => self.swept(frame),
        }
    }

    fn walk(&mut self, value: &mut Value, key: Option<&str>) {
        match value {
            Value::Object(object) => {
                let tag = RealPlayer::from_object(object).map(|p| self.tag_of(&p));
                // End-of-game players: `level` is the summoner level there.
                let account_level = object.contains_key("isLocalPlayer");
                let keys: Vec<String> = object.keys().cloned().collect();
                for key in keys {
                    let folded = key.to_ascii_lowercase();
                    if is_dropped_key(&folded) || (account_level && folded == "level") {
                        object.remove(&key);
                        continue;
                    }
                    let Some(member) = object.get_mut(&key) else {
                        continue;
                    };
                    if is_game_id_key(&folded) {
                        match member {
                            Value::Number(_) => *member = PSEUDO_GAME_ID.into(),
                            Value::String(_) => *member = PSEUDO_GAME_ID.to_string().into(),
                            _ => {}
                        }
                    } else if TAG_KEYS.contains(&folded.as_str()) {
                        if let Value::String(text) = member {
                            if !text.trim().is_empty() {
                                *text = tag.clone().unwrap_or_else(|| format!("{TAG_PREFIX}0"));
                            }
                        }
                    } else if NAME_KEYS.contains(&folded.as_str()) {
                        self.name_field(member);
                    } else {
                        self.walk(member, Some(&folded));
                    }
                }
            }
            Value::Array(items) => {
                for item in items {
                    self.walk(item, key);
                }
            }
            Value::String(text) => {
                let mut replaced = self.swept(text);
                if !key.is_some_and(is_vocabulary_key) {
                    if let Some(name) = self.known_name(&replaced) {
                        replaced = name;
                    }
                }
                *text = replaced;
            }
            Value::Number(number) => {
                let game_id = self.game_ids.contains(&number.to_string());
                if game_id {
                    *value = PSEUDO_GAME_ID.into();
                }
            }
            _ => {}
        }
    }

    /// A value of a name field: a known player gets their pseudonym, a game object
    /// keeps its name, and anything else becomes `Unknown<n>`.
    fn name_field(&mut self, value: &mut Value) {
        match value {
            Value::String(text) => {
                let trimmed = text.trim();
                if trimmed.is_empty() || is_game_object(trimmed) {
                    return;
                }
                *text = match self.known_name(trimmed) {
                    Some(pseudo) => pseudo,
                    None => {
                        let next = self.unknown.len() + 1;
                        self.unknown
                            .entry(fold(trimmed))
                            .or_insert_with(|| format!("{UNKNOWN_NAME}{next}"))
                            .clone()
                    }
                };
            }
            Value::Array(items) => items.iter_mut().for_each(|item| self.name_field(item)),
            other => self.walk(other, None),
        }
    }

    fn known_name(&self, text: &str) -> Option<String> {
        let folded = fold(text);
        if let Some(pseudo) = self
            .riot_ids
            .get(&folded)
            .or_else(|| self.names.get(&folded))
        {
            return Some(pseudo.clone());
        }
        if let Some(pseudo) = self.internal_names.get(&folded) {
            return Some(pseudo.clone());
        }
        // `Name#TAG` whose tag we haven't seen with that name.
        let (game, _) = folded.split_once('#')?;
        self.names
            .get(game)
            .map(|pseudo| format!("{pseudo}#{TAG_PREFIX}0"))
    }

    fn tag_of(&self, player: &RealPlayer) -> String {
        self.players
            .iter()
            .find(|(p, _)| p.same_as(player))
            .map_or_else(|| format!("{TAG_PREFIX}0"), |(_, tag)| tag.clone())
    }

    fn swept(&self, text: &str) -> String {
        let text = JWT.replace_all(text, "<redacted>");
        match &self.sweep {
            Some(sweep) if sweep.is_match(&text) => sweep
                .replace_all(&text, |caps: &regex::Captures<'_>| {
                    self.sweep_with
                        .get(&fold(&caps[0]))
                        .cloned()
                        .unwrap_or_default()
                })
                .into_owned(),
            _ => text.into_owned(),
        }
    }

    /// What `text` would leak: the League client password, a token, a real Riot ID,
    /// puuid or game ID.
    fn leak_in(&self, text: &str) -> Option<&'static str> {
        let folded = text.to_lowercase();
        if self.secrets.iter().any(|s| text.contains(s.as_str())) {
            return Some("the League client password");
        }
        if JWT.is_match(text) {
            return Some("a token");
        }
        if self.riot_ids.keys().any(|id| folded.contains(id.as_str())) {
            return Some("a real Riot ID");
        }
        let mut puuids = self.players.iter().filter_map(|(p, _)| p.puuid.as_deref());
        if puuids.any(|p| folded.contains(&fold(p))) {
            return Some("a puuid");
        }
        if self.game_ids.iter().any(|id| text.contains(id.as_str())) {
            return Some("a real game ID");
        }
        None
    }

    /// Last line of defence after [`Self::apply`]: any string or member name that still
    /// holds an identifier is replaced with `<redacted>`. Returns where (paths such as
    /// `playerlist[3].someField`, never the values), so the gap can be fixed.
    pub fn redact_leftovers(&self, value: &mut Value, path: &str) -> Vec<String> {
        let mut found = Vec::new();
        self.redact_into(value, path, &mut found);
        found
    }

    fn redact_into(&self, value: &mut Value, path: &str, found: &mut Vec<String>) {
        match value {
            Value::String(text) => {
                if let Some(what) = self.leak_in(text) {
                    found.push(format!("{path} ({what})"));
                    *text = "<redacted>".to_owned();
                }
            }
            Value::Array(items) => {
                for (index, item) in items.iter_mut().enumerate() {
                    self.redact_into(item, &format!("{path}[{index}]"), found);
                }
            }
            Value::Object(object) => {
                let keys: Vec<String> = object.keys().cloned().collect();
                for key in keys {
                    let (member_path, current) = match self.leak_in(&key) {
                        None => (format!("{path}.{key}"), key),
                        Some(what) => {
                            let renamed = format!("<redacted {}>", found.len() + 1);
                            found.push(format!("{path}.{renamed} (a member name: {what})"));
                            if let Some(member) = object.remove(&key) {
                                object.insert(renamed.clone(), member);
                            }
                            (format!("{path}.{renamed}"), renamed)
                        }
                    };
                    if let Some(member) = object.get_mut(&current) {
                        self.redact_into(member, &member_path, found);
                    }
                }
            }
            _ => {}
        }
    }

    /// What `text` (a file about to be written) would leak: a real Riot ID, puuid or game
    /// ID, a secret or token, or an identifier member that should have been dropped.
    pub fn check(&self, file: &str, text: &str) -> Result<(), Leak> {
        let leak = |what| {
            Err(Leak {
                file: file.to_owned(),
                what,
            })
        };
        if let Some(what) = self.leak_in(text) {
            return leak(what);
        }
        let folded = text.to_lowercase();
        for key in [
            "\"puuid\":",
            "summonerid\":",
            "accountid\":",
            "password\":",
            "jwt\":",
        ] {
            if folded.contains(key) {
                return leak("an identifier or credential member");
            }
        }
        Ok(())
    }
}

/// Calls `f` on every object in `value`, outermost first, in document order.
fn visit_objects(value: &Value, f: &mut impl FnMut(&Map<String, Value>)) {
    match value {
        Value::Object(object) => {
            f(object);
            object.values().for_each(|v| visit_objects(v, f));
        }
        Value::Array(items) => items.iter().for_each(|v| visit_objects(v, f)),
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Recording
// ---------------------------------------------------------------------------

/// Raw payloads of one match, as the APIs returned them.
#[derive(Debug, Clone, Default)]
pub struct Recording {
    pub activeplayer: Option<Value>,
    pub activeplayer_error: Option<Value>,
    pub playerlist: Option<Value>,
    pub gamestats: Option<Value>,
    /// `eventdata.Events`.
    pub events: Vec<Value>,
    pub lcu_session: Option<Value>,
    pub eog: Option<Value>,
    /// Gameflow-phase WebSocket frames, in arrival order.
    pub frames: Vec<String>,
}

/// The files of one recording, pseudonymised and checked: relative path → contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixtureFiles {
    pub files: Vec<(PathBuf, String)>,
    /// Where identifiers survived pseudonymisation and were redacted instead (paths,
    /// never values). Should be empty; anything here is a gap to fix.
    pub redacted: Vec<String>,
}

impl FixtureFiles {
    pub fn get(&self, path: impl AsRef<Path>) -> Option<&str> {
        self.files
            .iter()
            .find(|(p, _)| p == path.as_ref())
            .map(|(_, text)| text.as_str())
    }

    /// Writes every file under `dir`, each atomically (temp file + rename).
    pub fn write(&self, dir: &Path) -> io::Result<Vec<PathBuf>> {
        let mut written = Vec::new();
        for (relative, text) in &self.files {
            let path = dir.join(relative);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let temp = path.with_extension("json.tmp");
            std::fs::write(&temp, text)?;
            std::fs::rename(&temp, &path)?;
            written.push(path);
        }
        Ok(written)
    }
}

impl Recording {
    /// The game's mode (`gamestats.gameMode`), e.g. "CLASSIC".
    pub fn game_mode(&self) -> Option<&str> {
        self.gamestats.as_ref()?["gameMode"]
            .as_str()
            .filter(|m| !m.is_empty())
    }

    fn game_end_result(&self) -> Option<&str> {
        self.events
            .iter()
            .find(|e| e["EventName"] == "GameEnd")
            .and_then(|e| e["Result"].as_str())
    }

    /// Local player's champion and team from `playerlist`.
    fn local_champion(&self, pseudo: &Pseudonymizer, players: &Value) -> Option<(String, String)> {
        let me = pseudo.local_riot_id()?;
        players.as_array()?.iter().find_map(|p| {
            (p["riotId"].as_str() == Some(me.as_str())).then(|| {
                (
                    p["championName"].as_str().unwrap_or("?").to_owned(),
                    p["team"].as_str().unwrap_or("?").to_owned(),
                )
            })
        })
    }

    /// Pseudonymises everything and lays it out as fixture files: `<stem>.json` and,
    /// when the League client was there, `lcu/<stem>.session.json`, `.eog.json` and
    /// `.frames.json`. Fails rather than produce a file that would leak.
    pub fn fixture_files(
        &self,
        stem: &str,
        recorded_at: SystemTime,
        secrets: &[&str],
    ) -> Result<FixtureFiles, Leak> {
        let lcu = [&self.lcu_session, &self.eog];
        let mut frames: Vec<Value> = Vec::new();
        for frame in &self.frames {
            frames.push(serde_json::from_str(frame).unwrap_or_default());
        }
        let payloads = [&self.playerlist, &self.activeplayer_error]
            .into_iter()
            .chain(lcu)
            .flatten()
            .chain(self.events.iter())
            .chain(frames.iter());
        let mut pseudo = Pseudonymizer::new(self.activeplayer.as_ref(), payloads, secrets);

        let mut redacted = Vec::new();
        let mut copy = |value: &Option<Value>, path: &str| {
            value.clone().map(|mut value| {
                pseudo.apply(&mut value);
                redacted.extend(pseudo.redact_leftovers(&mut value, path));
                value
            })
        };
        let activeplayer = copy(&self.activeplayer, "activeplayer").unwrap_or(Value::Null);
        let activeplayer_error = copy(&self.activeplayer_error, "activeplayerError");
        let playerlist = copy(&self.playerlist, "playerlist").unwrap_or(Value::Array(Vec::new()));
        let mut gamestats =
            copy(&self.gamestats, "gamestats").unwrap_or_else(|| Value::Object(Map::new()));
        // As in the synthetic fixtures: the replayer and fake server fill in the clock.
        if let Some(stats) = gamestats.as_object_mut() {
            stats.insert("gameTime".into(), 0.0.into());
        }
        let session = copy(&self.lcu_session, "session");
        let eog = copy(&self.eog, "eog");
        let mut events = Vec::new();
        for (index, event) in self.events.iter().enumerate() {
            let mut event = event.clone();
            pseudo.apply(&mut event);
            redacted.extend(pseudo.redact_leftovers(&mut event, &format!("events[{index}]")));
            events.push(event);
        }
        let mut frames = Vec::new();
        for (index, frame) in self.frames.iter().enumerate() {
            let mut frame = pseudo.apply_frame(frame);
            if let Some(what) = pseudo.leak_in(&frame) {
                redacted.push(format!("frames[{index}] ({what})"));
                frame = "<redacted>".to_owned();
            }
            frames.push(frame);
        }

        let mut description = format!(
            "Recorded {} game on {} with riftreel-dev record",
            self.game_mode().unwrap_or("unknown"),
            utc_minute(recorded_at)
        );
        if let Some((champion, team)) = self.local_champion(&pseudo, &playerlist) {
            let _ = write!(description, ": {champion} ({team})");
        }
        if let Some(result) = self.game_end_result() {
            let _ = write!(description, ", {result}");
        }
        description.push_str(". Players pseudonymised; ");
        description.push_str(&match pseudo.local_riot_id() {
            Some(me) => format!("{me} is the local player"),
            None => "no local player (spectator)".to_owned(),
        });

        let mut files = vec![(
            PathBuf::from(format!("{stem}.json")),
            fixture_json(
                &description,
                &activeplayer,
                activeplayer_error.as_ref(),
                &playerlist,
                &gamestats,
                &events,
            ),
        )];
        let lcu_dir = Path::new("lcu");
        if let Some(session) = session {
            files.push((
                lcu_dir.join(format!("{stem}.session.json")),
                pretty(&session),
            ));
        }
        if let Some(eog) = eog {
            files.push((lcu_dir.join(format!("{stem}.eog.json")), pretty(&eog)));
        }
        if !frames.is_empty() {
            files.push((lcu_dir.join(format!("{stem}.frames.json")), pretty(&frames)));
        }
        for (path, text) in &files {
            pseudo.check(&path.display().to_string(), text)?;
        }
        Ok(FixtureFiles { files, redacted })
    }
}

fn pretty(value: &impl serde::Serialize) -> String {
    let mut text = serde_json::to_string_pretty(value).expect("JSON values always serialize");
    text.push('\n');
    text
}

/// The layout of the synthetic fixtures: one player or event per line.
fn fixture_json(
    description: &str,
    activeplayer: &Value,
    activeplayer_error: Option<&Value>,
    playerlist: &Value,
    gamestats: &Value,
    events: &[Value],
) -> String {
    fn lines(out: &mut String, items: &[Value]) {
        out.push_str("[\n");
        for (index, item) in items.iter().enumerate() {
            let comma = if index + 1 < items.len() { "," } else { "" };
            let _ = writeln!(out, "    {item}{comma}");
        }
        out.push_str("  ]");
    }
    let mut out = String::from("{\n");
    let _ = writeln!(out, "  \"description\": {},", Value::from(description));
    let _ = writeln!(out, "  \"activeplayer\": {activeplayer},");
    if let Some(error) = activeplayer_error {
        let _ = writeln!(out, "  \"activeplayerError\": {error},");
    }
    out.push_str("  \"playerlist\": ");
    match playerlist.as_array() {
        Some(players) => lines(&mut out, players),
        None => out.push_str(&playerlist.to_string()),
    }
    let _ = writeln!(out, ",\n  \"gamestats\": {gamestats},");
    out.push_str("  \"events\": ");
    lines(&mut out, events);
    out.push_str("\n}\n");
    out
}

/// `2026-10-15 16:12 UTC`
fn utc_minute(time: SystemTime) -> String {
    let (date, hour, minute) = utc_parts(time);
    format!("{date} {hour:02}:{minute:02} UTC")
}

/// `20261015-1612` (UTC), for file names.
fn utc_stamp(time: SystemTime) -> String {
    let (date, hour, minute) = utc_parts(time);
    format!("{}-{hour:02}{minute:02}", date.replace('-', ""))
}

pub(crate) fn utc_parts(time: SystemTime) -> (String, u64, u64) {
    let secs = time
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    let days = (secs / 86_400) as i64;
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (
        format!("{year:04}-{month:02}-{day:02}"),
        secs % 86_400 / 3600,
        secs % 3600 / 60,
    )
}

// ---------------------------------------------------------------------------
// Recording loop
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct RecorderTimings {
    /// Live Client API poll and League client phase check.
    pub poll: Duration,
    /// `playerlist`/`activeplayer` are re-read every this many polls.
    pub roster_every: u32,
    /// The game counts as closed once the Live Client API has been gone this long.
    pub live_gone: Duration,
    /// After the game closed, how long to wait for the end-of-game stats.
    pub eog_wait: Duration,
    /// Checkpoints are written at most this often (and when the recording ends).
    pub checkpoint: Duration,
}

impl Default for RecorderTimings {
    fn default() -> Self {
        Self {
            poll: Duration::from_secs(1),
            roster_every: 5,
            live_gone: Duration::from_secs(5),
            eog_wait: Duration::from_secs(180),
            checkpoint: Duration::from_secs(5),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RecordError {
    #[error(transparent)]
    Leak(#[from] Leak),
    #[error("cannot write the recording: {0}")]
    Io(#[from] io::Error),
}

/// What [`record`] saved.
#[derive(Debug, Clone)]
pub struct Recorded {
    pub fixture: PathBuf,
    pub files: Vec<PathBuf>,
    pub events: usize,
    pub has_eog: bool,
    /// See [`FixtureFiles::redacted`].
    pub redacted: Vec<String>,
}

/// Records the next match: waits for the Live Client API, records until the game
/// closes, then (with the League client) waits for the end-of-game stats. The files in
/// `out_dir` are rewritten as the match goes, so stopping the recorder early keeps what
/// it had. `progress` gets human-readable lines without player names.
pub async fn record(
    live: &LiveClient,
    lcu: Option<&LcuClient>,
    out_dir: &Path,
    timings: &RecorderTimings,
    mut progress: impl FnMut(String),
) -> Result<Recorded, RecordError> {
    let mut frames = match lcu {
        Some(client) => match client.subscribe_frames(GAMEFLOW_PHASE_EVENT).await {
            Ok(frames) => Some(frames),
            Err(e) => {
                progress(format!("no gameflow-phase frames: {e}"));
                None
            }
        },
        None => None,
    };
    let secret = lcu.map(|c| c.credentials().password().to_owned());
    let mut state = LoopState::default();
    let mut ticker = time::interval(timings.poll);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    progress(format!("waiting for a game on {}", live.base_url()));

    loop {
        tokio::select! {
            frame = next_frame(&mut frames) => match frame {
                Some(frame) => {
                    if let Some(phase) = parse_phase_event(&frame) {
                        progress(format!("client phase {phase}"));
                    }
                    state.recording.frames.push(frame);
                    state.dirty = true;
                }
                None => {
                    frames = None;
                    progress("League client WebSocket closed".to_owned());
                }
            },
            _ = ticker.tick() => {
                state.poll_live(live, timings, &mut progress).await;
                if let Some(lcu) = lcu {
                    state.poll_lcu(lcu, &mut progress).await;
                }
                if state.done(lcu.is_some(), timings, &mut progress) {
                    break;
                }
                if state.dirty && state.last_write.is_none_or(|t| t.elapsed() >= timings.checkpoint) {
                    // Keep recording either way; the final write reports the error.
                    let reported = state.redacted.clone();
                    if let Err(e) = state.write(out_dir, secret.as_deref()) {
                        progress(format!("checkpoint not written: {e}"));
                        state.last_write = Some(Instant::now());
                    }
                    if state.redacted != reported && !state.redacted.is_empty() {
                        progress(format!(
                            "redacted identifiers pseudonymisation missed (please report): {}",
                            state.redacted.join("; ")
                        ));
                    }
                }
            }
        }
    }
    let (stem, _) = state
        .started
        .clone()
        .expect("done only after a game started");
    let files = state.write(out_dir, secret.as_deref())?;
    Ok(Recorded {
        fixture: out_dir.join(format!("{stem}.json")),
        files,
        events: state.recording.events.len(),
        has_eog: state.recording.eog.is_some(),
        redacted: state.redacted.clone(),
    })
}

async fn next_frame(frames: &mut Option<FrameSubscription>) -> Option<String> {
    match frames {
        Some(frames) => frames.next().await,
        None => std::future::pending().await,
    }
}

#[derive(Default)]
struct LoopState {
    recording: Recording,
    /// File stem and wall time, once the game was found.
    started: Option<(String, SystemTime)>,
    last_live_ok: Option<Instant>,
    live_closed: Option<Instant>,
    game_ended: bool,
    polls: u32,
    phase: Option<GameflowPhase>,
    session_polls: u32,
    stale_eog_seen: bool,
    dirty: bool,
    last_write: Option<Instant>,
    redacted: Vec<String>,
}

impl LoopState {
    async fn poll_live(
        &mut self,
        live: &LiveClient,
        timings: &RecorderTimings,
        progress: &mut impl FnMut(String),
    ) {
        let stats = match live.get_raw("gamestats").await {
            Ok(response) if response.status == 200 => response.body,
            // Answering but not ready (loading screen).
            Ok(_) => return,
            Err(_) => {
                if let Some(last) = self.last_live_ok {
                    if self.live_closed.is_none() && last.elapsed() >= timings.live_gone {
                        self.live_closed = Some(Instant::now());
                        progress("game closed".to_owned());
                    }
                }
                return;
            }
        };
        // Back after a crash and reconnect.
        if self.live_closed.take().is_some() {
            progress("game back".to_owned());
        }
        if self.started.is_none() {
            let mode = stats["gameMode"].as_str().unwrap_or_default();
            // Teamfight Tactics serves the API too; it isn't League.
            if mode.eq_ignore_ascii_case("TFT") {
                return;
            }
            let now = SystemTime::now();
            let mode = if mode.is_empty() { "game" } else { mode };
            let stem = format!("{}-{}", mode.to_lowercase(), utc_stamp(now));
            progress(format!("game found: mode {mode}, recording as {stem}"));
            self.started = Some((stem, now));
        }
        self.last_live_ok = Some(Instant::now());
        if self.recording.gamestats.as_ref() != Some(&stats) {
            self.recording.gamestats = Some(stats);
        }

        let mut ended_now = false;
        if let Ok(response) = live.get_raw("eventdata").await {
            if let Some(events) = response.body["Events"].as_array() {
                // A restarted game may serve a partial history while it loads.
                if events.len() > self.recording.events.len() {
                    for event in &events[self.recording.events.len()..] {
                        let name = event["EventName"].as_str().unwrap_or("?");
                        let time = event["EventTime"].as_f64().unwrap_or_default();
                        progress(format!("{}  {name}", crate::replay::clock(time)));
                        if name == "GameEnd" && !self.game_ended {
                            self.game_ended = true;
                            ended_now = true;
                        }
                    }
                    self.recording.events = events.clone();
                    self.dirty = true;
                }
            }
        }

        let roster_due = self.polls.is_multiple_of(timings.roster_every.max(1));
        self.polls += 1;
        if roster_due || ended_now || self.recording.playerlist.is_none() {
            if let Ok(response) = live.get_raw("playerlist").await {
                if response.status == 200 && response.body.is_array() {
                    self.recording.playerlist = Some(response.body);
                    self.dirty = true;
                }
            }
            if let Ok(response) = live.get_raw("activeplayer").await {
                if response.status == 200 {
                    self.recording.activeplayer = Some(response.body);
                    self.recording.activeplayer_error = None;
                    self.dirty = true;
                } else if self.recording.activeplayer.is_none() {
                    self.recording.activeplayer_error = Some(response.body);
                }
            }
        }
    }

    async fn poll_lcu(&mut self, lcu: &LcuClient, progress: &mut impl FnMut(String)) {
        if let Ok(phase) = lcu.get_gameflow_phase().await {
            self.phase = Some(phase);
        }
        let post_game = self.phase.as_ref().is_some_and(GameflowPhase::is_post_game);
        // The session as it was during the game, not the end-of-game screen's.
        let in_game =
            self.started.is_some() && self.live_closed.is_none() && !self.game_ended && !post_game;
        if in_game {
            if self.session_polls.is_multiple_of(30) {
                if let Ok(session) = lcu.get_json::<Value>("/lol-gameflow/v1/session").await {
                    if self.recording.lcu_session.as_ref() != Some(&session) {
                        self.recording.lcu_session = Some(session);
                        self.dirty = true;
                    }
                }
            }
            self.session_polls += 1;
        }
        if self.started.is_some() && (post_game || self.live_closed.is_some()) {
            if let Ok(eog) = lcu
                .get_json::<Value>("/lol-end-of-game/v1/eog-stats-block")
                .await
            {
                let game_id = |v: &Value| v["gameId"].as_u64().filter(|id| *id != 0);
                let this_game = self.recording.lcu_session.as_ref().map(|s| &s["gameData"]);
                // Still the previous match's block (the session names this one).
                if this_game
                    .and_then(game_id)
                    .is_some_and(|id| game_id(&eog) != Some(id))
                {
                    if !self.stale_eog_seen {
                        self.stale_eog_seen = true;
                        progress("ignoring end-of-game stats of another game".to_owned());
                    }
                    return;
                }
                if self.recording.eog.is_none() {
                    progress("end-of-game stats received".to_owned());
                }
                if self.recording.eog.as_ref() != Some(&eog) {
                    self.recording.eog = Some(eog);
                    self.dirty = true;
                }
            }
        }
    }

    fn done(
        &self,
        with_lcu: bool,
        timings: &RecorderTimings,
        progress: &mut impl FnMut(String),
    ) -> bool {
        let Some(closed) = self.live_closed else {
            return false;
        };
        if !with_lcu {
            return true;
        }
        let phase = self.phase.as_ref();
        // Still in the game or on its way to the end-of-game screen.
        let in_flow = matches!(
            phase,
            Some(
                GameflowPhase::InProgress
                    | GameflowPhase::Reconnect
                    | GameflowPhase::GameStart
                    | GameflowPhase::WaitingForStats
                    | GameflowPhase::PreEndOfGame
            )
        );
        // Stats in hand: keep going until the client shows them (EndOfGame), so the
        // frames include the whole sequence.
        if self.recording.eog.is_some() && !in_flow {
            return true;
        }
        if closed.elapsed() >= timings.eog_wait {
            if self.recording.eog.is_none() {
                progress(format!(
                    "no end-of-game stats after {} s; saving without them",
                    timings.eog_wait.as_secs()
                ));
            }
            return true;
        }
        // The client moved on without stats (Practice Tool, or the screen was skipped).
        let moved_on = !in_flow && phase != Some(&GameflowPhase::EndOfGame);
        moved_on && closed.elapsed() >= timings.live_gone
    }

    fn write(&mut self, out_dir: &Path, secret: Option<&str>) -> Result<Vec<PathBuf>, RecordError> {
        let Some((stem, started)) = &self.started else {
            return Ok(Vec::new());
        };
        let secrets: Vec<&str> = secret.into_iter().collect();
        let files = self.recording.fixture_files(stem, *started, &secrets)?;
        let written = files.write(out_dir)?;
        self.redacted = files.redacted;
        self.dirty = false;
        self.last_write = Some(Instant::now());
        Ok(written)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::replay::{replay, replay_with, Fixture, LcuRecording};
    use crate::tracker::normalize::MarkerConfig;
    use crate::tracker::session::{ResultSource, SessionConfig};

    const PASSWORD: &str = "Lcu-Pa55word_zQ9";
    /// Shaped like the post-game chat token the real end-of-game block carries.
    const CHAT_JWT: &str = "eyJraWQiOiIxIn0.eyJzdWIiOiJyZWFsLWFjY291bnQifQ.c2lnbmF0dXJlLWJ5dGVz";
    const PUUID_ME: &str =
        "c0ffee00-1111-2222-3333-444455556666-local-player-puuid-0000000000000000000";
    const PUUID_YASUO: &str =
        "deadbeef-1111-2222-3333-444455556666-other-player-puuid-0000000000000000000";

    /// Real-looking identities: one named like a champion, one like a dragon type, two
    /// sharing a game name, one spectator-ish stranger in the events.
    const REAL: [(&str, &str, &str, &str); 6] = [
        ("Faker Fan", "XQZ7", "ORDER", "Shyvana"),
        ("Yasuo", "QWV3", "ORDER", "Yasuo"),
        ("Fire", "ZZK9", "ORDER", "Lulu"),
        ("Twin", "JJA1", "CHAOS", "Zed"),
        ("Twin", "JJA2", "CHAOS", "Ahri"),
        ("Grumpy Otter", "OTR5", "CHAOS", "Darius"),
    ];

    fn player(game: &str, tag: &str, team: &str, champion: &str) -> Value {
        json!({
            "championName": champion, "rawChampionName": format!("game_character_displayname_{champion}"),
            "isBot": false, "level": 18, "position": "", "team": team,
            "riotId": format!("{game}#{tag}"), "riotIdGameName": game, "riotIdTagLine": tag,
            "summonerName": format!("{game}#{tag}"),
            "scores": {"kills": 0, "deaths": 0, "assists": 0, "creepScore": 0},
            "summonerSpells": {"summonerSpellOne": {"displayName": "Smite"}}
        })
    }

    fn eog_player(game: &str, tag: &str, champion: &str, puuid: &str) -> Value {
        json!({
            "championName": champion, "puuid": puuid, "summonerId": 4_242_424_242u64,
            "summonerName": game, "riotIdGameName": game, "riotIdTagLine": tag,
            "profileIconId": 29, "stats": {"CHAMPIONS_KILLED": 2, "NUM_DEATHS": 1, "ASSISTS": 1, "WIN": 1}
        })
    }

    fn event(id: u64, name: &str, time: f64, extra: Value) -> Value {
        let mut event = json!({"EventID": id, "EventName": name, "EventTime": time});
        event
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        event
    }

    fn real_recording() -> Recording {
        let (me, me_tag, _, _) = REAL[0];
        let mut local = eog_player(me, me_tag, "Shyvana", PUUID_ME);
        local["isLocalPlayer"] = true.into();
        local["stats"] =
            json!({"CHAMPIONS_KILLED": 2, "NUM_DEATHS": 1, "ASSISTS": 1, "WIN": 1, "LEVEL": 18});
        local["level"] = 1055.into();
        local["note"] = format!("chat token {CHAT_JWT}").into();
        Recording {
            activeplayer: Some(json!({
                "riotId": "Faker Fan#XQZ7", "riotIdGameName": "Faker Fan", "riotIdTagLine": "XQZ7",
                "summonerName": "Faker Fan#XQZ7", "level": 18,
                "abilities": {"Q": {"displayName": "Twin Bite", "id": "ShyvanaDoubleAttack"}}
            })),
            activeplayer_error: None,
            playerlist: Some(Value::Array(
                REAL.iter()
                    .map(|(game, tag, team, champion)| player(game, tag, team, champion))
                    .collect(),
            )),
            gamestats: Some(json!({"gameMode": "CLASSIC", "gameTime": 1805.2, "mapName": "Map11", "mapNumber": 11, "mapTerrain": "Infernal"})),
            events: vec![
                event(0, "GameStart", 0.03, json!({})),
                event(1, "ChampionKill", 100.0, json!({"KillerName": "Faker Fan", "VictimName": "Twin", "Assisters": ["Yasuo"]})),
                event(2, "DragonKill", 300.0, json!({"DragonType": "Fire", "Stolen": "False", "KillerName": "Fire", "Assisters": ["Faker Fan"]})),
                event(3, "HordeKill", 380.0, json!({"Stolen": "False", "KillerName": "Faker Fan", "Assisters": []})),
                event(4, "ChampionKill", 500.0, json!({"KillerName": "Yasuo", "VictimName": "Grumpy Otter", "Assisters": ["Faker Fan", "Fire"]})),
                event(5, "ChampionKill", 600.0, json!({"KillerName": "Turret_T2_C_05_A", "VictimName": "Faker Fan", "Assisters": []})),
                event(6, "ChampionKill", 700.0, json!({"KillerName": "Mystery Man", "VictimName": "Faker Fan", "Assisters": ["Twin"]})),
                event(7, "ChampionKill", 800.0, json!({"KillerName": "Faker Fan", "VictimName": "Twin", "Assisters": []})),
                event(8, "Multikill", 800.0, json!({"KillerName": "Faker Fan", "KillStreak": 2})),
                event(9, "Ace", 801.0, json!({"Acer": "Faker Fan", "AcingTeam": "ORDER"})),
                event(10, "GameEnd", 1800.0, json!({"Result": "Win"})),
            ],
            lcu_session: Some(json!({
                "gameData": {
                    "gameId": 7_123_456_789u64, "gameName": "", "isCustomGame": false, "password": "",
                    "playerChampionSelections": [{"championId": 102, "puuid": PUUID_ME, "summonerInternalName": "fakerfan"}],
                    "queue": {"id": 420, "gameMode": "CLASSIC", "type": "RANKED_SOLO_5x5", "description": "Ranked Solo/Duo", "name": "Ranked Solo/Duo"},
                    "teamOne": [
                        {"championId": 102, "puuid": PUUID_ME, "summonerId": 1, "summonerName": "", "gameName": "Faker Fan", "tagLine": "XQZ7"},
                        {"championId": 157, "puuid": PUUID_YASUO, "summonerId": 2, "summonerName": "", "gameName": "Yasuo", "tagLine": "QWV3"}
                    ],
                    "teamTwo": []
                },
                "map": {"id": 11, "gameMode": "CLASSIC", "name": "Summoner's Rift"},
                "phase": "InProgress"
            })),
            eog: Some(json!({
                "gameId": 7_123_456_789u64, "reportGameId": 7_123_456_789u64, "accountId": 99, "gameLength": 1801,
                "gameMode": "CLASSIC", "queueType": "RANKED_SOLO_5x5",
                "currentLevel": 1055, "previousLevel": 1055, "experienceTotal": 1711,
                "mucJwtDto": {"channelClaim": "7123456789-eog", "jwt": CHAT_JWT, "targetRegion": "na1"},
                "multiUserChatId": "7123456789-eog", "multiUserChatPassword": CHAT_JWT,
                "localPlayer": local,
                "teams": [
                    {"teamId": 100, "isPlayerTeam": true, "isWinningTeam": true, "name": "", "players": [eog_player("Yasuo", "QWV3", "Yasuo", PUUID_YASUO)]},
                    {"teamId": 200, "isPlayerTeam": false, "isWinningTeam": false, "name": "", "players": [eog_player("Grumpy Otter", "OTR5", "Darius", "otter-puuid-0000000000000000000000000000000000")]}
                ]
            })),
            frames: vec![
                "[0,\"7e1a8c2f-0000-4000-8000-00000000d0d0\",1,\"Riot Games LCU\"]".into(),
                "[8,\"OnJsonApiEvent_lol-gameflow_v1_gameflow-phase\",{\"data\":\"InProgress\",\"eventType\":\"Update\",\"uri\":\"/lol-gameflow/v1/gameflow-phase\"}]".into(),
                format!("[8,\"OnJsonApiEvent_debug\",{{\"data\":\"--remoting-auth-token={PASSWORD}\"}}]"),
            ],
        }
    }

    fn at() -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(1_792_080_720) // 2026-10-15 16:12 UTC
    }

    fn files() -> FixtureFiles {
        real_recording()
            .fixture_files("classic-test", at(), &[PASSWORD])
            .unwrap()
    }

    #[test]
    fn writes_the_fixture_layout() {
        let files = files();
        let paths: Vec<String> = files
            .files
            .iter()
            .map(|(p, _)| p.display().to_string().replace('\\', "/"))
            .collect();
        assert_eq!(
            paths,
            [
                "classic-test.json",
                "lcu/classic-test.session.json",
                "lcu/classic-test.eog.json",
                "lcu/classic-test.frames.json"
            ]
        );
        let fixture = Fixture::parse(files.get("classic-test.json").unwrap()).unwrap();
        assert!(fixture.description.starts_with(
            "Recorded CLASSIC game on 2026-10-15 16:12 UTC with riftreel-dev record: Shyvana (ORDER), Win."
        ), "{}", fixture.description);
        assert_eq!(fixture.events.len(), 11);
        assert_eq!(
            fixture.gamestats["gameTime"], 0.0,
            "the replayer sets the clock"
        );
        let frames: Vec<String> = serde_json::from_str(
            files
                .get(Path::new("lcu").join("classic-test.frames.json"))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(frames.len(), 3);
        assert_eq!(
            crate::league::client::parse_phase_event(&frames[1]),
            Some(GameflowPhase::InProgress)
        );
    }

    #[test]
    fn no_real_name_tag_or_id_survives() {
        let files = files();
        for (path, text) in &files.files {
            let lower = text.to_lowercase();
            for secret in [
                "faker fan",
                "fakerfan",
                "grumpy otter",
                "mystery man",
                "xqz7",
                "qwv3",
                "zzk9",
                "jja1",
                "jja2",
                "otr5",
                "c0ffee00",
                "deadbeef",
                "otter-puuid",
                "7123456789",
                "4242424242",
                "lcu-pa55word_zq9",
                "eyjrawq",
                "1055",
                "1711",
                "mucjwtdto",
            ] {
                assert!(
                    !lower.contains(secret),
                    "{secret} leaked into {}",
                    path.display()
                );
            }
            for key in [
                "\"puuid\"",
                "\"summonerid\"",
                "\"accountid\"",
                "password\"",
                "\"jwt\"",
                "\"profileiconid\"",
            ] {
                assert!(!lower.contains(key), "{key} kept in {}", path.display());
            }
        }
    }

    #[test]
    fn pseudonyms_are_consistent_and_keep_game_vocabulary() {
        let files = files();
        let fixture: Value = serde_json::from_str(files.get("classic-test.json").unwrap()).unwrap();
        assert_eq!(fixture["activeplayer"]["riotId"], "Reeler#RR1");
        assert_eq!(fixture["activeplayer"]["riotIdTagLine"], "RR1");
        assert_eq!(
            fixture["activeplayer"]["abilities"]["Q"]["displayName"],
            "Twin Bite"
        );
        let ids: Vec<&str> = fixture["playerlist"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["riotId"].as_str().unwrap())
            .collect();
        // The twins share a game name, so they share a pseudonym and differ by tag.
        assert_eq!(
            ids,
            [
                "Reeler#RR1",
                "Player1#RR1",
                "Player2#RR1",
                "Player3#RR1",
                "Player3#RR2",
                "Player4#RR1"
            ]
        );
        let players = fixture["playerlist"].as_array().unwrap();
        assert_eq!(
            players[1]["championName"], "Yasuo",
            "a champion named like its player"
        );
        assert_eq!(players[1]["riotIdGameName"], "Player1");
        assert_eq!(
            players[0]["summonerSpells"]["summonerSpellOne"]["displayName"],
            "Smite"
        );

        let events = fixture["events"].as_array().unwrap();
        assert_eq!(events[1]["KillerName"], "Reeler");
        assert_eq!(events[1]["VictimName"], "Player3");
        assert_eq!(events[1]["Assisters"], json!(["Player1"]));
        assert_eq!(
            events[2]["DragonType"], "Fire",
            "dragon type, not the player called Fire"
        );
        assert_eq!(events[2]["KillerName"], "Player2");
        assert_eq!(events[5]["KillerName"], "Turret_T2_C_05_A");
        assert_eq!(
            events[6]["KillerName"], "Unknown1",
            "not a player in this match"
        );
        assert_eq!(events[9]["Acer"], "Reeler");

        let session: Value = serde_json::from_str(
            files
                .get(Path::new("lcu").join("classic-test.session.json"))
                .unwrap(),
        )
        .unwrap();
        let data = &session["gameData"];
        assert_eq!(data["gameId"], 1);
        assert_eq!(data["teamOne"][0]["gameName"], "Reeler");
        assert_eq!(data["teamOne"][0]["tagLine"], "RR1");
        assert_eq!(data["teamOne"][1]["gameName"], "Player1");
        assert_eq!(
            data["playerChampionSelections"][0]["summonerInternalName"],
            "reeler"
        );
        assert_eq!(data["queue"]["description"], "Ranked Solo/Duo");

        let eog: Value = serde_json::from_str(
            files
                .get(Path::new("lcu").join("classic-test.eog.json"))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(eog["localPlayer"]["riotIdGameName"], "Reeler");
        assert_eq!(eog["localPlayer"]["summonerName"], "Reeler");
        assert_eq!(eog["localPlayer"]["championName"], "Shyvana");
        assert_eq!(eog["teams"][0]["players"][0]["riotIdGameName"], "Player1");
        assert_eq!(eog["teams"][1]["players"][0]["riotIdTagLine"], "RR1");
        assert_eq!(eog["teams"][1]["players"][0]["riotIdGameName"], "Player4");
        assert_eq!(eog["reportGameId"], 1);
        assert_eq!(
            eog["multiUserChatId"], "1-eog",
            "the game ID inside a string"
        );
        assert!(eog.get("mucJwtDto").is_none() && eog.get("multiUserChatPassword").is_none());
        assert!(eog.get("currentLevel").is_none() && eog.get("experienceTotal").is_none());
        assert!(eog["localPlayer"].get("level").is_none(), "summoner level");
        assert!(eog["localPlayer"].get("profileIconId").is_none());
        assert_eq!(
            eog["localPlayer"]["stats"]["LEVEL"], 18,
            "champion level stays"
        );
        assert_eq!(eog["localPlayer"]["note"], "chat token <redacted>");
    }

    #[test]
    fn the_pseudonymised_match_replays_exactly_like_the_real_one() {
        let recording = real_recording();
        let raw = fixture_json(
            "raw",
            recording.activeplayer.as_ref().unwrap(),
            None,
            recording.playerlist.as_ref().unwrap(),
            recording.gamestats.as_ref().unwrap(),
            &recording.events,
        );
        let config = SessionConfig {
            markers: MarkerConfig::default().with_extras(),
            ..SessionConfig::default()
        };
        let original = replay(&Fixture::parse(&raw).unwrap(), &config)
            .unwrap()
            .summary;

        let files = files();
        let dir = std::env::temp_dir().join(format!("riftreel-recorder-{}", std::process::id()));
        files.write(&dir).unwrap();
        let path = dir.join("classic-test.json");
        let lcu = LcuRecording::load_beside(&path).unwrap();
        let pseudonymised = replay_with(&Fixture::load(&path).unwrap(), &lcu, &config)
            .unwrap()
            .summary;
        std::fs::remove_dir_all(&dir).ok();

        assert!(!original.markers.is_empty());
        assert_eq!(pseudonymised.markers, original.markers);
        assert_eq!(pseudonymised.moments, original.moments);
        assert_eq!(pseudonymised.champion, original.champion);
        assert_eq!(pseudonymised.team, original.team);
        assert_eq!(pseudonymised.result, original.result);
        assert_eq!(
            pseudonymised.result_source,
            Some(ResultSource::EndOfGameStats)
        );
        assert_eq!(pseudonymised.game_id, Some(PSEUDO_GAME_ID));
        assert_eq!(pseudonymised.queue_id, Some(420));
    }

    #[test]
    fn a_leak_refuses_to_write() {
        let recording = real_recording();
        let payloads = [
            recording.playerlist.as_ref().unwrap(),
            recording.lcu_session.as_ref().unwrap(),
        ];
        let pseudo = Pseudonymizer::new(recording.activeplayer.as_ref(), payloads, &[PASSWORD]);
        assert!(pseudo
            .check("x.json", "{\"note\":\"gg faker fan#xqz7\"}")
            .is_err());
        assert!(pseudo.check("x.json", PASSWORD).is_err());
        assert!(pseudo.check("x.json", "{\"summonerId\":1}").is_err());
        assert!(pseudo
            .check("x.json", &format!("{{\"t\":\"{CHAT_JWT}\"}}"))
            .is_err());
        assert!(pseudo
            .check("x.json", "{\"chat\":\"7123456789-eog\"}")
            .is_err());
        assert!(pseudo
            .check("x.json", "{\"riotId\":\"Reeler#RR1\"}")
            .is_ok());
    }

    #[test]
    fn identifiers_that_slip_through_are_redacted_not_lost() {
        let mut recording = real_recording();
        // A member named after a player: the walk never renames members.
        recording.activeplayer.as_mut().unwrap()["perPlayer"] = json!({"Faker Fan#XQZ7": 3});
        let files = recording.fixture_files("odd", at(), &[PASSWORD]).unwrap();
        assert_eq!(
            files.redacted,
            ["activeplayer.perPlayer.<redacted 1> (a member name: a real Riot ID)"]
        );
        let fixture: Value = serde_json::from_str(files.get("odd.json").unwrap()).unwrap();
        assert_eq!(fixture["activeplayer"]["perPlayer"]["<redacted 1>"], 3);
        assert!(!files
            .get("odd.json")
            .unwrap()
            .to_lowercase()
            .contains("faker fan"));
    }

    #[test]
    fn names_that_change_length_when_lower_cased_are_still_swept() {
        let mut recording = real_recording();
        let players = recording.playerlist.as_mut().unwrap();
        players
            .as_array_mut()
            .unwrap()
            .push(player("İbrahim", "TR1", "CHAOS", "Zed"));
        // `İ` lower-cases to two characters, so only the name as written matches it.
        recording.events.push(event(
            11,
            "SomethingNew",
            900.0,
            json!({"Text": "gg İbrahim#TR1"}),
        ));
        let files = recording.fixture_files("tr", at(), &[]).unwrap();
        assert!(files.redacted.is_empty(), "{:?}", files.redacted);
        let fixture: Value = serde_json::from_str(files.get("tr.json").unwrap()).unwrap();
        let last = fixture["events"].as_array().unwrap().last().unwrap();
        assert_eq!(last["Text"], "gg Player5#RR1");
        assert_eq!(fixture["playerlist"][6]["riotId"], "Player5#RR1");
    }

    #[test]
    fn spectator_recordings_keep_the_error_and_no_local_player() {
        let mut recording = real_recording();
        recording.activeplayer = None;
        recording.activeplayer_error = Some(
            json!({"errorCode": "RESOURCE_NOT_FOUND", "httpStatus": 404, "message": "No active player"}),
        );
        recording.lcu_session = None;
        recording.eog = None;
        recording.frames.clear();
        let files = recording.fixture_files("spec", at(), &[]).unwrap();
        assert_eq!(files.files.len(), 1, "no League client files");
        let fixture = Fixture::parse(files.get("spec.json").unwrap()).unwrap();
        assert!(fixture.active_player().unwrap().is_none());
        assert_eq!(fixture.activeplayer_error.unwrap()["httpStatus"], 404);
        assert!(fixture.description.ends_with("no local player (spectator)"));
        let text = files.get("spec.json").unwrap().to_lowercase();
        assert!(!text.contains("faker fan") && !text.contains("grumpy otter"));
    }

    #[test]
    fn utc_formatting() {
        assert_eq!(utc_stamp(at()), "20261015-1612");
        assert_eq!(utc_minute(at()), "2026-10-15 16:12 UTC");
        let leap_day = UNIX_EPOCH + Duration::from_secs(1_709_164_800 + 59);
        assert_eq!(utc_minute(leap_day), "2024-02-29 00:00 UTC");
    }
}
