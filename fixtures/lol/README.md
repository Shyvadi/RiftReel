# League fixtures

Data in the shapes Riot's local APIs return. All player names are invented or
pseudonymised (`Downplayer#DP1` is always the local player). The synthetic fixtures are
hand-written; `downplayed-dev record` (doc 02 §B5.1) records real games in the same
format.

## Recording a match

```
cargo run --bin downplayed-dev -- record <out-dir>
```

Start it before or during a game (Practice Tool works). It waits for the Live Client API,
records until the game closes, then waits up to 3 minutes for the League client's
end-of-game stats and exits after the end-of-game screen. It writes:

| File | What |
|---|---|
| `<name>.json` | Live Client timeline in the format below (`<name>` is `<mode>-<UTC date>-<time>`) |
| `lcu/<name>.session.json` | `GET /lol-gameflow/v1/session` during the game |
| `lcu/<name>.eog.json` | `GET /lol-end-of-game/v1/eog-stats-block` after it |
| `lcu/<name>.frames.json` | gameflow-phase WebSocket frames, in arrival order |

The files are rewritten every few seconds, so stopping the recorder early keeps what it
had. Raw payloads stay in memory; what's written is pseudonymised consistently across all
files: you become `Downplayer#DP1`, everyone else `Player1#DP1`, `Player2#DP1`, … in
`playerlist` order, and names in events that match nobody become `Unknown<n>`. puuids,
summoner IDs and account IDs are dropped, and game IDs become `1` wherever they appear
(a real one would look up everyone's name), including inside strings such as the
post-game chat's `"<gameId>-eog"`. Credentials are dropped too: the end-of-game block
carries a post-game chat JWT (`mucJwtDto`, `multiUserChatPassword`) that names your
account. So are profile icons and summoner level/XP. A file that would still contain a
real Riot ID, puuid, game ID, token or the League client password isn't written. Review
a recording before committing it anyway.

`downplayed-dev replay` uses the `lcu/` files of a fixture when they exist, so the result
comes from the end-of-game stats (`--json` shows `resultSource`).

## Live Client Data API timelines (`*.json`)

Each file is one match as `https://127.0.0.1:2999/liveclientdata/*` would serve it:

| Key | Endpoint |
|---|---|
| `activeplayer` | `activeplayer` (`null` when spectating) |
| `activeplayerError` | error body `activeplayer` returns without an active player |
| `playerlist` | `playerlist`, with end-of-game scores |
| `gamestats` | `gamestats` (`gameTime` is filled in by the replayer/fake server) |
| `events` | the complete `eventdata.Events` history |

At game time *t*, `eventdata` returns every event with `EventTime <= t`. The fake server
in the integration tests and `downplayed-dev replay` both work this way.

| Fixture | Covers |
|---|---|
| `ranked_win.json` | First blood, a Voidgrub (`HordeKill`), death, dragon, assist, turret, double kill, stolen baron, shutdown/spree, team ace, execution by a turret, `GameEnd` **Win** |
| `aram_loss.json` | ARAM mode filter, triple kill, executions by champions, `GameEnd` **Lose** (no victory marker) |
| `penta_cap.json` | Multikill relabelling up to a pentakill, and a long assist chain split by the 60 s moment cap |
| `spectator.json` | No active player: never tracked |

Run one through the tracker:

```
cargo run --bin downplayed-dev -- replay fixtures/lol/ranked_win.json
```

`*.expected.txt` holds that command's output (`--extras --team` for the `_extras`
variant), checked by `tests/fixture_replay.rs`. To regenerate it after an intended change,
set `UPDATE_EXPECTED=1` when running that test.

`--extras` turns on the markers that are off by default: Voidgrubs, first blood, ace,
shutdown and sprees.

## League client (`lcu/`)

| File | What |
|---|---|
| `ranked_win.session.json` | `GET /lol-gameflow/v1/session` during `ranked_win` (player lists emptied) |
| `ranked_win.eog.json` | `GET /lol-end-of-game/v1/eog-stats-block` afterwards (identifiers removed) |
| `gameflow_phase_frames.json` | WAMP frames from the client WebSocket for one queue → reconnect → end-of-game cycle |
