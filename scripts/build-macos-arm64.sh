#!/usr/bin/env bash
# Build the Mersey browser forks on macOS arm64 (Apple silicon).
#
# One driver for all four Stage-B hosts plus the C-ABI staticlib they share.
# It applies this repo's fork glue (the idempotent apply.sh scripts) and then
# builds each fork with its native toolchain. Unlike the Linux-arm64 recipes
# (chromium/setup-arm64-host.sh, chromium/args.arm64.gn), macOS needs no
# toolchain archaeology: the hermetic toolchains work and only target_os /
# target_cpu change (see chromium/README.md).
#
# The fork checkouts are NOT in this repo — they live BESIDE it (siblings of
# this repo dir, i.e. under ~/Work/mersey/ when the repo is ~/Work/mersey/mersey):
#     ../gecko          (Firefox fork, branch dom/mersey)   [build only]
#     ../chromium/src   (Blink fork,   branch mersey)
#     ../servo-src      (Servo fork)
#     ../ladybird       (Ladybird fork)
# Override any of them with the same env vars the runners use:
#     GECKO_SRC  CHROMIUM_SRC  SERVO_SRC  LADYBIRD_SRC
#
# Usage:
#     scripts/build-macos-arm64.sh [all|staticlib|gecko|chromium|servo|ladybird ...]
#   e.g.
#     scripts/build-macos-arm64.sh              # everything (staticlib first)
#     scripts/build-macos-arm64.sh ladybird     # just the staticlib + Ladybird
#     SERVO_SRC=~/src/servo scripts/build-macos-arm64.sh servo
#
# Per-fork failures are reported but do NOT abort the other forks; a summary
# prints at the end and the exit code is non-zero if anything failed.
set -uo pipefail

# ---------------------------------------------------------------------------
# Locations
# ---------------------------------------------------------------------------
HERE="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$HERE/.." && pwd)"                 # the mersey repo (…/mersey/mersey)
WORK="$(cd "$REPO/.." && pwd)"                 # …/mersey  → the forks are siblings here

GECKO_SRC="${GECKO_SRC:-$WORK/gecko}"
CHROMIUM_SRC="${CHROMIUM_SRC:-$WORK/chromium/src}"
SERVO_SRC="${SERVO_SRC:-$WORK/servo-src}"
LADYBIRD_SRC="${LADYBIRD_SRC:-$WORK/ladybird}"

STATICLIB="$REPO/target/release/libmersey_capi.a"
CHROMIUM_OUT="${CHROMIUM_OUT:-out/mersey-arm64}"   # relative to CHROMIUM_SRC
JOBS="${JOBS:-$(sysctl -n hw.ncpu 2>/dev/null || echo 4)}"

# ---------------------------------------------------------------------------
# Pretty output + failure tracking
# ---------------------------------------------------------------------------
if [ -t 1 ]; then B=$'\033[1m'; G=$'\033[32m'; Y=$'\033[33m'; R=$'\033[31m'; Z=$'\033[0m'
else B=""; G=""; Y=""; R=""; Z=""; fi
say()  { printf '%s\n' "${B}==> $*${Z}"; }
ok()   { printf '%s\n' "${G}    ok: $*${Z}"; }
warn() { printf '%s\n' "${Y}    warn: $*${Z}"; }
err()  { printf '%s\n' "${R}    FAIL: $*${Z}" >&2; }

FAILED=""
SKIPPED=""
BUILT=""
fail()  { err "$2"; FAILED="$FAILED $1"; }
skip()  { warn "$2"; SKIPPED="$SKIPPED $1"; }
done_() { ok "$2"; BUILT="$BUILT $1"; }

# ---------------------------------------------------------------------------
# Preflight: this script only makes sense on macOS arm64
# ---------------------------------------------------------------------------
preflight() {
  local os arch
  os="$(uname -s)"; arch="$(uname -m)"
  if [ "$os" != "Darwin" ]; then
    err "this is a macOS build script; host is $os. For Linux-arm64 see chromium/README.md."
    exit 2
  fi
  if [ "$arch" != "arm64" ]; then
    warn "host arch is $arch, not arm64 — building for the native arch, not cross."
  fi
  command -v cargo >/dev/null 2>&1 || { err "cargo not on PATH (install rustup)"; exit 2; }
  say "macOS $(sw_vers -productVersion 2>/dev/null) on $arch, $JOBS jobs"
  say "repo:     $REPO"
  say "forks:    gecko=$GECKO_SRC"
  printf '          chromium=%s\n          servo=%s\n          ladybird=%s\n' \
         "$CHROMIUM_SRC" "$SERVO_SRC" "$LADYBIRD_SRC"
}

# ---------------------------------------------------------------------------
# 0. The C-ABI staticlib — Ladybird and Chromium link this prebuilt; Gecko and
#    Servo build the crate themselves, but building it here also populates the
#    cargo cache and is a fast smoke test of the engine on this host.
# ---------------------------------------------------------------------------
build_staticlib() {
  say "staticlib: cargo build --release -p mersey_capi"
  if ( cd "$REPO" && cargo build --release -p mersey_capi ); then
    [ -f "$STATICLIB" ] && done_ staticlib "libmersey_capi.a -> $STATICLIB" \
                        || fail staticlib "cargo succeeded but $STATICLIB is missing"
  else
    fail staticlib "cargo build -p mersey_capi failed"
  fi
}

# ---------------------------------------------------------------------------
# Gecko / Firefox fork — no glue to apply (committed in its own checkout);
# just build. The objdir must be obj-mersey so bench/web/run-native.mjs finds
# dist/bin/firefox. A mozconfig is written only if the checkout lacks one.
# ---------------------------------------------------------------------------
build_gecko() {
  say "gecko: Firefox fork"
  if [ ! -f "$GECKO_SRC/mach" ]; then
    skip gecko "no ./mach at $GECKO_SRC — set GECKO_SRC or clone the fork"; return
  fi
  local mozconfig="$GECKO_SRC/mozconfig"
  if [ ! -f "$mozconfig" ]; then
    warn "no mozconfig — writing a release one with MOZ_OBJDIR=@TOPSRCDIR@/obj-mersey"
    cat > "$mozconfig" <<'EOF'
# Written by mersey scripts/build-macos-arm64.sh. Edit freely.
mk_add_options MOZ_OBJDIR=@TOPSRCDIR@/obj-mersey
ac_add_options --enable-application=browser
ac_add_options --disable-debug
ac_add_options --enable-release
EOF
  else
    ok "using existing mozconfig (objdir must be obj-mersey for the bench runner)"
  fi
  warn "if this is a fresh checkout, run '(cd $GECKO_SRC && ./mach bootstrap)' once (interactive)"
  if ( cd "$GECKO_SRC" && ./mach build ); then
    if [ -x "$GECKO_SRC/obj-mersey/dist/bin/firefox" ]; then
      done_ gecko "firefox -> $GECKO_SRC/obj-mersey/dist/bin/firefox"
    else
      done_ gecko "built (binary under obj-mersey/dist/bin, or Nightly.app on mac)"
    fi
  else
    fail gecko "./mach build failed (a fresh tree usually needs ./mach bootstrap first)"
  fi
}

# ---------------------------------------------------------------------------
# Chromium / Blink fork — prebuilt staticlib refreshed into //third_party/mersey,
# then a plain mac/arm64 gn build. None of the Linux-arm64 toolchain overrides
# apply here (chromium/README.md): only target_os/target_cpu differ.
# ---------------------------------------------------------------------------
build_chromium() {
  say "chromium: Blink fork"
  if [ ! -d "$CHROMIUM_SRC" ]; then
    skip chromium "no checkout at $CHROMIUM_SRC — set CHROMIUM_SRC"; return
  fi
  command -v gn >/dev/null 2>&1 && command -v autoninja >/dev/null 2>&1 || {
    skip chromium "gn/autoninja not on PATH — add depot_tools to PATH"; return; }
  [ -f "$STATICLIB" ] || { fail chromium "staticlib missing — build 'staticlib' first"; return; }

  # Refresh the engine into the fork. The fork's refresh.sh is Linux-only (it
  # cross-links a .so against the bullseye sysroot); on macOS the fork links the
  # prebuilt *staticlib*, so drop it into lib/ where third_party/mersey/BUILD.gn's
  # is_mac branch reads it, and refresh the header alongside.
  local tpm="$CHROMIUM_SRC/third_party/mersey"
  if [ -d "$tpm" ]; then
    mkdir -p "$tpm/lib" "$tpm/include"
    cp "$STATICLIB" "$tpm/lib/libmersey_capi.a" && ok "copied libmersey_capi.a -> lib/"
    cp "$REPO/crates/mersey_capi/include/mersey.h" "$tpm/include/mersey.h" \
      && ok "copied mersey.h"
  else
    warn "no third_party/mersey in the checkout — is this the 'mersey' branch?"
  fi

  # Write mac/arm64 gn args (component build for fast links; no reclient).
  local outdir="$CHROMIUM_SRC/$CHROMIUM_OUT"
  mkdir -p "$outdir"
  cat > "$outdir/args.gn" <<'EOF'
# Mersey fork — macOS arm64. Hermetic toolchain works here; only os/cpu change.
target_os = "mac"
target_cpu = "arm64"
is_debug = false
is_component_build = true
symbol_level = 0
blink_symbol_level = 0
enable_nacl = false
use_remoteexec = false
EOF
  say "chromium: gn gen $CHROMIUM_OUT && autoninja chrome (-j$JOBS)"
  if ( cd "$CHROMIUM_SRC" && gn gen "$CHROMIUM_OUT" && autoninja -j"$JOBS" -C "$CHROMIUM_OUT" chrome ); then
    done_ chromium "app -> $outdir/Chromium.app (bench runner expects '$CHROMIUM_OUT/chrome'; symlink if needed)"
  else
    fail chromium "gn/autoninja build failed"
  fi
}

# ---------------------------------------------------------------------------
# Servo fork — the engine is a Cargo crate (path dep). Apply the glue, vendor
# the engine's crates only if this is a vendored-source tree, then mach build.
# ---------------------------------------------------------------------------
build_servo() {
  say "servo: Servo fork"
  if [ ! -f "$SERVO_SRC/mach" ]; then
    skip servo "no ./mach at $SERVO_SRC — set SERVO_SRC"; return
  fi
  say "servo: apply.sh"
  "$REPO/servo/apply.sh" "$SERVO_SRC" "$REPO" || { fail servo "apply.sh failed"; return; }
  if [ -d "$SERVO_SRC/vendor" ]; then
    say "servo: vendor-deps.sh (vendored-source tree)"
    "$REPO/servo/vendor-deps.sh" "$SERVO_SRC" "$REPO" || warn "vendor-deps.sh failed"
  else
    ok "git checkout (no vendor/ dir) — cargo resolves the engine crate from crates.io + the path dep"
  fi
  say "servo: ./mach build --release (-j$JOBS)"
  if ( cd "$SERVO_SRC" && ./mach build --release -j "$JOBS" ); then
    [ -x "$SERVO_SRC/target/release/servoshell" ] \
      && done_ servo "servoshell -> $SERVO_SRC/target/release/servoshell" \
      || done_ servo "built (see target/release/servoshell)"
  else
    fail servo "./mach build --release failed (a fresh tree may need ./mach bootstrap)"
  fi
}

# ---------------------------------------------------------------------------
# Ladybird fork — prebuilt staticlib linked into LibWeb. Apply the glue (points
# CMake at the staticlib), then build the test-web headless runner. On macOS
# with Qt6/vcpkg present none of the Linux workarounds are needed.
# ---------------------------------------------------------------------------
build_ladybird() {
  say "ladybird: Ladybird fork"
  if [ ! -f "$LADYBIRD_SRC/Meta/ladybird.py" ]; then
    skip ladybird "no Meta/ladybird.py at $LADYBIRD_SRC — set LADYBIRD_SRC"; return
  fi
  [ -f "$STATICLIB" ] || { fail ladybird "staticlib missing — build 'staticlib' first"; return; }
  say "ladybird: apply.sh (MERSEY_STATICLIB=$STATICLIB)"
  MERSEY_STATICLIB="$STATICLIB" "$REPO/ladybird/apply.sh" "$LADYBIRD_SRC" "$REPO" \
    || { fail ladybird "apply.sh failed"; return; }
  say "ladybird: ./Meta/ladybird.py build test-web"
  if ( cd "$LADYBIRD_SRC" && ./Meta/ladybird.py build test-web ); then
    if [ -x "$LADYBIRD_SRC/Build/release/bin/test-web" ]; then
      done_ ladybird "test-web -> $LADYBIRD_SRC/Build/release/bin/test-web"
    else
      done_ ladybird "built (see Build/release/bin/test-web)"
    fi
  else
    fail ladybird "ladybird.py build test-web failed"
  fi
}

# ---------------------------------------------------------------------------
# Driver
# ---------------------------------------------------------------------------
preflight

TARGETS=("$@")
[ ${#TARGETS[@]} -eq 0 ] && TARGETS=(all)

want() {
  for t in "${TARGETS[@]}"; do
    [ "$t" = all ] && return 0
    [ "$t" = "$1" ] && return 0
  done
  return 1
}

# staticlib is a prerequisite for chromium+ladybird; build it if it's wanted,
# or if a fork that links it is requested and it isn't present yet.
needs_staticlib=false
if want staticlib || ( ( want chromium || want ladybird ) && [ ! -f "$STATICLIB" ] ); then
  needs_staticlib=true
fi
$needs_staticlib && build_staticlib

want gecko    && build_gecko
want chromium && build_chromium
want servo    && build_servo
want ladybird && build_ladybird

# ---------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------
echo
say "summary"
[ -n "$BUILT" ]   && ok   "built:  ${BUILT# }"
[ -n "$SKIPPED" ] && warn "skipped:${SKIPPED}"
[ -n "$FAILED" ]  && err  "failed:${FAILED}"
if [ -n "$FAILED" ]; then
  echo
  say "next: fix the failures above, then re-run just those forks, e.g."
  printf '      scripts/build-macos-arm64.sh%s\n' "$FAILED"
  exit 1
fi
say "done. measure from the repo with bench/web/run-native{,-chromium,-servo,-ladybird}.mjs"
