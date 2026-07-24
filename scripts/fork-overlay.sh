#!/usr/bin/env bash
# One overlay model for the git-based browser forks (servo, ladybird, firefox),
# mirroring chromium/ but shared. Each fork keeps its Mersey delta in this repo
# as <fork>/overlay/ (snapshots) + <fork>/BASELINE (upstream url/rev and the
# commands to regenerate artifacts and build). The full browser source is never
# stored here; the fork is reconstructed on top of pinned upstream.
#
# Chromium stays bespoke (its gclient monorepo can't be a plain git clone).
#
# BASELINE format (one "key value" per line; # comments; blank lines ok):
#   upstream  <git-url>              # upstream repo to clone
#   revision  <40-hex>              # exact upstream commit the overlay applies onto
#   regen     <shell>               # run in SRC after apply (repeatable) — regenerate
#                                    #   artifacts NOT stored here (vendored crates, bridge.js…)
#   build     <shell>               # run in SRC by bootstrap (repeatable)
#   $MERSEY_REPO and $SRC are exported for regen/build commands.
#
# Usage:
#   scripts/fork-overlay.sh apply     <fork> [SRC]
#   scripts/fork-overlay.sh verify    <fork> [SRC]     # integrity; +reconstruct if SRC given
#   scripts/fork-overlay.sh bootstrap <fork> [DEST]    # clone upstream@rev, apply, build
set -uo pipefail

CMD="${1:-}"; FORK="${2:-}"
[ -n "$CMD" ] && [ -n "$FORK" ] || { echo "usage: fork-overlay.sh <apply|verify|bootstrap> <fork> [dir]" >&2; exit 2; }

MERSEY_REPO="$(cd "$(dirname "$0")/.." && pwd)"; export MERSEY_REPO
FDIR="$MERSEY_REPO/$FORK"
BASE="$FDIR/BASELINE"
[ -f "$BASE" ] || { echo "no $FORK/BASELINE" >&2; exit 1; }

kv() { awk -v k="$1" '$1==k{$1="";sub(/^ /,"");print}' "$BASE"; }        # all values for key
first() { kv "$1" | head -1; }
UPSTREAM="$(first upstream)"; REVISION="$(first revision)"

run_hooks() { # $1=key
  while IFS= read -r c; do
    [ -n "$c" ] || continue
    echo "  + $c"
    ( cd "$SRC" && eval "$c" ) || { echo "hook failed: $c" >&2; return 1; }
  done < <(kv "$1")
}

apply() {
  SRC="${1:-$(cd "$MERSEY_REPO/.." && pwd)/browsers/$FORK}"; export SRC
  [ -d "$SRC" ] || { echo "no checkout at $SRC" >&2; exit 1; }
  want="$REVISION"; have="$(git -C "$SRC" rev-parse HEAD 2>/dev/null || echo '?')"
  [ "$have" = "$want" ] || echo "warn: $SRC at ${have:0:12}, BASELINE pins ${want:0:12}"
  n=0
  while IFS= read -r -d '' f; do
    rel="${f#"$FDIR"/overlay/}"; mkdir -p "$SRC/$(dirname "$rel")"; cp "$f" "$SRC/$rel"; n=$((n+1))
  done < <(find "$FDIR/overlay" -type f -print0)
  echo "$FORK: $n overlay files -> $SRC"
  run_hooks regen || exit 1
  echo "$FORK: applied."
}

verify() {
  local fail=0 SRC="${1:-}"
  ok() { printf '  ok    %s\n' "$1"; }
  bad() { printf '  FAIL  %s\n' "$1"; fail=1; }
  echo "verify $FORK overlay"
  printf '%s' "$REVISION" | grep -qE '^[0-9a-f]{40}$' && ok "revision ${REVISION:0:12}" || bad "revision not 40-hex"
  [ -n "$UPSTREAM" ] && ok "upstream $UPSTREAM" || bad "no upstream url"
  if [ -d "$FDIR/overlay" ]; then
    ok "overlay has $(find "$FDIR/overlay" -type f | wc -l | tr -d ' ') files"
    local b=0
    while IFS= read -r -d '' f; do
      [ "$(file --mime-encoding -b "$f" 2>/dev/null)" = binary ] && { bad "binary in overlay: ${f#"$FDIR"/}"; b=1; }
    done < <(find "$FDIR/overlay" -type f -print0)
    [ "$b" = 0 ] && ok "no binaries"
  else bad "overlay/ missing"; fi
  if [ -n "$SRC" ] && [ -d "$SRC/.git" ]; then
    echo "  -- completeness: every overlay file must match the checkout (the fork) --"
    # No regen here: the stored overlay files are compared as-is. Non-empty = drift/miss.
    local d; d="$(cd "$FDIR/overlay" && find . -type f | while read -r f; do
      cmp -s "$f" "$SRC/${f#./}" || echo "${f#./}"; done)"
    [ -z "$d" ] && ok "overlay matches checkout (complete)" || { bad "overlay differs from checkout:"; echo "$d" | sed 's/^/        /'; }
  fi
  echo; [ "$fail" = 0 ] && echo "PASS $FORK" || echo "FAIL $FORK"
  return "$fail"
}

bootstrap() {
  local DEST="${1:-$(cd "$MERSEY_REPO/.." && pwd)/browsers/$FORK}"; SRC="$DEST"; export SRC
  echo "=== $FORK: clone $UPSTREAM @ ${REVISION:0:12} ==="
  if [ "$(git -C "$SRC" rev-parse HEAD 2>/dev/null || echo none)" = "$REVISION" ]; then
    echo "already at BASELINE"
  else
    [ -d "$SRC/.git" ] || git clone "$UPSTREAM" "$SRC"
    git -C "$SRC" fetch origin "$REVISION" 2>/dev/null || git -C "$SRC" fetch origin
    git -C "$SRC" checkout -q "$REVISION"
  fi
  echo "=== apply overlay ==="; apply "$SRC"
  echo "=== build ==="; run_hooks build || exit 1
  echo "=== done: $FORK reconstructed at $SRC ==="
}

case "$CMD" in
  apply)     apply "${3:-}";;
  verify)    verify "${3:-}";;
  bootstrap) bootstrap "${3:-}";;
  *) echo "unknown command: $CMD" >&2; exit 2;;
esac
