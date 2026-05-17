# M9a Offline Queue — Test Requirements

Maps each acceptance criterion from the design plan to its verification mechanism. 26 ACs total: 21 fully automated, 2 hardware-only, 3 with both automated coverage and a hardware-confirmation pass.

## Automated Tests

### m9a-offline-queue.AC1.1
- **Type**: unit
- **Location**: `app/backend/src/main.rs` `#[cfg(test)] mod tests`
- **Test name**: `compose_empty_queue_matches_legacy_format`
- **Verifies**: With empty FlushSummary, `compose_sync_response` returns the legacy `Sync complete: ...` string byte-for-byte (no Flushed prefix, no skipped suffix).

### m9a-offline-queue.AC1.2
- **Type**: integration (also hardware-confirmed)
- **Location**: `app/backend/src/queue.rs` `#[cfg(test)] mod tests` (httpmock)
- **Test name**: `flush_pending_happy_path_toggle` (and the create/edit/delete equivalents added under Phase 4)
- **Verifies**: With seeded pending_op rows and an httpmock server returning 204, `flush_pending` drains in id-ascending order via the correct `nextcloud::*` helper per `op_type`. Hardware confirmation lives under Phase 6 Task 3 (see Human Verification AC1.2-HW).

### m9a-offline-queue.AC1.3
- **Type**: unit
- **Location**: `app/backend/src/main.rs` `#[cfg(test)] mod tests`
- **Test name**: `compose_flush_then_sync_prepends_flushed_line`
- **Verifies**: With non-empty FlushSummary, `compose_sync_response` prepends `Flushed N / M errored (K retried).\n` ahead of the sync-collection line.

### m9a-offline-queue.AC1.4
- **Type**: unit (composition) + integration (mid-drain break)
- **Location**: `app/backend/src/main.rs` `tests::compose_transient_break_appends_skipped_line`; `app/backend/src/queue.rs` `tests::flush_pending_transient_5xx_bumps_error_count_and_breaks_drain`
- **Verifies**: Composition emits `network failed; sync-collection skipped` when `sync_kind` is None; queue runner breaks the drain on first transient failure (`Ok(false)` return).

### m9a-offline-queue.AC1.5
- **Type**: integration
- **Location**: `app/backend/src/db.rs` `tests::fetch_next_drainable_returns_lowest_id_unerrored` + the Phase 4 transient test's residual state
- **Verifies**: After a mid-drain transient stop, `fetch_next_drainable` returns the next-lowest-id non-errored op (succeeded ops are already gone; in-progress op carries its `error_count` increment).

### m9a-offline-queue.AC2.1
- **Type**: unit
- **Location**: `app/backend/src/db.rs` `tests::enqueue_create_atomically_inserts_task_and_pending_op`
- **Verifies**: `enqueue_create` writes both task (sentinel href, empty etag, NEEDS-ACTION, uid, pending_delete=0) AND `pending_op` (op_type=create, target_uid, JSON payload) in one transaction.

### m9a-offline-queue.AC2.2
- **Type**: unit
- **Location**: `app/backend/src/db.rs` `tests::enqueue_toggle_flips_status_and_inserts_op`
- **Verifies**: `enqueue_toggle` rewrites cached ical_text via `toggle_completion`, flips denormalised status, AND inserts pending_op (op_type=toggle, payload NULL) atomically.

### m9a-offline-queue.AC2.3
- **Type**: unit
- **Location**: `app/backend/src/db.rs` `tests::enqueue_edit_rewrites_summary_and_enqueues_op` + `tests::enqueue_edit_escapes_special_characters_in_payload`
- **Verifies**: `enqueue_edit` rewrites ical_text via `replace_summary`, updates denormalised summary, AND inserts pending_op with `{"summary":"..."}` JSON payload (escaping verified separately).

### m9a-offline-queue.AC2.4
- **Type**: unit
- **Location**: `app/backend/src/db.rs` `tests::enqueue_delete_tombstones_row_and_hides_from_list`
- **Verifies**: `enqueue_delete` sets `pending_delete=1` AND inserts pending_op (op_type=delete, payload NULL) in one transaction; tombstoned row vanishes from `list_tasks`.

### m9a-offline-queue.AC2.5
- **Type**: unit (handler shape) — automated portion limited; full UI-snappiness check is human
- **Location**: `app/backend/src/main.rs` (handler helpers compile as non-async, calling `db::enqueue_*` only — no `reqwest::Client` in the helper bodies, statically verifiable by inspection)
- **Verifies**: Each handler returns `Queued: <op> "<summary>" (#<id>)` synchronously. The sub-100ms timing claim is observed in Phase 5's host emulator manual sanity, not automated (see AC2.5-HW).

### m9a-offline-queue.AC2.6
- **Type**: unit
- **Location**: `app/backend/src/db.rs` `tests::enqueue_toggle_unknown_uid_leaves_no_pending_op` + `tests::enqueue_edit_unknown_uid_rolls_back` + `tests::enqueue_delete_unknown_uid_rolls_back`
- **Verifies**: When the cache mutation step fails (uid not found), the pending_op insert is rolled back; `SELECT COUNT(*) FROM pending_op` is 0.

### m9a-offline-queue.AC3.1
- **Type**: unit + integration
- **Location**: `app/backend/src/queue.rs` `tests::apply_outcome_success_update_with_retry_increments_retried`; supplemented by `tests::flush_pending_partial_drain_consistent_on_simulated_kill`
- **Verifies**: A successful op's cache write and pending_op DELETE share a single `unchecked_transaction()`; post-flush the cache reflects the new etag/ical and pending_op is gone.

### m9a-offline-queue.AC3.2
- **Type**: unit
- **Location**: `app/backend/src/queue.rs` `tests::apply_outcome_success_update_with_retry_increments_retried`
- **Verifies**: `ExecOutcome::SuccessUpdate { retried: true }` increments `FlushSummary.retried`.

### m9a-offline-queue.AC3.3
- **Type**: integration (httpmock)
- **Location**: `app/backend/src/queue.rs` `tests::flush_pending_transient_5xx_bumps_error_count_and_breaks_drain`
- **Verifies**: On 503 response, `pending_op.error_count` increments to 1, `last_error` is set, `errored` stays 0.

### m9a-offline-queue.AC3.4
- **Type**: integration (httpmock)
- **Location**: `app/backend/src/queue.rs` `tests::flush_pending_fifth_transient_strike_promotes_and_cascades`
- **Verifies**: Seeded op with `error_count=4` plus one more 503 promotes `errored=1` AND cascades sibling ops on the same target_uid.

### m9a-offline-queue.AC3.5
- **Type**: unit (taxonomy) + integration (httpmock)
- **Location**: `app/backend/src/queue.rs` `tests::classify_404_as_terminal`, `tests::classify_401_as_terminal`, `tests::classify_double_412_as_terminal`, `tests::classify_uid_collision_as_terminal`; `tests::flush_pending_terminal_404_marks_errored_and_cascades`
- **Verifies**: 4xx/404/double-412/UID-collision flips `errored=1` immediately with no error_count threshold; cascade fires.

### m9a-offline-queue.AC3.6
- **Type**: unit
- **Location**: `app/backend/src/db.rs` `tests::cascade_uid_poisons_only_unerrored_siblings_on_same_uid`
- **Verifies**: `cascade_uid` UPDATE affects only rows where `target_uid` matches AND `errored=0`; already-errored siblings keep their original last_error; other UIDs are untouched.

### m9a-offline-queue.AC4.1
- **Type**: unit
- **Location**: `app/backend/src/db.rs` `tests::enqueue_create_optimistically_visible_in_list_tasks` (plus the cache-mutation assertions inside each `enqueue_*` test)
- **Verifies**: After enqueue, `list_tasks` reflects the optimistic state (toggled status, new summary, locally-created row) before any flush.

### m9a-offline-queue.AC4.2
- **Type**: unit
- **Location**: `app/backend/src/db.rs` `tests::list_tasks_filters_tombstoned_rows`
- **Verifies**: Rows with `pending_delete=1` are excluded from `list_tasks` and `get_first_task`.

### m9a-offline-queue.AC4.3
- **Type**: unit
- **Location**: `app/backend/src/db.rs` `tests::list_pending_ops_returns_id_ascending_with_summary` (DB layer) — the `show_pending` formatter is exercised by hardware verification in Phase 7 Task 4 (see AC4.3-HW).
- **Verifies**: `list_pending_ops` returns rows in id-ascending order with `COALESCE`'d summary; errored rows expose `last_error`; missing cache rows show `"(unknown)"`.

### m9a-offline-queue.AC4.4
- **Type**: unit
- **Location**: `app/backend/src/db.rs` `tests::clear_errored_drops_errored_ops_and_resets_cache_effects` + `tests::clear_errored_is_idempotent_on_empty`
- **Verifies**: `db::clear_errored` DELETEs errored pending_op rows, unsets pending_delete for errored Deletes, drops `href LIKE 'pending:%'` rows for errored Creates, leaves non-errored ops untouched, all in one transaction.

### m9a-offline-queue.AC6.1
- **Type**: unit
- **Location**: `app/backend/src/db.rs` `tests::migration_from_empty_db_creates_v2_layout` + `tests::migration_from_v1_drops_existing_data` + `tests::migration_is_idempotent`
- **Verifies**: Opening a DB with missing `meta` or `schema_version<2` DROPs all tables and recreates v2 (verified by inspecting PRAGMA table_info and seeded-data wipe).

### m9a-offline-queue.AC6.2
- **Type**: unit
- **Location**: `app/backend/src/db.rs` `tests::migration_from_v1_drops_existing_data` (asserts the post-migration sync_token is NULL)
- **Verifies**: After migration the calendar.sync_token is absent, so the next Sync call (driven by existing `get_sync_token` returning `Ok(None)`) takes the full sync-collection path. End-to-end Sync confirmation lives in Phase 6 hardware verification.

## Human Verification

### m9a-offline-queue.AC1.2-HW (hardware confirmation of automated AC1.2)
- **Justification**: The automated httpmock tests verify drain order and per-op-type dispatch; hardware confirmation proves the same against a real CalDAV server (Sabre / Nextcloud) over real TLS on RMPPM, where URL construction, auth, and ETag round-trips can surface server-specific quirks.
- **Approach**: Phase 6 Task 3 step 3. Disconnect USB-net on RMPPM; queue 3 mixed ops (Toggle, Edit, Create); reconnect; tap Sync; verify response `Flushed 3 / 0 errored (0 retried).\nSync complete: ...` and Show Pending reports empty.

### m9a-offline-queue.AC2.5-HW (UI snappiness)
- **Justification**: The automated check confirms the handlers are sync and don't reference `reqwest`; sub-100ms wall-clock latency is observable but isn't a unit-test property — it depends on SQLite disk latency on the real device.
- **Approach**: Phase 5 manual sanity. On host emulator (and again on RMPPM during Phase 6 Task 3), tap each of Create / Toggle First / Edit First / Delete First and confirm the `Queued: ... (#<n>)` response renders within UI-snappiness time (no visible lag).

### m9a-offline-queue.AC4.3-HW (Show Pending end-to-end)
- **Justification**: The DB layer is unit-tested; the format string built by `show_pending` and rendered by QML can only be verified by eye on the RMPPM response area.
- **Approach**: Phase 7 Task 4 step 4. Tap Show Pending after inducing an errored op; verify the response area shows `#<id>  <op>  "<summary>"  <age>  [errored Nx: <err>]` and that `No pending ops.` renders for an empty queue.

### m9a-offline-queue.AC4.4-HW (double-tap-arm timing)
- **Justification**: The QML `Timer { interval: 3000 }` arm/disarm flow is a UI affordance; no automated test exercises it.
- **Approach**: Phase 7 Task 4 steps 5–6. Tap Clear Errored once → button turns black with "Tap again to confirm" → wait 4s → label resets. Then double-tap within 3s → backend handler fires, response shows `Cleared N errored op(s). Tap Sync to reconcile.`

### m9a-offline-queue.AC4.5
- **Justification**: Requires a real CalDAV server to return authoritative state via sync-collection after Clear Errored. The tombstone-reset is unit-tested in AC4.4, but the "next Sync reconciles to server state" half is an integration property of `sync_collection_with_fallback` + `db::upsert_task` over real HTTP.
- **Approach**: Phase 7 Task 4 steps 7–8. After Clear Errored on an errored Delete, tap Sync; verify Show Tasks now reflects the SERVER state (summary, status, due, etag).

### m9a-offline-queue.AC5.1-HW (hardware confirmation of automated AC5.1)
- **Justification**: The automated test verifies the structural invariant (no half-applied state after `flush_pending` returns). A real process-kill mid-drain can only be exercised on hardware because in-memory SQLite does not survive Drop.
- **Approach**: Phase 6 Task 3 step 5. Queue 1 op; `kill -9` the backend process while tapping Sync (or force-kill via RMPPM UI); re-launch; inspect `pending_op` via `sqlite3` and confirm consistent state.

### m9a-offline-queue.AC5.2
- **Justification**: Logically equivalent to AC1.5 (next Sync resumes at lowest non-errored id) but framed as crash recovery. The resume behaviour is automatic from `fetch_next_drainable`; the kill-then-resume cycle requires real process termination.
- **Approach**: Phase 6 Task 3 step 5. After the kill, re-launch backend, tap Sync, verify the surviving op drains.

### m9a-offline-queue.AC5.3
- **Justification**: SQLite durability across process restart cannot be exercised by in-memory test DBs.
- **Approach**: Phase 6 Task 3 step 5. Queue 1 op (no disconnect needed); kill the backend; re-launch; verify the op is still in `pending_op` via `sqlite3 ~/.local/share/retaskable/db.sqlite "SELECT * FROM pending_op"`; tap Sync and confirm it drains.
