#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Kirk D. Brown

# Cross-platform build entry point for the Mersey project.
#
# Detects the host OS + arch and builds the requested targets. The engine, CLI,
# C-ABI staticlib and the WASM build are portable cargo builds that work on every
# platform (Linux / macOS / Windows × arm64 / x86_64) — built NATIVELY on
# whatever host runs this (that's how the CI runners in phase 3 validate the
# whole matrix). The browser forks dispatch to a per-platform recipe,
# scripts/build-<os>-<arch>.sh (macOS arm64 exists today; the rest land with the
# CI runners).
#
# Usage:  scripts/build.sh [target ...]
#   engine     the `mersey` CLI + libmersey_capi staticlib/dylib   (default)
#   wasm       the mersey_wasm engine (Stage-A polyfill) -> web/
#   gecko | chromium | servo | ladybird   a browser fork (per-platform recipe)
#   all        engine + wasm + every fork with a recipe on this platform
set -euo pipefail

REPO="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO"

# ---- platform detection ----------------------------------------------------
case "$(uname -s)" in
  Darwin) OS=macos ;;
  Linux)  OS=linux ;;
  MINGW*|MSYS*|CYGWIN*|Windows_NT) OS=windows ;;
  *) echo "unsupported OS: $(uname -s)" >&2; exit 1 ;;
esac
case "$(uname -m)" in
  arm64|aarch64) ARCH=arm64 ;;
  x86_64|amd64)  ARCH=x64 ;;
  *) echo "unsupported arch: $(uname -m)" >&2; exit 1 ;;
esac
PLATFORM="$OS-$ARCH"
case "$OS" in
  macos)   LIBEXT=a;   DYLIB=dylib; BINEXT="" ;;
  linux)   LIBEXT=a;   DYLIB=so;    BINEXT="" ;;
  windows) LIBEXT=lib; DYLIB=dll;   BINEXT=".exe" ;;
esac

say() { printf '\n==> %s\n' "$*"; }
say "platform: $PLATFORM   (rustc $(rustc --version 2>/dev/null | cut -d' ' -f2))"

# ---- portable targets (native cargo — every platform) ----------------------
build_engine() {
  # Build the CLI and the C-ABI staticlib specifically — NOT the whole
  # workspace: mersey_wasm is a wasm32 cdylib that imports host hooks and cannot
  # link on a native target (it is built separately by `wasm`).
  say "engine: cargo build --release -p mersey_cli -p mersey_capi"
  cargo build --release -p mersey_cli -p mersey_capi
  echo "    cli:       target/release/mersey$BINEXT"
  echo "    staticlib: target/release/libmersey_capi.$LIBEXT (and .$DYLIB)"
}

build_wasm() {
  say "wasm: mersey_wasm (wasm32-unknown-unknown) -> web/mersey_wasm.wasm"
  rustup target add wasm32-unknown-unknown >/dev/null 2>&1 || true
  cargo build -p mersey_wasm --target wasm32-unknown-unknown --release
  cp target/wasm32-unknown-unknown/release/mersey_wasm.wasm web/mersey_wasm.wasm
}

# ---- browser forks (per-platform recipe) -----------------------------------
build_browser() { # $1 = fork
  local fork="$1" recipe="$REPO/scripts/build-$PLATFORM.sh"
  if [ -x "$recipe" ]; then
    "$recipe" "$fork"
  else
    say "$fork: no build recipe for $PLATFORM yet (scripts/build-$PLATFORM.sh)"
    echo "    The browser-fork build matrix is filled in and validated by the CI"
    echo "    runners (phase 3). To reconstruct + build the checkout by hand:"
    if [ "$fork" = chromium ]; then
      echo "      chromium/bootstrap.sh              # sync upstream + apply overlay + build"
    else
      echo "      scripts/fork-overlay.sh bootstrap $fork"
    fi
  fi
}

# ---- dispatch --------------------------------------------------------------
[ "$#" -gt 0 ] || set -- engine
for t in "$@"; do
  case "$t" in
    engine)   build_engine ;;
    wasm)     build_wasm ;;
    gecko|chromium|servo|ladybird) build_browser "$t" ;;
    all)      build_engine; build_wasm
              for f in gecko chromium servo ladybird; do build_browser "$f"; done ;;
    *) echo "unknown target: $t   (engine|wasm|gecko|chromium|servo|ladybird|all)" >&2; exit 2 ;;
  esac
done
say "done: $PLATFORM"
