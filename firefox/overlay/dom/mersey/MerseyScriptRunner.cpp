/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

#include "mozilla/dom/MerseyScriptRunner.h"
#include "mozilla/Services.h"
#include "mozilla/SpinEventLoopUntil.h"
#include "nsIObserverService.h"

#include "js/CallAndConstruct.h"
#include "js/CompilationAndEvaluation.h"
#include "js/Conversions.h"
#include "js/PropertyAndElement.h"
#include "js/SourceText.h"
#include "mozilla/dom/AutoEntryScript.h"
#include "mozilla/dom/BindingUtils.h"
#include "mozilla/dom/CanvasRenderingContext2D.h"
#include "mozilla/dom/CanvasRenderingContext2DBinding.h"
#include "mozilla/dom/Document.h"
#include "mozilla/dom/Element.h"
#include "mozilla/dom/ElementBinding.h"
#include "mozilla/dom/Event.h"
#include "mozilla/dom/NodeList.h"
#include "nsDOMCSSDeclaration.h"
#include "nsDOMTokenList.h"
#include "nsICSSDeclaration.h"
#include "nsStyledElement.h"
#include "mozilla/dom/MerseyBridgeSource.h"
#include "mozilla/dom/Storage.h"
#include "mozilla/dom/ToJSValue.h"
#include "mozilla/dom/URL.h"
#include "nsIURI.h"
#include "js/experimental/TypedData.h"
#include "nsIPrincipal.h"
#include "nsNameSpaceManager.h"
#include "mozilla/dom/ScriptSettings.h"
#include "mozilla/ErrorResult.h"
#include "mozilla/Maybe.h"
#include "mozilla/TimeStamp.h"
#include "nsContentUtils.h"
#include "nsGlobalWindowInner.h"
#include "nsIGlobalObject.h"
#include "nsIRandomGenerator.h"
#include "nsJSUtils.h"
#include "nsServiceManagerUtils.h"
#include "prtime.h"
#include "xpcpublic.h"

namespace mozilla::dom {

// ---- the host table -------------------------------------------------------
//
// `data` is the runner. Everything the engine can do to the outside world goes
// through these ten pointers and nowhere else.

namespace {

MerseyScriptRunner* Runner(void* aData) {
  return static_cast<MerseyScriptRunner*>(aData);
}

// Point a reply at a UTF-16 string held in aScratch (valid until the next call).
void ReplyStr16(nsString& aScratch, const nsAString& aValue, int32_t aTag,
                msy_reply* aOut) {
  aScratch = aValue;
  aOut->tag = aTag;
  aOut->str16 = reinterpret_cast<const uint16_t*>(aScratch.BeginReading());
  aOut->str16_len = aScratch.Length();
}

// Type a raw JS value (from getWide/callWide) into a reply: a scalar as itself,
// {r: handle} as a ref, {j: json} as a JSON blob. Strings land in aScratch.
void FillReplyFromJs(JSContext* aCx, JS::Handle<JS::Value> aVal,
                     nsString& aScratch, msy_reply* aOut) {
  if (aVal.isString()) {
    nsAutoJSString s;
    if (s.init(aCx, aVal.toString())) {
      ReplyStr16(aScratch, s, 2, aOut);
    }
    return;
  }
  if (aVal.isNumber()) {
    aOut->tag = 1;
    aOut->num = aVal.toNumber();
    return;
  }
  if (aVal.isBoolean()) {
    aOut->tag = 4;
    aOut->num = aVal.toBoolean() ? 1.0 : 0.0;
    return;
  }
  if (aVal.isObject()) {
    JS::Rooted<JSObject*> obj(aCx, &aVal.toObject());
    JS::Rooted<JS::Value> prop(aCx);
    if (JS_GetProperty(aCx, obj, "r", &prop) && prop.isNumber()) {
      aOut->tag = 3;  // host object -> ref handle
      aOut->num = prop.toNumber();
      return;
    }
    if (JS_GetProperty(aCx, obj, "j", &prop) && prop.isString()) {
      nsAutoJSString s;
      if (s.init(aCx, prop.toString())) {
        ReplyStr16(aScratch, s, 7, aOut);  // rare non-scalar -> tagged JSON
      }
      return;
    }
  }
  aOut->tag = 0;  // null / undefined / unexpected
}

// A thrown bridge error -> a typed error reply (tag 5), message as UTF-16.
void FillReplyError(JSContext* aCx, nsString& aScratch, msy_reply* aOut) {
  nsAutoString msg;
  JS::Rooted<JS::Value> exc(aCx);
  if (JS_GetPendingException(aCx, &exc)) {
    JS_ClearPendingException(aCx);
    JS::Rooted<JSString*> s(aCx, JS::ToString(aCx, exc));
    nsAutoJSString js;
    if (s && js.init(aCx, s)) {
      msg = js;
    }
  }
  if (msg.IsEmpty()) {
    msg.AssignLiteral(u"bridge error");
  }
  ReplyStr16(aScratch, msg, 5, aOut);
}

// Append a JSON string literal (quotes included) for a UTF-16 value — used to
// build tagged-JSON replies for the direct-C++ storage path without pulling in
// a JSON writer. Matches what the webjson wire format expects.
void AppendJsonString(std::string& aOut, const nsAString& aValue) {
  NS_ConvertUTF16toUTF8 utf8(aValue);
  aOut.push_back('"');
  for (unsigned char c : std::string_view(utf8.get(), utf8.Length())) {
    switch (c) {
      case '"': aOut += "\\\""; break;
      case '\\': aOut += "\\\\"; break;
      case '\n': aOut += "\\n"; break;
      case '\r': aOut += "\\r"; break;
      case '\t': aOut += "\\t"; break;
      default:
        if (c < 0x20) {
          char buf[8];
          snprintf(buf, sizeof(buf), "\\u%04x", c);
          aOut += buf;
        } else {
          aOut.push_back(static_cast<char>(c));  // includes UTF-8 continuation bytes
        }
    }
  }
  aOut.push_back('"');
}

void HostPrintShim(void* aData, const char* aUtf8, size_t aLen) {
  Runner(aData)->HostPrint(std::string_view(aUtf8, aLen), /* isError */ false);
}

void HostErrorShim(void* aData, const char* aUtf8, size_t aLen) {
  Runner(aData)->HostPrint(std::string_view(aUtf8, aLen), /* isError */ true);
}

void HostSetTextShim(void* aData, const char* aId, size_t aIdLen,
                     const char* aText, size_t aTextLen) {
  Runner(aData)->HostSetText(std::string_view(aId, aIdLen),
                             std::string_view(aText, aTextLen));
}

const char* HostGetTextShim(void* aData, const char* aId, size_t aIdLen,
                            size_t* aOutLen) {
  const std::string* text =
      Runner(aData)->HostGetText(std::string_view(aId, aIdLen));
  if (!text) {
    return nullptr;
  }
  *aOutLen = text->size();
  return text->data();
}

void HostAddListenerShim(void* aData, const char* aId, size_t aIdLen,
                         const char* aEvent, size_t aEventLen, uint32_t aCb) {
  Runner(aData)->HostAddListener(std::string_view(aId, aIdLen),
                                 std::string_view(aEvent, aEventLen), aCb);
}

double HostTimeMsShim(void* aData, int32_t aEpoch) {
  return Runner(aData)->HostTimeMs(aEpoch != 0);
}

int32_t HostRandomBytesShim(void* aData, uint8_t* aBuf, size_t aLen) {
  return Runner(aData)->HostRandomBytes(aBuf, aLen) ? 0 : 1;
}

const char* HostCapsShim(void*, size_t* aOutLen) {
  // The universal bridge is wired now, so the full web surface is reachable —
  // the engine still gates each API by import (spec §5.4).
  static constexpr char kCaps[] =
      "[\"dom\",\"web\",\"time\",\"random\",\"net\",\"storage\"]";
  *aOutLen = sizeof(kCaps) - 1;
  return kCaps;
}

int64_t HostWebGlobalShim(void* aData, const char* aName, size_t aLen) {
  return Runner(aData)->HostWebGlobal(std::string_view(aName, aLen));
}

const char* Reply(const std::string* aStr, size_t* aOutLen) {
  if (!aStr) {
    return nullptr;
  }
  *aOutLen = aStr->size();
  return aStr->data();
}

const char* HostWebGetShim(void* aData, int64_t aTarget, const char* aProp,
                           size_t aPropLen, size_t* aOutLen) {
  return Reply(Runner(aData)->HostWebGet(aTarget, std::string_view(aProp, aPropLen)),
               aOutLen);
}

const char* HostWebSetShim(void* aData, int64_t aTarget, const char* aProp,
                           size_t aPropLen, const char* aValue, size_t aValueLen,
                           size_t* aOutLen) {
  return Reply(Runner(aData)->HostWebSet(aTarget, std::string_view(aProp, aPropLen),
                                         std::string_view(aValue, aValueLen)),
               aOutLen);
}

const char* HostWebCallShim(void* aData, int64_t aTarget, const char* aMethod,
                            size_t aMethodLen, const char* aArgs, size_t aArgsLen,
                            size_t* aOutLen) {
  return Reply(
      Runner(aData)->HostWebCall(aTarget, std::string_view(aMethod, aMethodLen),
                                 std::string_view(aArgs, aArgsLen)),
      aOutLen);
}

const char* HostWebNewShim(void* aData, const char* aCtor, size_t aCtorLen,
                           const char* aArgs, size_t aArgsLen, size_t* aOutLen) {
  return Reply(Runner(aData)->HostWebNew(std::string_view(aCtor, aCtorLen),
                                         std::string_view(aArgs, aArgsLen)),
               aOutLen);
}

const char* HostWebIterateShim(void* aData, int64_t aTarget, size_t* aOutLen) {
  return Reply(Runner(aData)->HostWebIterate(aTarget), aOutLen);
}

int32_t HostWebInstanceofShim(void* aData, int64_t aTarget, int64_t aCtor) {
  return Runner(aData)->HostWebInstanceof(aTarget, aCtor) ? 1 : 0;
}

void HostWebReleaseShim(void* aData, int64_t aTarget) {
  Runner(aData)->HostWebRelease(aTarget);
}

// ---- interned fast-path shims (ABI v3) ----------------------------------
uint32_t HostWebInternShim(void* aData, const char* aName, size_t aLen) {
  return Runner(aData)->HostWebIntern(std::string_view(aName, aLen));
}

const char* HostWebGetIdShim(void* aData, int64_t aTarget, uint32_t aNameId,
                             size_t* aOutLen) {
  return Reply(Runner(aData)->HostWebGetId(aTarget, aNameId), aOutLen);
}

const char* HostWebSetStrShim(void* aData, int64_t aTarget, uint32_t aNameId,
                              const char* aValue, size_t aValueLen,
                              size_t* aOutLen) {
  return Reply(Runner(aData)->HostWebSetStr(aTarget, aNameId,
                                            std::string_view(aValue, aValueLen)),
               aOutLen);
}

const char* HostWebSetNumShim(void* aData, int64_t aTarget, uint32_t aNameId,
                              double aValue, size_t* aOutLen) {
  return Reply(Runner(aData)->HostWebSetNum(aTarget, aNameId, aValue), aOutLen);
}

const char* HostWebCallStrShim(void* aData, int64_t aTarget, uint32_t aNameId,
                               const char* aArg, size_t aArgLen,
                               size_t* aOutLen) {
  return Reply(Runner(aData)->HostWebCallStr(aTarget, aNameId,
                                             std::string_view(aArg, aArgLen)),
               aOutLen);
}

const char* HostWebCallScalarsShim(void* aData, int64_t aTarget,
                                   uint32_t aNameId, const msy_scalar* aArgs,
                                   size_t aArgc, size_t* aOutLen) {
  return Reply(Runner(aData)->HostWebCallScalars(aTarget, aNameId, aArgs, aArgc),
               aOutLen);
}

const char* HostWebNewScalarsShim(void* aData, uint32_t aCtorId,
                                  const msy_scalar* aArgs, size_t aArgc,
                                  size_t* aOutLen) {
  return Reply(Runner(aData)->HostWebNewScalars(aCtorId, aArgs, aArgc), aOutLen);
}

// ---- wide-string fast-path shims (ABI v5) -------------------------------
void HostWebGetU16Shim(void* aData, int64_t aTarget, uint32_t aNameId,
                       msy_reply* aOut) {
  Runner(aData)->HostWebGetU16(aTarget, aNameId, aOut);
}

void HostWebSetU16Shim(void* aData, int64_t aTarget, uint32_t aNameId,
                       const msy_arg16* aValue, msy_reply* aOut) {
  Runner(aData)->HostWebSetU16(aTarget, aNameId, aValue, aOut);
}

void HostWebCallU16Shim(void* aData, int64_t aTarget, uint32_t aNameId,
                        const msy_arg16* aArgs, size_t aArgc, msy_reply* aOut) {
  Runner(aData)->HostWebCallU16(aTarget, aNameId, aArgs, aArgc, aOut);
}

void HostWebNewU16Shim(void* aData, uint32_t aCtorId, const msy_arg16* aArgs,
                       size_t aArgc, msy_reply* aOut) {
  Runner(aData)->HostWebNewU16(aCtorId, aArgs, aArgc, aOut);
}

void HostWebBindShim(void* aData, int64_t aTarget, uint32_t aBindId,
                     const double* aArgs, size_t aArgc, msy_reply* aOut) {
  Runner(aData)->HostWebBind(aTarget, aBindId, aArgs, aArgc, aOut);
}

// `mersey(source)` — the browser-console REPL: one growing, always-typechecked
// module against this document's engine (msy_context_repl_turn). Echoes a
// trailing bare expression; a rejected turn's diagnostics throw.
bool MerseyReplNative(JSContext* aCx, unsigned aArgc, JS::Value* aVp) {
  JS::CallArgs args = JS::CallArgsFromVp(aArgc, aVp);
  args.rval().setUndefined();
  if (args.length() < 1 || !args[0].isString()) {
    return true;
  }
  nsGlobalWindowInner* win = xpc::CurrentWindowOrNull(aCx);
  if (!win || !win->GetExtantDoc()) {
    return true;
  }
  nsAutoJSString source;
  if (!source.init(aCx, args[0].toString())) {
    return true;
  }
  NS_ConvertUTF16toUTF8 utf8(source);
  nsAutoCString reply;
  MerseyScriptRunner::For(win->GetExtantDoc())->ReplTurn(utf8, reply);
  if (reply.Length() > 0 && reply.First() == '!') {
    NS_ConvertUTF8toUTF16 wide(Substring(reply, 1));
    JS::Rooted<JS::Value> msg(aCx);
    if (xpc::NonVoidStringToJsval(aCx, wide, &msg)) {
      JS_SetPendingException(aCx, msg);
    }
    return false;
  }
  if (reply.IsEmpty()) {
    return true;
  }
  NS_ConvertUTF8toUTF16 wide(reply);
  JS::Rooted<JS::Value> out(aCx);
  if (xpc::NonVoidStringToJsval(aCx, wide, &out)) {
    args.rval().set(out);
  }
  return true;
}

// The native function the bridge calls when JS invokes a Mersey closure. It
// recovers the runner from the current window — one context per document — so
// no per-callback state has to be threaded through JS.
bool MerseyInvokeNative(JSContext* aCx, unsigned aArgc, JS::Value* aVp) {
  JS::CallArgs args = JS::CallArgsFromVp(aArgc, aVp);
  args.rval().setUndefined();
  if (args.length() < 2 || !args[0].isNumber() || !args[1].isString()) {
    return true;
  }
  nsGlobalWindowInner* win = xpc::CurrentWindowOrNull(aCx);
  if (!win) {
    return true;
  }
  Document* doc = win->GetExtantDoc();
  if (!doc) {
    return true;
  }
  nsAutoJSString json;
  if (!json.init(aCx, args[1].toString())) {
    return true;
  }
  MerseyScriptRunner::For(doc)->InvokeArgs(
      static_cast<uint32_t>(args[0].toNumber()), NS_ConvertUTF16toUTF8(json));
  return true;
}

}  // namespace

// ---- MerseyEventListener ---------------------------------------------------

NS_IMPL_ISUPPORTS(MerseyEventListener, nsIDOMEventListener)

NS_IMETHODIMP
MerseyEventListener::HandleEvent(Event*) {
  mRunner->Invoke(mCallbackId);
  return NS_OK;
}

// ---- MerseyScriptRunner ----------------------------------------------------

// static
MerseyScriptRunner* MerseyScriptRunner::For(Document* aDocument) {
  MOZ_ASSERT(aDocument);
  if (!aDocument->GetMerseyScriptRunner()) {
    RefPtr<MerseyScriptRunner> runner = new MerseyScriptRunner(aDocument);
    aDocument->SetMerseyScriptRunner(runner);
  }
  return aDocument->GetMerseyScriptRunner();
}

MerseyScriptRunner::MerseyScriptRunner(Document* aDocument)
    : mDocument(aDocument) {
  MOZ_RELEASE_ASSERT(msy_abi_version() == MSY_ABI_VERSION,
                     "Mersey engine/header ABI mismatch");

  // Announce, once per process, that this is an experimental build. "Mersey
  // Gecko" is a Mersey-engine fork of Firefox -- not for production use, and
  // not affiliated with Mozilla or the Firefox project.
  static bool sBannerEmitted = false;
  if (!sBannerEmitted) {
    sBannerEmitted = true;
    fputs(
        "\n  Mersey Gecko (Experimental)\n"
        "  A Mersey-engine fork of Firefox. Experimental software -- NOT for\n"
        "  production use. Not affiliated with Mozilla or the Firefox project.\n\n",
        stderr);
    fflush(stderr);
  }

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
  // The universal bridge: every web technology, reflectively.
  table.web_global = HostWebGlobalShim;
  table.web_get = HostWebGetShim;
  table.web_set = HostWebSetShim;
  table.web_call = HostWebCallShim;
  table.web_new = HostWebNewShim;
  table.web_iterate = HostWebIterateShim;
  table.web_instanceof = HostWebInstanceofShim;
  table.web_release = HostWebReleaseShim;
  table.web_intern = HostWebInternShim;
  table.web_get_id = HostWebGetIdShim;
  table.web_set_str = HostWebSetStrShim;
  table.web_set_num = HostWebSetNumShim;
  table.web_call_str = HostWebCallStrShim;
  table.web_call_scalars = HostWebCallScalarsShim;
  table.web_new_scalars = HostWebNewScalarsShim;
  table.web_get_u16 = HostWebGetU16Shim;
  table.web_set_u16 = HostWebSetU16Shim;
  table.web_call_u16 = HostWebCallU16Shim;
  table.web_new_u16 = HostWebNewU16Shim;
  table.web_bind = HostWebBindShim;

  // Tier 1 maps W^X pages, which this process permits. MSY_FLAG_NO_JIT is the
  // lever if a platform ever refuses a second JIT.
  mContext = msy_context_new_ex(&table, 0);
}

MerseyScriptRunner::~MerseyScriptRunner() {
  if (mContext) {
    msy_context_free(mContext);
    mContext = nullptr;
  }
}

uint32_t MerseyScriptRunner::Run(const nsAString& aSource) {
  // Recorded for the DevTools debugger panel (engine lines, no offsets).
  mDebugScripts.AppendElement(NS_ConvertUTF16toUTF8(aSource));
  // The bridge install also defines the page-visible surface — merseyNative
  // (the polyfill stand-down marker) and mersey() (the console REPL) — so it
  // must exist even for scripts that never touch a web API.
  EnsureBridge();
  if (!mContext) {
    return 2;
  }
  NS_ConvertUTF16toUTF8 utf8(aSource);
  // Enter the page realm ONCE for the whole run. The engine calls back into the
  // web bridge many thousands of times during a single run; without this each
  // call would build its own AutoEntryScript, which dominates the cost. With
  // mActiveCx set, the per-call helpers reuse this entry.
  nsIGlobalObject* global = mDocument ? mDocument->GetScopeObject() : nullptr;
  if (global && global->GetGlobalJSObject()) {
    AutoEntryScript aes(global, "Mersey run");
    JSAutoRealm ar(aes.cx(), global->GetGlobalJSObject());
    JSContext* prev = mActiveCx;
    mActiveCx = aes.cx();
    uint32_t rc = msy_context_run(mContext, utf8.get(), utf8.Length());
    mActiveCx = prev;
    return rc;
  }
  return msy_context_run(mContext, utf8.get(), utf8.Length());
}

void MerseyScriptRunner::Invoke(uint32_t aCallbackId) {
  if (mContext) {
    msy_context_invoke(mContext, aCallbackId);
  }
}

void MerseyScriptRunner::InvokeArgs(uint32_t aCallbackId,
                                    const nsACString& aArgsJson) {
  if (!mContext) {
    return;
  }
  const nsCString& flat = PromiseFlatCString(aArgsJson);
  // A callback re-enters the engine, which may make its own bridge calls; enter
  // the realm once here too, the same way Run does.
  nsIGlobalObject* global = mDocument ? mDocument->GetScopeObject() : nullptr;
  if (global && global->GetGlobalJSObject()) {
    AutoEntryScript aes(global, "Mersey callback");
    JSAutoRealm ar(aes.cx(), global->GetGlobalJSObject());
    JSContext* prev = mActiveCx;
    mActiveCx = aes.cx();
    msy_context_invoke_args(mContext, aCallbackId, flat.get(), flat.Length());
    mActiveCx = prev;
    return;
  }
  msy_context_invoke_args(mContext, aCallbackId, flat.get(), flat.Length());
}

void MerseyScriptRunner::ReplCompletions(nsACString& aOut) {
  aOut.AssignLiteral("[]");
  if (!mContext) {
    return;
  }
  size_t out_len = 0;
  const char* reply = msy_context_repl_complete(mContext, &out_len);
  if (reply && out_len) {
    aOut.Assign(reply, out_len);
  }
}

void MerseyScriptRunner::DebugScriptsJson(nsACString& aOut) {
  aOut.AssignLiteral("[");
  for (size_t i = 0; i < mDebugScripts.Length(); ++i) {
    if (i) {
      aOut.AppendLiteral(",");
    }
    // The engine's JSON string escaping, done by hand for the few specials.
    aOut.AppendLiteral("{\"source\":\"");
    const nsCString& src = mDebugScripts[i];
    for (size_t k = 0; k < src.Length(); ++k) {
      char c = src.CharAt(k);
      switch (c) {
        case '\\':
          aOut.AppendLiteral("\\\\");
          break;
        case '"':
          aOut.AppendLiteral("\\\"");
          break;
        case '\n':
          aOut.AppendLiteral("\\n");
          break;
        case '\r':
          aOut.AppendLiteral("\\r");
          break;
        case '\t':
          aOut.AppendLiteral("\\t");
          break;
        default:
          if (static_cast<unsigned char>(c) < 0x20) {
            aOut.AppendPrintf("\\u%04x", c);
          } else {
            aOut.Append(c);
          }
      }
    }
    aOut.AppendLiteral("\"}");
  }
  aOut.AppendLiteral("]");
}

void MerseyScriptRunner::EnsureDebugEnabled() {
  if (mDebugEnabled || !mContext) {
    return;
  }
  msy_context_debug_enable(mContext, &MerseyScriptRunner::DebugPausedShim,
                           this);
  mDebugEnabled = true;
}

void MerseyScriptRunner::DebugSetBreakpoints(const nsTArray<uint32_t>& aLines) {
  if (!mContext) {
    return;
  }
  EnsureDebugEnabled();
  msy_context_debug_set_breakpoints(mContext, nullptr, 0, aLines.Elements(),
                                    aLines.Length());
}

void MerseyScriptRunner::DebugAction(const nsACString& aAction) {
  if (!mContext) {
    return;
  }
  if (aAction.EqualsLiteral("pause")) {
    EnsureDebugEnabled();
    msy_context_debug_pause(mContext);
  } else if (aAction.EqualsLiteral("resume")) {
    msy_context_debug_resume(mContext);
    mDebugResumeRequested = true;
  } else if (aAction.EqualsLiteral("stepOver")) {
    msy_context_debug_step_over(mContext);
    mDebugResumeRequested = true;
  } else if (aAction.EqualsLiteral("stepInto")) {
    msy_context_debug_step_in(mContext);
    mDebugResumeRequested = true;
  } else if (aAction.EqualsLiteral("stepOut")) {
    msy_context_debug_step_out(mContext);
    mDebugResumeRequested = true;
  } else if (aAction.EqualsLiteral("disable")) {
    msy_context_debug_disable(mContext);
    mDebugEnabled = false;
    mDebugResumeRequested = true;  // release a pause we may be sitting in
  }
}

void MerseyScriptRunner::DebugPausedShim(void* aData, const char* aJson,
                                         size_t aLen) {
  auto* self = static_cast<MerseyScriptRunner*>(aData);
  if (self) {
    self->OnDebugPaused(nsDependentCSubstring(aJson, aLen));
  }
}

void MerseyScriptRunner::OnDebugPaused(const nsACString& aSnapshotJson) {
  nsCOMPtr<nsIObserverService> obs = mozilla::services::GetObserverService();
  uint64_t windowId =
      mDocument && mDocument->GetInnerWindow()
          ? mDocument->GetInnerWindow()->WindowID()
          : 0;
  if (obs) {
    nsAutoCString payload;
    payload.AppendLiteral("{\"windowId\":");
    payload.AppendInt(windowId);
    payload.AppendLiteral(",\"pause\":");
    payload.Append(aSnapshotJson);
    payload.AppendLiteral("}");
    obs->NotifyObservers(nullptr, "mersey-debugger-paused",
                         NS_ConvertUTF8toUTF16(payload).get());
  }
  // BLOCK here, Gecko-style: spin the event loop until a resume-family
  // action arrives. Actor messages ride the same loop, which is exactly what
  // keeps DevTools working while the engine is stopped.
  mDebugResumeRequested = false;
  mozilla::SpinEventLoopUntil("mersey debugger pause"_ns, [&]() -> bool {
    return mDebugResumeRequested;
  });
  mDebugResumeRequested = false;
  if (obs) {
    nsAutoCString payload;
    payload.AppendLiteral("{\"windowId\":");
    payload.AppendInt(windowId);
    payload.AppendLiteral("}");
    obs->NotifyObservers(nullptr, "mersey-debugger-resumed",
                         NS_ConvertUTF8toUTF16(payload).get());
  }
}

void MerseyScriptRunner::ReplTurn(const nsACString& aSource, nsACString& aOut) {
  aOut.Truncate();
  if (!mContext) {
    return;
  }
  size_t out_len = 0;
  const char* reply = msy_context_repl_turn(
      mContext, aSource.BeginReading(), aSource.Length(), &out_len);
  if (reply && out_len) {
    aOut.Assign(reply, out_len);
  }
}

// ---- the universal web bridge ---------------------------------------------
//
// The reflective bridge is JavaScript — the *same* bridge the WASM polyfill
// runs (dom/mersey/MerseyBridgeSource.h, generated from mersey-bridge.js). It is
// injected into the page realm once; the host shims then call its methods with
// JSON strings and read JSON back. What makes this "native" rather than the
// polyfill is that the Mersey *compute* runs in the engine, not in wasm — only
// the actual web-API call crosses into JS. (The binding-free reflection here is
// the honest bootstrap; a WebIDL-codegen path that avoids JS entirely is the
// later optimization.)

bool MerseyScriptRunner::EnsureBridge() {
  if (mBridgeReady) {
    return true;
  }
  if (!mDocument) {
    return false;
  }
  nsIGlobalObject* global = mDocument->GetScopeObject();
  if (!global || !global->GetGlobalJSObject()) {
    return false;
  }

  AutoEntryScript aes(global, "Mersey bridge setup");
  JSContext* cx = aes.cx();
  JS::Rooted<JSObject*> globalObj(cx, global->GetGlobalJSObject());
  JSAutoRealm ar(cx, globalObj);

  // The callback hook the bridge invokes when JS calls a Mersey closure.
  if (!JS_DefineFunction(cx, globalObj, "__merseyInvoke", MerseyInvokeNative, 2,
                         0)) {
    return false;
  }
  // The console REPL entry.
  if (!JS_DefineFunction(cx, globalObj, "mersey", MerseyReplNative, 1, 0)) {
    return false;
  }

  JS::CompileOptions options(cx);
  options.setFileAndLine("mersey-bridge.js", 1);
  JS::SourceText<mozilla::Utf8Unit> source;
  if (!source.init(cx, kMerseyBridgeSource, strlen(kMerseyBridgeSource),
                   JS::SourceOwnership::Borrowed)) {
    return false;
  }
  JS::Rooted<JS::Value> rval(cx);
  if (!JS::Evaluate(cx, options, source, &rval)) {
    JS_ClearPendingException(cx);
    return false;
  }
  // Warn in the page console, once per document, that this is an experimental build.
  nsContentUtils::ReportToConsoleNonLocalized(
      u"[mersey] Mersey Gecko is an EXPERIMENTAL build -- not for production use, and not affiliated with Mozilla or the Firefox project."_ns,
      nsIScriptError::warningFlag, "Mersey"_ns, mDocument);
  mBridgeReady = true;
  return true;
}

// Call __merseyBridge[method](args...). String args become JS strings; the
// reply string lands in mBridgeScratch (valid until the next host call).
const std::string* MerseyScriptRunner::CallBridgeStr(
    const char* aMethod, std::initializer_list<std::string_view> aArgs) {
  if (!EnsureBridge()) {
    return nullptr;
  }
  nsIGlobalObject* global = mDocument->GetScopeObject();
  mozilla::Maybe<AutoEntryScript> aes;
  mozilla::Maybe<JSAutoRealm> ar;
  JSContext* cx = mActiveCx;
  if (!cx) {
    aes.emplace(global, "Mersey bridge call");
    cx = aes->cx();
    ar.emplace(cx, global->GetGlobalJSObject());
  }
  JS::Rooted<JSObject*> globalObj(cx, global->GetGlobalJSObject());

  JS::Rooted<JS::Value> bridgeVal(cx);
  if (!JS_GetProperty(cx, globalObj, "__merseyBridge", &bridgeVal) ||
      !bridgeVal.isObject()) {
    return nullptr;
  }
  JS::Rooted<JSObject*> bridge(cx, &bridgeVal.toObject());

  JS::RootedVector<JS::Value> argv(cx);
  for (std::string_view a : aArgs) {
    JS::Rooted<JSString*> s(
        cx, JS_NewStringCopyUTF8N(cx, JS::UTF8Chars(a.data(), a.size())));
    if (!s || !argv.append(JS::StringValue(s))) {
      return nullptr;
    }
  }
  JS::Rooted<JS::Value> rval(cx);
  if (!JS_CallFunctionName(cx, bridge, aMethod, JS::HandleValueArray(argv),
                           &rval)) {
    JS_ClearPendingException(cx);
    return nullptr;
  }
  if (!rval.isString()) {
    mBridgeScratch.clear();
    return &mBridgeScratch;  // e.g. release() returns undefined
  }
  nsAutoJSString out;
  if (!out.init(cx, rval.toString())) {
    return nullptr;
  }
  NS_ConvertUTF16toUTF8 utf8(out);
  mBridgeScratch.assign(utf8.get(), utf8.Length());
  return &mBridgeScratch;
}

int64_t MerseyScriptRunner::CallBridgeInt(
    const char* aMethod, std::initializer_list<std::string_view> aArgs) {
  if (!EnsureBridge()) {
    return -1;
  }
  nsIGlobalObject* global = mDocument->GetScopeObject();
  mozilla::Maybe<AutoEntryScript> aes;
  mozilla::Maybe<JSAutoRealm> ar;
  JSContext* cx = mActiveCx;
  if (!cx) {
    aes.emplace(global, "Mersey bridge call");
    cx = aes->cx();
    ar.emplace(cx, global->GetGlobalJSObject());
  }
  JS::Rooted<JSObject*> globalObj(cx, global->GetGlobalJSObject());

  JS::Rooted<JS::Value> bridgeVal(cx);
  if (!JS_GetProperty(cx, globalObj, "__merseyBridge", &bridgeVal) ||
      !bridgeVal.isObject()) {
    return -1;
  }
  JS::Rooted<JSObject*> bridge(cx, &bridgeVal.toObject());
  JS::RootedVector<JS::Value> argv(cx);
  for (std::string_view a : aArgs) {
    JS::Rooted<JSString*> s(
        cx, JS_NewStringCopyUTF8N(cx, JS::UTF8Chars(a.data(), a.size())));
    if (!s || !argv.append(JS::StringValue(s))) {
      return -1;
    }
  }
  JS::Rooted<JS::Value> rval(cx);
  if (!JS_CallFunctionName(cx, bridge, aMethod, JS::HandleValueArray(argv),
                           &rval) ||
      !rval.isNumber()) {
    JS_ClearPendingException(cx);
    return -1;
  }
  return static_cast<int64_t>(rval.toNumber());
}

int64_t MerseyScriptRunner::HostWebGlobal(std::string_view aName) {
  int64_t handle = CallBridgeInt("global", {aName});
  // Remember the storage singletons so their operations can take the direct
  // C++ path. The handle is still a real bridge handle, so anything we do not
  // fast-path (an unusual member) still falls through to JS.
  if (handle >= 0) {
    if (aName == "localStorage") {
      mLocalStorageHandle = handle;
    } else if (aName == "sessionStorage") {
      mSessionStorageHandle = handle;
    }
  }
  return handle;
}

const std::string* MerseyScriptRunner::HostWebGet(int64_t aTarget,
                                                  std::string_view aProp) {
  // A C++-owned handle (negative) must enter JS before the bridge can touch
  // it — the wide paths do this too; the JSON path was the gap the events
  // workload found (addEventListener carries a closure, so it lands here).
  if (aTarget < 0 && !EnsureReflector(aTarget)) {
    return nullptr;
  }
  char tbuf[24];
  int n = snprintf(tbuf, sizeof(tbuf), "%lld", (long long)aTarget);
  return CallBridgeStr("get", {std::string_view(tbuf, n), aProp});
}

const std::string* MerseyScriptRunner::HostWebSet(int64_t aTarget,
                                                  std::string_view aProp,
                                                  std::string_view aValueJson) {
  if (aTarget < 0 && !EnsureReflector(aTarget)) {
    return nullptr;
  }
  char tbuf[24];
  int n = snprintf(tbuf, sizeof(tbuf), "%lld", (long long)aTarget);
  return CallBridgeStr("set", {std::string_view(tbuf, n), aProp, aValueJson});
}

const std::string* MerseyScriptRunner::HostWebCall(int64_t aTarget,
                                                   std::string_view aMethod,
                                                   std::string_view aArgsJson) {
  if (aTarget < 0 && !EnsureReflector(aTarget)) {
    return nullptr;
  }
  char tbuf[24];
  int n = snprintf(tbuf, sizeof(tbuf), "%lld", (long long)aTarget);
  return CallBridgeStr("call", {std::string_view(tbuf, n), aMethod, aArgsJson});
}

const std::string* MerseyScriptRunner::HostWebNew(std::string_view aCtor,
                                                  std::string_view aArgsJson) {
  return CallBridgeStr("construct", {aCtor, aArgsJson});
}

const std::string* MerseyScriptRunner::HostWebIterate(int64_t aTarget) {
  char tbuf[24];
  int n = snprintf(tbuf, sizeof(tbuf), "%lld", (long long)aTarget);
  return CallBridgeStr("iterate", {std::string_view(tbuf, n)});
}

bool MerseyScriptRunner::HostWebInstanceof(int64_t aTarget, int64_t aCtor) {
  char a[24], c[24];
  int na = snprintf(a, sizeof(a), "%lld", (long long)aTarget);
  int nc = snprintf(c, sizeof(c), "%lld", (long long)aCtor);
  return CallBridgeInt("instanceOf",
                       {std::string_view(a, na), std::string_view(c, nc)}) != 0;
}

void MerseyScriptRunner::HostWebRelease(int64_t aTarget) {
  if (aTarget < 0) {
    NativeObj* o = NativeFor(aTarget);
    const bool materialized = o && o->materialized;
    if (o) {
      // Free the slot: drop the Gecko object and byte storage, mark it dead,
      // and return the slot (and its handle id) to the freelist for reuse.
      *o = NativeObj{};
      mNativeFree.push_back(SlotForHandle(aTarget));
    }
    if (!materialized) {
      return;  // never entered JS: nothing to release there
    }
  }
  mNativeCache.erase(aTarget);  // the handle dies; so must the cached native
  if (aTarget == mBindCacheHandle) {
    mBindCacheHandle = 0;  // and the one-entry bind cache in front of it
  }
  char tbuf[24];
  int n = snprintf(tbuf, sizeof(tbuf), "%lld", (long long)aTarget);
  CallBridgeStr("release", {std::string_view(tbuf, n)});
}

// Call __merseyBridge[method](args...), where each arg is a JS number or a JS
// string. This is the interned fast path: the handle and the interned id cross
// as numbers (no snprintf here, no parseInt in JS), and scalar values need no
// JSON. The reply string lands in mBridgeScratch, valid until the next call.
const std::string* MerseyScriptRunner::CallBridgeValues(
    const char* aMethod, mozilla::Span<const BridgeArg> aArgs) {
  if (!EnsureBridge()) {
    return nullptr;
  }
  nsIGlobalObject* global = mDocument->GetScopeObject();
  mozilla::Maybe<AutoEntryScript> aes;
  mozilla::Maybe<JSAutoRealm> ar;
  JSContext* cx = mActiveCx;
  if (!cx) {
    aes.emplace(global, "Mersey bridge call");
    cx = aes->cx();
    ar.emplace(cx, global->GetGlobalJSObject());
  }
  JS::Rooted<JSObject*> globalObj(cx, global->GetGlobalJSObject());

  JS::Rooted<JS::Value> bridgeVal(cx);
  if (!JS_GetProperty(cx, globalObj, "__merseyBridge", &bridgeVal) ||
      !bridgeVal.isObject()) {
    return nullptr;
  }
  JS::Rooted<JSObject*> bridge(cx, &bridgeVal.toObject());

  JS::RootedVector<JS::Value> argv(cx);
  for (const BridgeArg& a : aArgs) {
    if (a.isNum) {
      if (!argv.append(JS::NumberValue(a.num))) {
        return nullptr;
      }
    } else {
      JS::Rooted<JSString*> s(
          cx, JS_NewStringCopyUTF8N(cx, JS::UTF8Chars(a.strData, a.strLen)));
      if (!s || !argv.append(JS::StringValue(s))) {
        return nullptr;
      }
    }
  }
  JS::Rooted<JS::Value> rval(cx);
  if (!JS_CallFunctionName(cx, bridge, aMethod, JS::HandleValueArray(argv),
                           &rval)) {
    JS_ClearPendingException(cx);
    return nullptr;
  }
  if (!rval.isString()) {
    mBridgeScratch.clear();
    return &mBridgeScratch;
  }
  nsAutoJSString out;
  if (!out.init(cx, rval.toString())) {
    return nullptr;
  }
  NS_ConvertUTF16toUTF8 utf8(out);
  mBridgeScratch.assign(utf8.get(), utf8.Length());
  return &mBridgeScratch;
}

uint32_t MerseyScriptRunner::HostWebIntern(std::string_view aName) {
  // intern() returns a small id, or 0xffffffff to decline. It runs once per
  // unique name (the engine caches the id), so a string arg is fine here.
  int64_t id = CallBridgeInt("intern", {aName});
  if (id < 0) {
    return UINT32_MAX;
  }
  // Mirror the bridge's id→name table so a storage call arriving as an id can
  // be dispatched by name in C++. The bridge assigns ids sequentially, so this
  // vector stays in lock-step with its own `names` array.
  auto uid = static_cast<uint32_t>(id);
  if (uid >= mInternedNames.size()) {
    mInternedNames.resize(uid + 1);
  }
  mInternedNames[uid].assign(aName);
  return uid;
}

const std::string& MerseyScriptRunner::InternedName(uint32_t aNameId) const {
  static const std::string kEmpty;
  return aNameId < mInternedNames.size() ? mInternedNames[aNameId] : kEmpty;
}

// The Storage object a handle refers to, or nullptr if it is not one of the two
// storage singletons. Fetched once and cached (the window owns the Storage).
Storage* MerseyScriptRunner::StorageFor(int64_t aTarget) {
  const bool isLocal = aTarget == mLocalStorageHandle;
  const bool isSession = aTarget == mSessionStorageHandle;
  if (!isLocal && !isSession) {
    return nullptr;
  }
  RefPtr<Storage>& slot = isLocal ? mLocalStorage : mSessionStorage;
  if (!slot && mDocument) {
    nsGlobalWindowInner* win =
        nsGlobalWindowInner::Cast(mDocument->GetInnerWindow());
    if (win) {
      IgnoredErrorResult rv;
      slot = isLocal ? win->GetLocalStorage(rv) : win->GetSessionStorage(rv);
    }
  }
  return slot.get();
}

const std::string* MerseyScriptRunner::HostWebGetId(int64_t aTarget,
                                                    uint32_t aNameId) {
  // Direct C++: localStorage.length et al. — no realm entry.
  if (Storage* st = StorageFor(aTarget)) {
    const std::string& name = InternedName(aNameId);
    if (name == "length" && mDocument) {
      IgnoredErrorResult rv;
      uint32_t len = st->GetLength(*mDocument->NodePrincipal(), rv);
      if (!rv.Failed()) {
        mBridgeScratch = "{\"ok\":" + std::to_string(len) + "}";
        return &mBridgeScratch;
      }
    }
    // Anything else on a storage object still goes through the bridge.
  }
  return CallBridgeValues("getId", {BridgeArg(aTarget), BridgeArg(aNameId)});
}

const std::string* MerseyScriptRunner::HostWebSetStr(int64_t aTarget,
                                                     uint32_t aNameId,
                                                     std::string_view aValue) {
  return CallBridgeValues(
      "setScalar", {BridgeArg(aTarget), BridgeArg(aNameId), BridgeArg(aValue)});
}

const std::string* MerseyScriptRunner::HostWebSetNum(int64_t aTarget,
                                                     uint32_t aNameId,
                                                     double aValue) {
  return CallBridgeValues(
      "setScalar", {BridgeArg(aTarget), BridgeArg(aNameId), BridgeArg(aValue)});
}

const std::string* MerseyScriptRunner::HostWebCallStr(int64_t aTarget,
                                                      uint32_t aNameId,
                                                      std::string_view aArg) {
  return CallBridgeValues(
      "callStr", {BridgeArg(aTarget), BridgeArg(aNameId), BridgeArg(aArg)});
}

// The bridge's callScalars(target, nameId, ...args) / newScalars(ctorId,
// ...args) take the scalars as ordinary JS arguments. We build a values array:
// target/id first, then each scalar as a JS number or string.
const std::string* MerseyScriptRunner::HostWebCallScalars(
    int64_t aTarget, uint32_t aNameId, const msy_scalar* aArgs, size_t aArgc) {
  // Direct C++ Web Storage: getItem/setItem/removeItem straight against the
  // Storage object, no realm entry and no reply JSON round-trip. String args
  // only (the storage API is all strings); anything else falls through.
  if (Storage* st = StorageFor(aTarget)) {
    const std::string& name = InternedName(aNameId);
    const bool allStrings = [&] {
      for (size_t i = 0; i < aArgc; i++) {
        if (aArgs[i].is_num) return false;
      }
      return true;
    }();
    if (allStrings && mDocument) {
      nsIPrincipal& principal = *mDocument->NodePrincipal();
      auto arg = [&](size_t i) {
        return NS_ConvertUTF8toUTF16(aArgs[i].str,
                                     static_cast<uint32_t>(aArgs[i].str_len));
      };
      if (name == "getItem" && aArgc == 1) {
        nsAutoString result;
        IgnoredErrorResult rv;
        st->GetItem(arg(0), result, principal, rv);
        if (!rv.Failed()) {
          mBridgeScratch = "{\"ok\":";
          if (result.IsVoid()) {
            mBridgeScratch += "null";  // missing key → null, per the spec
          } else {
            AppendJsonString(mBridgeScratch, result);
          }
          mBridgeScratch += "}";
          return &mBridgeScratch;
        }
      } else if (name == "setItem" && aArgc == 2) {
        IgnoredErrorResult rv;
        st->SetItem(arg(0), arg(1), principal, rv);
        if (!rv.Failed()) {
          mBridgeScratch = "{\"ok\":null}";
          return &mBridgeScratch;
        }
      } else if (name == "removeItem" && aArgc == 1) {
        IgnoredErrorResult rv;
        st->RemoveItem(arg(0), principal, rv);
        if (!rv.Failed()) {
          mBridgeScratch = "{\"ok\":null}";
          return &mBridgeScratch;
        }
      }
    }
    // Not a fast-pathed storage op (or it failed): fall through to the bridge.
  }
  AutoTArray<BridgeArg, 6> vals;
  vals.AppendElement(BridgeArg(aTarget));
  vals.AppendElement(BridgeArg(aNameId));
  for (size_t i = 0; i < aArgc; i++) {
    vals.AppendElement(aArgs[i].is_num
                           ? BridgeArg(aArgs[i].num)
                           : BridgeArg(std::string_view(aArgs[i].str,
                                                        aArgs[i].str_len)));
  }
  return CallBridgeValues("callScalars", vals);
}

const std::string* MerseyScriptRunner::HostWebNewScalars(
    uint32_t aCtorId, const msy_scalar* aArgs, size_t aArgc) {
  AutoTArray<BridgeArg, 6> vals;
  vals.AppendElement(BridgeArg(aCtorId));
  for (size_t i = 0; i < aArgc; i++) {
    vals.AppendElement(aArgs[i].is_num
                           ? BridgeArg(aArgs[i].num)
                           : BridgeArg(std::string_view(aArgs[i].str,
                                                        aArgs[i].str_len)));
  }
  return CallBridgeValues("newScalars", vals);
}

// ---- wide-string fast paths (UTF-16 args, typed UTF-16 replies) ---------

void MerseyScriptRunner::CallBridgeWide(const char* aMethod, const double* aLead,
                                        size_t aLeadCount, const msy_arg16* aArgs,
                                        size_t aArgc, msy_reply* aOut) {
  aOut->tag = 0;
  aOut->str16 = nullptr;
  aOut->str16_len = 0;
  if (!EnsureBridge()) {
    return;
  }
  nsIGlobalObject* global = mDocument->GetScopeObject();
  mozilla::Maybe<AutoEntryScript> aes;
  mozilla::Maybe<JSAutoRealm> ar;
  JSContext* cx = mActiveCx;
  if (!cx) {
    aes.emplace(global, "Mersey bridge call");
    cx = aes->cx();
    ar.emplace(cx, global->GetGlobalJSObject());
  }
  JS::Rooted<JSObject*> globalObj(cx, global->GetGlobalJSObject());
  JS::Rooted<JS::Value> bridgeVal(cx);
  if (!JS_GetProperty(cx, globalObj, "__merseyBridge", &bridgeVal) ||
      !bridgeVal.isObject()) {
    return;
  }
  JS::Rooted<JSObject*> bridge(cx, &bridgeVal.toObject());

  JS::RootedVector<JS::Value> argv(cx);
  for (size_t i = 0; i < aLeadCount; i++) {
    if (!argv.append(JS::NumberValue(aLead[i]))) {
      return;
    }
  }
  // A ref argument crosses as its handle number; the mask (one extra lead)
  // tells the bridge which positions to resolve back to objects.
  double refsMask = 0;
  for (size_t i = 0; i < aArgc && i < 32; i++) {
    if (aArgs[i].kind == 2) {
      refsMask += double(uint32_t(1) << i);
    }
  }
  if (!argv.append(JS::NumberValue(refsMask))) {
    return;
  }
  // ABI v8: a second mask for stable callback ids (kind 5) — the bridge
  // resolves those positions to its cached wrapper functions.
  double cbMask = 0;
  for (size_t i = 0; i < aArgc && i < 32; i++) {
    if (aArgs[i].kind == 5) {
      cbMask += double(uint32_t(1) << i);
    }
  }
  if (!argv.append(JS::NumberValue(cbMask))) {
    return;
  }
  for (size_t i = 0; i < aArgc; i++) {
    switch (aArgs[i].kind) {
      case 1:  // number
      case 2:  // ref handle (bridge resolves via the mask)
      case 5:  // callback id (bridge resolves via the cb mask)
        if (!argv.append(JS::NumberValue(aArgs[i].num))) {
          return;
        }
        break;
      case 3:  // bool
        if (!argv.append(JS::BooleanValue(aArgs[i].num != 0))) {
          return;
        }
        break;
      case 4:  // null
        if (!argv.append(JS::NullValue())) {
          return;
        }
        break;
      default: {
        // kind 0: the engine's argument is already UTF-16 — straight to JS.
        JS::Rooted<JSString*> s(
            cx, JS_NewUCStringCopyN(
                    cx, reinterpret_cast<const char16_t*>(aArgs[i].str16),
                    aArgs[i].str16_len));
        if (!s || !argv.append(JS::StringValue(s))) {
          return;
        }
      }
    }
  }
  JS::Rooted<JS::Value> rval(cx);
  if (!JS_CallFunctionName(cx, bridge, aMethod, JS::HandleValueArray(argv),
                           &rval)) {
    FillReplyError(cx, mReplyScratch16, aOut);
    return;
  }
  FillReplyFromJs(cx, rval, mReplyScratch16, aOut);
}

// One realm entry per handle: fetch the JS object a handle names and unwrap it
// to its native pointer. After this, fillRect / appendChild / textContent on
// that handle are pure C++ — no JS at all. The bridge's handles[] array roots
// the wrapper (and the wrapper its native) until web_release, which erases the
// cache entry, so the raw pointer cannot outlive the object it names.
int64_t MerseyScriptRunner::AddNativeObj(NativeObj&& aObj) {
  aObj.live = true;
  uint32_t slot;
  if (!mNativeFree.empty()) {
    slot = mNativeFree.back();
    mNativeFree.pop_back();
    mNativeSlots[slot] = std::move(aObj);
  } else {
    slot = uint32_t(mNativeSlots.size());
    mNativeSlots.push_back(std::move(aObj));
  }
  return HandleForSlot(slot);
}

MerseyScriptRunner::NativeObj* MerseyScriptRunner::NativeFor(int64_t aHandle) {
  if (aHandle >= 0) {
    return nullptr;
  }
  const uint32_t slot = SlotForHandle(aHandle);
  if (slot >= mNativeSlots.size() || !mNativeSlots[slot].live) {
    return nullptr;
  }
  return &mNativeSlots[slot];
}

void* MerseyScriptRunner::UnwrapNative(int64_t aHandle, int32_t& aKindOut) {
  // A C++-created object never needs unwrapping: it was never wrapped.
  if (aHandle < 0) {
    if (NativeObj* o = NativeFor(aHandle)) {
      aKindOut = o->kind;
      return o->ptr;
    }
    aKindOut = kNativeOther;
    return nullptr;
  }
  if (auto it = mNativeCache.find(aHandle); it != mNativeCache.end()) {
    aKindOut = it->second.first;
    return it->second.second;
  }
  aKindOut = kNativeOther;
  if (!EnsureBridge()) {
    return nullptr;
  }
  nsIGlobalObject* global = mDocument->GetScopeObject();
  mozilla::Maybe<AutoEntryScript> aes;
  mozilla::Maybe<JSAutoRealm> ar;
  JSContext* cx = mActiveCx;
  if (!cx) {
    aes.emplace(global, "Mersey unwrap");
    cx = aes->cx();
    ar.emplace(cx, global->GetGlobalJSObject());
  }
  JS::Rooted<JSObject*> globalObj(cx, global->GetGlobalJSObject());
  JS::Rooted<JS::Value> bridgeVal(cx);
  if (!JS_GetProperty(cx, globalObj, "__merseyBridge", &bridgeVal) ||
      !bridgeVal.isObject()) {
    return nullptr;
  }
  JS::Rooted<JSObject*> bridge(cx, &bridgeVal.toObject());
  JS::RootedVector<JS::Value> argv(cx);
  if (!argv.append(JS::NumberValue(double(aHandle)))) {
    return nullptr;
  }
  JS::Rooted<JS::Value> rval(cx);
  if (!JS_CallFunctionName(cx, bridge, "handleObj", JS::HandleValueArray(argv),
                           &rval) ||
      !rval.isObject()) {
    JS_ClearPendingException(cx);
    mNativeCache[aHandle] = {kNativeOther, nullptr};
    return nullptr;
  }
  JS::Rooted<JSObject*> obj(cx, &rval.toObject());
  int32_t kind = kNativeOther;
  void* ptr = nullptr;
  {
    Element* el = nullptr;
    if (NS_SUCCEEDED(UNWRAP_OBJECT(Element, &obj, el))) {
      kind = kNativeElement;
      ptr = el;
    }
  }
  if (!ptr) {
    CanvasRenderingContext2D* ctx = nullptr;
    if (NS_SUCCEEDED(UNWRAP_OBJECT(CanvasRenderingContext2D, &obj, ctx))) {
      kind = kNativeCanvas2D;
      ptr = ctx;
    }
  }
  if (!ptr) {
    Document* doc = nullptr;
    if (NS_SUCCEEDED(UNWRAP_OBJECT(Document, &obj, doc))) {
      kind = kNativeDocument;
      ptr = doc;
    }
  }
  mNativeCache[aHandle] = {kind, ptr};
  aKindOut = kind;
  return ptr;
}

// A C++-created object escaping to a JS-path API: build its reflector and
// hand it to the bridge under the SAME handle id (bridge `adopt`), preserving
// identity everywhere. Runs at most once per object.
bool MerseyScriptRunner::EnsureReflector(int64_t aHandle) {
  if (aHandle >= 0) {
    return true;
  }
  NativeObj* op = NativeFor(aHandle);
  if (!op) {
    return false;
  }
  NativeObj& o = *op;
  if (o.materialized) {
    return true;
  }
  if (!EnsureBridge()) {
    return false;
  }
  nsIGlobalObject* global = mDocument->GetScopeObject();
  mozilla::Maybe<AutoEntryScript> aes;
  mozilla::Maybe<JSAutoRealm> ar;
  JSContext* cx = mActiveCx;
  if (!cx) {
    aes.emplace(global, "Mersey materialize");
    cx = aes->cx();
    ar.emplace(cx, global->GetGlobalJSObject());
  }
  JS::Rooted<JS::Value> val(cx);
  switch (o.kind) {
    case kNativeElement:
      if (!ToJSValue(cx, *static_cast<Element*>(o.ptr), &val)) {
        return false;
      }
      break;
    case kNativeURL:
      if (!ToJSValue(cx, *static_cast<URL*>(o.ptr), &val)) {
        return false;
      }
      break;
    case kNativeTokenList:
      if (!ToJSValue(cx, *static_cast<nsDOMTokenList*>(o.ptr), &val)) {
        return false;
      }
      break;
    case kNativeStyle:
      if (!ToJSValue(cx, *static_cast<nsICSSDeclaration*>(o.ptr), &val)) {
        return false;
      }
      break;
    case kNativeNodeList:
      if (!ToJSValue(cx, *static_cast<NodeList*>(o.ptr), &val)) {
        return false;
      }
      break;
    case kNativeEvent:
      if (!ToJSValue(cx, *static_cast<Event*>(o.ptr), &val)) {
        return false;
      }
      break;
    case kNativeBytes: {
      JS::Rooted<JSObject*> arr(cx, JS_NewUint8Array(cx, o.bytes.Length()));
      if (!arr) {
        return false;
      }
      {
        bool shared;
        JS::AutoCheckCannotGC nogc;
        uint8_t* data = JS_GetUint8ArrayData(arr, &shared, nogc);
        memcpy(data, o.bytes.Elements(), o.bytes.Length());
      }
      val.setObject(*arr);
      break;
    }
    default:
      return false;
  }
  JS::Rooted<JSObject*> globalObj(cx, global->GetGlobalJSObject());
  JS::Rooted<JS::Value> bridgeVal(cx);
  if (!JS_GetProperty(cx, globalObj, "__merseyBridge", &bridgeVal) ||
      !bridgeVal.isObject()) {
    return false;
  }
  JS::Rooted<JSObject*> bridge(cx, &bridgeVal.toObject());
  JS::RootedVector<JS::Value> argv(cx);
  if (!argv.append(JS::NumberValue(double(aHandle))) || !argv.append(val)) {
    return false;
  }
  JS::Rooted<JS::Value> rval(cx);
  if (!JS_CallFunctionName(cx, bridge, "adopt", JS::HandleValueArray(argv),
                           &rval)) {
    JS_ClearPendingException(cx);
    return false;
  }
  o.materialized = true;
  return true;
}

void MerseyScriptRunner::HostWebGetU16(int64_t aTarget, uint32_t aNameId,
                                       msy_reply* aOut) {
  // Direct C++: localStorage.length — no realm entry.
  if (Storage* st = StorageFor(aTarget)) {
    if (InternedName(aNameId) == "length" && mDocument) {
      IgnoredErrorResult rv;
      uint32_t len = st->GetLength(*mDocument->NodePrincipal(), rv);
      if (!rv.Failed()) {
        aOut->tag = 1;
        aOut->num = double(len);
        aOut->str16 = nullptr;
        aOut->str16_len = 0;
        return;
      }
    }
  }
  // C++-created objects: property reads with no JS at all.
  if (aTarget < 0) {
    if (NativeObj* op = NativeFor(aTarget)) {
      const std::string& name = InternedName(aNameId);
      NativeObj& o = *op;
      if (o.kind == kNativeURL) {
        URL* url = static_cast<URL*>(o.ptr);
        nsAutoCString v8;
        bool have = true;
        if (name == "pathname") url->GetPathname(v8);
        else if (name == "search") url->GetSearch(v8);
        else if (name == "host") url->GetHost(v8);
        else if (name == "hostname") url->GetHostname(v8);
        else if (name == "port") url->GetPort(v8);
        else if (name == "protocol") url->GetProtocol(v8);
        else if (name == "href") url->GetHref(v8);
        else if (name == "hash") url->GetHash(v8);
        else have = false;
        if (have) {
          CopyUTF8toUTF16(v8, mReplyScratch16);
          aOut->tag = 2;
          aOut->str16 =
              reinterpret_cast<const uint16_t*>(mReplyScratch16.BeginReading());
          aOut->str16_len = mReplyScratch16.Length();
          return;
        }
      } else if (o.kind == kNativeBytes && name == "length") {
        aOut->tag = 1;
        aOut->num = double(o.bytes.Length());
        return;
      } else if (o.kind == kNativeElement) {
        Element* el = static_cast<Element*>(o.ptr);
        if (name == "classList") {
          if (nsDOMTokenList* list = el->ClassList()) {
            NativeObj obj;
            obj.kind = kNativeTokenList;
            obj.ptr = list;
            obj.holder = list;
            aOut->tag = 3;
            aOut->num = double(AddNativeObj(std::move(obj)));
            return;
          }
        } else if (name == "style") {
          if (nsStyledElement* styled = nsStyledElement::FromNode(el)) {
            if (nsICSSDeclaration* decl = styled->Style()) {
              NativeObj obj;
              obj.kind = kNativeStyle;
              obj.ptr = decl;
              obj.holder = decl;
              aOut->tag = 3;
              aOut->num = double(AddNativeObj(std::move(obj)));
              return;
            }
          }
        } else if (name == "textContent") {
          nsAutoString text;
          nsContentUtils::GetNodeTextContent(el, true, text);
          ReplyStr16(mReplyScratch16, text, 2, aOut);
          return;
        }
      } else if (o.kind == kNativeNodeList) {
        NodeList* list = static_cast<NodeList*>(o.ptr);
        if (name == "length") {
          aOut->tag = 1;
          aOut->num = double(list->Length());
          return;
        }
        // A digit-only name is an indexed access (`nodes[i]`).
        if (!name.empty() && name.size() <= 9 &&
            name.find_first_not_of("0123456789") == std::string::npos) {
          uint32_t index = uint32_t(strtoul(name.c_str(), nullptr, 10));
          nsIContent* item = list->Item(index);
          if (item && item->IsElement()) {
            NativeObj obj;
            obj.kind = kNativeElement;
            obj.ptr = item->AsElement();
            obj.holder = item;
            aOut->tag = 3;
            aOut->num = double(AddNativeObj(std::move(obj)));
            return;
          }
          aOut->tag = 0;  // out of range -> null, like the JS path
          return;
        }
      }
    }
    // Some other member: give the object a JS identity and use the bridge.
    if (!EnsureReflector(aTarget)) {
      return;  // tag 0 (null), like a stale handle
    }
  }
  const double lead[2] = {double(aTarget), double(aNameId)};
  CallBridgeWide("getWide", lead, 2, nullptr, 0, aOut);
}

void MerseyScriptRunner::HostWebSetU16(int64_t aTarget, uint32_t aNameId,
                                       const msy_arg16* aValue, msy_reply* aOut) {
  // Direct C++: el.textContent = "…" without entering JS.
  if (aValue->kind == 0 && InternedName(aNameId) == "textContent") {
    int32_t kind;
    void* ptr = UnwrapNative(aTarget, kind);
    if (kind == kNativeElement && ptr) {
      nsDependentString text(reinterpret_cast<const char16_t*>(aValue->str16),
                             aValue->str16_len);
      IgnoredErrorResult rv;
      static_cast<Element*>(ptr)->SetTextContent(text, rv);
      if (!rv.Failed()) {
        aOut->tag = 0;
        aOut->str16 = nullptr;
        aOut->str16_len = 0;
        return;
      }
    }
  }
  // Direct C++: el.className = "…" (the reflected class attribute).
  if (aValue->kind == 0 && InternedName(aNameId) == "className") {
    int32_t kind;
    void* ptr = UnwrapNative(aTarget, kind);
    if (kind == kNativeElement && ptr) {
      nsDependentString text(reinterpret_cast<const char16_t*>(aValue->str16),
                             aValue->str16_len);
      static_cast<Element*>(ptr)->SetClassName(text);
      aOut->tag = 0;
      aOut->str16 = nullptr;
      aOut->str16_len = 0;
      return;
    }
  }
  if (aTarget < 0 && !EnsureReflector(aTarget)) {
    return;
  }
  if (aValue->kind == 2 && aValue->num < 0 &&
      !EnsureReflector(int64_t(aValue->num))) {
    return;
  }
  const double lead[2] = {double(aTarget), double(aNameId)};
  CallBridgeWide("setWide", lead, 2, aValue, 1, aOut);
}

void MerseyScriptRunner::HostWebCallU16(int64_t aTarget, uint32_t aNameId,
                                        const msy_arg16* aArgs, size_t aArgc,
                                        msy_reply* aOut) {
  // Direct C++ Web Storage: getItem/setItem/removeItem straight against the
  // Storage object, UTF-16 in and out, no JS realm and no JSON.
  if (Storage* st = StorageFor(aTarget)) {
    const std::string& name = InternedName(aNameId);
    bool allStr = true;
    for (size_t i = 0; i < aArgc; i++) {
      if (aArgs[i].kind != 0) {
        allStr = false;
        break;
      }
    }
    if (allStr && mDocument) {
      nsIPrincipal& principal = *mDocument->NodePrincipal();
      // Args are already UTF-16 — a dependent view, no copy, valid for the call.
      nsDependentString a0, a1;
      if (aArgc >= 1)
        a0.Rebind(reinterpret_cast<const char16_t*>(aArgs[0].str16),
                  aArgs[0].str16_len);
      if (aArgc >= 2)
        a1.Rebind(reinterpret_cast<const char16_t*>(aArgs[1].str16),
                  aArgs[1].str16_len);
      if (name == "getItem" && aArgc == 1) {
        IgnoredErrorResult rv;
        st->GetItem(a0, mReplyScratch16, principal, rv);
        if (!rv.Failed()) {
          if (mReplyScratch16.IsVoid()) {
            aOut->tag = 0;  // missing key -> null
            aOut->str16 = nullptr;
            aOut->str16_len = 0;
          } else {
            aOut->tag = 2;
            aOut->str16 =
                reinterpret_cast<const uint16_t*>(mReplyScratch16.BeginReading());
            aOut->str16_len = mReplyScratch16.Length();
          }
          return;
        }
      } else if (name == "setItem" && aArgc == 2) {
        IgnoredErrorResult rv;
        st->SetItem(a0, a1, principal, rv);
        if (!rv.Failed()) {
          aOut->tag = 0;
          return;
        }
      } else if (name == "removeItem" && aArgc == 1) {
        IgnoredErrorResult rv;
        st->RemoveItem(a0, principal, rv);
        if (!rv.Failed()) {
          aOut->tag = 0;
          return;
        }
      }
    }
  }
  // Direct C++, no JS: hot DOM/canvas operations on unwrapped natives. Gated
  // on the operation name so unrelated handles never pay an unwrap.
  {
    const std::string& name = InternedName(aNameId);
    if (name == "fillRect" && aArgc == 4 && aArgs[0].kind == 1 &&
        aArgs[1].kind == 1 && aArgs[2].kind == 1 && aArgs[3].kind == 1) {
      int32_t kind;
      void* ptr = UnwrapNative(aTarget, kind);
      if (kind == kNativeCanvas2D && ptr) {
        static_cast<CanvasRenderingContext2D*>(ptr)->FillRect(
            aArgs[0].num, aArgs[1].num, aArgs[2].num, aArgs[3].num);
        aOut->tag = 0;
        return;
      }
    } else if (name == "createElement" && aArgc == 1 && aArgs[0].kind == 0) {
      int32_t kind;
      void* ptr = UnwrapNative(aTarget, kind);
      if (kind == kNativeDocument && ptr) {
        nsDependentString tag(
            reinterpret_cast<const char16_t*>(aArgs[0].str16),
            aArgs[0].str16_len);
        RefPtr<Element> el = static_cast<Document*>(ptr)->CreateElem(
            tag, nullptr, kNameSpaceID_XHTML);
        if (el) {
          NativeObj o;
          o.kind = kNativeElement;
          o.ptr = el.get();
          o.holder = ToSupports(el.get());
          const int64_t h = AddNativeObj(std::move(o));
          aOut->tag = 3;
          aOut->num = double(h);
          return;
        }
      }
    } else if (name == "getRandomValues" && aArgc == 1 && aArgs[0].kind == 2 &&
               aArgs[0].num < 0) {
      // crypto.getRandomValues(buf) where buf is a C++-owned byte buffer:
      // fill it right here — no JS, no typed-array plumbing. Once the buffer
      // has escaped to JS (materialized), the JS Uint8Array is a separate copy
      // and must be the one filled, so yield to the bridge in that case.
      NativeObj* o = NativeFor(int64_t(aArgs[0].num));
      if (o && o->kind == kNativeBytes && !o->materialized) {
        if (HostRandomBytes(o->bytes.Elements(), o->bytes.Length())) {
          aOut->tag = 3;  // returns the same array
          aOut->num = aArgs[0].num;
          return;
        }
      }
    } else if (name == "appendChild" && aArgc == 1 && aArgs[0].kind == 2) {
      int32_t pk, ck;
      void* parent = UnwrapNative(aTarget, pk);
      void* child = UnwrapNative(int64_t(aArgs[0].num), ck);
      if (pk == kNativeElement && parent && ck == kNativeElement && child) {
        IgnoredErrorResult rv;
        static_cast<Element*>(parent)->AppendChild(
            *static_cast<Element*>(child), rv);
        if (!rv.Failed()) {
          aOut->tag = 3;  // appendChild returns the child
          aOut->num = aArgs[0].num;
          return;
        }
      }
    } else if (name == "contains" && aArgc == 1 && aArgs[0].kind == 0) {
      // tokens.contains(s) on el.classList.
      if (NativeObj* o = NativeFor(aTarget);
          o && o->kind == kNativeTokenList) {
        nsDependentString token(
            reinterpret_cast<const char16_t*>(aArgs[0].str16),
            aArgs[0].str16_len);
        aOut->tag = 4;  // bool
        aOut->num =
            static_cast<nsDOMTokenList*>(o->ptr)->Contains(token) ? 1 : 0;
        return;
      }
    } else if ((name == "setProperty" || name == "getPropertyValue") &&
               aArgc >= 1 && aArgs[0].kind == 0) {
      // style.setProperty(p, v) / style.getPropertyValue(p).
      if (NativeObj* o = NativeFor(aTarget); o && o->kind == kNativeStyle) {
        auto* decl = static_cast<nsICSSDeclaration*>(o->ptr);
        NS_ConvertUTF16toUTF8 prop(
            reinterpret_cast<const char16_t*>(aArgs[0].str16),
            aArgs[0].str16_len);
        if (name == "setProperty" && aArgc >= 2 && aArgs[1].kind == 0) {
          NS_ConvertUTF16toUTF8 value(
              reinterpret_cast<const char16_t*>(aArgs[1].str16),
              aArgs[1].str16_len);
          IgnoredErrorResult rv;
          decl->SetProperty(prop, value, ""_ns, rv);
          if (!rv.Failed()) {
            aOut->tag = 0;
            return;
          }
        } else if (name == "getPropertyValue") {
          nsAutoCString v8;
          decl->GetPropertyValue(prop, v8);
          CopyUTF8toUTF16(v8, mReplyScratch16);
          aOut->tag = 2;
          aOut->str16 =
              reinterpret_cast<const uint16_t*>(mReplyScratch16.BeginReading());
          aOut->str16_len = mReplyScratch16.Length();
          return;
        }
      }
    } else if (name == "querySelectorAll" && aArgc == 1 &&
               aArgs[0].kind == 0) {
      // A real selector match; the NodeList stays on the C++ side.
      int32_t kind;
      void* ptr = UnwrapNative(aTarget, kind);
      nsINode* node = nullptr;
      if (kind == kNativeDocument) node = static_cast<Document*>(ptr);
      else if (kind == kNativeElement) node = static_cast<Element*>(ptr);
      if (node) {
        NS_ConvertUTF16toUTF8 sel(
            reinterpret_cast<const char16_t*>(aArgs[0].str16),
            aArgs[0].str16_len);
        IgnoredErrorResult rv;
        RefPtr<NodeList> list = node->QuerySelectorAll(sel, rv);
        if (!rv.Failed() && list) {
          NativeObj o;
          o.kind = kNativeNodeList;
          o.ptr = list.get();
          o.holder = list;
          aOut->tag = 3;
          aOut->num = double(AddNativeObj(std::move(o)));
          return;
        }
      }
    } else if (name == "dispatchEvent" && aArgc == 1 && aArgs[0].kind == 2 &&
               aArgs[0].num < 0) {
      // el.dispatchEvent(ev): direct dispatch — listeners (registered through
      // the JS bridge on the materialized element) still fire synchronously.
      NativeObj* ev = NativeFor(int64_t(aArgs[0].num));
      int32_t kind;
      void* ptr = UnwrapNative(aTarget, kind);
      if (ev && ev->kind == kNativeEvent && kind == kNativeElement && ptr) {
        IgnoredErrorResult rv;
        bool ok = static_cast<Element*>(ptr)->DispatchEvent(
            *static_cast<Event*>(ev->ptr), CallerType::System, rv);
        if (!rv.Failed()) {
          aOut->tag = 4;  // bool
          aOut->num = ok ? 1 : 0;
          return;
        }
      }
    } else if (name == "encode" && aArgc == 1 && aArgs[0].kind == 0) {
      // enc.encode(s): UTF-8 encode into a C++ byte buffer — the same codec
      // TextEncoder wraps (the URL/KURL precedent, applied to text).
      if (NativeObj* o = NativeFor(aTarget);
          o && o->kind == kNativeTextEncoder) {
        NS_ConvertUTF16toUTF8 utf8(
            reinterpret_cast<const char16_t*>(aArgs[0].str16),
            aArgs[0].str16_len);
        NativeObj bytes;
        bytes.kind = kNativeBytes;
        bytes.bytes.AppendElements(
            reinterpret_cast<const uint8_t*>(utf8.get()), utf8.Length());
        aOut->tag = 3;
        aOut->num = double(AddNativeObj(std::move(bytes)));
        return;
      }
    } else if (name == "decode" && aArgc == 1 && aArgs[0].kind == 2 &&
               aArgs[0].num < 0) {
      // dec.decode(bytes) on a C++-owned buffer: UTF-8 decode straight out.
      if (NativeObj* o = NativeFor(aTarget);
          o && o->kind == kNativeTextDecoder) {
        NativeObj* buf = NativeFor(int64_t(aArgs[0].num));
        if (buf && buf->kind == kNativeBytes) {
          CopyUTF8toUTF16(
              nsDependentCSubstring(
                  reinterpret_cast<const char*>(buf->bytes.Elements()),
                  buf->bytes.Length()),
              mReplyScratch16);
          aOut->tag = 2;
          aOut->str16 =
              reinterpret_cast<const uint16_t*>(mReplyScratch16.BeginReading());
          aOut->str16_len = mReplyScratch16.Length();
          return;
        }
      }
    }
  }
  if (aTarget < 0 && !EnsureReflector(aTarget)) {
    return;
  }
  for (size_t i = 0; i < aArgc; i++) {
    if (aArgs[i].kind == 2 && aArgs[i].num < 0 &&
        !EnsureReflector(int64_t(aArgs[i].num))) {
      return;
    }
  }
  const double lead[2] = {double(aTarget), double(aNameId)};
  CallBridgeWide("callWide", lead, 2, aArgs, aArgc, aOut);
}

// A one-entry unwrap cache in front of UnwrapNative: a tight web loop hits the
// same receiver every iteration, so this is a compare instead of a hashmap
// probe. Same pointer lifetime as mNativeCache (valid until release).
void* MerseyScriptRunner::UnwrapCached(int64_t aHandle, int32_t& aKindOut) {
  if (aHandle != 0 && aHandle == mBindCacheHandle) {
    aKindOut = mBindCacheKind;
    return mBindCachePtr;
  }
  void* ptr = UnwrapNative(aHandle, aKindOut);
  mBindCacheHandle = aHandle;
  mBindCacheKind = aKindOut;
  mBindCachePtr = ptr;
  return ptr;
}

// The method name behind a bind id, for the reflective fallback (a receiver
// whose type does not match, or an id without a direct C++ path here yet).
static const char* BindMethodName(uint32_t aBindId) {
  switch (aBindId) {
    case MSY_BIND_CANVAS2D_FILLRECT: return "fillRect";
    case MSY_BIND_CANVAS2D_CLEARRECT: return "clearRect";
    case MSY_BIND_CANVAS2D_STROKERECT: return "strokeRect";
    case MSY_BIND_CANVAS2D_RECT: return "rect";
    case MSY_BIND_CANVAS2D_MOVETO: return "moveTo";
    case MSY_BIND_CANVAS2D_LINETO: return "lineTo";
    case MSY_BIND_CANVAS2D_TRANSLATE: return "translate";
    case MSY_BIND_CANVAS2D_SCALE: return "scale";
    case MSY_BIND_CANVAS2D_ROTATE: return "rotate";
    default: return nullptr;
  }
}

void MerseyScriptRunner::HostWebBind(int64_t aTarget, uint32_t aBindId,
                                     const double* aArgs, size_t aArgc,
                                     msy_reply* aOut) {
  // Canvas 2D numeric ops: unwrap the receiver once (cached), verify its type,
  // and call the C++ method with no interned name, no msy_arg16, no dispatch.
  // FillRect is infallible and returns nothing (tag 0).
  int32_t kind;
  void* ptr = UnwrapCached(aTarget, kind);
  if (kind == kNativeCanvas2D && ptr) {
    auto* ctx = static_cast<CanvasRenderingContext2D*>(ptr);
    switch (aBindId) {
      case MSY_BIND_CANVAS2D_FILLRECT:
        if (aArgc == 4) {
          ctx->FillRect(aArgs[0], aArgs[1], aArgs[2], aArgs[3]);
          aOut->tag = 0;
          return;
        }
        break;
      case MSY_BIND_CANVAS2D_CLEARRECT:
        if (aArgc == 4) {
          ctx->ClearRect(aArgs[0], aArgs[1], aArgs[2], aArgs[3]);
          aOut->tag = 0;
          return;
        }
        break;
      case MSY_BIND_CANVAS2D_STROKERECT:
        if (aArgc == 4) {
          ctx->StrokeRect(aArgs[0], aArgs[1], aArgs[2], aArgs[3]);
          aOut->tag = 0;
          return;
        }
        break;
      default:
        break;  // an id without a direct path yet: reflective fallback below
    }
  }
  // Type mismatch, or an id we do not fast-path here: rebuild the numeric
  // arguments and take the ordinary interned route, which is always correct.
  const char* name = BindMethodName(aBindId);
  if (!name) {
    aOut->tag = 0;
    return;
  }
  uint32_t nameId = HostWebIntern(std::string_view(name));
  msy_arg16 args[8];
  size_t n = aArgc < 8 ? aArgc : 8;
  for (size_t i = 0; i < n; i++) {
    args[i].kind = 1;  // number
    args[i].num = aArgs[i];
    args[i].str16 = nullptr;
    args[i].str16_len = 0;
  }
  HostWebCallU16(aTarget, nameId, args, n, aOut);
}

void MerseyScriptRunner::HostWebNewU16(uint32_t aCtorId, const msy_arg16* aArgs,
                                       size_t aArgc, msy_reply* aOut) {
  const std::string& name = InternedName(aCtorId);
  // new URL(s[, base]) — Gecko's URL parser, no JS. The object lives on the
  // C++ side; its reflector materializes only if it ever escapes.
  if (name == "URL" && aArgc >= 1 && aArgs[0].kind == 0 && mDocument) {
    NS_ConvertUTF16toUTF8 href(
        reinterpret_cast<const char16_t*>(aArgs[0].str16), aArgs[0].str16_len);
    // A one-arg `new URL(s)` has NO base: pass a null nsIURI* base (the
    // nsACString-base overload runs NS_NewURI on the base unconditionally, so
    // even a void base fails to parse). This mirrors URL's own GlobalObject
    // overload, which passes nullptr when the optional base was not supplied.
    ErrorResult rv;
    nsISupports* parent = mDocument->GetScopeObject();
    RefPtr<URL> url;
    if (aArgc >= 2 && aArgs[1].kind == 0) {
      nsAutoCString base;
      CopyUTF16toUTF8(
          nsDependentString(reinterpret_cast<const char16_t*>(aArgs[1].str16),
                            aArgs[1].str16_len),
          base);
      url = URL::Constructor(parent, href, base, rv);
    } else {
      url = URL::Constructor(parent, href, static_cast<nsIURI*>(nullptr), rv);
    }
    if (!rv.Failed() && url) {
      NativeObj o;
      o.kind = kNativeURL;
      o.ptr = url.get();
      o.holder = static_cast<URLSearchParamsObserver*>(url.get());
      const int64_t h = AddNativeObj(std::move(o));
      aOut->tag = 3;
      aOut->num = double(h);
      return;
    }
    rv.SuppressException();
    nsAutoString msg(u"URL constructor: ");
    AppendUTF8toUTF16(href, msg);
    msg.AppendLiteral(u" is not a valid URL.");
    ReplyStr16(mReplyScratch16, msg, 5, aOut);
    return;
  }
  if (name == "Event" && aArgc >= 1 && aArgs[0].kind == 0 && mDocument) {
    // new Event(type): a real, dispatchable DOM event — untrusted, like the
    // JS Event constructor's.
    IgnoredErrorResult rv;
    RefPtr<Event> ev = mDocument->CreateEvent(u"event"_ns, CallerType::System, rv);
    if (!rv.Failed() && ev) {
      nsDependentString type(reinterpret_cast<const char16_t*>(aArgs[0].str16),
                             aArgs[0].str16_len);
      ev->InitEvent(type, false, false);
      ev->SetTrusted(false);
      NativeObj o;
      o.kind = kNativeEvent;
      o.ptr = ev.get();
      o.holder = ToSupports(ev.get());
      aOut->tag = 3;
      aOut->num = double(AddNativeObj(std::move(o)));
      return;
    }
  }
  if (name == "TextEncoder" || name == "TextDecoder") {
    NativeObj o;
    o.kind = name == "TextEncoder" ? kNativeTextEncoder : kNativeTextDecoder;
    aOut->tag = 3;
    aOut->num = double(AddNativeObj(std::move(o)));
    return;
  }
  if (name == "Uint8Array" && aArgc == 1 && aArgs[0].kind == 1) {
    NativeObj o;
    o.kind = kNativeBytes;
    o.bytes.SetLength(size_t(aArgs[0].num));
    memset(o.bytes.Elements(), 0, o.bytes.Length());
    const int64_t h = AddNativeObj(std::move(o));
    aOut->tag = 3;
    aOut->num = double(h);
    return;
  }
  for (size_t i = 0; i < aArgc; i++) {
    if (aArgs[i].kind == 2 && aArgs[i].num < 0 &&
        !EnsureReflector(int64_t(aArgs[i].num))) {
      return;
    }
  }
  const double lead[1] = {double(aCtorId)};
  CallBridgeWide("newWide", lead, 1, aArgs, aArgc, aOut);
}

void MerseyScriptRunner::HostPrint(std::string_view aMessage, bool aIsError) {
  // Benchmark hook: when MERSEY_CONSOLE_STDOUT is set, echo console output to
  // stdout so a headless harness can capture it. The browser console does not
  // reach the terminal in headless mode. Off by default; normal runs unaffected.
  static const bool sEcho = getenv("MERSEY_CONSOLE_STDOUT") != nullptr;
  if (sEcho) {
    fprintf(stdout, "%.*s\n", (int)aMessage.size(), aMessage.data());
    fflush(stdout);
  }
  if (!mDocument) {
    return;
  }
  NS_ConvertUTF8toUTF16 message(aMessage.data(), aMessage.size());
  nsContentUtils::ReportToConsoleNonLocalized(
      message,
      aIsError ? nsIScriptError::errorFlag : nsIScriptError::infoFlag,
      "Mersey"_ns, mDocument);
}

void MerseyScriptRunner::HostSetText(std::string_view aId,
                                     std::string_view aText) {
  if (!mDocument) {
    return;
  }
  NS_ConvertUTF8toUTF16 id(aId.data(), aId.size());
  Element* element = mDocument->GetElementById(id);
  if (!element) {
    return;
  }
  NS_ConvertUTF8toUTF16 text(aText.data(), aText.size());
  IgnoredErrorResult rv;
  element->SetTextContent(text, rv);
}

const std::string* MerseyScriptRunner::HostGetText(std::string_view aId) {
  if (!mDocument) {
    return nullptr;
  }
  NS_ConvertUTF8toUTF16 id(aId.data(), aId.size());
  Element* element = mDocument->GetElementById(id);
  if (!element) {
    return nullptr;
  }
  nsAutoString text;
  IgnoredErrorResult rv;
  element->GetTextContent(text, rv);
  NS_ConvertUTF16toUTF8 utf8(text);
  mGetTextScratch.assign(utf8.get(), utf8.Length());
  return &mGetTextScratch;
}

void MerseyScriptRunner::HostAddListener(std::string_view aId,
                                         std::string_view aEvent,
                                         uint32_t aCallbackId) {
  if (!mDocument) {
    return;
  }
  NS_ConvertUTF8toUTF16 id(aId.data(), aId.size());
  Element* element = mDocument->GetElementById(id);
  if (!element) {
    return;
  }
  NS_ConvertUTF8toUTF16 event(aEvent.data(), aEvent.size());
  RefPtr<MerseyEventListener> listener =
      new MerseyEventListener(this, aCallbackId);
  element->AddEventListener(event, listener, /* useCapture */ false);
}

double MerseyScriptRunner::HostTimeMs(bool aEpoch) {
  if (aEpoch) {
    return static_cast<double>(PR_Now()) / PR_USEC_PER_MSEC;
  }
  // A high-resolution monotonic clock. PR_IntervalNow truncates to whole
  // milliseconds — too coarse to see a per-call optimization in a tight web
  // loop. TimeStamp is Gecko's monotonic clock at sub-microsecond resolution;
  // a fixed process-creation origin keeps the returned values small.
  static const mozilla::TimeStamp origin = mozilla::TimeStamp::ProcessCreation();
  return (mozilla::TimeStamp::Now() - origin).ToMilliseconds();
}

bool MerseyScriptRunner::HostRandomBytes(uint8_t* aBuf, size_t aLength) {
  nsCOMPtr<nsIRandomGenerator> generator =
      do_GetService("@mozilla.org/security/random-generator;1");
  if (!generator) {
    return false;
  }
  uint8_t* bytes = nullptr;
  if (NS_FAILED(generator->GenerateRandomBytes(aLength, &bytes)) || !bytes) {
    return false;
  }
  memcpy(aBuf, bytes, aLength);
  free(bytes);
  return true;
}

}  // namespace mozilla::dom
