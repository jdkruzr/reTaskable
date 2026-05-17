# reTaskable

A CalDAV (VTODO) tasks client for the reMarkable Paper Pro Move, distributed
as an [AppLoad](https://github.com/asivery/rmpp-appload) app for users running
xovi. Nextcloud is the v1 sync target; iCloud is deferred to v1.5.

This repository currently contains **Milestone 9a** — an offline-first task
client with syncing to Nextcloud via CalDAV. Writes queue locally; network
round-trips are optional.

## Layout

```
reTaskable/
├── app/                       application source
│   ├── manifest.json
│   ├── application.qrc
│   ├── ui/Main.qml            QML frontend
│   ├── backend/               Rust backend
│   ├── build-pc.sh            host build (for the AppLoad PC emulator)
│   └── build-rmpp.sh          aarch64 cross-build (for the device)
├── vendor/appload-client/     vendored copy of asivery's appload-client crate
└── sdk/                       reMarkable cross-compile SDK (not committed)
```

## Prerequisites

- Rust toolchain (`cargo`, `rustup`) with the `aarch64-unknown-linux-gnu`
  target installed: `rustup target add aarch64-unknown-linux-gnu`
- The reMarkable cross-SDK extracted at `./sdk/` (it provides both `rcc` and
  the `aarch64-remarkable-linux-gcc` linker)
- For on-device install: an RMPPM with xovi + AppLoad already installed

## Configure

Starting with M1, the backend reads CalDAV credentials from a TOML config
file. **The file is not in this repo** — you create it locally on each
host that runs reTaskable.

`~/.config/retaskable/config.toml` (resolves to
`/home/root/.config/retaskable/config.toml` on the device):

```toml
[nextcloud]
base_url     = "https://nextcloud.example.com"
username     = "yourname"
app_password = "xxx-xxx-xxx-xxx-xxx"
```

Generate the app-password in Nextcloud under *Settings → Security → Devices &
sessions → Create new app password*. Use HTTPS only — Basic auth over plain
HTTP would leak the password.

## Build

```bash
# Host build for the AppLoad PC emulator
./app/build-pc.sh

# Cross-compile for the device
./app/build-rmpp.sh
```

## Run in the AppLoad PC emulator

One-time setup (after building the emulator with `qmake6 . && make` in
`~/rm-appload/`):

```bash
mkdir -p ~/rm-appload/applications_root
ln -sfn /home/jtd/reTaskable/app/output-pc ~/rm-appload/applications_root/retaskable
```

Then each run:

```bash
./app/build-pc.sh
(cd ~/rm-appload && ./appload)
```

The symlink means rebuilds are picked up automatically.

## Install on device

```bash
scp -r app/output-rmpp/ root@<device-ip>:/home/root/xovi/exthome/appload/retaskable
```

Launch from the AppLoad menu. Tap **Ping**; the text should change to
`pong from reTaskable`.

## Offline queue (M9a)

reTaskable now stores every write (Create, Toggle, Edit, Delete) in a local
queue and applies it optimistically to the cached task view, then flushes
the queue to the CalDAV server when you tap Sync.

- Writes return "Queued: ..." instantly, even with no network.
- Tap **Sync** to drain the queue and pull server state.
- Tap **Show Pending** to see every queued operation, including any errors.
- Tap **Clear Errored** (twice, to confirm) to drop ops the server rejected;
  the next Sync reconciles your cached view with authoritative server state.

On first launch after upgrading, the local cache is rebuilt from the server
(schema migration). Your data on the server is unchanged.

## Status

- [x] M0: hello-world round-trip
- [x] M1: one HTTPS request to Nextcloud
- [x] M2: RFC 4791 calendar discovery
- [x] M3: read-only task list
- [x] M4: SQLite cache + sync-collection
- [x] M5: toggle complete
- [x] M6: 412 auto-retry helper + delete first task
- [x] M7: create task (minimal VCALENDAR/VTODO + If-None-Match)
- [x] M8: edit summary (reuses retry helper)
- [x] M9a: offline queue + Show Pending + Clear Errored UI
- [ ] M9b: conflict-resolution UI
- [ ] M9c: pending-row markers in Show Tasks
- [ ] M10: polish for v1
- [ ] M11: companion xovi extension — sidebar tile in xochitl that launches reTaskable via `AppLoadLauncher.launchApplication`
