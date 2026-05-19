# reTaskable

Last verified: 2026-05-19

A CalDAV task client for the reMarkable Paper Pro (rMPP), running under the AppLoad on-device harness. Rust backend + QML frontend. Speaks to any RFC-compliant CalDAV server; primary test target is Nextcloud (`checkwithscience.com`).

Latest milestone: **M9b — conflict-resolution UI.** When a write loses a race and double-412s, the user gets a Resolve First Conflict button that shows a local-vs-server diff and offers Keep Mine / Take Theirs / Cancel. Toggle + edit only in M9b; delete + create deferred.

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

- `app/backend/src/main.rs` — `AppLoadBackend` impl, MSG dispatch, `sync()`, `compose_sync_response`, the four write handlers (`toggle_first`, `create`, `edit_first`, `delete_first`)
- `app/backend/src/db.rs` — SQLite schema + migration, cache CRUD, `enqueue_{create,edit,toggle,delete}`, `fetch_next_drainable`, `cascade_uid`, `clear_errored`, in-file tests
- `app/backend/src/queue.rs` — `flush_pending`, `dispatch_op`, `apply_outcome`, `classify_error`, `FlushSummary`, `ExecOutcome`, in-file tests including `#[tokio::test]` + httpmock integration
- `app/backend/src/nextcloud.rs` — HTTP helpers: discovery, sync-collection REPORT, `put_task_with_retry` (412 auto-retry), `delete_task_with_retry`, iCalendar mutations (`toggle_completion`, `replace_summary`, `escape_ical_text`)
- `app/backend/src/config.rs` — TOML loader for `~/.config/retaskable/config.toml`
- `app/ui/Main.qml` — Flow of 12 buttons + a TextField + a conditional Keep/Take/Cancel row (visible while a conflict is primed) + the response area
- `vendor/appload-client/` — modified AppLoad client crate (the `?Send` trait change is intentional)
- `docs/design-plans/` — per-milestone design plans (read these before changing architecture)
- `docs/implementation-plans/` — per-milestone implementation plans (read these to understand why specific code shape was chosen)
- `docs/test-plans/` — per-milestone human test plans (manual verification scripts)

## Conventions

**MSG protocol.** Requests are 1–13, responses are 101–113, dense numbering so QML can filter with simple `||` chains. Reserved system types: `0xFFFFFFFE` (new-coordinator), `0xFFFFFFFF` (terminate). Frames are `{type:u32 LE, len:u32 LE}` + UTF-8 payload. M9b's MSG 112 response is the first multi-line payload with a parseable sentinel — its first line is `OPID:<n>\n`, which QML strips and uses to drive the visibility of the Keep/Take/Cancel row.

**Async + rusqlite.** `rusqlite::Connection` is `Send` but `!Sync` (RefCell inside), so `&Connection` is `!Send`. The AppLoadBackend trait uses `#[async_trait(?Send)]` (the harness calls it sequentially under a mutex; Send is gratuitous). Async fns inside the backend hold `&mut Connection` not `&Connection`.

**Offline-first writes.** All four write handlers are *synchronous* — they only enqueue. The `Queued: ...` response renders in under a tick. `sync()` is two-phase: `queue::flush_pending(...).await` first, then sync-collection. A mid-drain transient skips sync-collection that pass; terminals continue draining other UIDs. See `docs/design-plans/2026-05-17-m9a-offline-queue.md` for the full model.

**iCalendar on the wire is CRLF.** Always. `nextcloud::ensure_crlf` runs at every parse boundary (XML extraction strips CRs per XML 1.0 §2.11, so calendar-data comes back LF-only and must be restored). ETags include their surrounding quotes — pass through verbatim.

**Tests are in-file.** No separate `tests/` directory. Each module has its own `#[cfg(test)] mod tests { ... }` block using `Connection::open_in_memory()`. Pre-M9a there were 6 tests; M9a brought it to 54; M9b to 86.

**Diagnostics.** `eprintln!("retaskable: ...")` with lowercase fragments and no terminal punctuation. Captured by xochitl → journald.

**Commits.** Direct-to-main per milestone; no feature branches. Descriptive multi-paragraph commit messages — explain *why*, not just *what*. After hardware verification, push immediately.

## Workflow discipline (small pieces)

Each milestone must verify on real rMPP hardware before the next one is planned. Code review + automated tests are necessary but not sufficient — bugs hide in cross-module contract assumptions that only surface against a real server. Two examples from the offline-queue era:

- **M9a (commit `f3dbd9c`):** `put_task_with_retry` double-mutated the cached body. Survived 54 passing tests and 8 phases of code review; surfaced on the first real toggle round-trip.
- **M9b (commit `5326ee3`):** `ensure_crlf` left a trailing bare CR untouched, so the icalendar crate couldn't parse Nextcloud's GET response. Survived 85 passing tests; surfaced on the first real conflict-preview attempt.

Plan accordingly; budget time for the hardware loop. M9b also added a `manufacture-m9b-conflict.py` test helper at `docs/test-plans/` because natural double-412s require a millisecond-window race that's impractical to trigger by hand — verification needed direct DB-state injection.

## Where to find more context

The forward-looking roadmap and per-milestone history are not in this repo — they live in Jarett's persistent memory at `/home/jtd/.claude/projects/-home-jtd-reTaskable/memory/` (auto-loaded in his sessions). Key files:

- `project_retaskable.md` — architecture overview and roadmap (M0–M11)
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
