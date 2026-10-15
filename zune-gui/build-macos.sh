#!/bin/sh
# Cross-build zune-gui for macOS arm64 from Linux (Zig as linker + macOS SDK), then package dist/.
#
#   SDKROOT=$HOME/opt/MacOSX11.3.sdk ./build-macos.sh          # both backends
#   BACKEND=glow ./build-macos.sh                              # only one backend
#
# Requirements: cargo-zigbuild, zig on PATH, an extracted MacOSX*.sdk
# (default ~/opt/MacOSX11.3.sdk), and the rust target installed:
#   rustup target add aarch64-apple-darwin
#set -e

cd "$(dirname "$0")"

export PATH="$HOME/.cargo/bin:$HOME/opt/zig-x86_64-linux-0.15.2:$PATH"
: "${SDKROOT:=$HOME/opt/MacOSX11.3.sdk}"
export SDKROOT
export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-11.0}"
BACKEND="${BACKEND:-all}"
TARGET=aarch64-apple-darwin
PKG="zune-gui-$TARGET"

# wgpu (Metal) is the primary artifact; glow (OpenGL) is the fallback.
mkdir -p dist
for backend in glow wgpu; do
	case "$BACKEND" in
	all | "$backend") ;;
	*) continue ;;
	esac

	echo "== building zune-gui ($backend) for $TARGET =="
	# Same linker scheme as zune-rs/build-macos.sh: zig's clang drives the link and
	# pins the deployment target to 11.0 (=> LC_BUILD_VERSION minos 11.0).
	CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER="$PWD/macos-linker.sh" \
		cargo build --release --target "$TARGET" \
		--no-default-features --features "$backend" \
		--target-dir "target-$backend"
	cp "target-$backend/$TARGET/release/zune-gui" "dist/zune-gui-$backend-$TARGET"
done

# Package: the wgpu build gets the canonical name, glow gets a -glow suffix.
[ -f "dist/zune-gui-wgpu-$TARGET" ] && {
	rm -rf dist/pkg && mkdir -p dist/pkg
	cp "dist/zune-gui-wgpu-$TARGET" dist/pkg/zune-gui
	cp README-RUN.md dist/pkg/RUN.md
	(cd dist/pkg && tar czf "../$PKG.tar.gz" zune-gui RUN.md)
}
[ -f "dist/zune-gui-glow-$TARGET" ] && {
	rm -rf dist/pkg && mkdir -p dist/pkg
	cp "dist/zune-gui-glow-$TARGET" dist/pkg/zune-gui
	cp README-RUN.md dist/pkg/RUN.md
	(cd dist/pkg && tar czf "../$PKG-glow.tar.gz" zune-gui RUN.md)
}
rm -rf dist/pkg

echo "-> dist/:"
ls -l dist
