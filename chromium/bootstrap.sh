#!/usr/bin/env bash
# From a clean machine to a built Mersey-in-Chromium fork — using only this
# repo's overlay. No full Chromium source lives here; this fetches upstream at
# chromium/BASELINE, lays the ~40-file Mersey overlay on top, and builds.
#
# The heavy step is `gclient sync` (tens of minutes, ~29 GB, from googlesource —
# which handles the shallow/large-file history that GitHub cannot). Everything
# else is fast. Safe to re-run: an existing checkout already at BASELINE is not
# re-synced.
#
# Usage:  chromium/bootstrap.sh [CHROMIUM_DIR]
#   CHROMIUM_DIR  where the gclient checkout lives (default: ../../chromium,
#                 i.e. a sibling of this repo — the layout the build scripts use)
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$HERE/.." && pwd)"
DEST="${1:-$(cd "$REPO/.." && pwd)/chromium}"
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
  ( cd "$DEST"
    [ -f .gclient ] || gclient config --name src --unmanaged https://chromium.googlesource.com/chromium/src.git
    # -r pins src to the baseline; --no-history keeps it shallow.
    gclient sync -r "src@$CR_REV" --no-history --shallow --reset --force
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
