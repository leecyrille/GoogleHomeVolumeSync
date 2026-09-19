# Unofficial Google Home Volume Sync

**Website: [googlehomevolumesync.com](https://googlehomevolumesync.com)** &nbsp;&middot;&nbsp; **[Download for Windows](https://github.com/leecyrille/GoogleHomeVolumeSync/releases/latest/download/GoogleHomeVolumeSync-setup.exe)** &nbsp;&middot;&nbsp; **[&#9749; Buy me a coffee](https://pactotech.com/products/google-home-volume-sync-tip-jar)**

A Windows desktop app (Tauri 2, pure Rust backend) that discovers Google Cast devices on your LAN and gives you per-device and per-group volume control — fully local, no cloud APIs.

## Screenshots

Every speaker on the network, with live volume sliders and a per-device Sync Gain for balancing rooms:

![Devices](docs/screenshots/devices.png)

Sync groups keep their members' volumes matched — change one anywhere and the rest follow:

![Sync Groups](docs/screenshots/groups.png)

Right-click the tray icon to set a whole group in 5% steps without opening the app:

<img src="docs/screenshots/tray.png" width="330" alt="Tray menu">

Weekly schedules quiet the house on your terms, and settings keep it starting with Windows:

![Schedule](docs/screenshots/schedule.png)

![Settings](docs/screenshots/settings.png)

## Features

- **Auto-discovery** of all Google Cast devices (Home/Nest speakers, Chromecasts, cast-enabled TVs, cast groups) via continuous mDNS browsing — new devices just appear.
- **Per-device volume sliders** with live updates (changes made on the speaker or in the Google Home app reflect immediately).
- **Sync groups**: put any devices in a group and control them with one slider; if one member's volume changes — in the app, in the Google Home app, or on the speaker itself — the others are forced to match.
- **Sync Gain** (0–200% per device): balance rooms against each other. A device at 50% gain sits at half the group's level and reports its own changes back at double, so groups stay consistent while quiet or loud rooms are corrected.
- **Media controls**: play / pause / next / previous for active sessions.
- **Scheduler**: weekly events (day-of-week checkboxes + time) that set volumes on devices or groups; each event has an enable checkbox.
- **System tray**: right-click → pick a group → set volume in 5% steps, or send transport commands. Open App / Exit. Close button hides to tray.
- **Custom names** for devices and groups.
- **Last seen** shown for devices offline >1 day, with a delete option (they re-add when seen again).
- **Config export/import** (JSON).
- **Full logging** of discoveries, observed changes and sent commands — always with cast UUIDs, IPs and names — to `%APPDATA%\GoogleHomeVolumeSync\logs\`.
- **Start with Windows** (login autostart, starts hidden in tray).
- **Optional auto-updates** from GitHub releases.

### Additional device backends

| Backend | Discovery | Volume | Hardware tested | Notes |
|---|---|---|---|---|
| Google Cast | automatic (mDNS) | absolute | yes | primary backend |
| Roku TV | "Scan for Roku TVs" (SSDP) | pseudo-absolute | yes | emulated via keypress ramping with a cached level; Recalibrate button re-zeros |
| Yamaha MusicCast (e.g. RX-V581) | add by IP | absolute | **no** | Yamaha Extended Control JSON API |
| LG webOS TV | add by IP | absolute | **no** | one-time on-screen pairing prompt |
| Optoma projector | add by IP | absolute (0–10 range) | **no** | RS-232-over-Telnet, experimental |

The Yamaha MusicCast, LG webOS and Optoma backends are written to their published protocols but have **not been tried against real hardware** — expect them to need fixes on first use. Bug reports from anyone who owns these are very welcome. The Roku backend has been tested and works.

## Support the project

This app is free and open source, built by one person. If it made your house sound better, a small tip keeps it that way: **[Buy me a coffee](https://pactotech.com/products/google-home-volume-sync-tip-jar)** (tip jar on the Pacto Tech store, pick any amount).

## Development

```
npm install
npm run tauri dev
```

Requires Rust (MSVC toolchain) and Node.

## Building the installer

```
npm run tauri build
```

Produces an NSIS `.exe` installer under `src-tauri/target/release/bundle/nsis/` (per-user install, no admin needed).

Update signing: the updater public key is in `tauri.conf.json`; the private key lives outside the repo (`~/.tauri/ghvs.key`). Release artifacts for auto-update are published to a public releases repo as `latest.json` + signed installers.

## Config & logs

- Config: `%APPDATA%\GoogleHomeVolumeSync\config.json`
- Logs: `%APPDATA%\GoogleHomeVolumeSync\logs\volume-sync.log.*` (daily rolling)
