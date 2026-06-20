# reTaskable

Last verified: 2026-06-20

A CalDAV task client for the reMarkable Paper Pro (rMPP), running under the AppLoad on-device harness. Rust backend + QML frontend. Speaks to any RFC-compliant CalDAV server; test targets are Nextcloud (`checkwithscience.com`) and a go-webdav server (UltraBridge) — see M12.

Latest milestone: **M14 Phase 1 — the xochitl capture hook (producing end of "lasso → to-do").** A xovi + qmldiff extension (`app/xovi/retaskableSend.qmd`) that adds a "Send to reTaskable" button to the **handwriting lasso menu** (`SceneSelectionHandler.qml`'s `?#tools`: cut/copy/convert/delete). On tap it captures the selection as a CalDAV to-do **without altering the note**: three coordinated patches mirror the stock convert-to-text wiring — (A) the menu button + a `sendToReTaskable` signal on `SceneSelectionHandler`; (B) `DeviceSceneView` wires the signal to a *non-destructive* convert-on-clone (`controller.cloneSelectedItems` → `hwcSelectionTool.sendToReTaskableFrom`, no `deleteSelectedItems`); (C) `HwcSelection` runs cloud recognition (`Recognizer.startSelectionToolJob`, no insert/UI) and, in a `REBUILD`'d `handwritingBar.onFinishedChanged`, reads `recognitionJob.plainText` plus the anchor (`document.id`, `currentPage`) and writes an M13 intake JSON (`cap_<ts>.json`) the backend drains. Hardware-verified end to end: handwrite → lasso → tap → cloud MyScript → anchored to-do with `X-RETASKABLE-SOURCE-{DOC,PAGE,LABEL}`. The button shows a `qrc:/ark/icons/checkmark_circle` glyph. The `.qmd` ships **hashed** (publish rule: committed = qmldiff-hashtab tokens, not symbol names; unhash with `qmldiff hash-diffs <device-hashtab> … -r` to edit, re-hash before committing — the round-trip is verified byte-clean apart from the intended diff). Backend prep: `db::open` creates the intake dir on startup so the hook's `file://` PUT always has a target. Offline dev tooling + xochitl-QML extraction method in `docs/m14-tooling/`. **Next:** auto-sync-after-capture, then **Phase 2** — an inline confirm/type dialog in xochitl (shown on every capture; pre-fills recognized text, editable, type-fallback when recognition is unavailable). Jump-back navigation is M15.

Prior milestones:
- **M13 — note-anchor intake (reTaskable receiving end of "lasso → to-do").** Builds the *receiving* end: an **intake spool** (`~/.local/share/retaskable/intake/*.json`) the backend drains on app launch (MSG 18/118 `drain_intake`, fired by QML on load) and at the top of every `sync()`. Each file → an **anchored create**: `db::enqueue_create_with_anchor` stamps `X-RETASKABLE-SOURCE-{DOC,PAGE,LABEL}` into the VTODO. The label renders as a `📓 …` line under the to-do (parsed back via `nextcloud::extract_source_label`; surfaced in the MSG 4/104 envelope as a per-task `source` field). The anchor survives the offline-queue flush because the create payload carries the full cached body and the queue PUTs it verbatim (`put_task_create_body`) instead of rebuilding from summary — so cache == what's sent == what the server stores (hardware-verified: all three X-props round-tripped through UltraBridge).
- **M12 — CalDAV servers without WebDAV-Sync.** Some servers (e.g. go-webdav) reject the `sync-collection` REPORT (`400 ... unsupported REPORT root "DAV:" "sync-collection"`). The sync path detects this (`nextcloud::sync_collection_unsupported`) and falls back to an RFC 4791 `calendar-query` REPORT (`calendar_query_all`), enumerating every VTODO each sync (no sync-token → always a full pull, reusing `parse_sync_response`). `db::set_last_synced` records a sync time without a token so token-less servers still show "Last synced …". Makes the "RFC-compliant for any server" posture real, not Nextcloud-only.
- **M11 — in-app Settings.** A settings overlay edits the full sync target (base_url, username, app password, calendar) and doubles as first-run setup. New MSG 15/115 `get_config` (returns a `has_password` bool, never the secret), 16/116 `save_config`, 17/117 `discover_with` (validate creds + list calendars *before* saving). `config::{load_optional, save, merge_password}` (blank password = keep existing). `db::reset_cache` fires only when the target (base_url or calendar) changes; a creds-only change keeps the cache + queued ops.
- **M10 — the real ListView UI.** Replaced the M0–M9c button-Flow + single response-Text scaffolding with a scrollable `ListView`/`ListModel`. MSG 4 / response 104 now carry a JSON envelope (`{last_synced, tasks:[{uid,summary,completed,due,mark}]}` via `format_tasks_json`). A per-row checkbox toggles a *specific* task by UID (MSG 14/114 `toggle_by_uid`, reusing `enqueue_toggle`'s optimistic cache flip; the compact reply updates one row, no full redraw). Completed tasks are hidden by default with a Show Completed toggle (`filter_for_display`; MSG 4 payload `"all"`/`"open"`). e-ink: kinetic scrolling is disabled (`interactive: false`) in favor of ▲/▼ discrete paging — one screenful per tap = one clean refresh.
- **M9c — pending-op visualization (now superseded for display).** `format_tasks_marked` prefixed each Show Tasks row with `! `/`* `/`  `. The live path emits JSON now; `format_tasks_marked` is retained only for its tests (`#[cfg_attr(not(test), allow(dead_code))]`).
- **M9b — conflict-resolution UI.** Resolve First Conflict shows a local-vs-server diff (Keep Mine / Take Theirs / Cancel) when a write double-412s. Toggle + edit only.

## Tech stack

- Rust 2021, single binary `retaskable-backend` (no library target — tests run via `cargo test --bin retaskable-backend`)
- rusqlite 0.32 (bundled SQLite, statically linked); schema is at `schema_version = 2`
- reqwest 0.12 with `rustls-tls-webpki-roots` (no system OpenSSL on device)
- tokio 1 (multi-thread runtime; `rt-multi-thread` feature on)
- Other deps: anyhow, uuid, chrono, icalendar 0.16, roxmltree, url, serde_json
- Dev-deps: `httpmock = "0.7"` (release binary is unaffected)
- Vendored: `vendor/appload-client/` (Rust crate that talks to AppLoad's AF_UNIX SOCK_SEQPACKET frames). **We've modified it (`?Send` on the async trait); treat it as forkable, not third-party.**
- Frontend: QML (Qt 5/6 dual-compatible) at `app/ui/Main.qml`

## Commands

| Action | Command |
|--------|---------|
| Host build (emulator) | `cd app && ./build-pc.sh` |
| Cross-build (rMPP aarch64) | `cd app && ./build-rmpp.sh` (requires `source sdk/environment-setup-cortexa55-remarkable-linux` first) |
| Run tests | `cd app/backend && cargo test --bin retaskable-backend` |
| Deploy to device | `cd app && ./install-device.sh` (default IP `10.11.99.1`, USB-net) |
| Watch device logs | `ssh root@10.11.99.1 'journalctl -u xochitl -f' \| grep retaskable` |
| Inspect device DB | `scp -O root@10.11.99.1:/home/root/.local/share/retaskable/db.sqlite /tmp/` then Python's `sqlite3` module (no CLI on busybox) |

The build scripts handle the cross-compile env (linker, sysroot, `CC_*` per-target vars for cc-rs). The install script kills the running backend, wipes the remote dir, scps fresh, and uses `-O` for legacy SCP protocol.

## Project structure

- `app/backend/src/main.rs` — `AppLoadBackend` impl, MSG dispatch, `sync()` (drains intake first), `compose_sync_response`, write handlers (`toggle_first` + the UID-addressed `toggle_by_uid`/`toggle_by_uid_inner`, `create`, `edit_first`, `delete_first`), `show_tasks` (JSON envelope, open/all filter), the M11 settings handlers (`get_config`, `save_config`/`save_config_inner` + `target_changed`, `discover_with`), and the M13 intake (`drain_intake` + testable `drain_intake_from`, `IntakeItem`/`make_anchor`)
- `app/backend/src/db.rs` — SQLite schema + migration, cache CRUD, `enqueue_{create,edit,toggle,delete}` (+ M13 `enqueue_create_with_anchor`; `enqueue_create` is now a `None` wrapper), `fetch_next_drainable`, `cascade_uid`, `clear_errored`, `pending_marks`, `source_labels` (M13 uid→label map, mirrors `pending_marks`), `reset_cache` (M11 target-switch wipe), `set_last_synced` (M12 token-less sync time), `intake_dir` (M13 spool path), the `Anchor` struct, in-file tests
- `app/backend/src/queue.rs` — `flush_pending`, `dispatch_op`, `apply_outcome`, `classify_error`, `FlushSummary`, `ExecOutcome`, in-file tests including `#[tokio::test]` + httpmock integration. `dispatch_create` PUTs the create payload's cached `ical` body verbatim (M13: carries anchor X-props), falling back to a summary rebuild for pre-M13 ops
- `app/backend/src/nextcloud.rs` — HTTP helpers: discovery, `sync_collection_with_fallback` (sync-collection → `calendar_query_all` when `sync_collection_unsupported`), `put_task_with_retry` (412 auto-retry), `delete_task_with_retry`, `put_task_create_body` (M13: PUT an exact VTODO body), iCalendar mutations (`toggle_completion`, `replace_summary`, `escape_ical_text` + inverse `unescape_ical_text`), M13 anchor read (`extract_source_label`, unfolds + unescapes), task rendering (`format_tasks_json` live — now emits a per-task `source` field; `format_tasks_marked` test-only; `filter_for_display`)
- `app/backend/src/config.rs` — TOML load + write for `~/.config/retaskable/config.toml`: `load`, `load_optional` (None on missing), `save`/`save_to`, `merge_password`
- `app/ui/Main.qml` — scrollable `ListView` of tasks (pending-mark + checkbox + summary + M13 `📓 source` line), ▲/▼ discrete paging, Show Completed toggle, Create field, Sync, the M9b Keep/Take/Cancel row, the M11 Settings overlay (URL/username/password + calendar picker), and the M13 launch-time intake drain (MSG 18 → `applyDrainResult` loads the list)
- `vendor/appload-client/` — modified AppLoad client crate (the `?Send` trait change is intentional)
- `docs/design-plans/` — per-milestone design plans (read these before changing architecture)
- `docs/implementation-plans/` — per-milestone implementation plans (read these to understand why specific code shape was chosen)
- `docs/test-plans/` — per-milestone human test plans (manual verification scripts)

## Conventions

**MSG protocol.** Requests are 1–18, responses are 101–118, dense numbering so QML can filter with simple `||` chains. Reserved system types: `0xFFFFFFFE` (new-coordinator), `0xFFFFFFFF` (terminate). Frames are `{type:u32 LE, len:u32 LE}` + UTF-8 payload. Notable payloads: **MSG 4 / 104** carry a JSON task envelope (M10; request payload `"open"`/`"all"` selects the completed filter; M13 added a per-task `source` field = the note-anchor label, `""` when absent); **14 / 114** `toggle_by_uid` (request = uid; reply = compact `{uid,completed,mark}`); **15–17 / 115–117** settings (`get_config` / `save_config` / `discover_with`, JSON both ways — `get_config` returns `has_password` but never the secret); **18 / 118** `drain_intake` (M13; request payload ignored; reply = compact `{"ingested":N}`); **112** (conflict preview) is multi-line with an `OPID:<n>\n` sentinel QML strips to drive the Keep/Take/Cancel row. Several response bodies are now JSON; QML `JSON.parse` failures are treated as plain status/error text (and clear the task list — see the M11 stale-list fix).

**Async + rusqlite.** `rusqlite::Connection` is `Send` but `!Sync` (RefCell inside), so `&Connection` is `!Send`. The AppLoadBackend trait uses `#[async_trait(?Send)]` (the harness calls it sequentially under a mutex; Send is gratuitous). Async fns inside the backend hold `&mut Connection` not `&Connection`.

**Offline-first writes.** All four write handlers are *synchronous* — they only enqueue. The `Queued: ...` response renders in under a tick. `sync()` is two-phase: `queue::flush_pending(...).await` first, then sync-collection. A mid-drain transient skips sync-collection that pass; terminals continue draining other UIDs. See `docs/design-plans/2026-05-17-m9a-offline-queue.md` for the full model.

**iCalendar on the wire is CRLF.** Always. `nextcloud::ensure_crlf` runs at every parse boundary (XML extraction strips CRs per XML 1.0 §2.11, so calendar-data comes back LF-only and must be restored). ETags include their surrounding quotes — pass through verbatim.

**Tests are in-file.** No separate `tests/` directory. Each module has its own `#[cfg(test)] mod tests { ... }` block using `Connection::open_in_memory()`. Pre-M9a there were 6 tests; M9a brought it to 54; M9b to 86; M9c to 94; M10 to 99; M11 to 107; M12 to 110; M13 to 120.

**Diagnostics.** `eprintln!("retaskable: ...")` with lowercase fragments and no terminal punctuation. Captured by xochitl → journald. Never log secrets — `save_config`/`discover_with` log only a char count, not the payload.

**e-ink scrolling.** The app has no control over the display refresh (xochitl owns the waveform pipeline; an in-scene AppLoad QML frontend can't set refresh modes — that would require a windowed/qtfb app, a possible future milestone). Kinetic/flick scrolling fights the refresh (one flick spans many slow cycles → lag + ghosting), so lists use `interactive: false` + explicit ▲/▼ paging. Do **not** reintroduce `flickDeceleration` (the `0` value is invalid) or `cacheBuffer: 0` (delegate churn).

**CalDAV sync strategy.** Prefer `sync-collection` (RFC 6578, incremental via sync-token). If the server rejects it (`sync_collection_unsupported`), fall back to a full `calendar-query` (RFC 4791) each sync. sync-collection is *optional* — never assume a server supports it.

**Commits.** Direct-to-main per milestone; no feature branches. Descriptive multi-paragraph commit messages — explain *why*, not just *what*. After hardware verification, push immediately.

## Workflow discipline (small pieces)

Each milestone must verify on real rMPP hardware before the next one is planned. Code review + automated tests are necessary but not sufficient — bugs hide in cross-module contract assumptions that only surface against a real server. Two examples from the offline-queue era:

- **M9a (commit `f3dbd9c`):** `put_task_with_retry` double-mutated the cached body. Survived 54 passing tests and 8 phases of code review; surfaced on the first real toggle round-trip.
- **M9b (commit `5326ee3`):** `ensure_crlf` left a trailing bare CR untouched, so the icalendar crate couldn't parse Nextcloud's GET response. Survived 85 passing tests; surfaced on the first real conflict-preview attempt.

Plan accordingly; budget time for the hardware loop. M9b also added a `manufacture-m9b-conflict.py` test helper at `docs/test-plans/` because natural double-412s require a millisecond-window race that's impractical to trigger by hand — verification needed direct DB-state injection.

## Where to find more context

The forward-looking roadmap and per-milestone history are not in this repo — they live in Jarett's persistent memory at `/home/jtd/.claude/projects/-home-jtd-reTaskable/memory/` (auto-loaded in his sessions). Key files:

- `project_retaskable.md` — architecture overview and roadmap (M0–M12)
- `project_retaskable_status.md` — per-milestone "what shipped" log
- `lessons_learned.md` — gotchas: CRLF, busybox quirks, scp `-O`, cc-rs per-target env, the M9a bug discoveries
- `reference_locations.md` — SDK and external repo paths

For collaborators or AI sessions without access to that memory: the `docs/` tree in this repo is the canonical project-context source. Design plans + implementation plans together describe what every milestone does and why.

## Device specifics

- **Address**: `root@10.11.99.1` over USB-net, passwordless SSH already set up.
- **Userland is busybox** — `ps aux` doesn't work (use plain `ps`), no `sqlite3` CLI (scp the DB locally), `head -N` is `head -n N`, `pkill -f <substring>` matches against argv.
- **Install path**: `/home/root/xovi/exthome/appload/retaskable/`. AppLoad keeps backends alive after the frontend closes; `install-device.sh` always `pkill`s first to avoid re-attaching to a stale process.
- **Screen**: ~954 px wide portrait. Layouts use `Flow { width: parent.width }` to wrap cleanly.
- **DB lives at** `/home/root/.local/share/retaskable/db.sqlite`. First launch after upgrading from a v1 schema drops and recreates tables (intentional — the next Sync repopulates from the server, which is authoritative).

## Boundaries

- Safe to edit: anything under `app/`, `docs/`
- Vendored but forkable: `vendor/appload-client/` — modifications are intentional (see the `?Send` change), but consult the design history before changing further
- Build artefacts (gitignored): `app/backend/target/`, `app/output-rmpp/`, `app/output-pc/`
- User config (not in repo): `~/.config/retaskable/config.toml`
