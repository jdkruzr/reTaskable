# reTaskable

A CalDAV (VTODO) tasks client for the reMarkable Paper Pro Move, distributed
as an [AppLoad](https://github.com/asivery/rmpp-appload) app for users running
xovi. Nextcloud is the v1 sync target; iCloud is deferred to v1.5.

This repository currently contains **Milestone 0** — a hello-world AppLoad
app that proves the toolchain, cross-compile, and QML↔Rust protocol end to
end. No CalDAV yet.

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

## Status

- [x] M0: hello-world round-trip
- [x] M1: one HTTPS request to Nextcloud
- [x] M2: RFC 4791 calendar discovery
- [x] M3: read-only task list
- [ ] M4: SQLite cache + sync-collection
- [ ] M5: toggle complete
- [ ] M6: full CRUD with 412 handling
- [ ] M7: offline queue + conflict resolution
- [ ] M8: polish for v1
- [ ] M9: companion xovi extension — sidebar tile in xochitl that launches reTaskable via `AppLoadLauncher.launchApplication`
