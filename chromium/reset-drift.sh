#!/usr/bin/env bash
# DEPS-drift guard. gclient sync and the build incidentally bump DEPS-managed
# submodule pointers (e.g. third_party/dawn) in the working tree. Left alone
# they show up in `git status` and get mistaken for fork work — which is exactly
# how a stray dawn bump nearly got committed. This resets every drifted gitlink
# to what the checkout committed, EXCEPT the Mersey-managed devtools-frontend
# pointer (the overlay/patch own that one).
#
# Usage:  chromium/reset-drift.sh [CHROMIUM_SRC]
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$HERE/.." && pwd)"
SRC="${1:-$(cd "$REPO/.." && pwd)/browsers/chromium/src}"

[ -d "$SRC/.git" ] || [ -f "$SRC/.git" ] || { echo "not a git checkout: $SRC" >&2; exit 1; }

reset=0
while IFS= read -r path; do
  [ -n "$path" ] || continue
  # Mersey owns the devtools-frontend pointer; never touch it.
  case "$path" in third_party/devtools-frontend*) continue ;; esac
  # Only gitlinks (submodule pointers, mode 160000) — leave real file edits alone.
  if git -C "$SRC" ls-files -s -- "$path" 2>/dev/null | grep -q '^160000'; then
    git -C "$SRC" checkout -- "$path"
    echo "reset drift: $path"
    reset=$((reset + 1))
  fi
done < <(git -C "$SRC" status --porcelain 2>/dev/null | awk '$1=="M"{print $2}')

echo "drift guard: $reset submodule pointer(s) reset"
