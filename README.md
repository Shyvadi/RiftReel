# Downplayed

A lightweight, local-first **League of Legends** clip recorder for Windows. It is a
clean-room alternative to Overwolf's Outplayed, with no ads, no telemetry and no
Overwolf runtime.

Status: **phases 0 and 1 of the [blueprint](docs/03-downplayed-blueprint.md#12-roadmap)**.
The tray app, settings, scrubbed logging, Riot's local API clients and the highlight
tracker work. Video capture (phase 2) and the clip library (phase 3) don't exist yet;
the tracker's capture/library steps are logged no-ops.

| Doc | What it covers |
|---|---|
| [docs/01-outplayed-rea-findings.md](docs/01-outplayed-rea-findings.md) | REA reverse-engineering report. The shortcut → Overwolf → Outplayed identity chain, architecture, plugins, capture engine, ads/telemetry data flows and memory hotspots, with evidence IDs and file:line citations. |
| [docs/02-lol-tracker-spec.md](docs/02-lol-tracker-spec.md) | How Outplayed's League of Legends tracker works (GEP features, events, highlight timing, match lifecycle) and the equivalent official Riot interfaces Downplayed uses instead. |
| [docs/03-downplayed-blueprint.md](docs/03-downplayed-blueprint.md) | Architecture, capture design, data model, storage policy, UI, performance budget, clean-room rules and roadmap. |

## Building

Prerequisites: Rust (the pinned 1.90.0 toolchain installs itself through
`rust-toolchain.toml`), the MSVC C++ build tools with a Windows SDK, Node.js 20.19+ or
22.12+, and the Tauri CLI (`cargo install tauri-cli --version "^2" --locked`).

```bash
npm --prefix ui install
```

```bash
cargo test
```

Run `cargo tauri dev` from the repository root. It starts the Vite dev server in `ui/` and
the app, which lives in the system tray. Left-click the tray icon (or use **Open
Downplayed**) for the window. Closing the window destroys it; **Quit** exits.

```bash
cargo tauri dev
```

Replay a recorded match through the tracker and print its markers and moments:

```bash
cargo run --bin downplayed-dev -- replay fixtures/lol/ranked_win.json
```

`downplayed-dev lcu` follows the running League client's gameflow phase.
`downplayed-dev live` prints markers from a game in progress (try Practice Tool).

> **This machine:** Visual Studio 2022 Community has a partial MSVC 14.42 toolset (no
> headers). `cc-rs` picks it first, so building `ring` fails with "Cannot open include
> file: 'vcruntime.h'". Repair VS 2022's *Desktop development with C++* workload, or
> point builds at the complete VS 2019 toolset:
> `$env:VCToolsVersion = "14.29.30133"` (PowerShell) or `export VCToolsVersion=14.29.30133`.

## Files and environment

| What | Where |
|---|---|
| Settings | `%APPDATA%\Downplayed\settings.toml` (created on first run; every value optional) |
| Logs | `%LOCALAPPDATA%\Downplayed\logs\downplayed.log`, 5 × 5 MB rotation, Riot identifiers scrubbed |
| `DOWNPLAYED_HOME` | Put settings and logs under one folder instead (development) |
| `DOWNPLAYED_LOG` | Override the log level, e.g. `debug` |
| `DOWNPLAYED_OPEN_WINDOW` | Open the window at startup (`hidden`: create it invisible, for smoke tests) |

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
  replay.rs, bin/            fixture replay and the downplayed-dev tool
src-tauri/tests/             fake HTTPS Live Client API + fake LCU, end-to-end runtime tests
fixtures/lol/                synthetic Live Client / LCU fixtures and expected replay output
ui/                          Vite + Svelte status window
```

Downplayed isn't endorsed by Riot Games and doesn't reflect the views or opinions of
Riot Games or anyone officially involved in producing or managing Riot Games
properties. Riot Games and all associated properties are trademarks or registered
trademarks of Riot Games, Inc. Outplayed and Overwolf are trademarks of Overwolf Ltd.;
Downplayed is not affiliated with them.
