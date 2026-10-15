//! Tracker state machine (doc 02 §B3).
//!
//! ```text
//! Idle ──(LCU ChampSelect)──► Armed ──(LCU InProgress / Live API reachable)──► InGame
//! InGame ──(LCU Reconnect)──► InGame   same match: session and event cursor are kept
//! InGame ──(GameEnd | LCU WaitingForStats/PreEndOfGame/EndOfGame | game gone)──► Finalizing
//! Finalizing ──(summary written)──► Idle (or Armed if the next champ select already began)
//! ```
//!
//! Degraded mode (no League client): the `League of Legends.exe` process arms the
//! tracker, a reachable Live Client API starts the match, and `GameEnd` or the process
//! exiting ends it.
//!
//! The machine is pure: it turns [`Signal`]s into [`Effect`]s, which
//! `tracker::runtime` carries out.

use serde::Serialize;

use crate::league::models::GameflowPhase;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub enum TrackerState {
    #[default]
    Idle,
    Armed,
    InGame,
    Finalizing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Signal {
    LcuConnected,
    LcuDisconnected,
    /// Gameflow phase from the League client (initial value or a change).
    Phase(GameflowPhase),
    /// `League of Legends.exe` appeared (degraded mode).
    GameProcessStarted,
    /// `League of Legends.exe` is gone.
    GameProcessExited,
    /// The Live Client Data API answered.
    LiveApiUp,
    /// Riot's `GameEnd` event arrived.
    GameEnded,
    /// The match summary is done.
    Finalized,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EndReason {
    /// Riot `GameEnd` event.
    GameEnd,
    /// The client moved to WaitingForStats / PreEndOfGame / EndOfGame.
    ClientPhase,
    /// The game process exited (degraded mode).
    ProcessExited,
    /// The client left the game some other way (lobby, error).
    Aborted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    /// Champ select: warm up the encoder. Capture lands in phase 2; for now this is a
    /// no-op.
    PrewarmCapture,
    /// A new match: create a match session and start polling the Live Client API.
    StartMatch,
    /// Back in the same match after a reconnect: keep the session and cursor and re-read
    /// `eventdata` right away.
    ResumeMatch,
    /// Stop polling (and capture), gather end-of-game stats, emit the match summary.
    Finalize(EndReason),
}

#[derive(Debug, Clone, Default)]
pub struct Machine {
    state: TrackerState,
    lcu_connected: bool,
    reconnecting: bool,
    last_phase: Option<GameflowPhase>,
}

impl Machine {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn state(&self) -> TrackerState {
        self.state
    }

    pub fn lcu_connected(&self) -> bool {
        self.lcu_connected
    }

    /// The client reported `Reconnect` and the game hasn't come back yet.
    pub fn is_reconnecting(&self) -> bool {
        self.reconnecting
    }

    pub fn last_phase(&self) -> Option<&GameflowPhase> {
        self.last_phase.as_ref()
    }

    pub fn handle(&mut self, signal: Signal) -> Vec<Effect> {
        use TrackerState::*;
        match signal {
            Signal::LcuConnected => {
                self.lcu_connected = true;
                vec![]
            }
            Signal::LcuDisconnected => {
                self.lcu_connected = false;
                self.last_phase = None;
                // Only the client reports reconnects; without it there's nothing to wait for.
                self.reconnecting = false;
                vec![]
            }
            Signal::Phase(phase) => {
                self.last_phase = Some(phase.clone());
                self.on_phase(&phase)
            }
            Signal::GameProcessStarted => match self.state {
                Idle if !self.lcu_connected => self.go(Armed, vec![]),
                _ => vec![],
            },
            // With the client connected its phases decide; a crash shows up as Reconnect.
            Signal::GameProcessExited => match self.state {
                Armed if !self.lcu_connected => self.go(Idle, vec![]),
                InGame if !self.lcu_connected => {
                    self.go(Finalizing, vec![Effect::Finalize(EndReason::ProcessExited)])
                }
                _ => vec![],
            },
            Signal::LiveApiUp => match self.state {
                Armed => self.start_match(),
                Idle if !self.lcu_connected => self.start_match(),
                _ => vec![],
            },
            Signal::GameEnded => match self.state {
                InGame => self.go(Finalizing, vec![Effect::Finalize(EndReason::GameEnd)]),
                _ => vec![],
            },
            Signal::Finalized => match self.state {
                // The next champ select may already be under way.
                Finalizing
                    if self.lcu_connected
                        && matches!(
                            self.last_phase,
                            Some(GameflowPhase::ChampSelect | GameflowPhase::GameStart)
                        ) =>
                {
                    self.go(Armed, vec![Effect::PrewarmCapture])
                }
                Finalizing => self.go(Idle, vec![]),
                _ => vec![],
            },
        }
    }

    fn on_phase(&mut self, phase: &GameflowPhase) -> Vec<Effect> {
        use GameflowPhase as P;
        use TrackerState::*;
        match (self.state, phase) {
            (Idle, P::ChampSelect | P::GameStart) => self.go(Armed, vec![Effect::PrewarmCapture]),
            (Idle | Armed, P::InProgress) => self.start_match(),
            (Idle | Armed, P::Reconnect) => {
                let effects = self.start_match();
                self.reconnecting = true;
                effects
            }
            (Armed, P::ChampSelect | P::GameStart) => vec![],
            // Dodge, or back to the lobby.
            (Armed, _) => self.go(Idle, vec![]),
            (InGame, P::Reconnect) => {
                self.reconnecting = true;
                vec![]
            }
            (InGame, P::InProgress | P::GameStart) if self.reconnecting => {
                self.reconnecting = false;
                vec![Effect::ResumeMatch]
            }
            (InGame, P::InProgress | P::GameStart) => vec![],
            (InGame, p) if p.is_post_game() => {
                self.go(Finalizing, vec![Effect::Finalize(EndReason::ClientPhase)])
            }
            (InGame, _) => self.go(Finalizing, vec![Effect::Finalize(EndReason::Aborted)]),
            (Idle | Finalizing, _) => vec![],
        }
    }

    fn start_match(&mut self) -> Vec<Effect> {
        self.reconnecting = false;
        self.go(TrackerState::InGame, vec![Effect::StartMatch])
    }

    fn go(&mut self, state: TrackerState, effects: Vec<Effect>) -> Vec<Effect> {
        self.state = state;
        if state != TrackerState::InGame {
            // A match that ends while the client still says Reconnect is over, not
            // reconnecting.
            self.reconnecting = false;
        }
        effects
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use GameflowPhase as P;
    use TrackerState::*;

    fn phase(m: &mut Machine, p: GameflowPhase) -> Vec<Effect> {
        m.handle(Signal::Phase(p))
    }

    fn with_client() -> Machine {
        let mut m = Machine::new();
        m.handle(Signal::LcuConnected);
        m
    }

    #[test]
    fn full_match_with_the_client() {
        let mut m = with_client();
        assert!(phase(&mut m, P::Lobby).is_empty());
        assert_eq!(m.state(), Idle);
        assert_eq!(phase(&mut m, P::ChampSelect), [Effect::PrewarmCapture]);
        assert_eq!(m.state(), Armed);
        assert!(phase(&mut m, P::GameStart).is_empty());
        assert_eq!(phase(&mut m, P::InProgress), [Effect::StartMatch]);
        assert_eq!(m.state(), InGame);
        assert_eq!(
            m.handle(Signal::GameEnded),
            [Effect::Finalize(EndReason::GameEnd)]
        );
        assert_eq!(m.state(), Finalizing);
        // The client's own end phases don't finalize twice.
        assert!(phase(&mut m, P::WaitingForStats).is_empty());
        assert!(phase(&mut m, P::EndOfGame).is_empty());
        assert!(m.handle(Signal::Finalized).is_empty());
        assert_eq!(m.state(), Idle);
    }

    #[test]
    fn live_api_can_start_the_match_while_armed() {
        let mut m = with_client();
        phase(&mut m, P::ChampSelect);
        assert_eq!(m.handle(Signal::LiveApiUp), [Effect::StartMatch]);
        // The client catching up to InProgress doesn't start a second match.
        assert!(phase(&mut m, P::InProgress).is_empty());
        assert_eq!(m.state(), InGame);
    }

    #[test]
    fn reconnect_keeps_the_same_match() {
        let mut m = with_client();
        phase(&mut m, P::ChampSelect);
        phase(&mut m, P::InProgress);
        // Game crashed; the game process vanishing is not the end while the client says Reconnect.
        assert!(phase(&mut m, P::Reconnect).is_empty());
        assert!(m.handle(Signal::GameProcessExited).is_empty());
        assert_eq!(m.state(), InGame);
        assert!(m.is_reconnecting());
        assert_eq!(phase(&mut m, P::InProgress), [Effect::ResumeMatch]);
        assert!(!m.is_reconnecting());
        assert_eq!(
            phase(&mut m, P::WaitingForStats),
            [Effect::Finalize(EndReason::ClientPhase)]
        );
    }

    #[test]
    fn dodges_return_to_idle() {
        let mut m = with_client();
        phase(&mut m, P::ChampSelect);
        assert!(phase(&mut m, P::Matchmaking).is_empty());
        assert_eq!(m.state(), Idle);
    }

    #[test]
    fn late_start_mid_game_and_after_a_crash() {
        let mut m = with_client();
        assert_eq!(phase(&mut m, P::InProgress), [Effect::StartMatch]);

        let mut crashed = with_client();
        assert_eq!(phase(&mut crashed, P::Reconnect), [Effect::StartMatch]);
        assert!(crashed.is_reconnecting());
        assert_eq!(phase(&mut crashed, P::InProgress), [Effect::ResumeMatch]);
    }

    #[test]
    fn leaving_the_game_unexpectedly_aborts() {
        let mut m = with_client();
        phase(&mut m, P::InProgress);
        assert_eq!(
            phase(&mut m, P::TerminatedInError),
            [Effect::Finalize(EndReason::Aborted)]
        );
    }

    #[test]
    fn finalizing_rearms_when_the_next_champ_select_started() {
        let mut m = with_client();
        phase(&mut m, P::InProgress);
        m.handle(Signal::GameEnded);
        phase(&mut m, P::EndOfGame);
        phase(&mut m, P::Lobby);
        phase(&mut m, P::ChampSelect);
        assert_eq!(m.handle(Signal::Finalized), [Effect::PrewarmCapture]);
        assert_eq!(m.state(), Armed);
    }

    #[test]
    fn degraded_mode_uses_process_and_live_api() {
        let mut m = Machine::new();
        assert!(m.handle(Signal::GameProcessStarted).is_empty());
        assert_eq!(m.state(), Armed);
        assert_eq!(m.handle(Signal::LiveApiUp), [Effect::StartMatch]);
        assert_eq!(
            m.handle(Signal::GameProcessExited),
            [Effect::Finalize(EndReason::ProcessExited)]
        );
        m.handle(Signal::Finalized);
        assert_eq!(m.state(), Idle);

        // App started while a game was already running.
        assert_eq!(m.handle(Signal::LiveApiUp), [Effect::StartMatch]);
        assert_eq!(
            m.handle(Signal::GameEnded),
            [Effect::Finalize(EndReason::GameEnd)]
        );
    }

    #[test]
    fn with_the_client_the_process_and_live_api_alone_do_nothing_when_idle() {
        let mut m = with_client();
        assert!(m.handle(Signal::GameProcessStarted).is_empty());
        assert!(m.handle(Signal::LiveApiUp).is_empty());
        assert_eq!(m.state(), Idle);
    }

    #[test]
    fn losing_the_client_mid_game_falls_back_to_degraded_rules() {
        let mut m = with_client();
        phase(&mut m, P::InProgress);
        m.handle(Signal::LcuDisconnected);
        assert_eq!(
            m.handle(Signal::GameProcessExited),
            [Effect::Finalize(EndReason::ProcessExited)]
        );
    }

    #[test]
    fn the_reconnect_flag_never_outlives_the_match_or_the_client() {
        // The game ends before the player makes it back.
        let mut m = with_client();
        phase(&mut m, P::InProgress);
        phase(&mut m, P::Reconnect);
        assert!(m.is_reconnecting());
        assert_eq!(
            phase(&mut m, P::WaitingForStats),
            [Effect::Finalize(EndReason::ClientPhase)]
        );
        assert!(!m.is_reconnecting());
        m.handle(Signal::Finalized);
        assert_eq!((m.state(), m.is_reconnecting()), (Idle, false));

        // Riot's GameEnd arrives while the client still shows Reconnect.
        let mut m = with_client();
        phase(&mut m, P::InProgress);
        phase(&mut m, P::Reconnect);
        m.handle(Signal::GameEnded);
        assert!(!m.is_reconnecting());

        // The client itself goes away mid-reconnect.
        let mut m = with_client();
        phase(&mut m, P::InProgress);
        phase(&mut m, P::Reconnect);
        m.handle(Signal::LcuDisconnected);
        assert_eq!((m.state(), m.is_reconnecting()), (InGame, false));
    }
}
