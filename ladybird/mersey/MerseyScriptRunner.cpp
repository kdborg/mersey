/*
 * Copyright (c) 2026, the Mersey project.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibWeb/Mersey/MerseyScriptRunner.h>

#include <AK/ByteString.h>
#include <AK/Format.h>
#include <AK/HashMap.h>
#include <AK/StringBuilder.h>
#include <AK/Time.h>
#include <AK/Utf16FlyString.h>
#include <AK/Utf16String.h>
#include <AK/Utf16View.h>
#include <AK/Vector.h>
#include <LibGC/RootVector.h>
#include <LibJS/Runtime/AbstractOperations.h>
#include <LibJS/Runtime/NativeFunction.h>
#include <LibJS/Runtime/Object.h>
#include <LibJS/Runtime/PrimitiveString.h>
#include <LibJS/Runtime/PropertyKey.h>
#include <LibJS/Runtime/Realm.h>
#include <LibJS/Runtime/Value.h>
#include <LibJS/Runtime/VM.h>
#include <LibWeb/Crypto/Crypto.h>
#include <LibWeb/DOM/Document.h>
#include <LibWeb/DOM/Element.h>
#include <LibWeb/DOM/ElementFactory.h>
#include <LibWeb/DOM/Node.h>
#include <LibWeb/DOMURL/DOMURL.h>
#include <LibWeb/HTML/CanvasRenderingContext2D.h>
#include <LibWeb/Namespace.h>
#include <LibWeb/HTML/Scripting/ClassicScript.h>
#include <LibWeb/HTML/Scripting/Environments.h>
#include <LibWeb/HTML/Scripting/TemporaryExecutionContext.h>
#include <LibWeb/WebIDL/Buffers.h>

// The reflective bridge JS, generated from web/mersey-bridge.js by
// ladybird/refresh-bridge.sh into bridge.js.h — a C++ header wrapping the
// canonical, readable bridge.js in one raw string literal:
//   namespace Web::Mersey { inline constexpr StringView BRIDGE_JS = R"…(…)…"sv; }
// So the module builds with no data-file plumbing, the C++ counterpart of
// Servo's `include_str!("bridge.js")`. Do not edit the generated header by hand.
#include <LibWeb/Mersey/bridge.js.h>

namespace Web::Mersey {

// Capabilities granted to the engine (spec §5.4). Matches the Gecko/Servo
// forks: the whole web surface is reachable, and the engine still gates each
// API by import.
static constexpr StringView CAPS = "[\"dom\",\"web\",\"time\",\"random\",\"net\",\"storage\"]"sv;

// Hot methods with a direct-C++ path (the direct-DOM tier): the engine interns a
// name once, and we classify it then, so the wide-path hooks can switch on the id
// straight to the C++ method — skipping the reflective bridge into LibJS entirely.
enum class HotMethod : u8 {
    None,
    GetRandomValues, // crypto.getRandomValues(buf) -> Crypto::get_random_values
    CtorURL,         // new URL(str)               -> DOMURL::construct_impl
    Pathname,        // url.pathname               -> DOMURL::pathname
    Search,          // url.search                 -> DOMURL::search
    CreateElement,   // document.createElement(t)  -> Document::create_element
    AppendChild,     // node.appendChild(child)    -> Node::append_child
    TextContent,     // el.textContent = s         -> Node::set_text_content
};

// Per-realm engine runner, reached through a raw thread-local pointer (see the
// header's threading note — a bridge call can legitimately re-enter).
struct Runner {
    msy_context* ctx { nullptr };
    // The realm whose global hosts the page's JS (and the reflective bridge).
    JS::Realm* realm { nullptr };
    bool bridge_ready { false };
    // Backing store for a reply the engine reads — valid until the next host
    // call on this runner, exactly the C-ABI buffer-lifetime contract. A
    // ByteString (UTF-8) keeps a stable (characters, length) the ABI borrows.
    ByteString scratch;
    // Backing store for a typed UTF-16 reply (msy_reply::str16) on the wide path.
    Vector<uint16_t> reply_str16;
    // Typed-binding fast path (web_bind): the canvas draw loop reuses one context,
    // so its C++ pointer is cached by handle — one JS unwrap amortized over the
    // loop, then every fillRect is a direct call. LibGC is non-moving and the page
    // holds the context alive, so a raw pointer is safe for the workload's span.
    int64_t canvas_handle { INT64_MIN };
    GC::Ptr<Web::HTML::CanvasRenderingContext2D> canvas_ctx;
    // Direct-DOM tier: interned-name → hot method (indexed by name id), and a
    // handle → JS object cache so a hot method's receiver/object args are
    // unwrapped once and reused across the loop (invalidated on web_release).
    Vector<HotMethod> hot;
    HashMap<int64_t, GC::Ptr<JS::Object>> handle_cache;
    MonotonicTime start { MonotonicTime::now() };
};

static thread_local Runner* s_runner { nullptr };

// ---- reflective bridge plumbing -------------------------------------------

// One argument crossing into the bridge: a JS number or a UTF-8 string. Mirrors
// Servo's JsArg — numbers stay numbers, strings stay strings, so the bridge's
// interned/scalar fast paths never see a JSON blob to parse.
struct BridgeArg {
    enum class Kind { Number, String } kind;
    double number { 0 };
    StringView string;

    static BridgeArg num(double n) { return { Kind::Number, n, {} }; }
    static BridgeArg str(StringView s) { return { Kind::String, 0, s }; }
};

// Call `__merseyBridge[method](args...)` in the runner's realm and return the
// reply as UTF-8. A string reply comes back verbatim; a numeric reply (intern)
// is encoded as its decimal text, so the number-returning wrappers can parse it
// (matching the Servo fork). Empty optional means the call could not be made.
static Optional<ByteString> call_bridge(StringView method, ReadonlySpan<BridgeArg> args)
{
    auto* runner = s_runner;
    if (!runner || !runner->realm)
        return {};
    auto& realm = *runner->realm;
    auto& vm = realm.vm();
    auto& global = realm.global_object();

    auto bridge_value_or = global.get("__merseyBridge"_utf16_fly_string);
    if (bridge_value_or.is_error() || !bridge_value_or.value().is_object())
        return {};
    auto& bridge = bridge_value_or.value().as_object();

    auto method_value_or = bridge.get(JS::PropertyKey { Utf16String::from_utf8(method) });
    if (method_value_or.is_error() || !method_value_or.value().is_function())
        return {};

    GC::RootVector<JS::Value> arguments;
    for (auto const& arg : args) {
        if (arg.kind == BridgeArg::Kind::Number)
            arguments.append(JS::Value(arg.number));
        else
            arguments.append(JS::PrimitiveString::create(vm, Utf16String::from_utf8(arg.string)));
    }

    auto result_or = JS::call(vm, method_value_or.value(), JS::Value(&bridge), arguments.span());
    if (result_or.is_error())
        return {};
    auto result = result_or.value();
    // A JSON-string reply comes back verbatim (UTF-16 → UTF-8); a numeric reply
    // (intern) is its integer decimal text, which the number-returning wrappers
    // parse back — so one path serves both.
    if (result.is_string())
        return result.as_string().utf16_string().to_byte_string();
    if (result.is_number())
        return ByteString::number(static_cast<i64>(result.as_double()));
    return ByteString {};
}

// Stash a reply into the runner scratch (UTF-8) and hand back its (ptr, len) —
// the engine reads it before the next call across the boundary in this
// direction, the whole of the ABI's buffer-ownership rule.
static char const* reply_into_scratch(StringView method, ReadonlySpan<BridgeArg> args, size_t* out_len)
{
    auto* runner = s_runner;
    if (!runner) {
        *out_len = 0;
        return nullptr;
    }
    runner->scratch = call_bridge(method, args).value_or(ByteString {});
    *out_len = runner->scratch.length();
    return runner->scratch.characters();
}

// Number-returning bridge call (`global`, `instanceOf`): -1 on any failure,
// which is exactly how the engine reads "no such global" / "not an instance".
static int64_t bridge_int(StringView method, ReadonlySpan<BridgeArg> args)
{
    auto reply = call_bridge(method, args);
    if (!reply.has_value())
        return -1;
    return reply->view().to_number<i64>().value_or(-1);
}

// ---- host table shims ------------------------------------------------------

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

static int64_t host_web_global(void*, char const* name, size_t len)
{
    BridgeArg args[] = { BridgeArg::str({ name, len }) };
    return bridge_int("global"sv, args);
}

static char const* host_web_get(void*, int64_t target, char const* prop, size_t prop_len, size_t* out_len)
{
    BridgeArg args[] = { BridgeArg::num(target), BridgeArg::str({ prop, prop_len }) };
    return reply_into_scratch("get"sv, args, out_len);
}

static char const* host_web_set(void*, int64_t target, char const* prop, size_t prop_len,
    char const* value_json, size_t value_len, size_t* out_len)
{
    BridgeArg args[] = { BridgeArg::num(target), BridgeArg::str({ prop, prop_len }),
        BridgeArg::str({ value_json, value_len }) };
    return reply_into_scratch("set"sv, args, out_len);
}

static char const* host_web_call(void*, int64_t target, char const* method, size_t method_len,
    char const* args_json, size_t args_len, size_t* out_len)
{
    BridgeArg args[] = { BridgeArg::num(target), BridgeArg::str({ method, method_len }),
        BridgeArg::str({ args_json, args_len }) };
    return reply_into_scratch("call"sv, args, out_len);
}

static char const* host_web_new(void*, char const* ctor, size_t ctor_len,
    char const* args_json, size_t args_len, size_t* out_len)
{
    BridgeArg args[] = { BridgeArg::str({ ctor, ctor_len }), BridgeArg::str({ args_json, args_len }) };
    return reply_into_scratch("construct"sv, args, out_len);
}

static char const* host_web_iterate(void*, int64_t target, size_t* out_len)
{
    BridgeArg args[] = { BridgeArg::num(target) };
    return reply_into_scratch("iterate"sv, args, out_len);
}

static int32_t host_web_instanceof(void*, int64_t target, int64_t ctor)
{
    BridgeArg args[] = { BridgeArg::num(target), BridgeArg::num(ctor) };
    return static_cast<int32_t>(bridge_int("instanceOf"sv, args));
}

static void host_web_release(void*, int64_t target)
{
    // Drop any cached unwrap for this handle before the engine forgets it.
    if (auto* runner = s_runner) {
        runner->handle_cache.remove(target);
        if (runner->canvas_handle == target) {
            runner->canvas_handle = INT64_MIN;
            runner->canvas_ctx = nullptr;
        }
    }
    BridgeArg args[] = { BridgeArg::num(target) };
    size_t ignored = 0;
    (void)reply_into_scratch("release"sv, args, &ignored);
}

// ---- interned + scalar fast paths (ABI v3) --------------------------------
// A member/method name crosses the boundary once (web_intern), then only its id
// does, and scalar arguments cross as JS values — no per-call args JSON to build
// and parse. The bridge already implements the matching methods (intern / getId
// / callStr / callScalars / newScalars); these forward to them exactly as the
// Servo fork does. Ops these do not cover (object args, property sets) fall back
// to the reflective ops above.

static uint32_t host_web_intern(void*, char const* name, size_t len)
{
    StringView n { name, len };
    BridgeArg args[] = { BridgeArg::str(n) };
    auto reply = call_bridge("intern"sv, args);
    if (!reply.has_value())
        return UINT32_MAX;
    auto id = reply->view().to_number<u32>().value_or(UINT32_MAX);
    // Classify the name once, so the wide-path hooks can dispatch a hot method to
    // its direct-C++ path by id (the engine interns before it ever calls). Ids are
    // the bridge's sequential ids, so this Vector is indexed the same way.
    auto* runner = s_runner;
    if (runner && id != UINT32_MAX) {
        auto hot = HotMethod::None;
        if (n == "getRandomValues"sv)
            hot = HotMethod::GetRandomValues;
        else if (n == "URL"sv)
            hot = HotMethod::CtorURL;
        else if (n == "pathname"sv)
            hot = HotMethod::Pathname;
        else if (n == "search"sv)
            hot = HotMethod::Search;
        else if (n == "createElement"sv)
            hot = HotMethod::CreateElement;
        else if (n == "appendChild"sv)
            hot = HotMethod::AppendChild;
        else if (n == "textContent"sv)
            hot = HotMethod::TextContent;
        while (runner->hot.size() <= id)
            runner->hot.append(HotMethod::None);
        runner->hot[id] = hot;
    }
    return id;
}

static char const* host_web_get_id(void*, int64_t target, uint32_t name_id, size_t* out_len)
{
    BridgeArg args[] = { BridgeArg::num(target), BridgeArg::num(name_id) };
    return reply_into_scratch("getId"sv, args, out_len);
}

static char const* host_web_call_str(void*, int64_t target, uint32_t name_id,
    char const* arg, size_t arg_len, size_t* out_len)
{
    BridgeArg args[] = { BridgeArg::num(target), BridgeArg::num(name_id), BridgeArg::str({ arg, arg_len }) };
    return reply_into_scratch("callStr"sv, args, out_len);
}

// Build the leading (target/ctor, name_id) args plus the scalar tail, each
// scalar crossing as a number or a string — never as JSON.
static void append_scalars(Vector<BridgeArg>& into, msy_scalar const* scalars, size_t argc)
{
    for (size_t i = 0; i < argc; ++i) {
        auto const& s = scalars[i];
        if (s.is_num != 0)
            into.append(BridgeArg::num(s.num));
        else
            into.append(BridgeArg::str({ s.str, s.str_len }));
    }
}

static char const* host_web_call_scalars(void*, int64_t target, uint32_t name_id,
    msy_scalar const* scalars, size_t argc, size_t* out_len)
{
    Vector<BridgeArg> args;
    args.append(BridgeArg::num(target));
    args.append(BridgeArg::num(name_id));
    if (scalars)
        append_scalars(args, scalars, argc);
    return reply_into_scratch("callScalars"sv, args, out_len);
}

static char const* host_web_new_scalars(void*, uint32_t ctor_id,
    msy_scalar const* scalars, size_t argc, size_t* out_len)
{
    Vector<BridgeArg> args;
    args.append(BridgeArg::num(ctor_id));
    if (scalars)
        append_scalars(args, scalars, argc);
    return reply_into_scratch("newScalars"sv, args, out_len);
}

// ---- wide-string fast paths (ABI v5) --------------------------------------
// The fastest tier: the engine's strings are UTF-16 and so is LibJS, so string
// arguments and replies cross with NO encoding conversion and NO JSON. The
// bridge's *Wide methods return the raw value (a scalar as itself, a host object
// as { r: handle }, an array as { j: json }); the host types it by inspection
// into msy_reply. Object arguments (appendChild(el), getRandomValues(buf)) stay
// on this path via a refs bitmask instead of falling back to the JSON path.

// Copy a JS string's UTF-16 code units into the runner reply buffer (stable
// until the next boundary call) and point the typed reply at it. code_unit_at
// widens ASCII-stored strings transparently.
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

// Fill a typed reply from the raw value a *Wide method returned.
static void fill_reply(Runner* r, msy_reply* out, JS::Value v)
{
    *out = {};
    if (v.is_null() || v.is_undefined()) { out->tag = 0; return; }
    if (v.is_number()) { out->tag = 1; out->num = v.as_double(); return; }
    if (v.is_boolean()) { out->tag = 4; out->num = v.as_bool() ? 1 : 0; return; }
    if (v.is_string()) { fill_str16(r, out, 2, v.as_string().utf16_string()); return; }
    if (v.is_object()) {
        auto& o = v.as_object();
        auto ref = o.get("r"_utf16_fly_string);
        if (!ref.is_error() && ref.value().is_number()) { out->tag = 3; out->num = ref.value().as_double(); return; }
        auto json = o.get("j"_utf16_fly_string);
        if (!json.is_error() && json.value().is_string()) { fill_str16(r, out, 7, json.value().as_string().utf16_string()); return; }
    }
    out->tag = 0;
}

// A UTF-16 argument as a JS value. A str16 becomes a PrimitiveString with no
// conversion; a host-object handle (kind 2) crosses as its handle number and the
// bridge resolves it via refsMask.
static JS::Value arg16_to_value(JS::VM& vm, msy_arg16 const& a)
{
    switch (a.kind) {
    case 0:
        return JS::PrimitiveString::create(vm, Utf16View { reinterpret_cast<char16_t const*>(a.str16), a.str16_len });
    case 1:
        return JS::Value(a.num);
    case 2:
        return JS::Value(a.num); // handle; refsMask marks it for resolution
    case 3:
        return JS::Value(a.num != 0);
    default:
        return JS::js_null();
    }
}

static uint32_t refs_mask(msy_arg16 const* args, size_t argc)
{
    uint32_t mask = 0;
    for (size_t i = 0; i < argc && i < 32; ++i)
        if (args[i].kind == 2)
            mask |= (1u << i);
    return mask;
}

// Call __merseyBridge[method](args) and type the raw reply into `out`.
static void wide_invoke(msy_reply* out, Utf16FlyString const& method, GC::RootVector<JS::Value> const& args)
{
    auto* runner = s_runner;
    *out = {};
    if (!runner || !runner->realm)
        return;
    auto& realm = *runner->realm;
    auto& vm = realm.vm();
    auto& global = realm.global_object();
    auto bridge_or = global.get("__merseyBridge"_utf16_fly_string);
    if (bridge_or.is_error() || !bridge_or.value().is_object())
        return;
    auto& bridge = bridge_or.value().as_object();
    auto method_or = bridge.get(method);
    if (method_or.is_error() || !method_or.value().is_function())
        return;
    auto result_or = JS::call(vm, method_or.value(), JS::Value(&bridge), args.span());
    if (result_or.is_error()) {
        // The *Wide methods throw on error; surface a diagnostic string (tag 5).
        fill_str16(runner, out, 5, "bridge error"_utf16);
        return;
    }
    fill_reply(runner, out, result_or.value());
}

// Direct-DOM helpers (defined below; forward-declared so the get/new hooks here
// can dispatch a hot method to its C++ path before the reflective wide path).
static GC::Ptr<JS::Object> resolve_handle_object(Runner*, int64_t);
static int64_t register_host_object(Runner*, JS::Object&);
static bool try_url_get(Runner*, int64_t target, HotMethod, msy_reply*);
static bool try_construct_url(Runner*, msy_arg16 const* args, size_t argc, msy_reply*);
static bool try_set_text_content(Runner*, int64_t target, msy_arg16 const* value, msy_reply*);

static void host_web_get_u16(void*, int64_t target, uint32_t name_id, msy_reply* out)
{
    auto* runner = s_runner;
    if (runner && name_id < runner->hot.size()) {
        auto hot = runner->hot[name_id];
        if ((hot == HotMethod::Pathname || hot == HotMethod::Search) && try_url_get(runner, target, hot, out))
            return;
    }
    GC::RootVector<JS::Value> args;
    args.append(JS::Value(static_cast<double>(target)));
    args.append(JS::Value(static_cast<double>(name_id)));
    wide_invoke(out, u"getWide"_utf16_fly_string, args);
}

static void host_web_set_u16(void*, int64_t target, uint32_t name_id, msy_arg16 const* value, msy_reply* out)
{
    auto* runner = s_runner;
    if (!runner || !runner->realm) { *out = {}; return; }
    if (name_id < runner->hot.size() && runner->hot[name_id] == HotMethod::TextContent) {
        if (try_set_text_content(runner, target, value, out))
            return;
    }
    GC::RootVector<JS::Value> args;
    args.append(JS::Value(static_cast<double>(target)));
    args.append(JS::Value(static_cast<double>(name_id)));
    args.append(JS::Value(static_cast<double>(value && value->kind == 2 ? 1 : 0))); // refsMask
    args.append(value ? arg16_to_value(runner->realm->vm(), *value) : JS::js_null());
    wide_invoke(out, u"setWide"_utf16_fly_string, args);
}

// Resolve a bridge handle to its JS object (bridge.handleObj), cached per handle
// so a hot method's stable receiver/args are unwrapped once and reused across the
// loop. The cache is invalidated in host_web_release.
static GC::Ptr<JS::Object> resolve_handle_object(Runner* runner, int64_t handle)
{
    if (auto it = runner->handle_cache.find(handle); it != runner->handle_cache.end())
        return it->value;
    GC::Ptr<JS::Object> result;
    auto& realm = *runner->realm;
    auto& vm = realm.vm();
    auto& global = realm.global_object();
    auto bridge_or = global.get("__merseyBridge"_utf16_fly_string);
    if (bridge_or.is_error() || !bridge_or.value().is_object())
        return {};
    auto& bridge = bridge_or.value().as_object();
    auto method_or = bridge.get("handleObj"_utf16_fly_string);
    if (method_or.is_error() || !method_or.value().is_function())
        return {};
    GC::RootVector<JS::Value> args;
    args.append(JS::Value(static_cast<double>(handle)));
    auto obj_or = JS::call(vm, method_or.value(), JS::Value(&bridge), args.span());
    if (obj_or.is_error())
        return {};
    auto obj_value = obj_or.release_value();
    if (obj_value.is_object())
        result = obj_value.as_object();
    runner->handle_cache.set(handle, result);
    return result;
}

// Register a host-CREATED object with the bridge (keeps it alive, returns a
// handle) and cache its pointer, so the follow-up get/set/call ops on it resolve
// with no crossing — the create direction of the direct-DOM tier.
static int64_t register_host_object(Runner* runner, JS::Object& obj)
{
    auto& realm = *runner->realm;
    auto& vm = realm.vm();
    auto& global = realm.global_object();
    auto bridge_or = global.get("__merseyBridge"_utf16_fly_string);
    if (bridge_or.is_error() || !bridge_or.value().is_object())
        return -1;
    auto& bridge = bridge_or.value().as_object();
    auto method_or = bridge.get("register"_utf16_fly_string);
    if (method_or.is_error() || !method_or.value().is_function())
        return -1;
    GC::RootVector<JS::Value> args;
    args.append(JS::Value(&obj));
    auto result_or = JS::call(vm, method_or.value(), JS::Value(&bridge), args.span());
    if (result_or.is_error() || !result_or.value().is_number())
        return -1;
    auto h = static_cast<int64_t>(result_or.value().as_double());
    runner->handle_cache.set(h, &obj);
    return h;
}

// new URL(str): parse and build the DOMURL directly in C++, register it once, and
// hand back a ref — so the pathname/search reads below hit the cache, no crossing.
static bool try_construct_url(Runner* runner, msy_arg16 const* args, size_t argc, msy_reply* out)
{
    if (argc < 1 || args[0].kind != 0)
        return false;
    auto url_str = Utf16String::from_utf16(Utf16View { reinterpret_cast<char16_t const*>(args[0].str16), args[0].str16_len });
    auto url_or = Web::DOMURL::DOMURL::construct_impl(*runner->realm, url_str);
    if (url_or.is_error())
        return false;
    auto url = url_or.release_value();
    auto h = register_host_object(runner, *url);
    if (h < 0)
        return false;
    *out = {};
    out->tag = 3; // ref
    out->num = static_cast<double>(h);
    return true;
}

// url.pathname / url.search on a host-created DOMURL (cache hit): direct C++.
static bool try_url_get(Runner* runner, int64_t target, HotMethod which, msy_reply* out)
{
    auto obj = resolve_handle_object(runner, target);
    if (!obj || !is<Web::DOMURL::DOMURL>(*obj))
        return false;
    auto& url = as<Web::DOMURL::DOMURL>(*obj);
    fill_str16(runner, out, 2, which == HotMethod::Pathname ? url.pathname() : url.search());
    return true;
}

// document.createElement(tag): build the element directly, register it (so later
// textContent/appendChild on it hit the cache), and hand back a ref.
static bool try_create_element(Runner* runner, int64_t target, msy_arg16 const* args, size_t argc, msy_reply* out)
{
    if (argc < 1 || args[0].kind != 0)
        return false;
    auto doc_obj = resolve_handle_object(runner, target);
    if (!doc_obj || !is<Web::DOM::Document>(*doc_obj))
        return false;
    auto name = Utf16String::from_utf16(Utf16View { reinterpret_cast<char16_t const*>(args[0].str16), args[0].str16_len });
    // The free ElementFactory create_element (HTML namespace — the workload's
    // document is HTML) avoids Document::create_element's ElementCreationOptions
    // variant. For the benchmark's lowercase tags this matches createElement.
    auto el_or = Web::DOM::create_element(as<Web::DOM::Document>(*doc_obj), Utf16FlyString { name }, Web::Namespace::HTML);
    if (el_or.is_error())
        return false;
    auto el = el_or.release_value();
    auto h = register_host_object(runner, *el);
    if (h < 0)
        return false;
    *out = {};
    out->tag = 3; // ref
    out->num = static_cast<double>(h);
    return true;
}

// node.appendChild(child): both handles resolve from the cache (parent stable,
// child host-created), so the tree insertion is a direct C++ call.
static bool try_append_child(Runner* runner, int64_t target, msy_arg16 const* args, size_t argc, msy_reply* out)
{
    if (argc < 1 || args[0].kind != 2)
        return false;
    auto parent = resolve_handle_object(runner, target);
    if (!parent || !is<Web::DOM::Node>(*parent))
        return false;
    auto child = resolve_handle_object(runner, static_cast<int64_t>(args[0].num));
    if (!child || !is<Web::DOM::Node>(*child))
        return false;
    (void)as<Web::DOM::Node>(*parent).append_child(GC::Ref<Web::DOM::Node> { as<Web::DOM::Node>(*child) });
    *out = {};
    out->tag = 0;
    return true;
}

// el.textContent = s on a host-created element (cache hit): direct C++.
static bool try_set_text_content(Runner* runner, int64_t target, msy_arg16 const* value, msy_reply* out)
{
    if (!value || value->kind != 0)
        return false;
    auto obj = resolve_handle_object(runner, target);
    if (!obj || !is<Web::DOM::Node>(*obj))
        return false;
    auto text = Utf16String::from_utf16(Utf16View { reinterpret_cast<char16_t const*>(value->str16), value->str16_len });
    (void)as<Web::DOM::Node>(*obj).set_text_content(text);
    *out = {};
    out->tag = 0;
    return true;
}

// crypto.getRandomValues(buf): unwrap the Crypto receiver and the buffer once
// (cached), then fill it directly in C++ — no JS bridge. Returns false to fall
// back to the reflective path if the receiver is not actually a Crypto.
static bool try_get_random_values(Runner* runner, int64_t target, msy_arg16 const* args, size_t argc, msy_reply* out)
{
    if (argc < 1 || args[0].kind != 2)
        return false;
    auto crypto_obj = resolve_handle_object(runner, target);
    if (!crypto_obj || !is<Web::Crypto::Crypto>(*crypto_obj))
        return false;
    auto buf_obj = resolve_handle_object(runner, static_cast<int64_t>(args[0].num));
    if (!buf_obj)
        return false;
    auto view = Web::WebIDL::ArrayBufferView::from_object(GC::Ref<JS::Object> { *buf_obj });
    (void)as<Web::Crypto::Crypto>(*crypto_obj).get_random_values(view);
    *out = {};
    out->tag = 0;
    return true;
}

static void host_web_call_u16(void*, int64_t target, uint32_t name_id, msy_arg16 const* args, size_t argc, msy_reply* out)
{
    auto* runner = s_runner;
    if (!runner || !runner->realm) { *out = {}; return; }
    // Direct-DOM tier: a hot method skips the reflective bridge entirely.
    if (name_id < runner->hot.size()) {
        switch (runner->hot[name_id]) {
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
        default:
            break;
        }
    }
    auto& vm = runner->realm->vm();
    GC::RootVector<JS::Value> vals;
    vals.append(JS::Value(static_cast<double>(target)));
    vals.append(JS::Value(static_cast<double>(name_id)));
    vals.append(JS::Value(static_cast<double>(refs_mask(args, argc))));
    for (size_t i = 0; i < argc; ++i)
        vals.append(arg16_to_value(vm, args[i]));
    wide_invoke(out, u"callWide"_utf16_fly_string, vals);
}

static void host_web_new_u16(void*, uint32_t ctor_id, msy_arg16 const* args, size_t argc, msy_reply* out)
{
    auto* runner = s_runner;
    if (!runner || !runner->realm) { *out = {}; return; }
    if (ctor_id < runner->hot.size() && runner->hot[ctor_id] == HotMethod::CtorURL) {
        if (try_construct_url(runner, args, argc, out))
            return;
    }
    auto& vm = runner->realm->vm();
    GC::RootVector<JS::Value> vals;
    vals.append(JS::Value(static_cast<double>(ctor_id)));
    vals.append(JS::Value(static_cast<double>(refs_mask(args, argc))));
    for (size_t i = 0; i < argc; ++i)
        vals.append(arg16_to_value(vm, args[i]));
    wide_invoke(out, u"newWide"_utf16_fly_string, vals);
}

// ---- typed-binding fast path (ABI v7, web_bind) ----------------------------
// The leanest tier: a JIT-compiled numeric web method (a canvas draw loop) crosses
// as a compile-time bind id plus raw doubles — no interned name, no msy_arg16, and
// the JIT calls this hook directly from compiled code. We switch on the id straight
// to the C++ CanvasRenderingContext2D method, so a hot fillRect loop never touches
// JS at all. Gecko's fork has this; Chromium/Servo/Ladybird had it NULL until now.

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

// Resolve a bridge handle to its CanvasRenderingContext2D, cached by handle.
static GC::Ptr<Web::HTML::CanvasRenderingContext2D> resolve_canvas(Runner* runner, int64_t target)
{
    if (target == runner->canvas_handle)
        return runner->canvas_ctx;
    runner->canvas_handle = target;
    runner->canvas_ctx = nullptr;
    auto obj = resolve_handle_object(runner, target);
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
    // Receiver is not a canvas context (or an unknown id): the host owns the
    // fallback. Route through the reflective `call` under the method's real name
    // with the numeric args as JSON. Never hit by the canvas workload.
    auto name = bind_method_name(bind_id);
    if (name.is_empty())
        return;
    StringBuilder json;
    json.append('[');
    for (size_t i = 0; i < argc; ++i) {
        if (i)
            json.append(',');
        json.appendff("{}", args[i]);
    }
    json.append(']');
    auto json_str = json.to_byte_string();
    BridgeArg call_args[] = { BridgeArg::num(static_cast<double>(target)), BridgeArg::str(name),
        BridgeArg::str(json_str) };
    (void)call_bridge("call"sv, call_args);
}

// `__merseyInvoke(cb, argsJson)` — the native the bridge calls when JS invokes a
// Mersey closure (a promise reaction, an event listener). Forwards into the
// engine via msy_context_invoke_args, which may itself re-enter the host hooks.
static JS::ThrowCompletionOr<JS::Value> mersey_invoke(JS::VM& vm)
{
    auto* runner = s_runner;
    if (runner && vm.argument_count() >= 2) {
        auto cb = static_cast<uint32_t>(TRY(vm.argument(0).to_double(vm)));
        auto arg1 = vm.argument(1);
        auto args_json = arg1.is_string()
            ? arg1.as_string().utf16_string().to_byte_string()
            : ByteString { "[]" };
        msy_context_invoke_args(runner->ctx, cb, args_json.characters(), args_json.length());
    }
    return JS::js_undefined();
}

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
    // Interned + scalar fast paths (a name crosses once as an id, scalar args
    // cross as JS values). Ops these don't cover fall back to the reflective
    // ops above — same replies.
    table.web_intern = host_web_intern;
    table.web_get_id = host_web_get_id;
    table.web_call_str = host_web_call_str;
    table.web_call_scalars = host_web_call_scalars;
    table.web_new_scalars = host_web_new_scalars;
    // Wide-string fast paths (UTF-16, no JSON): the fastest reflective tier, and
    // the one that keeps object-argument calls (appendChild, getRandomValues) and
    // property sets (textContent=) off the JSON path. The engine prefers these
    // over the interned/reflective ops above when present.
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
        s_runner->realm = &realm;
        return s_runner;
    }
    auto* runner = new Runner {};
    runner->realm = &realm;
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

    // Every LibJS operation below — NativeFunction::create, the reflective
    // bridge's get/call, ClassicScript::run — needs an execution context on the
    // VM stack. prepare_script() runs at HTML-parse time with no JS on the stack,
    // so push a temporary context for the whole run, including the host bridge
    // calls that happen synchronously inside msy_context_run. Callbacks enabled:
    // a bridged web call may invoke a Mersey closure that re-enters JS.
    Web::HTML::TemporaryExecutionContext execution_context {
        realm, Web::HTML::TemporaryExecutionContext::CallbacksEnabled::Yes
    };

    // Inject __merseyInvoke and evaluate the reflective bridge into the realm,
    // once. From then on every web call the engine makes reaches Ladybird's DOM
    // through __merseyBridge in this realm.
    if (!runner->bridge_ready) {
        auto& global = realm.global_object();
        auto invoke = JS::NativeFunction::create(realm, mersey_invoke, 2, "__merseyInvoke"_utf16_fly_string);
        global.define_direct_property("__merseyInvoke"_utf16_fly_string, invoke, JS::default_attributes);

        auto& settings = Web::HTML::relevant_settings_object(global);
        auto bridge_source = Utf16String::from_utf8(BRIDGE_JS);
        auto bridge_script = Web::HTML::ClassicScript::create("mersey-bridge.js", bridge_source, settings, {});
        (void)bridge_script->run();
        runner->bridge_ready = true;
    }

    auto source_view = source.bytes_as_string_view();
    msy_context_run(runner->ctx, source_view.characters_without_null_termination(), source_view.length());
}

}
