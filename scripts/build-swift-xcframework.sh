#!/usr/bin/env bash
#
# Build AgentStateGraphFFI.xcframework for the Swift binding.
#
# Cross-compiles the agentstategraph-ffi static library for every Apple
# platform the binding supports, fat-combines per-platform slices with
# lipo, and assembles an XCFramework consumable by SwiftPM / Xcode:
#
#   • macOS            arm64 + x86_64
#   • iOS (device)     arm64
#   • iOS (simulator)  arm64 + x86_64
#
# Output: bindings/swift/artifacts/AgentStateGraphFFI.xcframework
#
# Prerequisites:
#   rustup target add aarch64-apple-darwin x86_64-apple-darwin \
#       aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios
#   Xcode command-line tools (xcodebuild, lipo).
#
# Usage: scripts/build-swift-xcframework.sh
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SWIFT_DIR="$REPO_ROOT/bindings/swift"
CRATE="agentstategraph-ffi"
LIB="libagentstategraph_ffi.a"
OUT="$SWIFT_DIR/artifacts/AgentStateGraphFFI.xcframework"

MACOS_TARGETS=(aarch64-apple-darwin x86_64-apple-darwin)
IOS_DEVICE_TARGET=aarch64-apple-ios
IOS_SIM_TARGETS=(aarch64-apple-ios-sim x86_64-apple-ios)
ALL_TARGETS=("${MACOS_TARGETS[@]}" "$IOS_DEVICE_TARGET" "${IOS_SIM_TARGETS[@]}")

echo "==> Ensuring rust targets are installed"
for t in "${ALL_TARGETS[@]}"; do
    rustup target add "$t" >/dev/null 2>&1 || true
done

echo "==> Building $CRATE (release) for each Apple target"
for t in "${ALL_TARGETS[@]}"; do
    echo "    - $t"
    cargo build --release -p "$CRATE" --target "$t"
done

# Staging dir for fat libs + shared headers.
STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT
mkdir -p "$STAGE/macos" "$STAGE/ios" "$STAGE/iossim" "$STAGE/headers"

libpath() { echo "$REPO_ROOT/target/$1/release/$LIB"; }

echo "==> lipo: macOS (${MACOS_TARGETS[*]})"
lipo -create "$(libpath "${MACOS_TARGETS[0]}")" "$(libpath "${MACOS_TARGETS[1]}")" \
    -output "$STAGE/macos/$LIB"

echo "==> lipo: iOS simulator (${IOS_SIM_TARGETS[*]})"
lipo -create "$(libpath "${IOS_SIM_TARGETS[0]}")" "$(libpath "${IOS_SIM_TARGETS[1]}")" \
    -output "$STAGE/iossim/$LIB"

echo "==> iOS device ($IOS_DEVICE_TARGET)"
cp "$(libpath "$IOS_DEVICE_TARGET")" "$STAGE/ios/$LIB"

echo "==> Assembling headers module (CAgentStateGraph)"
cp "$SWIFT_DIR/Sources/CAgentStateGraph/include/agentstategraph.h" "$STAGE/headers/"
cat > "$STAGE/headers/module.modulemap" <<'EOF'
module CAgentStateGraph {
    header "agentstategraph.h"
    export *
}
EOF

echo "==> Creating XCFramework"
rm -rf "$OUT"
xcodebuild -create-xcframework \
    -library "$STAGE/macos/$LIB"   -headers "$STAGE/headers" \
    -library "$STAGE/ios/$LIB"     -headers "$STAGE/headers" \
    -library "$STAGE/iossim/$LIB"  -headers "$STAGE/headers" \
    -output "$OUT"

echo "==> Done: $OUT"
