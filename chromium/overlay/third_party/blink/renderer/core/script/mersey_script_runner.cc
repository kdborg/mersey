// Mersey fork. Use of this source code is governed by the Chromium BSD-style
// license and the Mersey project license.

#ifdef UNSAFE_BUFFERS_BUILD
// TODO: Harden the engine C-ABI bridge's buffer handling (spans) instead of
// opting the whole file out of -Wunsafe-buffer-usage.
#pragma allow_unsafe_buffers
#endif

#include "third_party/blink/renderer/core/script/mersey_script_runner.h"

#include <cstdio>
#include <memory>

#include "base/rand_util.h"
#include "base/time/time.h"
#include "third_party/blink/renderer/core/css/css_style_declaration.h"
#include "third_party/blink/renderer/core/dom/container_node.h"
#include "third_party/blink/renderer/core/dom/document.h"
#include "third_party/blink/renderer/core/fileapi/blob.h"
#include "third_party/blink/renderer/core/geometry/dom_matrix.h"
#include "third_party/blink/renderer/platform/bindings/exception_state.h"
#include "third_party/blink/renderer/core/frame/local_dom_window.h"
#include "third_party/blink/renderer/core/script/classic_script.h"
#include "third_party/blink/renderer/platform/loader/fetch/raw_resource.h"
#include "third_party/blink/renderer/platform/loader/fetch/fetch_parameters.h"
#include "third_party/blink/renderer/platform/loader/fetch/resource_loader_options.h"
#include "third_party/blink/renderer/platform/bindings/script_state.h"
#include "third_party/blink/renderer/bindings/core/v8/script_controller.h"
#include "third_party/blink/renderer/core/frame/local_frame.h"
#include "third_party/blink/renderer/bindings/core/v8/v8_binding_for_core.h"
#include "third_party/blink/renderer/bindings/core/v8/to_v8_traits.h"
#include "third_party/blink/renderer/platform/bindings/v8_binding.h"
#include "third_party/blink/public/mojom/fetch/fetch_api_request.mojom-blink.h"
#include "third_party/blink/renderer/core/dom/dom_token_list.h"
#include "third_party/blink/renderer/core/dom/element.h"
#include "third_party/blink/renderer/core/dom/events/event.h"
#include "third_party/blink/renderer/core/dom/events/native_event_listener.h"
#include "third_party/blink/renderer/core/dom/node.h"
#include "third_party/blink/renderer/core/dom/node_list.h"
#include "third_party/blink/renderer/core/dom/static_node_list.h"
#include "third_party/blink/renderer/core/html/canvas/html_canvas_element.h"
#include "third_party/blink/renderer/core/html/html_element.h"
#include "third_party/blink/renderer/core/html/forms/html_input_element.h"
#include "third_party/blink/renderer/core/html_names.h"
#include "third_party/blink/renderer/core/inspector/console_message.h"
#include "third_party/blink/renderer/platform/bindings/exception_state.h"
#include "third_party/blink/renderer/platform/json/json_parser.h"
#include "third_party/blink/renderer/platform/json/json_values.h"
#include "third_party/blink/renderer/platform/weborigin/kurl.h"
#include "third_party/blink/renderer/platform/wtf/functional.h"
#include "third_party/blink/renderer/platform/wtf/text/atomic_string.h"
#include "third_party/blink/public/platform/task_type.h"

namespace blink {

namespace {

// The static shims the C table points at: `data` is the runner.
MerseyScriptRunner* Runner(void* data) {
  return static_cast<MerseyScriptRunner*>(data);
}

// Print an experimental-build banner to stderr once per process, the first time
// the Mersey engine is initialized. This is "Mersey Blink" — an experimental
// Chromium fork that runs <script type="text/mersey"> in the Mersey engine. It
// is not affiliated with Google or the Chromium project and is not intended for
// production use.
void EmitExperimentalBannerOnce() {
  static bool emitted = false;
  if (emitted) {
    return;
  }
  emitted = true;
  fputs(
      "\n"
      "  Mersey Blink (Experimental)\n"
      "  A Mersey-engine fork of Chromium. Experimental software — NOT for\n"
      "  production use. Not affiliated with Google or the Chromium project.\n\n",
      stderr);
  fflush(stderr);
}

void HostPrintShim(void* data, const char* utf8, size_t len) {
  Runner(data)->HostPrint(std::string_view(utf8, len), /*is_error=*/false);
}

void HostErrorShim(void* data, const char* utf8, size_t len) {
  Runner(data)->HostPrint(std::string_view(utf8, len), /*is_error=*/true);
}

void HostSetTextShim(void* data,
                     const char* id,
                     size_t id_len,
                     const char* text,
                     size_t text_len) {
  Runner(data)->HostSetText(std::string_view(id, id_len),
                            std::string_view(text, text_len));
}

const char* HostGetTextShim(void* data,
                            const char* id,
                            size_t id_len,
                            size_t* out_len) {
  const std::string* text =
      Runner(data)->HostGetText(std::string_view(id, id_len));
  if (!text) {
    return nullptr;
  }
  *out_len = text->size();
  return text->data();
}

void HostAddListenerShim(void* data,
                         const char* id,
                         size_t id_len,
                         const char* event,
                         size_t event_len,
                         uint32_t cb) {
  Runner(data)->HostAddListener(std::string_view(id, id_len),
                                std::string_view(event, event_len), cb);
}

double HostTimeMsShim(void* data, int32_t epoch) {
  return Runner(data)->HostTimeMs(epoch != 0);
}

int32_t HostRandomBytesShim(void* data, uint8_t* buf, size_t n) {
  return Runner(data)->HostRandomBytes(buf, n) ? 0 : 1;
}

// A UTF-16 argument (msy_arg16::str16) as a Blink String.
String Str16ToString(const uint16_t* str16, size_t len) {
  if (!str16 || len == 0) {
    return String();
  }
  return String(base::span<const UChar>(
      reinterpret_cast<const UChar*>(str16), len));
}

// Return a reflective reply (a std::string owned by the runner) as the ABI's
// (const char*, *out_len). A null reply is "{}" per the header.
const char* WebReply(const std::string* reply, size_t* out_len) {
  if (!reply) {
    *out_len = 0;
    return nullptr;
  }
  *out_len = reply->size();
  return reply->data();
}

int64_t HostWebGlobalShim(void* data, const char* name, size_t len) {
  return Runner(data)->HostWebGlobal(std::string_view(name, len));
}

const char* HostWebGetShim(void* data,
                           int64_t target,
                           const char* prop,
                           size_t prop_len,
                           size_t* out_len) {
  return WebReply(
      Runner(data)->HostWebGet(target, std::string_view(prop, prop_len)),
      out_len);
}

const char* HostWebNewShim(void* data,
                           const char* ctor,
                           size_t ctor_len,
                           const char* args_json,
                           size_t args_len,
                           size_t* out_len) {
  return WebReply(
      Runner(data)->HostWebNew(std::string_view(ctor, ctor_len),
                               std::string_view(args_json, args_len)),
      out_len);
}

const char* HostWebSetShim(void* data,
                           int64_t target,
                           const char* prop,
                           size_t prop_len,
                           const char* value_json,
                           size_t value_len,
                           size_t* out_len) {
  return WebReply(
      Runner(data)->HostWebSet(target, std::string_view(prop, prop_len),
                               std::string_view(value_json, value_len)),
      out_len);
}

const char* HostWebCallShim(void* data,
                            int64_t target,
                            const char* method,
                            size_t method_len,
                            const char* args_json,
                            size_t args_len,
                            size_t* out_len) {
  return WebReply(
      Runner(data)->HostWebCall(target, std::string_view(method, method_len),
                                std::string_view(args_json, args_len)),
      out_len);
}

void HostWebReleaseShim(void* data, int64_t target) {
  Runner(data)->HostWebRelease(target);
}

uint32_t HostWebInternShim(void* data, const char* name, size_t len) {
  return Runner(data)->HostWebIntern(std::string_view(name, len));
}

void HostWebGetU16Shim(void* data, int64_t target, uint32_t id, msy_reply* out) {
  Runner(data)->HostWebGetU16(target, id, out);
}

void HostWebSetU16Shim(void* data,
                       int64_t target,
                       uint32_t id,
                       const msy_arg16* value,
                       msy_reply* out) {
  Runner(data)->HostWebSetU16(target, id, value, out);
}

void HostWebCallU16Shim(void* data,
                        int64_t target,
                        uint32_t id,
                        const msy_arg16* args,
                        size_t argc,
                        msy_reply* out) {
  Runner(data)->HostWebCallU16(target, id, args, argc, out);
}

void HostWebNewU16Shim(void* data,
                       uint32_t ctor_id,
                       const msy_arg16* args,
                       size_t argc,
                       msy_reply* out) {
  Runner(data)->HostWebNewU16(ctor_id, args, argc, out);
}

size_t HostWebApplyShim(void* data,
                        const int32_t* ops,
                        size_t nops,
                        const int64_t* nodes,
                        size_t nnodes,
                        const msy_str16* strs,
                        size_t nstrs,
                        int64_t* created_out,
                        size_t created_cap) {
  return Runner(data)->HostWebApply(ops, nops, nodes, nnodes, strs, nstrs,
                                    created_out, created_cap);
}

const char* HostCapsShim(void* data, size_t* out_len) {
  // The universal bridge is coming online (URL first), so grant the full
  // surface Gecko does; the engine still gates each API by import (spec §5.4),
  // and an unimplemented op returns an error reply rather than a wrong result.
  static constexpr char kCaps[] =
      "[\"dom\",\"web\",\"time\",\"random\",\"net\",\"storage\"]";
  *out_len = sizeof(kCaps) - 1;
  return kCaps;
}

// A listener the engine registered: firing it re-enters the engine through
// the ABI, on the Document's thread, exactly like the C demo's event loop.
class MerseyEventListener final : public NativeEventListener {
 public:
  MerseyEventListener(MerseyScriptRunner* runner, uint32_t callback_id)
      : runner_(runner), callback_id_(callback_id) {}

  void Invoke(ExecutionContext*, Event*) override {
    runner_->Invoke(callback_id_);
  }

  void Trace(Visitor* visitor) const override {
    visitor->Trace(runner_);
    NativeEventListener::Trace(visitor);
  }

 private:
  Member<MerseyScriptRunner> runner_;
  uint32_t callback_id_;
};

// The console REPL's native half. The runner is V8-free, so the shim and
// this listener converse through documentElement attributes: the shim writes
// data-mersey-repl-src and dispatches a (synchronous) event; this reads the
// source, runs the turn (msy_context_repl_turn), and writes data-mersey-repl,
// which the shim reads before dispatchEvent returns.
class MerseyReplCompleteListener final : public NativeEventListener {
 public:
  explicit MerseyReplCompleteListener(MerseyScriptRunner* runner) : runner_(runner) {}
  void Invoke(ExecutionContext*, Event*) override { runner_->ReplCompleteViaAttributes(); }
  void Trace(Visitor* visitor) const override {
    visitor->Trace(runner_);
    NativeEventListener::Trace(visitor);
  }

 private:
  Member<MerseyScriptRunner> runner_;
};

class MerseyReplListener final : public NativeEventListener {
 public:
  explicit MerseyReplListener(MerseyScriptRunner* runner) : runner_(runner) {}

  void Invoke(ExecutionContext*, Event* event) override {
    runner_->ReplTurnViaAttributes(event);
  }

  void Trace(Visitor* visitor) const override {
    visitor->Trace(runner_);
    NativeEventListener::Trace(visitor);
  }

 private:
  Member<MerseyScriptRunner> runner_;
};

}  // namespace


// ---- external Mersey scripts: the native module-graph loader ---------------
//
// The fork's native runner handles inline <script type="text/mersey"> directly.
// An EXTERNAL one (src=) is a module graph: the entry may import relatives,
// which import more. This mirrors the WASM loader's loadGraph in C++: fetch the
// entry, scan its imports (msy_context_scan_imports), fetch each dependency,
// close the graph, order it dependency-first, and run it (msy_context_run_graph)
// — natively, no polyfill. Fetches run one at a time for simplicity; the graphs
// are small.

namespace {

// The WASM loader's `resolve`: join `imp` (a relative specifier) against the
// importing module's spec, resolving "." and "..".
String ResolveMerseySpec(const String& base, const String& imp) {
  Vector<String> parts;
  for (const String& seg : base.Split('/')) {
    if (!seg.empty()) {
      parts.push_back(seg);
    }
  }
  if (!parts.empty()) {
    parts.pop_back();  // drop the importer's own filename
  }
  for (const String& seg : imp.Split('/')) {
    if (seg == "." || seg.empty()) {
      continue;
    }
    if (seg == "..") {
      if (!parts.empty()) {
        parts.pop_back();
      }
    } else {
      parts.push_back(seg);
    }
  }
  StringBuilder out;
  for (wtf_size_t i = 0; i < parts.size(); ++i) {
    if (i) {
      out.Append('/');
    }
    out.Append(parts[i]);
  }
  return out.ToString();
}

bool IsRelativeSpec(const String& s) {
  return s.length() >= 2 && s[0] == '.' &&
         (s[1] == '/' || (s.length() >= 3 && s[1] == '.' && s[2] == '/'));
}

String ResourceToUtf8(Resource* resource) {
  scoped_refptr<const SharedBuffer> buffer = resource->ResourceBuffer();
  if (!buffer) {
    return String();
  }
  Vector<char> flat;
  for (base::span<const char> span : *buffer) {
    flat.Append(span.data(), static_cast<wtf_size_t>(span.size()));
  }
  return String::FromUtf8(base::as_byte_span(flat));
}

}  // namespace

class MerseyExternalLoader final : public GarbageCollected<MerseyExternalLoader>,
                                   public RawResourceClient {
 public:
  MerseyExternalLoader(Document* document,
                       MerseyScriptRunner* runner,
                       const String& entry_spec)
      : document_(document), runner_(runner), entry_spec_(entry_spec) {}

  void Start() {
    requested_.insert(entry_spec_);
    queue_.push_back(entry_spec_);
    FetchNext();
  }

  String DebugName() const override { return "MerseyExternalLoader"; }

  void Trace(Visitor* visitor) const override {
    visitor->Trace(document_);
    visitor->Trace(runner_);
    RawResourceClient::Trace(visitor);
  }

  void NotifyFinished(Resource* resource) override {
    resource->RemoveClient(this);
    const String spec = current_;
    if (resource->ErrorOccurred()) {
      Fail("failed to fetch Mersey module: " + spec);
      return;
    }
    const String source = ResourceToUtf8(resource);
    sources_.insert(spec, source);
    specs_in_order_.push_back(spec);

    // Scan this module's imports and enqueue any not-yet-fetched relatives.
    String scan_json = runner_->ScanImportsJson(source);
    Vector<String> edges;
    std::unique_ptr<JSONValue> parsed = ParseJSON(scan_json);
    if (JSONObject* obj = JSONObject::Cast(parsed.get())) {
      for (const char* key : {"static", "dynamic"}) {
        JSONArray* arr = obj->GetArray(key);
        if (!arr) {
          continue;
        }
        for (wtf_size_t i = 0; i < arr->size(); ++i) {
          String imp;
          if (!arr->at(i)->AsString(&imp) || !IsRelativeSpec(imp)) {
            continue;
          }
          String target = ResolveMerseySpec(spec, imp);
          edges.push_back(target);
          if (!requested_.Contains(target)) {
            requested_.insert(target);
            queue_.push_back(target);
          }
        }
      }
    }
    edges_.insert(spec, edges);
    FetchNext();
  }

 private:
  void FetchNext() {
    if (!queue_.empty()) {
      current_ = queue_.TakeFirst();
      KURL url = document_->CompleteURL(current_);
      LocalFrame* frame = document_->GetFrame();
      ScriptState* script_state =
          frame ? ToScriptStateForMainWorld(frame) : nullptr;
      if (!script_state) {
        Fail("no script state for Mersey module fetch");
        return;
      }
      ResourceRequest request(url);
      request.SetRequestContext(mojom::blink::RequestContextType::SCRIPT);
      FetchParameters params(std::move(request),
                             ResourceLoaderOptions(&script_state->World()));
      RawResource::Fetch(params, document_->Fetcher(), this);
      return;
    }
    // Graph closed: order dependency-first (DFS post-order from the entry).
    Vector<String> order;
    HashSet<String> done;
    OrderVisit(entry_spec_, order, done);

    // Build the run_graph payload: {"entry":…, "modules":[{spec,source}…]}.
    auto payload = std::make_unique<JSONObject>();
    payload->SetString("entry", entry_spec_);
    auto modules = std::make_unique<JSONArray>();
    for (const String& spec : order) {
      auto module = std::make_unique<JSONObject>();
      module->SetString("spec", spec);
      module->SetString("source", sources_.at(spec));
      modules->PushObject(std::move(module));
    }
    payload->SetArray("modules", std::move(modules));
    // Record each module for DevTools: full URL (Sources tree identity,
    // editor lines = engine lines) plus the spec pause frames will name.
    for (const String& spec : order) {
      runner_->RecordScript(document_->CompleteURL(spec).GetString(), spec,
                            /*start_line=*/0, sources_.at(spec));
    }
    runner_->RunGraph(payload->ToJSONString());
  }

  void OrderVisit(const String& spec,
                  Vector<String>& order,
                  HashSet<String>& done) {
    if (done.Contains(spec) || !sources_.Contains(spec)) {
      return;
    }
    done.insert(spec);  // simple cycle guard (engine rejects true cycles)
    auto it = edges_.find(spec);
    if (it != edges_.end()) {
      for (const String& dep : it->value) {
        OrderVisit(dep, order, done);
      }
    }
    order.push_back(spec);
  }

  void Fail(const String& message) {
    if (document_) {
      document_->AddConsoleMessage(MakeGarbageCollected<ConsoleMessage>(
          mojom::blink::ConsoleMessageSource::kJavaScript,
          mojom::blink::ConsoleMessageLevel::kError, "[mersey] " + message));
    }
  }

  Member<Document> document_;
  Member<MerseyScriptRunner> runner_;
  String entry_spec_;
  String current_;
  Deque<String> queue_;
  HashSet<String> requested_;
  HashMap<String, String> sources_;
  HashMap<String, Vector<String>> edges_;
  Vector<String> specs_in_order_;
};

MerseyModulesBridge& GetMerseyModulesBridge() {
  static MerseyModulesBridge bridge;
  return bridge;
}

// static
const char MerseyScriptRunner::kSupplementName[] = "MerseyScriptRunner";

// static
MerseyScriptRunner& MerseyScriptRunner::From(Document& document) {
  MerseyScriptRunner* runner =
      Supplement<Document>::From<MerseyScriptRunner>(document);
  if (!runner) {
    runner = MakeGarbageCollected<MerseyScriptRunner>(document);
    Supplement<Document>::ProvideTo(document, runner);
  }
  return *runner;
}

MerseyScriptRunner::MerseyScriptRunner(Document& document)
    : Supplement<Document>(document) {
  EmitExperimentalBannerOnce();
  msy_host_table table = {};
  table.data = this;
  table.print = HostPrintShim;
  table.error = HostErrorShim;
  table.caps = HostCapsShim;
  table.time_ms = HostTimeMsShim;
  table.random_bytes = HostRandomBytesShim;
  table.dom_set_text = HostSetTextShim;
  table.dom_get_text = HostGetTextShim;
  table.dom_add_listener = HostAddListenerShim;
  table.web_global = HostWebGlobalShim;
  table.web_get = HostWebGetShim;
  table.web_set = HostWebSetShim;
  table.web_call = HostWebCallShim;
  table.web_new = HostWebNewShim;
  table.web_release = HostWebReleaseShim;
  table.web_intern = HostWebInternShim;
  table.web_get_u16 = HostWebGetU16Shim;
  table.web_set_u16 = HostWebSetU16Shim;
  table.web_call_u16 = HostWebCallU16Shim;
  table.web_new_u16 = HostWebNewU16Shim;
  table.web_apply = HostWebApplyShim;

  CHECK_EQ(msy_abi_version(), uint32_t{MSY_ABI_VERSION})
      << "engine/header ABI mismatch";
  // The engine's Tier 1 maps W^X pages, which the renderer sandbox permits on
  // this platform today; MSY_FLAG_NO_JIT is the lever if a platform says no.
  context_ = msy_context_new_ex(&table, 0);
}

MerseyScriptRunner::~MerseyScriptRunner() {
  if (context_) {
    msy_context_free(context_);
    context_ = nullptr;
  }
}

void MerseyScriptRunner::AnnounceNative() {
  if (!announced_native_) {
    announced_native_ = true;
    Document* document = GetSupplementable();
    LocalDOMWindow* window = document ? document->domWindow() : nullptr;
    if (document) {
      document->AddConsoleMessage(MakeGarbageCollected<ConsoleMessage>(
          mojom::blink::ConsoleMessageSource::kOther,
          mojom::blink::ConsoleMessageLevel::kInfo,
          "[mersey] running natively — the engine is hosted in Blink "
          "(no WASM polyfill)."));
      // This is an experimental build; say so where a web developer will see it.
      document->AddConsoleMessage(MakeGarbageCollected<ConsoleMessage>(
          mojom::blink::ConsoleMessageSource::kOther,
          mojom::blink::ConsoleMessageLevel::kWarning,
          "[mersey] Mersey Blink is an EXPERIMENTAL build — not for production "
          "use, and not affiliated with Google or the Chromium project."));
    }
    if (window && document && document->documentElement()) {
      // The REPL's native half listens for the shim's synchronous event.
      document->addEventListener(
          AtomicString("__mersey_repl"),
          MakeGarbageCollected<MerseyReplListener>(this),
          /*use_capture=*/false);
      document->addEventListener(
          AtomicString("__mersey_repl_complete"),
          MakeGarbageCollected<MerseyReplCompleteListener>(this),
          /*use_capture=*/false);
      ClassicScript::CreateUnspecifiedScript(
          "window.merseyNative = true;"
          "window.mersey = (first, ...subs) => {"
          "  const src = Array.isArray(first)"
          "    ? first.reduce((a, s, i) => a + s + (i < subs.length ? String(subs[i]) : ''), '')"
          "    : String(first);"
          "  const root = document.documentElement;"
          "  root.setAttribute('data-mersey-repl-src', src);"
          "  root.removeAttribute('data-mersey-repl');"
          "  document.dispatchEvent(new Event('__mersey_repl'));"
          "  const out = root.getAttribute('data-mersey-repl') ?? '';"
          "  root.removeAttribute('data-mersey-repl');"
          "  root.removeAttribute('data-mersey-repl-src');"
          "  if (out.startsWith('!')) throw new Error(out.slice(1));"
          "  return out === '' ? undefined : out;"
          "};"
          "window.mersey.completions = () => {"
          "  const root = document.documentElement;"
          "  root.removeAttribute('data-mersey-repl');"
          "  document.dispatchEvent(new Event('__mersey_repl_complete'));"
          "  const out = root.getAttribute('data-mersey-repl') ?? '[]';"
          "  root.removeAttribute('data-mersey-repl');"
          "  return JSON.parse(out);"
          "};")
          ->RunScript(window);
    }
  }
}

void MerseyScriptRunner::RecordScript(const String& url, const String& spec,
                                     int start_line, const String& source) {
  // Keyed by (url, start line): one document may carry several inline
  // scripts, but re-running the same one (or the growing console session)
  // refreshes in place.
  for (auto& entry : scripts_) {
    if (entry.url == url && entry.start_line == start_line) {
      entry.source = source;
      return;
    }
  }
  scripts_.push_back(RecordedScript{url, spec, start_line, source});
}

uint32_t MerseyScriptRunner::Run(const String& source_text, int start_line) {
  if (!context_) {
    return 2;
  }
  // Announce native Mersey to the page once: the Stage A polyfill loader
  // sees `globalThis.merseyNative` and stands down (no WASM fetch, no
  // double execution). Runs before the first script so a loader tag placed
  // anywhere still observes it.
  AnnounceNative();
  Document* document = GetSupplementable();
  RecordScript(document ? document->Url().GetString() : String("<inline>"),
               "<script>", start_line, source_text);
  std::string utf8 = source_text.Utf8();
  return msy_context_run(context_, utf8.data(), utf8.size());
}

String MerseyScriptRunner::ScanImportsJson(const String& source) {
  if (!context_) {
    return "{\"static\":[],\"dynamic\":[]}";
  }
  std::string utf8 = source.Utf8();
  size_t out_len = 0;
  const char* reply =
      msy_context_scan_imports(context_, utf8.data(), utf8.size(), &out_len);
  return reply ? String::FromUtf8(std::string_view(reply, out_len))
               : String("{\"static\":[],\"dynamic\":[]}");
}

void MerseyScriptRunner::RunGraph(const String& payload_json) {
  if (!context_) {
    return;
  }
  std::string utf8 = payload_json.Utf8();
  msy_context_run_graph(context_, utf8.data(), utf8.size());
}

void MerseyScriptRunner::LoadExternalGraph(const String& entry_spec) {
  Document* document = GetSupplementable();
  if (!document) {
    return;
  }
  AnnounceNative();  // set merseyNative so the polyfill loader stands down
  MakeGarbageCollected<MerseyExternalLoader>(document, this, entry_spec)->Start();
}


// ---- debugger --------------------------------------------------------------

void MerseyScriptRunner::DebugPausedShim(void* data, const char* json,
                                         size_t len) {
  auto* runner = static_cast<MerseyScriptRunner*>(data);
  if (!runner || runner->debug_paused_.is_null()) {
    return;
  }
  // Blocks for as long as the callback holds the pause.
  runner->debug_paused_.Run(String::FromUtf8(std::string_view(json, len)));
}

void MerseyScriptRunner::DebugEnable(MerseyPausedCallback paused) {
  if (!context_) {
    return;
  }
  debug_paused_ = std::move(paused);
  msy_context_debug_enable(context_, &MerseyScriptRunner::DebugPausedShim,
                           this);
}

void MerseyScriptRunner::DebugDisable() {
  if (context_) {
    msy_context_debug_disable(context_);
  }
  debug_paused_.Reset();
}

void MerseyScriptRunner::DebugSetBreakpoints(const String& source,
                                             const Vector<int>& lines) {
  if (!context_) {
    return;
  }
  Vector<uint32_t> u32;
  u32.reserve(lines.size());
  for (int line : lines) {
    u32.push_back(static_cast<uint32_t>(line));
  }
  std::string utf8 = source.Utf8();
  msy_context_debug_set_breakpoints(context_, utf8.data(), utf8.size(),
                                    u32.data(), u32.size());
}

void MerseyScriptRunner::DebugPause() {
  if (context_) {
    msy_context_debug_pause(context_);
  }
}

void MerseyScriptRunner::DebugResume() {
  if (context_) {
    msy_context_debug_resume(context_);
  }
}

void MerseyScriptRunner::DebugStepOver() {
  if (context_) {
    msy_context_debug_step_over(context_);
  }
}

void MerseyScriptRunner::DebugStepInto() {
  if (context_) {
    msy_context_debug_step_in(context_);
  }
}

void MerseyScriptRunner::DebugStepOut() {
  if (context_) {
    msy_context_debug_step_out(context_);
  }
}

String MerseyScriptRunner::DebugEvaluate(uint32_t frame, const String& expr) {
  if (!context_) {
    return String();
  }
  std::string utf8 = expr.Utf8();
  size_t out_len = 0;
  const char* reply = msy_context_debug_evaluate(context_, frame, utf8.data(),
                                                 utf8.size(), &out_len);
  return reply ? String::FromUtf8(std::string_view(reply, out_len)) : String("");
}

String MerseyScriptRunner::ReplTurn(const String& source) {
  if (!context_) {
    return String();
  }
  std::string utf8 = source.Utf8();
  size_t out_len = 0;
  const char* reply =
      msy_context_repl_turn(context_, utf8.data(), utf8.size(), &out_len);
  return reply ? String::FromUtf8(std::string_view(reply, out_len)) : String("");
}

String MerseyScriptRunner::ReplCompletionsJson() {
  if (!context_) {
    return String("[]");
  }
  size_t out_len = 0;
  const char* reply = msy_context_repl_complete(context_, &out_len);
  return reply ? String::FromUtf8(std::string_view(reply, out_len))
               : String("[]");
}

void MerseyScriptRunner::ReplTurnViaAttributes(Event*) {
  Document* document = GetSupplementable();
  Element* root = document ? document->documentElement() : nullptr;
  if (!root || !context_) {
    return;
  }
  const AtomicString src_attr("data-mersey-repl-src");
  const AtomicString out_attr("data-mersey-repl");
  String source = root->getAttribute(src_attr);
  if (source.IsNull()) {
    return;
  }
  std::string utf8 = source.Utf8();
  size_t out_len = 0;
  const char* reply =
      msy_context_repl_turn(context_, utf8.data(), utf8.size(), &out_len);
  String out = reply ? String::FromUtf8(std::string_view(reply, out_len))
                     : String("");
  root->setAttribute(out_attr, AtomicString(out));
}

void MerseyScriptRunner::ReplCompleteViaAttributes() {
  Document* document = GetSupplementable();
  Element* root = document ? document->documentElement() : nullptr;
  if (!root || !context_) {
    return;
  }
  size_t out_len = 0;
  const char* reply = msy_context_repl_complete(context_, &out_len);
  String out = reply ? String::FromUtf8(std::string_view(reply, out_len)) : String("[]");
  root->setAttribute(AtomicString("data-mersey-repl"), AtomicString(out));
}

void MerseyScriptRunner::Invoke(uint32_t callback_id) {
  if (context_) {
    msy_context_invoke(context_, callback_id);
  }
}

void MerseyScriptRunner::Trace(Visitor* visitor) const {
  Supplement<Document>::Trace(visitor);
  visitor->Trace(node_handles_);
}

void MerseyScriptRunner::HostPrint(std::string_view message, bool is_error) {
  Document* document = GetSupplementable();
  if (!document) {
    return;
  }
  document->AddConsoleMessage(MakeGarbageCollected<ConsoleMessage>(
      mojom::blink::ConsoleMessageSource::kJavaScript,
      is_error ? mojom::blink::ConsoleMessageLevel::kError
               : mojom::blink::ConsoleMessageLevel::kInfo,
      String::FromUtf8(message)));
}

void MerseyScriptRunner::HostSetText(std::string_view id,
                                     std::string_view text) {
  Document* document = GetSupplementable();
  if (!document) {
    return;
  }
  Element* element =
      document->getElementById(AtomicString(String::FromUtf8(id)));
  if (element) {
    element->setTextContent(String::FromUtf8(text));
  }
}

const std::string* MerseyScriptRunner::HostGetText(std::string_view id) {
  Document* document = GetSupplementable();
  if (!document) {
    return nullptr;
  }
  Element* element =
      document->getElementById(AtomicString(String::FromUtf8(id)));
  if (!element) {
    return nullptr;
  }
  get_text_scratch_ = element->textContent().Utf8();
  return &get_text_scratch_;
}

void MerseyScriptRunner::HostAddListener(std::string_view id,
                                         std::string_view event,
                                         uint32_t callback_id) {
  Document* document = GetSupplementable();
  if (!document) {
    return;
  }
  Element* element =
      document->getElementById(AtomicString(String::FromUtf8(id)));
  if (!element) {
    return;
  }
  element->addEventListener(
      AtomicString(String::FromUtf8(event)),
      MakeGarbageCollected<MerseyEventListener>(this, callback_id),
      /*use_capture=*/false);
}

double MerseyScriptRunner::HostTimeMs(bool epoch) {
  if (epoch) {
    return base::Time::Now().InMillisecondsFSinceUnixEpoch();
  }
  return (base::TimeTicks::Now() - base::TimeTicks()).InMillisecondsF();
}

bool MerseyScriptRunner::HostRandomBytes(uint8_t* buf, size_t n) {
  base::RandBytes(base::span<uint8_t>(buf, n));
  return true;
}

// ---- the universal web bridge --------------------------------------------

MerseyScriptRunner::NativeObject* MerseyScriptRunner::ObjectFor(int64_t handle) {
  if (handle < kValueBase) {
    return nullptr;  // a node handle [1, kValueBase) or invalid, not a value
  }
  uint32_t slot = SlotForHandle(handle);
  if (slot >= native_objects_.size()) {
    return nullptr;
  }
  NativeObject& obj = native_objects_[slot];
  return obj.kind == NativeObject::Kind::kEmpty ? nullptr : &obj;
}

int64_t MerseyScriptRunner::AllocObject(NativeObject obj) {
  uint32_t slot;
  if (!native_free_.empty()) {
    slot = native_free_.back();
    native_free_.pop_back();
    native_objects_[slot] = std::move(obj);
  } else {
    slot = static_cast<uint32_t>(native_objects_.size());
    native_objects_.push_back(std::move(obj));
  }
  return HandleForSlot(slot);
}

int64_t MerseyScriptRunner::AllocNode(Node* node) {
  uint32_t idx;
  if (!node_free_.empty()) {
    idx = node_free_.back();
    node_free_.pop_back();
    node_handles_[idx] = node;
  } else {
    idx = static_cast<uint32_t>(node_handles_.size());
    node_handles_.push_back(node);
  }
  return static_cast<int64_t>(idx) + 1;  // non-negative: an available handle
}

Node* MerseyScriptRunner::NodeFor(int64_t handle) {
  if (handle < 1) {
    return nullptr;  // not a node handle (0 unused, <0 is a value object)
  }
  uint32_t idx = static_cast<uint32_t>(handle - 1);
  if (idx >= node_handles_.size()) {
    return nullptr;
  }
  return node_handles_[idx].Get();
}

const std::string* MerseyScriptRunner::StashReply(const String& json) {
  web_reply_scratch_ = json.Utf8();
  return &web_reply_scratch_;
}

const std::string* MerseyScriptRunner::ReplyOkString(const String& value) {
  auto ok = std::make_unique<JSONObject>();
  ok->SetString("ok", value);
  return StashReply(ok->ToJSONString());
}

const std::string* MerseyScriptRunner::ReplyOkInt(int64_t value) {
  char buf[48];
  int n = snprintf(buf, sizeof(buf), "{\"ok\":%lld}",
                   static_cast<long long>(value));
  web_reply_scratch_.assign(buf, static_cast<size_t>(n));
  return &web_reply_scratch_;
}

const std::string* MerseyScriptRunner::ReplyOkDouble(double value) {
  char buf[64];
  int n = snprintf(buf, sizeof(buf), "{\"ok\":%.17g}", value);
  web_reply_scratch_.assign(buf, static_cast<size_t>(n));
  return &web_reply_scratch_;
}

const std::string* MerseyScriptRunner::ReplyOkRef(int64_t handle) {
  // Hand-built so the handle keeps its exact int64 form (no double formatting).
  char buf[48];
  int n = snprintf(buf, sizeof(buf), "{\"ok\":{\"__ref__\":%lld}}",
                   static_cast<long long>(handle));
  web_reply_scratch_.assign(buf, static_cast<size_t>(n));
  return &web_reply_scratch_;
}

const std::string* MerseyScriptRunner::ReplyOkNull() {
  web_reply_scratch_ = "{\"ok\":null}";
  return &web_reply_scratch_;
}

const std::string* MerseyScriptRunner::ReplyErr(const String& message) {
  auto err = std::make_unique<JSONObject>();
  err->SetString("err", message);
  return StashReply(err->ToJSONString());
}

int64_t MerseyScriptRunner::HostWebGlobal(std::string_view name) {
  // Only hand back a handle for globals we actually back. `document` is a real
  // DOM node; `crypto` a value proxy (getRandomValues). Everything else is
  // reported ABSENT (-1) — the honest answer, so an unbacked global (e.g.
  // localStorage) fails loudly rather than silently no-op'ing to a wrong result.
  // (URL needs no global: `new URL` goes through web_new by name.)
  if (name == "document") {
    if (Document* document = GetSupplementable()) {
      return AllocNode(document);
    }
    return -1;
  }
  if (name == "crypto") {
    NativeObject obj;
    obj.kind = NativeObject::Kind::kGlobal;
    obj.name = std::string(name);
    return AllocObject(std::move(obj));
  }
  if (name == "localStorage" && GetMerseyModulesBridge().local_storage_get) {
    NativeObject obj;
    obj.kind = NativeObject::Kind::kGlobal;
    obj.name = std::string(name);
    return AllocObject(std::move(obj));
  }
  // Timer functions: calling a bare imported function crosses as web_call with
  // an empty method name on the global's handle (see HostWebCall).
  if (name == "setTimeout" || name == "clearTimeout") {
    NativeObject obj;
    obj.kind = NativeObject::Kind::kGlobal;
    obj.name = std::string(name);
    return AllocObject(std::move(obj));
  }
  // Nothing typed backs it — but the page may still have it. `fetch`,
  // `navigator`, `location`, `indexedDB` and the rest reach the engine this
  // way. Still -1 when the global is genuinely absent, so an unbacked name
  // fails loudly rather than silently no-op'ing.
  return ReflectGlobal(name);
}

const std::string* MerseyScriptRunner::HostWebNew(std::string_view ctor,
                                                  std::string_view args_json) {
  if (ctor == "Uint8Array") {
    // new Uint8Array(n): a zero-filled byte buffer the engine holds by handle.
    std::unique_ptr<JSONValue> parsed = ParseJSON(String::FromUtf8(args_json));
    JSONArray* args = JSONArray::Cast(parsed.get());
    int len = 0;
    if (args && args->size() >= 1) {
      args->at(0)->AsInteger(&len);
    }
    NativeObject obj;
    obj.kind = NativeObject::Kind::kBytes;
    obj.bytes.assign(len > 0 ? static_cast<size_t>(len) : 0, 0);
    return ReplyOkRef(AllocObject(std::move(obj)));
  }
  if (ctor == "Event") {
    // new Event(type): a real DOM event, dispatchable on any node.
    std::unique_ptr<JSONValue> parsed = ParseJSON(String::FromUtf8(args_json));
    JSONArray* args = JSONArray::Cast(parsed.get());
    String type;
    if (args && args->size() >= 1) {
      args->at(0)->AsString(&type);
    }
    NativeObject obj;
    obj.kind = NativeObject::Kind::kEvent;
    obj.event = Event::Create(AtomicString(type));
    return ReplyOkRef(AllocObject(std::move(obj)));
  }
  if (ctor == "TextEncoder" || ctor == "TextDecoder") {
    // Stateless UTF-8 transcoders: encode/decode run on WTF's UTF-8 codec,
    // the same one modules/encoding wraps (the KURL precedent, for text).
    NativeObject obj;
    obj.kind = ctor == "TextEncoder" ? NativeObject::Kind::kTextEncoder
                                     : NativeObject::Kind::kTextDecoder;
    return ReplyOkRef(AllocObject(std::move(obj)));
  }
  if (ctor == "DOMMatrix") {
    // new DOMMatrix(): identity; translate/scale/reads run on Blink's own
    // geometry code (core/geometry), the same path the bindings take.
    NativeObject obj;
    obj.kind = NativeObject::Kind::kMatrix;
    obj.matrix = DOMMatrix::Create();
    return ReplyOkRef(AllocObject(std::move(obj)));
  }
  if (ctor == "Blob") {
    // new Blob(parts): string parts only (that is what crosses the bridge as
    // JSON); concatenated UTF-8 becomes the blob's bytes, type defaults "".
    std::unique_ptr<JSONValue> parsed = ParseJSON(String::FromUtf8(args_json));
    JSONArray* args = JSONArray::Cast(parsed.get());
    StringBuilder text;
    if (args && args->size() >= 1) {
      if (JSONArray* parts = JSONArray::Cast(args->at(0))) {
        for (wtf_size_t i = 0; i < parts->size(); ++i) {
          String part;
          if (parts->at(i)->AsString(&part)) {
            text.Append(part);
          }
        }
      }
    }
    std::string utf8 = text.ToString().Utf8();
    NativeObject obj;
    obj.kind = NativeObject::Kind::kBlob;
    obj.blob = Blob::Create(
        base::span(reinterpret_cast<const uint8_t*>(utf8.data()), utf8.size()),
        String(""));
    return ReplyOkRef(AllocObject(std::move(obj)));
  }
  if (ctor != "URL") {
    // Not one of the typed constructors above — ask the platform. This is the
    // difference between "this fork supports five constructors" and "this fork
    // supports the web platform"; only a name the global genuinely does not
    // have is still an error.
    std::unique_ptr<JSONValue> parsed = ParseJSON(String::FromUtf8(args_json));
    JSONArray* args = JSONArray::Cast(parsed.get());
    if (const std::string* reply = ReflectNew(ctor, args)) {
      return reply;
    }
    return ReplyErr(String::FromUtf8("unknown constructor"));
  }
  // args_json is a JSON array; its first element is the URL string.
  std::unique_ptr<JSONValue> parsed = ParseJSON(String::FromUtf8(args_json));
  JSONArray* args = JSONArray::Cast(parsed.get());
  String url_string;
  if (args && args->size() >= 1) {
    args->at(0)->AsString(&url_string);
  }
  KURL url(url_string);
  if (!url.IsValid()) {
    return ReplyErr(String::FromUtf8("invalid URL"));
  }
  NativeObject obj;
  obj.kind = NativeObject::Kind::kUrl;
  obj.url = url;
  return ReplyOkRef(AllocObject(std::move(obj)));
}

const std::string* MerseyScriptRunner::HostWebGet(int64_t target,
                                                  std::string_view prop) {
  // A DOM node (`document.body`, `input.value`).
  if (Node* node = NodeFor(target)) {
    if (prop == "body") {
      // One document per runner: its body is what `document.body` names.
      if (Document* document = GetSupplementable()) {
        if (HTMLElement* body = document->body()) {
          return ReplyOkRef(AllocNode(body));
        }
      }
    }
    if (prop == "value") {
      if (auto* input = DynamicTo<HTMLInputElement>(node)) {
        return ReplyOkString(input->Value());
      }
    }
    return ReplyOkNull();
  }
  NativeObject* obj = ObjectFor(target);
  if (obj && obj->kind == NativeObject::Kind::kUrl) {
    if (prop == "pathname") {
      return ReplyOkString(obj->url.GetPath().ToString());
    }
    if (prop == "search") {
      return ReplyOkString(obj->url.QueryWithLeadingQuestionMark().ToString());
    }
  }
  if (obj && obj->kind == NativeObject::Kind::kBytes) {
    if (prop == "length") {
      return ReplyOkInt(static_cast<int64_t>(obj->bytes.size()));
    }
  }
  if (obj && obj->kind == NativeObject::Kind::kMatrix) {
    if (prop == "m41") {
      return ReplyOkDouble(obj->matrix->m41());
    }
    if (prop == "m42") {
      return ReplyOkDouble(obj->matrix->m42());
    }
    if (prop == "a") {
      return ReplyOkDouble(obj->matrix->a());
    }
  }
  if (obj && obj->kind == NativeObject::Kind::kBlob) {
    if (prop == "size") {
      return ReplyOkInt(static_cast<int64_t>(obj->blob->size()));
    }
  }
  // A reflective object (an event, a channel, anything constructed by name):
  // read the property off the real thing.
  if (const std::string* reply = ReflectGet(target, prop)) {
    return reply;
  }
  return ReplyOkNull();
}

const std::string* MerseyScriptRunner::HostWebSet(int64_t target,
                                                  std::string_view prop,
                                                  std::string_view value_json) {
  Node* node = NodeFor(target);
  if (node && (prop == "textContent" || prop == "value")) {
    // value_json is the JSON encoding of the value — here a string literal.
    std::unique_ptr<JSONValue> parsed = ParseJSON(String::FromUtf8(value_json));
    String value;
    if (parsed) {
      parsed->AsString(&value);
    }
    if (prop == "value") {
      if (auto* input = DynamicTo<HTMLInputElement>(node)) {
        input->SetValue(value);
      }
    } else {
      node->setTextContent(value);
    }
    return ReplyOkNull();
  }
  {
    std::unique_ptr<JSONValue> parsed = ParseJSON(String::FromUtf8(value_json));
    if (const std::string* reply = ReflectSet(target, prop, parsed.get())) {
      return reply;
    }
  }
  return ReplyOkNull();
}

const std::string* MerseyScriptRunner::HostWebCall(int64_t target,
                                                   std::string_view method,
                                                   std::string_view args_json) {
  std::unique_ptr<JSONValue> parsed = ParseJSON(String::FromUtf8(args_json));
  JSONArray* args = JSONArray::Cast(parsed.get());
  // A method on a value object: `crypto.getRandomValues(buf)`.
  if (NativeObject* obj = ObjectFor(target)) {
    if (obj->kind == NativeObject::Kind::kMatrix) {
      auto num_at = [&](wtf_size_t i, double def) {
        double v = def;
        if (args && args->size() > i) {
          args->at(i)->AsDouble(&v);
        }
        return v;
      };
      DOMMatrix* result = nullptr;
      if (method == "translate") {
        result = obj->matrix->translate(num_at(0, 0), num_at(1, 0));
      } else if (method == "scale") {
        result = obj->matrix->scale(num_at(0, 1));
      }
      if (result) {
        NativeObject next;
        next.kind = NativeObject::Kind::kMatrix;
        next.matrix = result;
        return ReplyOkRef(AllocObject(std::move(next)));
      }
    }
    if (obj->kind == NativeObject::Kind::kBlob && method == "slice") {
      double start = 0;
      double end = 0;
      if (args && args->size() >= 1) {
        args->at(0)->AsDouble(&start);
      }
      if (args && args->size() >= 2) {
        args->at(1)->AsDouble(&end);
      }
      NonThrowableExceptionState exception_state;
      Blob* sliced =
          obj->blob->slice(static_cast<int64_t>(start),
                           static_cast<int64_t>(end), String(""), exception_state);
      NativeObject next;
      next.kind = NativeObject::Kind::kBlob;
      next.blob = sliced;
      return ReplyOkRef(AllocObject(std::move(next)));
    }
    if (obj->kind == NativeObject::Kind::kGlobal && obj->name == "crypto" &&
        method == "getRandomValues") {
      int64_t buf_handle = 0;
      if (args && args->size() >= 1) {
        if (JSONObject* ref = JSONObject::Cast(args->at(0))) {
          double value = 0;
          ref->GetDouble("__ref__", &value);
          buf_handle = static_cast<int64_t>(value);
        }
      }
      if (NativeObject* buffer = ObjectFor(buf_handle)) {
        if (buffer->kind == NativeObject::Kind::kBytes && !buffer->bytes.empty()) {
          base::RandBytes(base::span<uint8_t>(buffer->bytes.data(),
                                              buffer->bytes.size()));
        }
        return ReplyOkRef(buf_handle);  // getRandomValues returns the buffer
      }
    }
    // A bare imported function call (`setTimeout(cb, ms)`) crosses with an
    // empty method name on the function-global's handle. The timer posts a
    // cancellable delayed task on the JS-timer task queue — the same queue
    // DOMTimer uses — that re-enters the engine; clearTimeout cancels it.
    if (obj->kind == NativeObject::Kind::kGlobal && obj->name == "setTimeout" &&
        method.empty()) {
      uint32_t cb_id = 0;
      double delay_ms = 0;
      if (args && args->size() >= 1) {
        if (JSONObject* cb = JSONObject::Cast(args->at(0))) {
          double value = 0;
          cb->GetDouble("__cb__", &value);
          cb_id = static_cast<uint32_t>(value);
        }
        if (args->size() >= 2) {
          args->at(1)->AsDouble(&delay_ms);
        }
      }
      Document* document = GetSupplementable();
      if (!document) {
        return ReplyOkNull();
      }
      int timer_id = next_timer_id_++;
      timers_[timer_id] = PostDelayedCancellableTask(
          *document->GetTaskRunner(TaskType::kJavascriptTimerDelayedLowNesting),
          FROM_HERE,
          BindOnce(&MerseyScriptRunner::FireTimer,
                   WrapWeakPersistent(this), timer_id, cb_id),
          base::Milliseconds(delay_ms));
      return ReplyOkInt(timer_id);
    }
    if (obj->kind == NativeObject::Kind::kGlobal &&
        obj->name == "clearTimeout" && method.empty()) {
      int timer_id = 0;
      if (args && args->size() >= 1) {
        args->at(0)->AsInteger(&timer_id);
      }
      auto it = timers_.find(timer_id);
      if (it != timers_.end()) {
        it->second.Cancel();
        timers_.erase(it);
      }
      return ReplyOkNull();
    }
    // localStorage.setItem(k, v) / getItem(k), via the core->modules hook.
    if (obj->kind == NativeObject::Kind::kGlobal && obj->name == "localStorage") {
      MerseyModulesBridge& bridge = GetMerseyModulesBridge();
      LocalDOMWindow* window =
          GetSupplementable() ? GetSupplementable()->domWindow() : nullptr;
      if (method == "setItem" && args && args->size() >= 2 &&
          bridge.local_storage_set) {
        String key, value;
        args->at(0)->AsString(&key);
        args->at(1)->AsString(&value);
        bridge.local_storage_set(window, key, value);
        return ReplyOkNull();
      }
      if (method == "getItem" && args && args->size() >= 1 &&
          bridge.local_storage_get) {
        String key;
        args->at(0)->AsString(&key);
        String v = bridge.local_storage_get(window, key);
        return v.IsNull() ? ReplyOkNull() : ReplyOkString(v);
      }
    }
    // Not a kind this block knows: a reflective object goes to V8 rather than
    // being answered with a silent null.
    if (const std::string* reply = ReflectCall(target, method, args)) {
      return reply;
    }
    return ReplyOkNull();
  }
  Node* node = NodeFor(target);
  if (!node) {
    if (const std::string* reply = ReflectCall(target, method, args)) {
      return reply;
    }
    return ReplyOkNull();
  }
  if (method == "getElementById") {
    String id;
    if (args && args->size() >= 1) {
      args->at(0)->AsString(&id);
    }
    if (Document* document = GetSupplementable()) {
      if (Element* element = document->getElementById(AtomicString(id))) {
        return ReplyOkRef(AllocNode(element));
      }
    }
    return ReplyOkNull();
  }
  if (method == "createElement") {
    String tag;
    if (args && args->size() >= 1) {
      args->at(0)->AsString(&tag);
    }
    if (Document* document = GetSupplementable()) {
      Element* element = document->CreateElementForBinding(AtomicString(tag),
                                                           ASSERT_NO_EXCEPTION);
      if (element) {
        return ReplyOkRef(AllocNode(element));
      }
    }
    return ReplyOkNull();
  }
  if (method == "addEventListener") {
    // args: [type, {"__cb__": id}] — the listener re-enters the engine
    // synchronously when the event dispatches (MerseyEventListener).
    String type;
    uint32_t cb_id = 0;
    bool have_cb = false;
    if (args && args->size() >= 2) {
      args->at(0)->AsString(&type);
      if (JSONObject* cb = JSONObject::Cast(args->at(1))) {
        double value = 0;
        have_cb = cb->GetDouble("__cb__", &value);
        cb_id = static_cast<uint32_t>(value);
      }
    }
    if (!type.empty() && have_cb) {
      node->addEventListener(
          AtomicString(type),
          MakeGarbageCollected<MerseyEventListener>(this, cb_id));
    }
    return ReplyOkNull();
  }
  if (method == "appendChild") {
    // The one argument is a host-object handle: {"__ref__": <handle>}.
    int64_t child_handle = 0;
    if (args && args->size() >= 1) {
      if (JSONObject* ref = JSONObject::Cast(args->at(0))) {
        double value = 0;
        ref->GetDouble("__ref__", &value);
        child_handle = static_cast<int64_t>(value);
      }
    }
    if (Node* child = NodeFor(child_handle)) {
      node->appendChild(child);
    }
    return ReplyOkNull();
  }
  // A method on a reflective object — `channel.postMessage(...)`,
  // `channel.addEventListener("message", cb)`, `stream.getReader()`. A Mersey
  // closure in the arguments becomes a real JS function (see V8Callback), which
  // is what lets a listener registered from Mersey fire at all.
  if (const std::string* reply = ReflectCall(target, method, args)) {
    return reply;
  }
  return ReplyOkNull();
}

// ---- interned typed fast paths (no JSON) ---------------------------------

uint32_t MerseyScriptRunner::HostWebIntern(std::string_view name) {
  // ABI v8: the empty name is "call the handle itself" — the timer arm/disarm
  // pair rides the wide tier with a kind-5 callback id, no JSON either way.
  if (name.empty()) return kSelfCall;
  if (name == "pathname") return kPathname;
  if (name == "search") return kSearch;
  if (name == "body") return kBody;
  if (name == "textContent") return kTextContent;
  if (name == "length") return kLength;
  if (name == "createElement") return kCreateElement;
  if (name == "appendChild") return kAppendChild;
  if (name == "getRandomValues") return kGetRandomValues;
  if (name == "setItem") return kSetItem;
  if (name == "getItem") return kGetItem;
  if (name == "URL") return kCtorURL;
  if (name == "Uint8Array") return kCtorUint8Array;
  if (name == "getContext") return kGetContext;
  if (name == "fillRect") return kFillRect;
  if (name == "width") return kWidth;
  if (name == "height") return kHeight;
  if (name == "className") return kClassName;
  if (name == "classList") return kClassList;
  if (name == "contains") return kContains;
  if (name == "style") return kStyle;
  if (name == "setProperty") return kSetProperty;
  if (name == "getPropertyValue") return kGetPropertyValue;
  if (name == "querySelectorAll") return kQuerySelectorAll;
  if (name == "dispatchEvent") return kDispatchEvent;
  if (name == "Event") return kCtorEvent;
  if (name == "encode") return kEncode;
  if (name == "decode") return kDecode;
  if (name == "TextEncoder") return kCtorTextEncoder;
  if (name == "TextDecoder") return kCtorTextDecoder;
  // A digit-only name is an indexed access (`nodes[i]`): id = kIndexBase + i.
  if (!name.empty() && name.size() <= 6) {
    bool digits = true;
    uint32_t value = 0;
    for (char c : name) {
      if (c < '0' || c > '9') {
        digits = false;
        break;
      }
      value = value * 10 + static_cast<uint32_t>(c - '0');
    }
    if (digits) {
      return kIndexBase + value;
    }
  }
  return UINT32_MAX;  // decline: the engine uses the reflective path
}

void MerseyScriptRunner::FillNull(msy_reply* out) {
  out->tag = 0;
  out->num = 0;
  out->str16 = nullptr;
  out->str16_len = 0;
}

void MerseyScriptRunner::FillNum(msy_reply* out, double value) {
  out->tag = 1;
  out->num = value;
  out->str16 = nullptr;
  out->str16_len = 0;
}

void MerseyScriptRunner::FillBool(msy_reply* out, bool value) {
  out->tag = 4;
  out->num = value ? 1 : 0;
  out->str16 = nullptr;
  out->str16_len = 0;
}

void MerseyScriptRunner::FireTimer(int timer_id, uint32_t callback_id) {
  timers_.erase(timer_id);
  Invoke(callback_id);
}

void MerseyScriptRunner::FillRef(msy_reply* out, int64_t handle) {
  out->tag = 3;
  out->num = static_cast<double>(handle);
  out->str16 = nullptr;
  out->str16_len = 0;
}

void MerseyScriptRunner::FillStr16(msy_reply* out, const String& value) {
  reply_str16_.resize(value.length());
  for (unsigned i = 0; i < value.length(); ++i) {
    reply_str16_[i] = value[i];  // UChar -> uint16_t, correct for 8- or 16-bit
  }
  out->tag = 2;
  out->num = 0;
  out->str16 = reply_str16_.data();
  out->str16_len = reply_str16_.size();
}

void MerseyScriptRunner::HostWebGetU16(int64_t target,
                                       uint32_t id,
                                       msy_reply* out) {
  if (ReflectGetU16(target, id, out)) {
    return;
  }
  // Indexed access on a NodeList: `nodes[i]` crosses as the digit-only name i.
  if (id >= kIndexBase) {
    if (NativeObject* o = ObjectFor(target);
        o && o->kind == NativeObject::Kind::kNodeList && o->node_list) {
      if (Node* node = o->node_list->item(id - kIndexBase)) {
        FillRef(out, AllocNode(node));
        return;
      }
    }
    FillNull(out);
    return;
  }
  switch (id) {
    case kPathname:
      if (NativeObject* o = ObjectFor(target);
          o && o->kind == NativeObject::Kind::kUrl) {
        FillStr16(out, o->url.GetPath().ToString());
        return;
      }
      break;
    case kSearch:
      if (NativeObject* o = ObjectFor(target);
          o && o->kind == NativeObject::Kind::kUrl) {
        FillStr16(out, o->url.QueryWithLeadingQuestionMark().ToString());
        return;
      }
      break;
    case kLength:
      if (NativeObject* o = ObjectFor(target)) {
        if (o->kind == NativeObject::Kind::kBytes) {
          FillNum(out, static_cast<double>(o->bytes.size()));
          return;
        }
        if (o->kind == NativeObject::Kind::kNodeList && o->node_list) {
          FillNum(out, static_cast<double>(o->node_list->length()));
          return;
        }
      }
      break;
    case kTextContent:
      // The get direction (`li.textContent` in the query workload); the set
      // direction lives in HostWebSetU16.
      if (Node* node = NodeFor(target)) {
        FillStr16(out, node->textContent());
        return;
      }
      break;
    case kClassList:
      if (Element* element = DynamicTo<Element>(NodeFor(target))) {
        NativeObject obj;
        obj.kind = NativeObject::Kind::kTokenList;
        obj.token_list = &element->classList();
        FillRef(out, AllocObject(std::move(obj)));
        return;
      }
      break;
    case kStyle:
      if (Element* element = DynamicTo<Element>(NodeFor(target))) {
        if (CSSStyleDeclaration* style = element->style()) {
          NativeObject obj;
          obj.kind = NativeObject::Kind::kStyle;
          obj.style = style;
          FillRef(out, AllocObject(std::move(obj)));
          return;
        }
      }
      break;
    case kBody:
      if (NodeFor(target)) {
        if (Document* document = GetSupplementable()) {
          if (HTMLElement* body = document->body()) {
            FillRef(out, AllocNode(body));
            return;
          }
        }
      }
      break;
  }
  FillNull(out);
}

void MerseyScriptRunner::HostWebSetU16(int64_t target,
                                       uint32_t id,
                                       const msy_arg16* value,
                                       msy_reply* out) {
  if (value && ReflectSetU16(target, id, *value, out)) {
    return;
  }
  if (id == kTextContent) {
    if (Node* node = NodeFor(target)) {
      String text;
      if (value && value->kind == 0 && value->str16) {
        text = Str16ToString(value->str16, value->str16_len);
      }
      node->setTextContent(text);
    }
  } else if (id == kClassName) {
    // The reflected class attribute (`el.className = s`).
    if (Element* element = DynamicTo<Element>(NodeFor(target))) {
      String text;
      if (value && value->kind == 0 && value->str16) {
        text = Str16ToString(value->str16, value->str16_len);
      }
      element->setAttribute(html_names::kClassAttr, AtomicString(text));
    }
  } else if (id == kWidth || id == kHeight) {
    if (auto* canvas = DynamicTo<HTMLCanvasElement>(NodeFor(target))) {
      unsigned px =
          value && value->kind == 1 ? static_cast<unsigned>(value->num) : 0u;
      if (id == kWidth) {
        canvas->setWidth(px, ASSERT_NO_EXCEPTION);
      } else {
        canvas->setHeight(px, ASSERT_NO_EXCEPTION);
      }
    }
  }
  FillNull(out);
}

void MerseyScriptRunner::HostWebCallU16(int64_t target,
                                        uint32_t id,
                                        const msy_arg16* args,
                                        size_t argc,
                                        msy_reply* out) {
  if (ReflectCallU16(target, id, args, argc, out)) {
    return;
  }
  switch (id) {
    case kCreateElement: {
      String tag;
      if (argc >= 1 && args[0].kind == 0 && args[0].str16) {
        tag = Str16ToString(args[0].str16, args[0].str16_len);
      }
      if (NodeFor(target)) {
        if (Document* document = GetSupplementable()) {
          if (Element* element = document->CreateElementForBinding(
                  AtomicString(tag), ASSERT_NO_EXCEPTION)) {
            FillRef(out, AllocNode(element));
            return;
          }
        }
      }
      break;
    }
    case kAppendChild: {
      Node* node = NodeFor(target);
      if (node && argc >= 1 && args[0].kind == 2) {
        if (Node* child = NodeFor(static_cast<int64_t>(args[0].num))) {
          node->appendChild(child);
        }
      }
      break;
    }
    case kGetRandomValues: {
      NativeObject* obj = ObjectFor(target);
      if (obj && obj->kind == NativeObject::Kind::kGlobal &&
          obj->name == "crypto" && argc >= 1 && args[0].kind == 2) {
        int64_t buf_handle = static_cast<int64_t>(args[0].num);
        if (NativeObject* buffer = ObjectFor(buf_handle)) {
          if (buffer->kind == NativeObject::Kind::kBytes &&
              !buffer->bytes.empty()) {
            base::RandBytes(base::span<uint8_t>(buffer->bytes.data(),
                                                buffer->bytes.size()));
          }
          FillRef(out, buf_handle);  // returns the buffer
          return;
        }
      }
      break;
    }
    case kSetItem: {
      NativeObject* obj = ObjectFor(target);
      if (obj && obj->kind == NativeObject::Kind::kGlobal &&
          obj->name == "localStorage" && argc >= 2) {
        if (auto set = GetMerseyModulesBridge().local_storage_set) {
          LocalDOMWindow* window =
              GetSupplementable() ? GetSupplementable()->domWindow() : nullptr;
          set(window, Str16ToString(args[0].str16, args[0].str16_len),
              Str16ToString(args[1].str16, args[1].str16_len));
        }
      }
      break;
    }
    case kGetItem: {
      NativeObject* obj = ObjectFor(target);
      if (obj && obj->kind == NativeObject::Kind::kGlobal &&
          obj->name == "localStorage" && argc >= 1) {
        if (auto get = GetMerseyModulesBridge().local_storage_get) {
          LocalDOMWindow* window =
              GetSupplementable() ? GetSupplementable()->domWindow() : nullptr;
          String v = get(window, Str16ToString(args[0].str16, args[0].str16_len));
          if (!v.IsNull()) {
            FillStr16(out, v);
            return;
          }
        }
      }
      break;
    }
    case kGetContext: {
      // canvas.getContext("2d") — register the 2D context on the modules side
      // (it stays alive via the canvas, which the node table roots) and hand
      // back a value handle wrapping the registry id.
      if (auto* canvas = DynamicTo<HTMLCanvasElement>(NodeFor(target))) {
        if (auto get_ctx = GetMerseyModulesBridge().canvas_get_context) {
          int ctx_id = get_ctx(canvas);
          if (ctx_id >= 0) {
            NativeObject obj;
            obj.kind = NativeObject::Kind::kCanvasCtx;
            obj.canvas_ctx_id = ctx_id;
            FillRef(out, AllocObject(std::move(obj)));
            return;
          }
        }
      }
      break;
    }
    case kFillRect: {
      if (NativeObject* obj = ObjectFor(target);
          obj && obj->kind == NativeObject::Kind::kCanvasCtx && argc >= 4) {
        if (auto fill = GetMerseyModulesBridge().canvas_fill_rect) {
          fill(obj->canvas_ctx_id, args[0].num, args[1].num, args[2].num,
               args[3].num);
        }
      }
      break;
    }
    case kContains: {
      if (NativeObject* obj = ObjectFor(target);
          obj && obj->kind == NativeObject::Kind::kTokenList &&
          obj->token_list && argc >= 1 && args[0].kind == 0) {
        FillBool(out, obj->token_list->contains(AtomicString(
                          Str16ToString(args[0].str16, args[0].str16_len))));
        return;
      }
      break;
    }
    case kSetProperty: {
      if (NativeObject* obj = ObjectFor(target);
          obj && obj->kind == NativeObject::Kind::kStyle && obj->style &&
          argc >= 2 && args[0].kind == 0 && args[1].kind == 0) {
        Document* document = GetSupplementable();
        obj->style->setProperty(
            document ? document->GetExecutionContext() : nullptr,
            Str16ToString(args[0].str16, args[0].str16_len),
            Str16ToString(args[1].str16, args[1].str16_len), String(),
            ASSERT_NO_EXCEPTION);
        FillNull(out);
        return;
      }
      break;
    }
    case kGetPropertyValue: {
      if (NativeObject* obj = ObjectFor(target);
          obj && obj->kind == NativeObject::Kind::kStyle && obj->style &&
          argc >= 1 && args[0].kind == 0) {
        FillStr16(out, obj->style->getPropertyValue(
                           Str16ToString(args[0].str16, args[0].str16_len)));
        return;
      }
      break;
    }
    case kQuerySelectorAll: {
      if (auto* container = DynamicTo<ContainerNode>(NodeFor(target));
          container && argc >= 1 && args[0].kind == 0) {
        StaticElementList* list = container->QuerySelectorAll(
            AtomicString(Str16ToString(args[0].str16, args[0].str16_len)),
            ASSERT_NO_EXCEPTION);
        if (list) {
          NativeObject obj;
          obj.kind = NativeObject::Kind::kNodeList;
          obj.node_list = list;
          FillRef(out, AllocObject(std::move(obj)));
          return;
        }
      }
      break;
    }
    case kDispatchEvent: {
      Node* node = NodeFor(target);
      if (node && argc >= 1 && args[0].kind == 2) {
        NativeObject* ev = ObjectFor(static_cast<int64_t>(args[0].num));
        if (ev && ev->kind == NativeObject::Kind::kEvent && ev->event) {
          // Dispatch runs any Mersey listeners synchronously — they re-enter
          // the engine through MerseyEventListener, like the C demo's loop.
          DispatchEventResult result = node->DispatchEvent(*ev->event);
          FillBool(out, result == DispatchEventResult::kNotCanceled);
          return;
        }
      }
      break;
    }
    case kEncode: {
      if (NativeObject* obj = ObjectFor(target);
          obj && obj->kind == NativeObject::Kind::kTextEncoder && argc >= 1 &&
          args[0].kind == 0) {
        std::string utf8 =
            Str16ToString(args[0].str16, args[0].str16_len).Utf8();
        NativeObject bytes;
        bytes.kind = NativeObject::Kind::kBytes;
        bytes.bytes.assign(utf8.begin(), utf8.end());
        FillRef(out, AllocObject(std::move(bytes)));
        return;
      }
      break;
    }
    case kDecode: {
      if (NativeObject* obj = ObjectFor(target);
          obj && obj->kind == NativeObject::Kind::kTextDecoder && argc >= 1 &&
          args[0].kind == 2) {
        NativeObject* buffer = ObjectFor(static_cast<int64_t>(args[0].num));
        if (buffer && buffer->kind == NativeObject::Kind::kBytes) {
          FillStr16(out, String::FromUtf8(base::as_byte_span(buffer->bytes)));
          return;
        }
      }
      break;
    }
    case kSelfCall: {
      // The handle is itself callable (ABI v8). Timers dispatch straight to
      // the task queue — the callback is a kind-5 stable id, so the whole
      // arm/disarm pair crosses with no JSON and no JSONValue tree.
      NativeObject* obj = ObjectFor(target);
      if (obj && obj->kind == NativeObject::Kind::kGlobal &&
          GetSupplementable()) {
        if (obj->name == "setTimeout" && argc >= 1 && args[0].kind == 5) {
          uint32_t cb_id = static_cast<uint32_t>(args[0].num);
          double delay_ms = argc >= 2 && args[1].kind == 1 ? args[1].num : 0;
          Document* document = GetSupplementable();
          int timer_id = next_timer_id_++;
          timers_[timer_id] = PostDelayedCancellableTask(
              *document->GetTaskRunner(
                  TaskType::kJavascriptTimerDelayedLowNesting),
              FROM_HERE,
              BindOnce(&MerseyScriptRunner::FireTimer, WrapWeakPersistent(this),
                       timer_id, cb_id),
              base::Milliseconds(delay_ms));
          FillNum(out, timer_id);
          return;
        }
        if (obj->name == "clearTimeout" && argc >= 1 && args[0].kind == 1) {
          auto it = timers_.find(static_cast<int>(args[0].num));
          if (it != timers_.end()) {
            it->second.Cancel();
            timers_.erase(it);
          }
          FillNull(out);
          return;
        }
      }
      // Any other callable handle (an imported `fetch`): rebuild the tagged
      // JSON arguments and take the reflective path; its JSON reply crosses
      // back as tag 7 for the engine to parse. Identical replies, one detour.
      auto json_args = std::make_unique<JSONArray>();
      for (size_t i = 0; i < argc; ++i) {
        switch (args[i].kind) {
          case 0:
            json_args->PushString(Str16ToString(args[i].str16, args[i].str16_len));
            break;
          case 1:
            json_args->PushDouble(args[i].num);
            break;
          case 2: {
            auto ref = std::make_unique<JSONObject>();
            ref->SetDouble("__ref__", args[i].num);
            json_args->PushObject(std::move(ref));
            break;
          }
          case 3:
            json_args->PushBoolean(args[i].num != 0);
            break;
          case 5: {
            auto cb = std::make_unique<JSONObject>();
            cb->SetDouble("__cb__", args[i].num);
            json_args->PushObject(std::move(cb));
            break;
          }
          default:
            json_args->PushValue(JSONValue::Null());
        }
      }
      const std::string* reply =
          HostWebCall(target, "", json_args->ToJSONString().Utf8());
      if (reply) {
        FillStr16(out, String::FromUtf8(*reply));
        out->tag = 7;
        return;
      }
      break;
    }
  }
  FillNull(out);
}

void MerseyScriptRunner::HostWebNewU16(uint32_t ctor_id,
                                       const msy_arg16* args,
                                       size_t argc,
                                       msy_reply* out) {
  if (ctor_id == kCtorURL) {
    String url_string;
    if (argc >= 1 && args[0].kind == 0 && args[0].str16) {
      url_string = Str16ToString(args[0].str16, args[0].str16_len);
    }
    KURL url(url_string);
    if (url.IsValid()) {
      NativeObject obj;
      obj.kind = NativeObject::Kind::kUrl;
      obj.url = url;
      FillRef(out, AllocObject(std::move(obj)));
      return;
    }
  } else if (ctor_id == kCtorUint8Array) {
    int len = 0;
    if (argc >= 1 && args[0].kind == 1) {
      len = static_cast<int>(args[0].num);
    }
    NativeObject obj;
    obj.kind = NativeObject::Kind::kBytes;
    obj.bytes.assign(len > 0 ? static_cast<size_t>(len) : 0, 0);
    FillRef(out, AllocObject(std::move(obj)));
    return;
  } else if (ctor_id == kCtorEvent) {
    String type;
    if (argc >= 1 && args[0].kind == 0 && args[0].str16) {
      type = Str16ToString(args[0].str16, args[0].str16_len);
    }
    NativeObject obj;
    obj.kind = NativeObject::Kind::kEvent;
    obj.event = Event::Create(AtomicString(type));
    FillRef(out, AllocObject(std::move(obj)));
    return;
  } else if (ctor_id == kCtorTextEncoder || ctor_id == kCtorTextDecoder) {
    NativeObject obj;
    obj.kind = ctor_id == kCtorTextEncoder ? NativeObject::Kind::kTextEncoder
                                           : NativeObject::Kind::kTextDecoder;
    FillRef(out, AllocObject(std::move(obj)));
    return;
  }
  FillNull(out);
}

size_t MerseyScriptRunner::HostWebApply(const int32_t* ops,
                                        size_t nops,
                                        const int64_t* nodes,
                                        size_t nnodes,
                                        const msy_str16* strs,
                                        size_t nstrs,
                                        int64_t* created_out,
                                        size_t created_cap) {
  // A node operand resolves to a live blink::Node*: a temp id (>= 0) names a
  // node this batch created (its handle is already in created_out[id], and
  // AllocNode has it traced), a negative operand names a live handle, and
  // MSY_DOM_NULL is "no node". See mersey.h MSY_DOM_* for the op stream.
  auto resolve = [&](int32_t ref) -> Node* {
    if (ref == MSY_DOM_NULL) {
      return nullptr;
    }
    if (ref >= 0) {
      return static_cast<size_t>(ref) < created_cap ? NodeFor(created_out[ref])
                                                    : nullptr;
    }
    size_t idx = static_cast<size_t>(-ref - 1);
    return idx < nnodes ? NodeFor(nodes[idx]) : nullptr;
  };
  auto str_at = [&](int32_t i) -> String {
    return (i >= 0 && static_cast<size_t>(i) < nstrs)
               ? Str16ToString(strs[i].ptr, strs[i].len)
               : String();
  };
  size_t created = 0;
  for (size_t g = 0; g < nops; ++g) {
    int32_t op = ops[g * 4], a = ops[g * 4 + 1], b = ops[g * 4 + 2],
            c = ops[g * 4 + 3];
    switch (op) {
      case MSY_DOM_CREATE: {
        Document* document = DynamicTo<Document>(resolve(c));
        if (!document) {
          document = GetSupplementable();
        }
        Node* element = nullptr;
        if (document) {
          element = document->CreateElementForBinding(AtomicString(str_at(a)),
                                                      ASSERT_NO_EXCEPTION);
        }
        int64_t handle = element ? AllocNode(element) : 0;
        if (static_cast<size_t>(b) < created_cap) {
          created_out[b] = handle;
        }
        if (element) {
          ++created;
        }
        break;
      }
      case MSY_DOM_SET_TEXT:
        if (Node* node = resolve(a)) {
          node->setTextContent(str_at(b));
        }
        break;
      case MSY_DOM_APPEND:
        if (Node* parent = resolve(a)) {
          if (Node* child = resolve(b)) {
            parent->appendChild(child);
          }
        }
        break;
      case MSY_DOM_INSERT:
        if (Node* parent = resolve(a)) {
          if (Node* child = resolve(b)) {
            parent->insertBefore(child, resolve(c));
          }
        }
        break;
      case MSY_DOM_REMOVE:
        if (Node* parent = resolve(a)) {
          if (Node* child = resolve(b)) {
            parent->removeChild(child);
          }
        }
        break;
      default:
        break;
    }
  }
  return created;
}

// ---- reflection: the whole platform, by name -------------------------------
//
// Everything above this line is a typed fast path the fork wrote by hand. What
// follows is the fallback that makes the *rest* of the web platform reachable:
// resolve the name on the real global and go through V8. It is the same shape
// Ladybird's C++ host uses against LibJS, and it is why the forks with a
// reflective bridge could run workloads this one reported as
// `unknown constructor`.

ScriptState* MerseyScriptRunner::MainWorldScriptStateOrNull() {
  Document* document = GetSupplementable();
  if (!document) {
    return nullptr;
  }
  LocalFrame* frame = document->GetFrame();
  return frame ? ToScriptStateForMainWorld(frame) : nullptr;
}

int64_t MerseyScriptRunner::AllocJsValue(v8::Isolate* isolate,
                                         v8::Local<v8::Value> value) {
  NativeObject obj;
  obj.kind = NativeObject::Kind::kJs;
  int64_t handle = AllocObject(std::move(obj));
  uint32_t slot = SlotForHandle(handle);
  if (js_values_.size() <= slot) {
    js_values_.resize(slot + 1);
  }
  js_values_[slot].Reset(isolate, value);
  return handle;
}

v8::Local<v8::Value> MerseyScriptRunner::JsValueFor(v8::Isolate* isolate,
                                                   int64_t handle) {
  NativeObject* obj = ObjectFor(handle);
  if (!obj || obj->kind != NativeObject::Kind::kJs) {
    return v8::Local<v8::Value>();
  }
  uint32_t slot = SlotForHandle(handle);
  if (slot >= js_values_.size() || js_values_[slot].IsEmpty()) {
    return v8::Local<v8::Value>();
  }
  return js_values_[slot].Get(isolate);
}

const std::string* MerseyScriptRunner::ReplyFromV8(
    v8::Isolate* isolate,
    v8::Local<v8::Context> context,
    v8::Local<v8::Value> value) {
  if (value.IsEmpty() || value->IsNullOrUndefined()) {
    return ReplyOkNull();
  }
  if (value->IsBoolean()) {
    auto ok = std::make_unique<JSONObject>();
    ok->SetBoolean("ok", value->IsTrue());
    return StashReply(ok->ToJSONString());
  }
  if (value->IsNumber()) {
    return ReplyOkDouble(value.As<v8::Number>()->Value());
  }
  if (value->IsString()) {
    return ReplyOkString(ToBlinkString<String>(isolate, value.As<v8::String>(),
                                               kExternalize));
  }
  // Anything else — an object, an array, a function — crosses as a handle the
  // engine can read back through this same path.
  return ReplyOkRef(AllocJsValue(isolate, value));
}

void MerseyScriptRunner::JsCallbackTrampoline(
    const v8::FunctionCallbackInfo<v8::Value>& info) {
  v8::Isolate* isolate = info.GetIsolate();
  v8::Local<v8::Context> context = isolate->GetCurrentContext();
  if (context.IsEmpty()) {
    return;
  }
  // The callback id rides in the function's data; the runner is found the way
  // Blink finds any supplement — through the context's document. A raw pointer
  // in a `v8::External` would be the obvious alternative, but this V8 tags
  // external pointers, and a supplement lookup cannot dangle.
  uint32_t cb = info.Data()->Uint32Value(context).FromMaybe(0);
  auto* window = DynamicTo<LocalDOMWindow>(ToExecutionContext(context));
  Document* document = window ? window->document() : nullptr;
  if (!document) {
    return;
  }
  MerseyScriptRunner* runner = &MerseyScriptRunner::From(*document);
  if (!runner->context_) {
    return;
  }
  // Scalars cross as themselves and everything else as a handle. Wrapping a
  // resolved string in a handle would hand Mersey an object where it expects a
  // string — `text.length` would then be a property read on a JS string, which
  // the reflective read has to special-case for no reason.
  auto arr = std::make_unique<JSONArray>();
  for (int i = 0; i < info.Length(); ++i) {
    v8::Local<v8::Value> a = info[i];
    if (a.IsEmpty() || a->IsNullOrUndefined()) {
      arr->PushValue(JSONValue::Null());
    } else if (a->IsBoolean()) {
      arr->PushBoolean(a->IsTrue());
    } else if (a->IsNumber()) {
      arr->PushDouble(a.As<v8::Number>()->Value());
    } else if (a->IsString()) {
      arr->PushString(
          ToBlinkString<String>(isolate, a.As<v8::String>(), kExternalize));
    } else {
      auto ref = std::make_unique<JSONObject>();
      ref->SetDouble("__ref__",
                     static_cast<double>(runner->AllocJsValue(isolate, a)));
      arr->PushObject(std::move(ref));
    }
  }
  std::string args = arr->ToJSONString().Utf8();
  msy_context_invoke_args(runner->context_, cb, args.data(), args.size());
}

v8::Local<v8::Value> MerseyScriptRunner::V8Callback(
    v8::Isolate* isolate,
    v8::Local<v8::Context> context,
    uint32_t callback_id) {
  v8::Local<v8::Function> fn;
  if (!v8::Function::New(context, &JsCallbackTrampoline,
                         v8::Integer::NewFromUnsigned(isolate, callback_id))
           .ToLocal(&fn)) {
    return v8::Null(isolate);
  }
  return fn;
}

v8::Local<v8::Value> MerseyScriptRunner::V8FromJson(
    v8::Isolate* isolate,
    v8::Local<v8::Context> context,
    JSONValue* value) {
  if (!value) {
    return v8::Null(isolate);
  }
  if (JSONObject* obj = JSONObject::Cast(value)) {
    // The bridge's two wrappers. A handle first: a reflective object round-trips
    // as itself, so `rx.postMessage(blobHandle)` reaches the real object.
    double ref = 0;
    if (obj->GetDouble("__ref__", &ref)) {
      v8::Local<v8::Value> held =
          JsValueFor(isolate, static_cast<int64_t>(ref));
      if (!held.IsEmpty()) {
        return held;
      }
      if (Node* node = NodeFor(static_cast<int64_t>(ref))) {
        return ToV8Traits<Node>::ToV8(ScriptState::From(isolate, context), node);
      }
      return v8::Null(isolate);
    }
    double cb = 0;
    if (obj->GetDouble("__cb__", &cb)) {
      return V8Callback(isolate, context, static_cast<uint32_t>(cb));
    }
    v8::Local<v8::Object> out = v8::Object::New(isolate);
    for (wtf_size_t i = 0; i < obj->size(); ++i) {
      JSONObject::Entry entry = obj->at(i);
      v8::Local<v8::String> key =
          V8String(isolate, entry.first);
      std::ignore = out->Set(context, key, V8FromJson(isolate, context, entry.second));
    }
    return out;
  }
  if (JSONArray* arr = JSONArray::Cast(value)) {
    v8::Local<v8::Array> out = v8::Array::New(isolate, arr->size());
    for (wtf_size_t i = 0; i < arr->size(); ++i) {
      std::ignore = out->Set(context, i, V8FromJson(isolate, context, arr->at(i)));
    }
    return out;
  }
  bool b = false;
  if (value->AsBoolean(&b)) {
    return v8::Boolean::New(isolate, b);
  }
  double d = 0;
  if (value->AsDouble(&d)) {
    return v8::Number::New(isolate, d);
  }
  String str;
  if (value->AsString(&str)) {
    return V8String(isolate, str);
  }
  return v8::Null(isolate);
}

const std::string* MerseyScriptRunner::ReflectGet(int64_t target,
                                                  std::string_view name) {
  ScriptState* script_state = MainWorldScriptStateOrNull();
  if (!script_state) {
    return nullptr;
  }
  ScriptState::Scope scope(script_state);
  // Entering V8 from the host needs a microtask scope; without one this trips a
  // debug check on the queue's depth. `kDoNotRunMicrotasks`: the engine is
  // mid-execution here, and the page's own event loop drains the queue.
  v8::MicrotasksScope microtasks(script_state->GetIsolate(),
                                 ToMicrotaskQueue(script_state),
                                 v8::MicrotasksScope::kDoNotRunMicrotasks);
  v8::Isolate* isolate = script_state->GetIsolate();
  v8::Local<v8::Context> context = script_state->GetContext();
  v8::Local<v8::Value> recv = JsValueFor(isolate, target);
  if (recv.IsEmpty()) {
    return nullptr;
  }
  v8::TryCatch try_catch(isolate);
  v8::Local<v8::Object> recv_obj;
  if (!recv->ToObject(context).ToLocal(&recv_obj)) {
    return nullptr;
  }
  v8::Local<v8::Value> out;
  if (!recv_obj->Get(context, V8String(isolate, String::FromUtf8(name)))
           .ToLocal(&out)) {
    return ReplyErr("property read threw");
  }
  return ReplyFromV8(isolate, context, out);
}

const std::string* MerseyScriptRunner::ReflectSet(int64_t target,
                                                  std::string_view name,
                                                  JSONValue* value) {
  ScriptState* script_state = MainWorldScriptStateOrNull();
  if (!script_state) {
    return nullptr;
  }
  ScriptState::Scope scope(script_state);
  // Entering V8 from the host needs a microtask scope; without one this trips a
  // debug check on the queue's depth. `kDoNotRunMicrotasks`: the engine is
  // mid-execution here, and the page's own event loop drains the queue.
  v8::MicrotasksScope microtasks(script_state->GetIsolate(),
                                 ToMicrotaskQueue(script_state),
                                 v8::MicrotasksScope::kDoNotRunMicrotasks);
  v8::Isolate* isolate = script_state->GetIsolate();
  v8::Local<v8::Context> context = script_state->GetContext();
  v8::Local<v8::Value> recv = JsValueFor(isolate, target);
  if (recv.IsEmpty() || !recv->IsObject()) {
    return nullptr;
  }
  v8::TryCatch try_catch(isolate);
  std::ignore = recv.As<v8::Object>()->Set(
      context, V8String(isolate, String::FromUtf8(name)),
      V8FromJson(isolate, context, value));
  return ReplyOkNull();
}

const std::string* MerseyScriptRunner::ReflectCall(int64_t target,
                                                   std::string_view method,
                                                   JSONArray* args) {
  ScriptState* script_state = MainWorldScriptStateOrNull();
  if (!script_state) {
    return nullptr;
  }
  ScriptState::Scope scope(script_state);
  // Entering V8 from the host needs a microtask scope; without one this trips a
  // debug check on the queue's depth. `kDoNotRunMicrotasks`: the engine is
  // mid-execution here, and the page's own event loop drains the queue.
  v8::MicrotasksScope microtasks(script_state->GetIsolate(),
                                 ToMicrotaskQueue(script_state),
                                 v8::MicrotasksScope::kDoNotRunMicrotasks);
  v8::Isolate* isolate = script_state->GetIsolate();
  v8::Local<v8::Context> context = script_state->GetContext();
  v8::Local<v8::Value> recv = JsValueFor(isolate, target);
  if (recv.IsEmpty()) {
    return nullptr;
  }
  v8::TryCatch try_catch(isolate);
  // An empty method name means the handle is itself callable (an imported
  // `fetch(url)`), the same convention the interned tier uses.
  v8::Local<v8::Value> fn_value = recv;
  v8::Local<v8::Value> this_value = v8::Undefined(isolate).As<v8::Value>();
  if (!method.empty()) {
    if (!recv->IsObject()) {
      return nullptr;
    }
    this_value = recv;
    if (!recv.As<v8::Object>()
             ->Get(context, V8String(isolate, String::FromUtf8(method)))
             .ToLocal(&fn_value)) {
      return ReplyErr("method lookup threw");
    }
  }
  if (!fn_value->IsFunction()) {
    return ReplyErr(String::FromUtf8(method) + " is not a function");
  }
  Vector<v8::Local<v8::Value>> argv;
  if (args) {
    for (wtf_size_t i = 0; i < args->size(); ++i) {
      argv.push_back(V8FromJson(isolate, context, args->at(i)));
    }
  }
  v8::Local<v8::Value> out;
  if (!fn_value.As<v8::Function>()
           ->Call(context, this_value, static_cast<int>(argv.size()),
                  argv.data())
           .ToLocal(&out)) {
    return ReplyErr(String::FromUtf8(method) + " threw");
  }
  return ReplyFromV8(isolate, context, out);
}

const std::string* MerseyScriptRunner::ReflectNew(std::string_view ctor,
                                                  JSONArray* args) {
  ScriptState* script_state = MainWorldScriptStateOrNull();
  if (!script_state) {
    return nullptr;
  }
  ScriptState::Scope scope(script_state);
  // Entering V8 from the host needs a microtask scope; without one this trips a
  // debug check on the queue's depth. `kDoNotRunMicrotasks`: the engine is
  // mid-execution here, and the page's own event loop drains the queue.
  v8::MicrotasksScope microtasks(script_state->GetIsolate(),
                                 ToMicrotaskQueue(script_state),
                                 v8::MicrotasksScope::kDoNotRunMicrotasks);
  v8::Isolate* isolate = script_state->GetIsolate();
  v8::Local<v8::Context> context = script_state->GetContext();
  v8::TryCatch try_catch(isolate);
  v8::Local<v8::Value> ctor_value;
  if (!context->Global()
           ->Get(context, V8String(isolate, String::FromUtf8(ctor)))
           .ToLocal(&ctor_value) ||
      !ctor_value->IsFunction()) {
    return nullptr;  // genuinely unknown: let the caller say so
  }
  Vector<v8::Local<v8::Value>> argv;
  if (args) {
    for (wtf_size_t i = 0; i < args->size(); ++i) {
      argv.push_back(V8FromJson(isolate, context, args->at(i)));
    }
  }
  v8::Local<v8::Object> out;
  if (!ctor_value.As<v8::Function>()
           ->NewInstance(context, static_cast<int>(argv.size()), argv.data())
           .ToLocal(&out)) {
    return ReplyErr(String::FromUtf8(ctor) + " threw");
  }
  return ReplyOkRef(AllocJsValue(isolate, out));
}

int64_t MerseyScriptRunner::ReflectGlobal(std::string_view name) {
  ScriptState* script_state = MainWorldScriptStateOrNull();
  if (!script_state) {
    return -1;
  }
  ScriptState::Scope scope(script_state);
  // Entering V8 from the host needs a microtask scope; without one this trips a
  // debug check on the queue's depth. `kDoNotRunMicrotasks`: the engine is
  // mid-execution here, and the page's own event loop drains the queue.
  v8::MicrotasksScope microtasks(script_state->GetIsolate(),
                                 ToMicrotaskQueue(script_state),
                                 v8::MicrotasksScope::kDoNotRunMicrotasks);
  v8::Isolate* isolate = script_state->GetIsolate();
  v8::Local<v8::Context> context = script_state->GetContext();
  v8::TryCatch try_catch(isolate);
  v8::Local<v8::Value> value;
  if (!context->Global()
           ->Get(context, V8String(isolate, String::FromUtf8(name)))
           .ToLocal(&value) ||
      value->IsNullOrUndefined()) {
    return -1;  // absent, and the engine should hear so
  }
  return AllocJsValue(isolate, value);
}

const char* MerseyScriptRunner::NameForWebId(uint32_t id, std::string& scratch) {
  // An indexed access crosses as the digit-only name; everything else is one of
  // the interned ids, whose spelling this turns back into a property name.
  if (id >= kIndexBase) {
    scratch = std::to_string(id - kIndexBase);
    return scratch.c_str();
  }
  switch (id) {
    case kPathname: return "pathname";
    case kSearch: return "search";
    case kBody: return "body";
    case kTextContent: return "textContent";
    case kLength: return "length";
    case kCreateElement: return "createElement";
    case kAppendChild: return "appendChild";
    case kGetRandomValues: return "getRandomValues";
    case kSetItem: return "setItem";
    case kGetItem: return "getItem";
    case kGetContext: return "getContext";
    case kFillRect: return "fillRect";
    case kWidth: return "width";
    case kHeight: return "height";
    case kClassName: return "className";
    case kClassList: return "classList";
    case kContains: return "contains";
    case kStyle: return "style";
    case kSetProperty: return "setProperty";
    case kGetPropertyValue: return "getPropertyValue";
    case kQuerySelectorAll: return "querySelectorAll";
    case kDispatchEvent: return "dispatchEvent";
    case kEncode: return "encode";
    case kDecode: return "decode";
    case kSelfCall: return "";
    default: return nullptr;  // a constructor id: not a property name
  }
}

bool MerseyScriptRunner::IsReflective(int64_t handle) {
  NativeObject* obj = ObjectFor(handle);
  return obj && obj->kind == NativeObject::Kind::kJs;
}

void MerseyScriptRunner::FillFromV8(msy_reply* out,
                                    v8::Isolate* isolate,
                                    v8::Local<v8::Context> context,
                                    v8::Local<v8::Value> value) {
  if (value.IsEmpty() || value->IsNullOrUndefined()) {
    FillNull(out);
  } else if (value->IsBoolean()) {
    FillBool(out, value->IsTrue());
  } else if (value->IsNumber()) {
    FillNum(out, value.As<v8::Number>()->Value());
  } else if (value->IsString()) {
    FillStr16(out, ToBlinkString<String>(isolate, value.As<v8::String>(),
                                         kExternalize));
  } else {
    FillRef(out, AllocJsValue(isolate, value));
  }
  std::ignore = context;
}

v8::Local<v8::Value> MerseyScriptRunner::V8FromArg16(
    v8::Isolate* isolate,
    v8::Local<v8::Context> context,
    const msy_arg16& arg) {
  switch (arg.kind) {
    case 0:
      return V8String(isolate, Str16ToString(arg.str16, arg.str16_len));
    case 1:
      return v8::Number::New(isolate, arg.num);
    case 2: {
      v8::Local<v8::Value> held =
          JsValueFor(isolate, static_cast<int64_t>(arg.num));
      return held.IsEmpty() ? v8::Null(isolate).As<v8::Value>() : held;
    }
    case 3:
      return v8::Boolean::New(isolate, arg.num != 0);
    case 5:
      return V8Callback(isolate, context, static_cast<uint32_t>(arg.num));
    default:
      return v8::Null(isolate);
  }
}

bool MerseyScriptRunner::ReflectGetU16(int64_t target,
                                       uint32_t id,
                                       msy_reply* out) {
  if (!IsReflective(target)) {
    return false;
  }
  std::string scratch;
  const char* name = NameForWebId(id, scratch);
  ScriptState* script_state = name ? MainWorldScriptStateOrNull() : nullptr;
  if (!script_state) {
    return false;
  }
  ScriptState::Scope scope(script_state);
  v8::MicrotasksScope microtasks(script_state->GetIsolate(),
                                 ToMicrotaskQueue(script_state),
                                 v8::MicrotasksScope::kDoNotRunMicrotasks);
  v8::Isolate* isolate = script_state->GetIsolate();
  v8::Local<v8::Context> context = script_state->GetContext();
  v8::Local<v8::Value> recv = JsValueFor(isolate, target);
  if (recv.IsEmpty()) {
    return false;
  }
  v8::TryCatch try_catch(isolate);
  // `"abc".length` is a read on a primitive: box it the way JS does rather
  // than declining and letting the typed tier answer null.
  v8::Local<v8::Object> recv_obj;
  if (!recv->ToObject(context).ToLocal(&recv_obj)) {
    return false;
  }
  v8::Local<v8::Value> value;
  if (!recv_obj->Get(context, V8String(isolate, String::FromUtf8(name)))
           .ToLocal(&value)) {
    FillNull(out);
    return true;
  }
  FillFromV8(out, isolate, context, value);
  return true;
}

bool MerseyScriptRunner::ReflectSetU16(int64_t target,
                                       uint32_t id,
                                       const msy_arg16& value,
                                       msy_reply* out) {
  if (!IsReflective(target)) {
    return false;
  }
  std::string scratch;
  const char* name = NameForWebId(id, scratch);
  ScriptState* script_state = name ? MainWorldScriptStateOrNull() : nullptr;
  if (!script_state) {
    return false;
  }
  ScriptState::Scope scope(script_state);
  v8::MicrotasksScope microtasks(script_state->GetIsolate(),
                                 ToMicrotaskQueue(script_state),
                                 v8::MicrotasksScope::kDoNotRunMicrotasks);
  v8::Isolate* isolate = script_state->GetIsolate();
  v8::Local<v8::Context> context = script_state->GetContext();
  v8::Local<v8::Value> recv = JsValueFor(isolate, target);
  if (recv.IsEmpty() || !recv->IsObject()) {
    return false;
  }
  v8::TryCatch try_catch(isolate);
  std::ignore = recv.As<v8::Object>()->Set(
      context, V8String(isolate, String::FromUtf8(name)),
      V8FromArg16(isolate, context, value));
  FillNull(out);
  return true;
}

bool MerseyScriptRunner::ReflectCallU16(int64_t target,
                                        uint32_t id,
                                        const msy_arg16* args,
                                        size_t argc,
                                        msy_reply* out) {
  if (!IsReflective(target)) {
    return false;
  }
  std::string scratch;
  const char* name = NameForWebId(id, scratch);
  ScriptState* script_state = name ? MainWorldScriptStateOrNull() : nullptr;
  if (!script_state) {
    return false;
  }
  ScriptState::Scope scope(script_state);
  v8::MicrotasksScope microtasks(script_state->GetIsolate(),
                                 ToMicrotaskQueue(script_state),
                                 v8::MicrotasksScope::kDoNotRunMicrotasks);
  v8::Isolate* isolate = script_state->GetIsolate();
  v8::Local<v8::Context> context = script_state->GetContext();
  v8::Local<v8::Value> recv = JsValueFor(isolate, target);
  if (recv.IsEmpty()) {
    return false;
  }
  v8::TryCatch try_catch(isolate);
  // The empty interned name means the handle is itself callable.
  v8::Local<v8::Value> fn_value = recv;
  v8::Local<v8::Value> this_value = v8::Undefined(isolate).As<v8::Value>();
  if (*name) {
    if (!recv->IsObject()) {
      return false;
    }
    this_value = recv;
    if (!recv.As<v8::Object>()
             ->Get(context, V8String(isolate, String::FromUtf8(name)))
             .ToLocal(&fn_value)) {
      FillNull(out);
      return true;
    }
  }
  if (!fn_value->IsFunction()) {
    return false;  // not a method here; let the typed tier answer
  }
  Vector<v8::Local<v8::Value>> argv;
  for (size_t i = 0; i < argc; ++i) {
    argv.push_back(V8FromArg16(isolate, context, args[i]));
  }
  v8::Local<v8::Value> result;
  if (!fn_value.As<v8::Function>()
           ->Call(context, this_value, static_cast<int>(argv.size()),
                  argv.data())
           .ToLocal(&result)) {
    FillNull(out);
    return true;
  }
  FillFromV8(out, isolate, context, result);
  return true;
}

void MerseyScriptRunner::HostWebRelease(int64_t target) {
  // A value handle (>= kValueBase) — check first, since these are also >= 1.
  if (target >= kValueBase) {
    uint32_t slot = SlotForHandle(target);
    // Drop the JS root with the slot: a kJs handle holds its value strongly,
    // so a page that churns reflective objects would otherwise never let go.
    if (slot < js_values_.size()) {
      js_values_[slot].Reset();
    }
    if (slot < native_objects_.size() &&
        native_objects_[slot].kind != NativeObject::Kind::kEmpty) {
      native_objects_[slot] = NativeObject{};
      native_free_.push_back(slot);
    }
    return;
  }
  // A DOM node handle [1, kValueBase).
  if (target >= 1) {
    uint32_t idx = static_cast<uint32_t>(target - 1);
    if (idx < node_handles_.size() && node_handles_[idx]) {
      node_handles_[idx] = nullptr;
      node_free_.push_back(idx);
    }
  }
}

}  // namespace blink
