# M9a Offline Queue — Phase 6: Sync Handler Integration

**Goal:** Make the existing Sync button drain the queue first (via `queue::flush_pending`) and then run the existing sync-collection REPORT. This is the first M9a hardware landmark on the RMPPM.

**Architecture:** `sync()` in `main.rs` becomes two-phase: (1) discover calendar + flush queue; (2) if no transient failure, run sync-collection as before. The response prepends a `Flushed N / M errored (K retried).` line iff the queue did anything. On transient failure, sync-collection is skipped and the response appends `network failed; sync-collection skipped`. When the queue is empty, the response is byte-for-byte the existing M4 sync-collection output (AC1.1).

**Tech Stack:** Existing `reqwest::Client`, `queue::flush_pending` from Phase 4, the existing `nextcloud::sync_collection_with_fallback` flow.

**Scope:** Phase 6 of 8. Depends on Phase 4 (queue runner) + Phase 5 (handlers enqueue).

**Codebase verified:** 2026-05-17. Investigator confirmed: current `sync()` is at `main.rs:164–254`; response format is `"Sync complete: {kind}, +{updated} updated, -{deleted} deleted."` (this is the M4 baseline the AC calls "byte-for-byte" — the design's reference to `+N / ~M / -K (token …)` is shorthand for "the current sync response"); `reqwest::Client::new()` is constructed per-call (line 189); `SyncDelta` has `added_or_updated`, `deleted_hrefs`, `new_sync_token`.

---

## Acceptance Criteria Coverage

This phase implements and tests:

### m9a-offline-queue.AC1: Sync drains the queue then runs sync-collection
- **m9a-offline-queue.AC1.1 Success:** With `pending_op` empty, Sync runs sync-collection only; response matches the existing M4 format (`+N / ~M / -K (token …)`) byte-for-byte.
- **m9a-offline-queue.AC1.2 Success:** With `pending_op` non-empty and network available, Sync first flushes all ops in id-ascending order via the correct `nextcloud::*` helper per `op_type`, then runs sync-collection.
- **m9a-offline-queue.AC1.3 Success:** Sync response when flush ran prepends `Flushed N / M errored (K retried).` line ahead of the sync-collection summary.
- **m9a-offline-queue.AC1.4 Failure:** When flush hits a transient error mid-drain, Sync does NOT run sync-collection that run; response appends `network failed; sync-collection skipped`.
- **m9a-offline-queue.AC1.5 Edge:** After mid-drain transient break, the next Sync resumes from the first non-errored `pending_op` (succeeded ops are gone; in-progress op's `error_count` reflects prior failures).

### m9a-offline-queue.AC5: Crash safety and queue durability
- **m9a-offline-queue.AC5.2 Success:** After a process kill mid-drain, the next Sync's `flush_pending` resumes at the first non-errored `pending_op` by id.
- **m9a-offline-queue.AC5.3 Edge:** A `pending_op` survives backend restart (durable in SQLite, not held in memory).

---

## Notes for the implementing engineer

- The response-composition logic is factored into a pure function `compose_sync_response(flush, sync, sync_kind, updated, deleted)` so it can be unit-tested without HTTP.
- AC1.4's exact response wording: `Flushed N / M errored (K retried).\nnetwork failed; sync-collection skipped`. The newline is literal (not "\n" as text).
- AC1.1's exact wording: nothing prepended, nothing appended — just the existing `Sync complete: {kind}, +{updated} updated, -{deleted} deleted.` line. The design plan's "M4 format" reference is to the current format, whatever it is. Preserve verbatim.
- AC1.5 and AC5.2 are the same property in different framings: after any mid-drain stop (transient or crash), the next `flush_pending` invocation starts from `fetch_next_drainable`'s lowest non-errored id. This is automatic — no special code in Phase 6 — because `fetch_next_drainable` always picks lowest-id-not-errored.
- AC5.3 (durability across process restart) is a SQLite property, not a code property. Verified by hardware test, not unit test.
- Auth tuple lifetime: `cfg.nextcloud.username.as_str()` and `cfg.nextcloud.app_password.as_str()` borrow from `cfg`, which must outlive both the flush and the sync-collection calls. The existing code already structures this correctly; just keep `cfg` alive through both.

---

<!-- START_SUBCOMPONENT_A (tasks 1-2) -->

<!-- START_TASK_1 -->
### Task 1: Factor response composition into a pure function + unit-test it

**Verifies:** Foundation for AC1.1, AC1.3, AC1.4 (composition correctness).

**Files:**
- Modify: `app/backend/src/main.rs` — add a `compose_sync_response` function near the existing `sync()` helper.
- Modify: `app/backend/src/main.rs` — add a `#[cfg(test)] mod tests` for this function (first test module in `main.rs`).

**Implementation:**

Add this function (place it just above `async fn sync(...)`):

```rust
/// Build the Sync response string from the queue-flush result and the
/// sync-collection result. Pure function — no DB or HTTP access — so it's
/// trivially unit-testable.
///
/// Variants:
/// * Queue empty: returns just the sync-collection line (AC1.1).
/// * Queue had activity, sync-collection ran: prepends Flushed line (AC1.3).
/// * Queue hit transient failure: prepends Flushed line, appends
///   "network failed; sync-collection skipped" (AC1.4). `sync_kind` may be
///   None in that case.
fn compose_sync_response(
    flush: &queue::FlushSummary,
    sync_kind: Option<&str>,
    updated: usize,
    deleted: usize,
) -> String {
    let flush_line = if flush.is_empty() {
        None
    } else {
        // "M errored" rolls up newly_errored + cascade_errored; transient
        // failures are not counted in that total (they remain in the queue).
        Some(format!(
            "Flushed {} / {} errored ({} retried).",
            flush.flushed,
            flush.newly_errored + flush.cascade_errored,
            flush.retried,
        ))
    };

    let sync_line = match sync_kind {
        Some(kind) => format!(
            "Sync complete: {kind}, +{updated} updated, -{deleted} deleted."
        ),
        None => "network failed; sync-collection skipped".to_string(),
    };

    match flush_line {
        Some(f) => format!("{f}\n{sync_line}"),
        None => sync_line,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::queue::FlushSummary;

    #[test]
    fn compose_empty_queue_matches_legacy_format() {
        // AC1.1
        let empty = FlushSummary::default();
        let out = compose_sync_response(&empty, Some("incremental"), 3, 1);
        assert_eq!(out, "Sync complete: incremental, +3 updated, -1 deleted.");
        // byte-for-byte: no prefix, no suffix.
        assert!(!out.contains("Flushed"));
        assert!(!out.contains("skipped"));
    }

    #[test]
    fn compose_flush_then_sync_prepends_flushed_line() {
        // AC1.3
        let flush = FlushSummary { flushed: 5, retried: 1, ..Default::default() };
        let out = compose_sync_response(&flush, Some("incremental"), 0, 0);
        assert_eq!(out, "Flushed 5 / 0 errored (1 retried).\nSync complete: incremental, +0 updated, -0 deleted.");
    }

    #[test]
    fn compose_transient_break_appends_skipped_line() {
        // AC1.4
        let flush = FlushSummary { flushed: 2, transient_failed: 1, ..Default::default() };
        let out = compose_sync_response(&flush, None, 0, 0);
        assert_eq!(out, "Flushed 2 / 0 errored (0 retried).\nnetwork failed; sync-collection skipped");
    }

    #[test]
    fn compose_errored_and_cascade_count_toward_errored_total() {
        let flush = FlushSummary {
            flushed: 1,
            newly_errored: 1,
            cascade_errored: 2,
            ..Default::default()
        };
        let out = compose_sync_response(&flush, Some("full"), 10, 0);
        // "M errored" = newly_errored + cascade_errored
        assert!(out.contains("Flushed 1 / 3 errored (0 retried)."));
    }
}
```

**Verification:**

```bash
cd /home/jtd/reTaskable/app/backend
cargo test --lib tests::compose
cargo build --release
```

**Commit:**

```bash
cd /home/jtd/reTaskable
git add app/backend/src/main.rs
git commit -m "M9a Phase 6: compose_sync_response (Flushed prefix + skipped suffix)"
```
<!-- END_TASK_1 -->

<!-- START_TASK_2 -->
### Task 2: Rewrite `sync()` to drain queue before sync-collection

**Verifies:** m9a-offline-queue.AC1.1, m9a-offline-queue.AC1.2, m9a-offline-queue.AC1.3, m9a-offline-queue.AC1.4, m9a-offline-queue.AC1.5, m9a-offline-queue.AC5.2, m9a-offline-queue.AC5.3 (last two by inspection — durability is automatic).

**Files:**
- Modify: `app/backend/src/main.rs:164–254` — rewrite `sync()`.

**Implementation:**

Replace the entire `sync()` function with:

```rust
async fn sync(db: &mut Connection) -> anyhow::Result<String> {
    let cfg = config::load()?;
    let wanted = cfg.nextcloud.calendar.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "config is missing `calendar = \"...\"` under [nextcloud]. \
             Run List Calendars to see options."
        )
    })?;

    let calendars = nextcloud::discover_calendars(&cfg.nextcloud).await?;
    let cal = calendars
        .iter()
        .find(|c| c.display_name == *wanted)
        .ok_or_else(|| {
            let names: Vec<&str> = calendars.iter().map(|c| c.display_name.as_str()).collect();
            anyhow::anyhow!(
                "calendar {:?} not found among discovered calendars: {:?}",
                wanted,
                names
            )
        })?;

    db::upsert_calendar(db, &cal.href, &cal.display_name)?;

    let calendar_url = url::Url::parse(&cal.href)?;
    let client = reqwest::Client::new();
    let auth = (
        cfg.nextcloud.username.as_str(),
        cfg.nextcloud.app_password.as_str(),
    );

    // --- Phase 1: drain the queue ---
    let flush = queue::flush_pending(db, &client, auth, &calendar_url).await?;

    // If a transient failure broke the drain, skip sync-collection and report.
    if flush.transient_failed > 0 {
        return Ok(compose_sync_response(&flush, None, 0, 0));
    }

    // --- Phase 2: sync-collection REPORT (unchanged from M4) ---
    let prior_token = db::get_sync_token(db, &cal.href)?;
    let (delta, was_full) = nextcloud::sync_collection_with_fallback(
        &client,
        &calendar_url,
        prior_token.as_deref(),
        auth,
    )
    .await?;

    let mut updated = 0;
    let mut deleted = 0;

    if was_full {
        let kept: std::collections::HashSet<String> =
            delta.added_or_updated.iter().map(|r| r.href.clone()).collect();
        for record in &delta.added_or_updated {
            db::upsert_task(
                db,
                &cal.href,
                &record.href,
                &record.etag,
                &record.ical_text,
                &record.task.uid,
                &record.task,
            )?;
            updated += 1;
        }
        deleted = db::delete_tasks_not_in(db, &cal.href, &kept)?;
    } else {
        for record in &delta.added_or_updated {
            db::upsert_task(
                db,
                &cal.href,
                &record.href,
                &record.etag,
                &record.ical_text,
                &record.task.uid,
                &record.task,
            )?;
            updated += 1;
        }
        for href in &delta.deleted_hrefs {
            db::delete_task(db, &cal.href, href)?;
            deleted += 1;
        }
    }

    if let Some(token) = &delta.new_sync_token {
        db::set_sync_token(db, &cal.href, token, SystemTime::now())?;
    } else if was_full {
        db::clear_sync_token(db, &cal.href)?;
    }

    let kind = if was_full { "full" } else { "incremental" };
    Ok(compose_sync_response(&flush, Some(kind), updated, deleted))
}
```

**Diff summary** (against the existing `sync` at lines 164–254):
1. After the auth tuple (line 193), insert a `queue::flush_pending` call.
2. Insert the early-return on transient failure.
3. The `upsert_task` calls now pass `&record.task.uid` as the new positional arg (consistent with Phase 1's signature change). This was already covered in Phase 1 Task 5 — verify the call sites here line up.
4. Replace the final `format!` with `compose_sync_response(...)`.

**Important interaction with cascade:** if a queued op's flush errored terminally (not transient), `flush_pending` keeps draining other uids — so by the time sync-collection runs, some pending_ops may be in errored=1 state. That's fine: sync-collection reconciles the cache against the server. Errored ops are visible via Show Pending (Phase 7) and can be cleared by the user.

**Verification:**

```bash
cd /home/jtd/reTaskable/app/backend
cargo test --lib                  # all tests including the new compose_sync_response tests
cargo build --release
cd /home/jtd/reTaskable/app
./build-pc.sh
./build-rmpp.sh
```

Manual sanity on host emulator:
1. Tap Sync with an empty queue → response should be `Sync complete: full, +N updated, -M deleted.` (or `incremental, ...`). No "Flushed" prefix. (AC1.1)
2. Toggle a task, then tap Sync → response should be `Flushed 1 / 0 errored (0 retried).\nSync complete: incremental, +0 updated, -0 deleted.` (AC1.3)
3. Disconnect USB-net, queue 3 ops, tap Sync → response includes `network failed; sync-collection skipped`. (AC1.4)
4. Reconnect, tap Sync → all 3 flush; subsequent Sync goes empty-queue path. (AC1.5)

**Commit:**

```bash
cd /home/jtd/reTaskable
git add app/backend/src/main.rs
git commit -m "M9a Phase 6: sync() drains queue first, then runs sync-collection"
```
<!-- END_TASK_2 -->

<!-- END_SUBCOMPONENT_A -->

<!-- START_SUBCOMPONENT_B (task 3) -->

<!-- START_TASK_3 -->
### Task 3: Hardware verification on RMPPM (first landmark)

**Verifies:** AC1.1–AC1.5, AC5.2, AC5.3 — observed in production environment.

**Files:** None — this is execution, not code.

**Implementation:**

1. **Build and deploy:**
   ```bash
   cd /home/jtd/reTaskable/app
   ./build-rmpp.sh
   # Push to device per the project's existing deployment flow
   # (likely scp the output-rmpp/ directory; check README or build-rmpp.sh tail for the exact step)
   ```

2. **Pre-flight on device:**
   - Confirm `~root/.local/share/retaskable/db.sqlite` exists. If it's pre-M9a, the first launch performs the v2 migration automatically (Phase 1's `ensure_schema_v2`). Tap Sync once to repopulate the cache via full sync-collection.
   - Verify Show Tasks displays tasks.

3. **Happy-path queue + drain:**
   - Disconnect USB-net (or otherwise sever connectivity to the CalDAV server).
   - Toggle 3 different tasks (tap Toggle First, scroll past, repeat — or perform Create + Toggle + Edit for variety).
   - Each tap should respond `Queued: <op> "<summary>" ... (#<n>)` within ~100ms.
   - Tap Show Pending (Phase 7 — if Phase 7 not yet shipped, inspect with `sqlite3 ~/.local/share/retaskable/db.sqlite "SELECT * FROM pending_op"` over SSH).
   - Reconnect USB-net.
   - Tap Sync. Verify response: `Flushed 3 / 0 errored (0 retried).\nSync complete: incremental, +0 updated, -0 deleted.` (some retried count is possible if a 412 raced; that's acceptable.)
   - Show Tasks unchanged (same content, since the server already has the optimistic state).
   - Show Pending now empty.

4. **Transient-break loop (AC1.4 + AC1.5):**
   - Disconnect USB-net.
   - Queue 3 ops.
   - Tap Sync. Verify response includes `network failed; sync-collection skipped`.
   - Inspect `pending_op`: the first op's `error_count` is 1; `errored` is 0; the other two are untouched.
   - Reconnect and tap Sync. Verify all three drain in order, then sync-collection runs.

5. **Crash recovery (AC5.2 + AC5.3):**
   - Queue 1 op (no need to disconnect).
   - Kill the backend mid-flush (`kill -9` the `entry` process while you tap Sync — race-y but achievable; alternatively, force-kill the AppLoad app via the RMPPM UI).
   - Re-launch reTaskable.
   - Inspect `pending_op`: the op is still there (durability).
   - Tap Sync. The op drains.

6. **Record results** in `/home/jtd/.claude/projects/-home-jtd-reTaskable/memory/lessons_learned.md` (Phase 8 will format this; for now jot notes during hardware verification: any surprising error messages, any unexpected `error_count` behavior, any timing observations).

**Verification:** Manual. No automated tests for this task.

**Commit:** No code commit. If the hardware verification reveals a fix, that's a follow-up commit with the M9a Phase 6 fix prefix.
<!-- END_TASK_3 -->

<!-- END_SUBCOMPONENT_B -->

---

## Phase 6 Done When

- `sync()` drains the queue before running sync-collection.
- Response composition matches AC1.1, AC1.3, AC1.4 byte-for-byte.
- Unit tests for `compose_sync_response` pass.
- Host build + cross-build green.
- Hardware verification on RMPPM successfully demonstrates: happy-path drain (AC1.2), empty-queue Sync (AC1.1), transient break + resume (AC1.4 + AC1.5), crash recovery (AC5.2 + AC5.3).
- `git log` shows two commits with M9a Phase 6 prefix.

## Risks specific to Phase 6

- **Risk:** The auth tuple `auth` is passed to both `flush_pending` and `sync_collection_with_fallback`. If `cfg` is dropped between them, the &str references dangle. **Mitigation:** the function keeps `cfg` in scope through the entire body (just don't `let _ = cfg;` it). Rust's borrow checker will catch a regression at compile time.
- **Risk:** `flush_pending` errors out (returns `Err`) for a reason unrelated to transient HTTP (e.g., SQLite error). **Mitigation:** `?` propagates the error to the caller; `MSG_SYNC` handler wraps it as `error: {e:#}` and the user sees it. Acceptable.
- **Risk:** A terminal error in the queue (errored=1 op) prevents the Sync user from realising what happened until they open Show Pending in Phase 7. **Mitigation:** the Flushed line reports `M errored` count, which is the signal. Phase 7 makes it actionable. Acceptable for the hardware landmark.
- **Risk:** Some servers don't return a `new_sync_token` from sync-collection, causing the existing `clear_sync_token` path (line 240 of the old code) to fire. This is preserved in the rewrite. **Mitigation:** no change from M4 behavior; if it was working before, it works now.
