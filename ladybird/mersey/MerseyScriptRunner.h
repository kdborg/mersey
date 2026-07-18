/*
 * Copyright (c) 2026, the Mersey project.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

// Native Mersey engine hosted inside Ladybird — the "native" leg of the
// bench/web benchmark, the Ladybird counterpart of the Gecko fork's
// dom/mersey/MerseyScriptRunner, the Chromium fork's //components/mersey and
// core/script/mersey_script_runner, and Servo's components/script/mersey.
//
// A `<script type="text/mersey">` runs in the engine directly (Rust
// interpreter + Cranelift Tier-1 JIT), *not* as WASM and *not* as JS. Only the
// actual web-API calls cross into Ladybird's LibJS realm, through the same
// reflective bridge the WASM polyfill uses (web/mersey-bridge.js, embedded here
// as bridge.js): five reflective operations — global / get / set / call /
// construct — reach any object in the JS realm, with a handle table for object
// identity. The interned and wide-string (UTF-16, no-JSON) fast paths ARE wired,
// so the common get/set/call/new cross without JSON and with no encoding
// conversion (LibJS is UTF-16 end to end), and object-argument calls stay off the
// JSON path. The typed-binding path (web_bind) is wired too: the JIT-compiled
// canvas loop calls CanvasRenderingContext2D::fill_rect directly in C++, no JS.
// Left for later is extending that direct-DOM approach to the other hot methods
// (getRandomValues, appendChild, URL). Same results in every host, verified by
// matching workload checksums.
//
// Threading/re-entrancy: one engine context per script realm, always called
// from that realm's (main) event-loop thread. A bridge call the engine makes
// can re-enter (a JS callback invoking a Mersey closure), so the runner is
// reached through a raw thread-local pointer, not anything that would object to
// the legitimate re-entrant stack — the same discipline mersey_capi's own
// MsyContext and the Servo fork's Runner use.

#pragma once

#include <AK/String.h>
#include <LibJS/Forward.h>
#include <LibWeb/Forward.h>

// The C ABI the engine exposes (crates/mersey_capi/include/mersey.h), installed
// into the Ladybird tree beside this module by ladybird/apply.sh.
extern "C" {
#include <LibWeb/Mersey/mersey.h>
}

namespace Web::Mersey {

// Run one inline `<script type="text/mersey">` body in the engine hosted in
// `realm`'s global. `source` is UTF-8 (the engine's ABI takes UTF-8 bytes; the
// caller converts its Utf16String script text with `.to_utf8()`). Compiles and
// runs (Tier 0 straight into Tier 1 if hot); web-API calls cross through the
// reflective bridge into `realm`. Diagnostics and runtime errors land on stdout
// via the host `print` hook (what the headless bench harness reads); the engine
// never unwinds across the ABI.
void run_mersey_script(JS::Realm&, String const& source);

}
