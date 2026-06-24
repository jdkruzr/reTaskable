#!/usr/bin/env bash
# Cross-compile reTaskable for aarch64 reMarkable devices.
# Output lands in ./output-rmpp/, ready to be copied to
# /home/root/xovi/exthome/appload/retaskable/ on the device.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

if ! command -v rcc >/dev/null 2>&1; then
    echo "rcc not found on PATH" >&2
    exit 1
fi
if ! command -v aarch64-remarkable-linux-gnu-gcc >/dev/null 2>&1; then
    echo "aarch64-remarkable-linux-gnu-gcc not found on PATH" >&2
    exit 1
fi

# cc-rs (used by build scripts of crates with C dependencies like rusqlite's
# bundled SQLite) prefers per-target env vars.
export CC_aarch64_unknown_linux_gnu="${CC_aarch64_unknown_linux_gnu:-aarch64-remarkable-linux-gnu-gcc}"
export AR_aarch64_unknown_linux_gnu="${AR_aarch64_unknown_linux_gnu:-aarch64-remarkable-linux-gnu-ar}"

cd "$SCRIPT_DIR"
rm -rf output-rmpp
mkdir -p output-rmpp/backend
cp icon.png manifest.json output-rmpp/
rcc --binary -o output-rmpp/resources.rcc application.qrc

(
    cd backend
    cargo build --target aarch64-unknown-linux-gnu --release
)

cp backend/target/aarch64-unknown-linux-gnu/release/retaskable-backend output-rmpp/backend/entry
chmod +x output-rmpp/backend/entry

echo
echo "Built. Install on device with:"
echo "  scp -r output-rmpp/ root@<device-ip>:/home/root/xovi/exthome/appload/retaskable"
