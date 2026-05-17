#!/usr/bin/env bash
# Host build of reTaskable for running in the AppLoad PC emulator.
# Output lands in ./output-pc/.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SDK_ROOT="$(cd "$SCRIPT_DIR/../sdk" && pwd)"
RCC="$SDK_ROOT/sysroots/x86_64-codexsdk-linux/usr/libexec/rcc"

if [[ ! -x "$RCC" ]]; then
    echo "rcc not found at $RCC" >&2
    exit 1
fi

cd "$SCRIPT_DIR"
rm -rf output-pc
mkdir -p output-pc/backend
cp icon.png manifest.json output-pc/
"$RCC" --binary -o output-pc/resources.rcc application.qrc

(
    cd backend
    cargo build --release
)

cp backend/target/release/retaskable-backend output-pc/backend/entry
chmod +x output-pc/backend/entry

echo
echo "Built. Place output-pc/ inside the AppLoad PC emulator's applications_root/."
