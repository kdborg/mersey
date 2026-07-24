/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */
"use strict";

// Mersey fork: target-scoped debugger actor for the page's Mersey engine —
// a thin translator over the ChromeUtils.merseyDebug* doors. Pause/resume
// arrive as observer notifications from the engine's blocking callout; the
// content process spins its event loop while paused, which is exactly what
// keeps this actor's connection serviced.
const { Actor } = require("resource://devtools/shared/protocol.js");
const {
  merseyDebuggerSpec,
} = require("resource://devtools/shared/specs/mersey-debugger.js");

class MerseyDebuggerActor extends Actor {
  constructor(conn, targetActor) {
    super(conn, merseyDebuggerSpec);
    this.targetActor = targetActor;
    this.observe = this.observe.bind(this);
    Services.obs.addObserver(this.observe, "mersey-debugger-paused");
    Services.obs.addObserver(this.observe, "mersey-debugger-resumed");
  }

  get window() {
    return this.targetActor.window;
  }

  get innerWindowId() {
    return this.window?.windowGlobalChild?.innerWindowId ?? 0;
  }

  observe(subject, topic, data) {
    let payload;
    try {
      payload = JSON.parse(data);
    } catch (e) {
      return;
    }
    if (payload.windowId !== this.innerWindowId) {
      return;
    }
    if (topic === "mersey-debugger-paused") {
      this.emit("paused", payload.pause);
    } else {
      this.emit("resumed");
    }
  }

  getScripts() {
    return JSON.parse(ChromeUtils.merseyDebugScripts(this.window));
  }

  setBreakpoints(lines) {
    ChromeUtils.merseyDebugSetBreakpoints(this.window, lines);
  }

  action(name) {
    ChromeUtils.merseyDebugAction(this.window, name);
  }

  destroy() {
    Services.obs.removeObserver(this.observe, "mersey-debugger-paused");
    Services.obs.removeObserver(this.observe, "mersey-debugger-resumed");
    try {
      // Detaching restores the engine's VM tier and releases any pause.
      ChromeUtils.merseyDebugAction(this.window, "disable");
    } catch (e) {
      // The window may already be gone.
    }
    super.destroy();
  }
}

exports.MerseyDebuggerActor = MerseyDebuggerActor;
