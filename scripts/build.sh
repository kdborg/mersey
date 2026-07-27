#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Kirk D. Brown

# Cross-platform build entry point for the Mersey project.
#
# Detects the host OS + arch and builds the requested targets. The engine, CLI,
# C-ABI staticlib and the WASM build are portable cargo builds that work on every
# platform (Linux / macOS / Windows × arm64 / x86_64) — built NATIVELY on
# whatever host runs this (that's how the CI runners in phase 3 validate the
# whole matrix). A browser fork builds via a per-platform fast-path recipe,
# scripts/build-<os>-<arch>.sh (macOS arm64 has one), or, on any other platform,
# via the portable fallback: reconstruct the overlay onto the pinned checkout
# and run the fork's own build (fork-overlay.sh bootstrap / chromium bootstrap).
# So `build.sh <fork>` works on Linux / macOS / Windows, not only where a
# bespoke recipe exists.
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

# ---- browser forks ---------------------------------------------------------
# A fork builds one of two ways. A platform may ship a fast-path recipe
# (scripts/build-<os>-<arch>.sh) that knows that platform's toolchain quirks —
# macOS arm64 has one. Where it does not, we fall back to the PORTABLE path:
# reconstruct the Mersey overlay onto the pinned upstream checkout and run the
# fork's OWN build commands (its BASELINE `build` hook, or chromium's
# bootstrap). Those commands were written by whoever set the fork up, so this
# works on any platform whose build dependencies are present — the same path the
# self-hosted CI runners (browsers.yml) take — without a per-platform recipe
# guessing at toolchains. `bootstrap` skips the (large) upstream clone when the
# checkout is already present at the pinned revision, so it is safe to re-run.
build_browser() { # $1 = fork
  local fork="$1" recipe="$REPO/scripts/build-$PLATFORM.sh"
  if [ -x "$recipe" ]; then
    "$recipe" "$fork"
  elif [ "$fork" = chromium ]; then
    say "$fork: no scripts/build-$PLATFORM.sh — reconstructing + building via chromium/bootstrap.sh"
    "$REPO/chromium/bootstrap.sh"
  else
    say "$fork: no scripts/build-$PLATFORM.sh — reconstructing + building via fork-overlay.sh"
    "$REPO/scripts/fork-overlay.sh" bootstrap "$fork"
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
