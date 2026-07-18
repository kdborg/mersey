#!/usr/bin/env bash
# Apply the Mersey integration to a Servo checkout: install the engine module
# and hook `<script type="text/mersey">` into the script loader. Idempotent —
# re-running is a no-op (safe after `git pull` in the Servo tree, or to refresh
# the module from this repo).
#
# It does NOT vendor the engine's crates — run servo/vendor-deps.sh for that —
# and it does not build. See servo/README.md for the full build recipe.
#
# The five touch points (all in components/script):
#   mersey/{mod.rs,bridge.js}   the engine module (copied from this repo)
#   lib.rs                      `pub(crate) mod mersey;`
#   Cargo.toml                  the `mersey_capi` path dependency (jit on)
#   dom/html/htmlscriptelement.rs   ScriptType::Mersey + the text/mersey hook
#
# Usage:  servo/apply.sh [SERVO_SRC] [MERSEY_REPO]
#         defaults: ~/servo-src, the repo this script lives in
set -euo pipefail

SERVO_SRC="${1:-$HOME/servo-src}"
HERE="$(cd "$(dirname "$0")" && pwd)"
MERSEY_REPO="${2:-$(cd "$HERE/.." && pwd)}"
SC="$SERVO_SRC/components/script"

[ -d "$SC" ] || { echo "no components/script at $SERVO_SRC — is SERVO_SRC right?" >&2; exit 1; }

# 1. The engine module (also re-syncs bridge.js from web/mersey-bridge.js).
mkdir -p "$SC/mersey"
cp "$HERE/mersey/mod.rs" "$SC/mersey/mod.rs"
"$MERSEY_REPO/servo/refresh-bridge.sh" "$SC/mersey/bridge.js" "$MERSEY_REPO"
echo "installed components/script/mersey/{mod.rs,bridge.js}"

python3 - "$SC" "$MERSEY_REPO" <<'PY'
import sys, os
sc, repo = sys.argv[1], sys.argv[2]

def patch(path, edits):
    p = os.path.join(sc, path)
    s = open(p).read()
    changed = False
    for old, new in edits:
        if new in s:
            continue                 # already applied
        assert old in s, f"anchor not found in {path}:\n{old[:80]}"
        s = s.replace(old, new, 1)
        changed = True
    if changed:
        open(p, "w").write(s)
    print(f"{'patched ' if changed else 'ok      '}{path}")

# lib.rs — declare the module
patch("lib.rs", [(
    "pub(crate) mod messaging;",
    "pub(crate) mod mersey;\npub(crate) mod messaging;",
)])

# Cargo.toml — the engine crate (default features => jit on)
rel = os.path.relpath(os.path.join(repo, "crates/mersey_capi"),
                      os.path.join(sc))  # e.g. ../../../Work/mersey/crates/mersey_capi
patch("Cargo.toml", [(
    "[dependencies]\n",
    f'[dependencies]\nmersey_capi = {{ path = "{rel}" }}\n',
)])

# htmlscriptelement.rs — the four hook points
patch("dom/html/htmlscriptelement.rs", [
    ("pub(crate) enum ScriptType {\n    Classic,\n    Module,\n    ImportMap,\n}",
     "pub(crate) enum ScriptType {\n    Classic,\n    Module,\n    ImportMap,\n"
     "    /// `type=\"text/mersey\"`: run the source in the embedded Mersey engine\n"
     "    /// (native leg of bench/web). Inline only, mirroring the Gecko fork.\n    Mersey,\n}"),
    ('                if ty.to_ascii_lowercase().trim_matches(HTML_SPACE_CHARACTERS) == "importmap" {\n'
     "                    return Some(ScriptType::ImportMap);\n                }",
     '                if ty.to_ascii_lowercase().trim_matches(HTML_SPACE_CHARACTERS) == "importmap" {\n'
     "                    return Some(ScriptType::ImportMap);\n                }\n\n"
     '                if ty.to_ascii_lowercase().trim_matches(HTML_SPACE_CHARACTERS) == "text/mersey" {\n'
     "                    return Some(ScriptType::Mersey);\n                }"),
    ("                },\n                ScriptType::ImportMap => (),\n            }\n        } else {",
     "                },\n                ScriptType::ImportMap => (),\n"
     "                // External Mersey scripts are not supported (inline only).\n"
     "                ScriptType::Mersey => (),\n            }\n        } else {"),
    ("                    // Step 34.3\n                    self.execute(cx, Ok(script));\n"
     "                    return;\n                },\n            }\n        }",
     "                    // Step 34.3\n                    self.execute(cx, Ok(script));\n"
     "                    return;\n                },\n"
     "                ScriptType::Mersey => {\n"
     "                    // Run the inline source directly in the embedded Mersey engine\n"
     "                    // (native leg of bench/web). No classic/module machinery.\n"
     "                    crate::mersey::run_mersey_script(global, cx, &text);\n"
     "                    return;\n                },\n            }\n        }"),
])
PY
echo "apply done. next: servo/vendor-deps.sh, then build (see servo/README.md)."
