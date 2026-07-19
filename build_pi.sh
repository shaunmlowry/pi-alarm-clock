#!/bin/bash
# build_pi.sh - Cross-compile the project for Raspberry Pi (aarch64)
#
# Uses `cross` (https://github.com/cross-rs/cross), which reads `Cross.toml`
# and builds the custom `Dockerfile.cross-aarch64` image, then compiles the
# workspace for the aarch64 target using that image. The resulting binary is
# copied out of the build container into ./dist so it can be deployed to the
# Pi.

set -euo pipefail

TARGET_ARCH="aarch64-unknown-linux-gnu"
BINARY="alarm-clock"
DIST_DIR="dist"
PROFILE="${PROFILE:-release}"

echo "--- Starting Cross-Compilation for Raspberry Pi (${TARGET_ARCH}) ---"

# Ensure cross is available.
if ! command -v cross >/dev/null 2>&1; then
    echo "!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!"
    echo "FAILED: 'cross' is not installed or not on PATH."
    echo "Install it with: cargo install cross --locked"
    echo "!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!"
    exit 1
fi

# 1. Build the workspace for the Raspberry Pi target.
#    `cross` builds the custom image (via Cross.toml + Dockerfile.cross-aarch64)
#    and runs the cargo build inside it, mounting the cargo/registry caches so
#    subsequent runs are fast.
echo "=> Building with cross (profile: ${PROFILE})..."
if [ "$PROFILE" = "release" ]; then
    cross build --target "$TARGET_ARCH" --release
else
    cross build --target "$TARGET_ARCH"
fi

# 2. `cross` writes the compiled binary directly into the host's cargo
#    target directory (it mounts the target dir), so the artifact is already
#    on the host — no `docker cp` is needed. Copy it into ./dist so it can be
#    deployed to the Pi alongside dist/alarm-clock.service.
BUILD_SUBDIR="release"
if [ "$PROFILE" != "release" ]; then
    BUILD_SUBDIR="debug"
fi
SRC="target/${TARGET_ARCH}/${BUILD_SUBDIR}/${BINARY}"

echo ""
if [ ! -f "$SRC" ]; then
    echo "!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!"
    echo "WARNING: Expected binary not found at '${SRC}'."
    echo "The cross build reported success but the artifact is missing."
    echo "!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!"
else
    mkdir -p "$DIST_DIR"
    cp "$SRC" "${DIST_DIR}/${BINARY}"
    echo "=> Binary copied to ${DIST_DIR}/${BINARY}"
fi

echo ""
echo "✅ Cross-Compilation process completed successfully!"
echo "The aarch64 binary is at '${DIST_DIR}/${BINARY}'."
echo "Deploy it to the Pi (e.g. alongside dist/alarm-clock.service)."
