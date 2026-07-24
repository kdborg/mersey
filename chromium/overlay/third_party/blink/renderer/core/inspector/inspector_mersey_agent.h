// Copyright 2026 The Chromium Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef THIRD_PARTY_BLINK_RENDERER_CORE_INSPECTOR_INSPECTOR_MERSEY_AGENT_H_
#define THIRD_PARTY_BLINK_RENDERER_CORE_INSPECTOR_INSPECTOR_MERSEY_AGENT_H_

#include "third_party/blink/renderer/core/core_export.h"
#include "third_party/blink/renderer/core/inspector/inspector_base_agent.h"
#include "third_party/blink/renderer/core/inspector/protocol/mersey.h"

namespace blink {

class InspectedFrames;
class MerseyScriptRunner;

// The DevTools console in Mersey mode.
//
// Mersey is a separate engine with a separate heap: it holds integer handles
// to host objects and never a JS value. That is why this is its own domain
// rather than a mode of Runtime — a Mersey turn cannot be expressed as an
// evaluation in a V8 context, and no binding is shared with the JS realm.
class CORE_EXPORT InspectorMerseyAgent final
    : public InspectorBaseAgent<protocol::Mersey::Metainfo> {
 public:
  explicit InspectorMerseyAgent(InspectedFrames*);
  InspectorMerseyAgent(const InspectorMerseyAgent&) = delete;
  InspectorMerseyAgent& operator=(const InspectorMerseyAgent&) = delete;
  ~InspectorMerseyAgent() override;
  void Trace(Visitor*) const override;

  // Protocol methods.
  protocol::Response evaluate(const String& expression,
                              String* result,
                              bool* is_error,
                              bool* is_compile_error) override;
  protocol::Response completions(
      std::unique_ptr<protocol::Array<String>>* names) override;
  protocol::Response getScripts(
      std::unique_ptr<protocol::Array<protocol::Mersey::Script>>* scripts)
      override;
  protocol::Response enableDebugger() override;
  protocol::Response disableDebugger() override;
  protocol::Response setBreakpoints(
      const String& url, std::unique_ptr<protocol::Array<int>> lines) override;
  protocol::Response pause() override;
  protocol::Response resume() override;
  protocol::Response stepOver() override;
  protocol::Response stepInto() override;
  protocol::Response stepOut() override;

  // Invoked from inside the engine's pause callout. BLOCKS: it emits
  // Mersey.paused and then runs a nested message loop, so the renderer keeps
  // servicing DevTools while stopped.
  void OnMerseyPaused(const String& snapshot_json);

 private:
  protocol::Response ResumeWith(void (MerseyScriptRunner::*set_mode)());
  // Installs the engine's debug hook if it is not installed yet.
  void EnsureDebuggerEnabled(MerseyScriptRunner* runner);

 public:

 private:
  // Quitting the nested loop must happen exactly once per pause, and only
  // when we are actually inside one.
  bool paused_ = false;
  bool debugger_enabled_ = false;
  Member<InspectedFrames> inspected_frames_;
};

}  // namespace blink

#endif  // THIRD_PARTY_BLINK_RENDERER_CORE_INSPECTOR_INSPECTOR_MERSEY_AGENT_H_
