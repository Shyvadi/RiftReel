# League fixtures

Synthetic data in the shapes Riot's local APIs return. All player names are invented
(`Downplayer#DP1` is always the local player). Real recordings should replace or extend
these once the fixture recorder exists (doc 02 §B5.1). Pseudonymise names before
committing any real recording.

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
| `ranked_win.json` | First blood, death, dragon, assist, turret, double kill, stolen baron, shutdown/spree, team ace, execution by a turret, `GameEnd` **Win** |
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

## League client (`lcu/`)

| File | What |
|---|---|
| `ranked_win.session.json` | `GET /lol-gameflow/v1/session` during `ranked_win` (player lists emptied) |
| `ranked_win.eog.json` | `GET /lol-end-of-game/v1/eog-stats-block` afterwards (identifiers removed) |
| `gameflow_phase_frames.json` | WAMP frames from the client WebSocket for one queue → reconnect → end-of-game cycle |
