#!/usr/bin/env bash
# Apply the Mersey integration to a Ladybird checkout: install the engine module
# into LibWeb, link the mersey_capi staticlib, and hook
# `<script type="text/mersey">` into the script loader. Idempotent — re-running
# is a no-op (safe after `git pull` in the Ladybird tree, or to refresh the
# module from this repo).
#
# It does NOT build. See ladybird/README.md for the full build recipe.
#
# The touch points (all under Libraries/LibWeb):
#   Mersey/{MerseyScriptRunner.{h,cpp},bridge.js,bridge.js.h,mersey.h}
#                                    the engine module (copied from this repo)
#   CMakeLists.txt                   compile the module + link libmersey_capi.a
#   HTML/HTMLScriptElement.cpp       the text/mersey hook (attempted; see below)
#
# Model: like the Chromium fork's //third_party/mersey, the engine is a PREBUILT
# Rust staticlib the fork links (not a Cargo build like Servo's). Build it first
# in this repo:  cargo build --release -p mersey_capi  -> target/release/libmersey_capi.a
#
# Usage:  ladybird/apply.sh [LADYBIRD_SRC] [MERSEY_REPO]
#         defaults: ~/ladybird, the repo this script lives in
set -euo pipefail

LADYBIRD_SRC="${1:-$HOME/ladybird}"
HERE="$(cd "$(dirname "$0")" && pwd)"
MERSEY_REPO="${2:-$(cd "$HERE/.." && pwd)}"
LIBWEB="$LADYBIRD_SRC/Libraries/LibWeb"

[ -d "$LIBWEB" ] || { echo "no Libraries/LibWeb at $LADYBIRD_SRC — is LADYBIRD_SRC right?" >&2; exit 1; }

# 1. The engine module + the C ABI header + the reflective bridge.
DEST="$LIBWEB/Mersey"
mkdir -p "$DEST"
cp "$HERE/mersey/MerseyScriptRunner.h"   "$DEST/MerseyScriptRunner.h"
cp "$HERE/mersey/MerseyScriptRunner.cpp" "$DEST/MerseyScriptRunner.cpp"
cp "$MERSEY_REPO/crates/mersey_capi/include/mersey.h" "$DEST/mersey.h"
# Regenerate the bridge (readable .js + compiled .h) straight from the canonical
# web/mersey-bridge.js, so a checkout is never stale against it.
"$HERE/refresh-bridge.sh" "$DEST/bridge.js" "$MERSEY_REPO"
echo "installed Libraries/LibWeb/Mersey/{MerseyScriptRunner.{h,cpp},bridge.js,bridge.js.h,mersey.h}"

# 2. CMake wiring — appended once to LibWeb/CMakeLists.txt, guarded by a marker.
#    Appending is anchor-free (target_sources/-link work after the LibWeb target
#    is defined, which it is by end of that file), so it is robust across tree
#    layouts in a way that editing the SOURCES list in place would not be.
CMAKE="$LIBWEB/CMakeLists.txt"
STATICLIB="${MERSEY_STATICLIB:-$MERSEY_REPO/target/release/libmersey_capi.a}"
if ! grep -q "Mersey engine (native <script" "$CMAKE"; then
  cat >> "$CMAKE" <<EOF

# --- Mersey engine (native <script type="text/mersey">) ------------------------
# Installed by mersey/ladybird/apply.sh. The engine is a prebuilt Rust staticlib
# (build it in the Mersey repo: cargo build --release -p mersey_capi); override
# its path with -DMERSEY_STATICLIB=... if it is not at the default below.
set(MERSEY_STATICLIB "$STATICLIB" CACHE FILEPATH "Path to libmersey_capi.a")
target_sources(LibWeb PRIVATE Mersey/MerseyScriptRunner.cpp)
target_link_libraries(LibWeb PRIVATE "\${MERSEY_STATICLIB}")
EOF
  echo "patched Libraries/LibWeb/CMakeLists.txt (Mersey sources + staticlib link)"
else
  echo "ok      Libraries/LibWeb/CMakeLists.txt (already wired)"
fi

# 3. The text/mersey hook in HTMLScriptElement::prepare_script(). This is the one
#    edit that depends on Ladybird's exact source, which this repo cannot pin
#    (there is no in-tree checkout to verify anchors against — unlike servo/'s
#    apply.sh). It is ATTEMPTED here; if the anchor has drifted, the script does
#    not fail — it prints the exact manual edit and moves on.
python3 - "$LIBWEB" <<'PY' || true
import sys, os
libweb = sys.argv[1]
path = os.path.join(libweb, "HTML/HTMLScriptElement.cpp")
if not os.path.exists(path):
    print("SKIP    HTML/HTMLScriptElement.cpp not found — apply the hook manually (see below)")
    sys.exit(0)
s = open(path).read()

marker = "Web::Mersey::run_mersey_script"
if marker in s:
    print("ok      HTML/HTMLScriptElement.cpp (hook already present)")
    sys.exit(0)

include_anchor = '#include <LibWeb/HTML/HTMLScriptElement.h>'
# In prepare_script(), the script's type string is classified into classic /
# module / importmap; the first branch is "is a JavaScript MIME type essence
# match". We intercept just before it, for an INLINE text/mersey script, and run
# it in the engine directly. `source_text` and `script_block_type` are both in
# scope at that point (a Utf16String each).
hook_anchor = "if (MimeSniff::is_javascript_mime_type_essence_match(script_block_type)) {"

ok = include_anchor in s and hook_anchor in s
if not ok:
    print("SKIP    HTML/HTMLScriptElement.cpp anchors not found for this Ladybird revision.")
    print("        Apply the hook by hand — see ladybird/README.md, 'The text/mersey hook'.")
    sys.exit(0)

s = s.replace(include_anchor,
    include_anchor + "\n#include <LibWeb/Mersey/MerseyScriptRunner.h>", 1)
hook = (
    "// Mersey fork: an INLINE `<script type=\"text/mersey\">` runs in the embedded\n"
    "    // engine (native leg of bench/web), bypassing the classic/module machinery.\n"
    "    if (!has_attribute(HTML::AttributeNames::src)\n"
    "        && script_block_type.equals_ignoring_ascii_case(u\"text/mersey\"sv)) {\n"
    "        auto mersey_source = source_text.to_utf8();\n"
    "        Web::Mersey::run_mersey_script(realm(), mersey_source);\n"
    "        return;\n"
    "    }\n\n    " + hook_anchor)
s = s.replace(hook_anchor, hook, 1)
open(path, "w").write(s)
print("patched HTML/HTMLScriptElement.cpp (inline text/mersey hook)")
PY

echo "apply done. next: build libmersey_capi.a, then build Ladybird (see ladybird/README.md)."
