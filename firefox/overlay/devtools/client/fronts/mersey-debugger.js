/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */
"use strict";

const {
  FrontClassWithSpec,
  registerFront,
} = require("resource://devtools/shared/protocol.js");
const {
  merseyDebuggerSpec,
} = require("resource://devtools/shared/specs/mersey-debugger.js");

class MerseyDebuggerFront extends FrontClassWithSpec(merseyDebuggerSpec) {
  constructor(client, targetFront, parentFront) {
    super(client, targetFront, parentFront);
    this.formAttributeName = "merseyDebuggerActor";
  }
}

exports.MerseyDebuggerFront = MerseyDebuggerFront;
registerFront(MerseyDebuggerFront);
