// Mersey fork. Use of this source code is governed by the Chromium BSD-style
// license and the Mersey project license.

#ifndef THIRD_PARTY_BLINK_RENDERER_CORE_SCRIPT_MERSEY_SCRIPT_RUNNER_H_
#define THIRD_PARTY_BLINK_RENDERER_CORE_SCRIPT_MERSEY_SCRIPT_RUNNER_H_

#include "base/functional/callback.h"

#include <cstdint>
#include <map>
#include <string>
#include <vector>

#include "third_party/blink/renderer/core/core_export.h"
#include "third_party/blink/renderer/platform/allow_discouraged_type.h"
#include "third_party/blink/renderer/platform/heap/collection_support/heap_vector.h"
#include "third_party/blink/renderer/platform/heap/garbage_collected.h"
#include "third_party/blink/renderer/platform/heap/member.h"
#include "third_party/blink/renderer/platform/heap/persistent.h"
#include "third_party/blink/renderer/platform/scheduler/public/post_cancellable_task.h"
#include "third_party/blink/renderer/platform/supplementable.h"
#include "third_party/blink/renderer/platform/weborigin/kurl.h"
#include "third_party/blink/renderer/platform/wtf/text/wtf_string.h"
#include "third_party/mersey/include/mersey.h"

namespace blink {

class Blob;
class CSSStyleDeclaration;
class DOMMatrix;
class DOMTokenList;
class Document;
class Event;
class HTMLCanvasElement;
class LocalDOMWindow;
class Node;
class NodeList;

// A core->modules hook. `core` cannot depend on `modules`, but some web APIs
// (localStorage) live there — so modules fills this table at startup
// (ModulesInitializer) and the core script runner calls through it. An unset
// member means the capability is absent (the runner reports the global as -1).
struct MerseyModulesBridge {
  String (*local_storage_get)(LocalDOMWindow*, const String& key) = nullptr;
  void (*local_storage_set)(LocalDOMWindow*,
                            const String& key,
                            const String& value) = nullptr;
  // Canvas 2D also lives in modules. get_context registers the canvas's 2D
  // context (rooted on the modules side) and returns an id (>= 1), or -1;
  // fill_rect draws on the context with that id.
  int (*canvas_get_context)(HTMLCanvasElement*) = nullptr;
  void (*canvas_fill_rect)(int ctx_id,
                           double x,
                           double y,
                           double w,
                           double h) = nullptr;
};
CORE_EXPORT MerseyModulesBridge& GetMerseyModulesBridge();

// One Mersey engine context per Document, created on the first
// <script type="text/mersey"> and torn down with the Document.
//
// This is the Blink half of the boundary proven in the engine repository by
// native/host_demo.c and mersey_capi/tests/abi.rs: the same msy_host_table,
// the same return codes, the same buffer-lifetime rules. The initial fork
// wires the console and the small element surface (getElementById /
// textContent / event listeners); the universal web bridge (web_get/web_call
// over real DOM objects) is the tracked follow-up and slots into the same
// table without touching this class's callers.
class CORE_EXPORT MerseyScriptRunner final
    : public GarbageCollected<MerseyScriptRunner>,
      public Supplement<Document> {
 public:
  // A Mersey source this document has executed. `start_line` is the 0-based
  // DOCUMENT line of the source's first line - the script tag's line - which
  // lets the Sources panel map gutter clicks to engine lines and back.
  struct RecordedScript {
    String url;
    // The engine's own name for this module — a graph module's spec
    // ("demo/todo.mersey"), "<script>" for an inline block. Pause frames
    // report THIS name, so DevTools matches frames to sources with it.
    String spec;
    int start_line = 0;
    String source;
  };

  static const char kSupplementName[];
  static MerseyScriptRunner& From(Document&);

  explicit MerseyScriptRunner(Document&);
  ~MerseyScriptRunner();

  // Compile and run one inline script. Diagnostics and runtime errors land on
  // the console; the return mirrors the ABI (0 ran, 1 diagnostics, 2 threw).
  uint32_t Run(const String& source_text, int start_line = 0);

  // Load and run an EXTERNAL <script type="text/mersey" src=…> natively: fetch
  // the entry and its import graph, then run_graph. `entry_spec` is the src as
  // written (page-relative), the base for relative-import resolution.
  void LoadExternalGraph(const String& entry_spec);
  // Helpers the external loader drives (thin wrappers over the ABI).
  String ScanImportsJson(const String& source);
  void RunGraph(const String& payload_json);

  // Fire an engine callback (posted from an event listener).
  void Invoke(uint32_t callback_id);

  // One console turn. Returns the engine's raw reply: the echo of a trailing
  // bare expression, "" when the turn only declared something, "!"-prefixed
  // diagnostics for a REJECTED turn (it never ran), or a "runtime error:"
  // prefix when it threw. The DevTools agent parses those prefixes; the
  // attribute shim below hands the string through verbatim.
  // ---- debugger (msy_context_debug_*) ----------------------------------
  //
  // Pausing IS blocking: `paused` is invoked from inside the engine and the
  // engine does not continue until it returns. The caller holds the pause by
  // running a nested message loop in that callback.
  using MerseyPausedCallback = base::RepeatingCallback<void(const String&)>;
  void DebugEnable(MerseyPausedCallback paused);
  void DebugDisable();
  // REPLACES the breakpoint set for `source` (empty matches every module).
  void DebugSetBreakpoints(const String& source, const Vector<int>& lines);
  void DebugPause();
  void DebugResume();
  void DebugStepOver();
  void DebugStepInto();
  void DebugStepOut();

 private:
  // The C-ABI shim: bounces to the runner through `data`.
  static void DebugPausedShim(void* data, const char* json, size_t len);
  MerseyPausedCallback debug_paused_;
  // Recorded for the DevTools front-end; the engine keeps no source table.
// Recorded for the DevTools front-end; the engine keeps no source table.
  Vector<RecordedScript> scripts_;

 public:
  // Public: the external module-graph loader (a separate class) records the
  // modules it fetched.
  void RecordScript(const String& url, const String& spec, int start_line,
                    const String& source);

 private:

 public:

  // The Mersey sources this document has executed, url -> text, in the order
  // they ran. The console session is refreshed in place each turn.
  const Vector<RecordedScript>& Scripts() const { return scripts_; }

  String ReplTurn(const String& source);
  // The session's visible top-level names, as the engine's JSON array.
  String ReplCompletionsJson();

  void AnnounceNative();
  void ReplTurnViaAttributes(Event*);
  void ReplCompleteViaAttributes();

  void Trace(Visitor*) const override;

  // Host-table entry points (static shims bounce here through `data`).
  void HostPrint(std::string_view message, bool is_error);
  void HostSetText(std::string_view id, std::string_view text);
  // Returns the scratch-backed text; valid until the next host call.
  const std::string* HostGetText(std::string_view id);
  void HostAddListener(std::string_view id,
                       std::string_view event,
                       uint32_t callback_id);
  double HostTimeMs(bool epoch);
  bool HostRandomBytes(uint8_t* buf, size_t n);

  // ---- the universal web bridge (direct C++ on Blink objects) --------------
  // Resolve an ambient global (`document`, `URL`, `localStorage`, …) to a
  // handle; -1 means "no such global". Reflective get/set/call/new return a
  // tagged-JSON reply valid until the next host call (`*out_len` set by the
  // shim). The first slice wires `new URL(s)` and its string components.
  int64_t HostWebGlobal(std::string_view name);
  const std::string* HostWebGet(int64_t target, std::string_view prop);
  const std::string* HostWebSet(int64_t target,
                                std::string_view prop,
                                std::string_view value_json);
  const std::string* HostWebCall(int64_t target,
                                 std::string_view method,
                                 std::string_view args_json);
  const std::string* HostWebNew(std::string_view ctor, std::string_view args_json);
  void HostWebRelease(int64_t target);

  // Interned fast paths (ABI v3/v5): a member/method name crosses once as an id,
  // then scalar values cross typed (no JSON). We intern the handful of names the
  // bridged workloads use and switch on the id; unknown names decline
  // (UINT32_MAX) and fall back to the reflective ops above.
  uint32_t HostWebIntern(std::string_view name);
  void HostWebGetU16(int64_t target, uint32_t id, msy_reply* out);
  void HostWebSetU16(int64_t target,
                     uint32_t id,
                     const msy_arg16* value,
                     msy_reply* out);
  void HostWebCallU16(int64_t target,
                      uint32_t id,
                      const msy_arg16* args,
                      size_t argc,
                      msy_reply* out);
  void HostWebNewU16(uint32_t ctor_id,
                     const msy_arg16* args,
                     size_t argc,
                     msy_reply* out);
  // Batched DOM mutation (web_apply, ABI v10): a whole render's ops in one call.
  size_t HostWebApply(const int32_t* ops,
                      size_t nops,
                      const int64_t* nodes,
                      size_t nnodes,
                      const msy_str16* strs,
                      size_t nstrs,
                      int64_t* created_out,
                      size_t created_cap);

 private:
  // Interned-name ids the fast paths switch on. Append-only.
  enum WebId : uint32_t {
    kPathname = 1,
    kSearch,
    kBody,
    kTextContent,
    kLength,
    kCreateElement,
    kAppendChild,
    kGetRandomValues,
    kSetItem,
    kGetItem,
    kCtorURL,
    kCtorUint8Array,
    kGetContext,
    kFillRect,
    kWidth,
    kHeight,
    kClassName,
    kClassList,
    kContains,
    kStyle,
    kSetProperty,
    kGetPropertyValue,
    kQuerySelectorAll,
    kDispatchEvent,
    kCtorEvent,
    kEncode,
    kDecode,
    kCtorTextEncoder,
    kCtorTextDecoder,
    // The interned EMPTY name (ABI v8): the target handle is itself callable
    // (an imported `setTimeout(cb, ms)`, `fetch(url)`).
    kSelfCall,
  };
  // A digit-only interned name is an indexed access (`nodes[i]` crosses as a
  // property read of "42"): id = kIndexBase + the index. Above every WebId.
  static constexpr uint32_t kIndexBase = 0x100000;
  // Typed-reply fillers (msy_reply lifetime: until the next boundary call).
  void FillNull(msy_reply* out);
  void FillNum(msy_reply* out, double value);
  void FillBool(msy_reply* out, bool value);
  void FillRef(msy_reply* out, int64_t handle);
  void FillStr16(msy_reply* out, const String& value);
  // Fire-and-forget for a timer armed by the engine (`setTimeout`).
  void FireTimer(int timer_id, uint32_t callback_id);
  // A host object the engine holds by handle. Native (C++-created) objects use
  // negative handles (<= -2, matching the engine's convention), so they never
  // enter JS and release is O(1). `handle = -slot - 2`.
  struct NativeObject {
    enum class Kind {
      kEmpty,
      kGlobal,
      kUrl,
      kBytes,
      kCanvasCtx,
      kTokenList,
      kStyle,
      kNodeList,
      kEvent,
      kTextEncoder,
      kTextDecoder,
      kMatrix,
      kBlob,
    };
    Kind kind = Kind::kEmpty;
    std::string name;           // kGlobal: the global's name
    KURL url;                   // kUrl
    std::vector<uint8_t> bytes  // kBytes (e.g. a Uint8Array)
        ALLOW_DISCOURAGED_TYPE("raw byte payload crossing the engine C ABI");
    int canvas_ctx_id = -1;     // kCanvasCtx: id into the modules-side registry
    // Oilpan-managed web objects the engine holds by handle. Persistent (not
    // Member): this table is an untraced std::vector, and the freelist recycles
    // the slot on web_release, so nothing leaks past the page.
    Persistent<DOMTokenList> token_list;   // kTokenList
    Persistent<CSSStyleDeclaration> style; // kStyle
    Persistent<NodeList> node_list;        // kNodeList
    Persistent<Event> event;               // kEvent
    Persistent<DOMMatrix> matrix;          // kMatrix
    Persistent<Blob> blob;                 // kBlob
  };
  // Value objects (URL, byte buffers, global proxies) take handles >= kValueBase;
  // Oilpan node handles occupy [1, kValueBase). BOTH are non-negative, which is
  // required: the engine reads a global whose handle is < 0 as "absent". Routing
  // is by magnitude.
  static constexpr int64_t kValueBase = 0x40000000;
  static int64_t HandleForSlot(uint32_t slot) {
    return kValueBase + static_cast<int64_t>(slot);
  }
  static uint32_t SlotForHandle(int64_t handle) {
    return static_cast<uint32_t>(handle - kValueBase);
  }
  NativeObject* ObjectFor(int64_t handle);
  int64_t AllocObject(NativeObject obj);
  // Oilpan DOM node handles (document, elements) live in a traced table with
  // *non-negative* handles (handle = index + 1). This matters for `document`:
  // the engine reads `web_global("document") < 0` as "unavailable" and falls
  // back to the fake-DOM namespace, so a global must hand back a >= 0 handle.
  // Value objects (URL) keep negative handles, so a handle routes by sign.
  int64_t AllocNode(Node* node);
  Node* NodeFor(int64_t handle);
  // Stash `json` (UTF-8) in the reply scratch and return it — the engine reads
  // it before the next call across the boundary.
  const std::string* StashReply(const String& json);
  const std::string* ReplyOkString(const String& value);
  const std::string* ReplyOkInt(int64_t value);
  const std::string* ReplyOkDouble(double value);
  const std::string* ReplyOkRef(int64_t handle);
  const std::string* ReplyOkNull();
  const std::string* ReplyErr(const String& message);

  msy_context* context_ = nullptr;
  // The one-time `window.merseyNative = true` announcement happened.
  bool announced_native_ = false;
  // Backing store for HostGetText's reply-pointer contract.
  std::string get_text_scratch_;
  // The value handle table (URL/global; handle = -slot - 2) and its freelist.
  std::vector<NativeObject> native_objects_
      ALLOW_DISCOURAGED_TYPE("engine handle table; NativeObject is not GC-traced");
  std::vector<uint32_t> native_free_
      ALLOW_DISCOURAGED_TYPE("freelist of raw slot indices");
  // The Oilpan node handle table (traced) and its freelist.
  HeapVector<Member<Node>> node_handles_;
  std::vector<uint32_t> node_free_
      ALLOW_DISCOURAGED_TYPE("freelist of raw slot indices");
  // Backing store for the web bridge's reply-pointer contract.
  std::string web_reply_scratch_;
  // Backing store for a typed UTF-16 reply (msy_reply::str16).
  std::vector<uint16_t> reply_str16_
      ALLOW_DISCOURAGED_TYPE("UTF-16 code units for the engine reply-pointer contract");
  // Timers the engine armed (`setTimeout`): id -> cancellable task. The bench
  // workload arms + clears; a fired timer erases itself in FireTimer.
  std::map<int, TaskHandle> timers_
      ALLOW_DISCOURAGED_TYPE("small engine timer-id -> task map");
  int next_timer_id_ = 1;
};

}  // namespace blink

#endif  // THIRD_PARTY_BLINK_RENDERER_CORE_SCRIPT_MERSEY_SCRIPT_RUNNER_H_
