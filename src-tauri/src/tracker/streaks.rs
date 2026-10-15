//! Kill streaks: League's announcer sprees and shutdowns. Riot's event stream has no
//! events for these, so they're derived from the `ChampionKill` sequence.

use std::collections::HashMap;

use serde::Serialize;

/// Announcer spree tiers by kills without dying.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SpreeLevel {
    KillingSpree,
    Rampage,
    Unstoppable,
    Dominating,
    Godlike,
    Legendary,
}

impl SpreeLevel {
    /// 3 killing spree, 4 rampage, 5 unstoppable, 6 dominating, 7 godlike, 8+ legendary.
    pub fn for_streak(kills: u32) -> Option<Self> {
        Some(match kills {
            0..=2 => return None,
            3 => Self::KillingSpree,
            4 => Self::Rampage,
            5 => Self::Unstoppable,
            6 => Self::Dominating,
            7 => Self::Godlike,
            _ => Self::Legendary,
        })
    }
}

/// Killing a champion on a streak at least this long is a shutdown (the announcer's
/// "Shut down" plays for victims who were on a killing spree).
pub const SHUTDOWN_STREAK: u32 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct KillOutcome {
    /// Killer's streak including this kill (0 for executions).
    pub killer_streak: u32,
    /// Spree tier the killer just reached, if this kill moved them up a tier.
    pub killer_new_spree: Option<SpreeLevel>,
    /// Victim's streak before dying.
    pub victim_streak: u32,
    /// The victim was on a spree and a champion ended it.
    pub shutdown: bool,
}

/// Streaks per player name (case-insensitive).
#[derive(Debug, Default, Clone)]
pub struct StreakTracker {
    streaks: HashMap<String, u32>,
}

impl StreakTracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn streak(&self, name: &str) -> u32 {
        self.streaks.get(&key(name)).copied().unwrap_or(0)
    }

    /// Records a `ChampionKill`. An execution (killer is a turret, minion or monster)
    /// ends the victim's streak without crediting anyone.
    pub fn on_champion_kill(
        &mut self,
        killer: &str,
        killer_is_champion: bool,
        victim: &str,
    ) -> KillOutcome {
        let victim_key = key(victim);
        let victim_streak = self.streaks.insert(victim_key.clone(), 0).unwrap_or(0);
        let mut outcome = KillOutcome {
            victim_streak,
            shutdown: killer_is_champion && victim_streak >= SHUTDOWN_STREAK,
            ..KillOutcome::default()
        };
        let killer_key = key(killer);
        if killer_is_champion && !killer_key.is_empty() && killer_key != victim_key {
            let streak = self.streaks.entry(killer_key).or_insert(0);
            let tier_before = SpreeLevel::for_streak(*streak);
            *streak += 1;
            let tier_after = SpreeLevel::for_streak(*streak);
            outcome.killer_streak = *streak;
            if tier_after != tier_before {
                outcome.killer_new_spree = tier_after;
            }
        }
        outcome
    }
}

fn key(name: &str) -> String {
    name.trim().to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spree_tiers_follow_the_announcer() {
        assert_eq!(SpreeLevel::for_streak(2), None);
        assert_eq!(SpreeLevel::for_streak(3), Some(SpreeLevel::KillingSpree));
        assert_eq!(SpreeLevel::for_streak(7), Some(SpreeLevel::Godlike));
        assert_eq!(SpreeLevel::for_streak(12), Some(SpreeLevel::Legendary));
    }

    #[test]
    fn each_new_tier_is_reported_once() {
        let mut streaks = StreakTracker::new();
        let tiers: Vec<_> = (0..10)
            .map(|i| {
                streaks
                    .on_champion_kill("Me", true, &format!("Victim{i}"))
                    .killer_new_spree
            })
            .collect();
        assert_eq!(
            tiers,
            [
                None,
                None,
                Some(SpreeLevel::KillingSpree),
                Some(SpreeLevel::Rampage),
                Some(SpreeLevel::Unstoppable),
                Some(SpreeLevel::Dominating),
                Some(SpreeLevel::Godlike),
                Some(SpreeLevel::Legendary),
                None,
                None,
            ]
        );
        assert_eq!(streaks.streak("me"), 10);
    }

    #[test]
    fn dying_resets_and_ending_a_spree_is_a_shutdown() {
        let mut streaks = StreakTracker::new();
        for victim in ["A", "B", "C"] {
            streaks.on_champion_kill("Foe", true, victim);
        }
        let outcome = streaks.on_champion_kill("Me", true, "Foe");
        assert_eq!(outcome.victim_streak, 3);
        assert!(outcome.shutdown);
        assert_eq!(streaks.streak("Foe"), 0);
        assert_eq!(outcome.killer_streak, 1);

        // Two kills is not a spree yet.
        streaks.on_champion_kill("Foe", true, "A");
        streaks.on_champion_kill("Foe", true, "B");
        assert!(!streaks.on_champion_kill("Me", true, "Foe").shutdown);
    }

    #[test]
    fn executions_reset_the_victim_without_crediting_anyone() {
        let mut streaks = StreakTracker::new();
        for victim in ["A", "B", "C"] {
            streaks.on_champion_kill("Me", true, victim);
        }
        let outcome = streaks.on_champion_kill("Turret_T2_C_05_A", false, "Me");
        assert_eq!(outcome.victim_streak, 3);
        assert!(!outcome.shutdown, "turrets don't shut anyone down");
        assert_eq!(streaks.streak("Me"), 0);
        assert_eq!(streaks.streak("Turret_T2_C_05_A"), 0);
    }
}
