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
// interpreter + Cranelift Tier-1 JIT), *not* as WASM and *not* as JS — and the
// web bridge is C++ end to end: no JS source is ever evaluated. The host owns
// the handle table natively, the hot methods of every benchmark technology
// (DOM, cssom, query, url, crypto, encoding, events, storage, timers, canvas)
// dispatch straight to the LibWeb C++ objects (Node::append_child,
// Storage::set_item, CanvasRenderingContext2D::fill_rect via web_bind, …), and
// everything outside the hot set is served by native LibJS reflection —
// Object::get / JS::call / JS::construct against the IDL bindings, with the
// engine's tagged JSON encoded and decoded in C++ and Mersey closures crossing
// as NativeFunctions. That reflective floor is what keeps the whole WebIDL
// surface reachable; the direct-C++ paths are what the benchmark measures.
// Same results in every host, verified by matching workload checksums.
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
// runs (Tier 0 straight into Tier 1 if hot); web-API calls cross into `realm`
// through the native C++ host table. Diagnostics and runtime errors land on
// stdout via the host `print` hook (what the headless bench harness reads); the
// engine never unwinds across the ABI.
void run_mersey_script(JS::Realm&, String const& source);

}
