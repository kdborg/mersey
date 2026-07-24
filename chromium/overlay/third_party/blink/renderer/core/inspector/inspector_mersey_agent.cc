// Copyright 2026 The Chromium Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "third_party/blink/renderer/core/inspector/inspector_mersey_agent.h"

#include <string>
#include <string_view>

#include "third_party/blink/renderer/core/dom/document.h"
#include "third_party/blink/renderer/core/frame/local_frame.h"
#include "third_party/blink/renderer/core/frame/local_dom_window.h"
#include "third_party/blink/renderer/platform/wtf/functional.h"
#include "third_party/blink/renderer/core/inspector/inspected_frames.h"
#include "third_party/blink/renderer/core/inspector/main_thread_debugger.h"
#include "third_party/blink/renderer/core/script/mersey_script_runner.h"
#include "third_party/blink/renderer/platform/json/json_parser.h"
#include "third_party/blink/renderer/platform/json/json_values.h"

namespace blink {

InspectorMerseyAgent::InspectorMerseyAgent(InspectedFrames* inspected_frames)
    : inspected_frames_(inspected_frames) {}

InspectorMerseyAgent::~InspectorMerseyAgent() = default;

void InspectorMerseyAgent::Trace(Visitor* visitor) const {
  visitor->Trace(inspected_frames_);
  InspectorBaseAgent::Trace(visitor);
}

namespace {

// The engine lives with the Document, and is created on first use — so the
// console works in Mersey mode on any page, not only ones that carried a
// <script type="text/mersey">.
MerseyScriptRunner* RunnerFor(InspectedFrames* frames) {
  if (!frames || !frames->Root()) {
    return nullptr;
  }
  Document* document = frames->Root()->GetDocument();
  if (!document) {
    return nullptr;
  }
  return &MerseyScriptRunner::From(*document);
}

}  // namespace

protocol::Response InspectorMerseyAgent::evaluate(const String& expression,
                                                  String* result,
                                                  bool* is_error,
                                                  bool* is_compile_error) {
  *result = String("");
  *is_error = false;
  *is_compile_error = false;

  MerseyScriptRunner* runner = RunnerFor(inspected_frames_);
  if (!runner) {
    return protocol::Response::ServerError("No Mersey engine for this frame");
  }

  // Decode over UTF-8 bytes rather than WTF::String helpers: the reply is a
  // byte contract from the C ABI, and this keeps the agent independent of
  // WTF's string API.
  const std::string reply = runner->ReplTurn(expression).Utf8();

  // The ABI's reply contract, decoded once here so the front-end never parses
  // prefixes: "!" = rejected by the checker (it never ran); "runtime error:" =
  // accepted but threw; anything else is the echo.
  if (!reply.empty() && reply.front() == '!') {
    *result = String::FromUtf8(std::string_view(reply).substr(1));
    *is_error = true;
    *is_compile_error = true;
    return protocol::Response::Success();
  }
  if (reply.rfind("runtime error:", 0) == 0) {
    *result = String::FromUtf8(reply);
    *is_error = true;
    return protocol::Response::Success();
  }
  *result = String::FromUtf8(reply);
  return protocol::Response::Success();
}

protocol::Response InspectorMerseyAgent::getScripts(
    std::unique_ptr<protocol::Array<protocol::Mersey::Script>>* scripts) {
  *scripts = std::make_unique<protocol::Array<protocol::Mersey::Script>>();

  MerseyScriptRunner* runner = RunnerFor(inspected_frames_);
  if (!runner) {
    return protocol::Response::ServerError("No Mersey engine for this frame");
  }
  for (const auto& entry : runner->Scripts()) {
    (*scripts)->emplace_back(protocol::Mersey::Script::create()
                                 .setUrl(entry.url)
                                 .setSource(entry.source)
                                 .setSpec(entry.spec)
                                 .setStartLine(entry.start_line)
                                 .build());
  }
  return protocol::Response::Success();
}

// ---- debugger --------------------------------------------------------------

namespace {

// The engine reports positions 1-based; CDP call frames are 0-based.
int ToCdpLine(int engine_line) {
  return engine_line > 0 ? engine_line - 1 : 0;
}

}  // namespace

void InspectorMerseyAgent::OnMerseyPaused(const String& snapshot_json) {
  std::unique_ptr<JSONValue> parsed = ParseJSON(snapshot_json);
  std::unique_ptr<JSONObject> snapshot = JSONObject::From(std::move(parsed));
  if (!snapshot) {
    return;
  }

  String reason;
  snapshot->GetString("reason", &reason);

  auto call_frames =
      std::make_unique<protocol::Array<protocol::Mersey::CallFrame>>();
  JSONArray* frames = snapshot->GetArray("frames");
  for (wtf_size_t i = 0; frames && i < frames->size(); ++i) {
    JSONObject* frame = JSONObject::Cast(frames->at(i));
    if (!frame) {
      continue;
    }
    String name, module;
    int line = 0, column = 0;
    frame->GetString("name", &name);
    frame->GetString("module", &module);
    frame->GetInteger("line", &line);
    frame->GetInteger("column", &column);

    auto scope_chain = std::make_unique<protocol::Array<protocol::Mersey::Scope>>();
    JSONArray* scopes = frame->GetArray("scopes");
    for (wtf_size_t j = 0; scopes && j < scopes->size(); ++j) {
      JSONObject* scope = JSONObject::Cast(scopes->at(j));
      if (!scope) {
        continue;
      }
      String scope_name;
      scope->GetString("name", &scope_name);
      auto variables =
          std::make_unique<protocol::Array<protocol::Mersey::Variable>>();
      JSONArray* vars = scope->GetArray("variables");
      for (wtf_size_t k = 0; vars && k < vars->size(); ++k) {
        JSONObject* var = JSONObject::Cast(vars->at(k));
        if (!var) {
          continue;
        }
        String var_name, var_value;
        var->GetString("name", &var_name);
        var->GetString("value", &var_value);
        variables->emplace_back(protocol::Mersey::Variable::create()
                                    .setName(var_name)
                                    .setValue(var_value)
                                    .build());
      }
      scope_chain->emplace_back(protocol::Mersey::Scope::create()
                                    .setName(scope_name)
                                    .setVariables(std::move(variables))
                                    .build());
    }

    call_frames->emplace_back(protocol::Mersey::CallFrame::create()
                                  .setFunctionName(name)
                                  .setUrl(module)
                                  .setLineNumber(ToCdpLine(line))
                                  .setColumnNumber(column)
                                  .setScopeChain(std::move(scope_chain))
                                  .build());
  }

  GetFrontend()->paused(reason, std::move(call_frames));
  GetFrontend()->flush();

  // BLOCK here. The engine sits mid-statement until this returns, so holding
  // the pause means not returning — and the nested loop is what keeps
  // DevTools alive meanwhile.
  LocalFrame* frame = inspected_frames_ ? inspected_frames_->Root() : nullptr;
  if (!frame || !frame->DomWindow()) {
    return;
  }
  paused_ = true;
  MainThreadDebugger::Instance(frame->DomWindow()->GetIsolate())
      ->RunNestedMessageLoopForMersey(frame);
  paused_ = false;
  GetFrontend()->resumed();
}

void InspectorMerseyAgent::EnsureDebuggerEnabled(MerseyScriptRunner* runner) {
  if (debugger_enabled_ || !runner) {
    return;
  }
  runner->DebugEnable(BindRepeating(&InspectorMerseyAgent::OnMerseyPaused,
                                    WrapPersistent(this)));
  debugger_enabled_ = true;
}

protocol::Response InspectorMerseyAgent::enableDebugger() {
  MerseyScriptRunner* runner = RunnerFor(inspected_frames_);
  if (!runner) {
    return protocol::Response::ServerError("No Mersey engine for this frame");
  }
  EnsureDebuggerEnabled(runner);
  return protocol::Response::Success();
}

protocol::Response InspectorMerseyAgent::disableDebugger() {
  MerseyScriptRunner* runner = RunnerFor(inspected_frames_);
  if (runner) {
    runner->DebugDisable();
  }
  debugger_enabled_ = false;
  return protocol::Response::Success();
}

protocol::Response InspectorMerseyAgent::setBreakpoints(
    const String& url, std::unique_ptr<protocol::Array<int>> lines) {
  MerseyScriptRunner* runner = RunnerFor(inspected_frames_);
  if (!runner) {
    return protocol::Response::ServerError("No Mersey engine for this frame");
  }
  // A breakpoint with no debug hook installed can never fire — the engine
  // would never call out at all. Requiring a separate "attach" step first is
  // a trap that fails SILENTLY, so setting a breakpoint attaches.
  EnsureDebuggerEnabled(runner);

  Vector<int> engine_lines;
  if (lines) {
    for (int line : *lines) {
      // CDP is 0-based, the engine 1-based.
      engine_lines.push_back(line + 1);
    }
  }
  runner->DebugSetBreakpoints(url, engine_lines);
  return protocol::Response::Success();
}

// Leaving a pause means two things, in this order: tell the engine what to do
// next, THEN let its callout return by quitting the nested loop.
protocol::Response InspectorMerseyAgent::ResumeWith(void (MerseyScriptRunner::*
                                                        set_mode)()) {
  MerseyScriptRunner* runner = RunnerFor(inspected_frames_);
  if (!runner) {
    return protocol::Response::ServerError("No Mersey engine for this frame");
  }
  (runner->*set_mode)();
  if (paused_) {
    LocalFrame* frame = inspected_frames_ ? inspected_frames_->Root() : nullptr;
    if (frame && frame->DomWindow()) {
      MainThreadDebugger::Instance(frame->DomWindow()->GetIsolate())
          ->QuitNestedMessageLoopForMersey();
    }
  }
  return protocol::Response::Success();
}

protocol::Response InspectorMerseyAgent::pause() {
  MerseyScriptRunner* runner = RunnerFor(inspected_frames_);
  if (!runner) {
    return protocol::Response::ServerError("No Mersey engine for this frame");
  }
  runner->DebugPause();
  return protocol::Response::Success();
}

protocol::Response InspectorMerseyAgent::resume() {
  return ResumeWith(&MerseyScriptRunner::DebugResume);
}
protocol::Response InspectorMerseyAgent::stepOver() {
  return ResumeWith(&MerseyScriptRunner::DebugStepOver);
}
protocol::Response InspectorMerseyAgent::stepInto() {
  return ResumeWith(&MerseyScriptRunner::DebugStepInto);
}
protocol::Response InspectorMerseyAgent::stepOut() {
  return ResumeWith(&MerseyScriptRunner::DebugStepOut);
}

protocol::Response InspectorMerseyAgent::completions(
    std::unique_ptr<protocol::Array<String>>* names) {
  *names = std::make_unique<protocol::Array<String>>();

  MerseyScriptRunner* runner = RunnerFor(inspected_frames_);
  if (!runner) {
    return protocol::Response::ServerError("No Mersey engine for this frame");
  }

  // The engine hands back a JSON array of names.
  std::unique_ptr<JSONValue> parsed = ParseJSON(runner->ReplCompletionsJson());
  std::unique_ptr<JSONArray> array = JSONArray::From(std::move(parsed));
  if (!array) {
    return protocol::Response::Success();
  }
  for (wtf_size_t i = 0; i < array->size(); ++i) {
    String name;
    if (array->at(i)->AsString(&name)) {
      (*names)->emplace_back(name);
    }
  }
  return protocol::Response::Success();
}

}  // namespace blink
