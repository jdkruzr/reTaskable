# M10: ListView UI — Design

## Summary

M10 replaces the M0–M9c button-Flow + single response `Text` scaffolding with reTaskable's first real end-user task list. A QML `ListView` backed by a `ListModel` renders every task as a row with a checkbox, summary, optional due date, and a pending-op mark. Tapping a checkbox toggles the task by UID without redrawing the full list — critical for e-ink: a full-list redraw on every toggle produces perceptible ghosting. A "Show Completed" toggle hides finished tasks by default, solving the immediate hardware-surfaced problem (a 225-row list with 224 completed tasks was unnavigable).

The M0–M9c response-area `Text` node is retained as a status line (sync result, error messages, last-synced time), but it no longer displays the task list.

## Key Decisions

### JSON wire format for MSG 4 / 104

`show_tasks` now calls `nextcloud::format_tasks_json` instead of `format_tasks_marked`. The response is a JSON object:

```json
{
  "last_synced": "Last synced 12 minutes ago.",
  "tasks": [
    {"uid":"…","summary":"Buy milk","completed":false,"due":"2026-06-01","mark":""}
  ]
}
```

`mark` is `"!"` (errored, needs resolution), `"*"` (queued, will flush on next Sync), or `""` (clean). The field is always present so the QML delegate can key off it without an existence check. `completed` is a bool. `last_synced` is a string or `null`. The sort order (incomplete first, undated last, then ascending due date) matches the pre-M10 `format_tasks_marked` ordering.

Non-JSON responses (e.g. `"calendar ... not yet synced"`, error strings) still occur and QML must tolerate them: `applyTaskList` wraps the `JSON.parse` in a try/catch, clears the model on failure, and shows the raw string in the status line. This was a hardware-surfaced bug in early M10: a refresh-on-Settings-save returned a non-JSON string and the stale list from the previous calendar stayed on screen.

### MSG 4 payload: `"all"` vs `"open"`

`MSG_SHOW_TASKS` inspects `msg.contents.trim() == "all"`. The QML `refreshList()` function sends `"all"` when `root.showCompleted` is true, `"open"` otherwise. `nextcloud::filter_for_display(tasks, include_completed)` drops `Completed` and `Cancelled` status rows when `include_completed` is false. This was the primary UX fix: on hardware, a user's 225-row Nextcloud list (224 completed items) rendered as a single scrollable wall with no active tasks visible.

### MSG 14 / 114 `toggle_by_uid` — per-row checkbox

Rather than reusing `MSG_TOGGLE_FIRST` (positional, operates on the first non-completed task), M10 adds `MSG_TOGGLE_BY_UID = 14` / `MSG_TOGGLE_BY_UID_RESPONSE = 114`. The QML delegate's `CheckBox` fires `onToggled` (user interaction only, not binding re-evaluation) and calls `endpoint.sendMessage(14, model.uid)`.

The handler `toggle_by_uid` is a thin config/calendar-resolution wrapper around `toggle_by_uid_inner(db, cal_href, uid)`, which:
1. Calls `db::get_cached_task_by_uid` to look up the task.
2. Calls `db::enqueue_toggle` (flips cached `status` + inserts `pending_op` atomically).
3. Returns compact JSON: `{"uid":…,"completed":bool,"mark":"*"}`.

The mark is always `"*"` (freshly queued, not errored) at enqueue time. QML's `applyToggleResult` iterates `taskModel` to find the matching `uid` and calls `taskModel.set(i, {completed:…, mark:…})` — a single-row mutation that avoids a full-list redraw. `format_tasks_json` and the full `refreshList()` path are unaffected; they remain available for post-Sync redraw.

### e-ink scrolling: `interactive: false` + discrete paging

Kinetic (flick) scrolling spans many slow e-ink refresh cycles, producing lag and ghosting. The `ListView` uses `interactive: false` to disable flick; the header row has ▲ and ▼ buttons that call `pageUp()` / `pageDown()`, each jumping exactly one `taskList.height` with no animation. `boundsBehavior: Flickable.StopAtBounds` prevents over-scrolling. Animation transitions (`add`, `remove`, `displaced`) are zeroed out (`Transition {}`).

Research confirmed the app has no control over e-ink waveform modes from inside an AppLoad QML frontend: xochitl owns the display refresh path. Only a windowed/qtfb native app could request `GC16` or `A2` waveforms directly. That avenue is noted for M13+ but was out of scope here.

Two properties that were tried and removed during development:
- `flickDeceleration: 0` — invalid (no effect on a non-interactive ListView, caused a QML warning).
- `cacheBuffer: 0` — caused delegate churn on every page operation; removed after hardware testing showed it made paging noticeably slower.

### `format_tasks_marked` retained under `#[cfg_attr(not(test), allow(dead_code))]`

`format_tasks_marked` in `nextcloud.rs` remains because the M9c test suite exercises it directly. The attribute suppresses the dead-code warning in device builds while keeping the tests green. No API change; the function is unchanged.

## Acceptance Criteria

1. `MSG_SHOW_TASKS` with payload `"open"` returns a JSON envelope; `applyTaskList` populates `taskModel` with only non-Completed/non-Cancelled rows.
2. `MSG_SHOW_TASKS` with payload `"all"` includes Completed and Cancelled rows.
3. A non-JSON response clears `taskModel` and shows the raw string in `statusText`.
4. `MSG_TOGGLE_BY_UID` enqueues the toggle and returns `{"uid":…,"completed":bool,"mark":"*"}`; `applyToggleResult` mutates only the matching row.
5. ▲/▼ buttons page the list without animation; `interactive: false` prevents flick.
6. `format_tasks_json` tests pass (4 new tests in `nextcloud.rs`); `filter_for_display` tests pass (2 new tests); `toggle_by_uid_inner` tests pass (2 new tests in `main.rs`).
7. Hardware-verified on rMPP: 225-task list loads, "Show Completed" toggle hides/shows completed rows, checkbox tap enqueues toggle and flips the row without ghosting.

## Verification

Hardware issues found and fixed during this milestone:

- **Stale list on non-JSON response.** After the Settings save triggered an immediate Sync with a different calendar, `applyTaskList` received `"calendar ... not yet synced"` before the new calendar's first Sync ran. The old list stayed on screen. Fixed: `applyTaskList` calls `taskModel.clear()` in the catch branch.

- **225-row wall.** The user's real Nextcloud calendar had 224 completed tasks and 1 open one. Pre-M10, Show Tasks rendered all 225 lines in the response `Text`, unscrollable. Post-M10, `filter_for_display` with `include_completed=false` shows the 1 open task; tapping "Show Completed" expands to all 225 with the ▲/▼ paging.

Automated test count: 94 (M9c) → 110 (M10). 16 new tests: `format_tasks_json` (4), `filter_for_display` (2), `toggle_by_uid_inner` (2), `sync_collection_unsupported` (1), `parse_sync_response` for calendar-query (1), `set_last_synced` (1), `reset_cache` (1), config round-trip (4).
