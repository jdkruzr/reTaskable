#!/usr/bin/env bash
# Host build of reTaskable for running in the AppLoad PC emulator.
# Output lands in ./output-pc/.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

if ! command -v rcc >/dev/null 2>&1; then
    echo "rcc not found on PATH" >&2
    exit 1
fi

cd "$SCRIPT_DIR"
rm -rf output-pc
mkdir -p output-pc/backend
cp icon.png manifest.json output-pc/
rcc --binary -o output-pc/resources.rcc application.qrc

(
    cd backend
    cargo build --release
)

cp backend/target/release/retaskable-backend output-pc/backend/entry
chmod +x output-pc/backend/entry

echo
echo "Built. Place output-pc/ inside the AppLoad PC emulator's applications_root/."
