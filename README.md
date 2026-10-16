# RiftReel

A lightweight, local-first **League of Legends** clip recorder for Windows. It is a
clean-room alternative to Overwolf's Outplayed, with no ads, no telemetry and no
Overwolf runtime.

Status: **phases 0–4 of the [blueprint](docs/03-riftreel-blueprint.md#12-roadmap)**.
The tray app, settings, scrubbed logging, Riot's local API clients and the highlight
tracker work. Each tracked match is recorded
to `match.mp4` beside its `match.json`, with nothing to press: an ffmpeg sidecar
captures the monitor League runs on with Desktop Duplication (`ddagrab`, nothing
injected into the game) and the GPU's encoder, straight to disk, and the sound
(everything the output device plays, or only League's, plus the microphone if asked) is
mixed in as it records. Nothing is clipped during the game. The window shows whether the
League client is detected, the tracker is ready and the recorder is running, and lists
every tracked match and fixture recording from an index of the library: a page at a
time, with a thumbnail each, filtered by champion, queue, result, clips, multikills and
dates, and sorted. It reviews one: result, K/D/A, the video, a timeline of moments and
markers that seeks it, and clips. ✂ saves a highlight as a clip, or mark in and out on
the player to save any part; each is a stream copy out of the recording, listed with a
thumbnail, and can be played, made a favorite, shown in Explorer, copied to the
clipboard as a file, saved as a copy, or sent to the Recycle Bin. After each game the
window opens on that game's review, once the game has closed. Storage shows what the
library takes and, once turned on, the cleanup keeps the full-match videos within an age
and a size by sending the oldest to the Recycle Bin; it shows a dry run first. An
Outplayed library's League videos can be imported where they are: they're listed,
played and clipped like RiftReel's own, with what Outplayed's localStorage backup knew
about them (champion, K/D/A, a win, markers), and nothing in the Outplayed folder
changes.

| Doc | What it covers |
|---|---|
| [docs/02-lol-tracker-spec.md](docs/02-lol-tracker-spec.md) | How Outplayed's League of Legends tracker works (GEP features, events, highlight timing, match lifecycle) and the equivalent official Riot interfaces RiftReel uses instead. |
| [docs/03-riftreel-blueprint.md](docs/03-riftreel-blueprint.md) | Architecture, capture design, data model, storage policy, UI, performance budget, clean-room rules and roadmap. |

Docs 02 and 03 cite "doc 01", a reverse-engineering report of Outplayed that this
repository doesn't publish. RiftReel is built from those functional specs and Riot's
public interfaces, never from Outplayed's code or assets (blueprint §11).

## Building

Prerequisites: Rust (the pinned 1.90.0 toolchain installs itself through
`rust-toolchain.toml`), the MSVC C++ build tools with a Windows SDK, Node.js 20.19+ or
22.12+, and the Tauri CLI (`cargo install tauri-cli --version "^2" --locked`).

```bash
npm --prefix ui install
```

Capture needs an ffmpeg with `ddagrab` (the common 5.1 "essentials" build has none). This
downloads the pinned LGPL build, checks its SHA-256 and installs it with its license in
`src-tauri/binaries/` (not committed):

```bash
powershell -ExecutionPolicy Bypass -File tools/fetch-ffmpeg.ps1
```

Debug builds run it from there. A release build ships it next to `riftreel.exe`, with its
license, through the bundle overlay (kept out of `tauri.conf.json` so that plain builds
and `cargo test` don't need the binary):

```bash
cargo tauri build --config src-tauri/tauri.bundle.json
```

For everyday use, `tools/install-local.ps1` builds a release (its UI inside) and installs
it for this Windows user in `%LOCALAPPDATA%\Programs\RiftReel`, with ffmpeg beside it
and a **RiftReel** Start menu shortcut to pin to the taskbar. Run it again after
changes: a running RiftReel keeps going, and the next start (after **Quit** from the
tray) is the new build.

```bash
powershell -ExecutionPolicy Bypass -File tools/install-local.ps1
```

```bash
cargo test
```

Run `cargo tauri dev` from the repository root. It starts the Vite dev server in `ui/` and
the app, which opens its window. Closing the window destroys it and leaves RiftReel in
the system tray: left-click the tray icon (or use **Open RiftReel**) for the window
again; **Quit** exits. Opening RiftReel while it runs brings up its window rather than
starting a second one, and `--tray` starts it with the tray icon only.

```bash
cargo tauri dev
```

Replay a recorded match through the tracker and print its markers and moments:

```bash
cargo run --bin riftreel-dev -- replay fixtures/lol/ranked_win.json
```

`riftreel-dev lcu` follows the running League client's gameflow phase.
`riftreel-dev live` prints markers from a game in progress (try Practice Tool).
`riftreel-dev record` records the next game as a pseudonymised fixture into the
recordings folder the window lists (see [fixtures/lol/README.md](fixtures/lol/README.md)),
and `riftreel-dev eog` prints what RiftReel reads from the end-of-game stats.
`riftreel-dev probe` runs the encoder probe (NVENC → AMF → QSV → CPU) and lists the
monitors, `riftreel-dev audio` lists the sound devices, `riftreel-dev capture 30`
records 30 seconds the way a match is recorded and prints the frame, dropped-frame and
duplicated-frame counts and how the audio went, `riftreel-dev library [--cold]`
times the library index (rebuilt from scratch with `--cold`) and a few list queries,
`riftreel-dev cleanup` prints the cleanup's dry run, and
`riftreel-dev outplayed D:\Outplayed` what importing an Outplayed library would add
(`--write` imports it).

> **Troubleshooting:** if building `ring` fails with "Cannot open include file:
> 'vcruntime.h'", `cc-rs` picked an incomplete MSVC toolset (for example a partial
> Visual Studio 2022 install). Repair the *Desktop development with C++* workload, or
> point the build at a complete toolset, e.g. `$env:VCToolsVersion = "14.29.30133"`
> (PowerShell) or `export VCToolsVersion=14.29.30133`.

## Files and environment

| What | Where |
|---|---|
| Settings | `%APPDATA%\RiftReel\settings.toml` (created on first run; every value optional) |
| Logs | `%LOCALAPPDATA%\RiftReel\logs\riftreel.log`, 5 × 5 MB rotation, Riot identifiers scrubbed |
| Matches | `%LOCALAPPDATA%\RiftReel\library\League of Legends\<date>_<time>_<queue>_<champion>\` (UTC), one folder per tracked match: `match.json`, `match.mp4` when it was recorded, the clips made from it in `clips\` with their thumbnails in `thumbs\`, and its list thumbnail `thumbs\match.jpg` |
| Library index | `%LOCALAPPDATA%\RiftReel\library\library.db`: a cache of the match folders' `match.json` files, rebuilt from them if it is lost or damaged |
| Cleanup notes | `%LOCALAPPDATA%\RiftReel\library\cleanup.json`: videos the cleanup couldn't recycle, and its last run |
| Imports | `%LOCALAPPDATA%\RiftReel\library\imports.json`: the Outplayed folders imported, and whether the cleanup may remove their videos. Each imported video gets a folder `<date>_<time>_Outplayed[_<champion>]` whose `match.json` points at it |
| Encoder probe | `%LOCALAPPDATA%\RiftReel\capture-probe.json`: which encoder works here, probed once per ffmpeg binary |
| Recordings | `%LOCALAPPDATA%\RiftReel\recordings`, where `riftreel-dev record` saves by default |
| `RIFTREEL_HOME` | Put settings, logs, matches and recordings under one folder instead (development); such a RiftReel runs beside the real one |
| Installed app | `%LOCALAPPDATA%\Programs\RiftReel` (`tools/install-local.ps1`), and the Start menu shortcut **RiftReel** |
| `RIFTREEL_LOG` | Override the log level, e.g. `debug` |
| `RIFTREEL_OPEN_WINDOW` | `hidden`: create the window invisible at startup (smoke tests) |

Capture settings live in `[capture]`: `enabled`, `fps` (60), `bitrate_mbps` (10),
`encoder` (`auto`, or e.g. `h264_nvenc`) and `ffmpeg_path` (empty: the bundled ffmpeg).
The resolution is the monitor's League runs on. While a match is recorded ffmpeg writes
a fragmented MP4 with a keyframe every second, so a crash keeps the file; when it ends
the file is remuxed (a stream copy) into a regular MP4, which a player opens without
reading through it, and the audio is moved into sync with the video. `[capture.audio]`
picks the sound: `enabled`, `game_only` (only League's sound, not everything the output
device plays), `output_device` and `microphone_device` (part of a name; empty for the
default), `microphone` (off by default) and the volumes. Track 0 is the mix; each
source also gets a track of its own.

`[storage]` guards the library's drive: no recording starts with less than `min_free_gb`
free (20), and one stops cleanly, keeping its file, below `stop_free_gb` (2). Either is
said once, after the game: in the window if it is open, else as a Windows notification.
Nothing is ever shown during a game.

The cleanup is off until turned on (`cleanup`, or the Storage view). Then, after each
match and every hour, never during a game, full-match videos older than `keep_days` (30)
go to the Recycle Bin, then the oldest while full matches take more than
`full_matches_gb` (150). Clips are never removed (the window warns above
`clips_warn_gb`, 50), nor are favorites (★ in a match's review) or imported videos
(unless allowed for that import in Storage). A video too big for the Recycle Bin is never
deleted: it stays, flagged in Storage.
`riftreel-dev cleanup` prints what the cleanup would do now.

## Layout

```
src-tauri/src/
  app.rs, tray.rs, main.rs   tray app; the window is created on demand and destroyed on close
  settings.rs, logging.rs    TOML settings; tracing with size rotation
  privacy.rs                 pseudonyms in Debug output + log scrubber for Riot IDs/puuids
  league/live.rs             Live Client Data API client + EventID cursor (reconnect-safe)
  league/client.rs           League client (LCU): lockfile/argument discovery, REST, WSS phase feed
  league/tls.rs              trust only Riot's root (certs/riotgames.pem), strict verification
  tracker/                   normalize, streaks, moments, clock, machine (B3), session, runtime
  capture/                   ffmpeg sidecar (ddagrab + NVENC/AMF/QSV), encoder probe, monitor
                             lookup (DXGI), WASAPI audio on QPC time, clip cutting, the
                             controller that records each match
  library.rs, library/       match folders (match.json, match.mp4, clips), their SQLite
                             index (index.rs), the cleanup (janitor.rs) and the Outplayed
                             importer (outplayed.rs)
  thumbnails.rs              list thumbnails, made in the background (never in game)
  shell.rs                   Recycle Bin (never deleting outright), clipboard, Save as,
                             Explorer
  replay.rs, bin/            fixture replay and the riftreel-dev tool
src-tauri/tests/             fake HTTPS Live Client API + fake LCU, end-to-end runtime tests
fixtures/lol/                synthetic Live Client / LCU fixtures and expected replay output
ui/                          Vite + Svelte window: status, matches, review with video
tools/fetch-ffmpeg.ps1       pinned LGPL ffmpeg build (URL + SHA-256)
tools/install-local.ps1      release build installed for this user, Start menu shortcut
tools/make-icons.py          draws the icon set in src-tauri/icons (Pillow)
```

## License

[MIT](LICENSE). The ffmpeg build that `tools/fetch-ffmpeg.ps1` downloads is LGPL and is
shipped as a separate program next to RiftReel, with its license and source offer.

RiftReel isn't endorsed by Riot Games and doesn't reflect the views or opinions of
Riot Games or anyone officially involved in producing or managing Riot Games
properties. Riot Games and all associated properties are trademarks or registered
trademarks of Riot Games, Inc. Outplayed and Overwolf are trademarks of Overwolf Ltd.;
RiftReel is not affiliated with them.
