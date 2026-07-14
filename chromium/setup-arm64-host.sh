#!/usr/bin/env bash
# Prepare an Ubuntu **arm64** host to build the Mersey Chromium fork.
#
# Chromium ships no hermetic toolchain for linux-arm64: every prebuilt in
# `third_party` is x86-64, and an arm64 host cannot run them. So the build has
# to be carried by native tools, and Chromium has to be told where they are.
# Each substitution below is a hard failure without it, not a preference — the
# error each one fixes is named.
#
# None of this is needed on x86-64 Linux, macOS or Windows, where the hermetic
# toolchain works and only `target_cpu`/`target_os` change. That is what makes
# the rest of the platform matrix a configuration exercise rather than another
# archaeology, and it is why this file exists rather than living in someone's
# shell history.
#
# Usage:   chromium/setup-arm64-host.sh [CHROMIUM_SRC]
#          (default CHROMIUM_SRC: ~/chromium/src)
set -euo pipefail

CHROMIUM_SRC="${1:-$HOME/chromium/src}"
OPT="$HOME/opt"
LLVM_VER=21
RUST_NIGHTLY=nightly-2026-06-15   # rustc 1.98 — see "why this exact one" below
NODE_VER=24.12.0                  # what third_party/node pins, exactly
GPERF_VER=3.1

say() { printf '\n\033[1m== %s\033[0m\n' "$*"; }

[ -d "$CHROMIUM_SRC" ] || { echo "no such checkout: $CHROMIUM_SRC"; exit 1; }

# ---------------------------------------------------------------------------
say "system packages"
# clang: third_party/llvm-build is x86-64 ("rosetta error: failed to open elf").
# libclang: bindgen loads it at run time.
# gperf: there is no linux-arm64 CIPD package for it *at all*.
sudo apt-get install -y \
  "clang-$LLVM_VER" "lld-$LLVM_VER" "libclang-$LLVM_VER-dev" \
  build-essential curl

# ---------------------------------------------------------------------------
say "clang shadow tree ($OPT/llvm$LLVM_VER)"
# Ubuntu puts the clang runtime at lib/linux/libclang_rt.builtins-aarch64.a;
# Chromium expects lib/<triple>/libclang_rt.builtins.a. Rather than write into
# /usr, shadow it: symlinks in the layout Chromium looks for.
SHADOW="$OPT/llvm$LLVM_VER"
mkdir -p "$SHADOW/bin" "$SHADOW/lib/clang/$LLVM_VER/lib/aarch64-unknown-linux-gnu"
for b in clang clang++ clang-cpp lld ld.lld llvm-ar llvm-nm llvm-objcopy \
         llvm-objdump llvm-ranlib llvm-strip llvm-readelf; do
  [ -e "/usr/lib/llvm-$LLVM_VER/bin/$b" ] && ln -sf "/usr/lib/llvm-$LLVM_VER/bin/$b" "$SHADOW/bin/$b"
done
for f in /usr/lib/clang/$LLVM_VER/lib/linux/libclang_rt.*-aarch64* \
         /usr/lib/clang/$LLVM_VER/lib/linux/clang_rt.crt*-aarch64.o; do
  [ -e "$f" ] || continue
  ln -sf "$f" "$SHADOW/lib/clang/$LLVM_VER/lib/aarch64-unknown-linux-gnu/$(basename "$f" | sed 's/-aarch64//')"
done
ln -sfn "/usr/lib/clang/$LLVM_VER/include" "$SHADOW/lib/clang/$LLVM_VER/include"

# ---------------------------------------------------------------------------
say "rust $RUST_NIGHTLY (rustc 1.98)"
# The version matters, and not for taste. Chromium's own rustc is 1.98; a newer
# nightly mangles the allocator symbols (`__rustc::__rust_alloc`) differently
# from the std it ships, and *nothing links*.
rustup toolchain install "$RUST_NIGHTLY" --profile minimal
rustup component add rustfmt --toolchain "$RUST_NIGHTLY"
command -v bindgen >/dev/null || cargo install bindgen-cli --locked

BG="$OPT/rust-bindgen"
mkdir -p "$BG/bin" "$BG/lib"
ln -sf "$HOME/.cargo/bin/bindgen" "$BG/bin/bindgen"
ln -sf "$HOME/.rustup/toolchains/$RUST_NIGHTLY-aarch64-unknown-linux-gnu/bin/rustfmt" "$BG/bin/rustfmt"
# A real copy, not a symlink: bindgen's loader will not follow one.
cp -L "/usr/lib/llvm-$LLVM_VER/lib/libclang-$LLVM_VER.so.$LLVM_VER" "$BG/lib/libclang.so"

# ---------------------------------------------------------------------------
say "node v$NODE_VER"
# third_party/node's version check is exact, and its binary is x86-64.
if [ -s "$HOME/.nvm/nvm.sh" ]; then
  # shellcheck disable=SC1091
  . "$HOME/.nvm/nvm.sh"
  nvm install "$NODE_VER"
  ln -sf "$(nvm which "$NODE_VER")" "$CHROMIUM_SRC/third_party/node/linux/node-linux-x64/bin/node"
else
  echo "  !! nvm not found — install node v$NODE_VER yourself and symlink it to"
  echo "     $CHROMIUM_SRC/third_party/node/linux/node-linux-x64/bin/node"
fi

# ---------------------------------------------------------------------------
say "gperf $GPERF_VER (built from source)"
# DEPS carries `host_cpu != "arm64"` on the CIPD package because there is no
# linux-arm64 build of it to fetch.
GPERF_BIN="$CHROMIUM_SRC/third_party/gperf/cipd/bin/gperf"
if [ ! -x "$GPERF_BIN" ]; then
  tmp="$(mktemp -d)"
  ( cd "$tmp"
    curl -sLO "https://ftp.gnu.org/gnu/gperf/gperf-$GPERF_VER.tar.gz"
    tar xzf "gperf-$GPERF_VER.tar.gz"
    cd "gperf-$GPERF_VER"
    ./configure --quiet
    make -s -j"$(nproc)" )
  mkdir -p "$(dirname "$GPERF_BIN")"
  cp "$tmp/gperf-$GPERF_VER/src/gperf" "$GPERF_BIN"
  rm -rf "$tmp"
fi
"$GPERF_BIN" --version | head -1

# ---------------------------------------------------------------------------
say "done"
cat <<EOF
Now configure the build:

  cp $(dirname "$0")/args.arm64.gn $CHROMIUM_SRC/out/mersey-arm64/args.gn
  cd $CHROMIUM_SRC && gn gen out/mersey-arm64
  autoninja -C out/mersey-arm64 libblink_core.so

Paths in args.arm64.gn assume \$HOME=$HOME; edit if that is not true.
EOF
