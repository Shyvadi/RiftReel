# League of Legends tracker: what Outplayed does, and what Downplayed does instead

This is the critical spec for Downplayed.

- **Part A** documents Outplayed's League tracker as built (176.0.13334) and as it behaved
  on this machine. Labels: **[static]** (`bg:` = `background.js`, `idx:` = `index.js`),
  **[log]** (the user's Outplayed logs, 2026-09-07 → 09-29, 118 LoL sessions / 117
  saved matches), **[backup]**, **[docs]** (Overwolf/Riot developer docs),
  **[inferred]**, **[unknown]**.
- **Part B** maps every signal to Riot's own local interfaces, which Downplayed uses
  directly.

> Privacy note: Outplayed's logs on this machine contain the player's Riot identity in
> plain text (display name, Riot ID, puuid, summoner and account IDs). Those values are
> deliberately **not** reproduced here.

---

## Part A: Outplayed's tracker

### A0. Summary

- One class handles League: `KS` (`bg:45790-46151`), selected by the service factory
  `CE` for game ids **5426** and **22848** (PBE). It is a new instance per game launch
  (`bg:49048-49062`). It also contains embedded TFT and Arena logic, switched on GEP
  `match_info.game_mode`.
- **Highlights come from three sources** (all through Overwolf GEP; Outplayed never calls
  Riot directly):
  1. **Kill counters.** GEP `kill`-feature info `game_info.kills / doubleKills /
     tripleKills / quadraKills / pentaKills` is *diffed* by `LolGameEventsService`
     (`bg:53615-53727`).
  2. **GEP events.** `death`, `assist`, `usedAbility` (type 4 = ultimate) and `victory`.
  3. **Riot Live Client events relayed by GEP's `live_client_data`.** Only `BaronKill`,
     `DragonKill`, `HeraldKill`, `TurretKilled`, `InhibKilled`, and only when
     `KillerName` equals the local summoner name (`bg:45936-45954`, map at
     `bg:45778-45784`).
- **Event time is the time of receipt** (`Date.now()`, `bg:39970`, `53285`), not Riot's
  `EventTime`.
- **Wins are never recorded (bug).** `_onLolGameEvent` sets `isVictory` only for an event
  named `announcer` whose `data.name === "victory"` (`bg:~46029-46031`). GEP delivers
  `victory` as its own event, so that branch never runs. The Victory *clip* is still
  captured through the generic path.
  - [log] 54 `victory` events arrived, yet **0 of 117** League matches were saved with
    `isVictory: true`.
  - The library's win badge therefore never appears for League.

### A1. Data sources

| GEP provider | Overwolf id | Use by Outplayed |
|---|---|---|
| League of Legends (in-game) | **5426** (PBE **22848**) | Everything in A2–A4 |
| League launcher | **10902** | Only `overwolf.games.launchers.events.getInfo(10902)` → `lobby_info.queueId == "1160"` to detect TFT Double Up (`bg:45521-45530`, `52287-52298`). [docs] The launcher provider itself relays LCU data (game-flow phase, summoner info, champ select, end-of-game stats, and the LCU `port`/`token`). |

**Requested features** [static `bg:45778`, log `bg:45067/45070`]:

`kill, death, assist, summoner_info, game_mode, announcer, matchState, abilities,
live_client_data, match_info, me`

- Not requested: gold, minions, level, teams, jungle_camps, chat, counters.
- Registration retries: helper `ml` (`bg:22724-22779`), fixed 5 s between attempts.
  Attempts are 7 when the capture mode needs events, otherwise 1
  (`bg:38832-38840`, `42076-42079`). It aborts on "Not in a game".
- [log] "Provider did not set features yet." 187 times; registration usually succeeds
  on attempt 2–3.
- If registration fails, no listeners are attached and the status is treated as RED
  (`bg:42143-42171`).
- Consumption:
  - `onNewEvents` → `_fireOnGameEvent` → `KS._onGameEvent` (`bg:38850`, `43128-43152`)
  - `onInfoUpdates2` → `KS._onGameInfo` (`bg:38856`)
  - one `getInfo` snapshot at session start (`bg:42621`, `43078-43105`, `45834-45859`)
- **Fields actually read:**
  - `match_info.game_mode`, lowercased (`bg:45897-45900`)
  - `summoner_info.name` (`bg:45903`)
  - `live_client_data.events` (`bg:45908`)
  - the `kill`-feature `game_info` counters (`bg:45914-45923`)
  - from the snapshot only: `game_info.matchStarted`, `summoner_info.champion`, and
    URI-encoded `game_info.teams` (entry marked `**local**`)

### A2. What the tracker actually received [log]

Census of `Game event` lines (logged at `bg:43156`). Every event is logged:

| Group | Events (count) | Used for highlights? |
|---|---|---|
| Abilities | `ability` 94,029 · `usedAbility` 41,736 · `abilityReady` 34,008 · `ultimate` 273 | `usedAbility` type 4 → `ultimate` |
| Chat | `chat` 23,558 (name only logged) | no |
| Player combat | `kill` 823 · `death` 821 · `assist` 397 · `slain` 2,263 · `self_slain` 444 | death, assist. `kill` *events* are dropped as duplicates; kills come from counter diffs |
| Objectives | `turret_destroy` 1,034 · `turret_kill` 238 · `inhibitor_destroy` 155 · `inhibitor_kill` 34 · `inhibitor_respawn` 3 · `dragon_kill` 40 · `baron_kill` 12 · `rift_herald` 10 | `turret_kill`, `inhibitor_kill`, `dragon_kill`, `baron_kill`, `rift_herald` are Outplayed's own names for the Live Client events above. The `*_destroy` announcer events are ignored. |
| Match flow | `welcome_rift` 101 · `minions_spawn` 111 · `first_blood` 87 · `ace` 120 · `victory` 54 · `defeat` 40 · `matchStart` 1 | `victory` only |
| Announcer VO | `play_vo_announcer_female1_*` (multikills, sprees, shutdowns, disconnects…) | no |

89% of the 228,507 LoL log lines are these echoes. **First blood, ace, shutdowns, sprees,
defeat, Elder dragon/Voidgrubs, steals and executes are never turned into highlights**
(GEP sends several of them; Outplayed discards them).

### A3. Highlight rules

Defaults are hard-coded in table `sd` (`bg:27787-27863`, PBE copy `bg:27864-27939`,
UI copy `idx:64066-64142`):

| Highlight (UI name) | Source | Pre / post (ms) | Default on |
|---|---|---|---|
| `kill` (Kill) | Diff of `game_info.kills`; label from `doubleKills…pentaKills` diffs | 12000 / 8000 | yes |
| `kill` + `double/triple/quadra/penta_kill` | Counter diffs, merged at match end (below) | re-centred, past = future (e.g. 13570/13570 in logs) | same toggle |
| `death` (Death) | GEP `death` `{count}` | 12000 / 3000 | yes |
| `assist` (Assist) | GEP `assist` `{count}` | 12000 / 8000 | yes |
| `ultimate` (Ultimate) | GEP `usedAbility` with `type` 4 (`bg:46022-46027`) | 5000 / 5000 | yes |
| `victory` (Victory) | raw GEP `victory` event | 10000 / 1000 | yes |
| `baron_kill`, `dragon_kill`, `rift_herald`, `turret_kill`, `inhibitor_kill` | Live Client `BaronKill`/`DragonKill`/`HeraldKill`/`TurretKilled`/`InhibKilled`, local player as `KillerName` | 10000 / 5000 | yes |

**Kill handling:**
- Each positive counter change emits **one** `kill` event with `{label, count,
  totalKills, suppressNotification: count>1, suppressAppend}` (`bg:53623-53642`; labels
  `bg:45421-45442`).
- A plain kill within 10 s of the previous kill gets `suppressAppend`, so it extends the
  open clip instead of adding a bookmark (`bg:53643-53659`).
- At match end, each multikill absorbs the lower tiers into one event centred on the
  combined window, with `achievementName` "Double Kill"…"Pentakill"
  (`bg:53660-53726`, `45453-45458`).

**Objectives:**
- Processed only when `EventID > last` **and** `KillerName === summoner name`. The
  "last ID" only advances on a match, so other players' events are re-scanned on every
  update. `Stolen` and `DragonType` are ignored.
- Riot's `KillerName` is the Riot ID game name without the tag, which matches GEP
  `summoner_info.name`. The Live Client `summonerName` field is now `Name#TAG`.

**Settings, timings and toasts:**
- User controls: `disabledAutoCaptureEvents` (default `[]`) and per-event
  `eventsTiming` overrides in `settings.game.5426`. The UI sliders run 1–180 s
  (`idx:75148-75317`).
- Unknown events fall back to 15000/5000.
- In-game toast "{event} saved", 3 s (`bg:32222-32234`); multikill counter events are
  not toasted.
- [log] Every observed capture request matches these timings: death 12000/3000,
  kill/assist 12000/8000, objectives 10000/5000, victory 10000/1000, ultimate 5000/5000.

### A4. Match lifecycle

1. **Session start** (`bg:42591-42632`):
   - `sessionId` is the Overwolf session id. If the last match was the same game on the
     same day, its `sessionId` is reused (`bg:76679-76705`; [log] 108 times).
   - A dummy match is created and the `getInfo` snapshot is read.
2. **Match start:** the snapshot's `game_info.matchStarted` is a string, and any
   non-empty value counts ([log] "Found that a match is already in progress, starting a
   new match", 118×). A GEP `matchStart` during a real match is ignored
   (`bg:45861-45883`); only 1 such event was seen.
3. **Match end:** the game process exits. `_onGameClosed` → `_onSessionEnd` →
   `_onMatchEnd` (`bg:42579`, `42640-42647`, `42794-42911`; [log] 116×). No `matchEnd`
   event was ever seen.
4. **IDs:** `originalMatchId` is a dashless UUID. On first save,
   `id = "<sessionId>_<index>"` with `index = ++lastMatchIndex` (`bg:43282`).
5. **`info` contents:**
   - `{gameMode, selectedChampion, killCount, assistCount, deathCount}` merged with TFT
     defaults `{rank:null, roundWinCount:0, roundLossCount:0, lobbySize:8,
     isVictory:false}` (`bg:45785`, `45484`, `45887`). That's why League records carry
     TFT fields.
   - The champion comes only from the session-start snapshot (null in 5 of 117
     matches); `_onLolGameInfo` is dead code.
6. **Game modes:** GEP reports `lol` for Summoner's Rift, ARAM, Practice Tool and
   others ([log] proxied `gameMode`: CLASSIC 101, ARAM 10, PRACTICETOOL 6, KIWI 1).
   - **Nothing is filtered:** Practice Tool is recorded; there is no spectator
     handling.
   - The `brawl` mode drops every event, so no highlights.
7. **Crash/reconnect:** the restarted game gets a new service and a new match
   (`index+1`). Pieces are never merged; there is no awareness of the client's
   Reconnect phase.
8. **Post-game:**
   - Toasts are `session-start`, `match-start`, `match-end`.
   - The main window opens behind `LeagueClientUx` (`idx:96370-96375`); League is
     excluded from the "enhanced match end" screen.
   - `ddragon.leagueoflegends.com` supplies champion avatars, and
     `cdn.outplayed.tv/games/5426/lol/background/<Champ>.webp` supplies backgrounds.
   - There is no rank/LP/queue display anywhere.

### A5. Stored records

Stored in IndexedDB `MediaDatabase` (store `matches`); older builds used localStorage
`MATCH_*` keys:

```
{ id: "<sessionId>_<index>", originalMatchId, index, gameId: 5426, sessionId,
  info: { gameMode: "lol", selectedChampion, lobbySize, killCount, assistCount,
          deathCount, isVictory /* always false for LoL */, rank, roundWinCount, roundLossCount },
  captureMode: "fullMatch" | "highlights" | "fullSession" | "manual",
  startTime, endTime,                      // epoch ms
  medias: [ { type: "fullMatch" | "replay" | "fullSession", startTime, endTime,   // s from match start
              path, url: "overwolf://media/videos/Outplayed/…", format, id, audioTrackMap,
              creationTime, isFavorite, favoriteClip,
              events: [ { type, time /* ms into media */, timing: {past, future}, data } ] } ],
  isDummy, isServiceDown, requestedCaptureMode, isLowDiskSpace, isHighCpuUsage }
```

**APM** (generic, not League-specific):
- Sources: `overwolf.games.inputTracking` key down/up and mouse down, plus a mouse
  position polled every 1 s (`bg:62243-62251`, `62697-62717`).
- Calculation: 10 s buckets scaled per minute; `matchAPM` is the mean of 60 s buckets
  with ≥10 actions (`bg:62542-62617`).
- Stored in `matchesActions`/`apmPerGame` (`bg:76895-76971`).
- Per-game `shouldCollectAPM` defaults to **on**.
- Keystroke retention and upload are covered in doc 01 §6.

---

## Part B: Downplayed's tracker on official Riot interfaces

### B1. Sources

| Need | Riot interface | Notes |
|---|---|---|
| Live events, active player, roster, game clock | **Live Client Data API** `https://127.0.0.1:2999/liveclientdata/{eventdata, activeplayer, activeplayername, playerlist, gamestats, allgamedata}` | Official and documented. Only up while a match runs. Self-signed TLS; validate against `riotgames.pem`. Local schema at `/swagger/v3/openapi.json`. Outplayed already consumes exactly this, via GEP. |
| Match start/end, queue, champion, result, post-game stats, rank | **League Client (LCU)** via `lockfile` (port + password): WebSocket `OnJsonApiEvent_lol-gameflow_v1_gameflow-phase`; `GET /lol-gameflow/v1/session`, `/lol-end-of-game/v1/eog-stats-block`, `/lol-summoner/v1/current-summoner`, `/lol-ranked/v1/current-ranked-stats` | Riot: "not officially supported" for third parties. Optional; degrade gracefully. This is what Overwolf's launcher GEP relays. |

### B2. Signal mapping

"Me" = the active player. Match by `riotId`, then `riotIdGameName`, then `summonerName`.
Note that `KillerName` in events is the game name without the tag (A3).

| Outplayed highlight | Riot source | Downplayed derivation | Improvement over Outplayed |
|---|---|---|---|
| `kill` (+ multikill label) | `ChampionKill` (KillerName = me), `Multikill` (KillStreak 2–5) | One marker per kill. A following `Multikill` relabels the moment. Cross-check with `playerlist[me].scores.kills`. | Exact Riot `EventTime` instead of receipt time; per-kill markers |
| `assist` | `ChampionKill` (me ∈ `Assisters`) | — | — |
| `death` | `ChampionKill` (VictimName = me) | Killer may be a turret/minion/monster (execute) | — |
| `ultimate` | **none** | Optional, off: raw-input R-key detector while League is foreground | Opt-in only |
| `victory` | `GameEnd` (`Result` Win/Lose) and LCU EOG stats | `victory` marker 10 s/1 s; set `result.win` from EOG | **Fixes the win-recording bug** (0/117 wins recorded) |
| `baron_kill`, `dragon_kill`, `rift_herald`, `turret_kill`, `inhibitor_kill` | `BaronKill`, `DragonKill` (`DragonType`, `Stolen`), `HeraldKill`, `TurretKilled`, `InhibKilled` | Same as Outplayed (KillerName = me), plus optional "my team took it" and "stolen" labels | Uses `Stolen`/`DragonType`; tracks the last EventID correctly |
| *(not in Outplayed)* first blood, ace, shutdown, sprees, Voidgrubs/Elder dragon | `FirstBlood` (`Recipient`), `Ace` (`Acer`, `AcingTeam`); sprees/shutdowns derived in `tracker::streaks`; newer objectives (verify names) | Default **off**, opt-in | New |
| match start/end | LCU gameflow `InProgress` → `WaitingForStats`/`EndOfGame`; Live `GameStart`/`GameEnd` | State machine B3 | Ends on game end, not process exit; handles `Reconnect` |
| champion, queue, mode, K/D/A | `playerlist` (`championName`, `scores`), `gamestats.gameMode`, LCU session `gameData.queue.id`, EOG stats | Final numbers from EOG | Champion never null; real queue names; modes filterable |
| rank / LP | LCU `/lol-ranked/v1/current-ranked-stats`; EOG LP delta | Optional | Not present in Outplayed |
| `ability`, `abilityReady`, `chat`, announcer VO | **none** | Not replicated | Removes ~89% of Outplayed's event/log volume |
| APM timeline | **none** | Optional, off: per-minute counters from raw input while League is foreground; local only | Opt-in; no keystroke retention |

### B3. Tracker state machine

```
Idle ──(LCU phase ChampSelect)──► Armed ──(LCU InProgress / Live API reachable)──► InGame
InGame: poll eventdata every 500 ms → dedupe by EventID (always advance) → normalize
        → streaks → moments → capture.addMarker
InGame ──(LCU Reconnect)──► InGame (same match; keep last_event_id; re-read eventdata)
InGame ──(GameEnd | LCU WaitingForStats/EndOfGame | game window gone)──► Finalizing
Finalizing: stop capture → EOG stats → write match.json → cut clips (stream copy) → Idle
```

- **Degraded mode (no LCU):** watch for `League of Legends.exe`, then poll `gamestats`
  until it answers. Stop on `GameEnd` or process exit.
- **Filters (settings):** skip Practice Tool / custom / ARAM / Arena per user choice.
  Never record spectator or replay sessions as "my" matches (`activeplayer` errors
  there).

### B4. Clock alignment

`EventTime` is game seconds. Every ~10 s, sample `(gamestats.gameTime, QPC_now)` to map
game time to wall clock. A marker's video time is `wall(EventTime) − recording_start_wall`.
Overlapping windows merge into moments labelled by priority (penta > quadra > triple >
double > kill > objective > assist > death).

### B5. Test plan

1. **Fixtures:** dump `eventdata`, `playerlist`, `activeplayer` and `gamestats` every
   second during Practice Tool and real games, plus LCU gameflow WebSocket frames.
2. **Unit tests:** normalization, multikill relabeling, streaks/shutdowns, moment merging,
   clock mapping, EventID dedupe across a simulated reconnect.
3. **Integration:** a fake HTTPS server on 127.0.0.1:2999 replays fixtures; assert the
   markers and clip ranges.
4. **Parity:** for a game recorded by both apps, Downplayed's markers should be a
   superset of Outplayed's `medias[].events[]` for that match. Win/loss must match the
   client's EOG screen.

### B6. Riot policy constraints

From developer.riotgames.com, Game Client API section:

- Products must not use information that is not present in the game client to gain a
  competitive edge.
- Products may not provide game-session information previously unknown to the player.
- Products may not identify or analyze players who are deliberately hidden.
- The LCU API is "not officially supported" for third parties. Register or annotate the
  application in the Developer Portal.

Downplayed only records what the player already saw. Don't add live enemy-tracking or
champ-select de-anonymization features.
