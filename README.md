# Unofficial Google Home Volume Sync

**Website: [googlehomevolumesync.com](https://googlehomevolumesync.com)**

A Windows desktop app (Tauri 2, pure Rust backend) that discovers Google Cast devices on your LAN and gives you per-device and per-group volume control — fully local, no cloud APIs.



## Features

- **Auto-discovery** of all Google Cast devices (Home/Nest speakers, Chromecasts, cast-enabled TVs, cast groups) via continuous mDNS browsing — new devices just appear.
- **Per-device volume sliders** with live updates (changes made on the speaker or in the Google Home app reflect immediately).
- **App groups**: put any devices in a group, control them with one slider.
- **Volume sync**: optional per-group — if one member's volume changes, the others are forced to match.
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
