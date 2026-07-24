#!/usr/bin/env bash
# Overlay integrity check. Runs with NO Chromium checkout (CI-friendly): it
# guards against the overlay rotting — a binary sneaking in, a corrupt patch, a
# script that won't parse, a malformed BASELINE. With CHROMIUM_SRC pointing at a
# real checkout it additionally runs the full reconstruct proof (apply --verify).
#
# Usage:  chromium/verify.sh            # integrity only (CI)
#         CHROMIUM_SRC=~/…/src chromium/verify.sh   # + full reconstruct verify
set -uo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
fail=0
note() { printf '  %-6s %s\n' "$1" "$2"; }
bad() { note "FAIL" "$1"; fail=1; }
ok() { note "ok" "$1"; }

echo "chromium overlay verify"

# 1. BASELINE: two pins, each a 40-hex sha.
base="$HERE/BASELINE"
if [ ! -f "$base" ]; then
  bad "BASELINE missing"
else
  for key in "chromium/src" "devtools-frontend"; do
    sha="$(grep -E "^${key}([[:space:]]|\()" "$base" | grep -oE '[0-9a-f]{40}' | head -1)"
    if printf '%s' "$sha" | grep -qE '^[0-9a-f]{40}$'; then
      ok "BASELINE $key -> ${sha:0:12}"
    else
      bad "BASELINE $key pin is not a 40-hex sha (got '${sha:-<none>}')"
    fi
  done
fi

# 2. overlay: present, and strictly text (a stored binary is a bug — the engine
#    staticlib is a build artifact, never committed here).
ov="$HERE/overlay"
if [ ! -d "$ov" ]; then
  bad "overlay/ missing"
else
  n=$(find "$ov" -type f | wc -l | tr -d ' ')
  ok "overlay has $n files"
  bins=0
  while IFS= read -r -d '' f; do
    enc="$(file --mime-encoding -b "$f" 2>/dev/null || echo unknown)"
    if [ "$enc" = "binary" ]; then bad "binary file in overlay: ${f#"$ov"/}"; bins=$((bins+1)); fi
  done < <(find "$ov" -type f -print0)
  [ "$bins" -eq 0 ] && ok "no binaries in overlay"
fi

# 3. every shell script parses.
for s in "$HERE"/*.sh; do
  [ -e "$s" ] || continue
  if bash -n "$s" 2>/dev/null; then ok "syntax $(basename "$s")"; else bad "syntax error in $(basename "$s")"; fi
done

# 4. devtools-frontend patches are well-formed (parseable without a target).
for p in "$HERE"/patches-devtools-frontend/*.patch; do
  [ -e "$p" ] || continue
  if git apply --numstat "$p" >/dev/null 2>&1; then ok "patch parses $(basename "$p")"; else bad "malformed patch $(basename "$p")"; fi
done

# 5. optional: the real proof, when a checkout is available.
if [ -n "${CHROMIUM_SRC:-}" ] && [ -d "${CHROMIUM_SRC}/third_party/blink" ]; then
  echo "  -- full reconstruct verify against $CHROMIUM_SRC --"
  bash "$HERE/apply.sh" "$CHROMIUM_SRC" --verify || bad "apply --verify failed"
else
  note "skip" "reconstruct verify (set CHROMIUM_SRC to a synced checkout to enable)"
fi

echo
if [ "$fail" -eq 0 ]; then echo "PASS"; else echo "FAIL"; fi
exit "$fail"
