# Downplayed

A lightweight, local-first **League of Legends** clip recorder for Windows. It is a
clean-room alternative to Overwolf's Outplayed, with no ads, no telemetry and no
Overwolf runtime.

Status: **design phase**. The investigation of Outplayed and the build plan live in `docs/`:

| Doc | What it covers |
|---|---|
| [docs/01-outplayed-rea-findings.md](docs/01-outplayed-rea-findings.md) | REA reverse-engineering report. The shortcut → Overwolf → Outplayed identity chain, architecture, plugins, capture engine, ads/telemetry data flows and memory hotspots, with evidence IDs and file:line citations. |
| [docs/02-lol-tracker-spec.md](docs/02-lol-tracker-spec.md) | How Outplayed's League of Legends tracker works (GEP features, events, highlight timing, match lifecycle) and the equivalent official Riot interfaces Downplayed uses instead. |
| [docs/03-downplayed-blueprint.md](docs/03-downplayed-blueprint.md) | Architecture, capture design, data model, storage policy, UI, performance budget, clean-room rules and roadmap. |

Downplayed isn't endorsed by Riot Games and doesn't reflect the views or opinions of
Riot Games or anyone officially involved in producing or managing Riot Games
properties. Riot Games and all associated properties are trademarks or registered
trademarks of Riot Games, Inc. Outplayed and Overwolf are trademarks of Overwolf Ltd.;
Downplayed is not affiliated with them.
