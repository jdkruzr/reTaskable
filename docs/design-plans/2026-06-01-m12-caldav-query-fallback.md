# M12: CalDAV Query Fallback for Servers Without WebDAV-Sync — Design

## Summary

M12 makes reTaskable work against CalDAV servers that do not implement the RFC 6578 `sync-collection` REPORT. Before this milestone, every Sync assumed `sync-collection` was available: a server that rejected the REPORT would fail permanently. The change aligns with the project's CalDAV-standards posture: target any RFC-compliant server, not just Nextcloud.

The fix is a two-level fallback inside `nextcloud::sync_collection_with_fallback`:
1. If the server rejects `sync-collection` with a recognisable "unsupported" error, fall back to an RFC 4791 §9.5 `calendar-query` REPORT that fetches all VTODOs at once.
2. `calendar-query`'s multistatus has the same `href + getetag + calendar-data` shape as `sync-collection`'s, so the existing `parse_sync_response` handles both without modification.
3. Because `calendar-query` returns no sync-token, every Sync is a full pull; `db::set_last_synced` records the timestamp so the UI shows "Last synced …" instead of "Not yet synced" indefinitely.

## Problem

Found on hardware against a go-webdav-based server ("UltraBridge"). When reTaskable issued a `sync-collection` REPORT, the server responded:

```
400 Bad Request: caldav: unsupported REPORT root "DAV:" "sync-collection"
```

RFC 6578 (WebDAV Sync) is explicitly optional. Nextcloud implements it; go-webdav does not. The bug was a silent assumption baked in since M4.

## Key Decisions

### `sync_collection_unsupported(msg: &str) -> bool`

A pure predicate that detects the failure class:

```rust
fn sync_collection_unsupported(msg: &str) -> bool {
    let m = msg.to_ascii_lowercase();
    m.contains("sync-collection")
        && (m.contains("unsupported")
            || m.contains("not implemented")
            || m.contains("400")
            || m.contains("501"))
}
```

The detector is conservative: `400` alone does not trigger the fallback (avoids misidentifying a malformed-request error). It requires `sync-collection` to appear in the message, ensuring only the specific REPORT-not-supported case triggers the fallback. A unit test covers the go-webdav exact error string and verifies that unrelated 400s (e.g. a 412 on a PUT) do not match.

The detector is called in two places within `sync_collection_with_fallback`:
- After a failing incremental sync (in case the server changes support between runs).
- After a failing full sync (the primary fallback trigger on first contact with a non-WebDAV-Sync server).

### `calendar_query_all` and `calendar_query_body`

```rust
fn calendar_query_body() -> String { ... }  // RFC 4791 §9.5 VTODO comp-filter
async fn calendar_query_all(client, calendar_url, auth) -> Result<SyncDelta>
```

`calendar_query_body()` produces a `calendar-query` REPORT requesting `getetag` + `calendar-data` for all VTODO components. `calendar_query_all` issues the REPORT and pipes the response body through `parse_sync_response` with the same `base` URL — no new XML parsing code. The returned `SyncDelta` has `new_sync_token = None` and no `deleted_hrefs` (a full enumeration, not a delta); the caller treats the result as `was_full = true`.

### Token-less full sync: `db::set_last_synced`

`sync_collection_with_fallback` already returned `(SyncDelta, was_full: bool)`. When `was_full = true` and `delta.new_sync_token` is `None` (which was already possible if Nextcloud omitted the token on a full sync), the `sync` handler in `main.rs` calls:

```rust
db::clear_sync_token(db, &cal.href)?;
db::set_last_synced(db, &cal.href, SystemTime::now())?;
```

This was a pre-existing branch that now activates for the `calendar-query` path too. `set_last_synced` writes only `last_synced_at`; `sync_token` stays `NULL`. On the next Sync, `get_sync_token` returns `None`, triggering another full pull — correct for token-less servers. The "Last synced N minutes ago." string appears in the `format_tasks_json` envelope, so the UI stops showing "Not yet synced — tap Sync." after the first successful `calendar-query`.

### No new MSG pairs, no schema change

M12 is entirely within `nextcloud.rs` (the detection logic, query body, and fallback call) plus a small addition to `db.rs` (`set_last_synced` and its test). No QML changes. No new `pending_op` fields. Schema version stays at 2.

## Acceptance Criteria

1. `sync_collection_unsupported` returns `true` for the exact go-webdav error string; returns `false` for a 412 error or a plain network failure.
2. `parse_sync_response` handles a `calendar-query` multistatus with no `<sync-token>` element: `delta.new_sync_token` is `None`, `delta.deleted_hrefs` is empty, `delta.added_or_updated` contains the VTODOs.
3. `db::set_last_synced` updates `last_synced_at` without touching `sync_token` (stays `NULL`).
4. After a token-less full sync, the `format_tasks_json` envelope carries a non-null `last_synced` string.
5. Hardware verified: Sync against UltraBridge (go-webdav) succeeds, tasks appear in the ListView, "Last synced …" appears in the status line on subsequent syncs.

## Verification

The go-webdav server rejects `sync-collection` but accepts `calendar-query`. Verified on hardware:
- First Sync: falls through to `calendar-query`, fetches all VTODOs, `was_full = true`, token not stored.
- Second Sync: `get_sync_token` returns `None`, full pull again via `calendar-query`. Status line shows "Last synced N minutes ago."
- Nextcloud path unaffected: `sync_collection_unsupported` returns `false` for Nextcloud's normal 207 responses; the incremental/full `sync-collection` path continues to function normally.

Unit tests added (2): `sync_collection_unsupported_detects_go_webdav_400` (in `nextcloud.rs`) and `parse_sync_response_handles_calendar_query_without_sync_token` (in `nextcloud.rs`), plus `set_last_synced_updates_time_without_a_token` (in `db.rs`). These bring the total to 110 passing tests.
