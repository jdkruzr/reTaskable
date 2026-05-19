#!/usr/bin/env python3
"""Manufacture a double-412 conflict in a reTaskable cache DB so M9b's
"Resolve First Conflict" path has something to act on.

Natural double-412s require a millisecond-window race between the M5
retry helper's GET and its second PUT, which is essentially impossible
to trigger by hand. The downstream state, though, is just a row in
`pending_op` with `errored=1` and `last_error LIKE '%double-412%'` --
this script plants that state directly.

If multiple unerrored toggle/edit ops exist on the same UID, the oldest
is marked as the double-412 parent and the rest are marked as
"blocked by failed <op_type>" -- mirroring what `queue::cascade_uid`
would emit naturally. This is what makes the cascade-unwind hardware
test possible: enqueue Toggle First, then Edit First on the same task,
run this script, then verify a single Resolve / Keep-Mine clears both.

Usage:
    python3 manufacture-m9b-conflict.py <path-to-db.sqlite>

Full hardware-test workflow (rMPP at 10.11.99.1):

    # 1. In reTaskable on device, enqueue a toggle/edit on some task,
    #    then mutate the same task the OTHER way on Nextcloud web.
    #    (For cascade test, enqueue Toggle First + Edit First on the
    #    same task before continuing.) Do NOT tap Sync.

    # 2. Stop the backend, pull the DB, plant the conflict, push it back:
    ssh root@10.11.99.1 'pkill -f retaskable-backend; sleep 1'
    scp -O root@10.11.99.1:/home/root/.local/share/retaskable/db.sqlite /tmp/db.sqlite
    python3 docs/test-plans/manufacture-m9b-conflict.py /tmp/db.sqlite
    scp -O /tmp/db.sqlite root@10.11.99.1:/home/root/.local/share/retaskable/db.sqlite

    # 3. Relaunch reTaskable on the device. Tap "Resolve First Conflict".
"""
import sqlite3
import sys


DOUBLE_412 = "double-412 (server-side conflict)"


def main(db_path: str) -> int:
    db = sqlite3.connect(db_path)
    rows = list(db.execute(
        "SELECT id, op_type, target_uid FROM pending_op "
        "WHERE op_type IN ('toggle', 'edit') AND errored = 0 "
        "ORDER BY id ASC"
    ))
    if not rows:
        print("no unerrored toggle/edit ops queued -- nothing to manufacture",
              file=sys.stderr)
        return 1

    # Group ops by UID; pick the UID of the most recently queued op so
    # the conflict targets whatever the user was just working on.
    by_uid: dict[str, list[tuple[int, str, str]]] = {}
    for row in rows:
        by_uid.setdefault(row[2], []).append(row)
    target_uid = rows[-1][2]
    group = by_uid[target_uid]
    parent_id, parent_kind, _ = group[0]

    db.execute(
        "UPDATE pending_op SET errored = 1, last_error = ? WHERE id = ?",
        (DOUBLE_412, parent_id),
    )
    cascaded = 0
    for op_id, _, _ in group[1:]:
        db.execute(
            "UPDATE pending_op SET errored = 1, last_error = ? WHERE id = ?",
            (f"blocked by failed {parent_kind}", op_id),
        )
        cascaded += 1
    db.commit()

    print(f"Errored #{parent_id} ({parent_kind} on UID {target_uid}) as double-412.")
    if cascaded:
        print(f"Cascaded {cascaded} sibling op(s) on the same UID.")
    return 0


if __name__ == "__main__":
    if len(sys.argv) != 2:
        print(__doc__, file=sys.stderr)
        sys.exit(2)
    sys.exit(main(sys.argv[1]))
