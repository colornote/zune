#!/bin/sh
# Linker wrapper for cross-building a Mach-O binary with Zig's clang.
#
#   SDKROOT=$HOME/opt/MacOSX11.3.sdk \
#   CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER=$PWD/macos-linker.sh \
#   cargo build --release --target aarch64-apple-darwin
#
# Zig's `cc` resolves the macOS SDK (frameworks + .tbd stubs) through SDKROOT, and
# the deployment target is pinned in the -target triple.
ZIG="${ZIG:-zig}"
SDKROOT="${SDKROOT:-$HOME/opt/MacOSX11.3.sdk}"
MIN="${MACOSX_DEPLOYMENT_TARGET:-11.0}"
export SDKROOT
exec cargo-zigbuild zig cc -- -g '-fno-sanitize=all' -target "aarch64-macos.$MIN" "$@"
