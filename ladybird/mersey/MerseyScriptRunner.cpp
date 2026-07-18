/*
 * Copyright (c) 2026, the Mersey project.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibWeb/Mersey/MerseyScriptRunner.h>

#include <AK/AllOf.h>
#include <AK/ByteString.h>
#include <AK/CharacterTypes.h>
#include <AK/Format.h>
#include <AK/HashMap.h>
#include <AK/JsonArray.h>
#include <AK/JsonObject.h>
#include <AK/JsonValue.h>
#include <AK/OwnPtr.h>
#include <AK/StringBuilder.h>
#include <AK/Time.h>
#include <AK/Utf16FlyString.h>
#include <AK/Utf16String.h>
#include <AK/Utf16View.h>
#include <AK/Vector.h>
#include <LibGC/RootVector.h>
#include <LibJS/Runtime/AbstractOperations.h>
#include <LibJS/Runtime/Array.h>
#include <LibJS/Runtime/ArrayBuffer.h>
#include <LibJS/Runtime/BoundFunction.h>
#include <LibJS/Runtime/Completion.h>
#include <LibJS/Runtime/DataView.h>
#include <LibJS/Runtime/Error.h>
#include <LibJS/Runtime/Iterator.h>
#include <LibJS/Runtime/NativeFunction.h>
#include <LibJS/Runtime/Object.h>
#include <LibJS/Runtime/PrimitiveString.h>
#include <LibJS/Runtime/PropertyKey.h>
#include <LibJS/Runtime/Realm.h>
#include <LibJS/Runtime/TypedArray.h>
#include <LibJS/Runtime/Value.h>
#include <LibJS/Runtime/VM.h>
#include <LibWeb/CSS/CSSStyleDeclaration.h>
#include <LibWeb/CSS/CSSStyleProperties.h>
#include <LibWeb/Crypto/Crypto.h>
#include <LibWeb/DOM/DOMTokenList.h>
#include <LibWeb/DOM/Document.h>
#include <LibWeb/DOM/Element.h>
#include <LibWeb/DOM/ElementFactory.h>
#include <LibWeb/DOM/Event.h>
#include <LibWeb/DOM/EventTarget.h>
#include <LibWeb/DOM/Node.h>
#include <LibWeb/DOM/NodeList.h>
#include <LibWeb/DOM/ParentNode.h>
#include <LibWeb/DOMURL/DOMURL.h>
#include <LibWeb/Encoding/TextDecoder.h>
#include <LibWeb/Encoding/TextEncoder.h>
#include <LibWeb/HTML/AttributeNames.h>
#include <LibWeb/HTML/CanvasRenderingContext2D.h>
#include <LibWeb/HTML/Scripting/Environments.h>
#include <LibWeb/HTML/Scripting/TemporaryExecutionContext.h>
#include <LibWeb/HTML/Storage.h>
#include <LibWeb/HTML/Window.h>
#include <LibWeb/HTML/WindowOrWorkerGlobalScope.h>
#include <LibWeb/Namespace.h>
#include <LibWeb/WebIDL/Buffers.h>
#include <LibWeb/WebIDL/CallbackType.h>

namespace Web::Mersey {

// Capabilities granted to the engine (spec §5.4). Matches the Gecko/Servo
// forks: the whole web surface is reachable, and the engine still gates each
// API by import.
static constexpr StringView CAPS = "[\"dom\",\"web\",\"time\",\"random\",\"net\",\"storage\"]"sv;

// Member names with a direct-C++ path (the direct-DOM tier): the engine interns
// a name once, we classify it then, and the host hooks switch on the id straight
// to the LibWeb C++ method. Everything else takes the native reflective path
// below — LibJS property gets / JS::call on the IDL bindings — which still
// never evaluates or calls any JS source: the whole bridge is C++.
enum class HotMethod : u8 {
    None,
    GetRandomValues,  // crypto.getRandomValues(buf) -> Crypto::get_random_values
    CtorURL,          // new URL(str)               -> DOMURL::construct_impl
    Pathname,         // url.pathname               -> DOMURL::pathname
    Search,           // url.search                 -> DOMURL::search
    CreateElement,    // document.createElement(t)  -> DOM::create_element
    AppendChild,      // node.appendChild(child)    -> Node::append_child
    TextContent,      // el.textContent (get + set) -> Node::text_content / set_text_content
    CtorEvent,        // new Event(type)            -> DOM::Event::create
    DispatchEvent,    // el.dispatchEvent(ev)       -> EventTarget::dispatch_event
    ClassName,        // el.className = s           -> Element::set_attribute_value(class)
    ClassList,        // el.classList               -> Element::class_list
    Contains,         // tokens.contains(s)         -> DOMTokenList::contains
    Style,            // el.style                   -> Element::style_for_bindings
    SetProperty,      // style.setProperty(p, v)    -> CSSStyleDeclaration::set_property
    GetPropertyValue, // style.getPropertyValue(p)  -> CSSStyleDeclaration::get_property_value
    QuerySelectorAll, // doc.querySelectorAll(sel)  -> ParentNode::query_selector_all
    Length,           // nodes.length               -> NodeList::length
    Index,            // nodes[i] (digit-only name) -> NodeList::item
    Encode,           // enc.encode(s)              -> TextEncoder::encode
    Decode,           // dec.decode(bytes)          -> TextDecoder::decode
    GetItem,          // storage.getItem(k)         -> HTML::Storage::get_item
    SetItem,          // storage.setItem(k, v)      -> HTML::Storage::set_item
    RemoveItem,       // storage.removeItem(k)      -> HTML::Storage::remove_item
};

// Per-realm engine runner, reached through a raw thread-local pointer (see the
// header's threading note — a host call can legitimately re-enter).
struct Runner {
    msy_context* ctx { nullptr };
    // The realm whose global hosts the page (and every object a handle names).
    JS::Realm* realm { nullptr };
    // Backing store for a reply the engine reads — valid until the next host
    // call on this runner, exactly the C-ABI buffer-lifetime contract. A
    // ByteString (UTF-8) keeps a stable (characters, length) the ABI borrows.
    ByteString scratch;
    // Backing store for a typed UTF-16 reply (msy_reply::str16) on the wide path.
    Vector<uint16_t> reply_str16;
    // The native handle table — the C++ counterpart of the JS bridge's
    // `handles` array, owned here so no JS is involved in object identity.
    // Handle 0 is the realm's global object; slots are nulled on web_release
    // (indices must stay stable). The RootVector keeps every entry alive for
    // the GC; by_object dedups object handles exactly like the bridge's Map.
    OwnPtr<GC::RootVector<JS::Value>> handles;
    HashMap<JS::Object*, int64_t> by_object;
    // Interned member names (the id is the index) and their hot classification.
    Vector<ByteString> names;
    HashMap<ByteString, uint32_t> name_ids;
    Vector<HotMethod> hot;
    // For HotMethod::Index: the parsed index the digit-only name spells.
    Vector<uint32_t> hot_arg;
    // Callable-handle globals with a direct-C++ path in web_call: `setTimeout(cb,
    // ms)` crosses on the JSON path (the closure argument forces it), and we
    // dispatch it straight to WindowOrWorkerGlobalScopeMixin instead of calling
    // the binding function.
    int64_t set_timeout_handle { INT64_MIN };
    int64_t clear_timeout_handle { INT64_MIN };
    // One NativeFunction per callback id — the engine keys ids by closure
    // identity, so the same Mersey function is the same JS function on every
    // crossing (removeEventListener removes; a setTimeout/clearTimeout loop
    // stops allocating a function per call). Rooted like the handle table and
    // rebuilt with the realm. If a host ever calls
    // msy_context_release_callback it must evict here too: the engine reuses
    // released ids.
    OwnPtr<GC::RootVector<JS::Value>> cb_wrappers;
    HashMap<uint32_t, size_t> cb_wrapper_index;
    // Typed-binding fast path (web_bind): the canvas draw loop reuses one context,
    // so its C++ pointer is cached by handle — one unwrap amortized over the
    // loop, then every fillRect is a direct call. LibGC is non-moving and the
    // handle table holds the context alive, so the pointer is safe.
    int64_t canvas_handle { INT64_MIN };
    GC::Ptr<Web::HTML::CanvasRenderingContext2D> canvas_ctx;
    MonotonicTime start { MonotonicTime::now() };
};

static thread_local Runner* s_runner { nullptr };

// ---- native handle table ---------------------------------------------------

// (Re)build the table for a realm: handle 0 = the global object.
static void reset_handles(Runner* runner, JS::Realm& realm)
{
    runner->handles = make<GC::RootVector<JS::Value>>();
    runner->by_object.clear();
    auto& global = realm.global_object();
    // Announce native Mersey to the page: the Stage A polyfill loader sees
    // this and stands down (no WASM fetch, no double execution).
    (void)global.create_data_property(JS::PropertyKey { "merseyNative"_utf16_fly_string }, JS::Value(true));
    runner->handles->append(JS::Value(&global));
    runner->by_object.set(&global, 0);
    runner->set_timeout_handle = INT64_MIN;
    runner->clear_timeout_handle = INT64_MIN;
    runner->cb_wrappers = make<GC::RootVector<JS::Value>>();
    runner->cb_wrapper_index.clear();
    runner->canvas_handle = INT64_MIN;
    runner->canvas_ctx = nullptr;
}

// The value a handle names: js_null for a released slot (the bridge's `null`
// entry — stale), js_undefined for a handle that was never allocated.
static JS::Value handle_value(Runner* runner, int64_t handle)
{
    if (!runner->handles || handle < 0 || static_cast<size_t>(handle) >= runner->handles->size())
        return JS::js_undefined();
    return (*runner->handles)[handle];
}

static GC::Ptr<JS::Object> handle_object(Runner* runner, int64_t handle)
{
    auto v = handle_value(runner, handle);
    if (!v.is_object())
        return nullptr;
    return &v.as_object();
}

// The handle for a value, allocating one if needed (objects dedup, so a stable
// object keeps a stable handle — the loop-carried receivers depend on that).
static int64_t handle_for(Runner* runner, JS::Value v)
{
    if (v.is_object()) {
        if (auto it = runner->by_object.find(&v.as_object()); it != runner->by_object.end())
            return it->value;
    }
    int64_t h = static_cast<int64_t>(runner->handles->size());
    runner->handles->append(v);
    if (v.is_object())
        runner->by_object.set(&v.as_object(), h);
    return h;
}

// ---- tagged-JSON encode / decode (the C++ web/mersey-bridge.js) ------------
// The engine's reflective wire format (mersey.h): primitives are JSON scalars,
// host objects cross as {"__ref__": handle}, Mersey closures arrive as
// {"__cb__": id} and become NativeFunctions that re-enter the engine. This is
// exactly the bridge JS's encode/decode, natively — no JS runs.

static void append_json_quoted(StringBuilder& out, StringView utf8)
{
    out.append('"');
    for (char c : utf8) {
        switch (c) {
        case '"':
            out.append("\\\""sv);
            break;
        case '\\':
            out.append("\\\\"sv);
            break;
        case '\b':
            out.append("\\b"sv);
            break;
        case '\f':
            out.append("\\f"sv);
            break;
        case '\n':
            out.append("\\n"sv);
            break;
        case '\r':
            out.append("\\r"sv);
            break;
        case '\t':
            out.append("\\t"sv);
            break;
        default:
            if (static_cast<unsigned char>(c) < 0x20)
                out.appendff("\\u{:04x}", static_cast<unsigned char>(c));
            else
                out.append(c);
        }
    }
    out.append('"');
}

// One value, encoded. Non-finite numbers become null (JSON.stringify's rule);
// anything non-primitive becomes a (deduped) handle ref.
static void encode_value(Runner* runner, StringBuilder& out, JS::Value v)
{
    if (v.is_nullish()) {
        out.append("null"sv);
        return;
    }
    if (v.is_boolean()) {
        out.append(v.as_bool() ? "true"sv : "false"sv);
        return;
    }
    if (v.is_number()) {
        double d = v.as_double();
        if (!__builtin_isfinite(d)) {
            out.append("null"sv);
            return;
        }
        JS::number_to_string(out, d);
        return;
    }
    if (v.is_string()) {
        append_json_quoted(out, v.as_string().utf16_string().to_byte_string());
        return;
    }
    if (v.is_bigint()) {
        append_json_quoted(out, v.to_utf16_string_without_side_effects().to_byte_string());
        return;
    }
    out.appendff("{{\"__ref__\":{}}}", handle_for(runner, v));
}

// A Mersey closure crossing into JS-shaped code (an event listener, a promise
// reaction, a timer callback): a NativeFunction that encodes its arguments and
// re-enters the engine — C++ end to end, no JS trampoline. One wrapper per id,
// cached: ids are stable per closure, so identity survives crossings.
static JS::Value make_callback(Runner* runner, uint32_t id)
{
    if (auto idx = runner->cb_wrapper_index.get(id); idx.has_value())
        return (*runner->cb_wrappers)[*idx];
    auto fn = JS::NativeFunction::create(*runner->realm, [runner, id](JS::VM& vm) -> JS::ThrowCompletionOr<JS::Value> {
        StringBuilder json;
        json.append('[');
        for (size_t i = 0; i < vm.argument_count(); ++i) {
            if (i)
                json.append(',');
            encode_value(runner, json, vm.argument(i));
        }
        json.append(']');
        auto text = json.to_byte_string();
        msy_context_invoke_args(runner->ctx, id, text.characters(), text.length());
        return JS::js_undefined();
    }, 0);
    runner->cb_wrapper_index.set(id, runner->cb_wrappers->size());
    runner->cb_wrappers->append(JS::Value(fn.ptr()));
    return JS::Value(fn.ptr());
}

// A parsed JSON value back to a JS value. Practically infallible (the inputs
// are the engine's own well-formed argument JSON); allocation failures abort.
static JS::Value decode_json(Runner* runner, JsonValue const& v)
{
    auto& realm = *runner->realm;
    auto& vm = realm.vm();
    if (v.is_null())
        return JS::js_null();
    if (v.is_bool())
        return JS::Value(v.as_bool());
    if (v.is_number())
        return JS::Value(v.get_double_with_precision_loss().value_or(0));
    if (v.is_string())
        return JS::PrimitiveString::create(vm, Utf16String::from_utf8(v.as_string()));
    if (v.is_array()) {
        auto const& elems = v.as_array();
        GC::RootVector<JS::Value> items;
        for (size_t i = 0; i < elems.size(); ++i)
            items.append(decode_json(runner, elems.at(i)));
        return JS::Array::create_from(realm, items.span());
    }
    auto const& obj = v.as_object();
    if (auto ref = obj.get_i64("__ref__"sv); ref.has_value())
        return handle_value(runner, *ref);
    if (auto cb = obj.get_i64("__cb__"sv); cb.has_value())
        return make_callback(runner, static_cast<uint32_t>(*cb));
    auto plain = JS::Object::create(realm, realm.intrinsics().object_prototype());
    obj.for_each_member([&](auto const& key, JsonValue const& member) {
        auto member_value = decode_json(runner, member);
        (void)plain->create_data_property(JS::PropertyKey { Utf16String::from_utf8(key) }, member_value);
    });
    return plain;
}

// Handles only ever name objects (the bridge never allocates a handle for a
// primitive), so the ToObject coercions on the host paths reduce to a check.
// Local because JS::Value::to_object is not JS_API-exported in current trees.
static JS::ThrowCompletionOr<GC::Ref<JS::Object>> target_object(JS::VM& vm, JS::Value tv)
{
    if (tv.is_object())
        return GC::Ref { tv.as_object() };
    return JS::throw_completion(JS::Value(JS::PrimitiveString::create(vm, Utf16String::from_utf8("value is not an object"sv))));
}

// Ordinary `instanceof` (prototype-chain walk against Ctor.prototype). Local
// because JS::instance_of is not JS_API-exported in current trees; host types
// don't customize Symbol.hasInstance, so the ordinary algorithm is the whole
// story here.
static JS::ThrowCompletionOr<bool> value_instance_of(JS::Value v, JS::Value ctor)
{
    if (!v.is_object() || !ctor.is_object())
        return false;
    auto proto_val = TRY(ctor.as_object().get(JS::PropertyKey { "prototype"_utf16_fly_string }));
    if (!proto_val.is_object())
        return false;
    auto* proto = &proto_val.as_object();
    for (auto* p = TRY(v.as_object().internal_get_prototype_of()); p; p = TRY(p->internal_get_prototype_of())) {
        if (p == proto)
            return true;
    }
    return false;
}

// Bind a method to its receiver, the bridge's `v.bind(obj)`. Not
// JS::BoundFunction — that class is not JS_API-exported in current trees — but
// an exported NativeFunction forwarding the call; the GC::Roots in the capture
// keep callee and receiver alive for the wrapper's lifetime.
static JS::Value bind_receiver(Runner* runner, JS::Value fn, JS::Value receiver)
{
    return JS::Value(JS::NativeFunction::create(*runner->realm,
        [fn_root = GC::make_root(fn), recv_root = GC::make_root(receiver)](JS::VM& vm) -> JS::ThrowCompletionOr<JS::Value> {
            GC::RootVector<JS::Value> args;
            for (size_t i = 0; i < vm.argument_count(); ++i)
                args.append(vm.argument(i));
            return JS::call(vm, fn_root.value(), recv_root.value(), args.span());
        },
        0).ptr());
}

// ---- reply plumbing --------------------------------------------------------

// {"ok": value} — with the bridge's top-level-array rule: a returned JS array
// crosses inline as an array of encoded elements, not as a handle.
static JS::ThrowCompletionOr<ByteString> ok_reply(Runner* runner, JS::Value v)
{
    auto& vm = runner->realm->vm();
    StringBuilder b;
    b.append("{\"ok\":"sv);
    if (v.is_object() && is<JS::Array>(v.as_object())) {
        auto& arr = v.as_object();
        auto len = TRY(JS::length_of_array_like(vm, arr));
        b.append('[');
        for (size_t i = 0; i < len; ++i) {
            if (i)
                b.append(',');
            encode_value(runner, b, TRY(arr.get(JS::PropertyKey { static_cast<u32>(i) })));
        }
        b.append(']');
    } else {
        encode_value(runner, b, v);
    }
    b.append('}');
    return b.to_byte_string();
}

// The thrown value's message, the way the bridge spelled it: e.message when
// there is one, else the value's stringification.
static Utf16String error_message(JS::Value error)
{
    if (error.is_object()) {
        auto message = error.as_object().get_without_side_effects(JS::PropertyKey { "message"_utf16_fly_string });
        if (message.is_string())
            return message.as_string().utf16_string();
    }
    return error.to_utf16_string_without_side_effects();
}

static ByteString err_reply(JS::Value error)
{
    StringBuilder b;
    b.append("{\"err\":"sv);
    append_json_quoted(b, error_message(error).to_byte_string());
    b.append('}');
    return b.to_byte_string();
}

// Run a completion-returning body and stash its reply (or the error reply) in
// the runner scratch, handing back the ABI (ptr, len).
template<typename F>
static char const* json_hook(size_t* out_len, F&& body)
{
    auto* runner = s_runner;
    if (!runner || !runner->realm) {
        *out_len = 0;
        return nullptr;
    }
    auto result = body(runner);
    if (result.is_error())
        runner->scratch = err_reply(result.release_error().value());
    else
        runner->scratch = result.release_value();
    *out_len = runner->scratch.length();
    return runner->scratch.characters();
}

// ---- host table: basics ----------------------------------------------------

static void host_print(void*, char const* utf8, size_t len)
{
    // The headless bench harness reads the RESULT line from stdout (like the
    // Servo fork's stdout `print`), so write there directly and flush.
    out("{}\n", StringView { utf8, len });
    fflush(stdout);
}

static char const* host_caps(void*, size_t* out_len)
{
    *out_len = CAPS.length();
    return CAPS.characters_without_null_termination();
}

static double host_time_ms(void*, int32_t epoch)
{
    if (epoch != 0)
        return static_cast<double>(UnixDateTime::now().milliseconds_since_epoch());
    auto* runner = s_runner;
    if (!runner)
        return 0.0;
    return static_cast<double>((MonotonicTime::now() - runner->start).to_nanoseconds()) / 1.0e6;
}

// ---- host table: reflective ops (native) -----------------------------------

static int64_t host_web_global(void*, char const* name, size_t len)
{
    auto* runner = s_runner;
    if (!runner || !runner->realm)
        return -1;
    StringView n { name, len };
    auto& global = runner->realm->global_object();
    auto key = JS::PropertyKey { Utf16String::from_utf8(n) };
    auto has_or = global.has_property(key);
    if (has_or.is_error() || !has_or.value())
        return -1;
    auto value_or = global.get(key);
    if (value_or.is_error())
        return -1;
    auto h = handle_for(runner, value_or.value());
    // Callable-handle globals with a direct-C++ call path (see host_web_call).
    if (n == "setTimeout"sv)
        runner->set_timeout_handle = h;
    else if (n == "clearTimeout"sv)
        runner->clear_timeout_handle = h;
    return h;
}

static char const* host_web_get(void*, int64_t target, char const* prop, size_t prop_len, size_t* out_len)
{
    return json_hook(out_len, [&](Runner* runner) -> JS::ThrowCompletionOr<ByteString> {
        auto& vm = runner->realm->vm();
        auto tv = handle_value(runner, target);
        auto obj = TRY(target_object(vm, tv));
        auto v = TRY(obj->get(JS::PropertyKey { Utf16String::from_utf8(StringView { prop, prop_len }) }));
        // A method read binds its receiver (the bridge's v.bind(obj)), so a
        // later call through the handle has its `this`.
        if (v.is_function())
            v = bind_receiver(runner, v, tv);
        return ok_reply(runner, v);
    });
}

static char const* host_web_set(void*, int64_t target, char const* prop, size_t prop_len,
    char const* value_json, size_t value_len, size_t* out_len)
{
    return json_hook(out_len, [&](Runner* runner) -> JS::ThrowCompletionOr<ByteString> {
        auto& vm = runner->realm->vm();
        auto tv = handle_value(runner, target);
        auto obj = TRY(target_object(vm, tv));
        auto parsed = JsonValue::from_string(StringView { value_json, value_len });
        auto v = parsed.is_error() ? JS::js_null() : decode_json(runner, parsed.value());
        TRY(obj->set(JS::PropertyKey { Utf16String::from_utf8(StringView { prop, prop_len }) }, v,
            JS::Object::ShouldThrowExceptions::Yes));
        return ok_reply(runner, JS::js_null());
    });
}

// setTimeout(cb, ms, ...) straight to WindowOrWorkerGlobalScopeMixin — the
// callback is already a C++ NativeFunction, so the whole timer arm/fire path
// stays out of the binding layer. Shared by the JSON path and the u16 tier.
static JS::ThrowCompletionOr<i32> set_timeout_core(Runner* runner, ReadonlySpan<JS::Value> args)
{
    auto& global = runner->realm->global_object();
    auto& window = as<HTML::Window>(global);
    i32 delay = args.size() > 1 && args[1].is_number() ? static_cast<i32>(args[1].as_double()) : 0;
    auto callback_value = args[0];
    auto callback = runner->realm->heap().allocate<WebIDL::CallbackType>(callback_value.as_object(), *runner->realm);
    GC::RootVector<JS::Value> extra;
    for (size_t i = 2; i < args.size(); ++i)
        extra.append(args[i]);
    return window.set_timeout(HTML::TimerHandler { GC::Ref { *callback } }, delay, move(extra));
}

static JS::ThrowCompletionOr<ByteString> set_timeout_direct(Runner* runner, ReadonlySpan<JS::Value> args)
{
    return ok_reply(runner, JS::Value(TRY(set_timeout_core(runner, args))));
}

static char const* host_web_call(void*, int64_t target, char const* method, size_t method_len,
    char const* args_json, size_t args_len, size_t* out_len)
{
    return json_hook(out_len, [&](Runner* runner) -> JS::ThrowCompletionOr<ByteString> {
        auto& vm = runner->realm->vm();
        StringView method_name { method, method_len };

        GC::RootVector<JS::Value> args;
        auto parsed = JsonValue::from_string(StringView { args_json, args_len });
        if (!parsed.is_error() && parsed.value().is_array()) {
            auto const& arr = parsed.value().as_array();
            for (size_t i = 0; i < arr.size(); ++i)
                args.append(decode_json(runner, arr.at(i)));
        }

        auto tv = handle_value(runner, target);
        if (method_name.is_empty()) {
            // The handle is itself callable (an imported `setTimeout`, `fetch`).
            if (!args.is_empty() && args[0].is_function() && is<HTML::Window>(runner->realm->global_object())) {
                if (target == runner->set_timeout_handle)
                    return set_timeout_direct(runner, args.span());
            }
            if (target == runner->clear_timeout_handle && !args.is_empty() && args[0].is_number()
                && is<HTML::Window>(runner->realm->global_object())) {
                as<HTML::Window>(runner->realm->global_object()).clear_timeout(static_cast<i32>(args[0].as_double()));
                return ok_reply(runner, JS::js_null());
            }
            if (!tv.is_function())
                return ByteString { "{\"err\":\"value is not a function\"}" };
            return ok_reply(runner, TRY(JS::call(vm, tv, JS::js_undefined(), args.span())));
        }
        auto obj = TRY(target_object(vm, tv));
        auto fn = TRY(obj->get(JS::PropertyKey { Utf16String::from_utf8(method_name) }));
        if (!fn.is_function())
            return ByteString::formatted("{{\"err\":\"{} is not a function\"}}", method_name);
        return ok_reply(runner, TRY(JS::call(vm, fn, tv, args.span())));
    });
}

// Resolve a (possibly dotted, `Intl.NumberFormat`) constructor name from the
// global — the bridge's reduce over the path.
static JS::ThrowCompletionOr<JS::Value> resolve_ctor(Runner* runner, StringView name)
{
    JS::Value o { &runner->realm->global_object() };
    auto& vm = runner->realm->vm();
    for (auto segment : name.split_view('.')) {
        auto obj = TRY(target_object(vm, o));
        o = TRY(obj->get(JS::PropertyKey { Utf16String::from_utf8(segment) }));
    }
    return o;
}

static char const* host_web_new(void*, char const* ctor, size_t ctor_len,
    char const* args_json, size_t args_len, size_t* out_len)
{
    return json_hook(out_len, [&](Runner* runner) -> JS::ThrowCompletionOr<ByteString> {
        auto& vm = runner->realm->vm();
        GC::RootVector<JS::Value> args;
        auto parsed = JsonValue::from_string(StringView { args_json, args_len });
        if (!parsed.is_error() && parsed.value().is_array()) {
            auto const& arr = parsed.value().as_array();
            for (size_t i = 0; i < arr.size(); ++i)
                args.append(decode_json(runner, arr.at(i)));
        }
        auto ctor_value = TRY(resolve_ctor(runner, StringView { ctor, ctor_len }));
        if (!ctor_value.is_function())
            return ByteString::formatted("{{\"err\":\"{} is not a constructor\"}}", StringView { ctor, ctor_len });
        auto instance = TRY(JS::construct(vm, ctor_value.as_function(), args.span()));
        return ok_reply(runner, JS::Value(instance.ptr()));
    });
}

static char const* host_web_iterate(void*, int64_t target, size_t* out_len)
{
    return json_hook(out_len, [&](Runner* runner) -> JS::ThrowCompletionOr<ByteString> {
        auto& vm = runner->realm->vm();
        auto tv = handle_value(runner, target);
        if (tv.is_nullish())
            return ByteString::formatted("{{\"err\":\"stale handle {}\"}}", target);

        GC::RootVector<JS::Value> items;
        if (tv.is_object() && is<JS::Array>(tv.as_object())) {
            auto& arr = tv.as_object();
            auto len = TRY(JS::length_of_array_like(vm, arr));
            for (size_t i = 0; i < len; ++i)
                items.append(TRY(arr.get(JS::PropertyKey { static_cast<u32>(i) })));
        } else {
            // The iterator protocol first; array-likes (a length but no
            // Symbol.iterator) as the fallback — the bridge's order.
            auto iterator_or = JS::get_iterator(vm, tv, JS::IteratorHint::Sync);
            if (!iterator_or.is_error()) {
                auto list = TRY(JS::iterator_to_list(vm, *iterator_or.value()));
                for (auto v : list)
                    items.append(v);
            } else if (tv.is_object()) {
                auto len_value = TRY(tv.as_object().get(JS::PropertyKey { "length"_utf16_fly_string }));
                if (!len_value.is_number())
                    return ByteString { "{\"err\":\"value is not iterable\"}" };
                auto len = static_cast<size_t>(len_value.as_double());
                for (size_t i = 0; i < len; ++i)
                    items.append(TRY(tv.as_object().get(JS::PropertyKey { static_cast<u32>(i) })));
            } else {
                return ByteString { "{\"err\":\"value is not iterable\"}" };
            }
        }

        StringBuilder b;
        b.append("{\"ok\":["sv);
        for (size_t i = 0; i < items.size(); ++i) {
            if (i)
                b.append(',');
            encode_value(runner, b, items[i]);
        }
        b.append("]}"sv);
        return b.to_byte_string();
    });
}

static int32_t host_web_instanceof(void*, int64_t target, int64_t ctor)
{
    auto* runner = s_runner;
    if (!runner || !runner->realm)
        return 0;
    auto& vm = runner->realm->vm();
    (void)vm;
    auto result = value_instance_of(handle_value(runner, target), handle_value(runner, ctor));
    if (result.is_error())
        return 0;
    return result.value() ? 1 : 0;
}

static void host_web_release(void*, int64_t target)
{
    auto* runner = s_runner;
    if (!runner || !runner->handles || target <= 0 || static_cast<size_t>(target) >= runner->handles->size())
        return;
    auto v = (*runner->handles)[target];
    if (v.is_object())
        runner->by_object.remove(&v.as_object());
    (*runner->handles)[target] = JS::js_null();
    if (runner->canvas_handle == target) {
        runner->canvas_handle = INT64_MIN;
        runner->canvas_ctx = nullptr;
    }
}

// ---- interning + hot classification (ABI v3) --------------------------------

static uint32_t host_web_intern(void*, char const* name, size_t len)
{
    auto* runner = s_runner;
    if (!runner)
        return UINT32_MAX;
    ByteString n { StringView { name, len } };
    uint32_t id;
    if (auto it = runner->name_ids.find(n); it != runner->name_ids.end()) {
        id = it->value;
    } else {
        id = static_cast<uint32_t>(runner->names.size());
        runner->names.append(n);
        runner->name_ids.set(n, id);
    }
    // Classify the name once, so the wide-path hooks can dispatch a hot method
    // straight to its C++ path by id (the engine interns before it ever calls).
    auto sv = n.view();
    auto hot = HotMethod::None;
    if (sv == "getRandomValues"sv)
        hot = HotMethod::GetRandomValues;
    else if (sv == "URL"sv)
        hot = HotMethod::CtorURL;
    else if (sv == "pathname"sv)
        hot = HotMethod::Pathname;
    else if (sv == "search"sv)
        hot = HotMethod::Search;
    else if (sv == "createElement"sv)
        hot = HotMethod::CreateElement;
    else if (sv == "appendChild"sv)
        hot = HotMethod::AppendChild;
    else if (sv == "textContent"sv)
        hot = HotMethod::TextContent;
    else if (sv == "Event"sv)
        hot = HotMethod::CtorEvent;
    else if (sv == "dispatchEvent"sv)
        hot = HotMethod::DispatchEvent;
    else if (sv == "className"sv)
        hot = HotMethod::ClassName;
    else if (sv == "classList"sv)
        hot = HotMethod::ClassList;
    else if (sv == "contains"sv)
        hot = HotMethod::Contains;
    else if (sv == "style"sv)
        hot = HotMethod::Style;
    else if (sv == "setProperty"sv)
        hot = HotMethod::SetProperty;
    else if (sv == "getPropertyValue"sv)
        hot = HotMethod::GetPropertyValue;
    else if (sv == "querySelectorAll"sv)
        hot = HotMethod::QuerySelectorAll;
    else if (sv == "length"sv)
        hot = HotMethod::Length;
    else if (sv == "encode"sv)
        hot = HotMethod::Encode;
    else if (sv == "decode"sv)
        hot = HotMethod::Decode;
    else if (sv == "getItem"sv)
        hot = HotMethod::GetItem;
    else if (sv == "setItem"sv)
        hot = HotMethod::SetItem;
    else if (sv == "removeItem"sv)
        hot = HotMethod::RemoveItem;
    uint32_t index_arg = 0;
    // A digit-only name is an indexed access crossing as a property read
    // (`nodes[i]` interns "42"); dispatch it to NodeList::item by value.
    if (hot == HotMethod::None && !sv.is_empty() && sv.length() <= 9
        && all_of(sv, [](char c) { return is_ascii_digit(c); })) {
        hot = HotMethod::Index;
        index_arg = sv.to_number<u32>().value_or(0);
    }
    while (runner->hot.size() <= id) {
        runner->hot.append(HotMethod::None);
        runner->hot_arg.append(0);
    }
    runner->hot[id] = hot;
    runner->hot_arg[id] = index_arg;
    return id;
}

// ---- wide-string paths (ABI v5): typed args and replies, UTF-16, no JSON ---

// Copy a UTF-16 string into the runner reply buffer (stable until the next
// boundary call) and point the typed reply at it.
static void fill_str16(Runner* r, msy_reply* out, int32_t tag, Utf16String const& s)
{
    auto view = s.utf16_view();
    size_t n = view.length_in_code_units();
    r->reply_str16.resize(n);
    for (size_t i = 0; i < n; ++i)
        r->reply_str16[i] = static_cast<uint16_t>(view.code_unit_at(i));
    out->tag = tag;
    out->str16 = r->reply_str16.data();
    out->str16_len = n;
}

// Type a JS value into a wide reply: scalars as themselves, a top-level array
// inline as tagged JSON (tag 7), anything else as a (deduped) handle ref.
static void wide_fill(Runner* runner, msy_reply* out, JS::Value v)
{
    *out = {};
    if (v.is_nullish()) {
        out->tag = 0;
        return;
    }
    if (v.is_number()) {
        out->tag = 1;
        out->num = v.as_double();
        return;
    }
    if (v.is_boolean()) {
        out->tag = 4;
        out->num = v.as_bool() ? 1 : 0;
        return;
    }
    if (v.is_string()) {
        fill_str16(runner, out, 2, v.as_string().utf16_string());
        return;
    }
    if (v.is_bigint()) {
        fill_str16(runner, out, 2, v.to_utf16_string_without_side_effects());
        return;
    }
    if (v.is_object() && is<JS::Array>(v.as_object())) {
        auto& vm = runner->realm->vm();
        StringBuilder b;
        b.append('[');
        auto len_or = JS::length_of_array_like(vm, v.as_object());
        size_t len = len_or.is_error() ? 0 : len_or.value();
        for (size_t i = 0; i < len; ++i) {
            if (i)
                b.append(',');
            auto el = v.as_object().get(JS::PropertyKey { i });
            encode_value(runner, b, el.is_error() ? JS::js_undefined() : el.value());
        }
        b.append(']');
        fill_str16(runner, out, 7, Utf16String::from_utf8(b.string_view()));
        return;
    }
    out->tag = 3;
    out->num = static_cast<double>(handle_for(runner, v));
}

// Type a completed reflective op into the reply; a thrown value becomes the
// error tag (5) with its message.
static void wide_from(Runner* runner, msy_reply* out, JS::ThrowCompletionOr<JS::Value> result)
{
    if (result.is_error()) {
        *out = {};
        fill_str16(runner, out, 5, error_message(result.release_error().value()));
        return;
    }
    wide_fill(runner, out, result.value());
}

// A UTF-16 argument as a JS value; a host-object handle (kind 2) resolves
// straight from the native table.
static JS::Value arg16_to_value(Runner* runner, msy_arg16 const& a)
{
    switch (a.kind) {
    case 0:
        return JS::PrimitiveString::create(runner->realm->vm(),
            Utf16View { reinterpret_cast<char16_t const*>(a.str16), a.str16_len });
    case 1:
        return JS::Value(a.num);
    case 2:
        return handle_value(runner, static_cast<int64_t>(a.num));
    case 3:
        return JS::Value(a.num != 0);
    case 5:
        // A durable Mersey callable as its stable callback id (ABI v8) —
        // the same cached NativeFunction the JSON path's {"__cb__":id} gets.
        return make_callback(runner, static_cast<uint32_t>(a.num));
    default:
        return JS::js_null();
    }
}

static Utf16String arg16_string(msy_arg16 const& a)
{
    return Utf16String::from_utf16(Utf16View { reinterpret_cast<char16_t const*>(a.str16), a.str16_len });
}

// ---- direct-DOM tier: hot methods straight to LibWeb C++ -------------------

// Shared tail: put a host-created object in the table and reply with its ref.
static bool reply_ref(Runner* runner, JS::Object& obj, msy_reply* out)
{
    *out = {};
    out->tag = 3; // ref
    out->num = static_cast<double>(handle_for(runner, JS::Value(&obj)));
    return true;
}

// new URL(str): parse and build the DOMURL directly in C++ — the pathname and
// search reads below then resolve from the same table entry.
static bool try_construct_url(Runner* runner, msy_arg16 const* args, size_t argc, msy_reply* out)
{
    if (argc < 1 || args[0].kind != 0)
        return false;
    auto url_or = Web::DOMURL::DOMURL::construct_impl(*runner->realm, arg16_string(args[0]));
    if (url_or.is_error())
        return false;
    return reply_ref(runner, *url_or.release_value(), out);
}

// url.pathname / url.search: direct C++.
static bool try_url_get(Runner* runner, int64_t target, HotMethod which, msy_reply* out)
{
    auto obj = handle_object(runner, target);
    if (!obj || !is<Web::DOMURL::DOMURL>(*obj))
        return false;
    auto& url = as<Web::DOMURL::DOMURL>(*obj);
    fill_str16(runner, out, 2, which == HotMethod::Pathname ? url.pathname() : url.search());
    return true;
}

// document.createElement(tag): build the element directly and hand back a ref.
static bool try_create_element(Runner* runner, int64_t target, msy_arg16 const* args, size_t argc, msy_reply* out)
{
    if (argc < 1 || args[0].kind != 0)
        return false;
    auto doc_obj = handle_object(runner, target);
    if (!doc_obj || !is<Web::DOM::Document>(*doc_obj))
        return false;
    // The free ElementFactory create_element (HTML namespace — the workload's
    // document is HTML) avoids Document::create_element's ElementCreationOptions
    // variant. For the benchmark's lowercase tags this matches createElement.
    auto el_or = Web::DOM::create_element(as<Web::DOM::Document>(*doc_obj),
        Utf16FlyString { arg16_string(args[0]) }, Web::Namespace::HTML);
    if (el_or.is_error())
        return false;
    return reply_ref(runner, *el_or.release_value(), out);
}

// node.appendChild(child): the tree insertion is a direct C++ call.
static bool try_append_child(Runner* runner, int64_t target, msy_arg16 const* args, size_t argc, msy_reply* out)
{
    if (argc < 1 || args[0].kind != 2)
        return false;
    auto parent = handle_object(runner, target);
    if (!parent || !is<Web::DOM::Node>(*parent))
        return false;
    auto child = handle_object(runner, static_cast<int64_t>(args[0].num));
    if (!child || !is<Web::DOM::Node>(*child))
        return false;
    (void)as<Web::DOM::Node>(*parent).append_child(GC::Ref<Web::DOM::Node> { as<Web::DOM::Node>(*child) });
    *out = {};
    out->tag = 0;
    return true;
}

// el.textContent = s: direct C++.
static bool try_set_text_content(Runner* runner, int64_t target, msy_arg16 const* value, msy_reply* out)
{
    if (!value || value->kind != 0)
        return false;
    auto obj = handle_object(runner, target);
    if (!obj || !is<Web::DOM::Node>(*obj))
        return false;
    (void)as<Web::DOM::Node>(*obj).set_text_content(arg16_string(*value));
    *out = {};
    out->tag = 0;
    return true;
}

// crypto.getRandomValues(buf): fill the buffer directly in C++.
static bool try_get_random_values(Runner* runner, int64_t target, msy_arg16 const* args, size_t argc, msy_reply* out)
{
    if (argc < 1 || args[0].kind != 2)
        return false;
    auto crypto_obj = handle_object(runner, target);
    if (!crypto_obj || !is<Web::Crypto::Crypto>(*crypto_obj))
        return false;
    auto buf_obj = handle_object(runner, static_cast<int64_t>(args[0].num));
    if (!buf_obj)
        return false;
    auto view = Web::WebIDL::ArrayBufferView::from_object(GC::Ref<JS::Object> { *buf_obj });
    (void)as<Web::Crypto::Crypto>(*crypto_obj).get_random_values(view);
    *out = {};
    out->tag = 0;
    return true;
}

// el.textContent (get): direct C++.
static bool try_text_content_get(Runner* runner, int64_t target, msy_reply* out)
{
    auto obj = handle_object(runner, target);
    if (!obj || !is<Web::DOM::Node>(*obj))
        return false;
    auto text = as<Web::DOM::Node>(*obj).text_content();
    if (!text.has_value()) {
        *out = {};
        out->tag = 0;
        return true;
    }
    fill_str16(runner, out, 2, *text);
    return true;
}

// el.classList / el.style: hand back the element's own sub-object as a ref
// (the table dedups object → handle, so the loop sees a stable handle).
static bool try_object_get(Runner* runner, int64_t target, HotMethod which, msy_reply* out)
{
    auto obj = handle_object(runner, target);
    if (!obj || !is<Web::DOM::Element>(*obj))
        return false;
    auto& el = as<Web::DOM::Element>(*obj);
    if (which == HotMethod::ClassList)
        return reply_ref(runner, el.class_list(), out);
    return reply_ref(runner, el.style_for_bindings(), out);
}

// nodes.length on a NodeList: direct C++.
static bool try_length_get(Runner* runner, int64_t target, msy_reply* out)
{
    auto obj = handle_object(runner, target);
    if (!obj || !is<Web::DOM::NodeList>(*obj))
        return false;
    *out = {};
    out->tag = 1;
    out->num = static_cast<double>(as<Web::DOM::NodeList>(*obj).length());
    return true;
}

// nodes[i] on a NodeList (the engine crosses indexed access as a digit-named
// property read): direct C++ item().
static bool try_index_get(Runner* runner, int64_t target, uint32_t index, msy_reply* out)
{
    auto obj = handle_object(runner, target);
    if (!obj || !is<Web::DOM::NodeList>(*obj))
        return false;
    auto const* node = as<Web::DOM::NodeList>(*obj).item(index);
    if (!node) {
        *out = {};
        out->tag = 0;
        return true;
    }
    return reply_ref(runner, const_cast<Web::DOM::Node&>(*node), out);
}

// el.className = s: the reflected class attribute, set directly.
static bool try_set_class_name(Runner* runner, int64_t target, msy_arg16 const* value, msy_reply* out)
{
    if (!value || value->kind != 0)
        return false;
    auto obj = handle_object(runner, target);
    if (!obj || !is<Web::DOM::Element>(*obj))
        return false;
    as<Web::DOM::Element>(*obj).set_attribute_value(Web::HTML::AttributeNames::class_, arg16_string(*value));
    *out = {};
    out->tag = 0;
    return true;
}

// tokens.contains(s) on a DOMTokenList: direct C++.
static bool try_contains(Runner* runner, int64_t target, msy_arg16 const* args, size_t argc, msy_reply* out)
{
    if (argc < 1 || args[0].kind != 0)
        return false;
    auto obj = handle_object(runner, target);
    if (!obj || !is<Web::DOM::DOMTokenList>(*obj))
        return false;
    auto token = arg16_string(args[0]);
    *out = {};
    out->tag = 4; // bool
    out->num = as<Web::DOM::DOMTokenList>(*obj).contains(token.utf16_view()) ? 1 : 0;
    return true;
}

// style.setProperty(p, v) / style.getPropertyValue(p): direct C++ into the
// inline-style declaration.
static bool try_style_property(Runner* runner, int64_t target, HotMethod which, msy_arg16 const* args, size_t argc, msy_reply* out)
{
    if (argc < 1 || args[0].kind != 0)
        return false;
    auto obj = handle_object(runner, target);
    if (!obj || !is<Web::CSS::CSSStyleDeclaration>(*obj))
        return false;
    auto& style = as<Web::CSS::CSSStyleDeclaration>(*obj);
    auto prop = arg16_string(args[0]);
    if (which == HotMethod::SetProperty) {
        if (argc < 2 || args[1].kind != 0)
            return false;
        auto value = arg16_string(args[1]);
        if (style.set_property(Utf16FlyString { prop }, value.utf16_view(), Utf16View {}).is_error())
            return false;
        *out = {};
        out->tag = 0;
        return true;
    }
    fill_str16(runner, out, 2, style.get_property_value(Utf16FlyString { prop }));
    return true;
}

// doc.querySelectorAll(sel): run the real selector match and hand the NodeList
// back as a ref, so length/index reads on it stay direct.
static bool try_query_selector_all(Runner* runner, int64_t target, msy_arg16 const* args, size_t argc, msy_reply* out)
{
    if (argc < 1 || args[0].kind != 0)
        return false;
    auto obj = handle_object(runner, target);
    if (!obj || !is<Web::DOM::ParentNode>(*obj))
        return false;
    auto sel = arg16_string(args[0]);
    auto list_or = as<Web::DOM::ParentNode>(*obj).query_selector_all(sel.utf16_view());
    if (list_or.is_error())
        return false;
    return reply_ref(runner, *list_or.release_value(), out);
}

// enc.encode(s): direct UTF-8 encode; the Uint8Array crosses as a ref.
static bool try_encode(Runner* runner, int64_t target, msy_arg16 const* args, size_t argc, msy_reply* out)
{
    if (argc < 1 || args[0].kind != 0)
        return false;
    auto obj = handle_object(runner, target);
    if (!obj || !is<Web::Encoding::TextEncoder>(*obj))
        return false;
    auto bytes = as<Web::Encoding::TextEncoder>(*obj).encode(arg16_string(args[0]));
    return reply_ref(runner, *bytes, out);
}

// dec.decode(bytes) with a typed-array/ArrayBuffer handle: direct decode.
static bool try_decode(Runner* runner, int64_t target, msy_arg16 const* args, size_t argc, msy_reply* out)
{
    if (argc < 1 || args[0].kind != 2)
        return false;
    auto obj = handle_object(runner, target);
    if (!obj || !is<Web::Encoding::TextDecoder>(*obj))
        return false;
    auto buf_obj = handle_object(runner, static_cast<int64_t>(args[0].num));
    if (!buf_obj || !(is<JS::TypedArrayBase>(*buf_obj) || is<JS::ArrayBuffer>(*buf_obj) || is<JS::DataView>(*buf_obj)))
        return false;
    auto source = Web::WebIDL::BufferSource::from_object(GC::Ref<JS::Object> { *buf_obj });
    auto text_or = as<Web::Encoding::TextDecoder>(*obj).decode(source, {});
    if (text_or.is_error())
        return false;
    fill_str16(runner, out, 2, text_or.release_value());
    return true;
}

// el.dispatchEvent(ev): direct dispatch — the listeners still run (dispatch
// invokes them synchronously); a Mersey listener is a NativeFunction, so the
// whole round trip is C++.
static bool try_dispatch_event(Runner* runner, int64_t target, msy_arg16 const* args, size_t argc, msy_reply* out)
{
    if (argc < 1 || args[0].kind != 2)
        return false;
    auto obj = handle_object(runner, target);
    if (!obj || !is<Web::DOM::EventTarget>(*obj))
        return false;
    auto ev_obj = handle_object(runner, static_cast<int64_t>(args[0].num));
    if (!ev_obj || !is<Web::DOM::Event>(*ev_obj))
        return false;
    bool not_cancelled = as<Web::DOM::EventTarget>(*obj).dispatch_event(as<Web::DOM::Event>(*ev_obj));
    *out = {};
    out->tag = 4; // bool
    out->num = not_cancelled ? 1 : 0;
    return true;
}

// new Event(type): build the event directly and hand back a ref.
static bool try_construct_event(Runner* runner, msy_arg16 const* args, size_t argc, msy_reply* out)
{
    if (argc < 1 || args[0].kind != 0)
        return false;
    auto ev = Web::DOM::Event::create(*runner->realm, Utf16FlyString { arg16_string(args[0]) });
    return reply_ref(runner, *ev, out);
}

// storage.getItem(k) / setItem(k, v) / removeItem(k): direct C++ Web Storage —
// no binding layer, UTF-16 in and out.
static bool try_storage_get_item(Runner* runner, int64_t target, msy_arg16 const* args, size_t argc, msy_reply* out)
{
    if (argc < 1 || args[0].kind != 0)
        return false;
    auto obj = handle_object(runner, target);
    if (!obj || !is<Web::HTML::Storage>(*obj))
        return false;
    auto key = arg16_string(args[0]);
    auto item = as<Web::HTML::Storage>(*obj).get_item(key.utf16_view());
    if (!item.has_value()) {
        *out = {};
        out->tag = 0; // null — the "no such key" reply
        return true;
    }
    fill_str16(runner, out, 2, *item);
    return true;
}

static bool try_storage_set_item(Runner* runner, int64_t target, msy_arg16 const* args, size_t argc, msy_reply* out)
{
    if (argc < 2 || args[0].kind != 0 || args[1].kind != 0)
        return false;
    auto obj = handle_object(runner, target);
    if (!obj || !is<Web::HTML::Storage>(*obj))
        return false;
    auto key = arg16_string(args[0]);
    auto value = arg16_string(args[1]);
    // A quota error falls back to the reflective path, which throws it properly.
    if (as<Web::HTML::Storage>(*obj).set_item(key.utf16_view(), value.utf16_view()).is_error())
        return false;
    *out = {};
    out->tag = 0;
    return true;
}

static bool try_storage_remove_item(Runner* runner, int64_t target, msy_arg16 const* args, size_t argc, msy_reply* out)
{
    if (argc < 1 || args[0].kind != 0)
        return false;
    auto obj = handle_object(runner, target);
    if (!obj || !is<Web::HTML::Storage>(*obj))
        return false;
    auto key = arg16_string(args[0]);
    as<Web::HTML::Storage>(*obj).remove_item(key.utf16_view());
    *out = {};
    out->tag = 0;
    return true;
}

// ---- wide-path hooks: hot dispatch, then native reflection -----------------

static HotMethod hot_of(Runner* runner, uint32_t name_id)
{
    if (name_id >= runner->hot.size())
        return HotMethod::None;
    return runner->hot[name_id];
}

static StringView interned_name(Runner* runner, uint32_t name_id)
{
    // The engine only passes ids this host handed out, so this is never out of
    // range; guard anyway (a UINT32_MAX id from a declined intern). The view is
    // stable: ByteString data is ref-counted, unmoved by Vector growth.
    if (name_id >= runner->names.size())
        return ""sv;
    return runner->names[name_id].view();
}

static void host_web_get_u16(void*, int64_t target, uint32_t name_id, msy_reply* out)
{
    auto* runner = s_runner;
    if (!runner || !runner->realm) {
        *out = {};
        return;
    }
    switch (hot_of(runner, name_id)) {
    case HotMethod::Pathname:
    case HotMethod::Search:
        if (try_url_get(runner, target, hot_of(runner, name_id), out))
            return;
        break;
    case HotMethod::TextContent:
        if (try_text_content_get(runner, target, out))
            return;
        break;
    case HotMethod::ClassList:
    case HotMethod::Style:
        if (try_object_get(runner, target, hot_of(runner, name_id), out))
            return;
        break;
    case HotMethod::Length:
        if (try_length_get(runner, target, out))
            return;
        break;
    case HotMethod::Index:
        if (try_index_get(runner, target, runner->hot_arg[name_id], out))
            return;
        break;
    default:
        break;
    }
    wide_from(runner, out, [&]() -> JS::ThrowCompletionOr<JS::Value> {
        auto& vm = runner->realm->vm();
        auto tv = handle_value(runner, target);
        auto obj = TRY(target_object(vm, tv));
        auto v = TRY(obj->get(JS::PropertyKey { Utf16String::from_utf8(interned_name(runner, name_id)) }));
        if (v.is_function())
            v = bind_receiver(runner, v, tv);
        return v;
    }());
}

static void host_web_set_u16(void*, int64_t target, uint32_t name_id, msy_arg16 const* value, msy_reply* out)
{
    auto* runner = s_runner;
    if (!runner || !runner->realm) {
        *out = {};
        return;
    }
    auto hot = hot_of(runner, name_id);
    if (hot == HotMethod::TextContent && try_set_text_content(runner, target, value, out))
        return;
    if (hot == HotMethod::ClassName && try_set_class_name(runner, target, value, out))
        return;
    wide_from(runner, out, [&]() -> JS::ThrowCompletionOr<JS::Value> {
        auto& vm = runner->realm->vm();
        auto tv = handle_value(runner, target);
        auto obj = TRY(target_object(vm, tv));
        auto v = value ? arg16_to_value(runner, *value) : JS::js_null();
        TRY(obj->set(JS::PropertyKey { Utf16String::from_utf8(interned_name(runner, name_id)) }, v,
            JS::Object::ShouldThrowExceptions::Yes));
        return JS::js_null();
    }());
}

static void host_web_call_u16(void*, int64_t target, uint32_t name_id, msy_arg16 const* args, size_t argc, msy_reply* out)
{
    auto* runner = s_runner;
    if (!runner || !runner->realm) {
        *out = {};
        return;
    }
    // Direct-DOM tier: a hot method goes straight to LibWeb C++.
    switch (hot_of(runner, name_id)) {
    case HotMethod::GetRandomValues:
        if (try_get_random_values(runner, target, args, argc, out))
            return;
        break;
    case HotMethod::CreateElement:
        if (try_create_element(runner, target, args, argc, out))
            return;
        break;
    case HotMethod::AppendChild:
        if (try_append_child(runner, target, args, argc, out))
            return;
        break;
    case HotMethod::DispatchEvent:
        if (try_dispatch_event(runner, target, args, argc, out))
            return;
        break;
    case HotMethod::Contains:
        if (try_contains(runner, target, args, argc, out))
            return;
        break;
    case HotMethod::SetProperty:
    case HotMethod::GetPropertyValue:
        if (try_style_property(runner, target, hot_of(runner, name_id), args, argc, out))
            return;
        break;
    case HotMethod::QuerySelectorAll:
        if (try_query_selector_all(runner, target, args, argc, out))
            return;
        break;
    case HotMethod::Encode:
        if (try_encode(runner, target, args, argc, out))
            return;
        break;
    case HotMethod::Decode:
        if (try_decode(runner, target, args, argc, out))
            return;
        break;
    case HotMethod::GetItem:
        if (try_storage_get_item(runner, target, args, argc, out))
            return;
        break;
    case HotMethod::SetItem:
        if (try_storage_set_item(runner, target, args, argc, out))
            return;
        break;
    case HotMethod::RemoveItem:
        if (try_storage_remove_item(runner, target, args, argc, out))
            return;
        break;
    default:
        break;
    }
    wide_from(runner, out, [&]() -> JS::ThrowCompletionOr<JS::Value> {
        auto& vm = runner->realm->vm();
        auto tv = handle_value(runner, target);
        auto name = interned_name(runner, name_id);
        // ABI v8: the interned EMPTY name means the handle is itself callable
        // (an imported `setTimeout(cb, ms)`, `fetch(url)`) — same routing as
        // the JSON path's `method == ""`, minus all the JSON.
        if (name.is_empty()) {
            GC::RootVector<JS::Value> vals;
            for (size_t i = 0; i < argc; ++i)
                vals.append(arg16_to_value(runner, args[i]));
            if (is<HTML::Window>(runner->realm->global_object()) && !vals.is_empty()) {
                if (target == runner->set_timeout_handle && vals[0].is_function())
                    return JS::Value(TRY(set_timeout_core(runner, vals.span())));
                if (target == runner->clear_timeout_handle && vals[0].is_number()) {
                    as<HTML::Window>(runner->realm->global_object()).clear_timeout(static_cast<i32>(vals[0].as_double()));
                    return JS::js_null();
                }
            }
            if (!tv.is_function())
                return vm.throw_completion<JS::TypeError>(Utf16String::from_utf8("value is not a function"sv));
            return JS::call(vm, tv, JS::js_undefined(), vals.span());
        }
        auto obj = TRY(target_object(vm, tv));
        auto fn = TRY(obj->get(JS::PropertyKey { Utf16String::from_utf8(name) }));
        if (!fn.is_function())
            return runner->realm->vm().throw_completion<JS::TypeError>(
                Utf16String::formatted("{} is not a function", name));
        GC::RootVector<JS::Value> vals;
        for (size_t i = 0; i < argc; ++i)
            vals.append(arg16_to_value(runner, args[i]));
        return JS::call(vm, fn, tv, vals.span());
    }());
}

static void host_web_new_u16(void*, uint32_t ctor_id, msy_arg16 const* args, size_t argc, msy_reply* out)
{
    auto* runner = s_runner;
    if (!runner || !runner->realm) {
        *out = {};
        return;
    }
    auto hot = hot_of(runner, ctor_id);
    if (hot == HotMethod::CtorURL && try_construct_url(runner, args, argc, out))
        return;
    if (hot == HotMethod::CtorEvent && try_construct_event(runner, args, argc, out))
        return;
    wide_from(runner, out, [&]() -> JS::ThrowCompletionOr<JS::Value> {
        auto& vm = runner->realm->vm();
        auto ctor_value = TRY(resolve_ctor(runner, interned_name(runner, ctor_id)));
        if (!ctor_value.is_function())
            return vm.throw_completion<JS::TypeError>(
                Utf16String::formatted("{} is not a constructor", interned_name(runner, ctor_id)));
        GC::RootVector<JS::Value> vals;
        for (size_t i = 0; i < argc; ++i)
            vals.append(arg16_to_value(runner, args[i]));
        return JS::Value(TRY(JS::construct(vm, ctor_value.as_function(), vals.span())).ptr());
    }());
}

// ---- typed-binding fast path (ABI v7, web_bind) ----------------------------
// The leanest tier: a JIT-compiled numeric web method (a canvas draw loop) crosses
// as a compile-time bind id plus raw doubles — no interned name, no msy_arg16, and
// the JIT calls this hook directly from compiled code. We switch on the id straight
// to the C++ CanvasRenderingContext2D method, so a hot fillRect loop never leaves C++.

static StringView bind_method_name(uint32_t id)
{
    switch (id) {
    case MSY_BIND_CANVAS2D_FILLRECT: return "fillRect"sv;
    case MSY_BIND_CANVAS2D_CLEARRECT: return "clearRect"sv;
    case MSY_BIND_CANVAS2D_STROKERECT: return "strokeRect"sv;
    case MSY_BIND_CANVAS2D_RECT: return "rect"sv;
    case MSY_BIND_CANVAS2D_MOVETO: return "moveTo"sv;
    case MSY_BIND_CANVAS2D_LINETO: return "lineTo"sv;
    case MSY_BIND_CANVAS2D_TRANSLATE: return "translate"sv;
    case MSY_BIND_CANVAS2D_SCALE: return "scale"sv;
    case MSY_BIND_CANVAS2D_ROTATE: return "rotate"sv;
    default: return {};
    }
}

// Resolve a handle to its CanvasRenderingContext2D, cached by handle.
static GC::Ptr<Web::HTML::CanvasRenderingContext2D> resolve_canvas(Runner* runner, int64_t target)
{
    if (target == runner->canvas_handle)
        return runner->canvas_ctx;
    runner->canvas_handle = target;
    runner->canvas_ctx = nullptr;
    auto obj = handle_object(runner, target);
    if (obj && is<Web::HTML::CanvasRenderingContext2D>(*obj))
        runner->canvas_ctx = as<Web::HTML::CanvasRenderingContext2D>(*obj);
    return runner->canvas_ctx;
}

static void host_web_bind(void*, int64_t target, uint32_t bind_id, double const* args, size_t argc, msy_reply* out)
{
    *out = {};
    auto* runner = s_runner;
    if (!runner || !runner->realm) {
        out->tag = 5;
        return;
    }
    auto ctx = resolve_canvas(runner, target);
    if (ctx) {
        auto a = [&](size_t i) { return static_cast<float>(i < argc ? args[i] : 0.0); };
        switch (bind_id) {
        case MSY_BIND_CANVAS2D_FILLRECT: ctx->fill_rect(a(0), a(1), a(2), a(3)); return;
        case MSY_BIND_CANVAS2D_CLEARRECT: ctx->clear_rect(a(0), a(1), a(2), a(3)); return;
        case MSY_BIND_CANVAS2D_STROKERECT: ctx->stroke_rect(a(0), a(1), a(2), a(3)); return;
        case MSY_BIND_CANVAS2D_RECT: ctx->rect(a(0), a(1), a(2), a(3)); return;
        case MSY_BIND_CANVAS2D_MOVETO: ctx->move_to(a(0), a(1)); return;
        case MSY_BIND_CANVAS2D_LINETO: ctx->line_to(a(0), a(1)); return;
        case MSY_BIND_CANVAS2D_TRANSLATE: ctx->translate(a(0), a(1)); return;
        case MSY_BIND_CANVAS2D_SCALE: ctx->scale(a(0), a(1)); return;
        case MSY_BIND_CANVAS2D_ROTATE: ctx->rotate(a(0)); return;
        default: break;
        }
    }
    // Receiver is not a canvas context (or an unknown id): reflective call under
    // the method's real name. Never hit by the canvas workload.
    auto name = bind_method_name(bind_id);
    if (name.is_empty())
        return;
    wide_from(runner, out, [&]() -> JS::ThrowCompletionOr<JS::Value> {
        auto& vm = runner->realm->vm();
        auto tv = handle_value(runner, target);
        auto obj = TRY(target_object(vm, tv));
        auto fn = TRY(obj->get(JS::PropertyKey { Utf16String::from_utf8(name) }));
        if (!fn.is_function())
            return vm.throw_completion<JS::TypeError>(Utf16String::formatted("{} is not a function", name));
        GC::RootVector<JS::Value> vals;
        for (size_t i = 0; i < argc; ++i)
            vals.append(JS::Value(args[i]));
        return JS::call(vm, fn, tv, vals.span());
    }());
}

// ---- context setup ---------------------------------------------------------

static msy_host_table host_table()
{
    msy_host_table table {};
    table.data = nullptr;
    table.print = host_print;
    table.caps = host_caps;
    table.web_global = host_web_global;
    table.web_get = host_web_get;
    table.web_set = host_web_set;
    table.web_call = host_web_call;
    table.web_new = host_web_new;
    table.web_iterate = host_web_iterate;
    table.web_instanceof = host_web_instanceof;
    table.web_release = host_web_release;
    table.time_ms = host_time_ms;
    // Interning feeds the wide-path hot dispatch; the mid-tier scalar hooks
    // (web_get_id, web_call_str, web_call_scalars, web_new_scalars) stay NULL —
    // the engine prefers the wide UTF-16 hooks below, and anything they cannot
    // express (closure/dict arguments) goes to the reflective JSON ops above.
    table.web_intern = host_web_intern;
    // Wide-string paths (UTF-16, no JSON): hot methods dispatch straight to
    // LibWeb C++; everything else is native LibJS reflection — property gets,
    // JS::call on the IDL bindings — with no JS source anywhere.
    table.web_get_u16 = host_web_get_u16;
    table.web_set_u16 = host_web_set_u16;
    table.web_call_u16 = host_web_call_u16;
    table.web_new_u16 = host_web_new_u16;
    // Typed-binding fast path: the JIT-compiled canvas loop calls this directly
    // with raw doubles; we dispatch straight to the C++ CanvasRenderingContext2D.
    table.web_bind = host_web_bind;
    // The rest (print_level, error, web_bytes_*, random_bytes, the legacy
    // fake-DOM hooks) is left NULL: the engine denies or falls back.
    return table;
}

// One engine context per thread (one page's script realm), created lazily on
// the first text/mersey script and refreshed onto the current realm — the
// Servo fork's ensure_runner, in C++.
static Runner* ensure_runner(JS::Realm& realm)
{
    if (s_runner) {
        if (s_runner->realm != &realm) {
            s_runner->realm = &realm;
            reset_handles(s_runner, realm);
        }
        return s_runner;
    }
    auto* runner = new Runner {};
    runner->realm = &realm;
    reset_handles(runner, realm);
    s_runner = runner;

    auto table = host_table();
    table.data = runner;
    runner->ctx = msy_context_new(&table);
    return runner;
}

void run_mersey_script(JS::Realm& realm, String const& source)
{
    // The engine and the header must agree about the boundary before we install
    // a table (the Chromium fork's AbiVersionMatches test, inline here).
    VERIFY(msy_abi_version() == MSY_ABI_VERSION);

    auto* runner = ensure_runner(realm);
    if (!runner->ctx)
        return;

    // Every LibJS operation the host hooks make — property gets, JS::call into
    // the IDL bindings, NativeFunction invocation — needs an execution context
    // on the VM stack. prepare_script() runs at HTML-parse time with no JS on
    // the stack, so push a temporary context for the whole run, including the
    // host calls that happen synchronously inside msy_context_run. Callbacks
    // enabled: a web call may invoke a Mersey closure that re-enters.
    Web::HTML::TemporaryExecutionContext execution_context {
        realm, Web::HTML::TemporaryExecutionContext::CallbacksEnabled::Yes
    };

    auto source_view = source.bytes_as_string_view();
    msy_context_run(runner->ctx, source_view.characters_without_null_termination(), source_view.length());
}

}
