#!/usr/bin/env bash
# From a clean machine to a built Mersey-in-Chromium fork — using only this
# repo's overlay. No full Chromium source lives here; this fetches upstream at
# chromium/BASELINE, lays the ~40-file Mersey overlay on top, and builds.
#
# The heavy step is `gclient sync` — a FULL-history fetch from googlesource
# (budget ~60 GB of disk and tens of minutes). It must be full history, NOT
# shallow: googlesource rejects a shallow want-by-sha of an arbitrary historical
# revision (HTTP 500), and BASELINE pins exactly such a revision, so gclient
# fetches full history and checks the pin out from it locally. A shared
# GCLIENT_CACHE (git cache-dir) makes repeat/other forks cheap. Everything else
# is fast. Safe to re-run: a checkout already at BASELINE is not re-synced.
#
# Usage:  chromium/bootstrap.sh [CHROMIUM_DIR] [--no-build]
#   CHROMIUM_DIR  where the gclient checkout lives (default: ../../browsers/chromium,
#                 i.e. a sibling of this repo — the layout the build scripts use)
#   --no-build    reconstruct only (sync + overlay + staticlib); skip the
#                 hours-long compile
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$HERE/.." && pwd)"
DEST=""
NO_BUILD=0
for a in "$@"; do
  case "$a" in
    --no-build) NO_BUILD=1 ;;
    *) DEST="$a" ;;
  esac
done
[ -n "$DEST" ] || DEST="$(cd "$REPO/.." && pwd)/browsers/chromium"
SRC="$DEST/src"
CR_REV="$(awk '/^chromium\/src/{print $2}' "$HERE/BASELINE")"
[ -n "$CR_REV" ] || { echo "no chromium/src pin in BASELINE" >&2; exit 1; }

say() { printf '\n=== %s ===\n' "$1"; }

# 1. depot_tools on PATH (clone if absent).
say "depot_tools"
if ! command -v gclient >/dev/null 2>&1; then
  DT="${DEPOT_TOOLS:-$HOME/depot_tools}"
  [ -d "$DT" ] || git clone https://chromium.googlesource.com/chromium/tools/depot_tools.git "$DT"
  export PATH="$DT:$PATH"
fi
command -v gclient >/dev/null 2>&1 || { echo "depot_tools/gclient not found" >&2; exit 1; }
echo "gclient: $(command -v gclient)"

# 2. Fetch upstream at the pinned revision (idempotent).
say "upstream checkout @ ${CR_REV:0:12}"
have="$(git -C "$SRC" rev-parse HEAD 2>/dev/null || echo none)"
if [ "$have" = "$CR_REV" ]; then
  echo "already at BASELINE — skipping sync"
else
  mkdir -p "$DEST"
  # No arrays — this must run under macOS's stock bash 3.2.
  ( cd "$DEST"
    if [ ! -f .gclient ]; then
      if [ -n "${GCLIENT_CACHE:-}" ]; then
        gclient config --name src --unmanaged --cache-dir "$GCLIENT_CACHE" https://chromium.googlesource.com/chromium/src.git
      else
        gclient config --name src --unmanaged https://chromium.googlesource.com/chromium/src.git
      fi
    fi
    # Full history (see header): -r pins src to the baseline revision, gclient
    # checks it out from the fetched history. --nohooks when we won't build,
    # since hooks pull the (large) toolchains a compile needs.
    if [ "$NO_BUILD" = 1 ]; then
      gclient sync -r "src@$CR_REV" --reset --force --nohooks
    else
      gclient sync -r "src@$CR_REV" --reset --force
    fi
  )
fi

# 3. Reconstruct the fork: overlay + devtools patch, drift kept honest.
say "apply Mersey overlay"
bash "$HERE/apply.sh" "$SRC"
bash "$HERE/reset-drift.sh" "$SRC"

# 4. Build the engine staticlib the fork links (a build artifact, never stored).
say "engine staticlib"
( cd "$REPO" && cargo build --release -p mersey_capi )
[ -x "$SRC/third_party/mersey/refresh.sh" ] && "$SRC/third_party/mersey/refresh.sh" || \
  echo "note: refresh.sh missing/not-executable — the platform build script also refreshes it"

# 5. Build the fork with the platform toolchain.
if [ "$NO_BUILD" = 1 ]; then
  say "done (--no-build)"
  echo "fork reconstructed at $SRC — build with scripts/build-macos-arm64.sh chromium (or the Linux recipe)"
  exit 0
fi
say "build"
os="$(uname -s)"
if [ "$os" = "Darwin" ]; then
  CHROMIUM_SRC="$SRC" bash "$REPO/scripts/build-macos-arm64.sh" staticlib chromium
elif [ "$os" = "Linux" ] && [ "$(uname -m)" = "aarch64" ]; then
  echo "Linux arm64: run the toolchain prep, then gn/autoninja:"
  echo "  chromium/setup-arm64-host.sh $SRC"
  echo "  cp chromium/args.arm64.gn $SRC/out/mersey-arm64/args.gn && (cd $SRC && gn gen out/mersey-arm64 && autoninja -C out/mersey-arm64 chrome)"
else
  echo "$os/$(uname -m): hermetic toolchain works — gn gen out/mersey && autoninja -C out/mersey chrome (in $SRC)"
fi

say "done"
echo "fork reconstructed at $SRC"
