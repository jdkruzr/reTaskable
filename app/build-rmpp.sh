#!/usr/bin/env bash
# Cross-compile reTaskable for aarch64 reMarkable devices.
# Output lands in ./output-rmpp/, ready to be copied to
# /home/root/xovi/exthome/appload/retaskable/ on the device.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SDK_RCC="$SCRIPT_DIR/../sdk/sysroots/x86_64-codexsdk-linux/usr/libexec/rcc"

if command -v rcc >/dev/null 2>&1; then
    RCC_BIN="rcc"
elif [[ -x "$SDK_RCC" ]]; then
    RCC_BIN="$SDK_RCC"
else
    echo "rcc not found on PATH or at $SDK_RCC" >&2
    exit 1
fi

if command -v aarch64-remarkable-linux-gnu-gcc >/dev/null 2>&1; then
    TARGET_CC="aarch64-remarkable-linux-gnu-gcc"
    TARGET_AR="aarch64-remarkable-linux-gnu-ar"
elif command -v aarch64-remarkable-linux-gcc >/dev/null 2>&1; then
    TARGET_CC="aarch64-remarkable-linux-gcc"
    TARGET_AR="aarch64-remarkable-linux-ar"
else
    echo "aarch64 reMarkable gcc not found on PATH" >&2
    exit 1
fi

# cc-rs (used by build scripts of crates with C dependencies like rusqlite's
# bundled SQLite) prefers per-target env vars.
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER="${CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER:-$TARGET_CC}"
export CC_aarch64_unknown_linux_gnu="${CC_aarch64_unknown_linux_gnu:-${CC:-$TARGET_CC}}"
export AR_aarch64_unknown_linux_gnu="${AR_aarch64_unknown_linux_gnu:-${AR:-$TARGET_AR}}"
export CFLAGS_aarch64_unknown_linux_gnu="${CFLAGS_aarch64_unknown_linux_gnu:-${CFLAGS:-}}"

if [[ -n "${SDKTARGETSYSROOT:-}" && -z "${CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUSTFLAGS:-}" ]]; then
    export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUSTFLAGS="\
-C link-arg=--sysroot=${SDKTARGETSYSROOT} \
-C link-arg=-mcpu=cortex-a55+crypto \
-C link-arg=-mbranch-protection=standard"
fi

cd "$SCRIPT_DIR"
rm -rf output-rmpp
mkdir -p output-rmpp/backend
cp icon.png manifest.json output-rmpp/
"$RCC_BIN" --binary -o output-rmpp/resources.rcc application.qrc

(
    cd backend
    cargo build --target aarch64-unknown-linux-gnu --release
)

cp backend/target/aarch64-unknown-linux-gnu/release/retaskable-backend output-rmpp/backend/entry
chmod +x output-rmpp/backend/entry

echo
echo "Built. Install on device with:"
echo "  scp -r output-rmpp/ root@<device-ip>:/home/root/xovi/exthome/appload/retaskable"
