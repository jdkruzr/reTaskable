#!/usr/bin/env bash
# Install the latest cross-compiled reTaskable to the device.
#
# Avoids the two recurring footguns:
#   1. OpenSSH 9.x's scp insists destination exists; trailing-slash semantics
#      drift between versions and create subdir mishaps (e.g. .../retaskable/output-rmpp/).
#   2. AppLoad keeps backends alive in xochitl after the frontend closes, so a
#      newly-scp'd binary can be ignored in favor of an already-running stale one.
#
# We resolve both by wiping the install dir cleanly, scp'ing fresh, and
# killing any running backend before the user reopens the app.

set -euo pipefail

DEVICE="${1:-10.11.99.1}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LOCAL_BUILD="$SCRIPT_DIR/output-rmpp"
REMOTE_DIR="/home/root/xovi/exthome/appload/retaskable"

if [[ ! -d "$LOCAL_BUILD" ]]; then
    echo "no $LOCAL_BUILD; run ./build-rmpp.sh first" >&2
    exit 1
fi

echo "==> Killing any running reTaskable backend on $DEVICE"
# Backend is invoked as "backend/entry <socket>"; pkill -f matches the full
# command line. The binary is named "entry" so pidof would be too broad.
ssh "root@$DEVICE" "pkill -f '$REMOTE_DIR/backend/entry' 2>/dev/null || true"

echo "==> Wiping $REMOTE_DIR on $DEVICE"
ssh "root@$DEVICE" "rm -rf '$REMOTE_DIR'"

echo "==> Uploading fresh build"
# -O forces legacy SCP protocol so a non-existent destination dir is OK.
scp -O -r "$LOCAL_BUILD" "root@$DEVICE:$REMOTE_DIR"

echo "==> Done. Refresh the AppLoad launcher on device and relaunch reTaskable."
