#!/usr/bin/env bash
# Vendor the Mersey engine's crates into Servo's vendored-sources tree.
#
# Servo builds offline against vendor/ (see its .cargo/config.toml:
# `source.crates-io replace-with vendored-sources`). The engine crate
# `mersey_capi` and its dependency tree — the interpreter's few crates and,
# with the `jit` feature, Cranelift's ~30 — must therefore be present in
# vendor/ or the build cannot resolve them.
#
# Two traps this script exists to avoid, both learned the hard way:
#   1. Version, not name. Servo already vendors `gimli`, `tinyvec`, etc. — but
#      at *different* versions than Cranelift wants. Diffing by name says
#      "present"; the exact name-version is missing. We diff name-version.
#   2. Target-specific deps. `windows-sys` and `mach2` are cfg()-gated to
#      other platforms, so `cargo tree` on Linux never lists them — but cargo
#      still *resolves* them. So we drive the vendoring from mersey's own
#      Cargo.lock (the complete, target-agnostic resolution), not cargo tree.
#
# Sources come from the local cargo registry cache (populated by building the
# engine in the Mersey repo first: `cargo build --release -p mersey_capi`).
#
# Usage:  servo/vendor-deps.sh [SERVO_SRC] [MERSEY_REPO]
#         defaults: ../browsers/servo under the container
set -euo pipefail

HERE="$(cd "$(dirname "$0")/.." && pwd)"
MERSEY_REPO="${2:-$HERE}"
SERVO_SRC="${1:-$(cd "$MERSEY_REPO/.." && pwd)/browsers/servo}"
LOCK="$MERSEY_REPO/Cargo.lock"
VENDOR="$SERVO_SRC/vendor"

[ -d "$VENDOR" ] || { echo "no vendor dir at $VENDOR — is SERVO_SRC right?" >&2; exit 1; }
[ -f "$LOCK" ]   || { echo "no Cargo.lock at $LOCK" >&2; exit 1; }

SRCBASE="$(ls -d "$HOME"/.cargo/registry/src/* 2>/dev/null | head -1)"
CACHEBASE="$(ls -d "$HOME"/.cargo/registry/cache/* 2>/dev/null | head -1)"
[ -d "$SRCBASE" ] || { echo "no cargo registry src cache — build mersey_capi first" >&2; exit 1; }

python3 - "$SRCBASE" "$CACHEBASE" "$VENDOR" "$LOCK" <<'PY'
import tomllib, os, shutil, hashlib, json, sys
src, cache, vendor, lock = sys.argv[1:5]
have = set(os.listdir(vendor))
pkgs = tomllib.loads(open(lock).read())['package']
added, miss = [], []
for p in pkgs:
    n, v = p['name'], p.get('version')
    if not v:
        continue
    cv = f"{n}-{v}"
    if cv in have:
        continue
    s = os.path.join(src, cv)
    if not os.path.isdir(s):
        miss.append(cv); continue   # not needed on this target, or a path crate
    dest = os.path.join(vendor, cv)
    shutil.copytree(s, dest)
    crate = os.path.join(cache, cv + '.crate')
    pkg = hashlib.sha256(open(crate, 'rb').read()).hexdigest() if os.path.exists(crate) else None
    files = {}
    for dp, _, fs in os.walk(dest):
        for f in fs:
            rel = os.path.relpath(os.path.join(dp, f), dest).replace(os.sep, '/')
            if rel == '.cargo-checksum.json':
                continue
            files[rel] = hashlib.sha256(open(os.path.join(dp, f), 'rb').read()).hexdigest()
    json.dump({"files": files, "package": pkg}, open(os.path.join(dest, '.cargo-checksum.json'), 'w'))
    added.append(cv)
print(f"vendored {len(added)} crate(s) into {vendor}")
for c in sorted(added):
    print("  +", c)
# The mersey_* path crates and truly-unused target deps are expected misses.
unexpected = [c for c in miss if not c.startswith('mersey_')]
if unexpected:
    print("not in cache (ok if cfg-gated to another target):")
    for c in sorted(unexpected):
        print("  ?", c)
PY
echo "done."
