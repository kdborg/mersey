/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

#ifndef mozilla_dom_MerseyScriptRunner_h
#define mozilla_dom_MerseyScriptRunner_h

#include <string>
#include <unordered_map>
#include <vector>

#include "mozilla/RefPtr.h"
#include "mozilla/Span.h"
#include "nsTArray.h"
#include "nsCOMPtr.h"
#include "nsIDOMEventListener.h"
#include "nsISupportsImpl.h"
#include "nsString.h"
#include "mersey.h"  // the engine's C ABI (dom/mersey/include)

struct JSContext;

namespace mozilla::dom {

class Document;
class Storage;
class URL;

// One Mersey engine context per Document, created on the first
// <script type="text/mersey"> and torn down with the Document.
//
// The Blink half of this lives in the Chromium fork
// (core/script/mersey_script_runner.cc) and is the same code with different DOM
// types, because the boundary is the same: one `msy_host_table`, the same ten
// `msy_*` symbols, the same return codes. That is the whole point of having put
// the loader in `mersey_interp::embed` — two hosts, one implementation.
//
// What Gecko gives us that Blink did not: the engine is a *crate* of gkrust
// (dom/mersey/rust), so it shares Firefox's one `std` and one allocator. No
// second stdlib, no duplicate `rust_eh_personality`, no bundled `.so` needing a
// soname and an rpath. There is one link.
class MerseyScriptRunner final {
 public:
  NS_INLINE_DECL_REFCOUNTING(MerseyScriptRunner)

  // The runner for this document, created on first use.
  static MerseyScriptRunner* For(Document* aDocument);

  explicit MerseyScriptRunner(Document* aDocument);

  // Compile and run one inline script. Diagnostics and runtime errors go to the
  // console; the return mirrors the ABI (0 ran, 1 diagnostics, 2 threw).
  uint32_t Run(const nsAString& aSource);

  // Fire an engine callback (an event listener).
  void Invoke(uint32_t aCallbackId);
  // …with JSON arguments (an event object, a settled promise value). Called by
  // the bridge's __merseyInvoke when a Mersey closure is invoked from JS.
  void ReplTurn(const nsACString& aSource, nsACString& aOut);
  // The session's visible top-level names, as the engine's JSON array.
  void ReplCompletions(nsACString& aOut);

  // ---- DevTools debugger (msy_context_debug_*) -----------------------------
  //
  // Pausing IS blocking: the engine sits inside its callout until released.
  // The pause holds by spinning the event loop (Gecko's own idiom for a
  // synchronous wait), which keeps the DevTools server actors responsive; an
  // observer notification ("mersey-debugger-paused", JSON with the inner
  // window id) tells them why the world stopped.
  void DebugScriptsJson(nsACString& aOut);
  // REPLACES the whole breakpoint set (engine 1-based lines; empty clears).
  // Setting breakpoints installs the debug hook itself — a separate "attach"
  // step is a trap that fails silently.
  void DebugSetBreakpoints(const nsTArray<uint32_t>& aLines);
  // "pause" | "resume" | "stepOver" | "stepInto" | "stepOut" | "disable".
  void DebugAction(const nsACString& aAction);
  void InvokeArgs(uint32_t aCallbackId, const nsACString& aArgsJson);

  // Host-table entry points; the static shims bounce here through `data`.
  void HostPrint(std::string_view aMessage, bool aIsError);
  void HostSetText(std::string_view aId, std::string_view aText);
  const std::string* HostGetText(std::string_view aId);
  void HostAddListener(std::string_view aId, std::string_view aEvent,
                       uint32_t aCallbackId);
  double HostTimeMs(bool aEpoch);
  bool HostRandomBytes(uint8_t* aBuf, size_t aLength);

  // The universal web bridge (the same reflective bridge the WASM polyfill
  // uses, injected into the page realm). `aReply` receives tagged-JSON, valid
  // until the next host call.
  int64_t HostWebGlobal(std::string_view aName);
  const std::string* HostWebGet(int64_t aTarget, std::string_view aProp);
  const std::string* HostWebSet(int64_t aTarget, std::string_view aProp,
                                std::string_view aValueJson);
  const std::string* HostWebCall(int64_t aTarget, std::string_view aMethod,
                                 std::string_view aArgsJson);
  const std::string* HostWebNew(std::string_view aCtor,
                                std::string_view aArgsJson);
  const std::string* HostWebIterate(int64_t aTarget);
  bool HostWebInstanceof(int64_t aTarget, int64_t aCtor);
  void HostWebRelease(int64_t aTarget);

  // Interned fast paths (ABI v3): a member name crosses once as an id, and
  // scalar values skip JSON. These reach the bridge's getId/setScalar/callStr,
  // passing the handle and id as JS numbers rather than stringifying them.
  uint32_t HostWebIntern(std::string_view aName);
  const std::string* HostWebGetId(int64_t aTarget, uint32_t aNameId);
  const std::string* HostWebSetStr(int64_t aTarget, uint32_t aNameId,
                                   std::string_view aValue);
  const std::string* HostWebSetNum(int64_t aTarget, uint32_t aNameId,
                                   double aValue);
  const std::string* HostWebCallStr(int64_t aTarget, uint32_t aNameId,
                                    std::string_view aArg);
  // Multi-argument scalar fast paths: setItem(k,v), fillRect(x,y,w,h), new URL(s).
  const std::string* HostWebCallScalars(int64_t aTarget, uint32_t aNameId,
                                        const msy_scalar* aArgs, size_t aArgc);
  const std::string* HostWebNewScalars(uint32_t aCtorId, const msy_scalar* aArgs,
                                       size_t aArgc);

  // Wide-string fast paths (ABI v5): UTF-16 arguments in, a typed UTF-16 reply
  // out — no encoding conversion and no JSON. The engine is UTF-16 and so is
  // Gecko, so nsString's char16_t and the engine's code units are the same.
  void HostWebGetU16(int64_t aTarget, uint32_t aNameId, msy_reply* aOut);
  void HostWebSetU16(int64_t aTarget, uint32_t aNameId, const msy_arg16* aValue,
                     msy_reply* aOut);
  void HostWebCallU16(int64_t aTarget, uint32_t aNameId, const msy_arg16* aArgs,
                      size_t aArgc, msy_reply* aOut);
  void HostWebNewU16(uint32_t aCtorId, const msy_arg16* aArgs, size_t aArgc,
                     msy_reply* aOut);
  // Typed-binding fast path (ABI v7): a compiled numeric web method as a
  // compile-time id + raw doubles. Switches straight to the C++ method, no
  // interned name and no string dispatch. Falls back to the reflective path on
  // a receiver-type mismatch.
  void HostWebBind(int64_t aTarget, uint32_t aBindId, const double* aArgs,
                   size_t aArgc, msy_reply* aOut);

 private:
  ~MerseyScriptRunner();

  // Inject the bridge JS into the page realm on first use; returns false if the
  // realm is not available (no window).
  bool EnsureBridge();
  // Call one bridge method with string args; the reply lands in mBridgeScratch.
  const std::string* CallBridgeStr(const char* aMethod,
                                   std::initializer_list<std::string_view> aArgs);
  int64_t CallBridgeInt(const char* aMethod,
                        std::initializer_list<std::string_view> aArgs);

  // A bridge argument that is either a JS number or a JS string — the interned
  // fast paths pass the handle and id as numbers, avoiding a stringify here and
  // a parse in JS.
  // Fields are primitives only (no std::string_view), so Servo's bindgen —
  // which parses this header — can lay the struct out; the string_view appears
  // only in a constructor parameter, which bindgen tolerates.
  struct BridgeArg {
    bool isNum;
    double num;
    const char* strData;
    size_t strLen;
    BridgeArg(double n) : isNum(true), num(n), strData(nullptr), strLen(0) {}
    BridgeArg(int64_t n) : isNum(true), num(double(n)), strData(nullptr), strLen(0) {}
    BridgeArg(uint32_t n) : isNum(true), num(double(n)), strData(nullptr), strLen(0) {}
    BridgeArg(std::string_view s)
        : isNum(false), num(0), strData(s.data()), strLen(s.size()) {}
  };
  // Call a bridge method with mixed number/string args; reply in mBridgeScratch.
  const std::string* CallBridgeValues(const char* aMethod,
                                      mozilla::Span<const BridgeArg> aArgs);
  // Wide-string bridge call: `aLead` numbers (a target+id, or a ctor id) then
  // the UTF-32 args as JS strings; the raw (un-JSON'd) result is typed into aOut.
  void CallBridgeWide(const char* aMethod, const double* aLead, size_t aLeadCount,
                      const msy_arg16* aArgs, size_t aArgc, msy_reply* aOut);
  const std::string* CallBridgeValues(
      const char* aMethod, std::initializer_list<BridgeArg> aArgs) {
    return CallBridgeValues(aMethod,
                            mozilla::Span<const BridgeArg>(aArgs.begin(), aArgs.size()));
  }

  // Direct C++ for hot DOM/canvas operations: a handle is unwrapped ONCE to its
  // native object (one realm entry), then fillRect / appendChild / textContent
  // call Gecko's C++ with no JS at all. The bridge's handles[] array roots the
  // JS wrapper, and the wrapper keeps the native alive, so a cached pointer is
  // valid until the handle is released (which erases the cache entry).
  enum : int32_t {
    kNativeOther = 0,
    kNativeElement = 1,
    kNativeCanvas2D = 2,
    kNativeDocument = 3,
    kNativeURL = 4,
    kNativeBytes = 5,
    kNativeTokenList = 6,    // el.classList        -> nsDOMTokenList
    kNativeStyle = 7,        // el.style            -> nsICSSDeclaration
    kNativeNodeList = 8,     // querySelectorAll    -> dom::NodeList
    kNativeEvent = 9,        // new Event(type)     -> dom::Event
    kNativeTextEncoder = 10, // new TextEncoder()   (stateless; UTF-8 codec)
    kNativeTextDecoder = 11, // new TextDecoder()   (stateless; UTF-8 codec)
  };
  void* UnwrapNative(int64_t aHandle, int32_t& aKindOut);
  std::unordered_map<int64_t, std::pair<int32_t, void*>> mNativeCache;

  // A one-entry unwrap cache in front of UnwrapNative. A tight web loop
  // (`while (…) ctx.fillRect(…)`) hits the same receiver every iteration, so a
  // single remembered (handle → kind, ptr) turns the per-call hashmap probe
  // into a compare. Invalidated whenever a handle is released (its native may
  // be freed and the slot id reused).
  int64_t mBindCacheHandle = 0;
  int32_t mBindCacheKind = kNativeOther;
  void* mBindCachePtr = nullptr;
  void* UnwrapCached(int64_t aHandle, int32_t& aKindOut);

  // C++-CREATED objects: negative handles that never enter JS at all.
  // `new URL(s)`, `document.createElement(t)`, `new Uint8Array(n)` construct
  // the Gecko object (or a plain byte buffer) directly; every fast-pathed
  // operation dispatches on the kind. Only if such an object escapes to an
  // API the fast paths don't cover is its JS reflector materialized — once —
  // and adopted into the bridge's handle table under the same id, so identity
  // is preserved on both sides.
  struct NativeObj {
    int32_t kind = kNativeOther;
    nsCOMPtr<nsISupports> holder;  // keeps the Gecko object alive
    void* ptr = nullptr;           // typed pointer for dispatch
    nsTArray<uint8_t> bytes;       // kNativeBytes storage
    bool materialized = false;
    bool live = false;             // false = a freed slot awaiting reuse
  };
  // Negative handles index this slot vector directly (h → slot = -h - 2), with
  // a freelist so a released slot — and its handle id — is reused. Creation and
  // every subsequent access are then O(1) with no per-object allocation, which
  // matters because the URL/DOM workloads create and drop thousands of these.
  // Reuse is safe: the engine calls web_release exactly when a handle's last
  // reference dies, so no live engine value ever names a freed slot.
  std::vector<NativeObj> mNativeSlots;
  std::vector<uint32_t> mNativeFree;
  static int64_t HandleForSlot(uint32_t aSlot) { return -int64_t(aSlot) - 2; }
  static uint32_t SlotForHandle(int64_t aHandle) {
    return uint32_t(-aHandle - 2);
  }
  // The live native object for a negative handle, or nullptr.
  NativeObj* NativeFor(int64_t aHandle);
  int64_t AddNativeObj(NativeObj&& aObj);
  // Materialize the reflector for a negative handle (no-op otherwise) so the
  // JS bridge paths can proceed; returns false if that is impossible.
  bool EnsureReflector(int64_t aHandle);

  // Direct C++ Web Storage: localStorage / sessionStorage bypass the JS bridge
  // entirely — getItem/setItem/removeItem/length run against the real Storage
  // object in C++, no realm entry, no reply JSON round-trip. The handles are
  // learned from web_global; StorageFor maps a target handle to its Storage.
  Storage* StorageFor(int64_t aTarget);
  // The interned method name for an id, or "" — mirrors the bridge's own table,
  // so a storage call arriving as an id can be dispatched by name in C++.
  const std::string& InternedName(uint32_t aNameId) const;

  static void DebugPausedShim(void* aData, const char* aJson, size_t aLen);
  void OnDebugPaused(const nsACString& aSnapshotJson);
  void EnsureDebugEnabled();
  // The inline sources this document has run, in order — what the DevTools
  // panel lists and sets breakpoints in (engine lines; no offsets).
  nsTArray<nsCString> mDebugScripts;
  bool mDebugEnabled = false;
  bool mDebugResumeRequested = false;

  Document* mDocument;  // weak; the document owns us
  msy_context* mContext = nullptr;
  bool mBridgeReady = false;
  int64_t mLocalStorageHandle = INT64_MIN;
  int64_t mSessionStorageHandle = INT64_MIN;
  RefPtr<Storage> mLocalStorage;
  RefPtr<Storage> mSessionStorage;
  std::vector<std::string> mInternedNames;
  // Backs msy_reply::str16 for the wide-string path; valid until the next call.
  nsString mReplyScratch16;
  // Set while we are inside a synchronous engine run or callback that has
  // already entered the page realm. When set, the per-call bridge helpers reuse
  // it instead of paying for a fresh AutoEntryScript on every single web call.
  JSContext* mActiveCx = nullptr;
  std::string mBridgeScratch;
  // Backing store for HostGetText's reply-pointer contract: valid until the
  // next call across the boundary.
  std::string mGetTextScratch;
};

// A DOM listener that re-enters the engine. Firing it is an ordinary Gecko
// event dispatch; what it dispatches *to* is compiled Mersey.
class MerseyEventListener final : public nsIDOMEventListener {
 public:
  NS_DECL_ISUPPORTS
  NS_DECL_NSIDOMEVENTLISTENER

  MerseyEventListener(MerseyScriptRunner* aRunner, uint32_t aCallbackId)
      : mRunner(aRunner), mCallbackId(aCallbackId) {}

 private:
  ~MerseyEventListener() = default;

  RefPtr<MerseyScriptRunner> mRunner;
  uint32_t mCallbackId;
};

}  // namespace mozilla::dom

#endif  // mozilla_dom_MerseyScriptRunner_h
