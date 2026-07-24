/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */
"use strict";

// Mersey fork: the DevTools debugger for the page's Mersey engine. The
// engine is not SpiderMonkey — its pauses block the content process inside
// the engine's callout (the event loop spins, which is what keeps this actor
// deliverable) — so it gets its own actor rather than a thread-actor ride.
const {
  generateActorSpec,
  Arg,
  RetVal,
} = require("resource://devtools/shared/protocol.js");

const merseyDebuggerSpec = generateActorSpec({
  typeName: "merseyDebugger",

  events: {
    // The engine stopped: {reason, frames:[{name,module,line,column,
    // scopes:[{name,variables:[{name,value}]}]}]} — frames top-first,
    // engine lines 1-based.
    paused: {
      type: "paused",
      pause: Arg(0, "json"),
    },
    resumed: {
      type: "resumed",
    },
  },

  methods: {
    // The inline Mersey sources this document has run (engine line numbers).
    getScripts: {
      request: {},
      response: { scripts: RetVal("json") },
    },
    // REPLACES the engine's breakpoint set (1-based engine lines). Setting
    // breakpoints installs the debug hook itself.
    setBreakpoints: {
      request: { lines: Arg(0, "array:number") },
      response: {},
    },
    // "pause" | "resume" | "stepOver" | "stepInto" | "stepOut" | "disable".
    action: {
      request: { name: Arg(0, "string") },
      response: {},
    },
  },
});

exports.merseyDebuggerSpec = merseyDebuggerSpec;
