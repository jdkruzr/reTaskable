#!/usr/bin/env bash
# Cross-compile reTaskable for the reMarkable Paper Pro Move (aarch64).
# Output lands in ./output-rmpp/, ready to be copied to
# /home/root/xovi/exthome/appload/retaskable/ on the device.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SDK_ROOT="$(cd "$SCRIPT_DIR/../sdk" && pwd)"
RCC="$SDK_ROOT/sysroots/x86_64-codexsdk-linux/usr/libexec/rcc"
SDK_ENV="$SDK_ROOT/environment-setup-cortexa55-remarkable-linux"

if [[ ! -x "$RCC" ]]; then
    echo "rcc not found at $RCC" >&2
    exit 1
fi
if [[ ! -f "$SDK_ENV" ]]; then
    echo "SDK env-setup not found at $SDK_ENV" >&2
    exit 1
fi

# Sourcing the rM cross-SDK exports CC/CXX/SDKTARGETSYSROOT pointing at the
# device's exact toolchain. Disable nounset around the source — Yocto's
# env-setup script does not declare every variable it touches.
set +u
# shellcheck disable=SC1090
source "$SDK_ENV"
set -u

# Tell cargo to use the SDK's gcc as the linker for aarch64 and route it at
# the device sysroot. -mcpu/-mbranch-protection match the SDK's CFLAGS so
# the linker can find the right startup files.
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER="aarch64-remarkable-linux-gcc"
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUSTFLAGS="\
-C link-arg=--sysroot=${SDKTARGETSYSROOT} \
-C link-arg=-mcpu=cortex-a55+crypto \
-C link-arg=-mbranch-protection=standard"

# cc-rs (used by build scripts of crates with C dependencies like rusqlite's
# bundled SQLite) prefers per-target env vars. Point them at the SDK
# toolchain so any C code cross-compiles against the device sysroot.
export CC_aarch64_unknown_linux_gnu="${CC}"
export AR_aarch64_unknown_linux_gnu="${AR}"
export CFLAGS_aarch64_unknown_linux_gnu="${CFLAGS}"

cd "$SCRIPT_DIR"
rm -rf output-rmpp
mkdir -p output-rmpp/backend
cp icon.png manifest.json output-rmpp/
"$RCC" --binary -o output-rmpp/resources.rcc application.qrc

(
    cd backend
    cargo build --target aarch64-unknown-linux-gnu --release
)

cp backend/target/aarch64-unknown-linux-gnu/release/retaskable-backend output-rmpp/backend/entry
chmod +x output-rmpp/backend/entry

echo
echo "Built. Install on device with:"
echo "  scp -r output-rmpp/ root@<device-ip>:/home/root/xovi/exthome/appload/retaskable"
