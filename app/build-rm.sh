#!/usr/bin/env bash
# Cross-compile reTaskable for armv7 reMarkable devices.
# Output lands in ./output-rm/, ready to be copied to
# /home/root/xovi/exthome/appload/retaskable/ on the device.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

if ! command -v rcc >/dev/null 2>&1; then
    echo "rcc not found on PATH" >&2
    exit 1
fi
if ! command -v arm-linux-gnueabihf-gcc >/dev/null 2>&1; then
    echo "arm-linux-gnueabihf-gcc not found on PATH" >&2
    exit 1
fi

# cc-rs (used by build scripts of crates with C dependencies like rusqlite's
# bundled SQLite) prefers per-target env vars.
export CC_armv7_unknown_linux_gnueabihf="${CC_armv7_unknown_linux_gnueabihf:-arm-linux-gnueabihf-gcc}"
export AR_armv7_unknown_linux_gnueabihf="${AR_armv7_unknown_linux_gnueabihf:-arm-linux-gnueabihf-ar}"

cd "$SCRIPT_DIR"
rm -rf output-rm
mkdir -p output-rm/backend
cp icon.png manifest.json output-rm/
rcc --binary -o output-rm/resources.rcc application.qrc

(
    cd backend
    cargo build --target armv7-unknown-linux-gnueabihf --release
)

cp backend/target/armv7-unknown-linux-gnueabihf/release/retaskable-backend output-rm/backend/entry
chmod +x output-rm/backend/entry

echo
echo "Built. Install on device with:"
echo "  scp -r output-rm/ root@<device-ip>:/home/root/xovi/exthome/appload/retaskable"
