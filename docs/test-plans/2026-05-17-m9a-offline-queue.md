# Human Test Plan — M9a Offline Queue

Coverage validation: PASS. 53/53 automated tests pass. All 23 automatable acceptance criteria covered. The plan below covers hardware-only ACs (rMPP landmarks for Phases 6 and 7) plus end-to-end scenarios that exercise multiple ACs in the workflows real users will hit.

## Prerequisites

- rMPP device with built `retaskable-backend` binary and AppLoad QML frontend deployed.
- A real CalDAV server account (Sabre/Nextcloud) configured in `~/.config/retaskable/config.toml` on the device, with `[nextcloud].calendar` set to a calendar containing at least 1 existing task.
- USB-net or Wi-Fi between rMPP and host, controllable for offline simulation.
- `sqlite3` CLI available on the rMPP (or via shell).
- DB file location: `~/.local/share/retaskable/db.sqlite`.
- Pre-flight: `cd /home/jtd/reTaskable/app/backend && cargo test --bin retaskable-backend` passes (53/53).
- WARNING: This milestone DROPs existing tables on first launch (AC6.1). Migrate intentionally; do not bring forward production data you cannot recreate from the server.

## Phase 6 hardware landmark — sync drains queue

### 6.1 First-launch migration (AC6.1, AC6.2)

| Step | Action | Expected |
|------|--------|----------|
| 1 | Pre-state check: `sqlite3 ~/.local/share/retaskable/db.sqlite "SELECT schema_version FROM meta"` | Either `meta` table absent (v1 DB) or `schema_version < 2`. |
| 2 | Launch the M9a backend binary. | Backend starts without panic. |
| 3 | `sqlite3 ~/.local/share/retaskable/db.sqlite ".schema"` | v2 layout: `meta`, `calendar`, `task` (with `uid` and `pending_delete`), `pending_op`. `meta.schema_version = 2`. |
| 4 | `sqlite3 ... "SELECT COUNT(*) FROM task; SELECT COUNT(*) FROM calendar; SELECT sync_token FROM calendar"` | task/calendar counts may be 0; any retained calendar row has `sync_token = NULL` (forces full sync-collection). |
| 5 | Tap Sync. | Response begins with `Sync complete: ...` (no `Flushed` prefix because pending_op is empty); tasks repopulate from server. |

### 6.2 Sync with empty queue (AC1.1)

| Step | Action | Expected |
|------|--------|----------|
| 1 | Confirm queue empty: `sqlite3 ... "SELECT COUNT(*) FROM pending_op"` returns 0. | 0 rows. |
| 2 | Tap Sync. | Response area shows exactly `Sync complete: <calendar> ...` (legacy format, no `Flushed` line). |

### 6.3 Offline queue then online flush (AC1.2-HW, AC1.3, AC2.5-HW)

| Step | Action | Expected |
|------|--------|----------|
| 1 | Disconnect network. | Backend cannot reach CalDAV. |
| 2 | Tap Toggle First. | Response renders `Queued: toggle "<summary>" -> <new-state> (#1)` within visibly snappy time (no perceptible lag — should feel instant). |
| 3 | Tap Edit First, type a new summary, submit. | Response renders `Queued: edit "<old>" -> "<new>" (#2)` quickly. |
| 4 | Tap Create, type a new task summary, submit. | Response renders `Queued: create "<summary>" (#3)` quickly. |
| 5 | Tap Show Tasks. | Optimistic state visible: toggled task shows new status, edited task shows new summary, created task appears (with sentinel href `pending:*`). |
| 6 | Reconnect the network. | Connectivity restored. |
| 7 | Tap Sync. | Response area renders `Flushed 3 / 0 errored (0 retried).\nSync complete: <calendar> ...`. |
| 8 | Tap Show Pending. | Renders `No pending ops.` |
| 9 | `sqlite3 ... "SELECT COUNT(*) FROM pending_op"` | 0 rows. |
| 10 | Verify on CalDAV server (web UI or other client). | All three changes (toggle, edit, create) present on server. |

### 6.4 Transient break path (AC1.4)

| Step | Action | Expected |
|------|--------|----------|
| 1 | Disconnect network. | Offline. |
| 2 | Tap Toggle First. | `Queued: toggle ... (#N)`. |
| 3 | Tap Sync while still offline (or with CalDAV port blocked). | Flush attempt yields a transient failure. |
| 4 | Verify response. | Response renders `network failed; sync-collection skipped`, no `Flushed` claiming success. |
| 5 | `sqlite3 ... "SELECT id, error_count, errored FROM pending_op"` | One row, `error_count = 1`, `errored = 0`. |
| 6 | Restore connectivity, tap Sync again. | `Flushed 1 / 0 errored (...)` — op drains. |

### 6.5 Crash recovery (AC5.1-HW, AC5.2, AC5.3)

| Step | Action | Expected |
|------|--------|----------|
| 1 | Disconnect network. Tap Toggle First. | `Queued: toggle ... (#N)`. |
| 2 | `kill -9 $(pidof retaskable-backend)` (or force-kill via rMPP UI). | Process exits. |
| 3 | `sqlite3 ... "SELECT id, op_type, target_uid, errored, error_count FROM pending_op"` | Row still present, `errored=0`, `error_count=0` — SQLite has fsynced (AC5.3). |
| 4 | Inspect consistency: no half-applied cache write. | `errored=0`, `error_count=0` (AC5.1-HW). |
| 5 | Restart backend, reconnect, tap Sync. | The surviving op drains: response includes `Flushed 1 / 0 errored ...` (AC5.2). |

## Phase 7 hardware landmark — Show Pending + Clear Errored

### 7.1 Show Pending with empty queue (AC4.3-HW)

| Step | Action | Expected |
|------|--------|----------|
| 1 | Ensure queue empty. | 0 rows. |
| 2 | Tap Show Pending. | Response area: `No pending ops.` |

### 7.2 Show Pending with mixed ops (AC4.3-HW)

| Step | Action | Expected |
|------|--------|----------|
| 1 | Disconnect network. Queue 2 ops (Toggle First then Edit First). | Two `Queued: ...` responses. |
| 2 | Induce a terminal error on one op: e.g. delete one task on the server then tap Sync so the queued op gets 404. | One op promotes `errored=1` with `last_error` set. |
| 3 | Tap Show Pending. | Lines like:<br>`#1  toggle  "<summary>"  <Ns ago>`<br>`#2  edit    "<summary>"  <Ns ago>  [errored 1x: <err>]`<br>id-ascending; error annotation only on the errored row. |
| 4 | (Optional) Force-delete the cache row via sqlite to test the LEFT JOIN. | Show Pending displays `"(unknown)"` in the summary slot. |

### 7.3 Clear Errored double-tap arming (AC4.4-HW)

| Step | Action | Expected |
|------|--------|----------|
| 1 | With at least one errored op in the queue. | Setup. |
| 2 | Tap Clear Errored once. | Button switches to inverted state with label `Tap again to confirm`. No DB mutation (`sqlite3 ... "SELECT COUNT(*) FROM pending_op WHERE errored=1"` unchanged). |
| 3 | Wait > 3 seconds without tapping. | Button reverts to normal state. |
| 4 | Tap Clear Errored, then within 3s tap again. | Response area: `Cleared N errored op(s). Tap Sync to reconcile.` |
| 5 | `sqlite3 ... "SELECT COUNT(*) FROM pending_op WHERE errored=1; SELECT COUNT(*) FROM task WHERE pending_delete=1; SELECT COUNT(*) FROM task WHERE href LIKE 'pending:%'"` | All three counts: 0. Non-errored pending ops untouched. |

### 7.4 Clear Errored → Sync reconciliation (AC4.5)

| Step | Action | Expected |
|------|--------|----------|
| 1 | On the server, modify a task. Offline on device, Delete First the same task → `Queued: delete ... (#N)`. Reconnect after a server-side delete (or otherwise force a 404). Tap Sync → op errors. | One errored Delete op on a task whose server state diverges. |
| 2 | Double-tap Clear Errored. | `Cleared 1 errored op(s). Tap Sync to reconcile.` |
| 3 | Tap Show Tasks. | Locally-tombstoned row may or may not be visible (pending_delete reset). |
| 4 | Tap Sync. | Response: `Sync complete: ...`. |
| 5 | Tap Show Tasks. | Tasks reflect the SERVER's authoritative state. |

## End-to-end scenarios

### E2E-1: Cold start → migrate → first write → sync (AC6.1, AC6.2, AC4.1, AC1.2-HW, AC1.3)

1. Wipe `~/.local/share/retaskable/db.sqlite` (or start with a v1 DB).
2. Launch backend. Verify v2 schema via `sqlite3`.
3. Tap Sync — verifies sync_token=NULL → full sync-collection (AC6.2). Tasks repopulate.
4. Online: Tap Toggle First → `Queued: toggle ... (#1)`.
5. Tap Show Tasks. Verify toggled status appears optimistically (AC4.1).
6. Tap Sync → `Flushed 1 / 0 errored ...\nSync complete: ...`.
7. Confirm change landed on server.

### E2E-2: Offline burst → reconnect → drain in id order (AC1.2-HW, AC4.1, AC4.2)

1. Disconnect network.
2. Toggle First → Edit First → Create "Buy milk" → Delete First (the toggled+edited task). Confirm 4 `Queued: ...` responses with ids #1..#4.
3. Tap Show Tasks. Verify: original task GONE (tombstoned, AC4.2); "Buy milk" appears (AC4.1, sentinel href).
4. `sqlite3 ... "SELECT id, op_type FROM pending_op ORDER BY id"` shows 4 ops in order toggle/edit/create/delete.
5. Reconnect, tap Sync.
6. Expect: `Flushed 4 / 0 errored ...\nSync complete: ...`.
7. Verify on server: original task deleted; "Buy milk" exists; no extra artifacts.

### E2E-3: Terminal error → cascade → Clear Errored → reconcile (AC3.5, AC3.6, AC4.4-HW, AC4.5)

1. Online. Note an existing task's uid (`UID-X`).
2. From another client / server web UI, delete the task with uid `UID-X`.
3. On the device while offline: Toggle First on `UID-X`, then Edit First on `UID-X` (both target same uid). Two `Queued: ...` responses (#1, #2).
4. Reconnect, tap Sync.
5. Expect: `Flushed 0 / 2 errored ...\n<sync-collection result>`. 404 on #1 makes it terminal + cascades #2.
6. Tap Show Pending. Both rows annotated `[errored ...: ...]`.
7. Double-tap Clear Errored. Expect: `Cleared 2 errored op(s). Tap Sync to reconcile.`
8. Tap Sync. Expect: `Sync complete: ...`. Tasks reflect server state.

## Hardware-only AC summary

| Criterion | Why manual | Steps |
|-----------|------------|-------|
| AC1.2-HW | Real TLS/auth/ETag against real CalDAV server | 6.3 |
| AC2.5-HW | Wall-clock UI snappiness | 6.3 (observe response latency) |
| AC4.3-HW | QML render verification of Show Pending output | 7.2 |
| AC4.4-HW | QML Timer-driven double-tap-arm UX | 7.3 |
| AC4.5 | Real `sync_collection_with_fallback` round-trip post-Clear-Errored | 7.4, E2E-3 |
| AC5.1-HW | Actual process kill (in-memory DB cannot replicate) | 6.5 |
| AC5.2 | Crash-then-resume cycle | 6.5 |
| AC5.3 | SQLite disk durability across kill | 6.5 |

## Traceability

| AC | Automated | Manual |
|----|-----------|--------|
| AC1.1 | `compose_empty_queue_matches_legacy_format` | 6.2 |
| AC1.2 | `flush_pending_happy_path_toggle` + variants | 6.3 (HW) |
| AC1.3 | `compose_flush_then_sync_prepends_flushed_line` | 6.3 |
| AC1.4 | `compose_transient_break_appends_skipped_line` + `flush_pending_transient_5xx_bumps_error_count_and_breaks_drain` | 6.4 |
| AC1.5 | `fetch_next_drainable_returns_lowest_id_unerrored` | 6.4 |
| AC2.1 | `enqueue_create_atomically_inserts_task_and_pending_op` | — |
| AC2.2 | `enqueue_toggle_flips_status_and_inserts_op` | — |
| AC2.3 | `enqueue_edit_rewrites_summary_and_enqueues_op` + escaping | — |
| AC2.4 | `enqueue_delete_tombstones_row_and_hides_from_list` | — |
| AC2.5 | Static handler-shape inspection (main.rs) | 6.3 (HW timing) |
| AC2.6 | three `*_unknown_uid_*` tests | — |
| AC3.1 | `apply_outcome_success_update_with_retry_increments_retried` + partial-drain | — |
| AC3.2 | `apply_outcome_success_update_with_retry_increments_retried` | — |
| AC3.3 | `flush_pending_transient_5xx_bumps_error_count_and_breaks_drain` | — |
| AC3.4 | `flush_pending_fifth_transient_strike_promotes_and_cascades` | — |
| AC3.5 | four `classify_*` tests + `flush_pending_terminal_404_marks_errored_and_cascades` | E2E-3 |
| AC3.6 | `cascade_uid_poisons_only_unerrored_siblings_on_same_uid` | E2E-3 |
| AC4.1 | `enqueue_create_optimistically_visible_in_list_tasks` | E2E-2 |
| AC4.2 | `list_tasks_filters_tombstoned_rows` | E2E-2 |
| AC4.3 | `list_pending_ops_returns_id_ascending_with_summary` | 7.2 (HW) |
| AC4.4 | `clear_errored_drops_errored_ops_and_resets_cache_effects` + idempotency | 7.3 (HW) |
| AC4.5 | — (intentionally hardware-only) | 7.4, E2E-3 |
| AC5.1 | `flush_pending_partial_drain_consistent_on_simulated_kill` | 6.5 (HW) |
| AC5.2 | `fetch_next_drainable_returns_lowest_id_unerrored` (logical equiv) | 6.5 |
| AC5.3 | — (intentionally hardware-only) | 6.5 |
| AC6.1 | three `migration_*` tests | 6.1 |
| AC6.2 | `migration_from_v1_drops_existing_data` | 6.1 step 5 |
