#!/usr/bin/env bash
# Wire the Mersey console into Ladybird's DevTools (RDP) server. Idempotent —
# every edit is guarded by a marker, so re-running after a `git pull` in the
# Ladybird tree is a no-op.
#
# Run ladybird/apply.sh FIRST: this patches the fork's DevTools plumbing to
# reach Web::Mersey::repl_turn, which that script installs.
#
# WHY THIS IS A SEPARATE SCRIPT: apply.sh installs a self-contained module and
# appends to one CMakeLists. This touches seven core Ladybird files across
# three layers (RDP actor -> LibWebView -> WebContent IPC -> the engine), which
# is fork-level surgery rather than drop-in glue. Keeping it separate means a
# tree where the anchors have drifted still gets the engine, just not the
# console.
#
# THE DROPDOWN, HONESTLY: Ladybird ships no DevTools front-end — it is an RDP
# *server*, and the UI is Firefox's DevTools connecting over the wire. So the
# language selector itself cannot live here; this is the server half that such
# a client drives, via an optional "language":"mersey" parameter on
# evaluateJSAsync. Until that client exists, a `mersey>` line prefix selects
# Mersey from a STOCK Firefox DevTools console, which is what makes this half
# testable today.
#
# Usage:  ladybird/apply-devtools.sh [LADYBIRD_SRC]
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
MERSEY_REPO="$(cd "$HERE/.." && pwd)"
LADYBIRD_SRC="${1:-$(cd "$MERSEY_REPO/.." && pwd)/ladybird}"

[ -d "$LADYBIRD_SRC/Libraries/LibWeb" ] || {
  echo "no Libraries/LibWeb at $LADYBIRD_SRC — pass the Ladybird checkout path" >&2; exit 1; }

python3 - "$LADYBIRD_SRC" <<'PY'
import sys, os

root = sys.argv[1]
failures = []

def patch(relpath, marker, edits):
    """edits: list of (anchor, replacement). Applied once each, in order.
    A missing anchor aborts THIS file without writing anything."""
    path = os.path.join(root, relpath)
    if not os.path.exists(path):
        failures.append(f"{relpath}: not found")
        return
    s = open(path).read()
    if marker in s:
        print(f"ok      {relpath} (already wired)")
        return
    for anchor, replacement in edits:
        if anchor not in s:
            failures.append(f"{relpath}: anchor not found -> {anchor[:60]!r}")
            return
        s = s.replace(anchor, replacement, 1)
    open(path, "w").write(s)
    print(f"patched {relpath}")


# 1. IPC: a console turn crosses to the WebContent process.
patch("Services/WebContent/WebContentServer.ipc", "mersey_console_input", [
    ("    js_console_input(u64 page_id, String js_source) =|",
     "    js_console_input(u64 page_id, String js_source) =|\n"
     "    mersey_console_input(u64 page_id, String source) =|"),
])

patch("Services/WebContent/ConnectionFromClient.h", "mersey_console_input", [
    ("    virtual void js_console_input(u64 page_id, String) override;",
     "    virtual void js_console_input(u64 page_id, String) override;\n"
     "    virtual void mersey_console_input(u64 page_id, String) override;"),
])

patch("Services/WebContent/ConnectionFromClient.cpp", "mersey_console_input", [
    ("""void ConnectionFromClient::run_javascript(u64 page_id, String js_source)""",
     """void ConnectionFromClient::mersey_console_input(u64 page_id, String source)
{
    auto page = this->page(page_id);
    if (!page.has_value())
        return;

    page->mersey_console_input(source);
}

void ConnectionFromClient::run_javascript(u64 page_id, String js_source)"""),
])

# 2. PageClient: run the turn in the top-level document's realm.
patch("Services/WebContent/PageClient.h", "mersey_console_input", [
    ("    void js_console_input(StringView js_source);",
     "    void js_console_input(StringView js_source);\n"
     "    void mersey_console_input(StringView source);"),
])

patch("Services/WebContent/PageClient.cpp", "mersey_console_input", [
    ("#include <LibWeb/HTML/Scripting/ClassicScript.h>",
     "#include <LibWeb/HTML/Scripting/ClassicScript.h>\n"
     "#include <LibWeb/Mersey/MerseyScriptRunner.h>"),
    ("""void PageClient::run_javascript(StringView js_source)""",
     """// The DevTools console in Mersey mode. The session is one growing,
// always-typechecked module against this realm's engine — no JS is evaluated,
// and no binding is shared with the JS realm (the engine holds integer handles
// and reaches the page only through the host table).
void PageClient::mersey_console_input(StringView source)
{
    auto* document = page().top_level_browsing_context().active_document();
    if (!document) {
        did_execute_js_console_input(JsonValue {});
        return;
    }

    auto result = Web::Mersey::repl_turn(document->realm(), MUST(String::from_utf8(source)));

    // Match the JS console's serialization: a turn that declared something (or
    // evaluated to nothing) reads as `undefined`; anything else is its echo.
    // FIXME: an errored turn returns its diagnostics as the result string.
    // Rendering it as a real console exception needs `exception`/
    // `exceptionMessage` plumbed through ConsoleActor's evaluationResult.
    if (result.value.is_empty() && !result.is_error) {
        JsonObject undefined_result;
        undefined_result.set("type"sv, "undefined"sv);
        did_execute_js_console_input(move(undefined_result));
        return;
    }

    did_execute_js_console_input(JsonValue { result.value });
}

void PageClient::run_javascript(StringView js_source)"""),
])

# 3. LibWebView: the view-side send.
patch("Libraries/LibWebView/ViewImplementation.h", "mersey_console_input", [
    ("    void js_console_input(String const&);",
     "    void js_console_input(String const&);\n"
     "    void mersey_console_input(String const&);"),
])

patch("Libraries/LibWebView/ViewImplementation.cpp", "async_mersey_console_input", [
    ("""void ViewImplementation::exit_fullscreen()""",
     """void ViewImplementation::mersey_console_input(String const& source)
{
    client().async_mersey_console_input(page_id(), source);
}

void ViewImplementation::exit_fullscreen()"""),
])

# 4. The DevTools delegate: a Mersey sibling of evaluate_javascript. The reply
#    rides the SAME channel (on_received_js_console_result) — the console
#    awaits one result at a time, so a second one would be dead weight.
patch("Libraries/LibDevTools/DevToolsDelegate.h", "evaluate_mersey", [
    ("    virtual void evaluate_javascript(TabDescription const&, String const&, OnScriptEvaluationComplete) const { }",
     "    virtual void evaluate_javascript(TabDescription const&, String const&, OnScriptEvaluationComplete) const { }\n"
     "    // The console's Mersey mode. Separate entry point, separate engine,\n"
     "    // no shared bindings — see mersey/docs/architecture/devtools.md.\n"
     "    virtual void evaluate_mersey(TabDescription const&, String const&, OnScriptEvaluationComplete) const { }"),
])

patch("Libraries/LibWebView/Application.h", "evaluate_mersey", [
    ("    virtual void evaluate_javascript(DevTools::TabDescription const&, String const&, OnScriptEvaluationComplete) const override;",
     "    virtual void evaluate_javascript(DevTools::TabDescription const&, String const&, OnScriptEvaluationComplete) const override;\n"
     "    virtual void evaluate_mersey(DevTools::TabDescription const&, String const&, OnScriptEvaluationComplete) const override;"),
])

patch("Libraries/LibWebView/Application.cpp", "Application::evaluate_mersey", [
    ("""void Application::listen_for_console_messages(DevTools::TabDescription const& description, OnConsoleMessage on_console_message) const""",
     """void Application::evaluate_mersey(DevTools::TabDescription const& description, String const& source, OnScriptEvaluationComplete on_complete) const
{
    auto view = ViewImplementation::find_view_by_id(description.id);
    if (!view.has_value()) {
        on_complete(Error::from_string_literal("Unable to locate tab"));
        return;
    }

    view->on_received_js_console_result = [&view = *view, on_complete = move(on_complete)](JsonValue result) {
        view.on_received_js_console_result = nullptr;
        on_complete(move(result));
    };

    view->mersey_console_input(source);
}

void Application::listen_for_console_messages(DevTools::TabDescription const& description, OnConsoleMessage on_console_message) const"""),
])

# 5. The actor: pick the language for this turn.
patch("Libraries/LibDevTools/Actors/ConsoleActor.cpp", "evaluate_mersey", [
    ("""        if (auto tab = m_tab.strong_ref()) {
            devtools().delegate().evaluate_javascript(tab->description(), *text,
                async_handler({}, [result_id, input = *text](auto&, auto result, auto& response) {
                    received_console_result(response, move(result_id), move(input), move(result));
                }));
        }""",
     """        // Which language this turn is written in. A Mersey-aware front-end
        // sends "language":"mersey" (the console's dropdown); a STOCK Firefox
        // DevTools console has no such control, so a leading `mersey>` selects
        // it too — that prefix is what makes this server half usable before
        // the client exists.
        auto language = message.data.get_string("language"sv);
        auto is_mersey = language.has_value() && language->equals_ignoring_ascii_case("mersey"sv);

        auto source = *text;
        if (!is_mersey && source.bytes_as_string_view().starts_with("mersey>"sv)) {
            is_mersey = true;
            source = MUST(String::from_utf8(source.bytes_as_string_view().substring_view(7).trim_whitespace()));
        }

        if (auto tab = m_tab.strong_ref()) {
            auto handler = async_handler({}, [result_id, input = *text](auto&, auto result, auto& response) {
                received_console_result(response, move(result_id), move(input), move(result));
            });

            if (is_mersey)
                devtools().delegate().evaluate_mersey(tab->description(), source, move(handler));
            else
                devtools().delegate().evaluate_javascript(tab->description(), source, move(handler));
        }"""),
])

if failures:
    print("\nFAILED — no partial edit was written for the files below:")
    for f in failures:
        print("  " + f)
    sys.exit(1)
print("\ndevtools wiring applied.")
PY

echo "next: rebuild Ladybird (ninja -C Build/release)."
