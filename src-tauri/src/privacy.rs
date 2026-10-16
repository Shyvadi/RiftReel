//! Keeping Riot identifiers (Riot IDs, summoner names, puuids, account and summoner IDs)
//! out of logs.
//!
//! Two layers:
//! 1. Types that hold identifiers print [`Redacted`] pseudonyms in `Debug`, so normal
//!    logging never formats a real name.
//! 2. [`scrub`] runs over every formatted log line before it's written (see
//!    `logging`). It masks identifier-shaped values and any player names registered
//!    with [`register_names`] during a match.

use std::borrow::Cow;
use std::collections::hash_map::RandomState;
use std::collections::VecDeque;
use std::fmt;
use std::hash::BuildHasher;
use std::sync::{LazyLock, RwLock};

use regex::{Captures, Regex};

static SALT: LazyLock<RandomState> = LazyLock::new(RandomState::new);

/// A short pseudonym for a name. It's stable within one run, so log lines can be
/// correlated, and changes on every run because the hash key is random.
pub fn pseudonym(name: &str) -> String {
    let hash = SALT.hash_one(name.trim().to_lowercase());
    format!("p{:06x}", hash & 0xff_ffff)
}

/// Prints a pseudonym instead of the wrapped value.
#[derive(Clone, Copy)]
pub struct Redacted<'a>(pub &'a str);

impl fmt::Debug for Redacted<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<{}>", pseudonym(self.0))
    }
}

impl fmt::Display for Redacted<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

/// `Debug` for an optional identifier.
pub fn redact_opt(value: &Option<String>) -> Option<Redacted<'_>> {
    value.as_deref().map(Redacted)
}

/// Identifier-shaped values and their replacements, applied in order.
static PATTERNS: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    let rules: [(&str, &str); 6] = [
        // JSON members that carry identities: `"puuid":"…"`, `"KillerName":"…"`, …
        (
            r#"(?i)"(puuid|summonerId|accountId|currentAccountId|playerId|summonerName|riotId|riotIdGameName|riotIdTagLine|gameName|tagLine|displayName|internalName|killerName|victimName|acer|recipient|assisters|password|token)"\s*:\s*("(?:[^"\\]|\\.)*"|-?\d+(?:\.\d+)?|\[[^\]]*\])"#,
            r#""$1":"<redacted>""#,
        ),
        // `key=value` / `key: value` in tracing fields and Debug output.
        (
            r#"(?i)\b(puuid|summoner_?id|account_?id|riot_?id|game_?name|tag_?line|summoner_?name|password|remoting-auth-token|auth_?token)(\s*[=:]\s*)("[^"]*"|[^\s,;})\]]+)"#,
            "${1}${2}<redacted>",
        ),
        // HTTP basic auth (the LCU password).
        (r"(?i)\bBasic\s+[A-Za-z0-9+/=]{8,}", "Basic <redacted>"),
        // Credentials embedded in a URL.
        (r"riot:[^@\s/]+@", "riot:<redacted>@"),
        // Riot ID `GameName#TAG`.
        (r"[\p{L}\p{N}_.]{3,16}#[\p{L}\p{N}]{3,5}\b", "<riot-id>"),
        // puuids (78 chars), encrypted summoner/account IDs and other long opaque tokens.
        (LONG_TOKEN, "<id>"),
    ];
    rules
        .into_iter()
        .map(|(pattern, replacement)| {
            (
                Regex::new(pattern).expect("valid redaction regex"),
                replacement,
            )
        })
        .collect()
});

const LONG_TOKEN: &str = r"\b[0-9A-Za-z_-]{40,}\b";

/// Match folder names (`2026-10-15_23-30-00_Ranked-Solo-Duo_Shyvana`) are long enough to
/// look like tokens but only hold a date, the queue and the champion.
static MATCH_FOLDER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\d{4}-\d{2}-\d{2}_\d{2}-\d{2}-\d{2}(?:[_-]|$)").expect("valid regex")
});

/// The most names kept for scrubbing; the oldest are forgotten first.
const MAX_KNOWN_NAMES: usize = 256;

#[derive(Default)]
struct KnownNames {
    names: VecDeque<String>,
    matcher: Option<Regex>,
}

static KNOWN_NAMES: LazyLock<RwLock<KnownNames>> = LazyLock::new(Default::default);

/// Registers player names (Riot IDs, game names, summoner names) seen in the current
/// match so [`scrub`] masks them anywhere they appear.
pub fn register_names<'a>(names: impl IntoIterator<Item = &'a str>) {
    let Ok(mut known) = KNOWN_NAMES.write() else {
        return;
    };
    let mut changed = false;
    for name in names {
        let name = name.trim();
        // Very short names would mask ordinary words; the regex patterns still cover
        // their structured forms.
        if name.chars().count() < 3 || known.names.iter().any(|n| n.eq_ignore_ascii_case(name)) {
            continue;
        }
        if known.names.len() == MAX_KNOWN_NAMES {
            known.names.pop_front();
        }
        known.names.push_back(name.to_owned());
        changed = true;
    }
    if changed {
        // Longest first, so "Name#TAG" wins over its "Name" prefix.
        let mut sorted: Vec<&String> = known.names.iter().collect();
        sorted.sort_by_key(|n| std::cmp::Reverse(n.len()));
        let alternation = sorted
            .iter()
            .map(|n| regex::escape(n))
            .collect::<Vec<_>>()
            .join("|");
        known.matcher = Regex::new(&format!("(?i){alternation}")).ok();
    }
}

/// Masks identifiers in a formatted log line.
pub fn scrub(line: &str) -> Cow<'_, str> {
    let mut out = Cow::Borrowed(line);
    if let Ok(known) = KNOWN_NAMES.read() {
        if let Some(matcher) = &known.matcher {
            if matcher.is_match(&out) {
                out = Cow::Owned(matcher.replace_all(&out, "<player>").into_owned());
            }
        }
    }
    for (regex, replacement) in PATTERNS.iter() {
        if !regex.is_match(&out) {
            continue;
        }
        let replaced = if regex.as_str() == LONG_TOKEN {
            regex.replace_all(&out, |token: &Captures| {
                if MATCH_FOLDER.is_match(&token[0]) {
                    token[0].to_owned()
                } else {
                    (*replacement).to_owned()
                }
            })
        } else {
            regex.replace_all(&out, *replacement)
        };
        out = Cow::Owned(replaced.into_owned());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pseudonyms_are_stable_and_case_insensitive() {
        assert_eq!(pseudonym("Reeler"), pseudonym(" reeler "));
        assert_ne!(pseudonym("Reeler"), pseudonym("Someone Else"));
        assert!(!format!("{:?}", Redacted("Reeler")).contains("Reeler"));
    }

    #[test]
    fn scrubs_json_identity_members() {
        let line = r#"body={"puuid":"abc","summonerId":12345,"riotId":"Some One#EUW","gameMode":"CLASSIC"}"#;
        let out = scrub(line);
        assert!(!out.contains("abc"));
        assert!(!out.contains("12345"));
        assert!(!out.contains("Some One"));
        assert!(out.contains(r#""gameMode":"CLASSIC""#), "{out}");
    }

    #[test]
    fn scrubs_fields_tokens_and_riot_ids() {
        let puuid = "a".repeat(78);
        let line = format!(
            "puuid={puuid} summoner_name=\"Foo Bar\" Authorization: Basic cmlvdDpzZWNyZXQ= url=https://riot:hunter2@127.0.0.1:1234/ killer FakerFan#KR1 time=12.5"
        );
        let out = scrub(&line);
        for secret in [
            puuid.as_str(),
            "Foo Bar",
            "cmlvdDpzZWNyZXQ=",
            "hunter2",
            "FakerFan#KR1",
        ] {
            assert!(!out.contains(secret), "{secret} leaked: {out}");
        }
        assert!(out.contains("time=12.5"), "{out}");
    }

    #[test]
    fn scrubs_registered_names_anywhere() {
        register_names(["Registered Player", "Registered Player#TAG1"]);
        let out = scrub("killed by registered player at 12:00");
        assert_eq!(out, "killed by <player> at 12:00");
    }

    #[test]
    fn match_folder_names_are_not_tokens() {
        let id = "2026-10-15_23-30-00_Ranked-Solo-Duo_Shyvana";
        let line = format!("match saved id={id}");
        assert_eq!(scrub(&line), line);
        let path = format!(r"folder=C:\Users\x\library\League of Legends\{id}\match.mp4");
        assert_eq!(scrub(&path), path);
        // A token that only starts like a date is still masked.
        let token = format!("2026-10-15{}", "a".repeat(68));
        assert_eq!(scrub(&format!("puuid {token}")), "puuid <id>");
    }

    #[test]
    fn leaves_ordinary_lines_alone() {
        let line = "tracker state Idle -> Armed (phase ChampSelect)";
        assert!(matches!(scrub(line), Cow::Borrowed(_)));
    }
}
