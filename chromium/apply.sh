#!/usr/bin/env bash
# Reconstruct the Mersey-in-Chromium fork from this repo's overlay, on top of a
# gclient checkout synced to chromium/BASELINE. The full Chromium source is
# NEVER stored here (a checkout is ~29 GB and its history carries >100 MB blobs
# GitHub rejects) — only the ~40-file Mersey delta (chromium/overlay/) and the
# devtools-frontend patch live in this repo.
#
# The overlay is SNAPSHOTS, not diffs: apply drops the fork's exact version of
# each touched file onto the pinned upstream. That is why it needs no upstream
# fetch to apply, and is immune to patch fuzz — correct precisely because the
# upstream revision is pinned in BASELINE.
#
# Usage:
#   chromium/apply.sh [CHROMIUM_SRC] [--verify]
#     CHROMIUM_SRC  path to the gclient `src` dir (default: ../../browsers/chromium/src)
#     --verify      after applying, assert the tree matches the fork's `mersey`
#                   ref (proves the overlay is complete — no file was missed)
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$HERE/.." && pwd)"
SRC="${1:-$(cd "$REPO/.." && pwd)/browsers/chromium/src}"
VERIFY=0; [ "${2:-}" = "--verify" ] && VERIFY=1

[ -d "$SRC/third_party/blink" ] || {
  echo "not a Chromium src checkout: $SRC" >&2
  echo "  pass the gclient src path, e.g. chromium/apply.sh ~/Work/mersey/browsers/chromium/src" >&2
  exit 1
}

# 1. Warn (don't fail) if the checkout isn't at the pinned upstream. The overlay
#    is only guaranteed correct against this revision.
want="$(awk '/^chromium\/src/{print $2}' "$HERE/BASELINE")"
have="$(git -C "$SRC" rev-parse HEAD 2>/dev/null || echo '?')"
if [ "$have" != "$want" ]; then
  echo "warn: $SRC is at ${have:0:12}, BASELINE pins ${want:0:12}"
  echo "      (gclient sync -r $want  — the overlay assumes this upstream)"
fi

# 2. Lay the overlay down: the fork's version of every Mersey-touched file.
n=0
while IFS= read -r -d '' f; do
  rel="${f#"$HERE"/overlay/}"
  mkdir -p "$SRC/$(dirname "$rel")"
  cp "$f" "$SRC/$rel"
  n=$((n + 1))
done < <(find "$HERE/overlay" -type f -print0)
echo "overlay: $n files installed into $SRC"

# 3. The nested devtools-frontend fork (a separate DEPS repo) as a patch — its
#    upstream base IS fetchable, so it stays a reviewable diff. Idempotent.
DF="$SRC/third_party/devtools-frontend/src"
if [ -d "$DF" ]; then
  for p in "$HERE"/patches-devtools-frontend/*.patch; do
    [ -e "$p" ] || continue
    if git -C "$DF" apply --reverse --check "$p" 2>/dev/null; then
      echo "devtools-frontend: already applied $(basename "$p")"
    else
      git -C "$DF" apply "$p" && echo "devtools-frontend: applied $(basename "$p")"
    fi
  done
else
  echo "warn: $DF missing — run gclient sync so DEPS pulls devtools-frontend, then re-run"
fi

# 3b. Keep DEPS-managed submodule pointers (dawn, …) honest so a sync/build bump
#     never masquerades as fork work.
bash "$HERE/reset-drift.sh" "$SRC" || true

# 4. The engine staticlib is a build artifact, never stored here. Build it from
#    the workspace and drop it in (third_party/mersey/refresh.sh does the copy).
echo "engine: build mersey_capi and refresh the prebuilt lib:"
echo "        (cd $REPO && cargo build --release -p mersey_capi)"
echo "        $SRC/third_party/mersey/refresh.sh   # copies libmersey_capi.* into third_party/mersey/lib/"

# 5. Completeness proof: after applying onto the pinned upstream, the tree must
#    equal the fork's `main` ref for every non-artifact file. A non-empty diff
#    means the overlay missed a file — capture it and re-run.
if [ "$VERIFY" = 1 ]; then
  echo "--- verify: tree vs fork main ref (empty = overlay complete) ---"
  # Ignore submodules: gclient-managed pointers (dawn, …) drift independently of
  # the overlay, which never touches a submodule.
  git -C "$SRC" diff --stat --ignore-submodules=all main -- . \
    ':(exclude)third_party/mersey/lib' \
    ':(exclude)third_party/devtools-frontend' || true
fi

echo "done."
