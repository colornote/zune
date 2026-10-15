#!/bin/sh
# Cross-build the macOS (Apple Silicon) binary from Linux, using Zig + a macOS SDK.
#
#   brew-free, xcode-free: needs only `rustup target add aarch64-apple-darwin`,
#   a zig tarball on PATH and an extracted MacOSX*.sdk.
#
#   SDKROOT=$HOME/opt/MacOSX11.3.sdk ./build-macos.sh
set -e
: "${SDKROOT:?set SDKROOT to an extracted MacOSX*.sdk directory}"
export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-11.0}"
export CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER="$PWD/macos-linker.sh"
cargo build --release --target aarch64-apple-darwin
mkdir -p dist
cp target/aarch64-apple-darwin/release/zune dist/zune-aarch64-apple-darwin
echo "-> dist/zune-aarch64-apple-darwin"
