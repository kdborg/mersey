#!/usr/bin/env bash
# Rebuild the Mersey engine from its repo and refresh the prebuilt + header.
# Prebuilt-per-host-arch is the *initial fork* wiring; the follow-up is a
# GN rust_static_library build of the crate graph inside Chromium.
set -euo pipefail
MERSEY_REPO="${MERSEY_REPO:-$HOME/Work/mersey}"
HERE="$(cd "$(dirname "$0")" && pwd)"
# Two things the engine must agree with Chromium about, or the link fails:
#
#   the SYSROOT — Chromium links against its own bullseye sysroot (glibc 2.31).
#   A .so built against the host's newer glibc references symbols the sysroot
#   has never heard of (pthread_setspecific@GLIBC_2.34), and lld refuses it.
#
#   a SONAME — so the DT_NEEDED the linker records is a plain name that $ORIGIN
#   can resolve, not the build-time path.
CHROMIUM_SRC="${CHROMIUM_SRC:-$(cd "$HERE/../.." && pwd)}"
SYSROOT="$CHROMIUM_SRC/build/linux/debian_bullseye_arm64-sysroot"
CLANG="${CLANG:-$HOME/opt/llvm21/bin/clang}"

(cd "$MERSEY_REPO" && RUSTFLAGS="\
  -C linker=$CLANG \
  -C link-arg=--target=aarch64-linux-gnu \
  -C link-arg=--sysroot=$SYSROOT \
  -C link-arg=-fuse-ld=lld \
  -C link-arg=-Wl,-soname,libmersey_capi.so" \
   cargo build --release -p mersey_capi)
cp "$MERSEY_REPO/crates/mersey_capi/include/mersey.h" "$HERE/include/"
cp "$MERSEY_REPO/target/release/libmersey_capi.so" "$HERE/lib/"
echo "refreshed from $MERSEY_REPO"
