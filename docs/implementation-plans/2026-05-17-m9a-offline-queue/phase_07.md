# M9a Offline Queue — Phase 7: Show Pending + Clear Errored UI

**Goal:** Surface the queue to the user with a Show Pending button (dumps the pending_op table to the response area, annotated with age and error reasons) and a Clear Errored button (double-tap-armed, drops all errored ops and resets their cache effects so the next Sync reconciles). Second M9a hardware landmark.

**Architecture:** Two new MSG types: `MSG_SHOW_PENDING = 10` / `MSG_SHOW_PENDING_RESPONSE = 110` and `MSG_CLEAR_ERRORED = 11` / `MSG_CLEAR_ERRORED_RESPONSE = 111`. The backend handlers query `pending_op` (joined with `task` for summary text) and respond. Clear Errored runs three operations in one transaction: unset `pending_delete` for errored Deletes, drop locally-created task rows (`href LIKE 'pending:%'`) for errored Creates, and DELETE the errored pending_op rows. QML adds two buttons to the existing Flow, mimicking the Delete First double-tap-arm Timer pattern verbatim for Clear Errored.

**Tech Stack:** QML (Qt 5/6), Rust 2021. No new dependencies.

**Scope:** Phase 7 of 8. Depends on Phase 6 (Sync wires the queue) for hardware verification of the full clear→reconcile cycle.

**Codebase verified:** 2026-05-17. Investigator confirmed: QML lives at `app/ui/Main.qml` (Title case), uses `endpoint.sendMessage(type, payload)` to talk to backend; `onMessageReceived: (type, contents) => { ... }` currently filters types 101–109 and writes to `responseText.text`; Delete First button uses `armed: bool` property + 3000ms `Timer { onTriggered: armed = false }`; existing MSG constants 1–9 + 101–109 leave 10–11 / 110–111 free; `humanize_since(SystemTime)` exists at `main.rs:431` and accepts a SystemTime (we'll convert from `enqueued_at` Unix seconds at the call site).

---

## Acceptance Criteria Coverage

This phase implements and tests:

### m9a-offline-queue.AC3: Error classification and queue health
- **m9a-offline-queue.AC3.6 Failure (cascade):** A cascade UPDATE poisons only rows with the SAME `target_uid` AND only rows where `errored=0` (does not re-poison already-errored siblings). *(Re-verified at the queue-inspection level — Show Pending displays the cascade-poisoned message.)*

### m9a-offline-queue.AC4: Optimistic display, tombstoning, queue inspection UI
- **m9a-offline-queue.AC4.3 Success:** Show Pending dumps each pending_op as `#<id>  <op>  "<summary>"  <age>  [errored Nx: <err>]`; empty queue dumps `No pending ops.`.
- **m9a-offline-queue.AC4.4 Success:** Clear Errored (double-tap-armed via 3s `Timer`) DELETEs all `pending_op WHERE errored=1`, unsets `pending_delete` on affected task rows (errored Deletes), AND drops locally-created task rows (`href LIKE 'pending:%'`) corresponding to errored Creates.
- **m9a-offline-queue.AC4.5 Edge:** A tombstoned task reconciled by sync-collection (after Clear Errored has been tapped) has its server-state summary / status / due / ical_text / etag visible in Show Tasks on the next Sync.

---

## Notes for the implementing engineer

- The Show Pending output line format is `#<id>  <op>  "<summary>"  <age>  [errored Nx: <err>]`. Two spaces between fields is intentional (matches the existing `format_tasks` style in `nextcloud.rs`); the `[errored Nx: <err>]` segment is present only when `errored=1` OR `error_count>0`.
- A pending_op row may not have a matching `task.summary` if the cache row was already removed (e.g., a deleted task that the sync-collection cleaned out before the Delete op flushed). Use `COALESCE(task.summary, '(unknown)')` in the JOIN.
- Clear Errored's "Tap again to clear N op(s)" label requires the QML side to know the errored count. Two options:
  1. The Show Pending response includes a parseable trailing line like `# errored: 3` that QML scrapes (gross).
  2. The Clear Errored button keeps its label generic ("Clear Errored" / "Tap again to confirm") and lets the response area carry the result count after the action.
  Use option 2 — simpler and matches the existing Delete First label style.
- The `humanize_since` helper takes a `SystemTime`. The pending_op's `enqueued_at` is `i64` Unix seconds; convert with `SystemTime::UNIX_EPOCH + Duration::from_secs(enqueued_at as u64)`.
- AC4.5 is automatically true once Phase 6 is shipped: clearing an errored Delete unsets `pending_delete`, the row becomes visible to `get_first_task` / `list_tasks` again (now showing the stale optimistic state); the next Sync's sync-collection REPORT pulls fresh server state and `upsert_task` overwrites etag/ical_text/summary/status. No code change is needed here; the integration test below verifies it end-to-end on hardware.

---

<!-- START_SUBCOMPONENT_A (tasks 1-2) -->

<!-- START_TASK_1 -->
### Task 1: `db::list_pending_ops` and `db::clear_errored`

**Verifies:** AC4.3 (foundation), AC4.4 (clear path), AC3.6 (cascade message visible).

**Files:**
- Modify: `app/backend/src/db.rs` — add two new public functions.
- Modify: `app/backend/src/db.rs` test module — add tests.

**Implementation:**

Add a small struct for the JOIN result:

```rust
#[derive(Debug)]
pub struct PendingOpView {
    pub id: i64,
    pub op_type: String,
    pub summary: String,           // COALESCE'd; "(unknown)" if no cache row
    pub enqueued_at: i64,
    pub error_count: i64,
    pub last_error: Option<String>,
    pub errored: i64,
}

/// Fetch all pending_op rows (errored and not) for display in the Show
/// Pending response. Joined against task by target_uid for the summary
/// column.
pub fn list_pending_ops(conn: &Connection) -> Result<Vec<PendingOpView>> {
    let mut stmt = conn.prepare(
        "SELECT p.id, p.op_type, COALESCE(t.summary, '(unknown)'),
                p.enqueued_at, p.error_count, p.last_error, p.errored
           FROM pending_op p
           LEFT JOIN task t
             ON t.calendar_href = p.target_calendar_href
            AND t.uid = p.target_uid
          ORDER BY p.id ASC",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(PendingOpView {
            id: r.get(0)?,
            op_type: r.get(1)?,
            summary: r.get(2)?,
            enqueued_at: r.get(3)?,
            error_count: r.get(4)?,
            last_error: r.get(5)?,
            errored: r.get(6)?,
        })
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>().map_err(|e| e.into())
}

/// Clear all errored pending_op rows and reset their cache-side effects:
/// - For errored Deletes, unset `pending_delete = 0` on the corresponding
///   task rows (so the row becomes live again and sync-collection can
///   reconcile it).
/// - For errored Creates, drop the locally-created task rows
///   (`href LIKE 'pending:%'`) that never reached the server.
/// - DELETE all `pending_op WHERE errored = 1`.
///
/// Returns the number of pending_op rows cleared.
pub fn clear_errored(conn: &mut Connection) -> Result<usize> {
    let tx = conn.unchecked_transaction()?;

    // Collect target_uids of errored deletes and creates.
    let mut delete_uids: Vec<String> = Vec::new();
    let mut create_uids: Vec<String> = Vec::new();
    {
        let mut stmt = tx.prepare(
            "SELECT target_uid, op_type FROM pending_op \
             WHERE errored = 1 AND op_type IN ('delete', 'create')",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (uid, op_type) = row?;
            match op_type.as_str() {
                "delete" => delete_uids.push(uid),
                "create" => create_uids.push(uid),
                _ => {}
            }
        }
    }

    for uid in &delete_uids {
        tx.execute(
            "UPDATE task SET pending_delete = 0 WHERE uid = ?1",
            params![uid],
        )?;
    }
    for uid in &create_uids {
        tx.execute(
            "DELETE FROM task WHERE uid = ?1 AND href LIKE 'pending:%'",
            params![uid],
        )?;
    }
    let cleared = tx.execute("DELETE FROM pending_op WHERE errored = 1", [])?;
    tx.commit()?;
    Ok(cleared)
}
```

**Why `&mut Connection` on `clear_errored`:** `unchecked_transaction()` requires it. Consistent with the enqueue helpers.

**Why `LEFT JOIN`:** A pending_op may reference a `target_uid` whose task row was wiped by a prior errored op's clear, or by a server-side delete that synced through. The LEFT JOIN tolerates this; `COALESCE` substitutes a placeholder.

Tests:

```rust
#[test]
fn list_pending_ops_returns_id_ascending_with_summary() {
    let mut conn = fresh();
    ensure_schema_v2(&conn).expect("migrate");
    conn.execute(
        "INSERT INTO calendar (href, display_name) VALUES ('/cal/', 'Cal')",
        [],
    ).unwrap();
    conn.execute(
        "INSERT INTO task (calendar_href, href, etag, ical_text, summary, status, due, uid, pending_delete)
         VALUES ('/cal/', '/cal/a.ics', '', '', 'Apple', 'needs-action', NULL, 'uid-a', 0),
                ('/cal/', '/cal/b.ics', '', '', 'Banana', 'needs-action', NULL, 'uid-b', 0)",
        [],
    ).unwrap();
    conn.execute(
        "INSERT INTO pending_op (op_type, target_uid, target_calendar_href, payload, enqueued_at, errored, error_count, last_error)
         VALUES ('toggle', 'uid-a', '/cal/', NULL, 100, 0, 0, NULL),
                ('edit', 'uid-b', '/cal/', '{\"summary\":\"newb\"}', 101, 1, 1, '404 not found'),
                ('toggle', 'uid-missing', '/cal/', NULL, 102, 0, 0, NULL)",
        [],
    ).unwrap();

    let ops = list_pending_ops(&conn).unwrap();
    assert_eq!(ops.len(), 3);
    assert_eq!(ops[0].summary, "Apple");
    assert_eq!(ops[1].summary, "Banana");
    assert_eq!(ops[1].errored, 1);
    assert_eq!(ops[1].last_error.as_deref(), Some("404 not found"));
    assert_eq!(ops[2].summary, "(unknown)", "LEFT JOIN fills (unknown)");
}

#[test]
fn clear_errored_drops_errored_ops_and_resets_cache_effects() {
    let mut conn = fresh();
    ensure_schema_v2(&conn).expect("migrate");
    conn.execute(
        "INSERT INTO calendar (href, display_name) VALUES ('/cal/', 'Cal')",
        [],
    ).unwrap();
    // Two cache rows: one tombstoned (errored delete will reset it), one
    // locally-created (errored create will drop it).
    conn.execute(
        "INSERT INTO task (calendar_href, href, etag, ical_text, summary, status, due, uid, pending_delete)
         VALUES ('/cal/', '/cal/keep.ics', 'e', '', 'Keep me', 'needs-action', NULL, 'uid-keep', 1),
                ('/cal/', 'pending:uid-new', '', '', 'Locally made', 'needs-action', NULL, 'uid-new', 0)",
        [],
    ).unwrap();
    conn.execute(
        "INSERT INTO pending_op (op_type, target_uid, target_calendar_href, payload, enqueued_at, errored, last_error)
         VALUES ('delete', 'uid-keep', '/cal/', NULL, 0, 1, '404'),
                ('create', 'uid-new', '/cal/', '{\"summary\":\"Locally made\"}', 1, 1, 'auth'),
                ('toggle', 'uid-other', '/cal/', NULL, 2, 0, NULL)",
        [],
    ).unwrap();

    let cleared = clear_errored(&mut conn).unwrap();
    assert_eq!(cleared, 2);

    // Tombstoned Keep is restored (pending_delete = 0).
    let pd: i64 = conn.query_row(
        "SELECT pending_delete FROM task WHERE uid = 'uid-keep'", [], |r| r.get(0),
    ).unwrap();
    assert_eq!(pd, 0);

    // Locally-created row dropped.
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM task WHERE uid = 'uid-new'", [], |r| r.get(0),
    ).unwrap();
    assert_eq!(count, 0);

    // Only the non-errored toggle remains in pending_op.
    let remaining: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pending_op", [], |r| r.get(0),
    ).unwrap();
    assert_eq!(remaining, 1);
    let kind: String = conn.query_row(
        "SELECT op_type FROM pending_op", [], |r| r.get(0),
    ).unwrap();
    assert_eq!(kind, "toggle");
}

#[test]
fn clear_errored_is_idempotent_on_empty() {
    let mut conn = fresh();
    ensure_schema_v2(&conn).expect("migrate");
    let cleared = clear_errored(&mut conn).unwrap();
    assert_eq!(cleared, 0);
}
```

**Verification:**

```bash
cd /home/jtd/reTaskable/app/backend
cargo test --lib db::tests::list_pending_ops
cargo test --lib db::tests::clear_errored
cargo build --release
```

**Commit:**

```bash
cd /home/jtd/reTaskable
git add app/backend/src/db.rs
git commit -m "M9a Phase 7: db helpers for Show Pending + Clear Errored"
```
<!-- END_TASK_1 -->

<!-- START_TASK_2 -->
### Task 2: `MSG_SHOW_PENDING` and `MSG_CLEAR_ERRORED` handlers

**Verifies:** AC4.3 (Show Pending format), AC4.4 (Clear Errored side-effects).

**Files:**
- Modify: `app/backend/src/main.rs:13–30` — add four new MSG constants.
- Modify: `app/backend/src/main.rs:44–127` — add two match arms.
- Modify: `app/backend/src/main.rs` — add two helper functions (`show_pending`, `clear_errored`).

**Implementation:**

Append to the MSG constants block:

```rust
const MSG_SHOW_PENDING: u32 = 10;
const MSG_CLEAR_ERRORED: u32 = 11;
const MSG_SHOW_PENDING_RESPONSE: u32 = 110;
const MSG_CLEAR_ERRORED_RESPONSE: u32 = 111;
```

Add match arms in `handle_message`, just before the catch-all `t => ...`:

```rust
MSG_SHOW_PENDING => {
    eprintln!("retaskable: show pending requested");
    let response = match show_pending(&mut self.db) {
        Ok(s) => s,
        Err(e) => format!("error: {e:#}"),
    };
    eprintln!("retaskable: show pending result:\n{response}");
    send(replier, MSG_SHOW_PENDING_RESPONSE, &response);
}
MSG_CLEAR_ERRORED => {
    eprintln!("retaskable: clear errored requested");
    let response = match clear_errored(&mut self.db) {
        Ok(s) => s,
        Err(e) => format!("error: {e:#}"),
    };
    eprintln!("retaskable: clear errored result:\n{response}");
    send(replier, MSG_CLEAR_ERRORED_RESPONSE, &response);
}
```

Add the helper functions (placement: near `show_tasks` so the readers see them together):

```rust
fn show_pending(db: &mut Connection) -> anyhow::Result<String> {
    let ops = db::list_pending_ops(db)?;
    if ops.is_empty() {
        return Ok("No pending ops.".to_string());
    }
    let mut lines = Vec::with_capacity(ops.len());
    for op in &ops {
        let age = humanize_since(
            std::time::UNIX_EPOCH + Duration::from_secs(op.enqueued_at.max(0) as u64),
        );
        let annotation = if op.errored == 1 || op.error_count > 0 {
            let err = op.last_error.as_deref().unwrap_or("");
            format!("  [errored {}x: {err}]", op.error_count.max(1))
        } else {
            String::new()
        };
        lines.push(format!(
            "#{}  {}  \"{}\"  {}{}",
            op.id, op.op_type, op.summary, age, annotation
        ));
    }
    Ok(lines.join("\n"))
}

fn clear_errored(db: &mut Connection) -> anyhow::Result<String> {
    let n = db::clear_errored(db)?;
    if n == 0 {
        Ok("No errored ops to clear.".to_string())
    } else {
        Ok(format!("Cleared {n} errored op(s). Tap Sync to reconcile."))
    }
}
```

**Why `op.error_count.max(1)`:** terminal errors flip `errored = 1` immediately without incrementing `error_count` (since they bypass the 5-strike path). Displaying "errored 0x" reads weirdly; clamp to 1 so the annotation always reflects "at least one failure occurred."

**Why the `Duration::from_secs` cast guard with `.max(0)`:** `enqueued_at` is i64; if some bug ever produced a negative value, the `as u64` cast would underflow. Belt-and-braces.

**Verification:**

```bash
cd /home/jtd/reTaskable/app/backend
cargo build --release
```

Manual sanity (host): queue a few ops, tap Show Pending (assuming QML side from Task 3 is wired — if not yet, send a synthetic `endpoint.sendMessage(10, "")` from a test harness, or hand-edit main.qml temporarily for the smoke test).

**Commit:** Defer to Task 3.
<!-- END_TASK_2 -->

<!-- END_SUBCOMPONENT_A -->

<!-- START_SUBCOMPONENT_B (task 3) -->

<!-- START_TASK_3 -->
### Task 3: QML — add Show Pending + Clear Errored buttons

**Verifies:** AC4.3 (button → response visible), AC4.4 (double-tap-arm flow).

**Files:**
- Modify: `app/ui/Main.qml` — add two buttons to the existing Flow; extend `onMessageReceived` filter.

**Implementation:**

Find the Flow block (`Flow { width: parent.width; spacing: 16; ... }`) and append two new Rectangle entries after the Delete First button (which is the current last child).

**Show Pending button** (always enabled, single tap):

```qml
Rectangle {
    width: 240; height: 80
    color: "white"; border.color: "black"; border.width: 3
    Text {
        anchors.centerIn: parent
        text: "Show Pending"
        font.pixelSize: 22
        color: "black"
    }
    MouseArea {
        anchors.fill: parent
        onClicked: endpoint.sendMessage(10, "")
    }
}
```

**Clear Errored button** (double-tap-armed, copy of Delete First pattern):

```qml
Rectangle {
    id: clearErroredBtn
    property bool armed: false
    width: 260; height: 80
    color: clearErroredBtn.armed ? "black" : "white"
    border.color: "black"; border.width: 3
    Text {
        anchors.centerIn: parent
        text: clearErroredBtn.armed ? "Tap again to confirm" : "Clear Errored"
        font.pixelSize: 20
        color: clearErroredBtn.armed ? "white" : "black"
    }
    Timer {
        id: clearErroredArmTimer
        interval: 3000
        onTriggered: clearErroredBtn.armed = false
    }
    MouseArea {
        anchors.fill: parent
        onClicked: {
            if (clearErroredBtn.armed) {
                clearErroredBtn.armed = false
                clearErroredArmTimer.stop()
                endpoint.sendMessage(11, "")
            } else {
                clearErroredBtn.armed = true
                clearErroredArmTimer.restart()
            }
        }
    }
}
```

Then extend the `onMessageReceived` filter to include the two new response types. Find the current line (around `Main.qml` line 19–23) and update:

```qml
onMessageReceived: (type, contents) => {
    if (type === 101 || type === 102 || type === 103 || type === 104 || type === 105 || type === 106 || type === 107 || type === 108 || type === 109 || type === 110 || type === 111) {
        responseText.text = contents
    }
}
```

**Layout consideration:** the Flow now hosts 11 buttons. With `width: parent.width` (~954px portrait on RMPPM) and average button widths around 240px + 16px spacing, ~3 buttons fit per row → 4 rows total. Confirmed acceptable per `feedback_ux_debt.md` in project memory — the broader UX overhaul is a separate milestone.

**Verification:**

```bash
cd /home/jtd/reTaskable/app
./build-pc.sh        # host emulator
# Launch the emulator binary, tap Show Pending — verify response area
# updates with "No pending ops." Tap Clear Errored once — label flips to
# "Tap again to confirm" with black background. Wait 4s — label resets.
# Tap Clear Errored twice in <3s — backend handler fires, response shows
# "No errored ops to clear." (since none were queued in this run).
./build-rmpp.sh      # cross-build
```

**Commit:**

```bash
cd /home/jtd/reTaskable
git add app/ui/Main.qml app/backend/src/main.rs
git commit -m "M9a Phase 7: Show Pending + Clear Errored UI (QML + backend)"
```
<!-- END_TASK_3 -->

<!-- END_SUBCOMPONENT_B -->

<!-- START_SUBCOMPONENT_C (task 4) -->

<!-- START_TASK_4 -->
### Task 4: Hardware verification on RMPPM (second landmark)

**Verifies:** AC4.3, AC4.4, AC4.5, AC3.6 — observed on real device with real CalDAV server.

**Files:** None.

**Implementation:**

1. **Build and deploy** the RMPPM binary (`./build-rmpp.sh` and the project's deploy step).
2. **Induce a terminal failure** by creating a real conflict:
   - From the Nextcloud web UI (in a browser), edit a task's summary.
   - Without syncing on the device, tap Edit First and enter a different new summary. The local cache is now ahead of the optimistically-mutated version; on Sync, the server will return 412 (etag mismatch), the retry helper will fetch fresh state, the mutation re-applies, and the second PUT will succeed UNLESS the server's state diverges further — to reliably hit double-412, edit again on the web AFTER the device has queued its op but BEFORE Sync.
   - Alternative reliable terminal: temporarily change the Nextcloud app-password to invalidate auth → tap Sync → all ops get 401 → terminal.
3. **Tap Sync.** Verify the response contains `Flushed 0 / 1 errored (0 retried).` (or similar).
4. **Tap Show Pending.** Verify the response area dumps the errored op with the format `#<id>  <op>  "<summary>"  <age>  [errored Nx: <err>]`. AC4.3.
5. **Tap Clear Errored** (first tap). Verify the button turns black with "Tap again to confirm". Wait 4s — label resets, no action taken.
6. **Tap Clear Errored twice in <3s.** Verify the response area shows `Cleared 1 errored op(s). Tap Sync to reconcile.`
7. **Inspect the cache.** Show Tasks should now show the LOCAL optimistic state (or, for an errored Delete, the row is back to non-tombstoned). If the errored op was a Create, the locally-created row is gone.
8. **Tap Sync.** Verify the response is the normal "Sync complete: incremental, ..." line, and Show Tasks now reflects the AUTHORITATIVE SERVER state — proving AC4.5.
9. **Cascade check (AC3.6 re-verification):** queue two ops on the same UID (e.g., toggle then edit), induce a terminal failure on the first, tap Sync. Open Show Pending — both ops show as errored, the second annotated with `blocked by failed <op_type>`.
10. **Record findings** in `lessons_learned.md` (Phase 8 will format).

**Verification:** Manual. No automated tests for this task.

**Commit:** No code commit (unless verification surfaces a fix; if so, M9a Phase 7 fix commit).
<!-- END_TASK_4 -->

<!-- END_SUBCOMPONENT_C -->

---

## Phase 7 Done When

- All four tasks complete with tests passing.
- Show Pending button surfaces queue state with the AC4.3 format.
- Clear Errored button respects double-tap-arm, drops errored ops, resets cache effects.
- Host build + cross-build green.
- Hardware verification on RMPPM confirms the error → clear → reconcile loop.
- `git log` shows two commits with M9a Phase 7 prefix.

## Risks specific to Phase 7

- **Risk:** The QML AppLoad bridge expects message types 101–109 specifically in the response filter; adding 110/111 might require updating the appload-client crate. **Mitigation:** investigator confirmed the filter is just an `if` in `onMessageReceived` — pure QML, no plugin-level whitelist. Adding to the if-statement is sufficient.
- **Risk:** A pending_op for a UID that was sync-reconciled away (e.g., server deleted it) shows `"(unknown)"` in Show Pending, which is confusing. **Mitigation:** the placeholder is honest — the cache no longer has a summary to show. The user can still Clear Errored and Sync. If this happens frequently in practice, Phase 8's roadmap can include caching the summary into pending_op at enqueue time (deferred design decision).
- **Risk:** Layout: 11 buttons in the Flow is visually busy on RMPPM's ~954px width. **Mitigation:** acknowledged in `feedback_ux_debt.md`. Not blocking; broader overhaul is a future milestone.
- **Risk:** Clear Errored deletes a pending Create whose UID hasn't reached the server. The user loses the locally-typed summary forever. **Mitigation:** the Sync prompt ("Tap Sync to reconcile.") doesn't suggest "lose data" — the user implicitly understands Clear Errored is destructive when they confirm the double-tap. Document this in Phase 8's release notes.
