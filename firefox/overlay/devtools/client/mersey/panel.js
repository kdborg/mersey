/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */
"use strict";

// Mersey fork: the Mersey debugger panel — source view with gutter
// breakpoints (engine lines), stack/scopes on pause, stepping controls.
// Drives the merseyDebugger target actor; no SpiderMonkey machinery.

class MerseyDebuggerPanel {
  constructor(iframeWindow, toolbox, commands) {
    this.win = iframeWindow;
    this.toolbox = toolbox;
    this.commands = commands;
    this.breakpoints = new Set();
    this.scripts = [];
    this.pausedFrames = null;
    this.onPaused = this.onPaused.bind(this);
    this.onResumed = this.onResumed.bind(this);
  }

  async open() {
    const doc = this.win.document;
    this.$ = id => doc.getElementById(id);
    this.$("refresh").onclick = () => this.refresh();
    this.$("pause").onclick = () => this.front.action("pause");
    this.$("resume").onclick = () => this.front.action("resume");
    this.$("stepOver").onclick = () => this.front.action("stepOver");
    this.$("stepInto").onclick = () => this.front.action("stepInto");
    this.$("stepOut").onclick = () => this.front.action("stepOut");

    const target = this.commands.targetCommand.targetFront;
    this.front = await target.getFront("merseyDebugger");
    this.front.on("paused", this.onPaused);
    this.front.on("resumed", this.onResumed);
    await this.refresh();
    this.$("status").textContent = "running";
    return this;
  }

  async refresh() {
    this.scripts = await this.front.getScripts();
    this.render();
  }

  async pushBreakpoints() {
    await this.front.setBreakpoints([...this.breakpoints]);
  }

  onPaused(pause) {
    this.pausedFrames = pause.frames || [];
    this.$("status").textContent = `paused (${pause.reason})`;
    this.render();
  }

  onResumed() {
    this.pausedFrames = null;
    this.$("status").textContent = "running";
    this.render();
  }

  render() {
    const doc = this.win.document;
    const source = this.$("source");
    source.textContent = "";
    const script = this.scripts[0];
    if (!script) {
      const row = doc.createElement("div");
      row.textContent =
        'No Mersey sources yet — load a page with <script type="text/mersey"> and press Refresh.';
      source.appendChild(row);
    } else {
      const pausedLine = this.pausedFrames?.[0]?.line ?? -1;
      script.source.split("\n").forEach((text, i) => {
        const line = i + 1;
        const row = doc.createElement("div");
        const marker = this.breakpoints.has(line) ? "\u25cf" : "\u00a0";
        row.textContent = `${marker} ${String(line).padStart(3)}  ${text}`;
        if (line === pausedLine) {
          row.className = "paused";
        }
        row.onclick = () => {
          if (this.breakpoints.has(line)) {
            this.breakpoints.delete(line);
          } else {
            this.breakpoints.add(line);
          }
          this.pushBreakpoints();
          this.render();
        };
        source.appendChild(row);
      });
    }

    const stack = this.$("stack");
    stack.textContent = "";
    if (!this.pausedFrames) {
      const idle = doc.createElement("div");
      idle.textContent = "Not paused.";
      stack.appendChild(idle);
      return;
    }
    this.pausedFrames.forEach((frame, i) => {
      const head = doc.createElement("div");
      head.style.fontWeight = "bold";
      head.textContent = `#${i} ${frame.name} (line ${frame.line})`;
      stack.appendChild(head);
      for (const scope of frame.scopes || []) {
        const s = doc.createElement("div");
        s.textContent = `  ${scope.name}`;
        stack.appendChild(s);
        for (const v of scope.variables || []) {
          const row = doc.createElement("div");
          row.textContent = `    ${v.name} = ${v.value}`;
          stack.appendChild(row);
        }
      }
    });
  }

  destroy() {
    if (this.front) {
      this.front.off("paused", this.onPaused);
      this.front.off("resumed", this.onResumed);
    }
  }
}

exports.MerseyDebuggerPanel = MerseyDebuggerPanel;
