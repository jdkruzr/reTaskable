# reTaskable

<img width="400" alt="rTscreen" src="https://github.com/user-attachments/assets/db39b33f-66ed-4a9f-98fb-f9e6bd0bf9a2" /> <img width="400" alt="rTselect" src="https://github.com/user-attachments/assets/df5f9e0c-b45a-4aa9-8e58-634c80ca6e3f" />


A local-first task client for **reMarkable tablets**, with optional sync to an
RFC-compliant **CalDAV** server. It runs on-device under
[xovi](https://remarkable.guide/guide/software/xovi.html) with
[AppLoad](https://github.com/asivery/rmpp-appload). You can keep a list entirely
on the tablet, sync with Nextcloud or another CalDAV server, or try the beta
iCloud Reminders integration.

What makes it paper-native: you can **lasso handwriting in any notebook and turn
it into a to-do**, and that to-do keeps a link back to the exact page it came
from, so a tap takes you straight back to your notes.

## Features

- **Works without an account** — the permanent **On This reMarkable** list is
  ready on first launch and never depends on a sync server.
- **Optional CalDAV sync** — two-way sync with Nextcloud and other compliant
  servers. Complete, create, edit, reschedule, and delete tasks.
- **iCloud Reminders (beta)** — root-first Apple CalDAV discovery handles the
  per-user server redirect while keeping credentials on approved Apple hosts.
- **Explicit copy and move** — send a local task to your synced list; a move
  deletes the local copy only after the remote create is confirmed.
- **Due dates & times** — set a due date (and optional time) when you create a
  task or later from its detail view, with one-tap presets (Today / Tomorrow /
  +1 week) or a tap-and-type date field.
- **Capture from handwriting** *(optional)* — circle some handwriting in a
  notebook, tap **Send to reTaskable**, and it’s recognized into a to-do. The
  task lands in the currently selected list and shows a 📓 link to the source
  notebook and page.
- **Jump back to your notes** *(optional)* — open a captured to-do and tap
  **📓 Open note** to jump straight to the page you wrote it on.
- **Works offline** — local-list changes stay local, while synced-list changes
  are saved instantly and queued. Sync conflicts get a clear keep-mine /
  take-theirs prompt.
- **In-app setup** — point it at your server, username, and app password right
  on the device; no config files to edit.
- **e-ink friendly** — clean, high-contrast layout with discrete page turns (no
  laggy kinetic scrolling).

## Requirements

- A **reMarkable tablet** with [xovi](https://remarkable.guide/guide/software/xovi.html)
  and [AppLoad](https://github.com/asivery/rmpp-appload) installed.
- The optional handwriting-capture and jump-back hooks currently support
  **reMarkable OS 3.27.x**. The standalone reTaskable app has a broader support
  range because it does not patch xochitl's private QML.
- A **CalDAV account is optional**. For sync you need a tasks/VTODO calendar,
  its server URL and username, and an **app password** rather than your main
  account password.

## Install

The easiest path is [Vellum](https://remarkable.guide/guide/software/vellum.html),
the reMarkable package manager, driven by the
[reManager](https://github.com/rmitchellscott/reManager) desktop app (Linux /
macOS / Windows). Vellum resolves dependencies automatically, so installing
reTaskable also pulls in everything it needs — **xovi**, **AppLoad**, and (for
the handwriting-capture and jump-back features) the **command-executor** and
**xovi-message-broker** extensions.

**With reManager (recommended):**

1. Install reManager on your computer (e.g. from
   [Flathub](https://flathub.org/apps/io.scottlabs.reManager)) and connect your
   reMarkable.
2. If you haven’t already, use reManager to install **Vellum** on the device.
3. Find **reTaskable** in the package list and install it. Accept the
   dependencies it brings along.
4. Reload xovi when prompted (or reboot the tablet), then open **reTaskable**
   from the AppLoad launcher.

**With vellum-cli:** `vellum add retaskable` (then reload xovi / reboot).

> Building from source instead? See [Building from source](#building-from-source).

## First-run setup

1. Open **reTaskable** from the AppLoad launcher. **On This reMarkable** is
   immediately available; no setup is required for local tasks.
2. To add sync, tap **Settings**, choose **Generic / Nextcloud** or
   **iCloud Reminders (Beta)**, and enter your username and app password.
3. For a generic server, also enter its CalDAV URL (for example,
   `https://nextcloud.example.com`).
   - In Nextcloud: *Settings → Security → Devices & sessions → Create new app
     password*. Always use `https://`.
   - For iCloud, create an app-specific password in your Apple Account. Do not
     enter your normal Apple Account password.
4. Discover the available VTODO collections, pick one, and save.
5. Select either list from the task-list strip. Tap **Sync** to exchange changes
   with the configured remote list.

You can change the server, account, or calendar any time from **Settings**.

## Using reTaskable

- **Complete a task** — tap its checkbox. Completed tasks hide by default; tap
  **Show Completed** to see them.
- **Create a task** — type a summary in the field at the bottom, optionally set
  a **Due** date/time (presets or tap-and-type), and tap **Create**.
- **Edit / reschedule / delete** — tap a task’s text to open its detail view.
  Edit the summary, change or clear the due date, or delete it (two taps to
  confirm).
- **Copy or move a local task** — open a task from **On This reMarkable** and
  choose **Copy to synced list** or **Move to synced list**.
- **Sync** — tap **Sync** to push your queued changes and pull the latest from
  the server. The header shows when you last synced.
- **Page through a long list** — use the ▲ / ▼ buttons (paging is per-screen,
  which keeps the e-ink display crisp).

### Capture a to-do from your handwriting *(optional feature)*

1. In any notebook, use the **selection (lasso)** tool to circle some
   handwriting.
2. In the selection menu, tap the **checkmark icon**.
3. Your handwriting is recognized; review or edit the text, then tap **Create**.
   The note itself is never modified.
4. The new to-do appears in whichever task list was selected most recently,
   with a 📓 line showing the source notebook and page. A synced-list capture is
   pushed automatically even if reTaskable isn't open; a local-list capture
   stays on the tablet until you explicitly copy or move it.

> Online handwriting recognition needs Wi-Fi and a signed-in reMarkable account.
> Offline, you’ll get a text field to type the task instead.

### Jump back to the source note

Open a captured to-do’s detail view and tap **📓 Open note** — reTaskable closes
and your notebook opens to the exact page you wrote the task on.

## Building from source

For development, or to install without Vellum.

**Prerequisites**

- Rust toolchain with the device target:
  `rustup target add aarch64-unknown-linux-gnu armv7-unknown-linux-gnueabihf`
- Qt's `rcc` on `PATH`.
- The matching reMarkable cross linker on `PATH`
  (`aarch64-remarkable-linux-gnu-gcc` for Paper Pro / Move,
  `arm-linux-gnueabihf-gcc` for rM1/rM2).
- A reMarkable tablet with xovi + AppLoad for on-device install.

**Build**

```bash
./app/build-pc.sh      # host build for the AppLoad PC emulator
./app/build-rmpp.sh    # aarch64 cross-build for Paper Pro / Move
./app/build-rm.sh      # armv7 cross-build for rM1/rM2
```

**Install the app on the device**

```bash
./app/install-device.sh           # defaults to 10.11.99.1 over USB-net
# or: ./app/install-device.sh <device-ip>
```

This installs to `/home/root/xovi/exthome/appload/retaskable/`. Refresh the
AppLoad launcher and open reTaskable.

**Install the capture / jump-back hooks** *(optional)*

The handwriting-capture and jump-back features are xovi+qmldiff hooks in
`app/xovi/`. Copy them into the qt-resource-rebuilder extension directory and
reload xovi (a reMarkable “triple-tap” gesture, or reboot):

```bash
scp app/xovi/retaskableSend.qmd app/xovi/retaskableJump.qmd \
  root@<device-ip>:/home/root/xovi/exthome/qt-resource-rebuilder/
```

These hooks also require the `command-executor` (capture auto-sync) and
`xovi-message-broker` (jump-back) xovi extensions — the same dependencies Vellum
would install for you.

**Run tests**

```bash
cd app/backend && cargo test --bin retaskable-backend
```

### Configuration file (advanced)

The in-app **Settings** screen is the normal way to configure reTaskable, and it
writes this file for you. You can also edit it directly at
`~/.config/retaskable/config.toml` (on the device:
`/home/root/.config/retaskable/config.toml`):

```toml
[caldav]
provider     = "generic"
base_url     = "https://nextcloud.example.com"
username     = "yourname"
app_password = "xxx-xxx-xxx-xxx-xxx"
calendar_href = "https://nextcloud.example.com/remote.php/dav/calendars/yourname/tasks/"
calendar     = "Tasks"
```

`calendar_href` is the stable collection identity; `calendar` is only its
display name. Existing `[nextcloud]` configurations remain readable and are
migrated the next time Settings saves them. For a local-only configuration,
the file may be absent entirely.

### Diagnostics

Settings has a **Load diagnostics** button that shows the tail of a persistent,
rotating log. The log records DAV methods, status/error categories, elapsed
time, and sanitized host names; it does not record passwords, calendar paths,
task contents, or server response bodies.

For a support bundle, use reManager's terminal/SSH facility or connect over USB
and run:

```bash
ssh root@10.11.99.1 \
  /home/root/xovi/exthome/appload/retaskable/backend/entry --diagnostics
```

The current and previous files are
`/home/root/.local/share/retaskable/diagnostics.log` and
`diagnostics.log.1`. reManager's AppLoad **Debug** action is also useful for
foreground startup errors that occur before the persistent logger is ready.

## Compatibility & roadmap

reTaskable targets **RFC-compliant CalDAV**, not any one server. It’s verified
against **Nextcloud** and a **go-webdav** server, and it degrades gracefully on
servers that don’t support the optional WebDAV-Sync extension (it falls back to
a full `calendar-query` each sync).

iCloud Reminders support is beta until it has broader live-account coverage.
It uses Apple's CalDAV surface with an app-specific password, starts discovery
at `https://caldav.icloud.com`, and only forwards authentication to the
corresponding approved Apple CalDAV shard. reTaskable currently edits its
existing task fields and preserves unknown iCalendar properties when it rewrites
a task; recurrence and alarms still have no dedicated UI.

Not yet supported, on the roadmap:

- **Setting a due date at capture time** — today a to-do captured from a note
  comes in without a due date; you add it afterward in reTaskable.
- Recurring tasks (`RRULE`) and reminders/alarms.

## Project layout

```
reTaskable/
├── app/
│   ├── ui/                 QML frontend (Main.qml, DueField.qml)
│   ├── backend/            Rust backend
│   ├── xovi/               xochitl hooks: capture (retaskableSend.qmd) + jump-back (retaskableJump.qmd)
│   ├── build-pc.sh         host build (AppLoad PC emulator)
│   ├── build-rmpp.sh       aarch64 cross-build (Paper Pro / Move)
│   ├── build-rm.sh         armv7 cross-build (rM1/rM2)
│   └── install-device.sh   deploy to a connected device
├── vendor/appload-client/  vendored AppLoad client crate
└── docs/                   design / implementation / test plans
```

## License

See [`LICENSE`](LICENSE).
