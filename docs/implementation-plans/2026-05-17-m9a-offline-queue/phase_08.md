# M9a Offline Queue — Phase 8: Documentation & Memory Updates

**Goal:** Capture M9a's gotchas and bump project status so the next session picks up cleanly.

**Architecture:** No code changes. This is purely documentation: `lessons_learned.md` gets one or more new entries from Phases 1–7 observations, `project_retaskable_status.md` bumps to "M9a done & shipped", `project_retaskable.md` roadmap reflects any scope shifts for M9b/c, and `README.md` is touched if user-facing semantics changed.

**Tech Stack:** None.

**Scope:** Phase 8 of 8. Depends on Phase 7 (all hardware verification complete).

**Codebase verified:** 2026-05-17. Memory files at `/home/jtd/.claude/projects/-home-jtd-reTaskable/memory/`: `MEMORY.md`, `lessons_learned.md`, `project_retaskable_status.md`, `project_retaskable.md`, `feedback_*.md`, `reference_locations.md`, `user_context.md`, `README.md` exists at `/home/jtd/reTaskable/README.md` (105 lines).

---

## Acceptance Criteria Coverage

This phase does not directly verify any acceptance criterion. Its purpose is to leave the project in a state where the next milestone (M9b — conflict-resolution UI) can begin from a clean baseline.

---

## Notes for the implementing engineer

- This phase has **no automated tests**. It is documentation only.
- The implementing engineer should have notes from Phases 1–7 hardware verifications (any surprising error messages, any unexpected timing, any cross-compile gotchas encountered). Those notes become the Phase 8 `lessons_learned.md` content. If the verification was clean and no surprises arose, the lesson is "M9a shipped without surprises" — fine.
- Memory files use a YAML frontmatter format described at the top of `MEMORY.md`. Match the existing entry style.
- Update both the auto-memory side (`/home/jtd/.claude/projects/-home-jtd-reTaskable/memory/`) AND the in-repo README (`/home/jtd/reTaskable/README.md`) if user-facing behavior changed enough to mention.
- The current README has 105 lines — review it; the M9a section likely needs a paragraph explaining the offline-queue UX, the Show Pending button, and the Clear Errored button.

---

<!-- START_TASK_1 -->
### Task 1: Update `project_retaskable_status.md`

**Verifies:** None (documentation).

**Files:**
- Modify: `/home/jtd/.claude/projects/-home-jtd-reTaskable/memory/project_retaskable_status.md`

**Implementation:**

Read the current file to see its format. Append (or restructure to add) an M9a milestone summary block:

```markdown
## M9a — Offline write queue (done 2026-MM-DD)

- pending_op table + schema_version=2 migration
- Four enqueue helpers in db.rs (create, edit, toggle, delete) — atomic with optimistic cache mutation
- queue.rs runner with FlushSummary, classify_error, 5-strike + cascade
- Sync drains queue first, then runs sync-collection
- Show Pending + Clear Errored buttons (Flow now 11 buttons; UX overhaul still pending — see [[feedback_ux_debt]])
- Verified on RMPPM hardware (happy path, transient break, terminal cascade, clear + reconcile)

Next: M9b — conflict-resolution UI for the errored-op case.
```

Replace `2026-MM-DD` with the actual completion date when the implementing engineer commits.

If the file has a "current status" or "current milestone" header at the top, update it from "M9a in progress" (or whatever it says) to "M9a done; next M9b".

**Verification:** Visual review only. Read the file back, confirm M9a is recorded.

**Commit:** Memory files are outside the git repo (different directory). No git commit needed for this file. The runtime auto-loads it.
<!-- END_TASK_1 -->

<!-- START_TASK_2 -->
### Task 2: Add M9a entries to `lessons_learned.md`

**Verifies:** None (documentation).

**Files:**
- Modify: `/home/jtd/.claude/projects/-home-jtd-reTaskable/memory/lessons_learned.md`

**Implementation:**

The implementing engineer should curate observations from Phases 1–7 into one or more lessons. Likely candidates (based on the plan's "Risks specific to Phase N" sections):

- **Schema migration**: any unexpected behavior during the v1→v2 drop-and-recreate on a populated device DB.
- **JSON payload escaping**: whether `serde_json::to_string` on `json!({"summary": ...})` handled any surprising edge cases (newlines, embedded quotes, emoji).
- **Error classifier taxonomy**: which `anyhow::Error` messages turned out to need their own classifier branch (e.g., did a Nextcloud version surface a 412 with a different format string?).
- **Cross-compile**: did the new `httpmock` dev-dependency cause any cross-build issues? Did indexes/PRAGMAs need any platform-specific care?
- **String-based error classification**: the design accepts string matching as Phase-4-pragmatic; a typed-error refactor of nextcloud.rs is a known follow-up.
- **Layout headroom**: 11 buttons in the Flow on RMPPM — does it wrap as expected? Tap targets too close?
- **`build_task_url` href shape assumption**: did the server's sync-collection emit absolute or absolute-path hrefs? If absolute, `build_task_url`'s `set_path` approach fails; document the workaround.

Format each lesson per the existing file's convention (likely YAML frontmatter at top, then prose body with optional `[[wikilink]]` to related memories). Example shape:

```markdown
---
name: m9a-error-classifier-string-matching
description: Phase 4's classify_error uses string matching on anyhow::Error messages to distinguish transient (5xx, network) vs terminal (4xx, double-412). A typed-error refactor in nextcloud.rs is the cleaner long-term solution but was deferred from M9a.
metadata:
  type: feedback
---

The Phase 4 queue runner needs to know whether to retry an op or mark it errored.
... etc ...
```

Link related memories: `[[feedback_caldav_standards]]` (CalDAV posture is unchanged), `[[lessons_learned]]` (cross-reference).

If the lesson would benefit from a code snippet, include it. Keep each lesson under ~50 lines.

After adding the lessons, append a line to `MEMORY.md` for any new lesson files (one line per file, format: `- [Title](file.md) — short hook`).

**Verification:** Visual review. Confirm each lesson is self-contained and would be useful in a future session that hits the same issue.

**Commit:** No git commit (auto-memory files are outside the repo).
<!-- END_TASK_2 -->

<!-- START_TASK_3 -->
### Task 3: Update roadmap in `project_retaskable.md` (if scopes shifted)

**Verifies:** None (documentation).

**Files:**
- Modify: `/home/jtd/.claude/projects/-home-jtd-reTaskable/memory/project_retaskable.md`

**Implementation:**

Read the current file. It almost certainly contains an M9b / M9c roadmap line. Update if any M9a learning shifted the scope:

- M9b was originally "conflict-resolution UI". If the M9a verification surfaced specific conflict patterns (e.g., "users hit double-412 most often when editing on web during sync"), note that as M9b's primary use case.
- M9c was originally "pending-row markers inside Show Tasks output". Still relevant.
- Network detection was explicitly out of scope. Confirm it stays out (or has been bumped into M9b/c based on hardware verification observations).

If no scope shifts emerged, this task is a no-op — note it as complete with `(no changes — roadmap unchanged)`.

**Verification:** Visual review.

**Commit:** No git commit.
<!-- END_TASK_3 -->

<!-- START_TASK_4 -->
### Task 4: Update `README.md` with M9a user-facing semantics

**Verifies:** None (documentation).

**Files:**
- Modify: `/home/jtd/reTaskable/README.md`

**Implementation:**

Read the current README. M9a introduces user-visible behavior changes that warrant mention:

1. Writes are now instant ("Queued: <op> ...") — no waiting for the server.
2. Sync now flushes the queue before pulling server state.
3. Two new buttons: Show Pending and Clear Errored.
4. The on-device DB is migrated to schema_version=2 on first M9a launch.

Add a section (or extend the existing milestone history section) covering these changes. Suggested wording for the user-facing description (adapt to README style):

```markdown
## Offline queue (M9a)

reTaskable now stores every write (Create, Toggle, Edit, Delete) in a local
queue and applies it optimistically to the cached task view, then flushes
the queue to the CalDAV server when you tap Sync.

- Writes return "Queued: <op> ..." instantly, even with no network.
- Tap **Sync** to drain the queue and pull server state.
- Tap **Show Pending** to see every queued operation, including any errors.
- Tap **Clear Errored** (twice, to confirm) to drop ops the server rejected;
  the next Sync reconciles your cached view with authoritative server state.

On first launch after upgrading, the local cache is rebuilt from the server
(schema migration). Your data on the server is unchanged.
```

**Verification:**

```bash
cd /home/jtd/reTaskable
git diff README.md
```

Visual: render the README on GitHub or in a Markdown previewer. Confirm the new section reads cleanly.

**Commit:**

```bash
cd /home/jtd/reTaskable
git add README.md
git commit -m "M9a Phase 8: README documents offline queue + Show Pending + Clear Errored"
```
<!-- END_TASK_4 -->

---

## Phase 8 Done When

- `project_retaskable_status.md` records M9a as done with the milestone date.
- `lessons_learned.md` has at least one new entry capturing any non-obvious M9a discovery (or explicitly notes "no surprises encountered").
- `project_retaskable.md` roadmap reflects any M9b/c scope shifts (or is confirmed unchanged).
- `README.md` describes the M9a offline-queue UX visible to users.
- `git log` shows one commit (the README update).

## Risks specific to Phase 8

- **Risk:** Auto-memory files are written, but the implementing engineer forgets to update `MEMORY.md` to point at new lesson files. **Mitigation:** Task 2 explicitly calls this out. If forgotten, the next session might not surface the lesson on context load.
- **Risk:** README drift — describing the offline queue in language that diverges from in-app strings ("Show Pending" vs "View Queue"). **Mitigation:** copy the actual QML button labels verbatim into the README.
- **Risk:** Out-of-date memory content is worse than missing memory content. **Mitigation:** if the implementing engineer is unsure whether a lesson is still relevant, omit it rather than guess.
