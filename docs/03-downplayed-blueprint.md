# Downplayed — clean-room blueprint (League of Legends only)

Downplayed is a lightweight, local-first League of Legends clip recorder for Windows.
It reproduces what Outplayed's LoL tracker does for the player (auto-detect matches,
record, mark kills/deaths/objectives, cut highlights, manage storage) without Overwolf,
ads, telemetry, remotely loaded code or a stack of Chromium windows.

This blueprint is derived from the REA investigation in
[01-outplayed-rea-findings.md](01-outplayed-rea-findings.md) and the tracker spec in
[02-lol-tracker-spec.md](02-lol-tracker-spec.md). It is a *functional* spec: build from
these behaviors and from Riot's public interfaces, not from Outplayed's code or assets
(see [Clean-room rules](#11-clean-room-and-licensing-rules)).

---

## 1. Goals and non-goals

**Goals**

1. Parity with Outplayed's League experience: match detection, full-match recording,
   automatic highlight markers with the same default pre/post-roll windows, manual
   "save replay" hotkey, match library with KDA/champion/result, clip export.
2. Small, predictable footprint: one Rust process + one encoder process while recording,
   **no UI process at all** while the window is closed.
3. Zero ads, zero telemetry, no remote configuration, no remotely loaded scripts.
   Network access limited to `127.0.0.1` (Riot's local APIs) plus optional, cacheable
   Data Dragon static assets.
4. Storage you control: highlight clips kept, full matches pruned by age/quota.
5. Import the existing Outplayed library (`D:\Outplayed\League of Legends`, ~430 GB,
   544 recordings on this machine) with its match metadata where available.

**Non-goals (for v1)**

- Other games, TFT, cloud upload, accounts, social feeds, premium tiers, watermarks.
- Anything Riot's policies forbid: no information the client doesn't show the player,
  no de-anonymizing hidden players, no gameplay automation.
- Global keyboard/mouse hooks by default (Outplayed ships a low-level keyboard hook
  plugin; Downplayed uses `RegisterHotKey` instead).

---

## 2. Why Outplayed is heavy (what to avoid)

Observed in the REA investigation (details and evidence IDs in doc 01):

| Outplayed / Overwolf behavior | Downplayed decision |
|---|---|
| Runs inside the Overwolf client (CEF/Chromium). The manifest declares 9 windows, and each window loads the shared `dependencies.js` plus its own bundle (`index.js` alone is 5.1 MB). | Single Rust core. One WebView2 window, created on demand and **destroyed** when closed. |
| Background page is always on with `background_optimization: false`. | Core is event-driven and idle-polls only the cheap foreground/LCU state. |
| Every GEP event is logged (`ability`, `usedAbility`, `abilityReady`, `chat` ≈ 193k of 228k LoL log lines). 68 MB of background logs in about 3 weeks. Riot identifiers (puuid, account id) are logged in plain text. | Structured logs at `info` level only for state changes. Per-event logging is `trace` and off by default. Riot IDs are redacted. 5 × 5 MB rotation. |
| An unpinned ads SDK (`content.overwolf.com/libs/ads/latest/owads.min.js`) runs with full app privileges. Storage keys not written by the shipped code, most likely by that SDK, hold hashed emails, a hardware profile, installed games, and geo/ISP. | No third-party JS. Strict CSP (`script-src 'self'`). No ad or analytics code paths. |
| Capture runs in a separate Overwolf-managed OBS 31 build (`ow-obs.exe`) driven through the Overwolf streaming API. | Direct capture pipeline we own (§5). |
| One flat quota (default 25 GB; set to 500 GB on this machine), checked every 12 h. The oldest unprotected media go to the recycle bin, so full matches and highlights compete for the same space. | Two-tier retention: full matches by age/size, clips kept (§8). |
| Every analytics hit carries Overwolf username + MUID + MUIDV2 (`analyticsnew.overwolf.com/analytics/Counter`), with no opt-out. | No analytics. If diagnostics are ever added: opt-in, local file, user sends it. |
| A remote flag (`collect-keystrokes`) can turn on in-clip keystroke capture, which is attached to outplayed.tv shares with machineId + username. | No input capture by default. Any optional APM/ultimate heuristic keeps per-minute counters locally and never transmits. |
| An unauthenticated localhost WebSocket bridge (port 54310) executes any dotted function path from the background window. | No IPC listeners. Tauri commands are an explicit allowlist (Tauri capabilities), UI ↔ core only. |
| Remote config can change ads, telemetry, plans, the Overwolf update channel and Sentry rate, and inject promo HTML (`dangerouslySetInnerHTML`). | No remote config. Settings are local TOML. Updates are opt-in and signature-verified. |
| Client-side payment secrets (a hard-coded Tebex API key). | No payments. Never ship secrets in the client. |
| Main window auto-opens after every game, with ad slots; Discord RPC on by default with a promo button. | Nothing opens unless asked. Optional "match ready" toast only. |

---

## 3. Architecture

```
┌──────────────────────────── downplayed.exe (Rust, Tauri 2) ────────────────────────────┐
│                                                                                        │
│  tray + lifecycle ── settings (TOML) ── hotkeys (RegisterHotKey)                       │
│                                                                                        │
│  league::client  ──(LCU WebSocket: gameflow phase, session, EOG stats)──┐              │
│  league::live    ──(GET 127.0.0.1:2999/liveclientdata/* during game)────┤              │
│                                                                         ▼              │
│                                       tracker::normalize → tracker::streaks            │
│                                                 │  Marker{type,label,t_game,past,future}
│                                                 ▼                                      │
│   capture::controller ── spawns/controls ──► ffmpeg.exe (sidecar)                      │
│        │                  ddagrab → h264/hevc/av1 (NVENC|AMF|QSV) → fMP4 / TS segments │
│        └─ audio::wasapi (loopback + mic, optional game-only) ──named pipes──►          │
│                                                                                        │
│   library::db (SQLite index) ◄─► library::sidecar (match.json per match folder)        │
│   library::janitor (quota/age/low-disk) · library::import_outplayed                    │
│                                                                                        │
│   ui (WebView2, on demand) ◄── Tauri commands/events ──► core                          │
│   overlay (tiny native layered window for "Kill saved" toasts, optional)               │
└────────────────────────────────────────────────────────────────────────────────────────┘
```

**Stack:** Rust (tokio, reqwest/rustls, tokio-tungstenite, rusqlite, serde, tracing,
`windows` crate), Tauri 2 for the window, tray and global-shortcut plugin, and a small
Vite + Svelte (or React) frontend. ffmpeg is a pinned, checksum-verified sidecar binary.

---

## 4. League integration (replaces Overwolf GEP)

Outplayed gets League data from Overwolf's Game Events Provider. It requests the features
`kill, death, assist, summoner_info, game_mode, announcer, matchState, abilities,
live_client_data, match_info, me` (observed in logs, doc 02 §1). GEP's
`live_client_data` feature is Riot's own Live Client Data API relayed through
Overwolf, so Downplayed goes to the source.

### 4.1 Match lifecycle — League Client (LCU)

- Discover the client: find `LeagueClientUx.exe`, read `<install dir>\lockfile`
  (`name:pid:port:password:protocol`). Fall back to the `--app-port` and
  `--remoting-auth-token` command-line arguments.
- Connect `wss://127.0.0.1:<port>/` with basic auth `riot:<password>` (self-signed cert,
  localhost only). Subscribe to `OnJsonApiEvent_lol-gameflow_v1_gameflow-phase`.
- Phase handling:
  - `ChampSelect`: pre-warm the encoder (probe only, no capture).
  - `GameStart` / `InProgress`: arm the Live Client poller and start capture when the
    game window appears.
  - `WaitingForStats` / `PreEndOfGame` / `EndOfGame`: stop capture, then fetch
    `/lol-end-of-game/v1/eog-stats-block` for result, KDA, champion and queue.
- `/lol-gameflow/v1/session` gives queue id, map and game mode. Use it to skip unwanted
  modes (e.g. practice tool, or ARAM if the user disables it).
- Riot states the LCU API is *not officially supported* for third parties. Isolate it
  behind a trait. The Live Client API alone can still drive a degraded mode: start on
  `GameStart`, stop on `GameEnd` or on the window closing.

### 4.2 In-game events — Live Client Data API (official)

- Base `https://127.0.0.1:2999/liveclientdata/`. Validate the TLS chain against Riot's
  `riotgames.pem` root cert (published on developer.riotgames.com).
- Poll `eventdata` every 500 ms while in game. Deduplicate by `EventID`, which also
  keeps reconnects safe. Fetch `activeplayer` once (Riot ID), `playerlist` at start
  and on demand (team/champion map), and `gamestats` every ~10 s for clock alignment.
- Names: endpoints now return `riotId`, `riotIdGameName` and `riotIdTagLine`, and
  `summonerName` is a shim. Match "me" the way Riot does: riotId → gameName →
  summonerName.
- Before hard-coding event names, dump `https://127.0.0.1:2999/swagger/v3/openapi.json`
  and record `eventdata` from Practice Tool and custom games. The documented sample
  lists `GameStart, MinionsSpawning, FirstBrick, TurretKilled, InhibKilled, DragonKill,
  HeraldKill, BaronKill, ChampionKill, Multikill, Ace`. `FirstBlood`, `GameEnd`,
  `InhibRespawningSoon` and `InhibRespawned` are also emitted. Newer objectives (e.g.
  Voidgrubs) need verification against a live dump. Atakhan has been removed from the
  game, so don't map it.

### 4.3 Event normalization (Outplayed parity)

The full mapping table lives in doc 02 §B2. The defaults below copy Outplayed's
observed windows:

| Marker | Riot source | Pre-roll | Post-roll |
|---|---|---|---|
| `kill` (label `kill/double_kill/triple_kill/quadra_kill/penta_kill`) | `ChampionKill` with KillerName = me; label from `Multikill.KillStreak` | 12 s | 8 s |
| `assist` | `ChampionKill` with me ∈ Assisters | 12 s | 8 s |
| `death` | `ChampionKill` with VictimName = me | 12 s | 3 s |
| `turret_kill` / `inhibitor_kill` | `TurretKilled` / `InhibKilled` with KillerName = me | 10 s | 5 s |
| `dragon_kill` / `rift_herald` / `baron_kill` (+ Voidgrubs once verified) | `DragonKill` / `HeraldKill` / `BaronKill` (KillerName = me, or team, configurable) | 10 s | 5 s |
| `victory` | `GameEnd` Result = Win | 10 s | 1 s |
| `ultimate` (optional, off) | No official source (GEP `abilities`). Optional raw-input R-key heuristic while League is focused | 5 s | 5 s |
| *Downplayed additions (default off):* `first_blood`, `ace`, `shutdown`, sprees | `FirstBlood`, `Ace`; sprees/shutdowns derived from the kill sequence (`tracker::streaks`) | 10 s | 5 s |

Outplayed's League highlight set is exactly kill, assist, death, victory, ultimate,
baron, dragon, herald, turret and inhibitor (`background.js:27788`). The last row
lists extras Downplayed can offer because it sees the full Riot event stream.

**Where Downplayed deliberately differs from Outplayed's tracker** (doc 02 Part A):

- **Timestamps:** use Riot `EventTime` with clock alignment (§4.4). Outplayed stamps
  events on *receipt* (`Date.now()`), so clip windows drift by the GEP relay latency.
- **Win/loss:** set it from `GameEnd.Result` / LCU end-of-game stats. Outplayed's
  `isVictory` path never fires; 0 of 117 League matches on this machine were stored as
  wins.
- **Match end:** end on game end, not on process exit, and survive a `Reconnect`
  instead of splitting into a new match.
- **Champion and mode:** take them from `playerlist`/LCU (Outplayed's was null in 5 of
  117 matches), and let the user filter Practice Tool/ARAM/Arena. Outplayed records
  everything and drops all events in some modes.
- **Event IDs:** always advance `last_event_id`. Outplayed only advances it on the
  local player's objective events and re-scans the rest.
- **Ignored signals:** don't subscribe to ability spam or chat at all. They were 89% of
  Outplayed's League event and log volume.

Overlapping windows merge into one *moment*. The label is the highest-priority event
(penta > quadra > triple > double > kill > objective > assist > death). Clip length is
capped (default 60 s).

### 4.4 Clock alignment

Riot `EventTime` is game time in seconds. Recording time comes from the capture wall
clock. Sample `gamestats.gameTime` with a monotonic timestamp every ~10 s and compute
`video_t = gameTime_to_wall(EventTime) − recording_start_wall`. Keep the last good
offset across API hiccups.

---

## 5. Capture

**Recommendation: an ffmpeg sidecar using `ddagrab` and hardware encoders.**

- League runs on one monitor (borderless or fullscreen). Desktop Duplication captures it
  without injecting into the game, which matters under Vanguard. `ddagrab` hands D3D11
  frames straight to NVENC/AMF, so there are no CPU copies.
- The local ffmpeg is `5.1.2-essentials`, which has **no `ddagrab`**. Ship a pinned
  7.x/8.x LGPL build that includes `ddagrab`, `h264_nvenc/hevc_nvenc/av1_nvenc`,
  `*_amf`, `*_qsv`. This machine has an RTX 5070 Ti and an RTX 3070 Ti, so NVENC
  (including AV1 on the 5070 Ti) is available.
- **Encoder probe at first run:** NVENC → AMF → QSV → x264 fallback (warn). Store the
  result.
- **Defaults** match the user's Outplayed settings: 2560×1440 @ 60 fps, ~10 Mbps.
  Offer presets. Keyframe every 1 s (`-g 60`) so stream-copy cuts land within 1 s.
- **Full-match mode:** fragmented MP4 (`-movflags +frag_keyframe+empty_moov+default_base_moof`)
  so a crash never loses the file. On stop, optionally remux to a regular MP4 (`-c copy`).
- **Highlights/manual mode:** a disk ring buffer of 2 s MPEG-TS segments
  (`-f segment -segment_time 2 -segment_wrap N`). Saving concatenates the needed
  segments with `-c copy`. RAM stays flat regardless of buffer length, unlike an
  in-memory replay buffer.
- **Audio:** capture in Rust via WASAPI (loopback + microphone; optional per-process
  loopback for game-only audio, which mirrors Outplayed's `captureOnlyGameAudio`).
  Stream PCM over named pipes as separate ffmpeg inputs with wallclock timestamps,
  `aresample=async=1`, and one track per source. **Risk:** A/V sync must be validated
  over 40-minute games. The fallback is to write audio with QPC timestamps and mux at
  finalize with computed offsets.
- **Track layout:** write the mixed track as track 0 *at capture time* (ffmpeg `amix`
  in the same graph), with game, mic and (optional) voice-chat as tracks 1–3.
  Outplayed records per-source tracks: League process audio on track 1, mic on 2,
  Discord/Spotify/Chrome on 3–5. It then re-muxes every finished clip through the
  plugin's `CreateMuxedFirstAudioTrack` to add a mixed track 0. That is an extra
  full-file read/write per clip, which Downplayed avoids.
- **Upgrade path if profiling demands it:** in-process Windows.Graphics.Capture +
  Media Foundation encoder (one process, window-scoped capture). Don't start there.

---

## 6. Capture modes

Outplayed's values are `highlights | fullMatch | fullSession | manual`; League is set to
`fullMatch` on this machine.

| Downplayed mode | Behavior |
|---|---|
| `full_match` (default) | Record GameStart → GameEnd. Markers on the timeline. Auto-cut highlight clips at match end with stream copy (no re-encode). |
| `highlights` | Disk ring buffer. Each moment is saved as a clip when its post-roll elapses. No full match kept. |
| `manual` | Ring buffer. Only the hotkey saves the last N seconds (default 20 s, Outplayed's `manualVideoCaptureLength`). |

Hotkey semantics to match Outplayed (static read, doc 01 §4):

- **F8 (Save replay):** past = 20 s, future = 0.
- **Ctrl+F4 (start/stop):** opens a capture with 5 s pre-roll; it closes on the second
  press or at match end.
- **Ctrl+F2 (screenshot):** saves to `<match>\Screenshots`.

Outplayed keeps an in-memory OBS replay buffer of `2 × max(180 s, …)` (6 min by
default) for highlight mode. Downplayed's disk ring buffer makes that length cost
nothing in RAM.

`fullSession` (launch-to-exit) adds little for League and is dropped.

---

## 7. Library and data model

Folder per match (portable, rebuildable):

```
<root>\League of Legends\2026-10-15_19-02-11_Ranked-Solo_Shyvana\
    match.mp4           full match (fMP4 or remuxed)
    match.json          sidecar: everything needed to rebuild the index
    clips\0001_double_kill.mp4 …
    thumbs\0001.jpg …
```

`match.json` (superset of Outplayed's `MATCH_*` record, minus ad/experiment fields):

```json
{
  "schema": 1,
  "id": "uuid",
  "riot": { "gameId": 0, "platformId": "NA1", "queueId": 420, "gameMode": "CLASSIC", "patch": "" },
  "player": { "championName": "", "team": "ORDER|CHAOS" },
  "result": { "win": true, "kills": 0, "deaths": 0, "assists": 0, "durationS": 0 },
  "recording": { "startWallMs": 0, "endWallMs": 0, "file": "match.mp4", "encoder": "hevc_nvenc", "width": 2560, "height": 1440, "fps": 60 },
  "markers": [ { "id": 1, "type": "kill", "label": "double_kill", "gameTimeS": 0, "videoTimeS": 0, "preS": 12, "postS": 8, "riotEventIds": [12, 13] } ],
  "clips": [ { "file": "clips/0001_double_kill.mp4", "fromS": 0, "toS": 0, "favorite": false } ],
  "favorite": false
}
```

SQLite (`library.db`) is a cache of the sidecars. If it is lost or corrupt, rescan the
folders. Outplayed writes **no sidecar files**. All match metadata lives in the CEF
profile's IndexedDB `MediaDatabase` (v17, store `matches`), migrated from older
localStorage `MATCH_*` keys. The `D:\Outplayed\Recovered\outplayed-library-db-backup`
folder on this machine shows that database has needed recovery before. Downplayed
should never depend on a single database file.

**Outplayed importer:** walk `D:\Outplayed\League of Legends\League of Legends_<MM-DD-YYYY>_<H-m-s-ms>\*.mp4`.
If the user points at a localStorage backup (`D:\Outplayed\temp\outplayed.json`), join
its `MATCH_<sessionId>_<n>` records (champion, K/D/A, isVictory, `medias[].events[]`
with `time` + `timing.past/future`) to recover markers. Ignore every `owad*`, `owads_*`,
`hashedEmails`, `__mpq_*`, experiment and remote-config key.

---

## 8. Storage management

- Two quotas: **full matches** (default 150 GB or 30 days) and **clips** (default
  unlimited, warn at 50 GB). Favorites are never auto-deleted.
- The janitor runs at match end and hourly while idle. It deletes the oldest
  unprotected full matches first and removes empty folders and orphaned thumbnails.
- Low-disk guard: refuse to start recording below a threshold (default 20 GB free) and
  show one notification.
- First run after import: show the size breakdown (League 430 GB on this machine) and
  offer "keep highlights, delete full matches older than N days".

---

## 9. UI (single window, on demand)

1. **Matches:** cards with champion icon, queue, W/L, K/D/A, duration and date.
   Filters for champion, queue, result and has-multikill.
2. **Match view:** HTML5 `<video>` streaming from disk via Tauri's asset protocol (never
   loaded into memory). Timeline with marker icons, a moment list, "export clip" and
   "favorite".
3. **Clips:** grid, favorites, reveal in Explorer, copy file to clipboard.
4. **Settings:** capture (resolution, fps, bitrate, encoder), audio (devices, game-only),
   events (per-type toggle + pre/post-roll), hotkeys (Save replay `F8`, Start/stop
   `Ctrl+F4`, Screenshot `Ctrl+F2`, Outplayed's defaults), storage, import.
5. **In-game toasts (optional):** a native click-through layered window showing
   "Double kill saved" for 3 s. Off by default. No webview in game.

Champion icons: bundle a small icon set or fetch once from Data Dragon and cache it.
Make the fetch opt-in so the app works fully offline.

---

## 10. Performance budget and how to verify

| State | Target |
|---|---|
| Idle in tray, no game | core < 30 MB RSS, ~0% CPU |
| In game, recording | core < 60 MB, ffmpeg < 250 MB, < 3% CPU, encoder on GPU |
| UI open | + WebView2 (~100–150 MB), freed on close |

Verify with a scripted 30-minute Practice Tool session. Sample `Get-Process` for
downplayed and ffmpeg every 5 s, check the ffmpeg drop/lag counters, and compare
against the same session under Overwolf + Outplayed. That comparison is the one runtime
measurement this investigation did not perform (doc 01 §9).

---

## 11. Clean-room and licensing rules

- Use the behaviors and data shapes in docs 01/02 as requirements. **Do not copy
  Outplayed's JavaScript, plugin code, images, sounds, onboarding video, SVG sprites,
  `gameslist.json` or watermark.** Fonts like Lato/Montserrat are OFL; get them from
  their upstream sources if wanted.
- Overwolf GEP is proprietary and not used. All game data comes from Riot's local
  interfaces.
- ffmpeg: ship an LGPL build as a separate executable with its license and source
  offer. Avoid GPL-only parts (x264) unless Downplayed itself is GPL. The same applies
  if you ever embed libobs (GPL-2.0).
- Riot: follow the Developer Portal policies quoted in doc 02 §6. Register the product
  if you distribute it, and show the standard "not endorsed by Riot Games" notice.
  The LCU API is unsupported and can change without notice.
- Name/branding: keep Downplayed visually distinct from Outplayed (logo, colors,
  layout). "Downplayed" is a play on "Outplayed"; get advice before publishing
  commercially.

---

## 12. Roadmap

| Phase | Deliverable | Done when |
|---|---|---|
| 0 | Tauri scaffold, tray, settings file, logging with redaction | App idles in tray < 30 MB |
| 1 | `league::client` + `league::live` + normalizer + streaks, with fixture tests and a fake 2999 server | Recorded eventdata fixtures produce the expected markers |
| 2 | ffmpeg sidecar capture (full match + ring buffer), encoder probe, WASAPI audio pipes | 40-min recording, no drops, A/V in sync (< 40 ms) |
| 3 | Library (sidecar + SQLite), match/clip UI, stream-copy clip cutting, thumbnails | End-to-end: play a game → match card with markers → export clip |
| 4 | Janitor, low-disk guard, hotkeys, optional toasts, Outplayed importer | Imports `D:\Outplayed\League of Legends` with markers recovered |
| 5 | Polish: montage/concat export, game-only audio, AV1/HEVC presets, perf comparison report | Measured footprint vs Overwolf + Outplayed published in docs |

Proposed repo layout:

```
Downplayed/
  src-tauri/src/{main.rs, tray.rs, settings.rs, hotkeys.rs, overlay.rs}
  src-tauri/src/league/{client.rs, live.rs, models.rs}
  src-tauri/src/tracker/{normalize.rs, streaks.rs, moments.rs, clock.rs}
  src-tauri/src/capture/{controller.rs, ffmpeg.rs, probe.rs, audio_wasapi.rs}
  src-tauri/src/library/{db.rs, sidecar.rs, janitor.rs, import_outplayed.rs, thumbs.rs}
  ui/                      # Vite + Svelte/React
  fixtures/lol/            # recorded eventdata / gameflow JSON for tests
  tools/fetch-ffmpeg.ps1   # pinned URL + SHA-256
  docs/
```
