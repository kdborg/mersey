#!/usr/bin/env bash
# Manage the four browser-fork checkouts' GitHub forks: create them, keep the
# `mersey` branch rebased onto the latest upstream release, and push.
#
# The checkouts live beside the mersey repo (see CLAUDE.md):
#   gecko     ~/gecko          origin mozilla-firefox/firefox   base: FIREFOX_*_RELEASE tags
#   chromium  ~/chromium/src   origin chromium googlesource     base: chromiumdash latest Stable
#   servo     ~/servo-src      origin servo/servo               base: origin/main tip
#   ladybird  ~/ladybird       origin LadybirdBrowser/ladybird  base: origin/master tip
#
# Each checkout gets a second remote named `fork` pointing at your GitHub fork;
# the Mersey work lives on a `mersey` branch pushed there.
#
# Verbs:
#   setup  [fork…]   create the GitHub fork (gh repo fork) and add the `fork` remote
#   status [fork…]   show current base vs latest upstream release
#   update [fork…]   fetch upstream, rebase mersey onto the latest release base
#   push   [fork…]   push mersey to the fork remote (--force-with-lease)
#   all    [fork…]   update + push
# No fork names = all four. A specific base overrides release detection:
#   BASE=FIREFOX_141_0_RELEASE tools/update-forks.sh update gecko
#
# Updating is a REBASE, so history rewrites and the push is force-with-lease.
# If a rebase conflicts inside Mersey glue in Servo/Ladybird, the easy out is:
# take the upstream side, finish the rebase, re-run servo/apply.sh or
# ladybird/apply.sh (both idempotent), and commit the refreshed glue.
# Rebasing only moves git history — rebuilding (gclient sync, mach build, mach
# cargo, cmake) is a separate, per-fork manual step; see each fork's README.
set -euo pipefail

HERE="$(cd "$(dirname "$0")/.." && pwd)"

GECKO_SRC="${GECKO_SRC:-$HOME/gecko}"
CHROMIUM_SRC="${CHROMIUM_SRC:-$HOME/chromium/src}"
SERVO_SRC="${SERVO_SRC:-$HOME/servo-src}"
LADYBIRD_SRC="${LADYBIRD_SRC:-$HOME/ladybird}"

die() { echo "error: $*" >&2; exit 1; }

fork_dir() {
  case "$1" in
    gecko) echo "$GECKO_SRC" ;;
    chromium) echo "$CHROMIUM_SRC" ;;
    servo) echo "$SERVO_SRC" ;;
    ladybird) echo "$LADYBIRD_SRC" ;;
    *) die "unknown fork '$1' (gecko|chromium|servo|ladybird)" ;;
  esac
}

upstream_repo() {
  case "$1" in
    gecko) echo "mozilla-firefox/firefox" ;;
    chromium) echo "chromium/chromium" ;;
    servo) echo "servo/servo" ;;
    ladybird) echo "LadybirdBrowser/ladybird" ;;
  esac
}

# Resolve the ref the mersey branch should sit on. Prints a ref name that
# exists locally after latest_base fetched it.
latest_base() {
  local fork="$1" dir; dir="$(fork_dir "$fork")"
  if [ -n "${BASE:-}" ]; then
    git -C "$dir" fetch --quiet origin "refs/tags/$BASE:refs/tags/$BASE" 2>/dev/null || true
    echo "$BASE"; return
  fi
  case "$fork" in
    gecko)
      # Newest non-ESR release tag, by version sort on the numeric fields.
      local tag
      tag="$(git -C "$dir" ls-remote --tags origin 'FIREFOX_*_RELEASE' \
        | awk -F/ '{print $NF}' | grep -Ev 'esr|b[0-9]' \
        | sort -t_ -k2,2n -k3,3n -k4,4n | tail -1)"
      [ -n "$tag" ] || die "no FIREFOX_*_RELEASE tags found on origin"
      git -C "$dir" fetch --quiet origin "refs/tags/$tag:refs/tags/$tag"
      echo "$tag" ;;
    chromium)
      # chromiumdash knows the current Stable; the tag lives on googlesource.
      local ver
      ver="$(curl -fsS 'https://chromiumdash.appspot.com/fetch_releases?channel=Stable&platform=Linux&num=1' \
        | python3 -c 'import json,sys; print(json.load(sys.stdin)[0]["version"])')"
      [ -n "$ver" ] || die "could not resolve latest Chromium stable"
      git -C "$dir" fetch --quiet origin "refs/tags/$ver:refs/tags/$ver"
      echo "$ver" ;;
    servo)
      git -C "$dir" fetch --quiet origin main
      echo "origin/main" ;;
    ladybird)
      git -C "$dir" fetch --quiet origin master
      echo "origin/master" ;;
  esac
}

cmd_setup() {
  local fork="$1" dir up user
  dir="$(fork_dir "$fork")"; up="$(upstream_repo "$fork")"
  user="$(gh api user --jq .login)" || die "gh is not authenticated (gh auth login)"
  gh repo fork "$up" --clone=false >/dev/null
  local name="${up##*/}"
  if git -C "$dir" remote get-url fork >/dev/null 2>&1; then
    git -C "$dir" remote set-url fork "https://github.com/$user/$name.git"
  else
    git -C "$dir" remote add fork "https://github.com/$user/$name.git"
  fi
  echo "$fork: fork remote -> https://github.com/$user/$name"
}

cmd_status() {
  local fork="$1" dir base
  dir="$(fork_dir "$fork")"
  base="$(latest_base "$fork")"
  local mb latest
  mb="$(git -C "$dir" merge-base mersey "$base" 2>/dev/null || echo '?')"
  latest="$(git -C "$dir" rev-parse "$base^{commit}" 2>/dev/null || echo '?')"
  local n; n="$(git -C "$dir" rev-list --count "$base..mersey" 2>/dev/null || echo '?')"
  if [ "$mb" = "$latest" ]; then
    echo "$fork: up to date on $base ($n mersey commits)"
  else
    echo "$fork: BEHIND — base $(git -C "$dir" rev-parse --short "$mb" 2>/dev/null), latest $base = $(git -C "$dir" rev-parse --short "$latest" 2>/dev/null) ($n mersey commits to carry)"
  fi
}

cmd_update() {
  local fork="$1" dir base
  dir="$(fork_dir "$fork")"
  [ -z "$(git -C "$dir" status --porcelain)" ] || die "$fork: working tree not clean — commit or stash first"
  base="$(latest_base "$fork")"
  if [ "$(git -C "$dir" merge-base mersey "$base")" = "$(git -C "$dir" rev-parse "$base^{commit}")" ]; then
    echo "$fork: already based on $base"; return
  fi
  echo "$fork: rebasing mersey onto $base …"
  git -C "$dir" rebase "$base" mersey || die "$fork: rebase stopped on conflicts — resolve in $dir, then 'git rebase --continue' (for Servo/Ladybird glue conflicts: take upstream, finish, re-run apply.sh, commit)"
  echo "$fork: now based on $base — rebuild before benchmarking (see the fork's README)"
}

cmd_push() {
  local fork="$1" dir
  dir="$(fork_dir "$fork")"
  git -C "$dir" remote get-url fork >/dev/null 2>&1 || die "$fork: no 'fork' remote — run: $0 setup $fork"
  # Fetching origin first keeps push negotiation tight (the fork shares
  # upstream's objects; git must see those tips locally to exclude them).
  git -C "$dir" fetch --quiet origin || true
  git -C "$dir" push --force-with-lease fork mersey
  echo "$fork: pushed mersey -> $(git -C "$dir" remote get-url fork)"
}

verb="${1:-status}"; shift || true
forks=("$@"); [ ${#forks[@]} -gt 0 ] || forks=(gecko chromium servo ladybird)

for f in "${forks[@]}"; do
  case "$verb" in
    setup) cmd_setup "$f" ;;
    status) cmd_status "$f" ;;
    update) cmd_update "$f" ;;
    push) cmd_push "$f" ;;
    all) cmd_update "$f"; cmd_push "$f" ;;
    *) die "unknown verb '$verb' (setup|status|update|push|all)" ;;
  esac
done
