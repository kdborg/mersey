// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Kirk D. Brown

//! MVP tree-walking interpreter. Executes a bound module directly from the
//! AST. This is the Phase-"MVP" execution engine; the typed-bytecode VM and
//! JIT (ROADMAP Phases 2/4) replace it without changing observable behavior
//! — the runtime conformance suite is the contract.
//!
//! Semantics honored from the spec: UTF-32 strings with O(1) code-point
//! indexing (§3.4), C-style numeric promotion with defined wrapping (§3.3,
//! §3.6: division by zero and `INT_MIN / -1` throw `RangeError`, shift
//! counts masked), checked vs `wrapping` casts, sealed class shapes (§4.1:
//! assigning an undeclared field throws), class-chain method dispatch with
//! `super`, and module-level declaration hoisting (§4.5).
//!
//! Deliberately out of MVP scope (clean `TypeError` at runtime, tracked in
//! ROADMAP): `bigint`/`bigdec` arithmetic, `async`/`await`, dynamic
//! `import()`, multi-module graphs.
//!
//! The AST is borrowed with `&'static` lifetime; drivers leak one parsed
//! module per program (bounded, lives for the process/page lifetime).

use std::cell::RefCell;
/// The engine's own maps, hashed with FxHash rather than SipHash.
///
/// Every key in here is *program text* — a variable name, a member name, a
/// module path, an interned id, a pointer — and never anything a running
/// program supplies: Mersey's `Map<K,V>` is an insertion-ordered `Vec`, so no
/// user-controlled key ever reaches a hash table in the engine. SipHash's job
/// is to stop an attacker from colliding a table on purpose; with no such key
/// there is nothing to defend, and it is not free — `sip::Hasher::write` landed
/// in the top ten of a `url` benchmark profile, called from variable lookup and
/// namespace member lookup. FxHash is what rustc uses for exactly this case.
///
/// If a map keyed by runtime data is ever added here, give *that* map a
/// DoS-resistant hasher explicitly; do not widen this alias.
pub(crate) type HashMap<K, V> = std::collections::HashMap<K, V, rustc_hash::FxBuildHasher>;
pub(crate) type HashSet<K> = std::collections::HashSet<K, rustc_hash::FxBuildHasher>;
use std::rc::Rc;

use mersey_front::ast::*;
use mersey_front::check;

pub mod bignum;
pub mod debug;
pub mod embed;
pub mod gc;
use gc::GcCell;
pub mod regex;
pub mod vm;
pub mod webjson;
use bignum::{BigDec, BigInt, RoundingMode};
use webjson::Json;

/// The host-embedding ABI version. The single source of truth: `mersey_capi`'s
/// `MSY_ABI_VERSION` references this, and the language surfaces it as
/// `Mersey.abiVersion` (via `std:mersey`), so a program, the C header, and the
/// engine cannot disagree about which host contract they speak.
pub const ABI_VERSION: u32 = 10;

/// SHA-256 (FIPS 180-4), a dependency-free reference implementation. Returns the
/// 32-byte digest. Exposed to the language as `std:hash`'s `sha256`, and reused
/// by the CLI (SRI hashes, the remote-import cache) so there is one copy.
pub fn sha256(data: &[u8]) -> Vec<u8> {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut msg = data.to_vec();
    let bit_len = (data.len() as u64) * 8;
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in msg.chunks(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                chunk[i * 4],
                chunk[i * 4 + 1],
                chunk[i * 4 + 2],
                chunk[i * 4 + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh) =
            (h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]);
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (i, v) in [a, b, c, d, e, f, g, hh].into_iter().enumerate() {
            h[i] = h[i].wrapping_add(v);
        }
    }

    h.iter().flat_map(|v| v.to_be_bytes()).collect()
}

/// SHA-1 (FIPS 180-1), a dependency-free reference implementation. Returns the
/// 20-byte digest. Exposed as `std:hash`'s `sha1`. SHA-1 is broken for
/// collision resistance and must not be used for new signatures, but it is
/// still what git object IDs, HMAC-SHA1 and TOTP are defined over — this exists
/// for those, not for security.
pub fn sha1(data: &[u8]) -> Vec<u8> {
    let mut h: [u32; 5] = [0x67452301, 0xefcdab89, 0x98badcfe, 0x10325476, 0xc3d2e1f0];
    let mut msg = data.to_vec();
    let bit_len = (data.len() as u64) * 8;
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in msg.chunks(64) {
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                chunk[i * 4],
                chunk[i * 4 + 1],
                chunk[i * 4 + 2],
                chunk[i * 4 + 3],
            ]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let (mut a, mut b, mut c, mut d, mut e) = (h[0], h[1], h[2], h[3], h[4]);
        for (i, &wi) in w.iter().enumerate() {
            let (f, k) = if i < 20 {
                ((b & c) | ((!b) & d), 0x5a827999u32)
            } else if i < 40 {
                (b ^ c ^ d, 0x6ed9eba1)
            } else if i < 60 {
                ((b & c) | (b & d) | (c & d), 0x8f1bbcdc)
            } else {
                (b ^ c ^ d, 0xca62c1d6)
            };
            let tmp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = tmp;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }
    h.iter().flat_map(|v| v.to_be_bytes()).collect()
}

/// HMAC (RFC 2104) over a chosen hash with a 64-byte block. Shared by
/// [`hmac_sha256`] and [`hmac_sha1`].
fn hmac(hashfn: fn(&[u8]) -> Vec<u8>, key: &[u8], data: &[u8]) -> Vec<u8> {
    const BLOCK: usize = 64;
    // A key longer than the block is hashed down; then it is zero-padded.
    let mut k = if key.len() > BLOCK {
        hashfn(key)
    } else {
        key.to_vec()
    };
    k.resize(BLOCK, 0);
    let mut inner = Vec::with_capacity(BLOCK + data.len());
    inner.extend(k.iter().map(|b| b ^ 0x36));
    inner.extend_from_slice(data);
    let inner_hash = hashfn(&inner);
    let mut outer = Vec::with_capacity(BLOCK + inner_hash.len());
    outer.extend(k.iter().map(|b| b ^ 0x5c));
    outer.extend_from_slice(&inner_hash);
    hashfn(&outer)
}

/// HMAC-SHA-256 (RFC 2104): the primitive for a signed cookie or a JWT
/// signature. Returns the 32-byte MAC.
pub fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
    hmac(sha256, key, data)
}

/// HMAC-SHA-1 (RFC 2104): what TOTP/HOTP authenticator codes are built on.
/// Returns the 20-byte MAC.
pub fn hmac_sha1(key: &[u8], data: &[u8]) -> Vec<u8> {
    hmac(sha1, key, data)
}

// ---- host interface ---------------------------------------------------------

/// A call/constructor argument that needs no JSON: a number or a string. The
/// multi-scalar fast paths (web_call_scalars, web_new_scalars) carry these
/// directly, so `setItem(k, v)`, `fillRect(x, y, w, h)` and `new URL(s)` do not
/// build and parse an args array.
pub enum WebScalar<'a> {
    Num(f64),
    Str(&'a str),
}

/// Owned form, so a string argument outlives the borrow handed to the host.
enum OwnedScalar {
    Num(f64),
    Str(String),
}

/// Every argument as a scalar, or None if any is not (an object, an array, a
/// host handle) — those still need the reflective JSON path.
fn as_scalars(args: &[Value]) -> Option<Vec<OwnedScalar>> {
    let mut out = Vec::with_capacity(args.len());
    for a in args {
        out.push(match a {
            Value::Str(s) => OwnedScalar::Str(utf16_to_string(s)),
            Value::I32(n) => OwnedScalar::Num(*n as f64),
            Value::F64(f) => OwnedScalar::Num(*f),
            Value::I64(n) => OwnedScalar::Num(*n as f64),
            _ => return None,
        });
    }
    Some(out)
}

fn scalar_ref(s: &OwnedScalar) -> WebScalar<'_> {
    match s {
        OwnedScalar::Num(n) => WebScalar::Num(*n),
        OwnedScalar::Str(s) => WebScalar::Str(s),
    }
}

/// An argument for the wide-string fast path. A string is the engine's own
/// Compile-time-stable ids for the typed web-binding fast path (`Host::web_bind`).
///
/// A web method whose receiver type and argument shape are known at compile time
/// does not need to cross the bridge as an interned name that the host then
/// string-dispatches. It crosses as one of these small integers, and the host
/// switches straight to the C++ method. The ids are a fixed contract shared with
/// the fork (mirrored in `mersey.h`) — only ever append, never renumber.
///
/// Only *numeric-argument* methods are here: those are the ones the Tier-1 JIT
/// compiles (a canvas draw loop), which is where removing the per-call intern +
/// marshal + string-dispatch actually shows up. String-argument methods stay on
/// the reflective path, which the JIT does not compile anyway.
pub mod webbind {
    pub const CANVAS2D_FILLRECT: u32 = 1;
    pub const CANVAS2D_CLEARRECT: u32 = 2;
    pub const CANVAS2D_STROKERECT: u32 = 3;
    pub const CANVAS2D_RECT: u32 = 4;
    pub const CANVAS2D_MOVETO: u32 = 5;
    pub const CANVAS2D_LINETO: u32 = 6;
    pub const CANVAS2D_TRANSLATE: u32 = 7;
    pub const CANVAS2D_SCALE: u32 = 8;
    pub const CANVAS2D_ROTATE: u32 = 9;

    /// A method name and its argument count → a numeric bind id, or `None` when
    /// the method has no typed fast path. Name-based rather than type-based: the
    /// JIT applies it only to host-object (`JsRef`) receivers, and the host
    /// verifies the receiver's actual type before taking the fast path, so a
    /// same-named method on a different object simply falls back — never wrong.
    pub fn numeric(method: &str, argc: u8) -> Option<u32> {
        Some(match (method, argc) {
            ("fillRect", 4) => CANVAS2D_FILLRECT,
            ("clearRect", 4) => CANVAS2D_CLEARRECT,
            ("strokeRect", 4) => CANVAS2D_STROKERECT,
            ("rect", 4) => CANVAS2D_RECT,
            ("moveTo", 2) => CANVAS2D_MOVETO,
            ("lineTo", 2) => CANVAS2D_LINETO,
            ("translate", 2) => CANVAS2D_TRANSLATE,
            ("scale", 2) => CANVAS2D_SCALE,
            ("rotate", 1) => CANVAS2D_ROTATE,
            _ => return None,
        })
    }
}

/// A host reply, laid out exactly as the C `msy_reply` (and
/// `mersey_capi::MsyReply`). The typed-binding fast path (`WebBindFn`) reads only
/// the tag — 5 is a thrown error, anything else a success whose value the
/// compiled call discards — so the JIT can call the host directly and still tell
/// the two apart without decoding the reply.
#[repr(C)]
pub struct WebReplyRaw {
    pub tag: i32,
    pub num: f64,
    pub str16: *const u16,
    pub str16_len: usize,
}

/// The host's `web_bind` entry as a raw C function pointer the compiled tier can
/// call directly — no interpreter reentry, no dynamic `Host` dispatch. Same ABI
/// as the C host table's `web_bind` field; `mersey_capi` hands it over through
/// `Host::web_bind_raw`. Kept as a plain (not `unsafe`) `extern "C" fn` so this
/// crate stays free of `unsafe`; the one caller that invokes it (the JIT's
/// `heap.rs`) owns the safety argument.
pub type WebBindFn =
    extern "C" fn(*mut core::ffi::c_void, i64, u32, *const f64, usize, *mut WebReplyRaw);

/// UTF-16 (`&[u16]`), borrowed straight from the engine's string buffer and
/// passed to the host with zero copying and no conversion — Gecko/V8 are UTF-16
/// too, so both sides now speak the same encoding. See `Host::web_call_u16`.
pub enum WebArg<'a> {
    Num(f64),
    Str(&'a [u16]),
    /// A host-object handle (`body.appendChild(el)`, `getRandomValues(buf)`).
    /// Crosses as a number plus a kind tag; the bridge resolves it back to the
    /// object, so calls with object arguments stay on the fast path.
    Ref(i64),
    Bool(bool),
    Null,
    /// A durable Mersey callable, crossing as its stable callback id — the
    /// same id the JSON path's `{"__cb__":id}` carries, so the host resolves
    /// it to its cached wrapper. This is what keeps `setTimeout(cb, ms)` and
    /// `addEventListener(type, cb)` off the JSON path entirely (ABI v8).
    Cb(u32),
}

/// A typed reply from the wide-string fast path — no JSON for the common cases.
/// Scalars come back decoded; a string arrives as the host's native UTF-16 and
/// *is* the engine's string form (a plain copy of the code units). `Json`
/// carries the rare non-scalar result (an object or array) as tagged JSON.
pub enum WebReply {
    Null,
    Num(f64),
    Str(Vec<u16>),
    Ref(i64),
    Bool(bool),
    Err(String),
    Json(String),
}

/// True for values the wide-string fast path can carry (scalars only).
fn is_web_scalar(v: &Value) -> bool {
    matches!(
        v,
        Value::Str(_)
            | Value::I32(_)
            | Value::F64(_)
            | Value::I64(_)
            | Value::Bool(_)
            | Value::Null
            | Value::JsRef(_)
    )
}

/// True for values a `web_call_u16` argument can carry: scalars, plus durable
/// callables (which cross as their stable callback id — `WebArg::Cb`).
fn is_web_scalar_or_cb(v: &Value) -> bool {
    is_web_scalar(v) || matches!(v, Value::Closure(_) | Value::Native(_))
}

/// A `WebArg` back to a `Value`, for the general `web_call` fallback when the
/// host declines the interned wide path. The inverse of `value_as_webarg`.
fn webarg_to_value(a: &WebArg) -> Value {
    match a {
        WebArg::Num(f) => Value::F64(*f),
        WebArg::Str(units) => Value::Str(Rc::new(units.to_vec())),
        WebArg::Ref(h) => Value::JsRef(*h),
        WebArg::Bool(b) => Value::Bool(*b),
        WebArg::Null => Value::Null,
        // Only the JIT shims round-trip WebArgs back to Values, and compiled
        // code never carries callables — this arm is unreachable there.
        WebArg::Cb(_) => Value::Null,
    }
}

/// Borrow a scalar `Value` as a `WebArg` — the string case is zero-copy.
fn value_as_webarg(v: &Value) -> WebArg<'_> {
    match v {
        Value::Str(s) => WebArg::Str(s),
        Value::I32(n) => WebArg::Num(*n as f64),
        Value::F64(f) => WebArg::Num(*f),
        Value::I64(n) => WebArg::Num(*n as f64),
        Value::Bool(b) => WebArg::Bool(*b),
        Value::Null => WebArg::Null,
        Value::JsRef(h) => WebArg::Ref(*h),
        _ => WebArg::Num(0.0), // never reached: callers gate on is_web_scalar
    }
}

/// Everything the interpreter can ask of the outside world. The CLI wires
/// this to stdout; the browser build wires it to `console`/DOM via the
/// loader (docs/architecture/browser-integration.md, Stage A).
pub trait Host {
    fn print(&mut self, s: &str);
    fn dom_set_text(&mut self, id: &str, text: &str);
    fn dom_get_text(&mut self, id: &str) -> Option<String>;
    /// Register callback `cb` (an index the driver passes back to
    /// `Interp::invoke_callback`) for click events on element `id`.
    /// Register `cb` as a listener for `event` on element `id`. The host owns
    /// the event loop; the engine only ever hands it a callback id.
    fn dom_add_listener(&mut self, id: &str, event: &str, cb: u32);

    /// Create an element; returns its handle id.
    fn dom_create(&mut self, _tag: &str) -> String {
        String::new()
    }
    fn dom_append(&mut self, _parent: &str, _child: &str) {}
    fn dom_remove(&mut self, _id: &str) {}
    fn dom_get_value(&mut self, _id: &str) -> String {
        String::new()
    }
    fn dom_set_value(&mut self, _id: &str, _value: &str) {}

    // ---- capability-gated I/O (spec §5.3): deny by default -------------
    fn read_text(&mut self, _path: &str) -> Result<String, String> {
        Err("no `read` capability (run with --allow-read)".into())
    }
    fn env_var(&mut self, _name: &str) -> Option<String> {
        None
    }
    /// Fill `n` bytes with cryptographically secure randomness.
    ///
    /// Denied by default, like every other capability (§5.3). Randomness is
    /// gated because it is *authority*: it seeds tokens and keys, and it is an
    /// observable side channel — a program that can ask for it can fingerprint,
    /// and one that can only ask through the host can be given a deterministic
    /// stream for a reproducible test run.
    /// Fill a buffer the caller already has. The default routes through
    /// `random_bytes`, which allocates and copies; a host that can write
    /// straight into the slice should say so — that allocation is the whole
    /// point of having this hook.
    fn random_fill(&mut self, buf: &mut [u8]) -> Result<(), String> {
        let fresh = self.random_bytes(buf.len())?;
        buf.copy_from_slice(&fresh);
        Ok(())
    }
    fn random_bytes(&mut self, _n: usize) -> Result<Vec<u8>, String> {
        Err("no `random` capability (run with --allow-random)".into())
    }

    /// Register an HTTP request handler for `port` (the engine holds the handler
    /// as callback `cb_id`; the host records the (port, id) pair and the CLI's
    /// accept loop drives it via `Interp::http_dispatch`). Denied by default,
    /// like every capability (§5.3); the CLI grants it with `--allow-net`. Only
    /// the CLI host serves — every other embedder keeps the refusing default.
    fn request_serve(&mut self, _port: u16, _cb_id: u32) -> Result<(), String> {
        Err("no `net` capability (run with --allow-net), or this host cannot serve".into())
    }
    /// Consumed once by the driver after top-level completes: the (port, cb_id)
    /// a `net.serve` call recorded, if any.
    fn take_pending_server(&mut self) -> Option<(u16, u32)> {
        None
    }

    /// `console.warn`/`error`/`info`/`debug`. The default sends everything to
    /// `print`, so a host that does not care about levels needs to do nothing.
    fn print_level(&mut self, _level: &str, s: &str) {
        self.print(s);
    }
    fn caps(&self) -> Vec<String> {
        Vec::new()
    }
    fn drop_cap(&mut self, _cap: &str) {}

    // ---- universal web bridge (spec §5.4: import-gated) ---------------
    /// Resolve an ambient global to a handle; -1 = unavailable.
    fn web_global(&mut self, _name: &str) -> i64 {
        -1
    }
    /// Read `target[prop]`; returns a tagged-JSON WebValue, or an
    /// `{"err":"…"}` object. Default: not available.
    fn web_get(&mut self, _target: i64, _prop: &str) -> String {
        "{\"err\":\"no web bridge\"}".into()
    }
    fn web_set(&mut self, _target: i64, _prop: &str, _value_json: &str) -> String {
        "{\"err\":\"no web bridge\"}".into()
    }
    /// Call `target[method](args)`; method "" calls the target itself.
    fn web_call(&mut self, _target: i64, _method: &str, _args_json: &str) -> String {
        "{\"err\":\"no web bridge\"}".into()
    }
    fn web_new(&mut self, _ctor: &str, _args_json: &str) -> String {
        "{\"err\":\"no web bridge\"}".into()
    }

    // ---- fast paths (avoid JSON + per-call string decoding) -------------
    /// Intern a member name; the id is stable for the host's lifetime.
    fn web_intern(&mut self, _name: &str) -> u32 {
        u32::MAX
    }
    fn web_get_id(&mut self, _target: i64, _name_id: u32) -> String {
        "{\"err\":\"no web bridge\"}".into()
    }
    fn web_set_str(&mut self, _target: i64, _name_id: u32, _value: &str) -> String {
        "{\"err\":\"no web bridge\"}".into()
    }
    fn web_set_num(&mut self, _target: i64, _name_id: u32, _value: f64) -> String {
        "{\"err\":\"no web bridge\"}".into()
    }
    /// Call with a single string argument (`createElement("span")`, …).
    fn web_call_str(&mut self, _target: i64, _name_id: u32, _arg: &str) -> String {
        "{\"err\":\"no web bridge\"}".into()
    }
    /// Call with any number of scalar arguments, no JSON (`setItem(k, v)`,
    /// `fillRect(x, y, w, h)`). An empty reply means "not supported, use the
    /// reflective path" — so a host may implement only the ops it wants.
    fn web_call_scalars(&mut self, _target: i64, _name_id: u32, _args: &[WebScalar]) -> String {
        String::new()
    }
    /// Construct with scalar arguments, no JSON (`new URL(s)`). Empty reply =
    /// "not supported, use the reflective web_new".
    fn web_new_scalars(&mut self, _ctor_id: u32, _args: &[WebScalar]) -> String {
        String::new()
    }

    // ---- wide-string fast paths (UTF-32 args in, UTF-16 reply out) ------
    // The engine is UTF-32 and a UTF-16 host (Gecko) is UTF-16; these skip the
    // UTF-8 intermediary and the JSON reply entirely. `None` = not supported,
    // and the engine falls back to the UTF-8 ops above with identical results.
    fn web_get_u16(&mut self, _target: i64, _name_id: u32) -> Option<WebReply> {
        None
    }
    fn web_set_u16(&mut self, _target: i64, _name_id: u32, _value: &WebArg) -> Option<WebReply> {
        None
    }
    fn web_call_u16(&mut self, _target: i64, _name_id: u32, _args: &[WebArg]) -> Option<WebReply> {
        None
    }
    fn web_new_u16(&mut self, _ctor_id: u32, _args: &[WebArg]) -> Option<WebReply> {
        None
    }
    /// The typed-binding fast path: a compiled numeric web method identified by
    /// a compile-time-stable `bind_id` (see `webbind`), its arguments already
    /// `f64`. No interned name, no argument marshalling, no string dispatch —
    /// the host switches on the id and calls the C++ method directly, checking
    /// the receiver's type and falling back to the reflective path itself on a
    /// mismatch. A host that has no such fast path returns `None`, and the
    /// caller uses the ordinary `web_call_u16`.
    fn web_bind(&mut self, _target: i64, _bind_id: u32, _args: &[f64]) -> Option<WebReply> {
        None
    }
    /// Apply a batch of DOM mutations in one host call (ABI v10; see
    /// docs/architecture/dom-batching.md). `ops` is the flat op stream (groups
    /// of 4: opcode, x, y, z — opcodes 0 create, 1 set-text, 2 append, 3 insert,
    /// 4 remove). A node operand is a non-negative index into `nodes` (a live
    /// handle) or a negative `-(k+1)` naming the k-th node the batch created;
    /// str operands index `strs` (tag names + texts). Returns the created nodes'
    /// live handles in creation order, or `None` if the host has no batched path
    /// (the engine then replays the ops one at a time — identical result).
    fn web_apply(&mut self, _ops: &[i32], _nodes: &[i64], _strs: &[String]) -> Option<Vec<i64>> {
        None
    }
    /// The `web_bind` entry as a raw C function pointer plus its host data, so
    /// the compiled tier can call it *directly* — skipping the interpreter
    /// reentry and the dynamic dispatch that `web_bind` would go through. A host
    /// that has no C-level binding (or none at all) returns `None`, and the JIT
    /// uses the ordinary `web_bind` shim path. Only the error case (tag 5) then
    /// needs the interpreter, to build and stash the thrown value.
    fn web_bind_raw(&self) -> Option<(WebBindFn, *mut core::ffi::c_void)> {
        None
    }
    /// Snapshot a host iterable (NodeList, HTMLCollection, Set, …) as an
    /// array, so `for (const n of nodeList)` works.
    fn web_iterate(&mut self, _target: i64) -> String {
        "{\"err\":\"no web bridge\"}".into()
    }
    /// Drop a host handle (and any callbacks it retained).
    fn web_release(&mut self, _target: i64) {}

    /// Bulk-copy a host typed array / ArrayBuffer into engine memory.
    fn web_bytes_read(&mut self, _target: i64) -> Option<Vec<u8>> {
        None
    }
    /// Bulk-copy engine bytes back into a fresh host Uint8ClampedArray-ish
    /// object; returns its handle (or -1).
    fn web_bytes_write(&mut self, _bytes: &[u8]) -> i64 {
        -1
    }
    /// `object instanceof constructor` on the host side.
    fn web_instanceof(&mut self, _target: i64, _ctor: i64) -> bool {
        false
    }
    /// Wall-clock (`epoch = true`) or monotonic milliseconds. Time is not
    /// capability-gated: it leaks no data the program didn't already have.
    fn time_ms(&mut self, _epoch: bool) -> f64 {
        0.0
    }
}

// ---- values -------------------------------------------------------------------

// Strings are WTF-16: a buffer of UTF-16 code units, exactly as a browser holds
// them (Gecko's nsString, V8's String). `length` and indexing are code-unit
// based, byte-identical to JavaScript. Lone surrogates round-trip losslessly to
// a UTF-16 host (unlike a `Vec<char>`, which cannot represent them). See the
// `string-representation` project note.
type Str = Rc<Vec<u16>>;

/// A `Map` key or `Set` member, hashed and compared the way the language's `==`
/// compares it — which is what lets those collections be hash tables instead of
/// the linear scans they used to be.
///
/// `values_equal` is the semantics being matched: value equality for the
/// scalars and strings, *identity* for everything with an `Rc` behind it. It
/// differs in one place, deliberately. `values_equal` throws when asked to
/// compare two types that have no comparison — so a lookup in a map holding a
/// key of some other type used to fail with a `TypeError` that depended on what
/// else was in the map. Here such a pair is simply not equal, which is the
/// answer the caller was asking for.
#[derive(Clone)]
pub struct Key(pub Value);

/// Numbers compare across their representations (`1` and `1.0` are the same
/// key), so they must hash across them too: an integral value hashes as its
/// integer whatever type it arrived as.
fn num_key(v: &Value) -> Option<(bool, i64, u64)> {
    let f = as_num(v)?;
    if f.fract() == 0.0 && f >= i64::MIN as f64 && f <= i64::MAX as f64 {
        Some((true, f as i64, 0))
    } else {
        // -0.0 already hashed as integral 0; a NaN hashes consistently but is
        // equal to nothing, exactly as `==` says.
        Some((false, 0, f.to_bits()))
    }
}

impl PartialEq for Key {
    fn eq(&self, other: &Self) -> bool {
        match (&self.0, &other.0) {
            (Value::Null, Value::Null) => true,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Char(a), Value::Char(b)) => a == b,
            (Value::Str(a), Value::Str(b)) => a == b,
            (Value::BigIntV(a), Value::BigIntV(b)) => a.cmp(b) == std::cmp::Ordering::Equal,
            (Value::BigDecV(a), Value::BigDecV(b)) => a.cmp(b) == std::cmp::Ordering::Equal,
            (Value::JsRef(a), Value::JsRef(b)) => a == b,
            (Value::Bytes(a), Value::Bytes(b)) => Rc::ptr_eq(a, b),
            (Value::MapV(a), Value::MapV(b)) => Rc::ptr_eq(a, b),
            (Value::SetV(a), Value::SetV(b)) => Rc::ptr_eq(a, b),
            (Value::Array(a), Value::Array(b)) => Rc::ptr_eq(a, b),
            (Value::Record(a), Value::Record(b)) => Rc::ptr_eq(a, b),
            (Value::Instance(a), Value::Instance(b)) => Rc::ptr_eq(a, b),
            (Value::Closure(a), Value::Closure(b)) => Rc::ptr_eq(a, b),
            (a, b) => match (num_key(a), num_key(b)) {
                // NaN is equal to nothing, including itself (`==` says so).
                (Some((true, x, _)), Some((true, y, _))) => x == y,
                (Some((false, _, x)), Some((false, _, y))) => x == y && !f64::from_bits(x).is_nan(),
                _ => false,
            },
        }
    }
}
impl Eq for Key {}

impl std::hash::Hash for Key {
    fn hash<H: std::hash::Hasher>(&self, h: &mut H) {
        // The tag keeps two different kinds from colliding by accident; every
        // numeric kind shares one tag because they compare across kinds.
        match &self.0 {
            Value::Null => 0u8.hash(h),
            Value::Bool(b) => (1u8, b).hash(h),
            Value::Char(c) => (2u8, c).hash(h),
            Value::Str(s) => (3u8, s.as_slice()).hash(h),
            Value::BigIntV(b) => (4u8, b.to_decimal()).hash(h),
            Value::BigDecV(d) => (5u8, d.to_decimal()).hash(h),
            Value::JsRef(r) => (6u8, r).hash(h),
            Value::Bytes(b) => (7u8, Rc::as_ptr(b) as usize).hash(h),
            Value::MapV(m) => (7u8, Rc::as_ptr(m) as usize).hash(h),
            Value::SetV(s) => (7u8, Rc::as_ptr(s) as usize).hash(h),
            Value::Array(a) => (7u8, Rc::as_ptr(a) as usize).hash(h),
            Value::Record(r) => (7u8, Rc::as_ptr(r) as usize).hash(h),
            Value::Instance(i) => (7u8, Rc::as_ptr(i) as usize).hash(h),
            Value::Closure(c) => (7u8, Rc::as_ptr(c) as usize).hash(h),
            other => match num_key(other) {
                Some((true, i, _)) => (8u8, i).hash(h),
                Some((false, _, bits)) => (8u8, bits).hash(h),
                None => 9u8.hash(h),
            },
        }
    }
}

/// `Map` and `Set` storage.
///
/// `RandomState` — SipHash — and *not* the FxHash the engine's own tables use:
/// these are the one kind of table whose keys a running program chooses, so
/// this is exactly the case the alias above warns against widening.
pub type MapData = indexmap::IndexMap<Key, Value, std::collections::hash_map::RandomState>;
pub type SetData = indexmap::IndexSet<Key, std::collections::hash_map::RandomState>;

/// A Rust string as the engine's UTF-16 form.
///
/// Sized up front rather than `collect()`ed: `EncodeUtf16`'s lower size hint is
/// `len/3` (it must assume every character is three bytes), so collecting a
/// mostly-ASCII string reallocates two or three times on the way. `len` code
/// units is the exact answer for ASCII and never an under-estimate for anything
/// else — one allocation, always. Every string a native returns comes through
/// here.
pub(crate) fn utf16(s: &str) -> Vec<u16> {
    // ASCII widens byte-for-byte, and saying so in a form the compiler can see
    // is worth a lot: `Vec::extend` over `EncodeUtf16` re-checks capacity on
    // every unit (the iterator cannot promise its length), while mapping over
    // the bytes is exact-sized — one allocation and a vectorizable widening
    // loop. `is_ascii` is itself a word-at-a-time scan.
    if s.is_ascii() {
        return s.as_bytes().iter().map(|&b| u16::from(b)).collect();
    }
    // One SIMD pass, one allocation. UTF-8 never yields more UTF-16 units than it
    // has bytes, so `s.len()` is an exact upper bound — where `extend` over
    // `EncodeUtf16` had to re-check capacity per unit, because the iterator
    // cannot promise its length.
    let mut out = vec![0u16; s.len()];
    let n = encoding_rs::mem::convert_str_to_utf16(s, &mut out);
    out.truncate(n);
    out
}

/// Append a decimal integer to a UTF-16 buffer, exactly as the bytecode VM's
/// `TemplateJoin` does — the JIT's string-join shim calls this so a compiled
/// `` `…${i}` `` produces byte-identical output.
pub fn append_int_utf16(out: &mut Vec<u16>, v: i64) {
    vm::append_int_u16(out, v);
}

/// The engine's UTF-16 string as a Rust `String`. Lossy only on lone surrogates
/// (U+FFFD), which affects display and UTF-8 marshalling but not the raw units
/// that cross to a UTF-16 host.
/// The engine's UTF-16 string as UTF-8 *bytes*.
///
/// Split out from `utf16_to_string` because `bytes.encodeUtf8` wants exactly this
/// and was going the long way: build a `String` (which validates), then
/// `into_bytes()` (which throws the validation away).
pub(crate) fn utf16_to_utf8_bytes(s: &[u16]) -> Vec<u8> {
    // ASCII narrows byte-for-byte, and knowing that buys an exactly-sized
    // allocation rather than the conversion's worst-case three-per-unit.
    if !s.iter().any(|&u| u >= 0x80) {
        return s.iter().map(|&u| u as u8).collect();
    }
    // One SIMD pass. Unpaired surrogates become U+FFFD, exactly as
    // `String::from_utf16_lossy` had them — and as a browser does, this being
    // Gecko's own converter.
    let mut out = vec![0u8; s.len() * 3];
    let n = encoding_rs::mem::convert_utf16_to_utf8(s, &mut out);
    out.truncate(n);
    out
}

pub(crate) fn utf16_to_string(s: &[u16]) -> String {
    // ASCII is the overwhelming majority of what crosses here — URLs, JSON
    // keys, identifiers, format strings — and for ASCII the answer is the low
    // byte of each unit. `from_utf16_lossy` cannot know that: it decodes
    // surrogate pairs one char at a time into a `String` sized from an iterator
    // hint of `len/2`, so it re-allocates on the way. The scan below is a
    // vectorizable pass that buys a single exact allocation.
    // The bytes, then one vectorized validation of them. That scan is what keeps
    // this function free of `unsafe`; the converter's output is valid UTF-8 by
    // construction, so it never fails.
    let bytes = utf16_to_utf8_bytes(s);
    String::from_utf8(bytes).unwrap_or_else(|_| String::from_utf16_lossy(s))
}

/// One `char` (a Unicode scalar) as a UTF-16 string (1 or 2 code units).
pub(crate) fn char_utf16(c: char) -> Vec<u16> {
    let mut buf = [0u16; 2];
    c.encode_utf16(&mut buf).to_vec()
}

/// Decode a UTF-16 string to code points (lone surrogates → U+FFFD). Used by the
/// regex subsystem, which matches on code points, not code units.
pub(crate) fn utf16_to_chars(s: &[u16]) -> Vec<char> {
    char::decode_utf16(s.iter().copied())
        .map(|r| r.unwrap_or('\u{FFFD}'))
        .collect()
}

/// Re-encode code points as UTF-16.
pub(crate) fn chars_to_u16(cs: &[char]) -> Vec<u16> {
    cs.iter().flat_map(|c| char_utf16(*c)).collect()
}

/// The code point starting at code-unit `i` (combining a surrogate pair), as a
/// Rust `char`; a lone surrogate decodes to U+FFFD. Used where the language
/// hands back a scalar (`Char`) rather than a code unit.
pub(crate) fn code_point_at(s: &[u16], i: usize) -> Option<char> {
    let u = *s.get(i)? as u32;
    if (0xD800..=0xDBFF).contains(&u) {
        if let Some(&lo) = s.get(i + 1) {
            if (0xDC00..=0xDFFF).contains(&(lo as u32)) {
                let c = 0x10000 + ((u - 0xD800) << 10) + (lo as u32 - 0xDC00);
                return Some(char::from_u32(c).unwrap_or('\u{FFFD}'));
            }
        }
    }
    Some(char::from_u32(u).unwrap_or('\u{FFFD}'))
}

/// One Mersey value.
///
/// `#[repr(u8)]` is not decoration. Tier 1 loads and stores these cells *in
/// place* — a field of an object, an element of an array — and to do that it has
/// to know where the tag is and where the payload is. Rust's default enum layout
/// is deliberately unspecified, so compiled code reading it would be reading
/// whatever this build happened to choose. `repr(u8)` fixes the layout by
/// language rule: a tag byte at offset 0, and each variant's payload laid out
/// after it as a C struct would be. The discriminants are written out for the
/// same reason — a variant reordered by someone tidying up must not silently
/// change what the machine code thinks a `float64` is.
///
/// It costs nothing: the widest payload is still one word, so a `Value` is still
/// 16 bytes. `repr::check()` proves the layout at startup rather than trusting
/// this comment.
#[derive(Clone)]
#[repr(u8)]
pub enum Value {
    Null = 0,
    Bool(bool) = 1,
    I32(i32) = 2,
    I64(i64) = 3,
    U32(u32) = 4,
    U64(u64) = 5,
    F32(f32) = 6,
    F64(f64) = 7,
    Char(char) = 8,
    Str(Str) = 9,
    BigIntV(Rc<BigInt>) = 10,
    BigDecV(Rc<BigDec>) = 11,
    Array(Rc<GcCell<Vec<Value>>>) = 12,
    /// Insertion-ordered map; key equality is `values_equal` (O(n) MVP).
    MapV(Rc<GcCell<MapData>>) = 13,
    SetV(Rc<GcCell<SetData>>) = 14,
    /// Insertion-ordered fields: a record's field order is part of its
    /// observable behaviour (it survives `JSON.stringify` across the bridge).
    Record(Rc<GcCell<Vec<(String, Value)>>>) = 15,
    Closure(Rc<Closure>) = 16,
    Class(Rc<ClassDef>) = 17,
    Instance(Rc<GcCell<Instance>>) = 18,
    /// `console`, `document`, enum objects: named bags of values.
    Namespace(Rc<Namespace>) = 19,
    /// A DOM element handle (Stage A: identified by element id).
    Dom(Rc<String>) = 20,
    /// Opaque handle to a host (JS) object, reached via the universal
    /// bridge. Handle 0 is the global object (window).
    JsRef(i64) = 21,
    /// Packed byte buffer with O(1) element access — the engine-side home
    /// for pixel/audio/binary data (no per-element bridge hops).
    Bytes(Rc<RefCell<Vec<u8>>>) = 22,
    /// A compiled regular expression.
    RegexV(Rc<regex::Regex>) = 23,
    /// A generator: a coroutine that produces values (`Iter<T>`).
    IterV(Rc<GcCell<GenState>>) = 24,
    /// A Mersey promise (§ async/await).
    PromiseV(Rc<GcCell<PromiseState>>) = 25,
    /// A callable that settles a promise; handed to host `.then(…)` so JS
    /// promises can resume Mersey coroutines. Two variants rather than one with a
    /// flag: the flag would be the *only* thing in the whole `Value` enum needing
    /// more than a pointer's worth of payload, and it would cost every value in
    /// the engine 8 bytes to carry it — on every clone, every stack push, every
    /// frame slot, every instance field.
    Resolve(Rc<GcCell<PromiseState>>) = 26,
    Reject(Rc<GcCell<PromiseState>>) = 27,
    /// Internal reaction used by `Promise.all` (slot index, is_reject).
    AllSlot(u32, bool) = 28,
    /// Executor handed to `new Promise(…)` on the host side: receives the
    /// host's (resolve, reject) and wires them to a Mersey promise, so a
    /// Mersey promise can cross the bridge as a *real* JS promise.
    PromiseExec(Rc<GcCell<PromiseState>>) = 29,
    /// A built-in, by name. A `&'static str` is a *fat* pointer — a pointer and
    /// a length — and it was the only payload in this enum wider than a word.
    /// That one variant made every `Value` 24 bytes instead of 16: eight wasted
    /// bytes on every clone, every stack push, every frame slot, every field of
    /// every object. A reference *to* the string is a thin pointer.
    Native(&'static &'static str) = 30,
    /// An absolute URL, already parsed (`parse.url`). Holds the host's parse so
    /// the seven WHATWG parts can be cut on demand: building them all up front
    /// cost more than the parse itself. `Rc` keeps this a thin pointer, so the
    /// enum is still 16 bytes.
    UrlV(Rc<url::Url>) = 31,
}

/// The layout of a [`Value`], as Tier 1's compiled code sees it.
///
/// Compiled code loads a `float64` field with one instruction, and the address
/// it loads from is `object + slot * SIZE + PAYLOAD`. That arithmetic is only
/// right if these numbers are right, and getting them wrong would not produce a
/// compile error — it would produce a program that reads the wrong bytes. So
/// they are *checked*, once, against a real value, before any heap-touching code
/// is compiled; see `mersey_jit`.
pub mod repr {
    /// Bytes per value. Every field of every object is one of these.
    pub const SIZE: usize = 16;

    /// Where each payload starts.
    ///
    /// **Not one offset.** `repr(u8)` lays each variant out as the C struct
    /// `{ tag: u8, payload }` — so the payload sits at *its own* alignment, and a
    /// `float64` is at 8 while an `int32` is at 4 and a `bool` is at 1. Assuming a
    /// single offset is the natural mistake, and it is not one the compiler would
    /// catch: it would read four bytes from the wrong place and carry on. Each of
    /// these is checked against a real value before any code that uses them is
    /// emitted (`mersey_jit::heap::layout_holds`).
    pub const OFF_BOOL: i32 = 1;
    pub const OFF_I32: i32 = 4;
    pub const OFF_I64: i32 = 8;
    pub const OFF_F64: i32 = 8;
    pub const TAG_NULL: u8 = 0;
    pub const TAG_BOOL: u8 = 1;
    pub const TAG_I32: u8 = 2;
    pub const TAG_I64: u8 = 3;
    pub const TAG_F64: u8 = 7;
    pub const TAG_STRING: u8 = 9;
    pub const TAG_ARRAY: u8 = 12;
    pub const TAG_JSREF: u8 = 21;
    pub const TAG_INSTANCE: u8 = 18;
}

/// One input slot of a pending `Promise.all`.
struct AllCell {
    results: Rc<GcCell<Vec<Value>>>,
    remaining: Rc<RefCell<usize>>,
    out: Rc<GcCell<PromiseState>>,
    idx: usize,
}

#[derive(Clone, PartialEq)]
pub enum PromiseStatus {
    Pending,
    Fulfilled,
    Rejected,
}

/// A `then`/`catch` reaction: (on_fulfilled, on_rejected, downstream promise).
type Reaction = (Option<Value>, Option<Value>, Rc<GcCell<PromiseState>>);

pub struct PromiseState {
    pub status: PromiseStatus,
    pub value: Value,
    /// Coroutines awaiting this promise.
    waiters: Vec<Coro>,
    /// `then`/`catch` reactions.
    reactions: Vec<Reaction>,
}

impl PromiseState {
    pub(crate) fn waiters(&self) -> &[Coro] {
        &self.waiters
    }
    #[allow(clippy::type_complexity)]
    pub(crate) fn reactions(&self) -> &[(Option<Value>, Option<Value>, Rc<GcCell<PromiseState>>)] {
        &self.reactions
    }

    /// Sweep: drop every edge out of an unreachable promise. Nothing can
    /// settle it or observe it, so its waiters and reactions are dead too.
    pub(crate) fn clear_edges(&mut self) {
        let mut sink = Vec::new();
        self.value = Value::Null;
        self.take_edges(&mut sink);
    }

    /// Move every value this promise holds into `out` — its own result, and
    /// whatever its waiting coroutines and reactions were holding.
    pub(crate) fn take_edges(&mut self, out: &mut Vec<Value>) {
        for coro in std::mem::take(&mut self.waiters) {
            out.extend(coro.stack);
            out.extend(coro.frame);
        }
        for (ok, err, _) in std::mem::take(&mut self.reactions) {
            out.extend(ok);
            out.extend(err);
        }
    }

    fn pending() -> Rc<GcCell<PromiseState>> {
        let p = Rc::new(GcCell::new(PromiseState {
            status: PromiseStatus::Pending,
            value: Value::Null,
            waiters: Vec::new(),
            reactions: Vec::new(),
        }));
        gc::track_promise(&p);
        p
    }
}

/// A suspended generator.
pub struct GenState {
    coro: Option<Coro>,
    done: bool,
    /// An *async* generator: one coroutine that both yields and awaits. The VM
    /// already reports all three outcomes (done, yielded, awaiting), so this
    /// needs no second mechanism — only somewhere to put the promise that the
    /// current `next()` will settle when the body finally reaches a `yield`.
    is_async: bool,
    /// The promise handed out by the `next()` now in flight.
    pending: Option<Rc<GcCell<PromiseState>>>,
    /// A *derived* iterator: it has no coroutine of its own, it pulls from
    /// another one and transforms. This is what makes `map`/`filter`/`take`
    /// lazy — `it.map(f).take(3)` runs the generator three times, not to
    /// exhaustion and then throws the rest away.
    adapter: Option<Adapter>,
}

#[derive(Clone)]
pub(crate) enum Adapter {
    Map(Rc<GcCell<GenState>>, Value),
    Filter(Rc<GcCell<GenState>>, Value),
    /// The remaining count is shared, because the adapter is cloned out of the
    /// GenState to be run without holding a borrow on it.
    Take(Rc<GcCell<GenState>>, Rc<std::cell::Cell<i64>>),
}

impl Adapter {
    pub(crate) fn inner(&self) -> Rc<GcCell<GenState>> {
        match self {
            Adapter::Map(i, _) | Adapter::Filter(i, _) | Adapter::Take(i, _) => i.clone(),
        }
    }
    pub(crate) fn func(&self) -> Option<Value> {
        match self {
            Adapter::Map(_, f) | Adapter::Filter(_, f) => Some(f.clone()),
            Adapter::Take(..) => None,
        }
    }
}

impl GenState {
    /// The suspended coroutine, if this generator has not finished. Its saved
    /// operand stack and scopes are GC roots for as long as it can be resumed.
    pub(crate) fn saved(&self) -> Option<Coro> {
        self.coro.clone()
    }

    /// The promise the in-flight `next()` will settle, if this is an async
    /// generator that is mid-await.
    pub(crate) fn pending_next(&self) -> Option<Rc<GcCell<PromiseState>>> {
        self.pending.clone()
    }

    /// Sweep: an unreachable generator can never be resumed, so drop the
    /// coroutine it was holding (which is where its cycle runs through).
    pub(crate) fn discard(&mut self) {
        let mut sink = Vec::new();
        self.take_coro(&mut sink);
    }

    /// Move the suspended coroutine's values into `out`.
    pub(crate) fn take_coro(&mut self, out: &mut Vec<Value>) {
        if let Some(coro) = self.coro.take() {
            out.extend(coro.stack);
            out.extend(coro.frame);
        }
        if let Some(a) = self.adapter.take() {
            out.extend(a.func());
        }
        self.pending = None;
        self.done = true;
    }

    /// The iterator this one pulls from, and the closure it applies.
    pub(crate) fn adapter_edges(&self) -> Option<(Rc<GcCell<GenState>>, Option<Value>)> {
        self.adapter.as_ref().map(|a| (a.inner(), a.func()))
    }
}

/// How a module's top level finished.
enum ModuleFlow {
    Done,
    /// The module's top-level `await` is still waiting on something only the
    /// host can settle. Everything that imports it waits too.
    Awaiting(Rc<GcCell<PromiseState>>),
}

/// A module graph paused on a top-level `await`.
struct PendingGraph {
    /// The suspended module's completion promise.
    promise: Rc<GcCell<PromiseState>>,
    spec: String,
    module: &'static Module,
    /// The suspended module's own scope, so its exports can be collected once
    /// it finishes.
    env: Env,
    /// Modules that have not run yet — the ones that import it, and their
    /// importers.
    rest: Vec<(String, &'static Module)>,
}

/// One entry of the diagnostic call stack.
#[derive(Clone)]
pub struct Frame_ {
    /// Shared, not owned. A call used to allocate *two Strings* — the function's
    /// name and its module — purely so that a stack trace could be printed if one
    /// were ever needed. Almost none ever are.
    pub name: Rc<str>,
    pub module: Rc<str>,
    pub pos: mersey_front::diag::Pos,
}

/// A debugger's view of the interpreter at one statement boundary.
pub struct DebugPause<'a> {
    /// Source position of the statement about to execute.
    pub pos: mersey_front::diag::Pos,
    /// The call stack, outermost first (`frames.last()` is the current frame).
    pub frames: &'a [Frame_],
}

/// The engine side of a debugger (a DAP adapter, a browser's CDP agent):
/// installed with `Interp::set_debug_hook`, called before every executable
/// statement of tree-walked code. Pausing IS blocking inside `on_stmt` — the
/// engine sits mid-statement until it returns — so all breakpoint/step policy
/// lives in the hook; the engine only reports. `locals` snapshots the scope
/// chain on demand (innermost scope first, values display-formatted), so a
/// hook that does not pause never pays for it. Installing a hook forces the
/// pure tree-walker (`use_vm = false`): sync code gets statement-grained
/// callouts; async and generator bodies still run on the VM (only it can
/// suspend) and are not stepped — a recorded v1 limit.
pub trait DebugHook {
    /// `locals(i)` snapshots frame `i` counted from the TOP of the stack:
    /// 0 is the paused statement's own scope chain; deeper frames serve the
    /// environment they were entered with. Out of range → empty.
    ///
    /// `eval(i, expr)` evaluates an expression string against frame `i`'s live
    /// scope — the debug console's evaluate-in-frame. It runs with runtime
    /// semantics (names resolve by name against the paused scope chain; no
    /// static re-check, since the frame's type context is not reconstructed),
    /// and returns the display result or an error message. The engine's own
    /// debug hook is suspended for the call, so an evaluated call that would
    /// hit a breakpoint does not re-pause. Out-of-range frame → error.
    fn on_stmt(
        &mut self,
        pause: &DebugPause,
        locals: &mut dyn FnMut(usize) -> Vec<Vec<(String, String)>>,
        eval: &mut dyn FnMut(usize, &str) -> Result<String, String>,
    );
}

/// Best-effort source position of an expression: the first positioned node
/// under it, left to right. `None` for the rare shapes with no carrier.
fn expr_pos(e: &Expr) -> Option<mersey_front::diag::Pos> {
    match e {
        Expr::Ident(n) => Some(n.pos),
        Expr::This(p) => Some(*p),
        Expr::Lit { pos, .. }
        | Expr::Unary { pos, .. }
        | Expr::SuperMember { pos, .. }
        | Expr::SuperCall { pos, .. }
        | Expr::Yield { pos, .. } => Some(*pos),
        Expr::Paren(x)
        | Expr::Update { expr: x, .. }
        | Expr::Cast { expr: x, .. }
        | Expr::Is { expr: x, .. }
        | Expr::ImportCall(x) => expr_pos(x),
        Expr::Binary { l, .. } => expr_pos(l),
        Expr::Assign { target, .. } => expr_pos(target),
        Expr::Cond { cond, .. } => expr_pos(cond),
        Expr::Call { callee, .. } => expr_pos(callee),
        Expr::Member { obj, .. } | Expr::Index { obj, .. } => expr_pos(obj),
        _ => None,
    }
}

fn pattern_pos(p: &Pattern) -> Option<mersey_front::diag::Pos> {
    match p {
        Pattern::Name(n) => Some(n.pos),
        _ => None,
    }
}

/// Best-effort position of a statement — the line a breakpoint on it hits.
/// Blocks and `try` report through their inner statements; `Empty` never.
fn stmt_pos(s: &Stmt) -> Option<mersey_front::diag::Pos> {
    match s {
        Stmt::Var(v) => v.bindings.first().and_then(|b| pattern_pos(&b.target)),
        Stmt::Expr(e) | Stmt::Throw(e) => expr_pos(e),
        Stmt::If { cond, .. } | Stmt::While { cond, .. } | Stmt::DoWhile { cond, .. } => {
            expr_pos(cond)
        }
        Stmt::For { init, cond, .. } => match init {
            Some(ForInit::Var(v)) => v.bindings.first().and_then(|b| pattern_pos(&b.target)),
            Some(ForInit::Exprs(es)) => es.first().and_then(expr_pos),
            None => cond.as_ref().and_then(expr_pos),
        },
        Stmt::ForOf { target, .. } => pattern_pos(target),
        Stmt::Switch { scrutinee, .. } => expr_pos(scrutinee),
        Stmt::Break { pos, .. } | Stmt::Continue { pos, .. } | Stmt::Return { pos, .. } => {
            Some(*pos)
        }
        Stmt::Labeled { label, .. } => Some(label.pos),
        Stmt::Block(_) | Stmt::Try { .. } | Stmt::Empty => None,
    }
}

/// The scope chain as the debugger shows it: innermost first, each scope a
/// name-sorted list of display-formatted values.
fn snapshot_scopes(env: &Env) -> Vec<Vec<(String, String)>> {
    let mut out = Vec::new();
    let mut cur = Some(env.clone());
    while let Some(e) = cur {
        let s = e.borrow();
        let mut vars: Vec<(String, String)> = s
            .vars
            .iter()
            .map(|(k, v)| (k.clone(), to_display(v)))
            .collect();
        vars.sort();
        out.push(vars);
        cur = s.parent.clone();
    }
    out
}

/// The scope-chain root of each frame, indexed by `from_top` (0 = the innermost,
/// paused frame). Mirrors the `locals(i)` mapping in `debug_stmt`: frame 0 is the
/// current `env`; deeper frames are the environments they were entered with,
/// stored outermost-first in `debug_envs`.
fn frame_env_chain(env: &Env, debug_envs: &[Env]) -> Vec<Env> {
    let mut v = vec![env.clone()];
    let dl = debug_envs.len();
    let mut ft = 1usize;
    while let Some(e) = dl.checked_sub(ft + 1).and_then(|i| debug_envs.get(i)) {
        v.push(e.clone());
        ft += 1;
    }
    v
}

/// A suspended async function: the VM's whole state is data, so `await`
/// captures it and resumes later (no CPS transform, no threads).
#[derive(Clone)]
pub struct Coro {
    /// The async generator this coroutine belongs to, if any. An `await` inside
    /// it suspends through the ordinary promise machinery; when the microtask
    /// queue resumes it, this is how the engine knows a `yield` must settle the
    /// generator's pending `next()` rather than the coroutine's own result.
    pub(crate) gen: Option<Rc<GcCell<GenState>>>,
    pub chunk: Rc<vm::Chunk>,
    pub pc: usize,
    pub stack: Vec<Value>,
    pub(crate) scopes: Vec<Env>,
    /// The frame's slot-resolved locals. A suspended coroutine owns them, so
    /// they must be saved with the rest of its state — and rooted with it, or
    /// the collector would sweep values that only a paused generator holds.
    pub frame: Vec<Value>,
    pub handlers: Vec<(usize, usize, usize)>,
    pub cls: Option<Rc<ClassDef>>,
    /// The promise this coroutine's completion settles.
    pub result: Rc<GcCell<PromiseState>>,
}

/// Work the engine owes itself before returning to the host.
enum Task {
    Resume(Coro, Value, bool),
    React(
        Option<Value>,
        Option<Value>,
        Rc<GcCell<PromiseState>>,
        Value,
        bool,
    ),
}

pub struct Namespace {
    pub name: String,
    pub entries: HashMap<String, Value>,
}

pub struct Closure {
    data: Rc<FnData>,
    pub(crate) env: Env,
    pub(crate) this: Option<Value>,
    /// Class that lexically contains the function (for `super`).
    pub(crate) cls: Option<Rc<ClassDef>>,
}

pub struct FnData {
    name: Rc<str>,
    is_async: bool,
    params: &'static [Param],
    body: FnBody,
    /// The numeric type the function is declared to return, if it returns a
    /// number. A compiled call needs to know what comes back before it compiles
    /// the callee — and with recursion it cannot wait to find out.
    ret_num: Option<mersey_front::check::Num>,
    /// The function is declared to return `bool`.
    ///
    /// A compiled kernel has one numeric world, and in an integer one a `bool`
    /// is an `i32` — comparisons produce 0 or 1 and flow through the same slots
    /// as everything else. That is fine *inside* the kernel and wrong the moment
    /// the value leaves it: `isEven(10)` would hand the interpreter `1` where
    /// every other tier gives `true`, and `console.log` would print it. The
    /// declared type is the only thing that knows, so it travels with the
    /// function and the result is converted back at the boundary.
    ret_bool: bool,
    /// …or an object. The declared return type as written; resolved to a class
    /// on the first Tier 1 compile that asks, when every class exists.
    ret_ty: Option<&'static TypeExpr>,
    /// Lazily compiled bytecode: None = not tried, Some(None) = this body
    /// uses a construct the compiler doesn't cover (AST fallback),
    /// Some(Some(chunk)) = compiled.
    chunk: RefCell<Option<Option<Rc<vm::Chunk>>>>,
}

impl FnData {
    /// The declared return type as written. `vm`'s OSR context carries this
    /// rather than the class it resolves to — see `vm::OsrCtx::ret_ty`.
    pub(crate) fn ret_ty(&self) -> Option<&'static TypeExpr> {
        self.ret_ty
    }

    fn new(
        name: Rc<str>,
        is_async: bool,
        params: &'static [Param],
        body: FnBody,
        ret: Option<&'static TypeExpr>,
    ) -> FnData {
        FnData {
            name,
            is_async,
            params,
            body,
            ret_bool: matches!(ret, Some(TypeExpr::Named { name, .. }) if name == "bool"),
            ret_num: match ret {
                Some(TypeExpr::Named { name, .. }) => num_of_name(name),
                _ => None,
            },
            ret_ty: ret,
            chunk: RefCell::new(None),
        }
    }
}

enum FnBody {
    Block(&'static [Stmt]),
    Expr(&'static Expr),
}

/// What a field holds, as far as Tier 1 is concerned.
///
/// The class knows its own field types — they are written in the source — and
/// this is them, resolved: a name like `Node` has become the class it names, so
/// compiled code reading `this.left.value` can find `value`'s offset without
/// looking anything up at run time.
#[derive(Clone)]
pub enum FieldTy {
    Num(mersey_front::check::Num),
    Bool,
    /// A class-typed field. It may hold `null`, and it may hold an instance of a
    /// *subclass* — both of which compiled code handles, because a subclass's
    /// layout begins with its base's (§4.1), so this class's offsets stay right.
    Obj(Rc<ClassDef>),
    Arr(Rc<FieldTy>),
    /// A UTF-16 string, or null. Tier 1 has a register shape for one — three,
    /// in fact: data pointer, length, and the arena handle if it owns it.
    Str,
    /// An engine primitive Tier 1 can carry without looking inside: a `Bytes`, a
    /// `Url`, a `Regex`. One arena handle, the same shape a native's result takes.
    Val,
    /// A nullable `int32`. One register, with `i64::MIN` for null — see the JIT's
    /// `Ty::I32Opt`. `int32?` is what a scan over code points is written in.
    NumOpt,
    /// A record, a function, a generic — something Tier 1 has no register for.
    /// Reading one is not a bug; it is a reason to interpret.
    Opaque,
}

pub struct ClassDef {
    /// Process-unique, never reused. Inline caches key on this rather than on
    /// the `Rc` address, which a later class could otherwise reuse after a
    /// free and silently make a stale cache hit (§4.1 layouts differ).
    pub(crate) id: u64,
    name: String,
    pub(crate) parent: Option<Rc<ClassDef>>,
    /// The declared type of each field, as *written*. Resolving it needs the
    /// other classes, and a class can name itself (`left: Node` inside `Node`),
    /// so it cannot be resolved while the class is being built.
    field_tyexprs: Vec<Option<&'static TypeExpr>>,
    /// …and as *resolved*, worked out on the first Tier 1 compile that needs it
    /// and kept. By then every class in the module graph exists.
    field_tys: RefCell<Option<Rc<Vec<FieldTy>>>>,
    /// Instance fields in initialization order (base-class fields first).
    /// Sealed shapes (§4.1) mean this layout is fixed at class-definition
    /// time, so a field is a **constant offset** — the whole point of
    /// removing prototypes.
    fields: Vec<(String, Option<&'static Expr>)>,
    /// The instance's slots, exactly as `new` should leave them before the
    /// constructor runs: every literal initializer already in place, `null`
    /// everywhere else.
    ///
    /// `new` used to walk the field list *three times* — once to build a vector
    /// of nulls, once to fill in the literals, and once to collect the
    /// initializers that were not literals. That was 20ns a field, on every
    /// allocation, to reproduce a result that is the same every time. It is
    /// computed once, at class definition, and cloned.
    initial_slots: Vec<Value>,
    /// The initializers that actually compute something, and so cannot be folded:
    /// `(slot, expr)`. Usually empty.
    dynamic_inits: Vec<(usize, &'static Expr)>,
    /// Container fields with no initializer: each instance gets its own fresh
    /// empty one, because an empty array in `initial_slots` would be one array
    /// *shared* by every instance.
    container_inits: Vec<(usize, mersey_front::check::DefaultVal)>,
    /// name → slot, computed once when the class is defined.
    field_slots: HashMap<String, u32>,
    methods: HashMap<String, Rc<FnData>>,
    getters: HashMap<String, Rc<FnData>>,
    setters: HashMap<String, Rc<FnData>>,
    ctor: Option<Rc<FnData>>,
    pub(crate) statics: GcCell<HashMap<String, Value>>,
    static_methods: HashMap<String, Rc<FnData>>,
    /// Built-in error classes construct without an AST ctor.
    is_builtin_error: bool,
    /// Host interface this class extends, if any (`extends HTMLElement`).
    host_iface: Option<String>,
    pub(crate) env: Option<Env>,
}

pub struct Instance {
    pub(crate) class: Rc<ClassDef>,
    /// Flat slots, indexed by the class's fixed layout — a constant-offset
    /// load, not a hash lookup.
    pub(crate) slots: Vec<Value>,
    /// Host object backing this instance (`class X extends HTMLElement`):
    /// members not declared in Mersey resolve against it, and the instance
    /// crosses the bridge AS that object.
    host: Option<i64>,
}

/// A bare instance of `cls`, exactly as `new` leaves it before the constructor
/// runs: literal initializers in place, fresh containers, zeros and nulls
/// elsewhere. This is the allocation itself, factored out so Tier 1 can perform
/// it through a shim — the compiled `new` is this call plus a compiled
/// constructor.
///
/// Only for classes whose initializers all fold (`dynamic_inits` empty): an
/// initializer that computes needs an evaluator, and this has none.
pub fn alloc_instance(cls: &Rc<ClassDef>) -> Value {
    let mut slots = cls.initial_slots.clone();
    for (slot, d) in &cls.container_inits {
        slots[*slot] = default_value(*d);
    }
    let inst = Rc::new(GcCell::new(Instance {
        class: cls.clone(),
        slots,
        host: None,
    }));
    gc::track_instance(&inst);
    Value::Instance(inst)
}

/// The address of an instance's fields, so Tier 1 can load one at a constant
/// offset. This is what sealed shapes (§4.1) were *for*.
///
/// There is no `unsafe` here, and that is not an accident: `RefCell` and `Vec`
/// both hand out the address of what they hold in safe Rust. The engine crate
/// never dereferences it — `mersey_jit` does, which is the one crate allowed to.
///
/// The pointer stays valid for as long as the instance does, because an
/// instance's slot vector is **never resized**: its length is its class's field
/// count, fixed when the class was declared. A language where an object can grow
/// a property could not offer this at all.
///
/// `None` when the instance is already borrowed — an object the engine is
/// already inside is one compiled code declines to touch, rather than one it
/// races with.
pub fn instance_slots(inst: &GcCell<Instance>) -> Option<*mut Value> {
    let b = inst.try_borrow()?;
    Some(b.slots.as_ptr() as *mut Value)
}

/// The address and length of an array's elements. Unlike an instance's slots,
/// these move when the array grows — so compiled code, which cannot grow one and
/// cannot call anything that could, reads them once and keeps them.
pub fn array_data(a: &GcCell<Vec<Value>>) -> Option<(*mut Value, usize)> {
    let b = a.try_borrow()?;
    Some((b.as_ptr() as *mut Value, b.len()))
}

// ---- environments ----------------------------------------------------------------

type Env = Rc<GcCell<Scope>>;

pub(crate) struct Scope {
    pub(crate) vars: HashMap<String, Value>,
    pub(crate) parent: Option<Env>,
}

fn child_env(parent: &Env) -> Env {
    let e = Rc::new(GcCell::new(Scope {
        vars: HashMap::default(),
        parent: Some(parent.clone()),
    }));
    gc::track_env(&e);
    e
}

fn env_get(env: &Env, name: &str) -> Option<Value> {
    let scope = env.borrow();
    if let Some(v) = scope.vars.get(name) {
        return Some(v.clone());
    }
    scope.parent.as_ref().and_then(|p| env_get(p, name))
}

fn env_set(env: &Env, name: &str, value: Value) -> bool {
    let mut scope = env.borrow_mut();
    if let Some(slot) = scope.vars.get_mut(name) {
        *slot = value;
        return true;
    }
    match &scope.parent {
        Some(p) => env_set(p, name, value),
        None => false,
    }
}

fn env_define(env: &Env, name: &str, value: Value) {
    env.borrow_mut().vars.insert(name.to_string(), value);
}

// ---- control flow / errors ---------------------------------------------------------

enum Sig {
    Normal,
    Return(Value),
    Break(Option<String>),
    Continue(Option<String>),
}

enum LoopCtl {
    BreakLoop,
    NextIter,
    Out(Sig),
}

fn loop_ctl(sig: Sig, label: Option<&str>) -> LoopCtl {
    match sig {
        Sig::Normal | Sig::Continue(None) => LoopCtl::NextIter,
        Sig::Break(None) => LoopCtl::BreakLoop,
        Sig::Continue(Some(l)) if Some(l.as_str()) == label => LoopCtl::NextIter,
        Sig::Break(Some(l)) if Some(l.as_str()) == label => LoopCtl::BreakLoop,
        other => LoopCtl::Out(other),
    }
}

/// A runtime error is always a thrown value (built-in errors are instances
/// of the built-in `Error` classes, so `catch (e: RangeError)` works).
pub struct Thrown(pub Value);

type VResult = Result<Value, Thrown>;
type SResult = Result<Sig, Thrown>;

// ---- interpreter ------------------------------------------------------------------

pub struct Interp {
    host: Box<dyn Host>,
    /// Callback slots the host still holds (a JS listener, a promise
    /// reaction). Cleared slots are reused, so a page that churns through
    /// listeners doesn't grow the table forever.
    free_callbacks: Vec<u32>,
    /// Callback id per closure identity (`Rc`/static pointer). The same
    /// closure crossing the bridge twice gets the same `{"__cb__":id}` — the
    /// host can cache one wrapper per id, wrapper identity is stable (so
    /// `removeEventListener` removes), and a hot arm/disarm loop
    /// (`setTimeout`/`clearTimeout`) stops allocating a slot per call. The
    /// table slot keeps the closure's `Rc` alive, so the key pointer cannot
    /// be reused while the entry lives; `release_callback` evicts.
    callback_ids: HashMap<usize, u32>,
    /// Shared prelude (built-in classes); every module scope descends from it.
    root: Env,
    globals: Env,
    callbacks: Vec<Value>,
    /// The debugger, when attached (see `DebugHook`). `None` costs one branch
    /// per tree-walked statement.
    debug_hook: Option<Box<dyn DebugHook>>,
    /// While debugging: each diagnostic frame's environment, parallel to
    /// `frames` — what `stackTrace`'s outer-frame variables read.
    debug_envs: Vec<Env>,
    error_classes: HashMap<&'static str, Rc<ClassDef>>,
    /// Every class the program has defined.
    ///
    /// Tier 1 compiles `o.m()` into a **direct call** — no vtable, no inline
    /// cache, no guard — when no subclass of `o`'s class overrides `m`. Answering
    /// that needs the whole hierarchy, and Mersey can answer it: the module graph
    /// is closed (§4.5), classes are sealed (§4.1), and there is no `eval`, so the
    /// set of classes is *known*. A JS engine has to guess and check; this does
    /// not. See `overridden_below`.
    all_classes: Vec<Rc<ClassDef>>,
    /// Calls before a function is offered to Tier 1, and back edges before a
    /// running loop is. Settable so a test — or the differential fuzzer — can make
    /// a program that runs *once* still reach Tier 1. A fuzzer that never crossed
    /// the threshold would be fuzzing the interpreter and reporting on the JIT.
    pub jit_threshold: u32,
    pub osr_threshold: u32,
    /// Class whose method is currently executing (innermost last), for `super`.
    class_stack: Vec<Rc<ClassDef>>,
    /// Call stack for diagnostics: (function name, module, position of the
    /// instruction currently executing in that frame).
    frames: Vec<Frame_>,
    /// Mersey call depth. See `MAX_CALL_DEPTH`.
    depth: usize,
    /// A graph paused on a module's top-level `await`.
    pending_graph: Option<PendingGraph>,
    /// Modules that are in the graph but have not been run: the targets of a
    /// dynamic `import(…)`. They were loaded, checked and locked with the rest
    /// (§4.5 — the graph is closed); they simply do not execute until someone
    /// imports them.
    lazy_modules: HashMap<String, &'static Module>,
    /// Web globals this host does not provide. Importing one is fine — most are
    /// interfaces, wanted only for their type — but *using* one as a value is
    /// the error, and this is what makes that error say why.
    absent_globals: HashSet<String>,
    /// Execute compiled bytecode where available (Tier 0); AST fallback
    /// otherwise. Off = pure tree-walking (differential-test oracle).
    pub use_vm: bool,
    /// Microtask queue (promise reactions + coroutine resumptions), drained
    /// before control returns to the host — the engine owns no event loop
    /// (embedding-api.md rule 1); the host owns timers and I/O.
    tasks: std::collections::VecDeque<Task>,
    all_cells: Vec<AllCell>,
    /// Member-name interning: a name crosses the ABI once, then it is an id.
    interned: HashMap<String, u32>,
    /// The handle of the page's `JSON` global, if imported — stringify/parse on
    /// it are served by the engine's own writer/parser, no bridge call at all.
    json_handle: i64,
    /// Evaluated modules: specifier → its exported bindings.
    modules: HashMap<String, HashMap<String, Value>>,
    /// The module currently executing (for relative import resolution).
    current_module: String,
    /// Mersey classes declared in the module being defined but not yet
    /// created, so `extends` can tell a late Mersey base from a host one.
    pending_class_names: HashSet<String>,
    /// A `gc.collect()` request from Mersey: honoured at the next safe point.
    gc_pending: bool,
    /// Tier 1: optional JIT backend (native builds register Cranelift).
    pub jit: Option<JitHook>,
    /// Owns every value Tier 1 code allocates, for the duration of one call.
    jit_arena: Arena,
    /// The last few strings parked for a shim, so a *constant* receiver is parked
    /// once rather than copied on every call. See `jit_box_str`.
    jit_str_memo: Vec<(usize, u64)>,
    /// A UTF-8 buffer reused across `parse.url` and its like, so converting an
    /// argument does not allocate a `String` per call. Held rather than made
    /// because a URL parsed in a loop is the shape that matters.
    utf8_scratch: Vec<u8>,
    /// The scope of the compiled code currently running, if any. `globals` is the
    /// scope of the module being *run*, which is the wrong one for compiled code
    /// belonging to any other module: its globals would not be found, the shim
    /// would answer "absent", and the compiled body would bail on every iteration —
    /// silently, and at exactly interpreted speed.
    jit_scope: Option<Env>,
    /// A thrown value stashed by a compiled host call that failed: the shim
    /// cannot unwind through compiled code, so it records the error and traps,
    /// and `after_jit` raises this at the trapping instruction's position.
    jit_host_error: Option<Thrown>,
    /// Keyed by (chunk, receiver class): the same method body compiled against
    /// two classes is two different compilations, because what its own calls
    /// resolve to need not be the same.
    jit_cache: HashMap<(usize, u64), Option<Rc<Compiled>>>,
    #[allow(dead_code)]
    call_counts: HashMap<usize, u32>,
}

/// An argument to a compiled function.
///
/// `Ptr` is a **borrowed** object or array — the address of a live `Rc`'s
/// contents, not a new reference to it. The caller owns the `Rc` and outlives
/// the call.
///
/// `Owned` is an object handed over *with* its arena handle: the arena owns a
/// reference to it, and compiled code may release that handle when it overwrites
/// the local holding it. Only on-stack replacement produces these — it enters a
/// function in the middle, where a local that would have held a compiled
/// allocation instead holds an interpreter value, so the value is cloned into
/// the arena to make the frame look the way compiled code left it.
#[derive(Clone, Copy)]
pub enum JitArg {
    I32(i32),
    I64(i64),
    F64(f64),
    /// An object or an array; 0 is `null`.
    Ptr(*const u8),
    /// An arena-owned object: its address, and its arena handle.
    Owned(*const u8, u64),
    /// An opaque engine value, by arena handle — there is no address to hand
    /// over, because compiled code never looks inside one. 0 is `null`. The
    /// handle is also the reference the compiled code owns and will release, so
    /// this arrives already parked (see `try_osr`).
    Val(u64),
}

/// Every reference-counted value Tier 1 code creates, owned in one place.
///
/// Compiled code does not hold `Rc`s — it holds addresses. When it allocates, the
/// engine keeps the actual `Rc` here and hands back the address plus a *handle*
/// naming this slot. Releasing the handle drops the reference (that is what keeps
/// a hot allocating loop from growing forever); anything never released is
/// dropped when the call ends, on **every** exit — a return, a bail, a trap —
/// because the interpreter clears the arena, not the compiled code. Nothing
/// compiled ever frees; it only lets go.
#[derive(Default)]
pub struct Arena {
    slots: Vec<Option<Value>>,
    free: Vec<usize>,
    /// The interpreter itself, valid only for the duration of one compiled
    /// call — set before entering compiled code and cleared after. Compiled
    /// code that makes a host call (a numeric builtin or web-method call)
    /// reaches the real host, the globals, and the interpreter's own web-call
    /// logic through this pointer, so the JIT is no longer confined to
    /// allocation-free, host-free bodies. Reentrant, like every host callback.
    pub(crate) interp: Option<*mut Interp>,
    /// The host's `web_bind` entry as a raw C function pointer plus its data, set
    /// alongside `interp` for the duration of one compiled call. When present,
    /// the typed-binding shim calls it directly instead of reentering the
    /// interpreter — the whole point of the fast path. Storing the pointers is
    /// safe; only the JIT calls through them.
    pub(crate) web_bind: Option<(WebBindFn, *mut core::ffi::c_void)>,
}

impl Arena {
    /// Own `v`; the returned handle names it. Handles start at 1 — 0 is
    /// "borrowed, nothing to release" everywhere in compiled code.
    pub fn keep(&mut self, v: Value) -> u64 {
        match self.free.pop() {
            Some(i) => {
                self.slots[i] = Some(v);
                (i + 1) as u64
            }
            None => {
                self.slots.push(Some(v));
                self.slots.len() as u64
            }
        }
    }

    pub fn release(&mut self, h: u64) {
        if h == 0 {
            return;
        }
        let i = (h - 1) as usize;
        if let Some(slot) = self.slots.get_mut(i) {
            if slot.take().is_some() {
                self.free.push(i);
            }
        }
    }

    /// Borrow what a handle names, for a shim that reads an argument without
    /// taking ownership of it.
    pub fn get(&self, h: u64) -> Option<&Value> {
        if h == 0 {
            return None;
        }
        self.slots.get((h - 1) as usize)?.as_ref()
    }

    /// Take the value out, keeping it alive: how a compiled result crosses back
    /// to the interpreter as an owned `Value`.
    pub fn take(&mut self, h: u64) -> Option<Value> {
        if h == 0 {
            return None;
        }
        let i = (h - 1) as usize;
        let v = self.slots.get_mut(i)?.take();
        if v.is_some() {
            self.free.push(i);
        }
        v
    }

    /// Lend the value out for the duration of one call, keeping the slot.
    ///
    /// A native that only *reads* its argument does not need a clone, and a clone
    /// of a reference-counted `Value` is an increment now and a trip through
    /// `Value`'s drop glue — a jump table over thirty-odd variants — at the end:
    /// together ~12% of a compiled `random.fill(buf)` iteration, for a value that
    /// was never going to outlive the call. Moving it out and back is two 16-byte
    /// copies and no reference counting.
    ///
    /// The slot is left holding `Value::Null` and is **not** freed, so the handle
    /// stays valid. Nothing may read that handle before `give_back` — which holds
    /// for `Interp::NATIVE_FAST`, whose members touch no arena.
    pub fn lend(&mut self, h: u64) -> Option<Value> {
        if h == 0 {
            return None;
        }
        let slot = self.slots.get_mut((h - 1) as usize)?;
        // Only a live slot is lent; a freed one stays freed rather than being
        // resurrected holding `null`.
        slot.is_some().then(|| slot.replace(Value::Null))?
    }

    /// Put back what `lend` moved out.
    pub fn give_back(&mut self, h: u64, v: Value) {
        if h == 0 {
            return;
        }
        if let Some(slot) = self.slots.get_mut((h - 1) as usize) {
            *slot = Some(v);
        }
    }

    /// The end of a compiled call, on every path: drop whatever it never let go.
    pub fn clear(&mut self) {
        self.slots.clear();
        self.free.clear();
    }

    /// The interpreter for the current compiled call, if one is set. The JIT's
    /// host-call shims read this; returning the raw pointer is safe, only
    /// dereferencing it (inside a shim, which the interpreter guarantees is
    /// reached during a live call) is not.
    pub fn interp_ptr(&self) -> Option<*mut Interp> {
        self.interp
    }

    /// The host's direct `web_bind` entry for this call, if any (see the field).
    pub fn web_bind_fn(&self) -> Option<(WebBindFn, *mut core::ffi::c_void)> {
        self.web_bind
    }
}

/// What a compiled function returned.
///
/// `Bail` means it could not run this call at all — the code is fine, the values
/// were not (an argument was not the type the kernel was compiled for). Nothing
/// has happened; the interpreter runs the call instead.
///
/// `Trap` means it ran, got as far as `pc` in function `func`, and hit a
/// condition the language says must throw: `x / 0`, an index out of bounds, a
/// field of `null`, the recursion limit. It carries **where**, so the interpreter
/// raises the error at the position it actually happened — rather than re-running
/// the call to find out, which was the old answer and is not available once
/// compiled code can write to the heap.
pub enum JitResult {
    I32(i32),
    I64(i64),
    F64(f64),
    /// A finished Mersey value — how a compiled *object* comes back: the call
    /// wrapper pulls it out of the arena before the arena is cleared.
    Val(Value),
    Null,
    Bail,
    Trap(Trap),
}

/// Why compiled code stopped, and where.
#[derive(Clone, Copy)]
pub struct Trap {
    pub reason: TrapReason,
    /// Which function of the compiled group, and the bytecode position in it.
    pub func: usize,
    pub pc: usize,
    /// Detail for the message: the index and the length, for a bounds trap.
    pub a: i64,
    pub b: i64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TrapReason {
    DivZero,
    IntMinOverflow,
    Depth,
    Bounds,
    /// A field or element of `null`.
    NullAccess,
    /// A heap cell did not hold what its declared type says it holds. The type
    /// system says this cannot happen; compiled code checks anyway, because the
    /// alternative to checking is reading an integer as a pointer.
    BadTag,
    /// A compiled host call threw. The value is stashed on the interpreter
    /// (`jit_host_error`), because a shim cannot unwind through native frames.
    HostError,
}

/// What one value at the Tier 1 boundary is.
#[derive(Clone)]
pub enum JitSlot {
    I32,
    I64,
    F64,
    /// An instance of this class, or of a subclass, or `null`.
    Obj(Rc<ClassDef>),
    /// An array of these.
    Arr(Rc<FieldTy>),
    /// A UTF-16 string, or `null`.
    Str,
    /// A host-object handle (`JsRef`): one word, the handle id.
    Web,
    /// A nullable `int32`: one register, `i64::MIN` for null.
    NumOpt,
    /// Any other engine value, carried opaquely by arena handle — a `Bytes`, a
    /// `Url`, whatever a `std:` native hands back. Compiled code never looks
    /// inside one; it passes it to a shim, which is enough to keep a loop
    /// containing a native call in compiled code instead of surrendering the
    /// whole function to the interpreter.
    Val,
}

/// The scope a function's free names resolve in, carried opaquely.
///
/// Tier 1 has to ask what a name binds, and the honest answer depends on *where
/// the function was written*. `Interp::globals` is not that: it is swapped to the
/// module currently being run, so by the time a function gets hot it names the
/// entry module's scope. For anything defined elsewhere — every module in the
/// standard library — a `std:` import was therefore invisible, and the function
/// was refused on its first `LoadName`.
///
/// Opaque because the JIT only carries it back; `Scope` is the interpreter's
/// business. A compiled chunk has `needs_env == false`, so no *local* lives in a
/// scope: every name this resolves is one from outside the function, which is why
/// a call-time scope answers the same as the defining one.
#[derive(Clone)]
pub struct DefScope(Env);

impl DefScope {
    /// The scope itself. Crate-internal: outside it this is an opaque token.
    pub(crate) fn env(&self) -> Env {
        self.0.clone()
    }
}

/// What a free name binds, as far as Tier 1 cares.
pub enum NameKind {
    /// A `std:` namespace whose members go through the native shim.
    StdNs(&'static str),
    /// `std:time`, which has its own lowering.
    TimeNs,
    /// `std:math`, whose members lower to machine intrinsics.
    MathNs,
    /// An engine value carried opaquely by arena handle — a `Bytes`, a `Url`.
    Opaque,
    /// A binding holding a number or a bool: 0 an `int32`, 1 an `int64`, 2 a
    /// `float64`, 3 a `bool`. The kind is fixed by the checker — a binding has one
    /// type — so reading it once at compile time to decide the register is sound
    /// even though the *value* is read live.
    NumGlobal(u8),
    /// A binding holding a string. Compiled code takes it as the three registers
    /// any other string uses, so its methods and `==` work on it — which a
    /// module-level `const` needs, that being how a lookup table is written.
    StrGlobal,
    /// A binding currently holding a host object handle.
    Web,
    /// Anything else, including a name that does not resolve here.
    Other,
}

/// One function in a compiled group: its bytecode, and everything about its
/// signature that compiled code has to know before it compiles the body.
#[derive(Clone)]
pub struct JitFn {
    pub chunk: Rc<vm::Chunk>,
    pub params: Vec<String>,
    /// The declared type of each parameter. A parameter's type cannot be inferred
    /// from the body — the values come from outside it — so this is the only
    /// place an object parameter's class can come from.
    pub param_tys: Vec<Option<JitSlot>>,
    /// The class of `this`, for a method. `None` for a plain function.
    pub this: Option<Rc<ClassDef>>,
    /// The numeric type this function is declared to return. Recursion means a
    /// call cannot wait for the callee to be compiled to find out.
    pub ret: Option<mersey_front::check::Num>,
    /// …or `bool`.
    pub ret_bool: bool,
    /// …or an instance of this class (or a subclass, or null).
    pub ret_obj: Option<Rc<ClassDef>>,
    /// …or a string (or null). A string leaves the way an object does, by the
    /// handle of the arena slot that owns it — see the `Ty::Str` arm of the call
    /// wrapper's result marshalling.
    pub ret_str: bool,
    /// …or an engine primitive (or null): a `Bytes`, a `Url`. Which is what every
    /// `decode` in the standard library gives back.
    pub ret_val: bool,
    /// …or a *nullable* `int32`, which is one register with `i64::MIN` for null.
    /// Distinct from `ret` because the shape is not a number's: `int32?` and
    /// `int32` cross the boundary differently, and a function that returns the
    /// first had no describable signature at all until it did — which is why
    /// every `parse`-shaped function in the standard library was refused, along
    /// with each of its callers.
    pub ret_numopt: bool,
    /// The global binding this came from, if it came from one. Compiled code
    /// calls the function a name meant *when it was compiled*; if the name is
    /// later repointed (`f = g`), the code is discarded. A method has no binding:
    /// a class's method set cannot change (§4.1).
    pub bind: Option<(String, Rc<Closure>)>,
    /// Where this function's free names resolve. See `DefScope`.
    pub scope: Option<DefScope>,
    /// The declared return type, exactly as written — carried only so the trace
    /// can tell the two refusals apart. A `Return` in a signature that promises
    /// nothing and a `Return` of an unrepresentable value print the same line
    /// otherwise, and they are different bugs: the first is a type this tier has
    /// no shape for, the second is a value it could not produce. It is a
    /// `'static` reference, so carrying it costs nothing.
    pub ret_ty: Option<&'static TypeExpr>,
}

/// The numeric world a compiled function returns in.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum JitKind {
    I32,
    I64,
    F64,
}

/// What the compiler may ask the engine while it decides what it can compile.
///
/// The backend drives this, not the interpreter, and it has to: whether `o.m()`
/// is compilable depends on what `o` is, and working *that* out means propagating
/// types through the bytecode — which is the compiler's job and nobody else's.
pub trait JitEnv {
    /// A top-level function by name, if compiled code could call it directly.
    fn function(&self, scope: Option<&DefScope>, name: &str) -> Option<JitFn>;

    /// The method `name` on a receiver of class `cls`, if the call can be made
    /// **directly** — which needs `cls` to be the last word on what `m` means. See
    /// `Interp::overridden_below`.
    fn method(&self, cls: &Rc<ClassDef>, name: &str) -> Option<JitFn>;

    /// …and a static one, called on the class rather than on an instance.
    fn static_method(&self, cls: &Rc<ClassDef>, name: &str) -> Option<JitFn>;

    /// `Error`, `TypeError`, `RangeError` — a builtin error class by name, and
    /// only those. `throw new Error(msg)` is lowered by building the error *here*
    /// and trapping, so the compiled code never has to make one; that is only
    /// sound for the classes `Interp::throw` knows how to build.
    fn error_class(&self, scope: Option<&DefScope>, name: &str) -> Option<&'static str>;

    /// `new Map()` / `new Set()` — 1 and 2, matching `jit_array_new`'s `kind`.
    /// The builtin containers are the names that bind *nothing*, which is the
    /// same test `new_is_web` uses: a program that defines its own `Map` gets its
    /// own, and this declines.
    fn container_kind(&self, scope: Option<&DefScope>, name: &str) -> Option<i64>;

    /// The class a top-level name binds, if it binds one.
    fn class_named(&self, scope: Option<&DefScope>, name: &str) -> Option<Rc<ClassDef>>;
    /// The body behind `o.name` when `name` is a getter. A getter is a call
    /// wearing a field's clothes, so Tier-1 compiles it as the zero-argument
    /// method it is; without this the whole enclosing function fell back to the
    /// interpreter the moment it read one.
    fn getter(&self, cls: &Rc<ClassDef>, name: &str) -> Option<JitFn>;

    /// How many classes exist. Compiled code's dispatch is only static as long as
    /// the hierarchy it was compiled against is the whole hierarchy — and a
    /// dynamic `import()` can add to it.
    fn n_classes(&self) -> usize;

    /// The class `new name(…)` constructs, if compiled code can construct it:
    /// its field initializers all fold (no expressions to evaluate), it is not
    /// host-backed, and it is not one of the built-in error classes. The
    /// constructor body, if there is one, is compiled like any method.
    fn class_for_new(&self, name: &str) -> Option<Rc<ClassDef>>;

    /// A class's constructor as a compilable function, `None` if it has none
    /// (then `new` takes no arguments and only the field defaults run).
    fn ctor(&self, cls: &Rc<ClassDef>) -> Option<Option<JitFn>>;

    /// The `std:` namespace `name` binds at the top level, if it is one whose
    /// members compiled code can call through `native_call` — the general
    /// escape hatch, as opposed to `std:math`/`std:time`, which lower to
    /// instructions and a numeric shim respectively.
    /// What `name` binds in `scope` — the scope the function being compiled was
    /// defined in, or the current globals when there is none. This replaced four
    /// separate predicates that each asked `Interp::globals`; see `DefScope` for
    /// why that was the wrong place to ask.
    fn name_kind(&self, scope: Option<&DefScope>, name: &str) -> NameKind;

    /// True if `new name(...)` would go to the host constructor path (`web_new`)
    /// rather than instantiate a Mersey class — i.e. `name` is not a plain class
    /// binding, is not a namespaced path, and is not the `Map`/`Set` builtin.
    /// Mirrors `new_named` exactly so the compiled `new` throws or succeeds where
    /// the interpreter would.
    fn new_is_web(&self, name: &str) -> bool;

    /// The interned id a web method name *already* has, if any. By the time a
    /// web loop compiles it has run thousands of interpreted iterations, each of
    /// which interned its method names — so the id is known here, read-only, and
    /// compiled code can carry it instead of interning on every call. Returns
    /// `None` for a name never yet interned (then the shim interns lazily).
    fn interned_web(&self, name: &str) -> Option<u32>;
}

/// Native code for one root function *and everything it calls*.
///
/// A call graph is compiled as a unit rather than one function at a time: a
/// compiled function that had to return to the interpreter to make a call
/// would pay for the transition twice per call, which is most of what the
/// call costs. Compiled code calls compiled code directly.
pub struct JitCode {
    pub kind: JitKind,
    /// Where this code's free names resolve — the scope its root was written in.
    /// The interpreter makes it current for the duration of a compiled call, so
    /// the shims that read a global read the right one. See `DefScope`.
    pub scope: Option<DefScope>,
    /// Enter the root at the top, with its arguments. The arena owns everything
    /// the call allocates; the interpreter clears it when the call ends.
    #[allow(clippy::type_complexity)]
    pub call: Box<dyn Fn(&[JitArg], &mut Arena) -> JitResult>,
    /// Re-enter the root *at a loop header*, carrying the current value of
    /// every local (on-stack replacement). This is what lets a loop inside a
    /// function that is only ever called once reach Tier 1 at all.
    #[allow(clippy::type_complexity)]
    pub osr: Box<dyn Fn(&[JitArg], usize, &mut Arena) -> JitResult>,
    /// Frame size. The compiled code uses the *same* frame the interpreter does,
    /// with the same slot numbers — which is what makes on-stack replacement a
    /// copy of a `Vec` rather than a search for each local by name.
    pub n_slots: usize,
    /// What each slot holds. The frame is no longer all one type — that was the
    /// whole limitation — so marshalling it has to go slot by slot.
    pub slot_kinds: Vec<JitSlot>,
    /// Bytecode positions of the loop headers `osr` can resume at.
    pub osr_entries: Vec<usize>,
    /// Where the root's `this` lives, for a method. It is a frame slot like any
    /// other, and it comes *after* the parameters — so `call` takes the arguments
    /// in order and then, if there is one, the receiver.
    pub this_slot: Option<usize>,
    /// The chunks of the group, in the order a `Trap`'s `func` indexes them — so
    /// a trap's `pc` can be turned back into a line and a column.
    pub chunks: Vec<Rc<vm::Chunk>>,
    /// Which slots hold objects the compiled code *owns* (arena handles), so an
    /// on-stack replacement knows which locals to clone into the arena.
    pub owned_slots: Vec<bool>,
    /// The global bindings this code was compiled against.
    pub bound: Vec<(String, Rc<Closure>)>,
    /// The classes it constructs or reaches into. Held so the raw class pointers
    /// baked into the code stay valid for exactly as long as the code does.
    pub classes: Vec<Rc<ClassDef>>,
    /// The size of the class hierarchy it was compiled against.
    pub n_classes: usize,
}

/// Backend entry: compile `root` and everything it calls. `None` = outside the
/// subset Tier 1 can take.
pub type JitHook = fn(&dyn JitEnv, &JitFn) -> Option<Rc<JitCode>>;

/// Compiled code, plus the bindings it was compiled against.
///
/// Compiled code calls a callee's *chunk* directly, which is only correct while
/// the global name still refers to the function it named at compile time — and
/// a function declaration is an ordinary binding that can be reassigned
/// (`f = g`). Nothing reassigns it while the kernel runs (compiled code cannot
/// write a global), so checking at the entry is enough: if a binding moved, the
/// code is discarded and the call is interpreted.
struct Compiled {
    code: Rc<JitCode>,
}

/// The engine, as the compiler is allowed to see it.
struct InterpEnv<'a> {
    i: &'a Interp,
}

impl JitEnv for InterpEnv<'_> {
    fn function(&self, scope: Option<&DefScope>, name: &str) -> Option<JitFn> {
        self.i.top_level_fn(scope.map(|s| &s.0), name)
    }
    fn method(&self, cls: &Rc<ClassDef>, name: &str) -> Option<JitFn> {
        self.i.direct_method(cls, name)
    }

    fn static_method(&self, cls: &Rc<ClassDef>, name: &str) -> Option<JitFn> {
        self.i.direct_static(cls, name)
    }

    fn error_class(&self, scope: Option<&DefScope>, name: &str) -> Option<&'static str> {
        const KNOWN: &[&str] = &["Error", "TypeError", "RangeError"];
        let env = scope.map_or(&self.i.globals, |s| &s.0);
        let matched = KNOWN.iter().copied().find(|k| *k == name)?;
        // …and it must still *be* that class here, not a rebinding of the name.
        match env_get(env, name) {
            Some(Value::Class(c)) if c.is_builtin_error => Some(matched),
            _ => None,
        }
    }

    fn container_kind(&self, scope: Option<&DefScope>, name: &str) -> Option<i64> {
        let env = scope.map_or(&self.i.globals, |s| &s.0);
        if env_get(env, name).is_some() {
            return None;
        }
        match name {
            "Map" => Some(1),
            "Set" => Some(2),
            _ => None,
        }
    }

    fn class_named(&self, scope: Option<&DefScope>, name: &str) -> Option<Rc<ClassDef>> {
        let env = scope.map_or(&self.i.globals, |s| &s.0);
        match env_get(env, name) {
            Some(Value::Class(c)) => Some(c),
            _ => None,
        }
    }
    fn getter(&self, cls: &Rc<ClassDef>, name: &str) -> Option<JitFn> {
        self.i.direct_getter(cls, name)
    }
    fn n_classes(&self) -> usize {
        self.i.all_classes.len()
    }
    fn class_for_new(&self, name: &str) -> Option<Rc<ClassDef>> {
        let Some(Value::Class(cls)) = env_get(&self.i.globals, name) else {
            return None;
        };
        // Field initializers that compute need an evaluator, and the shim that
        // allocates for compiled code has none. Host-backed and built-in error
        // classes construct through machinery of their own.
        if !cls.dynamic_inits.is_empty() || cls.is_host_backed() || cls.is_builtin_error {
            return None;
        }
        Some(cls)
    }
    fn name_kind(&self, scope: Option<&DefScope>, name: &str) -> NameKind {
        // The function's own scope when it has one, and only otherwise the
        // globals — which are the right answer exactly when the function was
        // written in the module being run.
        let env = scope.map_or(&self.i.globals, |s| &s.0);
        match env_get(env, name) {
            Some(Value::Namespace(ns)) => {
                // `math` and `time` have their own lowerings; everything else
                // goes through the general native shim.
                if ns.name == "time" {
                    return NameKind::TimeNs;
                }
                if ns.name == "math" {
                    return NameKind::MathNs;
                }
                match Interp::NATIVE_NS.iter().copied().find(|n| *n == ns.name) {
                    Some(n) => NameKind::StdNs(n),
                    None => NameKind::Other,
                }
            }
            Some(Value::Bytes(_) | Value::UrlV(_) | Value::RegexV(_)) => NameKind::Opaque,
            Some(Value::Str(_)) => NameKind::StrGlobal,
            Some(Value::I32(_)) => NameKind::NumGlobal(0),
            Some(Value::I64(_)) => NameKind::NumGlobal(1),
            Some(Value::F64(_)) => NameKind::NumGlobal(2),
            Some(Value::Bool(_)) => NameKind::NumGlobal(3),
            Some(Value::JsRef(_)) => NameKind::Web,
            _ => NameKind::Other,
        }
    }
    fn new_is_web(&self, name: &str) -> bool {
        // A namespaced `new geo.Point(…)` resolves through an import; leave it to
        // the interpreter.
        if name.contains('.') {
            return false;
        }
        // `Map`/`Set` with no binding are the builtin containers, not host `new`.
        if (name == "Map" || name == "Set") && env_get(&self.i.globals, name).is_none() {
            return false;
        }
        // Anything not bound to a Mersey class goes to `web_new` (a bound URL,
        // WebSocket, Uint8Array, or an unbound name the host may still know).
        !matches!(env_get(&self.i.globals, name), Some(Value::Class(_)))
    }
    fn interned_web(&self, name: &str) -> Option<u32> {
        match self.i.interned.get(name) {
            Some(&id) if id != u32::MAX => Some(id),
            _ => None,
        }
    }
    fn ctor(&self, cls: &Rc<ClassDef>) -> Option<Option<JitFn>> {
        let Some(data) = cls.ctor_data() else {
            // No constructor anywhere in the chain: `new` is just the defaults.
            return Some(None);
        };
        if data.is_async {
            return None;
        }
        // A constructor is compiled on first *use*; a hot `new` may arrive first.
        if data.chunk.borrow().is_none() {
            let module = self.i.current_module.clone();
            let out = vm::compile_fn_in(&data.body, &module, data.params);
            *data.chunk.borrow_mut() = Some(out);
        }
        let chunk = data.chunk.borrow().clone()??;
        if chunk.yields || chunk.needs_env || !chunk.simple_params {
            return None;
        }
        // A method's free names resolve where its *class* was written.
        let scope = cls.env.clone().map(DefScope);
        Some(Some(JitFn {
            params: simple_param_names(data.params)?,
            param_tys: self.i.param_types(cls.env.as_ref(), data.params),
            chunk,
            this: Some(cls.clone()),
            ret: None,
            ret_bool: false,
            ret_obj: None,
            ret_str: false,
            ret_val: false,
            ret_numopt: false,
            bind: None,
            scope,
            ret_ty: data.ret_ty(),
        }))
    }
}

/// Parameter names, if every parameter is a plain name: no destructuring, no
/// rest, no default. Anything else is bound by machinery the kernel does not
/// have, so the function stays interpreted.
fn simple_param_names(params: &[Param]) -> Option<Vec<String>> {
    params
        .iter()
        .map(|p| match (&p.target, p.rest, &p.default) {
            (Pattern::Name(n), false, None) => Some(n.text.clone()),
            _ => None,
        })
        .collect()
}

/// The numeric type a declared type *name* stands for.
fn num_of_name(name: &str) -> Option<mersey_front::check::Num> {
    use mersey_front::check::{IntKind, Num};
    Some(match name {
        "int8" => Num::Int(IntKind::I8),
        "int16" => Num::Int(IntKind::I16),
        "int32" => Num::Int(IntKind::I32),
        "int64" => Num::Int(IntKind::I64),
        "uint8" => Num::Int(IntKind::U8),
        "uint16" => Num::Int(IntKind::U16),
        "uint32" => Num::Int(IntKind::U32),
        "uint64" => Num::Int(IntKind::U64),
        "float32" => Num::F32,
        "float64" => Num::F64,
        _ => return None,
    })
}

/// The value an uninitialized binding or field starts with: its type's zero.
///
/// Numbers are 0 at their declared width, `string` is `""`, `char` is `'\0'`,
/// `bool` is `false`, containers are empty. The *kind* comes from the checker
/// ([`check::default_for_ty`]) — only the checker can see through a type alias —
/// and this is where it becomes a value.
///
/// Containers are freshly allocated on every call, never shared: an empty array
/// stored in `initial_slots` would be *one* array handed to every instance, and
/// the first push would prove it.
pub(crate) fn default_value(d: check::DefaultVal) -> Value {
    use check::DefaultVal as D;
    match d {
        D::Num(n) => match n {
            check::Num::F32 => Value::F32(0.0),
            check::Num::F64 => Value::F64(0.0),
            check::Num::Int(check::IntKind::I64) => Value::I64(0),
            check::Num::Int(check::IntKind::U32) => Value::U32(0),
            check::Num::Int(check::IntKind::U64) => Value::U64(0),
            // The kinds below 32 bits are carried as int32 (§3.3).
            check::Num::Int(_) => Value::I32(0),
        },
        D::BigInt => Value::BigIntV(Rc::new(BigInt::zero())),
        D::BigDec => Value::BigDecV(Rc::new(BigDec::parse("0").expect("zero"))),
        D::Str => Value::Str(Rc::new(Vec::new())),
        D::Char => Value::Char('\0'),
        D::Bool => Value::Bool(false),
        D::Array => new_array(Vec::new()),
        D::Map => new_map(Vec::new()),
        D::Set => new_set(Vec::new()),
        D::Bytes => Value::Bytes(Rc::new(RefCell::new(Vec::new()))),
    }
}

/// Is this default an immutable value, safe to precompute once and share?
/// A container is not: sharing one is aliasing, not defaulting.
pub(crate) fn default_is_shareable(d: check::DefaultVal) -> bool {
    use check::DefaultVal as D;
    !matches!(d, D::Array | D::Map | D::Set | D::Bytes)
}

/// The slots a fresh instance starts with, the initializers that have to run,
/// and the container defaults that must be allocated per instance.
/// See `ClassDef::initial_slots`.
#[allow(clippy::type_complexity)]
fn fold_field_inits(
    fields: &[(String, Option<&'static Expr>)],
    tyexprs: &[Option<&'static TypeExpr>],
) -> (
    Vec<Value>,
    Vec<(usize, &'static Expr)>,
    Vec<(usize, check::DefaultVal)>,
) {
    let mut slots = Vec::with_capacity(fields.len());
    let mut dynamic = Vec::new();
    let mut containers = Vec::new();
    for (slot, (_, init)) in fields.iter().enumerate() {
        match init {
            Some(e @ Expr::Lit { kind, text, .. }) => match parse_literal(*kind, text) {
                Ok(v) => slots.push(v),
                // A literal the engine cannot build (out of range at its declared
                // type) still has to be *evaluated*, so it can throw where it is
                // written rather than where it is used.
                Err(_) => {
                    slots.push(Value::Null);
                    dynamic.push((slot, *e));
                }
            },
            Some(e) => {
                slots.push(Value::Null);
                dynamic.push((slot, *e));
            }
            // No initializer: the field starts at its type's zero, not at null.
            None => match tyexprs
                .get(slot)
                .copied()
                .flatten()
                .and_then(check::default_for_ty)
            {
                Some(d) if default_is_shareable(d) => slots.push(default_value(d)),
                Some(d) => {
                    slots.push(Value::Null);
                    containers.push((slot, d));
                }
                // No default (a class type, an interface): null, honestly.
                None => slots.push(Value::Null),
            },
        }
    }
    (slots, dynamic, containers)
}

/// What the interpreter would have said, had it been the one to reach through the
/// null. Compiled code stopped at an instruction; the instruction says which of
/// the three it was.
fn null_access_message(chunk: &vm::Chunk, pc: usize) -> String {
    let name = |ni: u16| chunk.names[ni as usize].clone();
    match chunk.code.get(pc) {
        Some(vm::Op::GetMember(ni, _)) => format!("no member `{}` on null", name(*ni)),
        Some(vm::Op::CallMethod(ni, _)) => format!("no method `{}` on null", name(*ni)),
        Some(vm::Op::SetMember(_, _)) => "cannot assign to a member of this value".to_string(),
        Some(vm::Op::IndexGet) => "only arrays and strings are indexable".to_string(),
        Some(vm::Op::IndexSet) => "only array elements can be assigned by index".to_string(),
        _ => "no member on null".to_string(),
    }
}

/// A runtime value as an argument to compiled code — but only if it is what that
/// slot was compiled to hold. This is the entry guard: it is what makes the
/// compiled code deopt-free, because inside it every value's type is known.
///
/// An object passes if it is an instance of the class the code expects **or of a
/// subclass**. That is not a concession; it is the point. A subclass's fields
/// begin with its base's (§4.1), so every offset the code computed is still the
/// right offset, and every method it resolved is still the right method (nothing
/// below the class overrides it, or the code would not exist). A `Shape[]` full
/// of `Circle`s and `Square`s runs the compiled code.
fn jit_arg(v: &Value, slot: &JitSlot) -> Option<JitArg> {
    match (v, slot) {
        (Value::I32(n), JitSlot::I32) => Some(JitArg::I32(*n)),
        (Value::I64(n), JitSlot::I64) => Some(JitArg::I64(*n)),
        (Value::F64(n), JitSlot::F64) => Some(JitArg::F64(*n)),
        // A `bool` lives in an integer register — a comparison produces one — and
        // this is the edge where it goes back to being a value.
        (Value::Bool(t), JitSlot::I32) => Some(JitArg::I32(*t as i32)),
        (Value::Instance(i), JitSlot::Obj(cls)) => {
            let ok = i.try_borrow()?.class.descends_from(cls);
            ok.then_some(JitArg::Ptr(Rc::as_ptr(i) as *const u8))
        }
        (Value::Array(a), JitSlot::Arr(_)) => Some(JitArg::Ptr(Rc::as_ptr(a) as *const u8)),
        // A string crosses as the address of its `Rc<Vec<u16>>` contents; the
        // entry wrapper derives the data pointer and length from it, as it does
        // an object's fields. A borrow — the caller (or the OSR clone) owns it.
        (Value::Str(s), JitSlot::Str) => Some(JitArg::Ptr(Rc::as_ptr(s) as *const u8)),
        // A nullable number crosses as its value, or the sentinel.
        (Value::I32(n), JitSlot::NumOpt) => Some(JitArg::I64(*n as i64)),
        (Value::Null, JitSlot::NumOpt) => Some(JitArg::I64(i64::MIN)),
        // A host handle is one word — the id itself. A null handle is 0.
        (Value::JsRef(h), JitSlot::Web) => Some(JitArg::I64(*h)),
        (Value::Null, JitSlot::Web) => Some(JitArg::I64(0)),
        // A null object, array, or string is a null pointer, which compiled code
        // checks before it dereferences — exactly as the interpreter does.
        (Value::Null, JitSlot::Obj(_) | JitSlot::Arr(_) | JitSlot::Str) => {
            Some(JitArg::Ptr(std::ptr::null()))
        }
        _ => None,
    }
}

/// What compiled code returned, as a Mersey value — or handed back whole, when
/// it did not produce one (a bail, a trap) and the caller must deal with that.
fn jit_value(r: JitResult, ret_bool: bool) -> Result<Value, JitResult> {
    match r {
        // In an integer kernel a `bool` *is* an i32: a comparison yields 0 or 1
        // and flows through the same slots as every other value. Only the
        // declared type knows it was a bool, and only at the boundary does the
        // difference become visible — so this is where it is put back.
        JitResult::I32(v) if ret_bool => Ok(Value::Bool(v != 0)),
        JitResult::I32(v) => Ok(Value::I32(v)),
        JitResult::I64(v) => Ok(Value::I64(v)),
        JitResult::F64(v) => Ok(Value::F64(v)),
        JitResult::Val(v) => Ok(v),
        JitResult::Null => Ok(Value::Null),
        other => Err(other),
    }
}

/// Calls before a function is considered hot (Tier 1 threshold).
const JIT_THRESHOLD: u32 = 64;

/// Loop iterations before the *containing* function is compiled and re-entered
/// at the loop header. A function called once around a long loop never becomes
/// hot by call count — the counter only ever reaches 1 — so the loop's own
/// back edge is what has to trigger it.
///
/// Low enough that a warm-up pass (the conventional `work(1000)` before a timed
/// run) compiles the function, so the timed run enters it already compiled from
/// the top (see `try_osr`, which marks the chunk hot). 500 iterations is still a
/// genuinely hot loop — worth the one-time Cranelift compile.
const OSR_THRESHOLD: u32 = 500;

/// How deep compiled code may recurse before handing the call back. The
/// interpreter's own limit (`MAX_CALL_DEPTH`) then raises the `RangeError`
/// with a stack trace, exactly as it would have without a JIT.
pub const JIT_DEPTH_LIMIT: i64 = MAX_CALL_DEPTH as i64;

pub fn new_interp(host: Box<dyn Host>) -> Interp {
    let root = Rc::new(GcCell::new(Scope {
        vars: HashMap::default(),
        parent: None,
    }));
    let globals = child_env(&root);
    let mut error_classes = HashMap::default();
    let base = Rc::new(builtin_error_class("Error", None));
    for name in ["RangeError", "TypeError"] {
        error_classes.insert(name, Rc::new(builtin_error_class(name, Some(base.clone()))));
    }
    error_classes.insert("Error", base);
    for (name, cls) in &error_classes {
        env_define(&root, name, Value::Class(cls.clone()));
    }
    Interp {
        host,
        free_callbacks: Vec::new(),
        callback_ids: HashMap::default(),
        root,
        globals,
        callbacks: Vec::new(),
        debug_hook: None,
        debug_envs: Vec::new(),
        error_classes,
        all_classes: Vec::new(),
        jit_threshold: JIT_THRESHOLD,
        osr_threshold: OSR_THRESHOLD,
        class_stack: Vec::new(),
        frames: Vec::new(),
        depth: 0,
        pending_graph: None,
        lazy_modules: HashMap::default(),
        absent_globals: HashSet::default(),
        use_vm: true,
        tasks: std::collections::VecDeque::new(),
        all_cells: Vec::new(),
        interned: HashMap::default(),
        json_handle: -1,
        modules: HashMap::default(),
        current_module: String::new(),
        pending_class_names: HashSet::default(),
        gc_pending: false,
        jit: None,
        jit_arena: Arena::default(),
        jit_str_memo: Vec::new(),
        utf8_scratch: Vec::new(),
        jit_scope: None,
        jit_host_error: None,
        jit_cache: HashMap::default(),
        call_counts: HashMap::default(),
    }
}

thread_local! {
    static NEXT_CLASS_ID: std::cell::Cell<u64> = const { std::cell::Cell::new(1) };
}

fn fresh_class_id() -> u64 {
    NEXT_CLASS_ID.with(|c| {
        let id = c.get();
        c.set(id + 1);
        id
    })
}

impl ClassDef {
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    /// The constant offset of `name` in this class's instances, if it is a
    /// declared field.
    pub(crate) fn slot_of(&self, name: &str) -> Option<u32> {
        self.field_slots.get(name).copied()
    }

    pub fn class_id(&self) -> u64 {
        self.id
    }

    pub fn field_slot(&self, name: &str) -> Option<u32> {
        self.field_slots.get(name).copied()
    }

    pub fn n_fields(&self) -> usize {
        self.fields.len()
    }

    /// Is `other` this class, or one of its ancestors? Which is the same question
    /// as "are this class's field offsets valid on an instance of `other`" — and
    /// they are, because a subclass's layout begins with its base's.
    pub fn descends_from(&self, other: &ClassDef) -> bool {
        let mut c = Some(self);
        while let Some(k) = c {
            if k.id == other.id {
                return true;
            }
            c = k.parent.as_deref();
        }
        false
    }

    /// Every field's type, resolved. Worked out once, on the first Tier 1 compile
    /// that needs it — not at class-definition time, when the classes a field
    /// names may not exist yet (and one of them may be this very class).
    pub fn field_types(&self) -> Rc<Vec<FieldTy>> {
        if let Some(t) = self.field_tys.borrow().as_ref() {
            return t.clone();
        }
        // Placed *before* resolving, so a class whose field names itself does not
        // recurse forever: the inner resolution finds this empty answer, and only
        // needs the class, not its fields.
        let env = self.env.clone();
        let tys: Vec<FieldTy> = self
            .field_tyexprs
            .iter()
            .map(|t| match (t, &env) {
                (Some(t), Some(env)) => resolve_field_ty(t, env),
                _ => FieldTy::Opaque,
            })
            .collect();
        let tys = Rc::new(tys);
        *self.field_tys.borrow_mut() = Some(tys.clone());
        tys
    }

    /// The method `name` resolves to on this class, and the class that declares
    /// it (which is where its `super` and its private fields are relative to).
    pub fn lookup_method(&self, name: &str) -> Option<Rc<FnData>> {
        let mut c = Some(self);
        while let Some(k) = c {
            if let Some(m) = k.methods.get(name) {
                return Some(m.clone());
            }
            c = k.parent.as_deref();
        }
        None
    }

    /// The getter body for `name`, walking the chain like `lookup_method`.
    pub fn lookup_getter(&self, name: &str) -> Option<Rc<FnData>> {
        let mut c = Some(self);
        while let Some(k) = c {
            if let Some(g) = k.getters.get(name) {
                return Some(g.clone());
            }
            c = k.parent.as_deref();
        }
        None
    }

    /// Does this class declare a getter or setter for `name` itself? A subclass
    /// that re-declares either one takes over what `o.name` means, so a direct
    /// call compiled against the base would run the wrong body.
    pub fn declares_accessor(&self, name: &str) -> bool {
        self.getters.contains_key(name) || self.setters.contains_key(name)
    }

    /// Does this class declare `name` itself (as opposed to inheriting it)?
    pub fn declares_method(&self, name: &str) -> bool {
        self.methods.contains_key(name)
    }

    /// Is `name` a getter, a setter, or a static — anything that makes `o.name`
    /// or `o.name()` mean something other than a field load or a method call?
    pub fn is_accessor(&self, name: &str) -> bool {
        let mut c = Some(self);
        while let Some(k) = c {
            if k.getters.contains_key(name) || k.setters.contains_key(name) {
                return true;
            }
            c = k.parent.as_deref();
        }
        false
    }

    pub fn is_host_backed(&self) -> bool {
        self.host_iface.is_some()
    }

    pub fn is_builtin_error_class(&self) -> bool {
        self.is_builtin_error
    }

    /// The constructor that runs for `new`, walking up to the base class exactly
    /// as `instantiate` does.
    pub fn ctor_data(&self) -> Option<Rc<FnData>> {
        let mut c = Some(self);
        while let Some(k) = c {
            if let Some(f) = &k.ctor {
                return Some(f.clone());
            }
            c = k.parent.as_deref();
        }
        None
    }
}

/// A field's declared type, as a thing Tier 1 can reason about. Anything it has
/// no register for — a string, a record, a generic, a nullable number — is
/// `Opaque`, which is a decision to interpret, not a failure.
fn resolve_field_ty(t: &TypeExpr, env: &Env) -> FieldTy {
    match t {
        TypeExpr::Named { name, args, .. } if args.is_empty() => {
            if name == "bool" {
                return FieldTy::Bool;
            }
            if let Some(n) = num_of_name(name) {
                return FieldTy::Num(n);
            }
            if name == "string" {
                return FieldTy::Str;
            }
            // The engine primitives compiled code carries opaquely. They are
            // prelude *classes* to the binder, but they have no Mersey layout —
            // there is nothing to look inside — so an arena handle is the whole
            // representation, exactly as for a native's result.
            if matches!(name.as_str(), "Bytes" | "Url" | "Regex") {
                return FieldTy::Val;
            }
            match env_get(env, name) {
                Some(Value::Class(c)) => FieldTy::Obj(c),
                _ => FieldTy::Opaque,
            }
        }
        // `Map<K, V>` / `Set<T>` — containers this tier carries as opaques, by
        // arena handle, the same way it carries one built in compiled code. The
        // builtin ones are the names that bind nothing, which is the test the
        // rest of the engine uses; a program with its own generic `Map` falls
        // through to `Opaque` and interprets.
        TypeExpr::Named { name, .. }
            if (name == "Map" || name == "Set") && env_get(env, name).is_none() =>
        {
            FieldTy::Val
        }
        TypeExpr::ArrayOf(e) => match resolve_field_ty(e, env) {
            FieldTy::Opaque => FieldTy::Opaque,
            inner => FieldTy::Arr(Rc::new(inner)),
        },
        // `Node?` holds a `Node` or `null`, and compiled code represents `null`
        // as a null pointer — so a nullable *object* is the same register. A
        // nullable number is not: there is no `null` in an f64.
        TypeExpr::Nullable(e) => match resolve_field_ty(e, env) {
            FieldTy::Obj(c) => FieldTy::Obj(c),
            FieldTy::Arr(e) => FieldTy::Arr(e),
            // `string?` is the same three registers as `string`: a null string is
            // a null data pointer, which compiled code already checks for.
            FieldTy::Str => FieldTy::Str,
            // …and `Bytes?` the same handle, with 0 for null.
            FieldTy::Val => FieldTy::Val,
            // `int32?` is one register with a sentinel; the other widths have no
            // spare value to spend on null, so they stay opaque.
            FieldTy::Num(mersey_front::check::Num::Int(mersey_front::check::IntKind::I32)) => {
                FieldTy::NumOpt
            }
            _ => FieldTy::Opaque,
        },
        _ => FieldTy::Opaque,
    }
}

fn builtin_error_class(name: &'static str, parent: Option<Rc<ClassDef>>) -> ClassDef {
    let fields = vec![("message".to_string(), None), ("stack".to_string(), None)];
    let field_slots = fields
        .iter()
        .enumerate()
        .map(|(i, (n, _))| (n.clone(), i as u32))
        .collect();
    let (initial_slots, dynamic_inits, container_inits) = fold_field_inits(&fields, &[None, None]);
    ClassDef {
        id: fresh_class_id(),
        name: name.to_string(),
        parent,
        field_slots,
        field_tyexprs: vec![None; fields.len()], // `message` and `stack`: strings
        field_tys: RefCell::new(None),
        initial_slots,
        dynamic_inits,
        container_inits,
        fields,
        methods: HashMap::default(),
        getters: HashMap::default(),
        setters: HashMap::default(),
        ctor: None,
        statics: GcCell::new(HashMap::default()),
        static_methods: HashMap::default(),
        is_builtin_error: true,
        host_iface: None,
        env: None,
    }
}

impl Interp {
    /// Public throw for the VM module.
    pub(crate) fn throw_public(&self, class: &'static str, msg: impl Into<String>) -> Thrown {
        self.throw(class, msg)
    }

    fn throw(&self, class: &'static str, msg: impl Into<String>) -> Thrown {
        let cls = self.error_classes[class].clone();
        let stack = self.stack_trace();
        let mut slots = vec![Value::Null; cls.fields.len()];
        slots[0] = Value::Str(Rc::new(utf16(&(msg.into())))); // message
        if slots.len() > 1 {
            slots[1] = Value::Str(Rc::new(utf16(&(stack)))); // stack
        }
        Thrown(Value::Instance(Rc::new(GcCell::new(Instance {
            class: cls,
            slots,
            host: None,
        }))))
    }

    /// `at fn (module:line:col)` per frame, innermost first.
    ///
    /// Deep traces are truncated: a runaway recursion has thousands of
    /// identical frames, and a multi-megabyte error message is its own denial
    /// of service — the frames that say something are the ones at each end.
    pub fn stack_trace(&self) -> String {
        const HEAD: usize = 12;
        const TAIL: usize = 4;
        let n = self.frames.len();
        let mut out = String::new();
        let frame_line = |f: &Frame_| {
            let loc = if f.pos.line > 0 {
                format!("{}:{}:{}", f.module, f.pos.line, f.pos.col)
            } else {
                f.module.to_string()
            };
            format!("\n    at {} ({loc})", f.name)
        };
        for (i, f) in self.frames.iter().rev().enumerate() {
            if n > HEAD + TAIL + 1 && i == HEAD {
                let hidden = n - HEAD - TAIL;
                out.push_str(&format!("\n    ... {hidden} more frames"));
            }
            if n > HEAD + TAIL + 1 && i >= HEAD && i < n - TAIL {
                continue;
            }
            out.push_str(&frame_line(f));
        }
        out
    }

    /// Update the position of the innermost frame (called by the VM loop).
    pub(crate) fn set_site(&mut self, pos: mersey_front::diag::Pos) {
        if let Some(f) = self.frames.last_mut() {
            f.pos = pos;
        }
    }

    pub(crate) fn push_frame(&mut self, name: &Rc<str>, module: &Rc<str>) {
        self.frames.push(Frame_ {
            name: name.clone(),
            module: module.clone(),
            pos: mersey_front::diag::Pos { line: 0, col: 0 },
        });
    }

    pub(crate) fn pop_frame(&mut self) {
        self.frames.pop();
    }

    fn type_error<T>(&self, msg: impl Into<String>) -> Result<T, Thrown> {
        Err(self.throw("TypeError", msg))
    }

    /// Render a thrown value for host error reporting.
    pub fn describe_thrown(&self, t: &Thrown) -> String {
        match &t.0 {
            Value::Instance(i) => {
                let i = i.borrow();
                let get = |name: &str| {
                    i.class
                        .field_slots
                        .get(name)
                        .and_then(|s| i.slots.get(*s as usize))
                        .map(to_display)
                        .unwrap_or_default()
                };
                format!("{}: {}{}", i.class.name, get("message"), get("stack"))
            }
            other => format!("uncaught: {}", to_display(other)),
        }
    }

    // ---- module execution ------------------------------------------------------

    /// Execute a module graph (dependency-first). Each module gets its own
    /// scope; imports link to the exporting module's evaluated bindings.
    pub fn run_graph(&mut self, modules: Vec<(String, &'static Module)>) -> Result<(), Thrown> {
        self.run_modules(modules)?;
        self.maybe_collect();
        Ok(())
    }

    /// Run modules in dependency order, stopping if one suspends on a
    /// top-level `await` — its importers cannot run until it has finished.
    fn run_modules(&mut self, modules: Vec<(String, &'static Module)>) -> Result<(), Thrown> {
        let mut queue = modules.into_iter();
        while let Some((spec, module)) = queue.next() {
            let env = child_env(&self.root);
            let saved_globals = std::mem::replace(&mut self.globals, env.clone());
            let saved_spec = std::mem::replace(&mut self.current_module, spec.clone());
            let result = self.run_module_inner(module);
            let exports = match &result {
                Ok(ModuleFlow::Done) => collect_exports(module, &env),
                _ => HashMap::default(),
            };
            self.globals = saved_globals;
            self.current_module = saved_spec;
            match result? {
                ModuleFlow::Done => {
                    self.modules.insert(spec, exports);
                }
                ModuleFlow::Awaiting(promise) => {
                    self.pending_graph = Some(PendingGraph {
                        promise,
                        spec,
                        module,
                        env,
                        rest: queue.collect(),
                    });
                    return Ok(());
                }
            }
        }
        Ok(())
    }

    /// Is a paused graph now able to continue?
    fn graph_can_resume(&self) -> bool {
        self.pending_graph
            .as_ref()
            .is_some_and(|p| p.promise.borrow().status != PromiseStatus::Pending)
    }

    /// The awaited thing settled: finish that module and run the ones waiting
    /// on it.
    fn resume_graph(&mut self) -> Result<(), Thrown> {
        let Some(p) = self.pending_graph.take() else {
            return Ok(());
        };
        let (status, value) = {
            let st = p.promise.borrow();
            (st.status.clone(), st.value.clone())
        };
        if status == PromiseStatus::Rejected {
            // A module that throws takes its importers with it.
            return Err(Thrown(value));
        }
        let saved_spec = std::mem::replace(&mut self.current_module, p.spec.clone());
        let exports = collect_exports(p.module, &p.env);
        self.current_module = saved_spec;
        self.modules.insert(p.spec, exports);
        self.run_modules(p.rest)
    }

    /// Did the graph stop on a top-level `await` that nothing has settled?
    pub fn graph_is_waiting(&self) -> bool {
        self.pending_graph.is_some()
    }

    /// Register a module that is in the graph but does not run at startup —
    /// the target of a dynamic `import(…)`.
    pub fn register_lazy(&mut self, spec: String, module: &'static Module) {
        self.lazy_modules.insert(spec, module);
    }

    /// `import("./x.mersey")` — a promise of that module's exports.
    ///
    /// The module was already loaded, checked and locked with the rest of the
    /// graph, so this defers *evaluation*, not loading: running code has no
    /// authority to reach for code that was not named up front (§5.4). The
    /// first import runs the module; later ones get the same exports.
    pub(crate) fn dynamic_import(&mut self, spec: &str) -> VResult {
        let target = mersey_front::graph::resolve_module(&self.current_module, spec);
        if !self.modules.contains_key(&target) {
            let Some(module) = self.lazy_modules.get(&target).copied() else {
                return Err(self.throw("Error", format!("`{spec}` is not in the module graph")));
            };
            let env = child_env(&self.root);
            let saved_globals = std::mem::replace(&mut self.globals, env.clone());
            let saved_spec = std::mem::replace(&mut self.current_module, target.clone());
            let result = self.run_module_inner(module);
            let exports = match &result {
                Ok(ModuleFlow::Done) => collect_exports(module, &env),
                _ => HashMap::default(),
            };
            self.globals = saved_globals;
            self.current_module = saved_spec;
            match result? {
                ModuleFlow::Done => {
                    self.modules.insert(target.clone(), exports);
                }
                ModuleFlow::Awaiting(_) => {
                    // The imported module's own top level is awaiting. Nothing
                    // here can wait for it without blocking the whole engine.
                    return Err(self.throw(
                        "Error",
                        format!(
                            "`{spec}` suspends on a top-level `await`; import it statically \
                             so the graph can wait for it"
                        ),
                    ));
                }
            }
        }
        let exports = self.modules.get(&target).cloned().unwrap_or_default();
        let mut fields: Vec<(String, Value)> = exports.into_iter().collect();
        fields.sort_by(|a, b| a.0.cmp(&b.0));
        let rec = Rc::new(GcCell::new(fields));
        gc::track_record(&rec);
        let promise = PromiseState::pending();
        self.settle(&promise, Value::Record(rec), false);
        Ok(Value::PromiseV(promise))
    }

    pub fn run_module(&mut self, module: &'static Module) -> Result<(), Thrown> {
        self.run_module_inner(module)?;
        Ok(())
    }

    /// One REPL turn — a HOST feature; the language itself has no `eval`
    /// (§1.2). The caller re-parses its whole accumulated program so binding
    /// and the checker see every prior declaration, then this executes only
    /// the items from `first_item` on: earlier items already ran in earlier
    /// turns, and their declarations keep their original bindings (an
    /// instance from turn 1 stays an instance of turn 1's class). A
    /// redefinition typed in a NEW turn appears in the tail and takes effect
    /// normally. The trailing bare expression statement's value comes back
    /// displayed, for echoing; statements echo nothing.
    pub fn run_repl_turn(
        &mut self,
        module: &'static Module,
        first_item: usize,
    ) -> Result<Option<String>, Thrown> {
        // Imports and declarations from the NEW items only.
        let mut decls: Vec<&'static Decl> = Vec::new();
        for item in module.items.iter().skip(first_item) {
            match item {
                Item::Import(im) => self.bind_import(im)?,
                Item::Decl(d) => decls.push(d),
                Item::Export(ex) => {
                    if let ExportKind::Decl(d) = &ex.kind {
                        decls.push(d);
                    }
                }
                Item::Stmt(_) => {}
            }
        }
        for d in &decls {
            if let Decl::Function(f) = d {
                let data = Rc::new(FnData::new(
                    f.name.text.as_str().into(),
                    f.is_async,
                    &f.params,
                    FnBody::Block(&f.body),
                    f.ret.as_ref(),
                ));
                let c = Closure {
                    data,
                    env: self.globals.clone(),
                    this: None,
                    cls: None,
                };
                env_define(&self.globals, &f.name.text, Value::Closure(Rc::new(c)));
            }
        }
        let mut pending: Vec<&'static ClassDecl> = decls
            .iter()
            .filter_map(|d| match d {
                Decl::Class(c) => Some(c),
                _ => None,
            })
            .collect();
        self.pending_class_names = pending.iter().map(|c| c.name.text.clone()).collect();
        while !pending.is_empty() {
            let mut still = Vec::new();
            let mut progressed = false;
            for c in pending {
                if self.try_define_class(c)? {
                    progressed = true;
                    self.pending_class_names.remove(&c.name.text);
                } else {
                    still.push(c);
                }
            }
            pending = still;
            if !pending.is_empty() && !progressed {
                let name = &pending[0].name.text;
                return Err(self.throw(
                    "TypeError",
                    format!("cannot resolve base class of `{name}`"),
                ));
            }
        }
        for d in &decls {
            if let Decl::Enum(e) = d {
                self.define_enum(e)?;
            }
        }

        // Execute the new statements on the tree-walker (REPL turns are not
        // performance surfaces), echoing a trailing bare expression.
        let mut echo = None;
        for item in module.items.iter().skip(first_item) {
            match item {
                Item::Stmt(Stmt::Expr(e)) => {
                    let v = self.eval(e, &self.globals.clone())?;
                    echo = match v {
                        Value::Null => None,
                        other => Some(to_display(&other)),
                    };
                }
                Item::Stmt(s) => {
                    self.exec_stmt(s, &self.globals.clone())?;
                    echo = None;
                }
                Item::Export(ExportDecl {
                    kind: ExportKind::Var(v),
                    ..
                }) => {
                    self.exec_var(v, &self.globals.clone())?;
                    echo = None;
                }
                _ => {}
            }
        }
        self.drain_tasks()?;
        Ok(echo)
    }

    fn run_module_inner(&mut self, module: &'static Module) -> Result<ModuleFlow, Thrown> {
        let mut decls: Vec<&'static Decl> = Vec::new();
        for item in &module.items {
            match item {
                Item::Import(im) => self.bind_import(im)?,
                Item::Decl(d) => decls.push(d),
                Item::Export(ex) => match &ex.kind {
                    ExportKind::Decl(d) => decls.push(d),
                    // Exported variables execute with the other statements
                    // (second walk below); named re-exports are inert here.
                    ExportKind::Var(_) | ExportKind::Named { .. } => {}
                },
                Item::Stmt(_) => {}
            }
        }

        // Hoist declarations (order-independent, §4.5). Classes may extend
        // classes declared later, so define in dependency order.
        for d in &decls {
            if let Decl::Function(f) = d {
                let data = Rc::new(FnData::new(
                    f.name.text.as_str().into(),
                    f.is_async,
                    &f.params,
                    FnBody::Block(&f.body),
                    f.ret.as_ref(),
                ));
                let c = Closure {
                    data,
                    env: self.globals.clone(),
                    this: None,
                    cls: None,
                };
                env_define(&self.globals, &f.name.text, Value::Closure(Rc::new(c)));
            }
        }
        let mut pending: Vec<&'static ClassDecl> = decls
            .iter()
            .filter_map(|d| match d {
                Decl::Class(c) => Some(c),
                _ => None,
            })
            .collect();
        self.pending_class_names = pending.iter().map(|c| c.name.text.clone()).collect();
        while !pending.is_empty() {
            let mut still = Vec::new();
            let mut progressed = false;
            for c in pending {
                if self.try_define_class(c)? {
                    progressed = true;
                    self.pending_class_names.remove(&c.name.text);
                } else {
                    still.push(c);
                }
            }
            pending = still;
            if !pending.is_empty() && !progressed {
                let name = &pending[0].name.text;
                return Err(self.throw(
                    "TypeError",
                    format!("cannot resolve base class of `{name}`"),
                ));
            }
        }
        for d in &decls {
            if let Decl::Enum(e) = d {
                self.define_enum(e)?;
            }
        }

        // Execute remaining top-level statements in order (including
        // exported variable statements) — compiled when possible.
        let spec = self.current_module.clone();
        let compiled = vm::compile_module_stmts_in(module, &spec);
        {
            if let Some(chunk) = compiled.clone() {
                let globals = self.globals.clone();
                // Top-level `await`: the module *is* the async function. It runs
                // as a coroutine, and the modules that import it wait for it to
                // settle (§4.5) — exactly what a caller of an async function
                // does.
                //
                // This happens on the bytecode VM whether or not the tree-walker
                // is selected, for the same reason an async *function* does:
                // `await` suspends by capturing VM state, which the AST walker
                // has none of. The two tiers therefore agree on async semantics
                // by construction rather than by keeping two implementations in
                // step.
                if vm::chunk_awaits(&chunk) {
                    let result = PromiseState::pending();
                    let coro = Coro {
                        gen: None,
                        frame: vm::new_frame(&chunk, &globals, None),
                        chunk,
                        pc: 0,
                        stack: Vec::new(),
                        scopes: vec![globals],
                        handlers: Vec::new(),
                        cls: None,
                        result: result.clone(),
                    };
                    self.push_frame(&"<module>".into(), &spec.as_str().into());
                    let out = self.drive(coro, None);
                    self.pop_frame();
                    out?;
                    self.drain_tasks()?;
                    return match result.borrow().status {
                        PromiseStatus::Fulfilled => Ok(ModuleFlow::Done),
                        PromiseStatus::Rejected => Err(Thrown(result.borrow().value.clone())),
                        // Still waiting on something only the host can settle
                        // (a fetch, a timer). The graph continues when it does.
                        PromiseStatus::Pending => Ok(ModuleFlow::Awaiting(result.clone())),
                    };
                }
            }
        }
        if self.use_vm {
            if let Some(chunk) = compiled {
                let globals = self.globals.clone();
                self.push_frame(&"<module>".into(), &spec.as_str().into());
                let frame = vm::new_frame(&chunk, &globals, None);
                let out = vm::run_chunk(self, &chunk, globals, frame, None);
                self.pop_frame();
                out?;
                self.drain_tasks()?;
                return Ok(ModuleFlow::Done);
            }
        }
        // The debugger's stack wants the module frame under tree-walked
        // top-level statements too (the VM branch above pushes its own).
        let debug_module_frame = self.debug_hook.is_some();
        if debug_module_frame {
            self.push_frame(&"<module>".into(), &spec.as_str().into());
            self.debug_envs.push(self.globals.clone());
        }
        let run = (|| -> Result<(), Thrown> {
            for item in &module.items {
                match item {
                    Item::Stmt(s) => {
                        self.exec_stmt(s, &self.globals.clone())?;
                    }
                    Item::Export(ExportDecl {
                        kind: ExportKind::Var(v),
                        ..
                    }) => {
                        self.exec_var(v, &self.globals.clone())?;
                    }
                    _ => {}
                }
            }
            Ok(())
        })();
        if debug_module_frame {
            self.pop_frame();
            self.debug_envs.pop();
        }
        run?;
        self.drain_tasks()?;
        Ok(ModuleFlow::Done)
    }

    fn bind_import(&mut self, im: &'static ImportDecl) -> Result<(), Thrown> {
        // `import * as m from "…"`: bind the module (or built-in namespace)
        // under one name. Relative specifiers are handled below, where the
        // module's exports are known.
        let namespace_alias: Option<&'static Name> = match &im.clause {
            Some(ImportClause::Namespace(n)) => Some(n),
            _ => None,
        };
        let names: Vec<&Name> = match &im.clause {
            None => return Ok(()),
            Some(ImportClause::Namespace(_)) => Vec::new(),
            Some(ImportClause::Named(specs)) => specs
                .iter()
                .map(|s| s.alias.as_ref().unwrap_or(&s.name))
                .collect(),
        };
        match im.from.as_str() {
            "std:console" => {
                let mut entries = HashMap::default();
                for level in ["log", "warn", "error", "info", "debug"] {
                    let id: &'static str = Box::leak(format!("console.{level}").into_boxed_str());
                    entries.insert(level.to_string(), Value::Native(Box::leak(Box::new(id))));
                }
                let console = Value::Namespace(Rc::new(Namespace {
                    name: "console".to_string(),
                    entries,
                }));
                for n in names {
                    env_define(&self.globals, &n.text, console.clone());
                }
                Ok(())
            }
            "std:regex" | "std:parse" => {
                let (ns_name, natives): (&str, &[&str]) = if im.from == "std:regex" {
                    ("regex", &["compile"])
                } else {
                    (
                        "parse",
                        &[
                            "int32", "int64", "float64", "bigint", "bigdec", "bool", "url",
                        ],
                    )
                };
                let mut entries = HashMap::default();
                for n in natives {
                    let id: &'static str = Box::leak(format!("{ns_name}.{n}").into_boxed_str());
                    entries.insert(n.to_string(), Value::Native(Box::leak(Box::new(id))));
                }
                let ns = Value::Namespace(Rc::new(Namespace {
                    name: ns_name.to_string(),
                    entries,
                }));
                for n in names.iter().chain(namespace_alias.iter()) {
                    env_define(&self.globals, &n.text, ns.clone());
                }
                Ok(())
            }
            "std:gc" => {
                let mut entries = HashMap::default();
                for n in ["collect", "stats"] {
                    let id: &'static str = Box::leak(format!("gc.{n}").into_boxed_str());
                    entries.insert(n.to_string(), Value::Native(Box::leak(Box::new(id))));
                }
                let ns = Value::Namespace(Rc::new(Namespace {
                    name: "gc".to_string(),
                    entries,
                }));
                for n in names {
                    env_define(&self.globals, &n.text, ns.clone());
                }
                Ok(())
            }
            "std:time" => {
                let mut entries = HashMap::default();
                for n in ["now", "monotonic", "parts", "fromParts", "format", "parse"] {
                    let id: &'static str = Box::leak(format!("time.{n}").into_boxed_str());
                    entries.insert(n.to_string(), Value::Native(Box::leak(Box::new(id))));
                }
                let ns = Value::Namespace(Rc::new(Namespace {
                    name: "time".to_string(),
                    entries,
                }));
                for n in names {
                    env_define(&self.globals, &n.text, ns.clone());
                }
                Ok(())
            }
            "std:bytes" => {
                let mut entries = HashMap::default();
                for n in [
                    "alloc",
                    "fromHost",
                    "toHost",
                    "fill",
                    "encodeUtf8",
                    "decodeUtf8",
                ] {
                    let id: &'static str = Box::leak(format!("bytes.{n}").into_boxed_str());
                    entries.insert(n.to_string(), Value::Native(Box::leak(Box::new(id))));
                }
                let ns = Value::Namespace(Rc::new(Namespace {
                    name: "bytes".to_string(),
                    entries,
                }));
                for n in names {
                    env_define(&self.globals, &n.text, ns.clone());
                }
                Ok(())
            }
            "std:async" => {
                let mut entries = HashMap::default();
                for n in ["resolve", "reject", "all"] {
                    let id: &'static str = Box::leak(format!("promise.{n}").into_boxed_str());
                    entries.insert(n.to_string(), Value::Native(Box::leak(Box::new(id))));
                }
                let ns = Value::Namespace(Rc::new(Namespace {
                    name: "Promise".to_string(),
                    entries,
                }));
                for n in names {
                    env_define(&self.globals, &n.text, ns.clone());
                }
                Ok(())
            }
            "std:mersey" => {
                // Data properties, built at import time (a `Str` cannot live in a
                // `const` slice, so this namespace gets its own arm). `version` is
                // the engine's own package version; `abiVersion` is the single
                // source of truth shared with the C header and `mersey_capi`.
                let mut entries = HashMap::default();
                entries.insert(
                    "version".to_string(),
                    Value::Str(Rc::new(utf16(env!("CARGO_PKG_VERSION")))),
                );
                entries.insert("abiVersion".to_string(), Value::I32(ABI_VERSION as i32));
                let ns = Value::Namespace(Rc::new(Namespace {
                    name: "Mersey".to_string(),
                    entries,
                }));
                for n in names.iter().chain(namespace_alias.iter()) {
                    env_define(&self.globals, &n.text, ns.clone());
                }
                Ok(())
            }
            "std:math" | "std:format" | "std:fs" | "std:env" | "std:caps" | "std:json"
            | "std:random" | "std:net" | "std:dom" | "std:hash" => {
                let (ns_name, natives, consts): (&str, &[&str], &[(&str, Value)]) =
                    match im.from.as_str() {
                        "std:json" => ("json", &["stringify", "parse"], &[]),
                        "std:hash" => ("hash", &["sha256", "sha1", "hmacSha256", "hmacSha1"], &[]),
                        "std:net" => ("net", &["serve"], &[]),
                        "std:dom" => ("dom", &["apply"], &[]),
                        "std:random" => ("random", &["float", "int", "bytes", "fill"], &[]),
                        "std:math" => (
                            "math",
                            &[
                                "abs", "min", "max", "floor", "ceil", "sqrt", "pow", "round",
                                "trunc", "sign", "clamp", "exp", "log", "log2", "log10", "cbrt",
                                "hypot", "sin", "cos", "tan", "asin", "acos", "atan", "atan2",
                                "isNaN", "isFinite",
                            ],
                            &[
                                ("PI", Value::F64(std::f64::consts::PI)),
                                ("E", Value::F64(std::f64::consts::E)),
                            ],
                        ),
                        "std:format" => ("format", &["pad", "fixed"], &[]),
                        "std:fs" => ("fs", &["readText"], &[]),
                        "std:env" => ("env", &["get"], &[]),
                        _ => ("caps", &["has", "list", "drop"], &[]),
                    };
                let mut entries = HashMap::default();
                for n in natives {
                    // Native ids are `<ns>.<method>`, leaked once per import.
                    let id: &'static str = Box::leak(format!("{ns_name}.{n}").into_boxed_str());
                    entries.insert(n.to_string(), Value::Native(Box::leak(Box::new(id))));
                }
                for (n, v) in consts {
                    entries.insert(n.to_string(), v.clone());
                }
                let ns = Value::Namespace(Rc::new(Namespace {
                    name: ns_name.to_string(),
                    entries,
                }));
                for n in names.iter().chain(namespace_alias.iter()) {
                    env_define(&self.globals, &n.text, ns.clone());
                }
                Ok(())
            }
            "browser:dom" => {
                for n in names {
                    // Engine-provided helpers (not IDL): explicit handle
                    // release for long-lived pages.
                    if n.text == "release" {
                        env_define(&self.globals, "release", Value::Native(&"web.release"));
                        continue;
                    }
                    // Bind a Mersey instance of a host-backed class to an
                    // existing host object (the browser builds custom elements).
                    if n.text == "attach" {
                        env_define(&self.globals, "attach", Value::Native(&"web.attach"));
                        continue;
                    }
                    // Fast path: the hand-written DOM surface (kept because
                    // the Stage A demos and goldens pin it).
                    if n.text == "document" && self.host.web_global("document") < 0 {
                        let mut entries = HashMap::default();
                        entries.insert(
                            "getElementById".to_string(),
                            Value::Native(&"dom.getElementById"),
                        );
                        entries.insert(
                            "createElement".to_string(),
                            Value::Native(&"dom.createElement"),
                        );
                        let document = Value::Namespace(Rc::new(Namespace {
                            name: "document".to_string(),
                            entries,
                        }));
                        env_define(&self.globals, &n.text, document);
                        continue;
                    }
                    // General path: any ambient web global, via the bridge.
                    //
                    // A name the host does not have is NOT an error here. Most of
                    // the web surface is interfaces — `Element`, `Event` — imported
                    // to be *written in type positions*, and demanding a live global
                    // for those would make every type annotation a runtime dependency
                    // on the host implementing it. It also breaks feature detection,
                    // which the platform is built on. So the name is simply left
                    // unbound, and the error surfaces only if the program actually
                    // uses it as a value — see the `is not defined` path in `eval`.
                    let handle = self.host.web_global(&n.text);
                    if handle >= 0 && n.text == "JSON" {
                        self.json_handle = handle;
                    }
                    if handle < 0 {
                        self.absent_globals.insert(n.text.clone());
                        continue;
                    }
                    self.absent_globals.remove(&n.text);
                    env_define(&self.globals, &n.text, Value::JsRef(handle));
                }
                Ok(())
            }
            other if crate::graph_is_module(other) => {
                let target = mersey_front::graph::resolve_module(&self.current_module, other);
                let Some(exports) = self.modules.get(&target).cloned() else {
                    return self.type_error(format!(
                        "module `{other}` was not loaded (resolved to `{target}`)"
                    ));
                };
                match &im.clause {
                    Some(ImportClause::Named(specs)) => {
                        for s in specs {
                            let local = s.alias.as_ref().unwrap_or(&s.name);
                            match exports.get(&s.name.text) {
                                Some(v) => env_define(&self.globals, &local.text, v.clone()),
                                None => {
                                    return self.type_error(format!(
                                        "`{}` is not exported by `{other}`",
                                        s.name.text
                                    ))
                                }
                            }
                        }
                    }
                    Some(ImportClause::Namespace(n)) => {
                        let ns = Value::Namespace(Rc::new(Namespace {
                            name: n.text.clone(),
                            entries: exports,
                        }));
                        env_define(&self.globals, &n.text, ns);
                    }
                    None => {}
                }
                Ok(())
            }
            other => self.type_error(format!(
                "module `{other}` is not available (built-ins: std:console, std:math, \
                 std:format, std:fs, std:env, std:caps, std:async, browser:dom)"
            )),
        }
    }

    fn try_define_class(&mut self, c: &'static ClassDecl) -> Result<bool, Thrown> {
        let mut host_iface: Option<String> = None;
        let parent = match &c.extends {
            None => None,
            Some(TypeExpr::Named { name, .. }) => {
                let head = name.split('.').next().unwrap_or(name).to_string();
                match env_get(&self.globals, &head) {
                    Some(Value::Class(p)) => Some(p),
                    // A Mersey base class declared later in this module.
                    _ if self.pending_class_names.contains(&head) => return Ok(false),
                    // Otherwise it is a host interface (`extends HTMLElement`):
                    // instances are backed by host objects.
                    _ => {
                        host_iface = Some(head);
                        None
                    }
                }
            }
            Some(_) => return self.type_error("invalid extends clause"),
        };
        // A Mersey base class may itself be host-backed: inherit that.
        if host_iface.is_none() {
            if let Some(p) = &parent {
                host_iface = p.host_iface.clone();
            }
        }

        let mut fields: Vec<(String, Option<&'static Expr>)> = Vec::new();
        // The declared type of each field, in the same order. Tier 1 needs it to
        // know that `this.x` is a `float64` at offset 3 — and the base class's
        // fields come first, which is exactly why a base's offsets stay valid on
        // a subclass.
        let mut field_tyexprs: Vec<Option<&'static TypeExpr>> = Vec::new();
        if let Some(p) = &parent {
            fields.extend(p.fields.iter().map(|(n, e)| (n.clone(), *e)));
            field_tyexprs.extend(p.field_tyexprs.iter().copied());
        }
        let mut methods = HashMap::default();
        let mut getters = HashMap::default();
        let mut setters = HashMap::default();
        let mut static_methods = HashMap::default();
        let mut ctor = None;
        let statics: GcCell<HashMap<String, Value>> = GcCell::new(HashMap::default());

        for m in &c.members {
            match m {
                ClassMember::Field {
                    mods,
                    name,
                    init,
                    ty,
                    ..
                } => {
                    if mods.is_static {
                        let v = match init {
                            Some(e) => self.eval(e, &self.globals.clone())?,
                            // Uninitialized: the type's zero, like any binding.
                            None => match check::default_for_ty(ty) {
                                Some(d) => default_value(d),
                                None => Value::Null,
                            },
                        };
                        statics.borrow_mut().insert(name.clone(), v);
                    } else {
                        fields.push((name.clone(), init.as_ref()));
                        field_tyexprs.push(Some(ty));
                    }
                }
                ClassMember::Method {
                    mods,
                    is_async,
                    name,
                    params,
                    body,
                    ret,
                    ..
                } => {
                    if let Some(body) = body {
                        // A method's declared return type, which it used not to
                        // keep: a method was never compiled, so what it returned
                        // at the boundary was moot. It is compiled now.
                        let data = Rc::new(FnData::new(
                            name.as_str().into(),
                            *is_async,
                            params,
                            FnBody::Block(body),
                            Some(ret),
                        ));
                        if mods.is_static {
                            static_methods.insert(name.clone(), data);
                        } else {
                            methods.insert(name.clone(), data);
                        }
                    }
                }
                ClassMember::Getter {
                    name, ret, body, ..
                } => {
                    // The declared return type, kept for the same reason a
                    // method's is: a getter is compiled now. Without it Tier-1
                    // reads the body as returning nothing, sees a `Return` with
                    // a value on the stack, and gives up on the whole group.
                    getters.insert(
                        name.clone(),
                        Rc::new(FnData::new(
                            name.as_str().into(),
                            false,
                            &[],
                            FnBody::Block(body),
                            Some(ret),
                        )),
                    );
                }
                ClassMember::Setter {
                    name, param, body, ..
                } => {
                    setters.insert(
                        name.clone(),
                        Rc::new(FnData::new(
                            name.as_str().into(),
                            false,
                            std::slice::from_ref(param),
                            FnBody::Block(body),
                            None,
                        )),
                    );
                }
                ClassMember::Ctor { params, body, .. } => {
                    ctor = Some(Rc::new(FnData::new(
                        format!("{}.constructor", c.name.text).into(),
                        false,
                        params,
                        FnBody::Block(body),
                        None,
                    )));
                }
            }
        }

        let field_slots: HashMap<String, u32> = fields
            .iter()
            .enumerate()
            .map(|(i, (n, _))| (n.clone(), i as u32))
            .collect();
        let (initial_slots, dynamic_inits, container_inits) =
            fold_field_inits(&fields, &field_tyexprs);
        let def = Rc::new(ClassDef {
            id: fresh_class_id(),
            name: c.name.text.clone(),
            parent,
            field_slots,
            field_tyexprs,
            field_tys: RefCell::new(None),
            initial_slots,
            dynamic_inits,
            container_inits,
            fields,
            methods,
            getters,
            setters,
            ctor,
            statics,
            static_methods,
            is_builtin_error: false,
            host_iface,
            env: Some(self.globals.clone()),
        });
        gc::track_class(&def);
        self.all_classes.push(def.clone());
        env_define(&self.globals, &c.name.text, Value::Class(def));
        Ok(true)
    }

    fn define_enum(&mut self, e: &'static EnumDecl) -> Result<(), Thrown> {
        let mut entries = HashMap::default();
        let mut next: i64 = 0;
        for (name, init) in &e.members {
            let v = match init {
                Some(expr) => {
                    let v = self.eval(expr, &self.globals.clone())?;
                    as_i64(&v).ok_or_else(|| {
                        self.throw("TypeError", "enum member value must be an integer")
                    })?
                }
                None => next,
            };
            next = v + 1;
            entries.insert(name.text.clone(), Value::I64(v));
        }
        let ns = Value::Namespace(Rc::new(Namespace {
            name: e.name.text.clone(),
            entries,
        }));
        env_define(&self.globals, &e.name.text, ns);
        Ok(())
    }

    /// Driver entry point for host event callbacks (Stage A DOM events).
    pub fn invoke_callback(&mut self, id: u32) -> Result<(), Thrown> {
        let cb = match self.callbacks.get(id as usize) {
            Some(v) => v.clone(),
            None => return self.type_error(format!("unknown callback #{id}")),
        };
        self.call_value(&cb, Vec::new())?;
        self.drain_microtasks()?;
        // A finished callback is a host boundary: no VM frame is live, so the
        // roots really are the roots and it is safe to collect.
        self.maybe_collect();
        Ok(())
    }

    /// The (port, callback id) a top-level `net.serve` recorded, if any — the
    /// CLI driver takes this after `run_graph` and enters its accept loop.
    pub fn take_pending_server(&mut self) -> Option<(u16, u32)> {
        self.host.take_pending_server()
    }

    /// Driver entry point for one HTTP request (the CLI `serve` accept loop).
    /// Calls the registered handler with the request `(method, path, body)` —
    /// all strings — and returns the raw HTTP response string it produced. The
    /// ergonomic Request/Response object layer lives in `std/http.mersey`; this
    /// boundary moves only strings, so no host-side Value construction is needed.
    pub fn http_dispatch(
        &mut self,
        cb_id: u32,
        method: &str,
        path: &str,
        body: &str,
    ) -> Result<String, Thrown> {
        let cb = match self.callbacks.get(cb_id as usize) {
            Some(v) => v.clone(),
            None => return Err(self.throw("TypeError", format!("unknown callback #{cb_id}"))),
        };
        let args = vec![
            Value::Str(Rc::new(utf16(method))),
            Value::Str(Rc::new(utf16(path))),
            Value::Str(Rc::new(utf16(body))),
        ];
        let ret = self.call_value(&cb, args)?;
        self.drain_microtasks()?;
        self.maybe_collect();
        match &ret {
            Value::Str(s) => Ok(utf16_to_string(s)),
            _ => Err(self.throw("TypeError", "http handler must return a string")),
        }
    }

    // ---- statements -----------------------------------------------------------

    fn exec_block(&mut self, stmts: &'static [Stmt], env: &Env) -> SResult {
        let scope = child_env(env);
        for s in stmts {
            match self.exec_stmt(s, &scope)? {
                Sig::Normal => {}
                other => return Ok(other),
            }
        }
        Ok(Sig::Normal)
    }

    /// Attach a debugger (see `DebugHook`). Forces the pure tree-walker so
    /// every sync statement reports; async/generator bodies stay on the VM.
    pub fn set_debug_hook(&mut self, hook: Box<dyn DebugHook>) {
        self.use_vm = false;
        self.debug_hook = Some(hook);
    }

    /// Detach the debugger: drop the hook and restore the VM tier. A browser
    /// closing DevTools should get its speed back, so this is not merely
    /// "stop reporting" — `use_vm` returns to the constructor's default.
    /// Callable between statements, never from inside a callout (the hook is
    /// taken out for the call; see `debug_stmt`).
    pub fn clear_debug_hook(&mut self) {
        self.debug_hook = None;
        self.use_vm = true;
    }

    /// The debugger callout: this statement's position, the call stack, and
    /// on-demand locals. The hook is taken out for the call so the borrows
    /// stay disjoint (a hook installing another hook from inside itself is
    /// not supported — the swap-back would drop it).
    fn debug_stmt(&mut self, s: &'static Stmt, env: &Env) {
        let Some(pos) = stmt_pos(s) else { return };
        // Keep the innermost frame's position current: when a call pushes a
        // new frame, the caller's frame is left holding its call-site line —
        // exactly what a debugger's stack view shows for outer frames.
        if let Some(f) = self.frames.last_mut() {
            f.pos = pos;
        }
        let Some(mut hook) = self.debug_hook.take() else {
            return;
        };
        // Own the pause data before the callout. Evaluate-in-frame borrows
        // `&mut self` and can push frames (an evaluated call), which would
        // dangle a borrow of `self.frames`/`self.debug_envs`; clone the frame
        // list and the scope-chain roots (Rc handles — cheap) so `self` is free.
        let frames_owned: Vec<Frame_> = self.frames.clone();
        let frame_envs: Vec<Env> = frame_env_chain(env, &self.debug_envs);
        {
            let pause = DebugPause {
                pos,
                frames: &frames_owned,
            };
            let mut do_locals = |from_top: usize| -> Vec<Vec<(String, String)>> {
                frame_envs
                    .get(from_top)
                    .map(snapshot_scopes)
                    .unwrap_or_default()
            };
            let mut do_eval = |from_top: usize, expr: &str| -> Result<String, String> {
                match frame_envs.get(from_top) {
                    Some(e) => self.eval_in_frame(&e.clone(), expr),
                    None => Err("no such frame".to_string()),
                }
            };
            hook.on_stmt(&pause, &mut do_locals, &mut do_eval);
        }
        self.debug_hook = Some(hook);
    }

    /// Evaluate an expression against a paused frame's live scope — the debug
    /// console's evaluate-in-frame. Runtime semantics: names resolve by name
    /// against `env`'s scope chain, there is no static re-check, and the engine's
    /// own debug hook is already taken out so an evaluated call cannot re-pause.
    /// Returns the display result, or a parse/runtime error message.
    fn eval_in_frame(&mut self, env: &Env, expr: &str) -> Result<String, String> {
        let trimmed = expr.trim().trim_end_matches(';').trim();
        if trimmed.is_empty() {
            return Err("empty expression".to_string());
        }
        let text = format!("{trimmed};\n");
        let src = mersey_front::source::decode("<debug-eval>", text.as_bytes())
            .map_err(|_| "could not decode expression".to_string())?;
        let parsed = mersey_front::parser::parse(&src);
        if !parsed.diagnostics.is_empty() {
            return Err(parsed
                .diagnostics
                .iter()
                .map(|d| d.to_string())
                .collect::<Vec<_>>()
                .join("; "));
        }
        // Leaked to `'static` like a REPL turn; a debug evaluation is human-paced
        // and rare, so the small leak is acceptable (same trade the REPL makes).
        let module: &'static Module = Box::leak(Box::new(parsed.module));
        let expr_ast = match module.items.first() {
            Some(Item::Stmt(Stmt::Expr(e))) => e,
            _ => return Err("expected a single expression".to_string()),
        };
        match self.eval(expr_ast, env) {
            Ok(v) => Ok(to_display(&v)),
            Err(Thrown(v)) => Err(format!("uncaught: {}", to_display(&v))),
        }
    }

    /// Whether a debugger is attached (the VM loop's per-op gate).
    pub(crate) fn debug_hook_attached(&self) -> bool {
        self.debug_hook.is_some()
    }

    /// The VM's debugger callout — async/generator bodies execute there, and
    /// this is how their line changes reach the hook. Same contract as
    /// `debug_stmt`; the VM's slot-resolved locals may not all live in the
    /// scope chain, so snapshots there are best-effort.
    pub(crate) fn debug_vm_stmt(
        &mut self,
        pos: mersey_front::diag::Pos,
        env: &Env,
        slots: &[(String, Value)],
    ) {
        if let Some(f) = self.frames.last_mut() {
            f.pos = pos;
        }
        let Some(mut hook) = self.debug_hook.take() else {
            return;
        };
        let envs = &self.debug_envs;
        hook.on_stmt(
            &DebugPause {
                pos,
                frames: &self.frames,
            },
            &mut |from_top| {
                if from_top == 0 {
                    // Slot-resolved locals are the innermost scope (registers the
                    // scope chain never sees), then the chain itself.
                    let mut out = Vec::new();
                    if !slots.is_empty() {
                        let mut vars: Vec<(String, String)> = slots
                            .iter()
                            .map(|(k, v)| (k.clone(), to_display(v)))
                            .collect();
                        vars.sort();
                        out.push(vars);
                    }
                    out.extend(snapshot_scopes(env));
                    out
                } else {
                    match envs
                        .len()
                        .checked_sub(from_top + 1)
                        .and_then(|i| envs.get(i))
                    {
                        Some(e) => snapshot_scopes(e),
                        None => Vec::new(),
                    }
                }
            },
            // Evaluate-in-frame is not supported for VM (async/generator) frames:
            // the paused state lives in slots, not a re-enterable scope chain —
            // the same reason those bodies are not stepped (a recorded v1 limit).
            &mut |_from_top, _expr| {
                Err("evaluate-in-frame is not available in async/generator frames".to_string())
            },
        );
        self.debug_hook = Some(hook);
    }

    fn exec_stmt(&mut self, s: &'static Stmt, env: &Env) -> SResult {
        self.exec_stmt_l(s, env, None)
    }

    /// `label` is the label attached to this statement, if it is a loop —
    /// `break label`/`continue label` signals matching it are consumed here.
    fn exec_stmt_l(&mut self, s: &'static Stmt, env: &Env, label: Option<&str>) -> SResult {
        if self.debug_hook.is_some() {
            self.debug_stmt(s, env);
        }
        match s {
            Stmt::Block(b) => self.exec_block(b, env),
            Stmt::Var(v) => {
                self.exec_var(v, env)?;
                Ok(Sig::Normal)
            }
            Stmt::Expr(e) => {
                self.eval(e, env)?;
                Ok(Sig::Normal)
            }
            Stmt::Empty => Ok(Sig::Normal),
            Stmt::If { cond, then, els } => {
                if self.truthy(cond, env)? {
                    self.exec_stmt(then, env)
                } else if let Some(e) = els {
                    self.exec_stmt(e, env)
                } else {
                    Ok(Sig::Normal)
                }
            }
            Stmt::While { cond, body } => {
                while self.truthy(cond, env)? {
                    match loop_ctl(self.exec_stmt(body, env)?, label) {
                        LoopCtl::BreakLoop => break,
                        LoopCtl::NextIter => {}
                        LoopCtl::Out(sig) => return Ok(sig),
                    }
                }
                Ok(Sig::Normal)
            }
            Stmt::DoWhile { body, cond } => {
                loop {
                    match loop_ctl(self.exec_stmt(body, env)?, label) {
                        LoopCtl::BreakLoop => break,
                        LoopCtl::NextIter => {}
                        LoopCtl::Out(sig) => return Ok(sig),
                    }
                    if !self.truthy(cond, env)? {
                        break;
                    }
                }
                Ok(Sig::Normal)
            }
            Stmt::For {
                init,
                cond,
                step,
                body,
            } => {
                let outer = child_env(env);
                let mut per_iteration: Vec<String> = Vec::new();
                match init {
                    Some(ForInit::Var(v)) => {
                        self.exec_var(v, &outer)?;
                        // `for (let i = 0; …)` gives each iteration its own
                        // `i`, so a closure made in the body captures the value
                        // it saw rather than the one the loop finished with —
                        // the reason `let` exists in a loop head at all.
                        // Only when something can actually capture it:
                        // otherwise this is an ordinary counted loop and stays
                        // one, with no scope allocated per iteration.
                        if v.kind == VarKind::Let && vm::loop_captures(cond, step, body) {
                            for b in &v.bindings {
                                pattern_names_of(&b.target, &mut per_iteration);
                            }
                        }
                    }
                    Some(ForInit::Exprs(es)) => {
                        for e in es {
                            self.eval(e, &outer)?;
                        }
                    }
                    None => {}
                }
                let fresh = |from: &Env, names: &[String]| -> Env {
                    let it = child_env(from);
                    for name in names {
                        let v = env_get(from, name).unwrap_or(Value::Null);
                        env_define(&it, name, v);
                    }
                    it
                };
                let mut scope = if per_iteration.is_empty() {
                    outer.clone()
                } else {
                    fresh(&outer, &per_iteration)
                };
                loop {
                    if let Some(c) = cond {
                        if !self.truthy(c, &scope)? {
                            break;
                        }
                    }
                    match loop_ctl(self.exec_stmt(body, &scope)?, label) {
                        LoopCtl::BreakLoop => break,
                        LoopCtl::NextIter => {}
                        LoopCtl::Out(sig) => return Ok(sig),
                    }
                    // The update runs in the *next* iteration's scope: if it
                    // ran in this one, the closure just created in the body
                    // would see the incremented value — exactly the bug that
                    // per-iteration bindings exist to prevent.
                    if !per_iteration.is_empty() {
                        scope = fresh(&scope, &per_iteration);
                    }
                    for e in step {
                        self.eval(e, &scope)?;
                    }
                }
                Ok(Sig::Normal)
            }
            Stmt::ForOf {
                target, iter, body, ..
            } => {
                let iterable = self.eval(iter, env)?;
                // An array iterates **live**, by index: the length is re-read
                // every pass, so growth is seen and a shrink ends the loop —
                // which is what `for…of` means in JS, and what the bytecode VM
                // does. A snapshot here was a full copy of the array per loop,
                // and a semantic the other tier no longer has.
                if let Value::Array(a) = &iterable {
                    let a = a.clone();
                    let mut ix = 0usize;
                    loop {
                        let item = {
                            let items = a.borrow();
                            if ix >= items.len() {
                                break;
                            }
                            items[ix].clone()
                        };
                        ix += 1;
                        let scope = child_env(env);
                        self.bind_pattern(target, item, &scope)?;
                        match loop_ctl(self.exec_stmt(body, &scope)?, label) {
                            LoopCtl::BreakLoop => break,
                            LoopCtl::NextIter => {}
                            LoopCtl::Out(sig) => return Ok(sig),
                        }
                    }
                    return Ok(Sig::Normal);
                }
                let items: Vec<Value> = self.iter_values(&iterable)?;
                for item in items {
                    let scope = child_env(env);
                    self.bind_pattern(target, item, &scope)?;
                    match loop_ctl(self.exec_stmt(body, &scope)?, label) {
                        LoopCtl::BreakLoop => break,
                        LoopCtl::NextIter => {}
                        LoopCtl::Out(sig) => return Ok(sig),
                    }
                }
                Ok(Sig::Normal)
            }
            Stmt::Switch { scrutinee, clauses } => {
                let v = self.eval(scrutinee, env)?;
                let scope = child_env(env);
                let mut matched = clauses.len();
                for (i, c) in clauses.iter().enumerate() {
                    if let Some(t) = &c.test {
                        let tv = self.eval(t, &scope)?;
                        if self.values_equal(&v, &tv)? {
                            matched = i;
                            break;
                        }
                    }
                }
                if matched == clauses.len() {
                    if let Some(i) = clauses.iter().position(|c| c.test.is_none()) {
                        matched = i;
                    }
                }
                'clauses: for c in clauses.iter().skip(matched) {
                    for s in &c.body {
                        match self.exec_stmt(s, &scope)? {
                            Sig::Normal => {}
                            Sig::Break(None) => break 'clauses,
                            other => return Ok(other),
                        }
                    }
                }
                Ok(Sig::Normal)
            }
            Stmt::Break { label, .. } => Ok(Sig::Break(label.as_ref().map(|l| l.text.clone()))),
            Stmt::Continue { label, .. } => {
                Ok(Sig::Continue(label.as_ref().map(|l| l.text.clone())))
            }
            Stmt::Return { value, .. } => {
                let v = match value {
                    Some(e) => self.eval(e, env)?,
                    None => Value::Null,
                };
                Ok(Sig::Return(v))
            }
            Stmt::Throw(e) => {
                let v = self.eval(e, env)?;
                Err(Thrown(v))
            }
            Stmt::Try {
                block,
                catches,
                finally,
            } => {
                let result = self.exec_block(block, env);
                let result = match result {
                    Err(thrown) => {
                        let mut handled = None;
                        for c in catches {
                            if self.catch_matches(&c.ty, &thrown.0) {
                                let scope = child_env(env);
                                env_define(&scope, &c.name.text, thrown.0.clone());
                                handled = Some(self.exec_block_in(&c.block, &scope));
                                break;
                            }
                        }
                        handled.unwrap_or(Err(thrown))
                    }
                    ok => ok,
                };
                if let Some(f) = finally {
                    match self.exec_block(f, env)? {
                        Sig::Normal => {}
                        other => return Ok(other), // finally overrides
                    }
                }
                result
            }
            Stmt::Labeled { label: l, body } => {
                // The loop consumes matching break/continue signals itself.
                self.exec_stmt_l(body, env, Some(&l.text))
            }
        }
    }

    fn exec_block_in(&mut self, stmts: &'static [Stmt], scope: &Env) -> SResult {
        for s in stmts {
            match self.exec_stmt(s, scope)? {
                Sig::Normal => {}
                other => return Ok(other),
            }
        }
        Ok(Sig::Normal)
    }

    fn catch_matches(&self, ty: &TypeExpr, thrown: &Value) -> bool {
        let want = match ty {
            TypeExpr::Named { name, .. } => name.as_str(),
            _ => return false,
        };
        if want == "Error" {
            return true;
        }
        if let Value::Instance(i) = thrown {
            let mut cls = Some(i.borrow().class.clone());
            while let Some(c) = cls {
                if c.name == want {
                    return true;
                }
                cls = c.parent.clone();
            }
        }
        false
    }

    fn exec_var(&mut self, v: &'static VarStmt, env: &Env) -> Result<(), Thrown> {
        for b in &v.bindings {
            let value = match &b.init {
                Some(e) => self.eval(e, env)?,
                // No initializer: the binding starts at its type's zero — 0, "",
                // '\0', false, or a fresh empty container — and at `null` only
                // when the type has no zero (a class, an interface, `T?`).
                None => match b.ty.as_ref().and_then(check::default_for_ty) {
                    Some(d) => default_value(d),
                    None => Value::Null,
                },
            };
            self.bind_pattern(&b.target, value, env)?;
        }
        Ok(())
    }

    fn bind_pattern(&mut self, p: &'static Pattern, value: Value, env: &Env) -> Result<(), Thrown> {
        match p {
            Pattern::Name(n) => {
                env_define(env, &n.text, value);
                Ok(())
            }
            Pattern::Array { elems, rest } => {
                let items: Vec<Value> = match &value {
                    Value::Array(a) => a.borrow().clone(),
                    Value::Str(s) => char::decode_utf16(s.iter().copied())
                        .map(|r| Value::Char(r.unwrap_or('\u{FFFD}')))
                        .collect(),
                    _ => return self.type_error("cannot destructure a non-array"),
                };
                for (i, e) in elems.iter().enumerate() {
                    let mut v = items.get(i).cloned().unwrap_or(Value::Null);
                    if matches!(v, Value::Null) {
                        if let Some(d) = &e.default {
                            v = self.eval(d, env)?;
                        }
                    }
                    self.bind_pattern(&e.target, v, env)?;
                }
                if let Some(r) = rest {
                    let tail: Vec<Value> = items.iter().skip(elems.len()).cloned().collect();
                    self.bind_pattern(r, new_array(tail), env)?;
                }
                Ok(())
            }
            Pattern::Record(fields) => {
                for f in fields {
                    let mut v = self
                        .get_member(&value, &f.name.text)?
                        .unwrap_or(Value::Null);
                    if matches!(v, Value::Null) {
                        if let Some(d) = &f.default {
                            v = self.eval(d, env)?;
                        }
                    }
                    match &f.target {
                        Some(t) => self.bind_pattern(t, v, env)?,
                        None => env_define(env, &f.name.text, v),
                    }
                }
                Ok(())
            }
        }
    }

    // ---- calls --------------------------------------------------------------------

    fn call_closure(&mut self, c: &Closure, args: Vec<Value>) -> VResult {
        // The tree-walker recurses on the Rust stack for a Mersey call (the VM
        // and JIT loop instead, and reach `MAX_CALL_DEPTH`). Unbounded recursion
        // would overflow the Rust stack — a process abort, not a catchable
        // exception — so the depth is a budget the engine enforces: past it, an
        // ordinary `RangeError`. `MAX_CALL_DEPTH` is the single limit both tiers
        // share, so they agree on exactly which recursions throw.
        if self.depth >= MAX_CALL_DEPTH {
            return Err(self.throw("RangeError", "maximum call depth exceeded"));
        }
        self.depth += 1;
        // Grow the native stack on demand so the tree-walker can actually reach
        // `MAX_CALL_DEPTH`: its per-Mersey-frame Rust stack is large (a match arm
        // over every expression/statement), so a fixed stack would run out dozens
        // of frames in — long before the depth limit, and long before the VM
        // does. `maybe_grow` allocates a fresh segment only when the remaining
        // stack dips below the red zone; on targets that cannot grow, it runs in
        // place (the depth limit still bounds recursion). Chosen so a segment
        // holds many frames and the red zone clears several.
        let out = stacker::maybe_grow(512 * 1024, 4 * 1024 * 1024, || {
            self.call_closure_inner(c, args)
        });
        self.depth -= 1;
        out
    }

    fn call_closure_inner(&mut self, c: &Closure, args: Vec<Value>) -> VResult {
        // The fast path: a body with nothing in the environment needs no
        // environment. No `Scope`, so no `Rc`, no `GcCell`, no `HashMap`, and
        // nothing handed to the collector — and the arguments go straight into
        // the frame slots the compiler gave them instead of being inserted into
        // a map by name and hashed back out again.
        //
        // Names that are not locals still resolve: the chain this runs against is
        // the closure's own environment, whose root is the globals.
        if self.use_vm && !c.data.is_async {
            if let Some(chunk) = self.chunk_of(c) {
                if chunk.needs_env
                    || !chunk.simple_params
                    || args.len() != c.data.params.len()
                    || vm::chunk_yields(&chunk)
                {
                    // Not a candidate; fall through to the general path below.
                } else {
                    return self.call_fast(c, chunk, args);
                }
            }
        }
        let scope = child_env(&c.env);
        self.bind_params(c.data.params, args, &scope)?;
        if let Some(this) = &c.this {
            env_define(&scope, "this", this.clone());
        }
        // A generator (its body contains `yield`) returns an iterator: the
        // body doesn't run until the first `next()`. Like async functions,
        // generators must run on the VM — only it can suspend.
        if !c.data.is_async {
            let cached = c.data.chunk.borrow().clone();
            let compiled = match cached {
                Some(x) => x,
                None => {
                    let module = self.current_module.clone();
                    let out = vm::compile_fn_in(&c.data.body, &module, c.data.params);
                    *c.data.chunk.borrow_mut() = Some(out.clone());
                    out
                }
            };
            if let Some(chunk) = compiled {
                if vm::chunk_yields(&chunk) {
                    let coro = Coro {
                        gen: None,
                        frame: vm::new_frame(&chunk, &scope, c.this.as_ref()),
                        chunk,
                        pc: 0,
                        stack: Vec::new(),
                        scopes: vec![scope],
                        handlers: Vec::new(),
                        cls: c.cls.clone(),
                        result: PromiseState::pending(),
                    };
                    let g = Rc::new(GcCell::new(GenState {
                        coro: Some(coro),
                        done: false,
                        is_async: false,
                        pending: None,
                        adapter: None,
                    }));
                    gc::track_gen(&g);
                    return Ok(Value::IterV(g));
                }
            }
        }
        // Async functions always run on the bytecode VM: `await` suspends by
        // capturing VM state, which the AST walker cannot do. (Both tiers
        // therefore agree on async semantics by construction.)
        if c.data.is_async {
            let cached = c.data.chunk.borrow().clone();
            let compiled = match cached {
                Some(x) => x,
                None => {
                    let module = self.current_module.clone();
                    let out = vm::compile_fn_in(&c.data.body, &module, c.data.params);
                    *c.data.chunk.borrow_mut() = Some(out.clone());
                    out
                }
            };
            let Some(chunk) = compiled else {
                return self.type_error(
                    "this async function uses a construct the compiler cannot suspend",
                );
            };
            // An `async` function that yields is an async generator: one
            // coroutine that both awaits and yields. Its `next()` hands back a
            // promise, which settles when the body reaches the next `yield`.
            if vm::chunk_yields(&chunk) {
                let coro = Coro {
                    gen: None,
                    frame: vm::new_frame(&chunk, &scope, c.this.as_ref()),
                    chunk,
                    pc: 0,
                    stack: Vec::new(),
                    scopes: vec![scope],
                    handlers: Vec::new(),
                    cls: c.cls.clone(),
                    result: PromiseState::pending(),
                };
                let g = Rc::new(GcCell::new(GenState {
                    coro: Some(coro),
                    done: false,
                    is_async: true,
                    pending: None,
                    adapter: None,
                }));
                gc::track_gen(&g);
                return Ok(Value::IterV(g));
            }
            return self.start_coro(c, chunk, scope);
        }
        if self.use_vm {
            let cached = c.data.chunk.borrow().clone();
            let compiled = match cached {
                Some(x) => x,
                None => {
                    let module = self.current_module.clone();
                    let out = vm::compile_fn_in(&c.data.body, &module, c.data.params);
                    *c.data.chunk.borrow_mut() = Some(out.clone());
                    out
                }
            };
            if let Some(chunk) = compiled {
                // Tier 1: hot kernels run native (Phase 4). The arguments are read
                // back out of the scope they were just bound into, so defaults and
                // destructuring stayed with `bind_params` and this path sees only
                // finished values.
                if self.jit.is_some() {
                    let args: Option<Vec<Value>> = simple_param_names(c.data.params)
                        .map(|names| names.iter().filter_map(|n| env_get(&scope, n)).collect());
                    if let Some(args) = args.filter(|a| a.len() == c.data.params.len()) {
                        if let Some(v) = self.try_jit_args(
                            &chunk,
                            c.data.params,
                            c.data.ret_num,
                            c.data.ret_bool,
                            c.data.ret_ty(),
                            c.this.as_ref(),
                            &args,
                            Some(c.env.clone()),
                        )? {
                            return Ok(v);
                        }
                    }
                }
                // The signature travels with the call so a loop inside this body
                // can be compiled and resumed at its header (OSR) without waiting
                // for the function to be called again — it may never be.
                let osr = if self.jit.is_some() {
                    Some(vm::OsrCtx {
                        params: c.data.params,
                        ret: c.data.ret_num,
                        ret_bool: c.data.ret_bool,
                        ret_ty: c.data.ret_ty(),
                        this: match &c.this {
                            Some(Value::Instance(i)) => Some(i.borrow().class.clone()),
                            _ => None,
                        },
                    })
                } else {
                    None
                };
                self.push_frame(&c.data.name, &chunk.module);
                let out = {
                    let frame = Frame::enter(self, c, &scope);
                    let f = vm::new_frame(&chunk, &scope, c.this.as_ref());
                    vm::run_chunk(frame.i, &chunk, scope, f, osr)
                };
                self.pop_frame();
                return out;
            }
        }
        match &c.data.body {
            FnBody::Expr(e) => {
                let frame = Frame::enter(self, c, &scope);
                frame.i.eval(e, &scope)
            }
            FnBody::Block(stmts) => {
                let frame = Frame::enter(self, c, &scope);
                match frame.i.exec_block_in(stmts, &scope)? {
                    Sig::Return(v) => Ok(v),
                    _ => Ok(Value::Null),
                }
            }
        }
    }

    /// A method's class, on the stack for `super` while the method runs.
    pub(crate) fn globals_env(&self) -> Env {
        self.globals.clone()
    }

    pub(crate) fn class_stack_push(&mut self, cls: Rc<ClassDef>) {
        self.class_stack.push(cls);
    }

    pub(crate) fn class_stack_pop(&mut self) {
        self.class_stack.pop();
    }

    /// The method `name` on this class, and the class that declares it (which is
    /// what `super` inside it will look above).
    ///
    /// A method call used to walk the whole of `call_member` — past iterators,
    /// promises, arrays, strings — and *then* search the class chain, on every
    /// call. It is 169ns against 70ns for a plain function, and a method call is
    /// what object-oriented code is made of.
    pub(crate) fn method_of(
        &self,
        cls: &Rc<ClassDef>,
        name: &str,
    ) -> Option<(Rc<FnData>, Rc<ClassDef>)> {
        find_in_chain(cls, |c| c.methods.get(name).map(|d| (d.clone(), c.clone())))
    }

    /// Can this call run *inside* the interpreter's loop, rather than by
    /// re-entering it?
    ///
    /// The same conditions as the environment-free fast path — nothing in the
    /// environment, plain parameters, not a generator, not async — plus the
    /// arity, because a missing argument is a `null` and this path does not bind
    /// defaults. Anything else goes the long way.
    pub(crate) fn inlinable(&mut self, c: &Closure, argc: usize) -> Option<Rc<vm::Chunk>> {
        if !self.use_vm || c.data.is_async {
            return None;
        }
        let chunk = self.chunk_of(c)?;
        if chunk.needs_env || !chunk.simple_params || chunk.yields || argc != c.data.params.len() {
            return None;
        }
        Some(chunk)
    }

    pub(crate) fn jit_enabled(&self) -> bool {
        self.jit.is_some()
    }

    /// Does any class *below* `cls` override `name`?
    ///
    /// This is the whole of method dispatch in Tier 1. If the answer is no, then
    /// every instance a `cls`-typed expression can hold — `cls` itself or any
    /// subclass — runs the same method body, and the call compiles to a direct
    /// jump: no vtable load, no inline cache, no class check, and so no deopt.
    /// If the answer is yes, the function stays in Tier 0.
    ///
    /// A JS engine cannot ask this question, because the answer changes when
    /// someone assigns to a prototype. Mersey deleted that (§4.1), and this is
    /// what it bought.
    fn overridden_below(&self, cls: &Rc<ClassDef>, name: &str) -> bool {
        self.all_classes
            .iter()
            .any(|k| !Rc::ptr_eq(k, cls) && k.descends_from(cls) && k.declares_method(name))
    }

    /// Count a call for Tier 1, and run it natively if it is hot and compiled.
    /// `Ok(None)` means the interpreter should run it.
    pub(crate) fn jit_call(
        &mut self,
        chunk: &Rc<vm::Chunk>,
        c: &Closure,
        args: &[Value],
    ) -> Result<Option<Value>, Thrown> {
        if self.jit.is_none() {
            return Ok(None);
        }
        self.try_jit_args(
            chunk,
            c.data.params,
            c.data.ret_num,
            c.data.ret_bool,
            c.data.ret_ty(),
            c.this.as_ref(),
            args,
            Some(c.env.clone()),
        )
    }

    /// `time.now()` / `time.monotonic()` from compiled code.
    /// The `std:` namespaces whose members compiled code may call through the
    /// native shim. `math` and `time` are absent on purpose: they have their own
    /// lowerings (instructions, and a numeric shim) that are strictly better
    /// than a general call. Everything here goes through the interpreter's own
    /// `call_native`, so behaviour is the interpreter's by construction.
    pub const NATIVE_NS: &[&str] = &["random", "bytes", "parse", "json", "hash"];

    /// The natives a *compiled* loop sits on, in id order.
    ///
    /// `call_native` dispatches on the name, and that match is a decision tree
    /// over forty-odd string literals — affordable once, not on every iteration
    /// of a compiled loop, where it measured 20% of `random.fill(buf)`. Tier 1
    /// resolves the name to one of these ids at compile time (`native_fast_id`)
    /// and the shim switches on the integer. There is one implementation of each
    /// native, here: `call_native`'s arms delegate to it.
    pub const NATIVE_FAST: &[&str] = &[
        "random.fill",
        "bytes.encodeUtf8",
        "bytes.decodeUtf8",
        "parse.url",
    ];

    /// `NATIVE_FAST`'s index for `name`, or `u32::MAX` for "not in the fast set".
    pub fn native_fast_id(name: &str) -> u32 {
        match Self::NATIVE_FAST.iter().position(|n| *n == name) {
            Some(i) => i as u32,
            None => u32::MAX,
        }
    }

    /// A string method from compiled code, by arena handle.
    ///
    /// There is one implementation of these — `call_member`'s — and this is how
    /// compiled code reaches it. Tier 1 knows from its own table what each method
    /// gives back (the checker's `string` member types are where that table comes
    /// from), so the two entry points below unwrap to the shape it is expecting
    /// and report a mismatch as a thrown `TypeError` rather than a silent zero.
    fn jit_str_call(&mut self, recv: u64, name: &str, args: &[u64]) -> Result<Value, ()> {
        let recv_v = self.jit_arena.get(recv).cloned().unwrap_or(Value::Null);
        let argv: Vec<Value> = args
            .iter()
            .map(|h| self.jit_arena.get(*h).cloned().unwrap_or(Value::Null))
            .collect();
        match self.call_member(&recv_v, name, argv) {
            Ok(v) => Ok(v),
            Err(t) => {
                self.jit_host_error = Some(t);
                Err(())
            }
        }
    }

    /// …one that answers with a number or a bool (`indexOf`, `startsWith`).
    /// `i64::MIN` means it threw, as it does for a numeric web property.
    pub fn jit_str_num(&mut self, recv: u64, name: &str, args: &[u64]) -> i64 {
        match self.jit_str_call(recv, name, args) {
            Ok(Value::I32(n)) => n as i64,
            Ok(Value::Bool(b)) => i64::from(b),
            Ok(_) => {
                let t = self.throw("TypeError", format!("string.{name} gave no number"));
                self.jit_host_error = Some(t);
                i64::MIN
            }
            Err(()) => i64::MIN,
        }
    }

    /// …and one that answers with a *nullable* number (`codePointAt`). `out` gets
    /// the value, or `i64::MIN` for null; the return is 0, or 1 if it threw —
    /// which is why the two are separate, null being an ordinary answer here and
    /// not an error.
    pub fn jit_str_numopt(&mut self, recv: u64, name: &str, args: &[u64], out: &mut i64) -> i64 {
        match self.jit_str_call(recv, name, args) {
            Ok(Value::I32(n)) => {
                *out = n as i64;
                0
            }
            Ok(Value::Null) => {
                *out = i64::MIN;
                0
            }
            Ok(_) => {
                let t = self.throw("TypeError", format!("string.{name} gave no number"));
                self.jit_host_error = Some(t);
                1
            }
            Err(()) => 1,
        }
    }

    /// `throw new Error(msg)` from compiled code: build the error here and stash
    /// it, exactly as a failed host call does, so the compiled body only has to
    /// trap. The class name is one of the few `throw` knows how to build.
    pub fn jit_throw_error(&mut self, class: &'static str, msg: &[u16]) {
        let t = self.throw(class, utf16_to_string(msg));
        self.jit_host_error = Some(t);
    }

    /// The same, from a `Value` the caller already holds — a heap cell's contents,
    /// read in place. This is the fused `this.u.pathname`: the field read makes no
    /// arena entry of its own, which is one `keep` and one `release` saved per
    /// part read, and a `Value` clone with them.
    ///
    pub fn jit_prop_str_of(&mut self, v: &Value, name: &str) -> u64 {
        match self.get_member(v, name) {
            Ok(Some(s @ Value::Str(_))) => self.jit_arena.keep(s),
            Ok(_) => 0,
            Err(t) => {
                self.jit_host_error = Some(t);
                u64::MAX
            }
        }
    }

    /// A string-valued property of an opaque (`u.pathname` on a `Url`). The
    /// handle of the resulting string, 0 if the property is absent or is not a
    /// string, `u64::MAX` if reading it threw.
    pub fn jit_val_prop_str(&mut self, h: u64, name: &str) -> u64 {
        let o = self.jit_arena.get(h).cloned().unwrap_or(Value::Null);
        match self.get_member(&o, name) {
            Ok(Some(v @ Value::Str(_))) => self.jit_arena.keep(v),
            Ok(_) => 0,
            Err(t) => {
                self.jit_host_error = Some(t);
                u64::MAX
            }
        }
    }

    /// …and one that answers with a value of no particular shape — an opaque, or
    /// nothing at all (`push` is void, and the interpreter's nothing is `null`).
    /// The handle is 0 for null and `u64::MAX` if it threw.
    pub fn jit_member_val(&mut self, recv: u64, name: &str, args: &[u64]) -> u64 {
        match self.jit_str_call(recv, name, args) {
            Ok(Value::Null) => 0,
            Ok(v) => self.jit_arena.keep(v),
            Err(()) => u64::MAX,
        }
    }

    /// …and one that answers with a string, parked in the arena. `u64::MAX` means
    /// it threw; the handle is the compiled code's to release.
    pub fn jit_str_str(&mut self, recv: u64, name: &str, args: &[u64]) -> u64 {
        match self.jit_str_call(recv, name, args) {
            Ok(v @ Value::Str(_)) => self.jit_arena.keep(v),
            Ok(_) => {
                let t = self.throw("TypeError", format!("string.{name} gave no string"));
                self.jit_host_error = Some(t);
                u64::MAX
            }
            Err(()) => u64::MAX,
        }
    }

    /// `[]` from compiled code: a fresh array, parked in the arena.
    ///
    /// Compiled code carries it as an *opaque* rather than as `Ty::Arr`. An array
    /// that grows cannot use that shape — it caches the element buffer's address
    /// and length, and a `push` moves both — so a growable one goes through the
    /// same shims a `Bytes` does. `index_get`, `index_set` and `length` already
    /// answer for an array, so only `push` is new.
    pub fn jit_array_new(&mut self, kind: i64) -> u64 {
        let v = match kind {
            1 => new_map(Vec::new()),
            2 => new_set(Vec::new()),
            _ => new_array(Vec::new()),
        };
        self.jit_arena.keep(v)
    }

    /// `a.push(v)` for a numeric element. 0, or 1 if the handle names no array.
    pub fn jit_array_push(&mut self, h: u64, kind: i64, bits: i64) -> i64 {
        let v = match kind {
            1 => Value::F64(f64::from_bits(bits as u64)),
            _ => Value::I32(bits as i32),
        };
        match self.jit_arena.get(h) {
            Some(Value::Array(a)) => {
                a.borrow_mut().push(v);
                0
            }
            _ => 1,
        }
    }

    /// `a[i]` on an opaque known to hold *strings* — a `split` result. The handle
    /// of the element, 0 for a missing or non-string one (which reads as a null
    /// string), `u64::MAX` if the index threw.
    pub fn jit_val_index_str(&mut self, h: u64, idx: i64) -> u64 {
        let o = self.jit_arena.get(h).cloned().unwrap_or(Value::Null);
        match self.index_get(&o, &Value::I64(idx)) {
            Ok(v @ Value::Str(_)) => self.jit_arena.keep(v),
            Ok(_) => 0,
            Err(t) => {
                self.jit_host_error = Some(t);
                u64::MAX
            }
        }
    }

    /// `b[i]` on an opaque (a `Bytes`) from compiled code. `i64::MIN` means it
    /// threw — out of range, or not something indexable — with the error stashed,
    /// so the message is the interpreter's own, down to the length it reports.
    pub fn jit_val_index_get(&mut self, h: u64, idx: i64) -> i64 {
        let o = self.jit_arena.get(h).cloned().unwrap_or(Value::Null);
        match self.index_get(&o, &Value::I64(idx)) {
            Ok(v) => as_i64(&v).unwrap_or(i64::MIN),
            Err(t) => {
                self.jit_host_error = Some(t);
                i64::MIN
            }
        }
    }

    /// …and `b[i] = v`. 0 on success, 1 if it threw.
    pub fn jit_val_index_set(&mut self, h: u64, idx: i64, v: i64) -> i64 {
        let o = self.jit_arena.get(h).cloned().unwrap_or(Value::Null);
        match self.index_set(&o, &Value::I64(idx), Value::I64(v)) {
            Ok(()) => 0,
            Err(t) => {
                self.jit_host_error = Some(t);
                1
            }
        }
    }

    /// `random.fill(buf)` straight from compiled code.
    ///
    /// The general native path is name (or id) plus an argument array plus a
    /// lend/give-back plus a `Result<Value, _>` — 18% of a compiled iteration of
    /// this call, against 66% for the ChaCha it exists to run. None of that is
    /// needed here: one opaque argument, no result. This is the same "typed bind"
    /// idea the web tier uses for `fillRect`, applied to the one native a
    /// compiled loop can sit on this tightly.
    ///
    /// The arena is read while the host is written, which the borrow checker
    /// allows because they are disjoint *fields* of `Interp` — the reason the
    /// general path has to move the value out instead of borrowing it.
    /// Returns 0 on success, 1 if it threw (the error is stashed as usual).
    pub fn jit_random_fill(&mut self, handle: u64) -> i64 {
        let Some(Value::Bytes(b)) = self.jit_arena.get(handle) else {
            let t = self.throw("TypeError", "random.fill needs a Bytes buffer");
            self.jit_host_error = Some(t);
            return 1;
        };
        let mut slot = b.borrow_mut();
        let r = self.host.random_fill(&mut slot);
        drop(slot);
        match r {
            Ok(()) => 0,
            Err(msg) => {
                let t = self.throw("Error", msg);
                self.jit_host_error = Some(t);
                1
            }
        }
    }

    /// One of `NATIVE_FAST`, by id, with its arguments *borrowed*. `call_native`
    /// has to own its `Vec` (some natives consume their arguments); these four do
    /// not, which is what lets a compiled call avoid the allocation entirely.
    fn call_native_fast(&mut self, id: u32, args: &[Value]) -> VResult {
        match id {
            0 => {
                let Some(Value::Bytes(b)) = args.first() else {
                    return self.type_error("random.fill needs a Bytes buffer");
                };
                // No `Rc` clone: `args` is borrowed from the caller, not from
                // `self`, so the buffer's borrow and `self.host`'s are disjoint.
                // (It used to clone, from when this arm owned its `args` vector.)
                let mut slot = b.borrow_mut();
                match self.host.random_fill(&mut slot) {
                    Ok(()) => {
                        drop(slot);
                        Ok(Value::Null)
                    }
                    Err(msg) => {
                        drop(slot);
                        Err(self.throw("Error", msg))
                    }
                }
            }
            1 => {
                let Some(Value::Str(s)) = args.first() else {
                    return self.type_error("bytes.encodeUtf8 needs a string");
                };
                // No intermediate `String`: that one was allocated, validated,
                // and then had its validation discarded by `into_bytes`.
                Ok(Value::Bytes(Rc::new(RefCell::new(utf16_to_utf8_bytes(s)))))
            }
            2 => {
                let Some(Value::Bytes(b)) = args.first() else {
                    return self.type_error("bytes.decodeUtf8 needs bytes");
                };
                // Invalid UTF-8 is `null`, not U+FFFD: a decode that quietly
                // succeeds on garbage is how corrupt data travels.
                //
                // Validated in place. `String::from_utf8(b.borrow().clone())`
                // copied the whole buffer first only to hand the copy back for
                // re-encoding — a payload-sized allocation and memcpy for
                // nothing.
                Ok(match std::str::from_utf8(&b.borrow()) {
                    Ok(text) => Value::Str(Rc::new(utf16(text))),
                    Err(_) => Value::Null,
                })
            }
            3 => {
                let Some(Value::Str(s)) = args.first() else {
                    return self.type_error("parse.url needs a string");
                };
                // Converted into a buffer this interpreter keeps. `want_string`
                // made a fresh `String` every call, which for a URL parsed in a
                // loop is an allocation per URL for a value discarded immediately
                // after. `resize` on an already-large buffer only moves its
                // length. Three bytes per unit is the conversion's documented
                // worst case.
                let mut buf = std::mem::take(&mut self.utf8_scratch);
                buf.resize(s.len().saturating_mul(3).max(1), 0);
                let n = encoding_rs::mem::convert_utf16_to_utf8(s, &mut buf);
                // Absolute URLs only: a relative reference is not a URL until
                // you say what it is relative to, which this does not do.
                let out = match std::str::from_utf8(&buf[..n]) {
                    Ok(text) => match url::Url::parse(text.trim()) {
                        Ok(u) => Ok(Value::UrlV(Rc::new(u))),
                        Err(_) => Ok(Value::Null),
                    },
                    Err(_) => Ok(Value::Null),
                };
                self.utf8_scratch = buf;
                out
            }
            _ => unreachable!("id is an index into NATIVE_FAST"),
        }
    }

    pub fn jit_time_ms(&mut self, epoch: bool) -> f64 {
        self.host.time_ms(epoch)
    }

    /// A top-level binding whose value compiled code carries opaquely, parked in
    /// the arena so a handle names it. Handle 0 means "not one of those", and
    /// the caller bails.
    /// A top-level binding holding a string, parked so its buffer outlives any
    /// reassignment of the binding during the call. 0 if it is not one.
    pub fn jit_global_str(&mut self, name: &str) -> u64 {
        let env = self
            .jit_scope
            .clone()
            .unwrap_or_else(|| self.globals.clone());
        match env_get(&env, name) {
            Some(v @ Value::Str(_)) => self.jit_arena.keep(v),
            _ => 0,
        }
    }

    /// A top-level binding holding a number, as raw bits — an integer or a bool
    /// as itself, a `float64` as its IEEE pattern.
    ///
    /// Read *live*, once per use, rather than hoisted to the top of the call like
    /// the handles are: nothing here can tell a `const` from a `let`, and a `let`
    /// reassigned by something this call goes on to invoke would leave a hoisted
    /// copy stale. A shim call is the price of not having to know.
    pub fn jit_global_num(&self, name: &str) -> i64 {
        let env = self.jit_scope.as_ref().unwrap_or(&self.globals);
        match env_get(env, name) {
            Some(Value::I32(n)) => n as i64,
            Some(Value::I64(n)) => n,
            Some(Value::F64(f)) => f.to_bits() as i64,
            Some(Value::Bool(t)) => i64::from(t),
            _ => 0,
        }
    }

    pub fn jit_global_val(&mut self, name: &str) -> u64 {
        let env = self
            .jit_scope
            .clone()
            .unwrap_or_else(|| self.globals.clone());
        match env_get(&env, name) {
            Some(v @ (Value::Bytes(_) | Value::UrlV(_) | Value::RegexV(_))) => {
                self.jit_arena.keep(v)
            }
            _ => 0,
        }
    }

    /// Park a compiled string in the arena so a native can take it as an
    /// argument. Compiled code holds a string as a pointer and a length; a
    /// native wants a `Value`, and the arena is where the two meet.
    /// `have` is the handle the compiled string already owns, or 0 if it owns
    /// none (a constant, or a borrow). A *built* string is already parked here as
    /// a `Value::Str` — the very thing a native wants — so reusing that entry
    /// saves an `Rc` allocation and a payload-sized copy per call. Returns the
    /// handle to pass; the caller releases it only if it differs from `have`.
    pub fn jit_box_str(&mut self, units: &[u16], have: u64) -> u64 {
        if have != 0 && matches!(self.jit_arena.get(have), Some(Value::Str(_))) {
            return have;
        }
        // A string with no handle of its own is a constant, or a borrow of one:
        // `base.lastIndexOf(".")` in a loop would otherwise copy both the receiver
        // and the argument on every iteration, which costs more than the method.
        // So the last few are kept, keyed on where the units live and *verified by
        // content* — a freed buffer can be replaced by an unrelated string at the
        // same address, and a stale key must not hand back the wrong text.
        let key = units.as_ptr() as usize;
        if let Some((_, h)) = self.jit_str_memo.iter().find(|(k, _)| *k == key) {
            let h = *h;
            if matches!(self.jit_arena.get(h), Some(Value::Str(rc)) if rc.as_slice() == units) {
                return h;
            }
        }
        let h = self.jit_arena.keep(Value::Str(Rc::new(units.to_vec())));
        // Bounded, and what it displaces is released here — which is why nothing
        // parked by this function is the caller's to release. Eight is comfortably
        // more than the receiver plus arguments of any one call, so a handle in
        // flight is never the one evicted.
        const MEMO: usize = 8;
        if self.jit_str_memo.len() == MEMO {
            let (_, old) = self.jit_str_memo.remove(0);
            self.jit_arena.release(old);
        }
        self.jit_str_memo.push((key, h));
        h
    }

    /// The same for a number. `int32` and `float64` are separate because the
    /// language's `parse`/`bytes` members distinguish them, and a native handed
    /// the wrong one would silently do the wrong arithmetic.
    pub fn jit_box_i32(&mut self, n: i32) -> u64 {
        self.jit_arena.keep(Value::I32(n))
    }
    pub fn jit_box_f64(&mut self, n: f64) -> u64 {
        self.jit_arena.keep(Value::F64(n))
    }

    /// Call `ns.member(args)` — the general `std:` native path, the one thing
    /// that used to send a whole function back to the interpreter.
    ///
    /// Arguments and the result cross as arena handles, because that is the one
    /// representation that fits every `Value` without the tier having to model
    /// it. Returns the result's handle, or 0 for a void/null result; on a throw
    /// it stashes the error for `after_jit` and returns `u64::MAX`, since a shim
    /// cannot unwind through native frames.
    pub fn jit_native_call(&mut self, name: &str, id: u32, args: &[u64]) -> u64 {
        // A fast-set native takes its arguments from a stack array and reaches its
        // one implementation by integer switch. The general path below has to
        // build an owned `Vec` — `call_native` drops it, and some natives consume
        // their arguments — and then match the name against every native in the
        // language. On `random.fill(buf)` those two were 20% and 11% of a
        // compiled iteration respectively, against 61% for the ChaCha it exists
        // to run.
        let out = if id != u32::MAX && args.len() == 1 {
            // Every member of `NATIVE_FAST` takes exactly one argument, so there
            // is one value to hand over: no buffer to build, index and tear down.
            // And it is *lent*, not cloned — see `Arena::lend`.
            match self.jit_arena.lend(args[0]) {
                Some(v) => {
                    let r = self.call_native_fast(id, std::slice::from_ref(&v));
                    self.jit_arena.give_back(args[0], v);
                    r
                }
                // The handle names nothing — the native sees the `null` it would
                // have seen anyway, and reports its own type error.
                None => self.call_native_fast(id, &[Value::Null]),
            }
        } else {
            let argv: Vec<Value> = args
                .iter()
                .map(|h| self.jit_arena.get(*h).cloned().unwrap_or(Value::Null))
                .collect();
            self.call_native(name, None, argv)
        };
        match out {
            Ok(Value::Null) => 0,
            Ok(v) => self.jit_arena.keep(v),
            Err(t) => {
                self.jit_host_error = Some(t);
                u64::MAX
            }
        }
    }

    /// The handle a top-level web global currently holds (0 if it is somehow not
    /// a host object — impossible for a `JsRef`-typed binding, but the web call
    /// on handle 0 would simply fail and be raised like any host error).
    pub fn jit_global_web(&self, name: &str) -> i64 {
        let env = self.jit_scope.as_ref().unwrap_or(&self.globals);
        match env_get(env, name) {
            Some(Value::JsRef(h)) => h,
            _ => 0,
        }
    }

    /// A numeric-argument web method call whose result compiled code discards
    /// (`ctx.fillRect(x, y, w, h)`). The fast path builds the `WebArg`s on the
    /// stack (no heap for the common ≤8-argument case) and takes the same
    /// interned wide path the interpreter's `web_call` takes for all-scalar
    /// args, so behaviour is identical. Returns 0 on success; on a thrown error
    /// it stashes the value for `after_jit` and returns 1, since a shim cannot
    /// unwind through native frames.
    pub fn jit_web_call_num(&mut self, target: i64, name: &str, args: &[f64]) -> i64 {
        if args.len() <= 8 {
            if let Some(id) = self.intern(name) {
                let n = args.len();
                let buf: [WebArg; 8] =
                    std::array::from_fn(|k| WebArg::Num(if k < n { args[k] } else { 0.0 }));
                if let Some(reply) = self.host.web_call_u16(target, id, &buf[..n]) {
                    return match reply {
                        WebReply::Err(msg) => {
                            let t = self.throw("Error", msg);
                            self.jit_host_error = Some(t);
                            1
                        }
                        _ => 0,
                    };
                }
            }
        }
        // The host declined the wide path (or too many args): fall back to the
        // interpreter's general web_call, so nothing is ever left unhandled.
        let vals: Vec<Value> = args.iter().map(|f| Value::F64(*f)).collect();
        match self.web_call(target, name, vals) {
            Ok(_) => 0,
            Err(t) => {
                self.jit_host_error = Some(t);
                1
            }
        }
    }

    /// The typed-binding fast path for a compiled numeric web call
    /// (`ctx.fillRect(…)` as bind id `CANVAS2D_FILLRECT`). No intern, no
    /// `WebArg` marshalling, no string dispatch: the id and the `f64` arguments
    /// cross straight to the host. If the host has no typed binding at all
    /// (`web_bind` returns `None`), fall back to the ordinary interned path by
    /// name so nothing is ever left unhandled. Same error protocol as
    /// `jit_web_call_num` (0 ok, 1 threw-and-stashed).
    pub fn jit_web_bind(&mut self, target: i64, bind_id: u32, name: &str, args: &[f64]) -> i64 {
        if let Some(reply) = self.host.web_bind(target, bind_id, args) {
            return match reply {
                WebReply::Err(msg) => {
                    let t = self.throw("Error", msg);
                    self.jit_host_error = Some(t);
                    1
                }
                _ => 0,
            };
        }
        self.jit_web_call_num(target, name, args)
    }

    /// A web method call from compiled code whose result is a string (or null)
    /// captured by the caller (`getItem(k)`). Returns the reply value; a thrown
    /// call returns `None` after stashing the error. The shim turns the value
    /// into an arena-owned string for compiled code.
    pub fn jit_web_call_str_value(
        &mut self,
        target: i64,
        id: u32,
        name: &str,
        args: &[WebArg],
    ) -> Option<Value> {
        let id = if id != u32::MAX {
            Some(id)
        } else {
            self.intern(name)
        };
        if let Some(id) = id {
            if let Some(reply) = self.host.web_call_u16(target, id, args) {
                return match reply {
                    WebReply::Err(msg) => {
                        let t = self.throw("Error", msg);
                        self.jit_host_error = Some(t);
                        None
                    }
                    other => match self.value_from_reply(other) {
                        Ok(v) => Some(v),
                        Err(t) => {
                            self.jit_host_error = Some(t);
                            None
                        }
                    },
                };
            }
        }
        let vals: Vec<Value> = args.iter().map(webarg_to_value).collect();
        match self.web_call(target, name, vals) {
            Ok(v) => Some(v),
            Err(t) => {
                self.jit_host_error = Some(t);
                None
            }
        }
    }

    /// A numeric-valued web property read from compiled code (`buf.length`).
    /// Reuses the interpreter's `web_get`; a non-numeric or missing result comes
    /// back as 0 (the compiled site only asks for properties the checker typed
    /// as integers). Errors are stashed and signalled by returning `i64::MIN`,
    /// which the caller treats as "threw".
    pub fn jit_web_get_num(&mut self, target: i64, id: u32, name: &str) -> i64 {
        // With a pre-interned id, read the property straight through the host's
        // wide-get path — no intern, no `web_get` string dispatch.
        if id != u32::MAX {
            if let Some(reply) = self.host.web_get_u16(target, id) {
                return match reply {
                    WebReply::Num(n) => n as i64,
                    WebReply::Err(msg) => {
                        let t = self.throw("Error", msg);
                        self.jit_host_error = Some(t);
                        i64::MIN
                    }
                    _ => 0,
                };
            }
        }
        match self.web_get(target, name) {
            Ok(Value::I32(n)) => n as i64,
            Ok(Value::I64(n)) => n,
            Ok(Value::F64(f)) => f as i64,
            Ok(_) => 0,
            Err(t) => {
                self.jit_host_error = Some(t);
                i64::MIN
            }
        }
    }

    /// A web property set from compiled code (`el.textContent = str`). With a
    /// pre-interned id it crosses the wide-set path directly. Same 0/1 protocol.
    pub fn jit_web_set(&mut self, target: i64, id: u32, name: &str, value: &WebArg) -> i64 {
        let id = if id != u32::MAX {
            Some(id)
        } else {
            self.intern(name)
        };
        if let Some(id) = id {
            if let Some(reply) = self.host.web_set_u16(target, id, value) {
                return match reply {
                    WebReply::Err(msg) => {
                        let t = self.throw("Error", msg);
                        self.jit_host_error = Some(t);
                        1
                    }
                    _ => 0,
                };
            }
        }
        match self.web_set(target, name, webarg_to_value(value)) {
            Ok(()) => 0,
            Err(t) => {
                self.jit_host_error = Some(t);
                1
            }
        }
    }

    /// A host-constructor call from compiled code (`new URL(s)`). Interns the
    /// constructor name (or uses a pre-interned id), takes the same wide-arg
    /// `web_new` path the interpreter's `new_named` reaches for any non-class
    /// name, and returns the resulting handle value. `None` on a throw (stashed).
    pub fn jit_web_new_value(&mut self, id: u32, name: &str, args: &[WebArg]) -> Option<Value> {
        let id = if id != u32::MAX {
            Some(id)
        } else {
            self.intern(name)
        };
        if let Some(id) = id {
            if let Some(reply) = self.host.web_new_u16(id, args) {
                return match reply {
                    WebReply::Err(msg) => {
                        let t = self.throw("Error", msg);
                        self.jit_host_error = Some(t);
                        None
                    }
                    other => match self.value_from_reply(other) {
                        Ok(v) => Some(v),
                        Err(t) => {
                            self.jit_host_error = Some(t);
                            None
                        }
                    },
                };
            }
        }
        // The wide path is unavailable: fall back to the interpreter's own
        // `new_named`/`web_new` (UTF-8 scalar or reflective), so behaviour is
        // identical to the uncompiled call.
        let vals: Vec<Value> = args.iter().map(webarg_to_value).collect();
        match self.web_new(name, vals) {
            Ok(v) => Some(v),
            Err(t) => {
                self.jit_host_error = Some(t);
                None
            }
        }
    }

    /// A string-valued web property read from compiled code (`url.pathname`).
    /// Reuses the interpreter's `web_get`; a non-string or missing result comes
    /// back as `None`-string (the compiled site only asks for properties known to
    /// return strings). `None` signals a throw (stashed for `after_jit`).
    pub fn jit_web_get_str_value(&mut self, target: i64, id: u32, name: &str) -> Option<Value> {
        if id != u32::MAX {
            if let Some(reply) = self.host.web_get_u16(target, id) {
                return match reply {
                    WebReply::Err(msg) => {
                        let t = self.throw("Error", msg);
                        self.jit_host_error = Some(t);
                        None
                    }
                    other => match self.value_from_reply(other) {
                        Ok(v) => Some(v),
                        Err(t) => {
                            self.jit_host_error = Some(t);
                            None
                        }
                    },
                };
            }
        }
        match self.web_get(target, name) {
            Ok(v) => Some(v),
            Err(t) => {
                self.jit_host_error = Some(t);
                None
            }
        }
    }

    /// The length of a string-valued web property, without materializing the
    /// string (`url.pathname.length`). The compiler fuses the read and the
    /// `.length` when the string flows straight into it, so the arena never keeps
    /// a string it would drop next instruction. A null result throws the same
    /// `TypeError` reading `.length` off `null` would; errors return `i64::MIN`.
    pub fn jit_web_get_str_len(&mut self, target: i64, id: u32, name: &str) -> i64 {
        let reply = if id != u32::MAX {
            self.host.web_get_u16(target, id)
        } else {
            None
        };
        let value = match reply {
            Some(WebReply::Str(v)) => return v.len() as i64,
            Some(WebReply::Null) => Value::Null,
            Some(WebReply::Err(msg)) => {
                let t = self.throw("Error", msg);
                self.jit_host_error = Some(t);
                return i64::MIN;
            }
            Some(other) => match self.value_from_reply(other) {
                Ok(v) => v,
                Err(t) => {
                    self.jit_host_error = Some(t);
                    return i64::MIN;
                }
            },
            // No wide path: fall back to the interpreter's string `web_get`.
            None => match self.web_get(target, name) {
                Ok(v) => v,
                Err(t) => {
                    self.jit_host_error = Some(t);
                    return i64::MIN;
                }
            },
        };
        match value {
            Value::Str(s) => s.len() as i64,
            // `.length` off a non-string (a null property) is the same throw the
            // two-op interpreter path would raise on the `GetMember(length)`.
            other => {
                let t = self.throw(
                    "TypeError",
                    format!("cannot read `length` of {}", kind_of(&other)),
                );
                self.jit_host_error = Some(t);
                i64::MIN
            }
        }
    }

    /// A web method call from compiled code whose arguments are already built
    /// (numbers, host handles, strings) and whose result is discarded
    /// (`getRandomValues(buf)`, `appendChild(el)`). Same 0/1 protocol as
    /// `jit_web_call_num`.
    pub fn jit_web_call_args(&mut self, target: i64, id: u32, name: &str, args: &[WebArg]) -> i64 {
        let id = if id != u32::MAX {
            Some(id)
        } else {
            self.intern(name)
        };
        if let Some(id) = id {
            if let Some(reply) = self.host.web_call_u16(target, id, args) {
                return match reply {
                    WebReply::Err(msg) => {
                        let t = self.throw("Error", msg);
                        self.jit_host_error = Some(t);
                        1
                    }
                    _ => 0,
                };
            }
        }
        let vals: Vec<Value> = args.iter().map(webarg_to_value).collect();
        match self.web_call(target, name, vals) {
            Ok(_) => 0,
            Err(t) => {
                self.jit_host_error = Some(t);
                1
            }
        }
    }

    /// Build and stash the thrown value for a host error the *direct* binding
    /// path reported (reply tag 5). That path skips the interpreter, so it comes
    /// back here only to construct the throw — no second call to the host — and
    /// returns 1, the same "threw" signal `jit_web_bind` uses.
    pub fn jit_stash_host_error(&mut self, msg: &[u16]) -> i64 {
        let t = self.throw("Error", utf16_to_string(msg));
        self.jit_host_error = Some(t);
        1
    }

    /// This closure's compiled body, compiling it once on first use.
    fn chunk_of(&mut self, c: &Closure) -> Option<Rc<vm::Chunk>> {
        if let Some(cached) = c.data.chunk.borrow().clone() {
            return cached;
        }
        let module = self.current_module.clone();
        let out = vm::compile_fn_in(&c.data.body, &module, c.data.params);
        *c.data.chunk.borrow_mut() = Some(out.clone());
        out
    }

    /// A call with no environment. See `call_closure_inner`.
    fn call_fast(&mut self, c: &Closure, chunk: Rc<vm::Chunk>, args: Vec<Value>) -> VResult {
        let jittable = self.jit.is_some();
        if jittable {
            if let Some(v) = self.try_jit_args(
                &chunk,
                c.data.params,
                c.data.ret_num,
                c.data.ret_bool,
                c.data.ret_ty(),
                c.this.as_ref(),
                &args,
                Some(c.env.clone()),
            )? {
                return Ok(v);
            }
        }
        let osr = if jittable {
            Some(vm::OsrCtx {
                params: c.data.params,
                ret: c.data.ret_num,
                ret_bool: c.data.ret_bool,
                ret_ty: c.data.ret_ty(),
                this: match &c.this {
                    Some(Value::Instance(i)) => Some(i.borrow().class.clone()),
                    _ => None,
                },
            })
        } else {
            None
        };
        let frame = vm::arg_frame(&chunk, args, c.this.as_ref());
        self.push_frame(&c.data.name, &chunk.module);
        let out = {
            let f = Frame::enter(self, c, &c.env.clone());
            // The closure's own environment *is* the scope: there is nothing to
            // put in a fresh one.
            vm::run_chunk(f.i, &chunk, c.env.clone(), frame, osr)
        };
        self.pop_frame();
        out
    }

    /// Attempt a Tier 1 native call: count the call site, compile once hot, and
    /// dispatch when every argument is what the compiled code expects.
    #[allow(clippy::too_many_arguments)]
    fn try_jit_args(
        &mut self,
        chunk: &Rc<vm::Chunk>,
        params: &'static [Param],
        ret_num: Option<mersey_front::check::Num>,
        ret_bool: bool,
        // The declared return type, *unresolved* — resolving it is a lookup by
        // name through the scope chain, and only the compile path below needs
        // the answer. See `vm::OsrCtx::ret_ty`.
        ret_ty: Option<&'static TypeExpr>,
        this: Option<&Value>,
        args: &[Value],
        scope: Option<Env>,
    ) -> Result<Option<Value>, Thrown> {
        if !self.count_call(chunk) {
            return Ok(None);
        }
        // The receiver's class *id*, not the class: on the path this function
        // takes most often — a chunk Tier 1 has already refused — the id is all
        // that is wanted, and cloning the `Rc` to get it is pure cost.
        let cls_id = match this {
            Some(Value::Instance(i)) => i.borrow().class.id,
            Some(_) => return Ok(None),
            None => 0,
        };
        // The refusal, remembered on the chunk itself: a `Cell` read and one
        // comparison. The `jit_cache` below answers the same question, but
        // getting there costs a receiver `Rc` clone and a hash of the key —
        // affordable once, not on every call of every function this tier will
        // never take. Tier 1 refuses whole functions for a single unsupported
        // op, so "refused" is the normal state for most code, and this is the
        // path that must be free.
        if chunk.jit_refused.get() == Some(cls_id) {
            return Ok(None);
        }
        let cls = match this {
            Some(Value::Instance(i)) => Some(i.borrow().class.clone()),
            Some(_) => return Ok(None),
            None => None,
        };
        // Ask the cache *before* building a `JitFn`. The key is the chunk and the
        // receiver's class — nothing from the signature — so a decision already
        // taken needs none of the work that describing the signature costs:
        // `root_fn` clones every parameter name into a fresh `Vec<String>` and
        // resolves every declared type through the scope chain by name. For a
        // function this tier has *refused*, all of that is repaid on every call
        // for the life of the program, and the refusal is the common case in any
        // code the tier cannot take. Measured on `bench/cli/mersey/url.mersey`,
        // whose three hot functions are all refused: 23.2 ms with the JIT
        // enabled against 19.6 ms with `MERSEY_JIT=0` — an 18% tax for compiled
        // code that does not exist.
        let key = (Rc::as_ptr(chunk) as usize, cls_id);
        let compiled = match self.jit_cache.get(&key).cloned() {
            Some(Some(c)) => c,
            Some(None) => {
                chunk.jit_refused.set(Some(cls_id));
                return Ok(None);
            }
            None => {
                let ret_obj = self.ret_class_in(scope.as_ref(), ret_ty);
                let ret_str = self.ret_is_str_in(scope.as_ref(), ret_ty);
                let ret_val = self.ret_is_val_in(scope.as_ref(), ret_ty);
                let ret_numopt = self.ret_is_numopt_in(scope.as_ref(), ret_ty);
                let Some(root) = self.root_fn(
                    chunk, params, ret_num, ret_bool, ret_obj, ret_str, ret_val, ret_numopt, cls,
                    scope, ret_ty,
                ) else {
                    // Not a signature this tier can even describe. That is a
                    // property of the function, not of this call, so record it —
                    // otherwise the attempt repeats forever, uncached.
                    self.jit_cache.insert(key, None);
                    chunk.jit_refused.set(Some(cls_id));
                    return Ok(None);
                };
                let Some(c) = self.jit_compile(chunk, &root) else {
                    chunk.jit_refused.set(Some(cls_id));
                    return Ok(None);
                };
                c
            }
        };
        if !self.assumptions_hold(&compiled) {
            self.jit_cache.remove(&key);
            // Not a refusal — the code is discarded so it can be *rebuilt* on a
            // later call, so the memo must not say "never".
            chunk.jit_refused.set(None);
            return Ok(None);
        }
        // The entry guard, per slot: the frame is not one type, so each argument
        // is checked against the slot it goes into. `this` goes last, into the
        // slot the compiler gave it — which is *after* the parameters, not before.
        let mut jargs = Vec::with_capacity(args.len() + 1);
        for (i, v) in args.iter().enumerate() {
            // An opaque parameter (`data: Bytes`) is parked in the arena and
            // crosses as its handle — `jit_arg` cannot do it, having no arena to
            // park it in. The compiled body owns that reference and releases it,
            // the same discipline an OSR entry uses.
            if matches!(compiled.code.slot_kinds.get(i), Some(JitSlot::Val)) {
                jargs.push(match v {
                    Value::Null => JitArg::Val(0),
                    other => JitArg::Val(self.jit_arena.keep(other.clone())),
                });
                continue;
            }
            match compiled.code.slot_kinds.get(i).and_then(|k| jit_arg(v, k)) {
                Some(a) => jargs.push(a),
                None => return Ok(None), // interpret instead
            }
        }
        if let (Some(t), Some(s)) = (this, compiled.code.this_slot) {
            match compiled.code.slot_kinds.get(s).and_then(|k| jit_arg(t, k)) {
                Some(a) => jargs.push(a),
                None => return Ok(None),
            }
        }
        // The arena owns everything this call allocates, and is cleared on every
        // way out — that is the whole memory story of a compiled call. The interp
        // pointer is live only across this one call: a compiled host call reaches
        // the interpreter through it, and it is cleared the moment we return so no
        // stale pointer can outlive the borrow.
        let ip: *mut Interp = self;
        let wb = self.host.web_bind_raw();
        self.jit_arena.interp = Some(ip);
        self.jit_arena.web_bind = wb;
        // The scope this code's globals live in, current for the call.
        let saved_scope = std::mem::replace(
            &mut self.jit_scope,
            compiled.code.scope.as_ref().map(|s| s.env()),
        );
        let r = (compiled.code.call)(&jargs, &mut self.jit_arena);
        self.jit_arena.interp = None;
        self.jit_arena.web_bind = None;
        self.jit_scope = saved_scope;
        self.jit_arena.clear();
        // Its handles named arena slots that have just gone.
        self.jit_str_memo.clear();
        match jit_value(r, ret_bool) {
            Ok(v) => Ok(Some(v)),
            Err(r) => self.after_jit(r, &compiled),
        }
    }

    /// Compiled code produced no value. Either it declined the call — in which
    /// case the interpreter runs it, and nothing has happened — or it ran and hit
    /// something the language says must throw, in which case it says *where*, and
    /// the error is raised there.
    ///
    /// It used to be one case: bail, and re-run the call interpreted to find out
    /// what went wrong. That was a fine answer while compiled code could not touch
    /// the heap, because re-running a pure function is free of consequence. It is
    /// not a fine answer now — a function that has already written to an object
    /// would write to it a second time — so a trap carries its position, and the
    /// error is built from that instead.
    fn after_jit(&mut self, r: JitResult, compiled: &Compiled) -> Result<Option<Value>, Thrown> {
        let JitResult::Trap(t) = r else {
            return Ok(None); // Bail: nothing ran, interpret the call
        };
        let chunk = compiled.code.chunks.get(t.func).cloned();
        if let Some(c) = &chunk {
            self.set_site(c.pos_at(t.pc));
        }
        Err(match t.reason {
            TrapReason::DivZero => self.throw("RangeError", "division by zero"),
            TrapReason::IntMinOverflow => self.throw("RangeError", "integer overflow in division"),
            TrapReason::Depth => self.throw("RangeError", "maximum call depth exceeded"),
            TrapReason::Bounds => self.throw(
                "RangeError",
                format!("index {} out of bounds (length {})", t.a, t.b),
            ),
            // The *same* message the interpreter gives, which is a different message
            // for each way of reaching through a null — so the instruction it
            // stopped at is what says which. Compiled code that got this wrong would
            // still throw, and a test that only checked "it threw" would pass.
            TrapReason::NullAccess => {
                let msg = chunk
                    .as_ref()
                    .map(|c| null_access_message(c, t.pc))
                    .unwrap_or_else(|| "no member on null".to_string());
                self.throw("TypeError", msg)
            }
            TrapReason::BadTag => self.throw("TypeError", "value is not of its declared type"),
            // A compiled host call threw: raise exactly what it recorded, at the
            // position of the call, so it is indistinguishable from the
            // interpreter having made the same call.
            TrapReason::HostError => self
                .jit_host_error
                .take()
                .unwrap_or_else(|| self.throw("Error", "host call failed")),
        })
    }

    /// Count a call, and say whether the chunk is hot. The counter lives on the
    /// chunk: hashing its address on every call, forever, to decide whether to
    /// compile it once is a strange thing to pay for.
    fn count_call(&self, chunk: &Rc<vm::Chunk>) -> bool {
        let n = chunk.hot.get();
        if n < self.jit_threshold {
            chunk.hot.set(n + 1);
            return false;
        }
        true
    }

    /// Re-enter a running function's compiled body at a loop header.
    ///
    /// The interpreter is *inside* the function, several thousand iterations
    /// into a loop, and holds the live locals in `frame`. Compiling the whole
    /// function and jumping into it at that loop header hands the rest of the
    /// execution — the remaining iterations *and* everything after the loop —
    /// to native code. Without this, a `main` that loops a hundred million
    /// times is compiled only after it finishes, which is never.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn try_osr(
        &mut self,
        chunk: &Rc<vm::Chunk>,
        params: &'static [Param],
        ret_num: Option<mersey_front::check::Num>,
        ret_bool: bool,
        ret_obj: Option<Rc<ClassDef>>,
        ret_str: bool,
        ret_val: bool,
        ret_numopt: bool,
        this: Option<Rc<ClassDef>>,
        target: usize,
        frame: &[Value],
        scope: Option<Env>,
        ret_ty: Option<&'static TypeExpr>,
    ) -> Result<Option<Value>, Thrown> {
        let Some(root) = self.root_fn(
            chunk, params, ret_num, ret_bool, ret_obj, ret_str, ret_val, ret_numopt, this, scope,
            ret_ty,
        ) else {
            return Ok(None);
        };
        let Some(compiled) = self.jit_compile(chunk, &root) else {
            return Ok(None);
        };
        if !self.assumptions_hold(&compiled) || !compiled.code.osr_entries.contains(&target) {
            return Ok(None);
        }
        // This function is now compiled. Mark it hot so the *next* call enters the
        // compiled body from the top rather than interpreting until the loop's OSR
        // point again — a function called around a long loop otherwise re-pays the
        // pre-OSR interpretation on every call (the timed run after a warm-up call,
        // the classic case). The compilation is cached; this only changes the entry.
        chunk.hot.set(self.jit_threshold);
        // The compiled code uses the interpreter's own frame layout, so resuming
        // is a straight transfer of the locals it is already holding — every one
        // of which must be what the code compiled them as.
        if frame.len() != compiled.code.n_slots {
            return Ok(None);
        }
        let mut locals = Vec::with_capacity(frame.len());
        for (i, v) in frame.iter().enumerate() {
            let kind = compiled.code.slot_kinds.get(i).cloned();
            let Some(kind) = kind else { return Ok(None) };
            // A slot the body has not reached yet is still `null`: dead at this
            // loop header, and the code gives it a zero.
            let a = match (v, &kind) {
                (Value::Null, JitSlot::I32) => JitArg::I32(0),
                (Value::Null, JitSlot::I64) => JitArg::I64(0),
                (Value::Null, JitSlot::F64) => JitArg::F64(0.0),
                // An opaque crosses as an arena handle and nothing else. Parking
                // it here is also what gives the compiled code the reference it
                // will release, so it does not need the `owned_slots` step below —
                // which only knows how to promote a `Ptr`.
                (Value::Null, JitSlot::Val) => JitArg::Val(0),
                (other, JitSlot::Val) => JitArg::Val(self.jit_arena.keep(other.clone())),
                (other, k) => match jit_arg(other, k) {
                    Some(a) => a,
                    None => return Ok(None),
                },
            };
            // A slot the compiled code *owns* — one it would have allocated into,
            // and will release when it overwrites. The interpreter's frame is
            // abandoned after a successful OSR, so the arena takes a reference of
            // its own; both sides are counted, and both let go correctly.
            let a = match (a, compiled.code.owned_slots.get(i)) {
                (JitArg::Ptr(p), Some(true)) if !p.is_null() => {
                    let h = self.jit_arena.keep(v.clone());
                    JitArg::Owned(p, h)
                }
                (a, _) => a,
            };
            locals.push(a);
        }
        let ip: *mut Interp = self;
        let wb = self.host.web_bind_raw();
        self.jit_arena.interp = Some(ip);
        self.jit_arena.web_bind = wb;
        // The scope this code's globals live in, current for the call.
        let saved_scope = std::mem::replace(
            &mut self.jit_scope,
            compiled.code.scope.as_ref().map(|s| s.env()),
        );
        let r = (compiled.code.osr)(&locals, target, &mut self.jit_arena);
        self.jit_arena.interp = None;
        self.jit_arena.web_bind = None;
        self.jit_scope = saved_scope;
        self.jit_arena.clear();
        // Its handles named arena slots that have just gone.
        self.jit_str_memo.clear();
        match jit_value(r, ret_bool) {
            Ok(v) => Ok(Some(v)),
            Err(r) => self.after_jit(r, &compiled),
        }
    }

    /// Does the world still look the way the compiled code assumed it did?
    ///
    /// Two assumptions. Every global it calls still names the function it named
    /// when it was compiled — a function declaration cannot be reassigned (§4.5,
    /// E0304), but an *import* binding could once be, so this is checked rather
    /// than trusted. And no class has appeared since: dispatch is direct because
    /// nothing overrode the method, and a `import()` that pulls in a new subclass
    /// is the one thing that could make that false.
    fn assumptions_hold(&self, compiled: &Compiled) -> bool {
        if compiled.code.n_classes != self.all_classes.len() {
            return false;
        }
        compiled.code.bound.iter().all(|(name, expected)| {
            // In the scope the name was *resolved* in, which is the closure's own:
            // `top_level_fn` only takes a callee whose environment is the scope it
            // was found in. Asking `self.globals` instead is right only for the
            // module being run — for anything else (the whole standard library)
            // the name is not bound there, every check fails, and the code is
            // discarded and rebuilt on every single call.
            matches!(env_get(&expected.env, name), Some(Value::Closure(c)) if Rc::ptr_eq(&c, expected))
        })
    }

    /// Is this top-level function inside Tier 1's subset — really compiled, not
    /// silently interpreted?
    ///
    /// This asks the compiler the same way the engine does, through the same
    /// environment, so a test using it is testing the path that runs. A test that
    /// assembled the compiler's input by hand would be testing its own assembly.
    pub fn jit_accepts(&mut self, name: &str) -> bool {
        let Some(Value::Closure(c)) = env_get(&self.globals, name) else {
            return false;
        };
        if self.chunk_of(&c).is_none() {
            return false;
        }
        // A test helper: the function is a top-level one of the module under
        // test, so the globals are its scope.
        let Some(root) = self.top_level_fn(None, name) else {
            return false;
        };
        let Some(hook) = self.jit else { return false };
        hook(&InterpEnv { i: self }, &root).is_some()
    }

    /// …and the same for a method of a class.
    pub fn jit_accepts_method(&mut self, class: &str, name: &str) -> bool {
        let Some(Value::Class(cls)) = env_get(&self.globals, class) else {
            return false;
        };
        let Some(data) = cls.lookup_method(name) else {
            return false;
        };
        // A method is compiled to bytecode on its first call; a test may ask
        // before that has happened.
        if data.chunk.borrow().is_none() {
            let module = self.current_module.clone();
            let out = vm::compile_fn_in(&data.body, &module, data.params);
            *data.chunk.borrow_mut() = Some(out);
        }
        let Some(root) = self.direct_method(&cls, name) else {
            return false;
        };
        let Some(hook) = self.jit else { return false };
        hook(&InterpEnv { i: self }, &root).is_some()
    }

    /// Compiled code is specialised to its receiver's class, so two classes
    /// sharing an inherited method get two compilations of it — the field offsets
    /// are the same, but what its *own* calls resolve to need not be.
    fn jit_key(&self, chunk: &Rc<vm::Chunk>, root: &JitFn) -> (usize, u64) {
        (
            Rc::as_ptr(chunk) as usize,
            root.this.as_ref().map_or(0, |c| c.id),
        )
    }

    /// The root of a compiled group: this function, with everything the compiler
    /// needs to know about its signature before it looks at its body.
    #[allow(clippy::too_many_arguments)]
    fn root_fn(
        &self,
        chunk: &Rc<vm::Chunk>,
        params: &'static [Param],
        ret: Option<mersey_front::check::Num>,
        ret_bool: bool,
        ret_obj: Option<Rc<ClassDef>>,
        ret_str: bool,
        ret_val: bool,
        ret_numopt: bool,
        this: Option<Rc<ClassDef>>,
        // Where the *hot* function's free names resolve. Its callees carry their
        // own (a method's is its class's); this is the one the group starts from,
        // and it has to come from the caller, which is the only thing that knows
        // which closure is running.
        scope: Option<Env>,
        ret_ty_decl: Option<&'static TypeExpr>,
    ) -> Option<JitFn> {
        Some(JitFn {
            chunk: chunk.clone(),
            params: simple_param_names(params)?,
            param_tys: self.param_types(scope.as_ref(), params),
            this,
            ret,
            ret_bool,
            ret_obj,
            ret_str,
            ret_val,
            ret_numopt,
            bind: None,
            scope: scope.map(DefScope),
            ret_ty: ret_ty_decl,
        })
    }

    /// What each parameter is declared to be. A body cannot tell you: the values
    /// come from outside it.
    fn param_types(&self, env: Option<&Env>, params: &[Param]) -> Vec<Option<JitSlot>> {
        let env = env.unwrap_or(&self.globals);
        params
            .iter()
            .map(|p| {
                let t = p.ty.as_ref()?;
                match resolve_field_ty(t, env) {
                    FieldTy::Num(mersey_front::check::Num::Int(
                        mersey_front::check::IntKind::I32,
                    )) => Some(JitSlot::I32),
                    FieldTy::Num(mersey_front::check::Num::Int(
                        mersey_front::check::IntKind::I64,
                    )) => Some(JitSlot::I64),
                    FieldTy::Num(mersey_front::check::Num::F64) => Some(JitSlot::F64),
                    FieldTy::Bool => Some(JitSlot::I32),
                    FieldTy::Obj(c) => Some(JitSlot::Obj(c)),
                    FieldTy::Arr(e) => Some(JitSlot::Arr(e)),
                    FieldTy::Str => Some(JitSlot::Str),
                    FieldTy::Val => Some(JitSlot::Val),
                    FieldTy::NumOpt => Some(JitSlot::NumOpt),
                    _ => None,
                }
            })
            .collect()
    }

    /// Compile `chunk` and everything it calls, once, and remember the result
    /// (including the decision *not* to compile it).
    fn jit_compile(&mut self, chunk: &Rc<vm::Chunk>, root: &JitFn) -> Option<Rc<Compiled>> {
        let key = self.jit_key(chunk, root);
        if let Some(cached) = self.jit_cache.get(&key) {
            return cached.clone();
        }
        let hook = self.jit?;
        let out = hook(&InterpEnv { i: self }, root).map(|code| Rc::new(Compiled { code }));
        self.jit_cache.insert(key, out.clone());
        out
    }

    /// The global `name`, if it is a plain top-level function that compiled
    /// code could call directly: no receiver, no captured environment beyond
    /// the globals, not a generator, not async, and already compiled to
    /// bytecode (if it has never run, it is not on any hot path).
    fn top_level_fn(&self, scope: Option<&Env>, name: &str) -> Option<JitFn> {
        // `scope` is where the *calling* function's names resolve. For a call
        // between two functions of the same module that is the module's scope,
        // and the globals are not it — which is why a std-library function
        // calling its own sibling was refused.
        let env = scope.unwrap_or(&self.globals);
        let Some(Value::Closure(c)) = env_get(env, name) else {
            return None;
        };
        if c.this.is_some() || c.cls.is_some() || c.data.is_async {
            return None;
        }
        // Nothing captured beyond the scope the caller itself resolves in: a
        // nested closure holding locals is not a direct call.
        if !Rc::ptr_eq(&c.env, env) {
            return None;
        }
        let chunk = c.data.chunk.borrow().clone()??;
        if chunk.yields {
            return None;
        }
        Some(JitFn {
            params: simple_param_names(c.data.params)?,
            param_tys: self.param_types(Some(&c.env), c.data.params),
            chunk,
            this: None,
            ret: c.data.ret_num,
            ret_bool: c.data.ret_bool,
            ret_obj: self.ret_class_in(Some(&c.env), c.data.ret_ty()),
            ret_str: self.ret_is_str_in(Some(&c.env), c.data.ret_ty()),
            ret_val: self.ret_is_val_in(Some(&c.env), c.data.ret_ty()),
            ret_numopt: self.ret_is_numopt_in(Some(&c.env), c.data.ret_ty()),
            bind: Some((name.to_string(), c.clone())),
            scope: Some(DefScope(c.env.clone())),
            ret_ty: c.data.ret_ty(),
        })
    }

    /// The method `name` on a receiver of class `cls`, if the call can be
    /// compiled into a *direct* one.
    /// A *static* method (`Version.parse(text)`). No receiver, so none of the
    /// override reasoning applies — a class's statics are fixed with the class
    /// (§4.1), and there is no subclass to dispatch through.
    fn direct_static(&self, cls: &Rc<ClassDef>, name: &str) -> Option<JitFn> {
        let data = cls.static_methods.get(name)?.clone();
        if data.is_async {
            return None;
        }
        let chunk = data.chunk.borrow().clone()??;
        if chunk.yields || chunk.needs_env || !chunk.simple_params {
            return None;
        }
        let scope = cls.env.clone().map(DefScope);
        Some(JitFn {
            params: simple_param_names(data.params)?,
            param_tys: self.param_types(cls.env.as_ref(), data.params),
            chunk,
            this: None,
            ret: data.ret_num,
            ret_bool: data.ret_bool,
            ret_obj: self.ret_class_in(cls.env.as_ref(), data.ret_ty()),
            ret_str: self.ret_is_str_in(cls.env.as_ref(), data.ret_ty()),
            ret_val: self.ret_is_val_in(cls.env.as_ref(), data.ret_ty()),
            ret_numopt: self.ret_is_numopt_in(cls.env.as_ref(), data.ret_ty()),
            bind: None,
            scope,
            ret_ty: data.ret_ty(),
        })
    }

    fn direct_method(&self, cls: &Rc<ClassDef>, name: &str) -> Option<JitFn> {
        // A getter or a setter means `o.name` is a call, not a load, and this is
        // not the shape the compiler thinks it is.
        if cls.is_accessor(name) || cls.is_host_backed() {
            return None;
        }
        let data = cls.lookup_method(name)?;
        if data.is_async {
            return None;
        }
        // The whole of dispatch: if nothing below `cls` overrides `name`, then
        // every instance this receiver can hold runs *this* body.
        if self.overridden_below(cls, name) {
            return None;
        }
        let chunk = data.chunk.borrow().clone()??;
        if chunk.yields || chunk.needs_env || !chunk.simple_params {
            return None;
        }
        // A method's free names resolve where its *class* was written.
        let scope = cls.env.clone().map(DefScope);
        Some(JitFn {
            params: simple_param_names(data.params)?,
            param_tys: self.param_types(cls.env.as_ref(), data.params),
            chunk,
            this: Some(cls.clone()),
            ret: data.ret_num,
            ret_bool: data.ret_bool,
            ret_obj: self.ret_class_in(cls.env.as_ref(), data.ret_ty()),
            ret_str: self.ret_is_str_in(cls.env.as_ref(), data.ret_ty()),
            ret_val: self.ret_is_val_in(cls.env.as_ref(), data.ret_ty()),
            ret_numopt: self.ret_is_numopt_in(cls.env.as_ref(), data.ret_ty()),
            bind: None, // a class's method set cannot change (§4.1)
            scope,
            ret_ty: data.ret_ty(),
        })
    }

    /// The getter `name` on a receiver of class `cls`, if reading it can be
    /// compiled into a *direct* call.
    ///
    /// `o.x` on a getter is a call, and Tier-1 used to give up on the whole
    /// enclosing function when it met one — a getter cost ~200ns where the
    /// equivalent method call cost under 2ns, because one accessor read sent
    /// every other instruction in that function back to the interpreter. The
    /// getter's body is an ordinary zero-argument method body, so it compiles
    /// like one; the conditions are `direct_method`'s, plus the same
    /// no-one-below-overrides-it rule applied to accessors.
    fn direct_getter(&self, cls: &Rc<ClassDef>, name: &str) -> Option<JitFn> {
        if cls.is_host_backed() {
            return None;
        }
        // A field of this name shadows nothing — the load path already handles
        // it — and a getter that only exists further down the hierarchy is not
        // this receiver's to call.
        if cls.field_slot(name).is_some() {
            return None;
        }
        let data = cls.lookup_getter(name)?;
        if data.is_async {
            return None;
        }
        if self
            .all_classes
            .iter()
            .any(|k| !Rc::ptr_eq(k, cls) && k.descends_from(cls) && k.declares_accessor(name))
        {
            return None;
        }
        let chunk = data.chunk.borrow().clone()??;
        if chunk.yields || chunk.needs_env || !chunk.simple_params {
            return None;
        }
        // A method's free names resolve where its *class* was written.
        let scope = cls.env.clone().map(DefScope);
        Some(JitFn {
            params: vec![],
            param_tys: vec![],
            chunk,
            this: Some(cls.clone()),
            ret: data.ret_num,
            ret_bool: data.ret_bool,
            ret_obj: self.ret_class_in(cls.env.as_ref(), data.ret_ty()),
            ret_str: self.ret_is_str_in(cls.env.as_ref(), data.ret_ty()),
            ret_val: self.ret_is_val_in(cls.env.as_ref(), data.ret_ty()),
            ret_numopt: self.ret_is_numopt_in(cls.env.as_ref(), data.ret_ty()),
            bind: None, // a class's accessor set cannot change (§4.1)
            scope,
            ret_ty: data.ret_ty(),
        })
    }

    /// What a function is declared to return — a class, a string, an engine
    /// primitive, a nullable number — asked in the scope the function was
    /// *written* in rather than the one being run.
    ///
    /// A declared return type is a name, and a name means what its own scope says
    /// — so a method of a class defined in a module resolves `Version?` to
    /// nothing when asked of the entry module's globals. The signature then reads
    /// as `void`, and every caller comparing the result against `null` is refused.
    /// This is the same mistake `name_kind` and `top_level_fn` made before they
    /// took a scope, and it hides the same way: as a refusal somewhere else.
    pub(crate) fn ret_class_in(
        &self,
        env: Option<&Env>,
        ret_ty: Option<&'static TypeExpr>,
    ) -> Option<Rc<ClassDef>> {
        match resolve_field_ty(ret_ty?, env.unwrap_or(&self.globals)) {
            FieldTy::Obj(c) => Some(c),
            _ => None,
        }
    }

    pub(crate) fn ret_is_str_in(
        &self,
        env: Option<&Env>,
        ret_ty: Option<&'static TypeExpr>,
    ) -> bool {
        match ret_ty {
            Some(t) => matches!(
                resolve_field_ty(t, env.unwrap_or(&self.globals)),
                FieldTy::Str
            ),
            None => false,
        }
    }

    pub(crate) fn ret_is_numopt_in(
        &self,
        env: Option<&Env>,
        ret_ty: Option<&'static TypeExpr>,
    ) -> bool {
        match ret_ty {
            Some(t) => matches!(
                resolve_field_ty(t, env.unwrap_or(&self.globals)),
                FieldTy::NumOpt
            ),
            None => false,
        }
    }

    pub(crate) fn ret_is_val_in(
        &self,
        env: Option<&Env>,
        ret_ty: Option<&'static TypeExpr>,
    ) -> bool {
        match ret_ty {
            Some(t) => matches!(
                resolve_field_ty(t, env.unwrap_or(&self.globals)),
                FieldTy::Val | FieldTy::Arr(_)
            ),
            None => false,
        }
    }

    fn bind_params(
        &mut self,
        params: &'static [Param],
        mut args: Vec<Value>,
        scope: &Env,
    ) -> Result<(), Thrown> {
        let mut rest_param: Option<&'static Param> = None;
        let positional: Vec<&'static Param> = params
            .iter()
            .filter(|p| {
                if p.rest {
                    rest_param = Some(p);
                    false
                } else {
                    true
                }
            })
            .collect();
        let n = positional.len().min(args.len());
        let rest_args: Vec<Value> = args.split_off(n);
        for (i, p) in positional.iter().enumerate() {
            let mut v = args.get(i).cloned().unwrap_or(Value::Null);
            if matches!(v, Value::Null) {
                if let Some(d) = &p.default {
                    v = self.eval(d, scope)?;
                }
            }
            self.bind_pattern(&p.target, v, scope)?;
        }
        if let Some(r) = rest_param {
            self.bind_pattern(&r.target, new_array(rest_args), scope)?;
        }
        Ok(())
    }

    fn call_value(&mut self, callee: &Value, args: Vec<Value>) -> VResult {
        match callee {
            Value::Closure(c) => self.call_closure(c, args),
            Value::PromiseExec(p) => {
                let p = p.clone();
                let mut it = args.into_iter();
                let (resolve, reject) = (it.next(), it.next());
                self.promise_then(&p, resolve, reject);
                Ok(Value::Null)
            }
            Value::AllSlot(slot, is_reject) => {
                let (slot, is_reject) = (*slot as usize, *is_reject);
                let v = args.into_iter().next().unwrap_or(Value::Null);
                let (results, remaining, out, idx) = {
                    let c = &self.all_cells[slot];
                    (c.results.clone(), c.remaining.clone(), c.out.clone(), c.idx)
                };
                if is_reject {
                    self.settle(&out, v, true); // first rejection wins
                } else {
                    results.borrow_mut()[idx] = v;
                    let left = {
                        let mut r = remaining.borrow_mut();
                        *r -= 1;
                        *r
                    };
                    if left == 0 {
                        let all = new_array(results.borrow().clone());
                        self.settle(&out, all, false);
                    }
                }
                Ok(Value::Null)
            }
            // Settling callbacks handed to host promises.
            Value::Resolve(p) | Value::Reject(p) => {
                let rejected = matches!(callee, Value::Reject(_));
                let p = p.clone();
                let v = args.into_iter().next().unwrap_or(Value::Null);
                self.settle(&p, v, rejected);
                Ok(Value::Null)
            }
            Value::Native(name) => self.call_native(name, None, args),
            // A handle to a JS function (e.g. imported `fetch`): call it.
            Value::JsRef(h) => {
                let h = *h;
                self.web_call(h, "", args)
            }
            _ => self.type_error("value is not callable"),
        }
    }

    fn call_native(&mut self, name: &str, recv: Option<&Value>, args: Vec<Value>) -> VResult {
        match name {
            "console.log" | "console.warn" | "console.error" | "console.info" | "console.debug" => {
                let mut parts: Vec<String> = Vec::with_capacity(args.len());
                for a in &args {
                    parts.push(self.display(a)?);
                }
                let line = parts.join(" ");
                match name {
                    "console.log" => self.host.print(&line),
                    level => self.host.print_level(&level["console.".len()..], &line),
                }
                Ok(Value::Null)
            }
            "dom.getElementById" => {
                let id = self.want_string(args.first())?;
                Ok(Value::Dom(Rc::new(id)))
            }
            "dom.createElement" => {
                let tag = self.want_string(args.first())?;
                let id = self.host.dom_create(&tag);
                Ok(Value::Dom(Rc::new(id)))
            }
            "dom.appendChild" => {
                let Some(Value::Dom(parent)) = recv else {
                    return self.type_error("appendChild needs an element");
                };
                let Some(Value::Dom(child)) = args.first() else {
                    return self.type_error("appendChild takes an element");
                };
                let (p, c) = (parent.to_string(), child.to_string());
                self.host.dom_append(&p, &c);
                Ok(Value::Null)
            }
            "dom.remove" => {
                let Some(Value::Dom(id)) = recv else {
                    return self.type_error("remove needs an element");
                };
                let id = id.to_string();
                self.host.dom_remove(&id);
                Ok(Value::Null)
            }
            "dom.addEventListener" => {
                let Some(Value::Dom(id)) = recv else {
                    return self.type_error("addEventListener needs an element");
                };
                let event = self.want_string(args.first())?;
                let cb = args.get(1).cloned().unwrap_or(Value::Null);
                let cb_id = self.callbacks.len() as u32;
                self.callbacks.push(cb);
                // Any event: the engine does not have a list of which ones
                // exist. The host owns the event loop, so it is the host that
                // knows — and in a browser that is the DOM itself.
                self.host.dom_add_listener(id, &event, cb_id);
                Ok(Value::Null)
            }
            "math.abs" => Ok(match args.first() {
                Some(Value::I32(n)) => Value::I32(n.wrapping_abs()),
                Some(Value::I64(n)) => Value::I64(n.wrapping_abs()),
                Some(Value::F32(f)) => Value::F32(f.abs()),
                Some(Value::F64(f)) => Value::F64(f.abs()),
                v => Value::F64(v.and_then(as_num).unwrap_or(f64::NAN).abs()),
            }),
            "math.min" | "math.max" => {
                // NaN propagates, as in JS: `Math.max(NaN, 5)` and
                // `Math.max(5, NaN)` are both NaN. The fold below compares with
                // `<`, and an ordered comparison is false whichever side the NaN
                // is on — so it returned the *other* operand, which made the
                // answer depend on argument order (`math.max(NaN, 5)` was 5 but
                // `math.max(5, NaN)` was NaN) and disagreed with Tier 1, whose
                // lowering propagates. Argument order must not change the answer.
                let mut best: Option<Value> = None;
                for a in args {
                    // Checked as the fold goes rather than in a pass of its own:
                    // NaN wins whatever else is in the list, so the first one seen
                    // is the answer. The tiers with no JIT (the wasm engine, every
                    // browser leg) take this path for every `math.max` they make.
                    if matches!(&a, Value::F64(f) if f.is_nan()) {
                        return Ok(Value::F64(f64::NAN));
                    }
                    best = Some(match best {
                        None => a,
                        Some(b) => {
                            let take_a =
                                match self.numeric_binop(BinOp::Lt, a.clone(), b.clone())? {
                                    Value::Bool(lt) => lt == (name == "math.min"),
                                    _ => false,
                                };
                            if take_a {
                                a
                            } else {
                                b
                            }
                        }
                    });
                }
                Ok(best.unwrap_or(Value::Null))
            }
            // Single-argument float64 -> float64. `round` is round-half-away
            // -from-zero (Rust's `f64::round`), which is what people expect and
            // what IEEE calls roundTiesToAway.
            "math.floor" | "math.ceil" | "math.sqrt" | "math.round" | "math.trunc"
            | "math.cbrt" | "math.exp" | "math.log" | "math.log2" | "math.log10" | "math.sin"
            | "math.cos" | "math.tan" | "math.asin" | "math.acos" | "math.atan" | "math.sign" => {
                let x = args.first().and_then(as_num).unwrap_or(f64::NAN);
                Ok(Value::F64(match name {
                    "math.floor" => x.floor(),
                    "math.ceil" => x.ceil(),
                    "math.round" => x.round(),
                    "math.trunc" => x.trunc(),
                    "math.cbrt" => x.cbrt(),
                    "math.exp" => x.exp(),
                    "math.log" => x.ln(),
                    "math.log2" => x.log2(),
                    "math.log10" => x.log10(),
                    "math.sin" => x.sin(),
                    "math.cos" => x.cos(),
                    "math.tan" => x.tan(),
                    "math.asin" => x.asin(),
                    "math.acos" => x.acos(),
                    "math.atan" => x.atan(),
                    // NaN has no sign; propagating it beats inventing one.
                    "math.sign" => {
                        if x.is_nan() {
                            f64::NAN
                        } else if x > 0.0 {
                            1.0
                        } else if x < 0.0 {
                            -1.0
                        } else {
                            x // preserves -0.0
                        }
                    }
                    _ => x.sqrt(),
                }))
            }
            "math.atan2" | "math.hypot" => {
                let y = args.first().and_then(as_num).unwrap_or(f64::NAN);
                let x = args.get(1).and_then(as_num).unwrap_or(f64::NAN);
                Ok(Value::F64(if name == "math.atan2" {
                    y.atan2(x)
                } else {
                    y.hypot(x)
                }))
            }
            "math.clamp" => {
                let x = args.first().and_then(as_num).unwrap_or(f64::NAN);
                let lo = args.get(1).and_then(as_num).unwrap_or(f64::NEG_INFINITY);
                let hi = args.get(2).and_then(as_num).unwrap_or(f64::INFINITY);
                if lo > hi {
                    return Err(self.throw(
                        "RangeError",
                        format!("clamp: lower bound {lo} is above upper bound {hi}"),
                    ));
                }
                Ok(Value::F64(x.clamp(lo, hi)))
            }
            "math.isNaN" => Ok(Value::Bool(
                args.first().and_then(as_num).is_none_or(|x| x.is_nan()),
            )),
            "math.isFinite" => Ok(Value::Bool(
                args.first().and_then(as_num).is_some_and(|x| x.is_finite()),
            )),
            "math.pow" => {
                let x = args.first().and_then(as_num).unwrap_or(f64::NAN);
                let y = args.get(1).and_then(as_num).unwrap_or(f64::NAN);
                Ok(Value::F64(x.powf(y)))
            }
            "format.pad" => {
                let text = to_display(args.first().unwrap_or(&Value::Null));
                let width = args.get(1).and_then(as_i64).unwrap_or(0).max(0) as usize;
                let n = text.chars().count();
                let padded = if n >= width {
                    text
                } else {
                    format!("{}{text}", " ".repeat(width - n))
                };
                Ok(Value::Str(Rc::new(utf16(&(padded)))))
            }
            "format.fixed" => {
                let x = args.first().and_then(as_num).unwrap_or(f64::NAN);
                let d = args.get(1).and_then(as_i64).unwrap_or(0).clamp(0, 17) as usize;
                Ok(Value::Str(Rc::new(utf16(&format!("{x:.d$}")))))
            }
            "json.stringify" => {
                let v = args.first().cloned().unwrap_or(Value::Null);
                let j = self.to_web(&v);
                let mut out = String::new();
                webjson::write(&mut out, &j);
                Ok(Value::Str(Rc::new(utf16(&(out)))))
            }
            "json.parse" => {
                let text = self.want_string(args.first())?;
                match webjson::parse(&text) {
                    Some(j) => Ok(self.from_web(&j)),
                    None => Err(self.throw("Error", "invalid JSON")),
                }
            }
            // Every one of these draws from the host's CSPRNG, and every one of
            // them is refused unless the `random` capability was granted.
            "random.bytes" => {
                let n = args.first().and_then(as_i64).unwrap_or(0);
                if !(0..=(1 << 24)).contains(&n) {
                    return Err(self.throw(
                        "RangeError",
                        format!("random.bytes: {n} is outside 0..=16777216"),
                    ));
                }
                match self.host.random_bytes(n as usize) {
                    Ok(b) => Ok(Value::Bytes(Rc::new(RefCell::new(b)))),
                    Err(msg) => Err(self.throw("Error", msg)),
                }
            }
            // Fill a caller's buffer in place — the primitive `bytes` is built
            // on, and the one a loop should use: allocating a fresh buffer per
            // call costs more than generating the randomness that goes in it.
            "random.fill" => self.call_native_fast(0, &args),
            "random.float" => {
                let b = match self.host.random_bytes(8) {
                    Ok(b) => b,
                    Err(msg) => return Err(self.throw("Error", msg)),
                };
                let mut raw = [0u8; 8];
                raw.copy_from_slice(&b);
                // 53 bits of mantissa: the largest integer range f64 represents
                // exactly, so every value in [0, 1) is equally likely.
                let bits = u64::from_le_bytes(raw) >> 11;
                Ok(Value::F64(bits as f64 / (1u64 << 53) as f64))
            }
            "random.int" => {
                let lo = args.first().and_then(as_i64).unwrap_or(0);
                let hi = args.get(1).and_then(as_i64).unwrap_or(0);
                if lo > hi {
                    return Err(self.throw("RangeError", format!("random.int: {lo} is above {hi}")));
                }
                let span = (hi - lo) as u64 + 1; // inclusive
                let b = match self.host.random_bytes(8) {
                    Ok(b) => b,
                    Err(msg) => return Err(self.throw("Error", msg)),
                };
                let mut raw = [0u8; 8];
                raw.copy_from_slice(&b);
                let r = u64::from_le_bytes(raw);
                // Modulo would bias the low values; rejection sampling does not.
                let limit = u64::MAX - (u64::MAX % span);
                let mut r = r;
                let mut tries = 0;
                while r >= limit {
                    tries += 1;
                    if tries > 64 {
                        break; // astronomically unlikely; do not spin forever
                    }
                    let b = match self.host.random_bytes(8) {
                        Ok(b) => b,
                        Err(msg) => return Err(self.throw("Error", msg)),
                    };
                    raw.copy_from_slice(&b);
                    r = u64::from_le_bytes(raw);
                }
                Ok(Value::I64(lo + (r % span) as i64))
            }
            "fs.readText" => {
                let path = self.want_string(args.first())?;
                match self.host.read_text(&path) {
                    Ok(text) => Ok(Value::Str(Rc::new(utf16(&(text))))),
                    Err(msg) => Err(self.throw("Error", msg)),
                }
            }
            "env.get" => {
                let key = self.want_string(args.first())?;
                Ok(match self.host.env_var(&key) {
                    Some(v) => Value::Str(Rc::new(utf16(&(v)))),
                    None => Value::Null,
                })
            }
            // Register an HTTP request handler and hand the (port, callback id) to
            // the host. The host does not serve here — it records the request; the
            // accept loop runs in the CLI driver after top-level completes, so it
            // can re-enter the engine per request (`http_dispatch`) without a
            // borrow conflict. Refused unless the `net` capability was granted.
            "net.serve" => {
                let port = args.first().and_then(as_i64).unwrap_or(0);
                if !(1..=65535).contains(&port) {
                    return Err(self.throw(
                        "RangeError",
                        format!("net.serve: port {port} is outside 1..=65535"),
                    ));
                }
                let cb = args.get(1).cloned().unwrap_or(Value::Null);
                let cb_id = self.callbacks.len() as u32;
                self.callbacks.push(cb);
                match self.host.request_serve(port as u16, cb_id) {
                    Ok(()) => Ok(Value::Null),
                    Err(msg) => Err(self.throw("Error", msg)),
                }
            }
            // dom.apply(ops, nodes, strs): submit a whole render's DOM mutations
            // in ONE host call and get back the created nodes. See
            // docs/architecture/dom-batching.md. Node operands cross as JsRef
            // (typed `unknown`); the created nodes come back as JsRef too.
            "dom.apply" => {
                let ops: Vec<i32> = match args.first() {
                    Some(Value::Array(a)) => a
                        .borrow()
                        .iter()
                        .map(|v| as_i64(v).unwrap_or(0) as i32)
                        .collect(),
                    _ => Vec::new(),
                };
                let nodes: Vec<i64> = match args.get(1) {
                    Some(Value::Array(a)) => a
                        .borrow()
                        .iter()
                        .map(|v| if let Value::JsRef(h) = v { *h } else { -1 })
                        .collect(),
                    _ => Vec::new(),
                };
                let strs: Vec<String> = match args.get(2) {
                    Some(Value::Array(a)) => a
                        .borrow()
                        .iter()
                        .map(|v| {
                            if let Value::Str(s) = v {
                                utf16_to_string(s)
                            } else {
                                String::new()
                            }
                        })
                        .collect(),
                    _ => Vec::new(),
                };
                match self.host.web_apply(&ops, &nodes, &strs) {
                    Some(handles) => Ok(new_array(handles.into_iter().map(Value::JsRef).collect())),
                    // The host declined the batched path (web_apply is NULL):
                    // replay the ops one at a time through the ordinary web
                    // bridge. Same result, no crossing-collapse.
                    None => self.dom_apply_replay(&ops, &nodes, &strs),
                }
            }
            "caps.has" => {
                let cap = self.want_string(args.first())?;
                Ok(Value::Bool(self.host.caps().contains(&cap)))
            }
            "caps.list" => {
                let caps: Vec<Value> = self
                    .host
                    .caps()
                    .into_iter()
                    .map(|c| Value::Str(Rc::new(utf16(&(c)))))
                    .collect();
                Ok(new_array(caps))
            }
            "promise.resolve" => {
                let p = PromiseState::pending();
                let v = args.into_iter().next().unwrap_or(Value::Null);
                self.settle(&p, v, false);
                Ok(Value::PromiseV(p))
            }
            "promise.reject" => {
                let p = PromiseState::pending();
                let v = args.into_iter().next().unwrap_or(Value::Null);
                self.settle(&p, v, true);
                Ok(Value::PromiseV(p))
            }
            "promise.all" => {
                let items: Vec<Value> = match args.first() {
                    Some(Value::Array(a)) => a.borrow().clone(),
                    _ => return self.type_error("Promise.all needs an array"),
                };
                let out = PromiseState::pending();
                let results = Rc::new(GcCell::new(vec![Value::Null; items.len()]));
                let remaining = Rc::new(RefCell::new(items.len()));
                if items.is_empty() {
                    let all = new_array(Vec::new());
                    self.settle(&out, all, false);
                    return Ok(Value::PromiseV(out));
                }
                for (idx, item) in items.into_iter().enumerate() {
                    let p = self.as_promise(item)?;
                    let cell = AllCell {
                        results: results.clone(),
                        remaining: remaining.clone(),
                        out: out.clone(),
                        idx,
                    };
                    self.all_cells.push(cell);
                    let slot = (self.all_cells.len() - 1) as u32;
                    let on_ok = Value::AllSlot(slot, false);
                    let on_err = Value::AllSlot(slot, true);
                    self.promise_then(&p, Some(on_ok), Some(on_err));
                }
                Ok(Value::PromiseV(out))
            }
            // A collection cannot run mid-expression (live VM frames are not
            // roots), so this *requests* one for the next safe point.
            "gc.collect" => {
                self.gc_pending = true;
                Ok(Value::Null)
            }
            "gc.stats" => {
                // Reports only — sweeping here would be unsound (live VM
                // frames are not roots mid-expression).
                let stats = gc::stats_only();
                Ok(new_record(vec![(
                    "live".to_string(),
                    Value::I32(stats.tracked as i32),
                )]))
            }
            "regex.compile" => {
                let pattern = self.want_string(args.first())?;
                let flags = match args.get(1) {
                    Some(Value::Str(s)) => utf16_to_string(s),
                    _ => String::new(),
                };
                match regex::Regex::new(&pattern, &flags) {
                    Ok(re) => Ok(Value::RegexV(Rc::new(re))),
                    Err(msg) => Err(self.throw("Error", format!("bad regex: {msg}"))),
                }
            }
            "parse.url" => self.call_native_fast(3, &args),
            "parse.bool" => {
                let text = self.want_string(args.first())?;
                // Exactly "true" or "false". Nothing else is a boolean, and
                // guessing at one is how `"no"` becomes `true` elsewhere.
                Ok(match text.trim() {
                    "true" => Value::Bool(true),
                    "false" => Value::Bool(false),
                    _ => Value::Null,
                })
            }
            "parse.int32" | "parse.int64" | "parse.float64" | "parse.bigint" | "parse.bigdec" => {
                let text = self.want_string(args.first())?;
                let t = text.trim();
                // Parsing returns null on failure — no exceptions for input
                // you expected to be dubious (§1.3: no sentinel values).
                Ok(match name {
                    "parse.int32" => {
                        let radix = args.get(1).and_then(as_i64).unwrap_or(10).clamp(2, 36) as u32;
                        match i32::from_str_radix(t, radix) {
                            Ok(v) => Value::I32(v),
                            Err(_) => Value::Null,
                        }
                    }
                    "parse.int64" => {
                        let radix = args.get(1).and_then(as_i64).unwrap_or(10).clamp(2, 36) as u32;
                        match i64::from_str_radix(t, radix) {
                            Ok(v) => Value::I64(v),
                            Err(_) => Value::Null,
                        }
                    }
                    "parse.float64" => match t.parse::<f64>() {
                        Ok(v) => Value::F64(v),
                        Err(_) => Value::Null,
                    },
                    "parse.bigint" => {
                        let (neg, digits) = match t.strip_prefix('-') {
                            Some(rest) => (true, rest),
                            None => (false, t.strip_prefix('+').unwrap_or(t)),
                        };
                        match BigInt::parse(digits, 10) {
                            Some(b) if !digits.is_empty() => {
                                Value::BigIntV(Rc::new(if neg { b.negate() } else { b }))
                            }
                            _ => Value::Null,
                        }
                    }
                    _ => match BigDec::parse(t) {
                        Some(d) => Value::BigDecV(Rc::new(d)),
                        None => Value::Null,
                    },
                })
            }
            // Civil calendar from a millisecond timestamp (Howard Hinnant's
            // days-from-civil algorithm, proleptic Gregorian).
            // ISO-8601 in UTC — the one format that round-trips, sorts as text,
            // and means the same thing to every other system. No locale
            // formatting: that is presentation, and it belongs to the host.
            "time.format" => {
                let ms = args.first().and_then(as_num).unwrap_or(0.0);
                let secs = (ms / 1000.0).floor() as i64;
                let ms_part = (ms - (secs as f64) * 1000.0).round() as i64;
                let days = secs.div_euclid(86_400);
                let tod = secs.rem_euclid(86_400);
                let (y, m, d) = civil_from_days(days);
                let text = format!(
                    "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{ms_part:03}Z",
                    tod / 3600,
                    (tod % 3600) / 60,
                    tod % 60,
                );
                Ok(Value::Str(Rc::new(utf16(&(text)))))
            }
            "time.parse" => {
                let text = self.want_string(args.first())?;
                Ok(match parse_iso8601(text.trim()) {
                    // Null on failure, like every other parser here (§1.3).
                    Some(ms) => Value::F64(ms),
                    None => Value::Null,
                })
            }
            "time.parts" => {
                let ms = args.first().and_then(as_num).unwrap_or(0.0);
                let secs = (ms / 1000.0).floor() as i64;
                let ms_part = (ms - (secs as f64) * 1000.0).round() as i64;
                let days = secs.div_euclid(86_400);
                let tod = secs.rem_euclid(86_400);
                let (y, m, d) = civil_from_days(days);
                let weekday = (days + 4).rem_euclid(7); // 1970-01-01 was a Thursday
                Ok(new_record(vec![
                    ("year".into(), Value::I32(y as i32)),
                    ("month".into(), Value::I32(m as i32)),
                    ("day".into(), Value::I32(d as i32)),
                    ("hour".into(), Value::I32((tod / 3600) as i32)),
                    ("minute".into(), Value::I32(((tod % 3600) / 60) as i32)),
                    ("second".into(), Value::I32((tod % 60) as i32)),
                    ("millis".into(), Value::I32(ms_part as i32)),
                    ("weekday".into(), Value::I32(weekday as i32)),
                ]))
            }
            "time.fromParts" => {
                let Some(Value::Record(r)) = args.first() else {
                    return self.type_error("time.fromParts needs a record");
                };
                let f = r.borrow();
                let get =
                    |k: &str, dflt: i64| rec_get(&f, k).and_then(|v| as_i64(&v)).unwrap_or(dflt);
                let days = days_from_civil(get("year", 1970), get("month", 1), get("day", 1));
                let secs = days * 86_400
                    + get("hour", 0) * 3600
                    + get("minute", 0) * 60
                    + get("second", 0);
                Ok(Value::F64((secs as f64) * 1000.0 + get("millis", 0) as f64))
            }
            "time.now" | "time.monotonic" => Ok(Value::F64(self.host.time_ms(name == "time.now"))),
            "bytes.alloc" => {
                let n = args.first().and_then(as_i64).unwrap_or(0).max(0) as usize;
                Ok(Value::Bytes(Rc::new(RefCell::new(vec![0u8; n]))))
            }
            "hash.sha256" => {
                let Some(Value::Bytes(b)) = args.first() else {
                    return self.type_error("hash.sha256 needs a Bytes buffer");
                };
                let digest = sha256(&b.borrow());
                Ok(Value::Bytes(Rc::new(RefCell::new(digest))))
            }
            "hash.sha1" => {
                let Some(Value::Bytes(b)) = args.first() else {
                    return self.type_error("hash.sha1 needs a Bytes buffer");
                };
                let digest = sha1(&b.borrow());
                Ok(Value::Bytes(Rc::new(RefCell::new(digest))))
            }
            "hash.hmacSha256" => {
                let (Some(Value::Bytes(key)), Some(Value::Bytes(data))) =
                    (args.first(), args.get(1))
                else {
                    return self.type_error("hash.hmacSha256 needs (key: Bytes, data: Bytes)");
                };
                let mac = hmac_sha256(&key.borrow(), &data.borrow());
                Ok(Value::Bytes(Rc::new(RefCell::new(mac))))
            }
            "hash.hmacSha1" => {
                let (Some(Value::Bytes(key)), Some(Value::Bytes(data))) =
                    (args.first(), args.get(1))
                else {
                    return self.type_error("hash.hmacSha1 needs (key: Bytes, data: Bytes)");
                };
                let mac = hmac_sha1(&key.borrow(), &data.borrow());
                Ok(Value::Bytes(Rc::new(RefCell::new(mac))))
            }
            "bytes.encodeUtf8" => self.call_native_fast(1, &args),
            "bytes.decodeUtf8" => self.call_native_fast(2, &args),
            "bytes.fromHost" => {
                let Some(Value::JsRef(h)) = args.first() else {
                    return self.type_error("bytes.fromHost needs a host typed array");
                };
                let h = *h;
                match self.host.web_bytes_read(h) {
                    Some(v) => Ok(Value::Bytes(Rc::new(RefCell::new(v)))),
                    None => self.type_error("value is not a typed array / ArrayBuffer"),
                }
            }
            "bytes.toHost" => {
                let Some(Value::Bytes(b)) = args.first() else {
                    return self.type_error("bytes.toHost needs a Bytes buffer");
                };
                let data = b.borrow().clone();
                let handle = self.host.web_bytes_write(&data);
                if handle < 0 {
                    self.type_error("host cannot accept byte buffers")
                } else {
                    Ok(Value::JsRef(handle))
                }
            }
            "bytes.fill" => {
                let Some(Value::Bytes(b)) = args.first() else {
                    return self.type_error("bytes.fill needs a Bytes buffer");
                };
                let v = (args.get(1).and_then(as_i64).unwrap_or(0) & 0xFF) as u8;
                b.borrow_mut().iter_mut().for_each(|x| *x = v);
                Ok(Value::Null)
            }
            "web.attach" => {
                let (Some(inst_v), Some(host)) = (args.first(), args.get(1)) else {
                    return self.type_error("attach(instance, hostObject) needs both");
                };
                let Value::Instance(inst) = inst_v else {
                    return self.type_error("attach: the first value must be a class instance");
                };
                let h = match host {
                    Value::JsRef(h) => *h,
                    Value::Instance(i) => match i.borrow().host {
                        Some(h) => h,
                        None => {
                            return self.type_error("attach: the second value is not a host object")
                        }
                    },
                    _ => return self.type_error("attach: the second value is not a host object"),
                };
                inst.borrow_mut().host = Some(h);
                Ok(inst_v.clone())
            }
            "web.release" => {
                if let Some(v) = args.first() {
                    self.web_release_value(v);
                }
                Ok(Value::Null)
            }
            "caps.drop" => {
                let cap = self.want_string(args.first())?;
                self.host.drop_cap(&cap);
                Ok(Value::Null)
            }
            _ => self.type_error(format!("unknown native `{name}`")),
        }
    }

    fn want_string(&self, v: Option<&Value>) -> Result<String, Thrown> {
        match v {
            Some(Value::Str(s)) => Ok(utf16_to_string(s)),
            _ => Err(self.throw("TypeError", "expected a string argument")),
        }
    }

    pub(crate) fn instantiate(&mut self, cls: &Rc<ClassDef>, args: Vec<Value>) -> VResult {
        if cls.is_builtin_error {
            let mut slots = vec![Value::Null; cls.fields.len()];
            slots[0] = args.into_iter().next().unwrap_or(Value::Null);
            if slots.len() > 1 {
                slots[1] = Value::Str(Rc::new(utf16(&(self.stack_trace()))));
            }
            let inst = Rc::new(GcCell::new(Instance {
                class: cls.clone(),
                slots,
                host: None,
            }));
            gc::track_instance(&inst);
            return Ok(Value::Instance(inst));
        }
        // The slots a fresh instance starts with, literals already in place: one
        // clone of a vector that was built once, at class definition.
        let mut slots = cls.initial_slots.clone();
        // Container fields with no initializer: a fresh empty one per instance.
        for (slot, d) in &cls.container_inits {
            slots[*slot] = default_value(*d);
        }
        let inst = Rc::new(GcCell::new(Instance {
            class: cls.clone(),
            slots,
            host: None,
        }));
        gc::track_instance(&inst);
        let this = Value::Instance(inst.clone());
        let env = cls.env.clone().unwrap_or_else(|| self.globals.clone());

        // Field initializers, base-first, with `this` in scope.
        //
        // One scope for all of them, and only if there is an initializer to run.
        // This used to clone the whole field list on *every* instantiation — the
        // names and all — and then allocate a fresh environment per initialized
        // field, to hold one binding that every one of them wanted to be the same.
        // The literal initializers are already in `initial_slots`, cloned above.
        // What is left is the ones that compute, or read `this` — usually none,
        // and one scope is enough for all of them.
        if !cls.dynamic_inits.is_empty() {
            let scope = child_env(&env);
            env_define(&scope, "this", this.clone());
            for (slot, e) in &cls.dynamic_inits {
                let v = self.eval(e, &scope)?;
                inst.borrow_mut().slots[*slot] = v;
            }
        }

        // Nearest constructor up the chain; implicit pass-through otherwise.
        let mut search = Some(cls.clone());
        while let Some(c) = search {
            if let Some(ctor) = &c.ctor {
                let closure = Closure {
                    data: ctor.clone(),
                    env,
                    this: Some(this.clone()),
                    cls: Some(c.clone()),
                };
                self.call_closure(&closure, args)?;
                break;
            }
            search = c.parent.clone();
        }
        Ok(this)
    }

    // ---- member access -----------------------------------------------------------

    fn get_member(&mut self, obj: &Value, name: &str) -> Result<Option<Value>, Thrown> {
        match obj {
            Value::Str(s) => Ok(match name {
                "length" => Some(Value::I32(s.len() as i32)),
                _ => None,
            }),
            Value::Array(a) => Ok(match name {
                "length" => Some(Value::I32(a.borrow().len() as i32)),
                _ => None,
            }),
            Value::JsRef(h) => {
                let h = *h;
                self.web_get(h, name).map(Some)
            }
            Value::Bytes(b) => Ok(match name {
                "length" => Some(Value::I32(b.borrow().len() as i32)),
                _ => None,
            }),
            // Cut on demand, which is the whole point of holding the parse: a
            // caller that wants the path should not pay to build the fragment.
            Value::UrlV(u) => {
                let s = |t: &str| Some(Value::Str(Rc::new(utf16(t))));
                Ok(match name {
                    "href" => s(u.as_str()),
                    "protocol" => s(&format!("{}:", u.scheme())),
                    "hostname" => s(u.host_str().unwrap_or("")),
                    "port" => s(&u.port().map(|p| p.to_string()).unwrap_or_default()),
                    "pathname" => s(u.path()),
                    "search" => s(&u.query().map(|q| format!("?{q}")).unwrap_or_default()),
                    "hash" => s(&u.fragment().map(|f| format!("#{f}")).unwrap_or_default()),
                    _ => None,
                })
            }
            Value::MapV(m) => Ok(match name {
                "size" => Some(Value::I32(m.borrow().len() as i32)),
                _ => None,
            }),
            Value::SetV(m) => Ok(match name {
                "size" => Some(Value::I32(m.borrow().len() as i32)),
                _ => None,
            }),
            Value::Record(r) => Ok(rec_get(&r.borrow(), name)),
            Value::Namespace(ns) => Ok(ns.entries.get(name).cloned()),
            Value::Dom(id) => match name {
                "textContent" => Ok(Some(Value::Str(Rc::new(utf16(
                    &self.host.dom_get_text(id).unwrap_or_default(),
                ))))),
                "value" => {
                    let id = id.to_string();
                    Ok(Some(Value::Str(Rc::new(utf16(
                        &self.host.dom_get_value(&id),
                    )))))
                }
                _ => Ok(None),
            },
            Value::Class(c) => {
                if let Some(v) = c.statics.borrow().get(name) {
                    return Ok(Some(v.clone()));
                }
                if let Some(m) = c.static_methods.get(name) {
                    let env = c.env.clone().unwrap_or_else(|| self.globals.clone());
                    return Ok(Some(Value::Closure(Rc::new(Closure {
                        data: m.clone(),
                        env,
                        this: None,
                        cls: Some(c.clone()),
                    }))));
                }
                Ok(None)
            }
            Value::Instance(inst) => {
                {
                    // Constant-offset load: sealed shapes make the slot
                    // known from the class alone (§4.1).
                    let i = inst.borrow();
                    if let Some(slot) = i.class.field_slots.get(name) {
                        return Ok(i.slots.get(*slot as usize).cloned());
                    }
                }
                let class = inst.borrow().class.clone();
                if let Some((getter, defining)) = find_in_chain(&class, |c| {
                    c.getters.get(name).map(|g| (g.clone(), c.clone()))
                }) {
                    let env = defining.env.clone().unwrap_or_else(|| self.globals.clone());
                    let closure = Closure {
                        data: getter,
                        env,
                        this: Some(obj.clone()),
                        cls: Some(defining),
                    };
                    return self.call_closure(&closure, Vec::new()).map(Some);
                }
                if let Some((m, defining)) = find_in_chain(&class, |c| {
                    c.methods.get(name).map(|m| (m.clone(), c.clone()))
                }) {
                    let env = defining.env.clone().unwrap_or_else(|| self.globals.clone());
                    return Ok(Some(Value::Closure(Rc::new(Closure {
                        data: m,
                        env,
                        this: Some(obj.clone()),
                        cls: Some(defining),
                    }))));
                }
                // Host-backed class: read it off the host object.
                let host = inst.borrow().host;
                if let Some(h) = host {
                    return self.web_get(h, name).map(Some);
                }
                Ok(None)
            }
            _ => Ok(None),
        }
    }

    fn set_member(&mut self, obj: &Value, name: &str, value: Value) -> Result<(), Thrown> {
        match obj {
            Value::JsRef(h) => {
                let h = *h;
                self.web_set(h, name, value)
            }
            Value::Record(r) => {
                rec_set(&mut r.borrow_mut(), name, value);
                Ok(())
            }
            Value::Dom(id) => match name {
                "textContent" => {
                    let id = id.to_string();
                    self.host.dom_set_text(&id, &to_display(&value));
                    Ok(())
                }
                "value" => {
                    let id = id.to_string();
                    self.host.dom_set_value(&id, &to_display(&value));
                    Ok(())
                }
                _ => self.type_error(format!("DOM elements have no settable `{name}`")),
            },
            Value::Class(c) => {
                if c.statics.borrow().contains_key(name) {
                    c.statics.borrow_mut().insert(name.to_string(), value);
                    Ok(())
                } else {
                    self.type_error(format!("no static field `{name}` on class `{}`", c.name))
                }
            }
            Value::Instance(inst) => {
                let class = inst.borrow().class.clone();
                if let Some((setter, defining)) = find_in_chain(&class, |c| {
                    c.setters.get(name).map(|s| (s.clone(), c.clone()))
                }) {
                    let env = defining.env.clone().unwrap_or_else(|| self.globals.clone());
                    let closure = Closure {
                        data: setter,
                        env,
                        this: Some(obj.clone()),
                        cls: Some(defining),
                    };
                    self.call_closure(&closure, vec![value])?;
                    return Ok(());
                }
                // Sealed shapes (§4.1): the field must be declared, and its
                // slot is a constant.
                let slot = class.field_slots.get(name).copied();
                if let Some(slot) = slot {
                    inst.borrow_mut().slots[slot as usize] = value;
                    return Ok(());
                }
                // Host-backed class: write through to the host object
                // (`this.textContent = …` on a class extending HTMLElement).
                let host = inst.borrow().host;
                if let Some(h) = host {
                    return self.web_set(h, name, value);
                }
                self.type_error(format!(
                    "class `{}` has no field `{name}` (shapes are sealed)",
                    class.name
                ))
            }
            _ => self.type_error("cannot assign to a member of this value"),
        }
    }

    // ---- expressions -----------------------------------------------------------------

    fn truthy(&mut self, e: &'static Expr, env: &Env) -> Result<bool, Thrown> {
        let v = self.eval(e, env)?;
        self.value_truthy(&v)
    }

    /// Conditions accept `bool` or numeric (`!= 0`), per §3.3 — nothing else.
    fn value_truthy(&self, v: &Value) -> Result<bool, Thrown> {
        Ok(match v {
            Value::Bool(b) => *b,
            Value::I32(n) => *n != 0,
            Value::I64(n) => *n != 0,
            Value::U32(n) => *n != 0,
            Value::U64(n) => *n != 0,
            Value::F32(f) => *f != 0.0,
            Value::F64(f) => *f != 0.0,
            _ => {
                return Err(self.throw(
                    "TypeError",
                    "condition must be bool or numeric (§3.3); write the comparison",
                ))
            }
        })
    }

    /// Evaluate an expression, then apply whatever numeric conversion the
    /// checker recorded for it (§3.3) — the same conversion the bytecode
    /// compiler turns into a `Convert` op. The tree-walker is the differential
    /// oracle: if it did not do this, it would disagree with the VM about what
    /// the program *means*, and the tests would be comparing two answers neither
    /// of which is the language's.
    fn eval(&mut self, e: &'static Expr, env: &Env) -> VResult {
        let Some(to) = vm::coercion_for(e) else {
            return self.eval_uncoerced(e, env);
        };
        // A literal is built *at* its declared type, never converted into it.
        // `let b: uint32 = 4294967295` has no int32 to convert from — reading it
        // as one is a range error for a value that fits the type it was given.
        if let Some(v) = vm::fold_const(e, to) {
            return Ok(v);
        }
        let v = self.eval_uncoerced(e, env)?;
        Ok(vm::convert_num(&v, to))
    }

    fn eval_uncoerced(&mut self, e: &'static Expr, env: &Env) -> VResult {
        match e {
            Expr::Ident(n) => env_get(env, &n.text).ok_or_else(|| {
                if self.absent_globals.contains(&n.text) {
                    return self.throw(
                        "TypeError",
                        format!("`{}` is not available in this host (no web bridge)", n.text),
                    );
                }
                self.throw("TypeError", format!("`{}` is not defined", n.text))
            }),
            Expr::This(_) => env_get(env, "this")
                .ok_or_else(|| self.throw("TypeError", "`this` is not available here")),
            Expr::Lit { kind, text, .. } => self.eval_literal(*kind, text),
            Expr::Template(parts) => {
                let mut out = String::new();
                for p in parts {
                    match p {
                        TplPart::Text(t) => out.push_str(&unescape(t)),
                        TplPart::Expr(e) => {
                            let v = self.eval(e, env)?;
                            let shown = self.display(&v)?;
                            out.push_str(&shown);
                        }
                    }
                }
                Ok(Value::Str(Rc::new(utf16(&(out)))))
            }
            Expr::Array(elems) => {
                let mut items = Vec::new();
                for el in elems {
                    let v = self.eval(&el.expr, env)?;
                    if el.spread {
                        match v {
                            Value::Array(a) => items.extend(a.borrow().iter().cloned()),
                            _ => return self.type_error("can only spread arrays"),
                        }
                    } else {
                        items.push(v);
                    }
                }
                Ok(new_array(items))
            }
            Expr::Record(fields) => {
                let mut out: Vec<(String, Value)> = Vec::new();
                for f in fields {
                    match f {
                        RecordField::Named { name, value } => {
                            let v = match value {
                                Some(e) => self.eval(e, env)?,
                                None => {
                                    let v = env_get(env, &name.text).ok_or_else(|| {
                                        self.throw(
                                            "TypeError",
                                            format!("`{}` is not defined", name.text),
                                        )
                                    })?;
                                    // `{ x }` may widen (§3.3); the conversion is
                                    // keyed on the field name, there being no
                                    // expression to key it on.
                                    match vm::coercion_for_name(name) {
                                        Some(to) => vm::convert_num(&v, to),
                                        None => v,
                                    }
                                }
                            };
                            rec_set(&mut out, &name.text, v);
                        }
                        RecordField::Spread(e) => {
                            let v = self.eval(e, env)?;
                            match v {
                                Value::Record(r) => {
                                    for (k, val) in r.borrow().iter() {
                                        rec_set(&mut out, k, val.clone());
                                    }
                                }
                                _ => return self.type_error("can only spread records"),
                            }
                        }
                    }
                }
                Ok(new_record(out))
            }
            Expr::Paren(e) => self.eval(e, env),
            Expr::Arrow {
                is_async,
                params,
                ret,
                body,
            } => {
                let data = Rc::new(FnData::new(
                    "<arrow>".into(),
                    *is_async,
                    params,
                    match body {
                        ArrowBody::Expr(e) => FnBody::Expr(e),
                        ArrowBody::Block(b) => FnBody::Block(b),
                    },
                    ret.as_ref(),
                ));
                // Arrows capture `this` lexically.
                let this = env_get(env, "this");
                Ok(Value::Closure(Rc::new(Closure {
                    data,
                    env: env.clone(),
                    this,
                    cls: None,
                })))
            }
            Expr::Unary { op, expr, .. } => {
                if *op == UnaryOp::Await {
                    return self.type_error("`await` is not in the MVP");
                }
                // `-2147483648` is one literal, not a negation of one.
                if let (
                    UnaryOp::Neg,
                    Expr::Lit {
                        kind: LitKind::Int,
                        text,
                        ..
                    },
                ) = (op, &**expr)
                {
                    return negated_int_literal(text)
                        .map_err(|(class, msg)| self.throw(class, msg));
                }
                let v = self.eval(expr, env)?;
                self.eval_unary(*op, v)
            }
            Expr::Update { prefix, inc, expr } => {
                let old = self.eval(expr, env)?;
                let one = Value::I32(1);
                let new = self.numeric_binop(
                    if *inc { BinOp::Add } else { BinOp::Sub },
                    old.clone(),
                    one,
                )?;
                self.assign_to(expr, new.clone(), env)?;
                Ok(if *prefix { new } else { old })
            }
            Expr::Binary { op, l, r } => match op {
                BinOp::And => {
                    let lv = self.eval(l, env)?;
                    if !self.value_truthy(&lv)? {
                        return Ok(Value::Bool(false));
                    }
                    let rv = self.eval(r, env)?;
                    Ok(Value::Bool(self.value_truthy(&rv)?))
                }
                BinOp::Or => {
                    let lv = self.eval(l, env)?;
                    if self.value_truthy(&lv)? {
                        return Ok(Value::Bool(true));
                    }
                    let rv = self.eval(r, env)?;
                    Ok(Value::Bool(self.value_truthy(&rv)?))
                }
                BinOp::Coalesce => {
                    let lv = self.eval(l, env)?;
                    if matches!(lv, Value::Null) {
                        self.eval(r, env)
                    } else {
                        Ok(lv)
                    }
                }
                BinOp::Instanceof => {
                    let lv = self.eval(l, env)?;
                    let rv = self.eval(r, env)?;
                    self.instance_of(&lv, &rv)
                }
                BinOp::Eq | BinOp::Ne => {
                    let lv = self.eval(l, env)?;
                    let rv = self.eval(r, env)?;
                    let eq = self.values_equal(&lv, &rv)?;
                    Ok(Value::Bool(if *op == BinOp::Eq { eq } else { !eq }))
                }
                _ => {
                    let lv = self.eval(l, env)?;
                    let rv = self.eval(r, env)?;
                    self.numeric_binop(*op, lv, rv)
                }
            },
            Expr::Assign { op, target, value } => {
                let rhs = self.eval(value, env)?;
                let new = if *op == "=" {
                    rhs
                } else {
                    let old = self.eval(target, env)?;
                    match *op {
                        "&&=" => {
                            let keep = self.value_truthy(&old)?;
                            if keep {
                                rhs
                            } else {
                                old
                            }
                        }
                        "||=" => {
                            let keep = self.value_truthy(&old)?;
                            if keep {
                                old
                            } else {
                                rhs
                            }
                        }
                        "??=" => {
                            if matches!(old, Value::Null) {
                                rhs
                            } else {
                                old
                            }
                        }
                        _ => {
                            let bin = match *op {
                                "+=" => BinOp::Add,
                                "-=" => BinOp::Sub,
                                "*=" => BinOp::Mul,
                                "/=" => BinOp::Div,
                                "%=" => BinOp::Rem,
                                "**=" => BinOp::Pow,
                                "<<=" => BinOp::Shl,
                                ">>=" => BinOp::Shr,
                                "&=" => BinOp::BitAnd,
                                "|=" => BinOp::BitOr,
                                "^=" => BinOp::BitXor,
                                _ => return self.type_error("unknown assignment operator"),
                            };
                            self.numeric_binop(bin, old, rhs)?
                        }
                    }
                };
                // §3.3 rule 6: a compound assignment computes in the common type
                // and converts back to the target's — the same conversion the VM
                // emits after the operator.
                let new = match vm::result_coercion_for(e) {
                    Some(to) => vm::convert_num(&new, to),
                    None => new,
                };
                self.assign_to(target, new.clone(), env)?;
                Ok(new)
            }
            Expr::Cond { cond, then, els } => {
                let c = self.eval(cond, env)?;
                if self.value_truthy(&c)? {
                    self.eval(then, env)
                } else {
                    self.eval(els, env)
                }
            }
            Expr::Cast { expr, wrapping, ty } => {
                let v = self.eval(expr, env)?;
                self.eval_cast(v, *wrapping, ty)
            }
            Expr::Is { expr, ty } => {
                let v = self.eval(expr, env)?;
                Ok(Value::Bool(self.value_is(&v, ty)))
            }
            Expr::Call {
                callee,
                args,
                optional,
                ..
            } => {
                // Receiver/callee evaluates before the arguments; a null
                // receiver under `?.` skips argument evaluation entirely.
                if let Expr::Member {
                    obj,
                    name,
                    optional: mopt,
                } = callee.as_ref()
                {
                    let recv = self.eval(obj, env)?;
                    if (*mopt || *optional) && matches!(recv, Value::Null) {
                        return Ok(Value::Null);
                    }
                    let argv = self.eval_args(args, env)?;
                    return self.call_member(&recv, name, argv);
                }
                if let Expr::SuperMember { name, .. } = callee.as_ref() {
                    let argv = self.eval_args(args, env)?;
                    return self.call_super_method(name, argv, env_get(env, "this"));
                }
                let f = self.eval(callee, env)?;
                if *optional && matches!(f, Value::Null) {
                    return Ok(Value::Null);
                }
                let argv = self.eval_args(args, env)?;
                self.call_value(&f, argv)
            }
            Expr::New { ty, args } => {
                let TypeExpr::Named { name, .. } = ty else {
                    return self.type_error("`new` needs a class");
                };
                let argv = self.eval_args(args, env)?;
                self.new_named(name, argv, env)
            }
            Expr::Member {
                obj,
                name,
                optional,
            } => {
                let o = self.eval(obj, env)?;
                if *optional && matches!(o, Value::Null) {
                    return Ok(Value::Null);
                }
                match self.get_member(&o, name)? {
                    Some(v) => Ok(v),
                    None => self.type_error(format!("no member `{name}` on {}", kind_of(&o))),
                }
            }
            Expr::Index {
                obj,
                index,
                optional,
            } => {
                let o = self.eval(obj, env)?;
                if *optional && matches!(o, Value::Null) {
                    return Ok(Value::Null);
                }
                let i = self.eval(index, env)?;
                self.index_get(&o, &i)
            }
            Expr::SuperMember { name, .. } => {
                // Non-call super member: resolve to a bound closure.
                self.super_lookup(name, env_get(env, "this"))
            }
            Expr::SuperCall { args, .. } => {
                let argv = self.eval_args(args, env)?;
                self.super_call(argv, env_get(env, "this"))
            }
            Expr::ImportCall(inner) => {
                let spec = match &**inner {
                    Expr::Lit {
                        kind: LitKind::Str,
                        text,
                        ..
                    } => mersey_front::ast::string_value(text),
                    // The checker rejects a non-literal specifier (§4.5).
                    _ => return self.type_error("`import(…)` needs a literal specifier"),
                };
                self.dynamic_import(&spec)
            }
            // Generators run on the VM (only it can suspend); reaching here
            // means the AST tier was asked to run one.
            Expr::Yield { .. } => self.type_error("`yield` requires the bytecode VM"),
        }
    }

    fn eval_args(&mut self, args: &'static [ArrayElem], env: &Env) -> Result<Vec<Value>, Thrown> {
        let mut out = Vec::new();
        for a in args {
            let v = self.eval(&a.expr, env)?;
            if a.spread {
                match v {
                    Value::Array(arr) => out.extend(arr.borrow().iter().cloned()),
                    _ => return self.type_error("can only spread arrays"),
                }
            } else {
                out.push(v);
            }
        }
        Ok(out)
    }

    fn call_member(&mut self, recv: &Value, name: &str, args: Vec<Value>) -> VResult {
        match recv {
            Value::IterV(g) => match name {
                "next" => self.gen_next(g.clone()),
                "toArray" => {
                    let mut out = Vec::new();
                    loop {
                        match self.gen_next(g.clone())? {
                            Value::Null => break,
                            v => out.push(v),
                        }
                    }
                    Ok(new_array(out))
                }
                "map" | "filter" | "take" => self.iter_adapt(g.clone(), name, args),
                _ => self.type_error(format!("no method `{name}` on Iter")),
            },
            Value::PromiseV(p) => {
                let p = p.clone();
                let mut it = args.into_iter();
                match name {
                    "then" => {
                        let ok = it.next();
                        let err = it.next();
                        Ok(self.promise_then(&p, ok, err))
                    }
                    "catch" => {
                        let err = it.next();
                        Ok(self.promise_then(&p, None, err))
                    }
                    _ => self.type_error(format!("no method `{name}` on Promise")),
                }
            }
            Value::JsRef(h) => {
                let h = *h;
                self.web_call(h, name, args)
            }
            Value::Array(a) => {
                let a = a.clone();
                let items = || a.borrow().clone();
                match name {
                    "push" => {
                        for v in args {
                            a.borrow_mut().push(v);
                        }
                        Ok(Value::Null)
                    }
                    "pop" => Ok(a.borrow_mut().pop().unwrap_or(Value::Null)),
                    "clear" => {
                        a.borrow_mut().clear();
                        Ok(Value::Null)
                    }
                    "keys" => {
                        let n = a.borrow().len();
                        Ok(new_array((0..n).map(|i| Value::I32(i as i32)).collect()))
                    }
                    "join" => {
                        let sep = match args.first() {
                            Some(Value::Str(s)) => utf16_to_string(s),
                            _ => String::new(),
                        };
                        let items = a.borrow().clone();
                        let mut parts: Vec<String> = Vec::with_capacity(items.len());
                        for it in &items {
                            parts.push(self.display(it)?);
                        }
                        Ok(Value::Str(Rc::new(utf16(&(parts.join(&sep))))))
                    }
                    "map" => {
                        let f = args.first().cloned().unwrap_or(Value::Null);
                        let mut out = Vec::new();
                        for item in items() {
                            out.push(self.call_value(&f, vec![item])?);
                        }
                        Ok(new_array(out))
                    }
                    "filter" => {
                        let f = args.first().cloned().unwrap_or(Value::Null);
                        let mut out = Vec::new();
                        for item in items() {
                            let keep = self.call_value(&f, vec![item.clone()])?;
                            if self.value_truthy(&keep)? {
                                out.push(item);
                            }
                        }
                        Ok(new_array(out))
                    }
                    "reduce" => {
                        let f = args.first().cloned().unwrap_or(Value::Null);
                        let mut acc = args.get(1).cloned().unwrap_or(Value::Null);
                        for item in items() {
                            acc = self.call_value(&f, vec![acc, item])?;
                        }
                        Ok(acc)
                    }
                    "forEach" => {
                        let f = args.first().cloned().unwrap_or(Value::Null);
                        for item in items() {
                            self.call_value(&f, vec![item])?;
                        }
                        Ok(Value::Null)
                    }
                    "find" | "findIndex" | "some" | "every" => {
                        let f = args.first().cloned().unwrap_or(Value::Null);
                        let want_all = name == "every";
                        for (i, item) in items().into_iter().enumerate() {
                            let hit = self.call_value(&f, vec![item.clone()])?;
                            let hit = self.value_truthy(&hit)?;
                            if hit && !want_all {
                                return Ok(match name {
                                    "find" => item,
                                    "findIndex" => Value::I32(i as i32),
                                    _ => Value::Bool(true),
                                });
                            }
                            if !hit && want_all {
                                return Ok(Value::Bool(false));
                            }
                        }
                        Ok(match name {
                            "find" => Value::Null,
                            "findIndex" => Value::I32(-1),
                            "some" => Value::Bool(false),
                            _ => Value::Bool(true),
                        })
                    }
                    "indexOf" | "contains" => {
                        let want = args.first().cloned().unwrap_or(Value::Null);
                        for (i, item) in items().into_iter().enumerate() {
                            if self.values_equal(&item, &want)? {
                                return Ok(if name == "contains" {
                                    Value::Bool(true)
                                } else {
                                    Value::I32(i as i32)
                                });
                            }
                        }
                        Ok(if name == "contains" {
                            Value::Bool(false)
                        } else {
                            Value::I32(-1)
                        })
                    }
                    "lastIndexOf" => {
                        let want = args.first().cloned().unwrap_or(Value::Null);
                        let src = items();
                        for (i, item) in src.iter().enumerate().rev() {
                            if self.values_equal(item, &want)? {
                                return Ok(Value::I32(i as i32));
                            }
                        }
                        Ok(Value::I32(-1))
                    }
                    // Indexing that admits it can miss, and counts from the end
                    // for a negative index.
                    "at" => {
                        let src = items();
                        let i = args.first().and_then(as_i64).unwrap_or(0);
                        Ok(resolve_at(i, src.len())
                            .and_then(|i| src.get(i).cloned())
                            .unwrap_or(Value::Null))
                    }
                    "insertAt" => {
                        let n = a.borrow().len();
                        let i = args.first().and_then(as_i64).unwrap_or(0);
                        let v = args.get(1).cloned().unwrap_or(Value::Null);
                        // Inserting *at* the end is meaningful, so the index may
                        // be one past the last element.
                        let Some(i) = resolve_at(i, n + 1) else {
                            return Err(self.throw(
                                "RangeError",
                                format!("insertAt: index {i} is outside 0..={n}"),
                            ));
                        };
                        a.borrow_mut().insert(i, v);
                        Ok(Value::Null)
                    }
                    "removeAt" => {
                        let n = a.borrow().len();
                        let i = args.first().and_then(as_i64).unwrap_or(0);
                        Ok(match resolve_at(i, n) {
                            Some(i) => a.borrow_mut().remove(i),
                            None => Value::Null,
                        })
                    }
                    "fillInPlace" => {
                        let v = args.first().cloned().unwrap_or(Value::Null);
                        for item in a.borrow_mut().iter_mut() {
                            *item = v.clone();
                        }
                        Ok(Value::Null)
                    }
                    "flat" => {
                        let mut out = Vec::new();
                        for item in items() {
                            match item {
                                Value::Array(inner) => out.extend(inner.borrow().iter().cloned()),
                                other => out.push(other),
                            }
                        }
                        Ok(new_array(out))
                    }
                    "slice" => {
                        let src = items();
                        let len = src.len() as i64;
                        let norm = |v: i64| v.clamp(0, len) as usize;
                        let start = norm(args.first().and_then(as_i64).unwrap_or(0));
                        let end = norm(args.get(1).and_then(as_i64).unwrap_or(len));
                        let out = if start < end {
                            src[start..end].to_vec()
                        } else {
                            Vec::new()
                        };
                        Ok(new_array(out))
                    }
                    "concat" => {
                        let mut out = items();
                        if let Some(Value::Array(b)) = args.first() {
                            out.extend(b.borrow().iter().cloned());
                        }
                        Ok(new_array(out))
                    }
                    "reverseInPlace" => {
                        a.borrow_mut().reverse();
                        Ok(Value::Null)
                    }
                    "toReversed" => {
                        let mut out = items();
                        out.reverse();
                        Ok(new_array(out))
                    }
                    // Comparator-driven sort: merge sort so the comparator is
                    // called a predictable number of times and the sort is
                    // stable (a comparator can throw, so it must be fallible).
                    "sortInPlace" | "toSorted" => {
                        let f = args.first().cloned().unwrap_or(Value::Null);
                        let sorted = self.merge_sort(items(), &f)?;
                        if name == "sortInPlace" {
                            *a.borrow_mut() = sorted;
                            Ok(Value::Null)
                        } else {
                            Ok(new_array(sorted))
                        }
                    }
                    "toString" => Ok(Value::Str(Rc::new(utf16(&(to_display(recv)))))),
                    _ => self.type_error(format!("arrays have no method `{name}`")),
                }
            }
            Value::MapV(m) => {
                let m = m.clone();
                match name {
                    "set" => {
                        let (k, v) = (
                            args.first().cloned().unwrap_or(Value::Null),
                            args.get(1).cloned().unwrap_or(Value::Null),
                        );
                        // `insert` on an existing key keeps its original
                        // position, which is the insertion order the language
                        // promises: re-setting a key does not move it.
                        m.borrow_mut().insert(Key(k), v);
                        Ok(Value::Null)
                    }
                    "get" => {
                        let k = args.first().cloned().unwrap_or(Value::Null);
                        Ok(m.borrow().get(&Key(k)).cloned().unwrap_or(Value::Null))
                    }
                    "has" => {
                        let k = args.first().cloned().unwrap_or(Value::Null);
                        Ok(Value::Bool(m.borrow().contains_key(&Key(k))))
                    }
                    "remove" => {
                        let k = args.first().cloned().unwrap_or(Value::Null);
                        // `shift_remove`, not `swap_remove`: order survives a
                        // removal. It is O(n), and removal is the rare op.
                        Ok(Value::Bool(m.borrow_mut().shift_remove(&Key(k)).is_some()))
                    }
                    "keys" => Ok(new_array(m.borrow().keys().map(|k| k.0.clone()).collect())),
                    "values" => Ok(new_array(m.borrow().values().cloned().collect())),
                    "entries" => {
                        let pairs: Vec<Value> = m
                            .borrow()
                            .iter()
                            .map(|(k, v)| new_array(vec![k.0.clone(), v.clone()]))
                            .collect();
                        Ok(new_array(pairs))
                    }
                    "clear" => {
                        m.borrow_mut().clear();
                        Ok(Value::Null)
                    }
                    "toString" => Ok(Value::Str(Rc::new(utf16(&(to_display(recv)))))),
                    _ => self.type_error(format!("no method `{name}` on Map")),
                }
            }
            Value::SetV(m) => {
                let m = m.clone();
                match name {
                    "add" => {
                        let v = args.first().cloned().unwrap_or(Value::Null);
                        m.borrow_mut().insert(Key(v));
                        Ok(Value::Null)
                    }
                    "has" => {
                        let v = args.first().cloned().unwrap_or(Value::Null);
                        Ok(Value::Bool(m.borrow().contains(&Key(v))))
                    }
                    "remove" => {
                        let v = args.first().cloned().unwrap_or(Value::Null);
                        // Order survives a removal — see the Map note.
                        Ok(Value::Bool(m.borrow_mut().shift_remove(&Key(v))))
                    }
                    "values" => Ok(new_array(m.borrow().iter().map(|k| k.0.clone()).collect())),
                    "clear" => {
                        m.borrow_mut().clear();
                        Ok(Value::Null)
                    }
                    "toString" => Ok(Value::Str(Rc::new(utf16(&(to_display(recv)))))),
                    _ => self.type_error(format!("no method `{name}` on Set")),
                }
            }
            Value::RegexV(re) => {
                let re = re.clone();
                let Some(Value::Str(subject)) = args.first() else {
                    return self.type_error(format!("regex `{name}` needs a string"));
                };
                // Regex matches on code points; decode here, re-encode results.
                let chars: Vec<char> = utf16_to_chars(subject);
                let slice = |a: usize, b: usize| -> Value {
                    Value::Str(Rc::new(chars_to_u16(&chars[a..b])))
                };
                let make_match = |m: &regex::Match| -> Value {
                    let groups: Vec<Value> = m
                        .groups
                        .iter()
                        .map(|g| match g {
                            Some((a, b)) => slice(*a, *b),
                            None => Value::Null,
                        })
                        .collect();
                    new_record(vec![
                        ("text".into(), slice(m.start, m.end)),
                        ("start".into(), Value::I32(m.start as i32)),
                        ("end".into(), Value::I32(m.end as i32)),
                        ("groups".into(), new_array(groups)),
                    ])
                };
                match name {
                    "test" => Ok(Value::Bool(re.is_match(&chars))),
                    "find" => Ok(match re.find_at(&chars, 0) {
                        Some(m) => make_match(&m),
                        None => Value::Null,
                    }),
                    "findAll" => {
                        let mut out = Vec::new();
                        let mut at = 0;
                        while at <= chars.len() {
                            match re.find_at(&chars, at) {
                                Some(m) => {
                                    at = if m.end > m.start { m.end } else { m.start + 1 };
                                    out.push(make_match(&m));
                                }
                                None => break,
                            }
                        }
                        Ok(new_array(out))
                    }
                    "replace" => {
                        let with = match args.get(1) {
                            Some(Value::Str(w)) => utf16_to_string(w),
                            Some(other) => to_display(other),
                            None => String::new(),
                        };
                        Ok(match re.find_at(&chars, 0) {
                            Some(m) => {
                                let mut out: Vec<char> = chars[..m.start].to_vec();
                                out.extend(with.chars());
                                out.extend(&chars[m.end..]);
                                Value::Str(Rc::new(chars_to_u16(&out)))
                            }
                            None => Value::Str(Rc::new(chars_to_u16(&chars))),
                        })
                    }
                    "replaceAll" => {
                        let with = match args.get(1) {
                            Some(Value::Str(w)) => utf16_to_string(w),
                            Some(other) => to_display(other),
                            None => String::new(),
                        };
                        let mut out: Vec<char> = Vec::new();
                        let mut at = 0;
                        while at <= chars.len() {
                            match re.find_at(&chars, at) {
                                Some(m) => {
                                    out.extend_from_slice(&chars[at..m.start]);
                                    out.extend(with.chars());
                                    at = if m.end > m.start {
                                        m.end
                                    } else {
                                        if m.start < chars.len() {
                                            out.push(chars[m.start]);
                                        }
                                        m.start + 1
                                    };
                                }
                                None => break,
                            }
                        }
                        if at < chars.len() {
                            out.extend_from_slice(&chars[at..]);
                        }
                        Ok(Value::Str(Rc::new(chars_to_u16(&out))))
                    }
                    "split" => {
                        let mut parts = Vec::new();
                        let mut at = 0;
                        let mut last = 0;
                        while at <= chars.len() {
                            match re.find_at(&chars, at) {
                                Some(m) if m.end > m.start => {
                                    parts.push(slice(last, m.start));
                                    last = m.end;
                                    at = m.end;
                                }
                                _ => break,
                            }
                        }
                        parts.push(slice(last, chars.len()));
                        Ok(new_array(parts))
                    }
                    _ => self.type_error(format!("no method `{name}` on Regex")),
                }
            }
            Value::Str(s) => {
                // Methods that read only the UTF-16 code units answer here,
                // BEFORE the UTF-8 transcode below. `utf16_to_string` copies the
                // whole receiver, so doing it up front charged every string method
                // the price of the most expensive one: a `slice` or a `charAt` cost
                // as much as a `toUpperCase`. Measured on a 46-char string, that is
                // ~250ns a call — which is most of what URL parsing spends.
                let units_only: Option<VResult> = match name {
                    "toString" => Some(Ok(Value::Str(s.clone()))),
                    "substring" => Some({
                        let len = s.len() as i64;
                        let norm = |v: i64| v.clamp(0, len) as usize;
                        let a = norm(args.first().and_then(as_i64).unwrap_or(0));
                        let b = norm(args.get(1).and_then(as_i64).unwrap_or(len));
                        // Bounds the wrong way round are swapped, not empty —
                        // this is what distinguishes it from `slice`.
                        let (start, end) = if a <= b { (a, b) } else { (b, a) };
                        Ok(Value::Str(Rc::new(s[start..end].to_vec())))
                    }),
                    "concat" => Some({
                        let mut out: Vec<u16> = s.to_vec();
                        for a in &args {
                            match a {
                                Value::Str(other) => out.extend(other.iter()),
                                other => out.extend(utf16(&to_display(other))),
                            }
                        }
                        Ok(Value::Str(Rc::new(out)))
                    }),
                    "charAt" => Some({
                        // The i-th UTF-16 code unit as a 1-unit string (JS).
                        let i = args.first().and_then(as_i64).unwrap_or(0);
                        let out: Vec<u16> = resolve_at(i, s.len())
                            .and_then(|i| s.get(i).copied())
                            .into_iter()
                            .collect();
                        Ok(Value::Str(Rc::new(out)))
                    }),
                    "codePointAt" => Some({
                        // The code point starting at code-unit i (JS: combines a
                        // surrogate pair).
                        let i = args.first().and_then(as_i64).unwrap_or(0);
                        Ok(resolve_at(i, s.len())
                            .and_then(|i| code_point_at(s, i))
                            .map(|c| Value::I32(c as i32))
                            .unwrap_or(Value::Null))
                    }),
                    "at" => Some({
                        // Char-returning: the code point at code-unit i.
                        let i = args.first().and_then(as_i64).unwrap_or(0);
                        Ok(resolve_at(i, s.len())
                            .and_then(|i| code_point_at(s, i))
                            .map(Value::Char)
                            .unwrap_or(Value::Null))
                    }),
                    "slice" => Some({
                        let len = s.len() as i64;
                        let norm = |v: i64| v.clamp(0, len) as usize;
                        let start = norm(args.first().and_then(as_i64).unwrap_or(0));
                        let end = norm(args.get(1).and_then(as_i64).unwrap_or(len));
                        let out: Vec<u16> = if start < end {
                            s[start..end].to_vec()
                        } else {
                            Vec::new()
                        };
                        Ok(Value::Str(Rc::new(out)))
                    }),
                    _ => None,
                };
                if let Some(r) = units_only {
                    return r;
                }
                let text: String = utf16_to_string(s);
                let arg0 = || -> String {
                    match args.first() {
                        Some(Value::Str(a)) => utf16_to_string(a),
                        Some(other) => to_display(other),
                        None => String::new(),
                    }
                };
                match name {
                    "indexOf" => {
                        let needle = arg0();
                        // Code-point index, not byte index (§3.4).
                        Ok(Value::I32(match text.find(&needle) {
                            Some(b) => text[..b].chars().count() as i32,
                            None => -1,
                        }))
                    }
                    "contains" => Ok(Value::Bool(text.contains(&arg0()))),
                    "lastIndexOf" => {
                        let needle = arg0();
                        // Code-point index, not byte index (§3.4).
                        Ok(Value::I32(match text.rfind(&needle) {
                            Some(b) => text[..b].chars().count() as i32,
                            None => -1,
                        }))
                    }
                    "trimStart" => Ok(Value::Str(Rc::new(utf16(text.trim_start())))),
                    "trimEnd" => Ok(Value::Str(Rc::new(utf16(text.trim_end())))),
                    "startsWith" => Ok(Value::Bool(text.starts_with(&arg0()))),
                    "endsWith" => Ok(Value::Bool(text.ends_with(&arg0()))),
                    "toUpperCase" => Ok(Value::Str(Rc::new(utf16(&(text.to_uppercase()))))),
                    "toLowerCase" => Ok(Value::Str(Rc::new(utf16(&(text.to_lowercase()))))),
                    "trim" => Ok(Value::Str(Rc::new(utf16(text.trim())))),
                    "replace" | "replaceAll" => {
                        let needle = arg0();
                        let with = match args.get(1) {
                            Some(Value::Str(a)) => utf16_to_string(a),
                            Some(other) => to_display(other),
                            None => String::new(),
                        };
                        let out = if name == "replace" {
                            text.replacen(&needle as &str, &with, 1)
                        } else {
                            text.replace(&needle as &str, &with)
                        };
                        Ok(Value::Str(Rc::new(utf16(&(out)))))
                    }
                    "repeat" => {
                        let n = args
                            .first()
                            .and_then(as_i64)
                            .unwrap_or(0)
                            .clamp(0, 1_000_000);
                        Ok(Value::Str(Rc::new(utf16(&(text.repeat(n as usize))))))
                    }
                    "padStart" | "padEnd" => {
                        let width = args.first().and_then(as_i64).unwrap_or(0).max(0) as usize;
                        let pad = match args.get(1) {
                            Some(Value::Str(a)) if !a.is_empty() => utf16_to_string(a),
                            _ => " ".to_string(),
                        };
                        let mut out: Vec<u16> = s.as_ref().clone();
                        let pad_chars: Vec<u16> = utf16(&(pad));
                        let mut k = 0;
                        while out.len() < width {
                            let c = pad_chars[k % pad_chars.len()];
                            if name == "padStart" {
                                out.insert(k, c);
                            } else {
                                out.push(c);
                            }
                            k += 1;
                        }
                        Ok(Value::Str(Rc::new(out)))
                    }
                    "split" => {
                        let sep = arg0();
                        let parts: Vec<Value> = if sep.is_empty() {
                            s.iter().map(|u| Value::Str(Rc::new(vec![*u]))).collect()
                        } else {
                            text.split(&sep as &str)
                                .map(|p| Value::Str(Rc::new(utf16(p))))
                                .collect()
                        };
                        Ok(new_array(parts))
                    }
                    _ => self.type_error(format!("no method `{name}` on string")),
                }
            }
            // bigdec.divide(other, { scale: 2, mode: "HALF_EVEN" }) — §3.7
            Value::BigDecV(a) if name == "divide" => {
                let Some(Value::BigDecV(b)) = args.first() else {
                    return self.type_error("divide(divisor, context) needs a bigdec divisor");
                };
                let ctx = args.get(1);
                let (scale, mode) = match ctx {
                    Some(Value::Record(fields)) => {
                        let f = fields.borrow();
                        let scale = rec_get(&f, "scale")
                            .and_then(|v| as_i64(&v))
                            .unwrap_or(0)
                            .clamp(0, 1000) as u32;
                        let mode_name = match rec_get(&f, "mode") {
                            Some(Value::Str(s)) => utf16_to_string(&s),
                            _ => "HALF_EVEN".to_string(),
                        };
                        let Some(mode) = RoundingMode::parse(&mode_name) else {
                            return self.type_error(format!("unknown rounding mode `{mode_name}`"));
                        };
                        (scale, mode)
                    }
                    _ => return self.type_error("divide needs a rounding context"),
                };
                match a.divide(b, scale, mode) {
                    Some(q) => Ok(Value::BigDecV(Rc::new(q))),
                    None => Err(self.throw("RangeError", "division by zero")),
                }
            }
            Value::Char(_)
            | Value::I32(_)
            | Value::I64(_)
            | Value::U32(_)
            | Value::U64(_)
            | Value::F32(_)
            | Value::F64(_)
            | Value::Bool(_)
                if name == "toString" =>
            {
                Ok(Value::Str(Rc::new(utf16(&(to_display(recv))))))
            }
            Value::Dom(_) if name == "addEventListener" => {
                self.call_native("dom.addEventListener", Some(recv), args)
            }
            Value::Dom(_) if name == "appendChild" => {
                self.call_native("dom.appendChild", Some(recv), args)
            }
            Value::Dom(_) if name == "remove" => self.call_native("dom.remove", Some(recv), args),
            Value::Namespace(ns) => match ns.entries.get(name) {
                Some(Value::Native(n)) => {
                    let n = *n;
                    self.call_native(n, Some(recv), args)
                }
                Some(v @ Value::Closure(_)) => {
                    let v = v.clone();
                    self.call_value(&v, args)
                }
                _ => self.type_error(format!("no member `{name}` on `{}`", ns.name)),
            },
            // Host-backed instances: a method not declared in Mersey is the
            // host's (`this.addEventListener(…)`).
            Value::Instance(inst) => {
                let declared_in_mersey = {
                    let i = inst.borrow();
                    i.class.field_slots.contains_key(name)
                        || find_in_chain(&i.class, |c| c.methods.get(name).map(|_| ())).is_some()
                        || find_in_chain(&i.class, |c| c.getters.get(name).map(|_| ())).is_some()
                };
                let host = inst.borrow().host;
                if !declared_in_mersey {
                    if let Some(h) = host {
                        return self.web_call(h, name, args);
                    }
                }
                let member = self.get_member(recv, name)?;
                match member {
                    Some(f) => self.call_value(&f, args),
                    None => self.type_error(format!("no method `{name}` on {}", kind_of(recv))),
                }
            }
            _ => {
                let member = self.get_member(recv, name)?;
                match member {
                    Some(f) => self.call_value(&f, args),
                    None => self.type_error(format!("no method `{name}` on {}", kind_of(recv))),
                }
            }
        }
    }

    fn index_get(&mut self, o: &Value, i: &Value) -> VResult {
        if let Value::Bytes(b) = o {
            let ix = as_i64(i).unwrap_or(-1);
            let bytes = b.borrow();
            return if ix < 0 || ix as usize >= bytes.len() {
                Err(self.throw(
                    "RangeError",
                    format!("index {ix} out of bounds (length {})", bytes.len()),
                ))
            } else {
                Ok(Value::I32(bytes[ix as usize] as i32))
            };
        }
        // Host objects: `list[0]`, `obj["key"]` → bridge property read.
        if let Value::JsRef(h) = o {
            let (h, prop) = (*h, to_display(i));
            return self.web_get(h, &prop);
        }
        match (o, as_i64(i)) {
            (Value::Array(a), Some(ix)) => {
                let a = a.borrow();
                if ix < 0 || ix as usize >= a.len() {
                    Err(self.throw(
                        "RangeError",
                        format!("index {ix} out of bounds (length {})", a.len()),
                    ))
                } else {
                    Ok(a[ix as usize].clone())
                }
            }
            (Value::Str(s), Some(ix)) => {
                if ix < 0 || ix as usize >= s.len() {
                    Err(self.throw(
                        "RangeError",
                        format!("index {ix} out of bounds (length {})", s.len()),
                    ))
                } else {
                    Ok(Value::Char(
                        code_point_at(s, ix as usize).unwrap_or('\u{FFFD}'),
                    ))
                }
            }
            _ => self.type_error("only arrays and strings are indexable"),
        }
    }

    fn index_set(&mut self, o: &Value, i: &Value, value: Value) -> Result<(), Thrown> {
        if let Value::Bytes(b) = o {
            let ix = as_i64(i).unwrap_or(-1);
            let v = as_i64(&value).unwrap_or(0);
            let mut bytes = b.borrow_mut();
            return if ix < 0 || ix as usize >= bytes.len() {
                Err(self.throw(
                    "RangeError",
                    format!("index {ix} out of bounds (length {})", bytes.len()),
                ))
            } else {
                // Wrapping, like a Uint8 store (§3.6).
                bytes[ix as usize] = (v & 0xFF) as u8;
                Ok(())
            };
        }
        if let Value::JsRef(h) = o {
            let (h, prop) = (*h, to_display(i));
            return self.web_set(h, &prop, value);
        }
        match (o, as_i64(i)) {
            (Value::Array(a), Some(ix)) => {
                let mut a = a.borrow_mut();
                if ix < 0 || ix as usize >= a.len() {
                    Err(self.throw(
                        "RangeError",
                        format!("index {ix} out of bounds (length {})", a.len()),
                    ))
                } else {
                    a[ix as usize] = value;
                    Ok(())
                }
            }
            _ => self.type_error("only array elements can be assigned by index"),
        }
    }

    fn instance_of(&mut self, l: &Value, r: &Value) -> VResult {
        match r {
            // `x instanceof SomeMerseyClass`
            Value::Class(want) => {
                let mut ok = false;
                if let Value::Instance(i) = l {
                    let mut cls = Some(i.borrow().class.clone());
                    while let Some(c) = cls {
                        if Rc::ptr_eq(&c, want) {
                            ok = true;
                            break;
                        }
                        cls = c.parent.clone();
                    }
                }
                Ok(Value::Bool(ok))
            }
            // `x instanceof HTMLElement` — a host interface object. The left
            // side may be a host object, or a host-backed Mersey instance.
            Value::JsRef(ctor) => {
                let target = match l {
                    Value::JsRef(h) => Some(*h),
                    Value::Instance(i) => i.borrow().host,
                    _ => None,
                };
                match target {
                    Some(h) => Ok(Value::Bool(self.host.web_instanceof(h, *ctor))),
                    None => Ok(Value::Bool(false)),
                }
            }
            _ => self.type_error("right side of instanceof must be a class or host interface"),
        }
    }

    // ---- promises, microtasks, coroutines -------------------------------

    /// Settle a promise and queue its reactions/waiters as microtasks.
    fn settle(&mut self, p: &Rc<GcCell<PromiseState>>, value: Value, rejected: bool) {
        {
            let st = p.borrow();
            if st.status != PromiseStatus::Pending {
                return; // already settled: first settle wins
            }
        }
        // Resolving with a promise adopts its state.
        if !rejected {
            if let Value::PromiseV(inner) = &value {
                let inner = inner.clone();
                let outer = p.clone();
                let st = inner.borrow().status.clone();
                match st {
                    PromiseStatus::Pending => {
                        inner.borrow_mut().reactions.push((None, None, outer));
                        return;
                    }
                    PromiseStatus::Fulfilled => {
                        let v = inner.borrow().value.clone();
                        return self.settle(p, v, false);
                    }
                    PromiseStatus::Rejected => {
                        let v = inner.borrow().value.clone();
                        return self.settle(p, v, true);
                    }
                }
            }
        }
        let (waiters, reactions) = {
            let mut st = p.borrow_mut();
            st.status = if rejected {
                PromiseStatus::Rejected
            } else {
                PromiseStatus::Fulfilled
            };
            st.value = value.clone();
            (
                std::mem::take(&mut st.waiters),
                std::mem::take(&mut st.reactions),
            )
        };
        for coro in waiters {
            self.tasks
                .push_back(Task::Resume(coro, value.clone(), rejected));
        }
        for (on_ok, on_err, downstream) in reactions {
            self.tasks.push_back(Task::React(
                on_ok,
                on_err,
                downstream,
                value.clone(),
                rejected,
            ));
        }
    }

    /// Register `then`-style reactions, returning the chained promise.
    fn promise_then(
        &mut self,
        p: &Rc<GcCell<PromiseState>>,
        on_ok: Option<Value>,
        on_err: Option<Value>,
    ) -> Value {
        let downstream = PromiseState::pending();
        let st = p.borrow().status.clone();
        match st {
            PromiseStatus::Pending => {
                p.borrow_mut()
                    .reactions
                    .push((on_ok, on_err, downstream.clone()));
            }
            PromiseStatus::Fulfilled | PromiseStatus::Rejected => {
                let rejected = st == PromiseStatus::Rejected;
                let value = p.borrow().value.clone();
                self.tasks.push_back(Task::React(
                    on_ok,
                    on_err,
                    downstream.clone(),
                    value,
                    rejected,
                ));
            }
        }
        Value::PromiseV(downstream)
    }

    /// Convert any awaitable into a Mersey promise: a host (JS) promise is
    /// adopted by handing it Resolver callbacks through the bridge.
    fn as_promise(&mut self, v: Value) -> Result<Rc<GcCell<PromiseState>>, Thrown> {
        match v {
            Value::PromiseV(p) => Ok(p),
            Value::JsRef(h) => {
                let p = PromiseState::pending();
                let ok = Value::Resolve(p.clone());
                let err = Value::Reject(p.clone());
                // A JS thenable settles our promise through the bridge.
                self.web_call(h, "then", vec![ok, err])?;
                Ok(p)
            }
            other => {
                // Awaiting a plain value: already-resolved promise.
                let p = PromiseState::pending();
                self.settle(&p, other, false);
                Ok(p)
            }
        }
    }

    /// The engine's live set at a safe point (no VM frames on the Rust
    /// stack), for the cycle collector.
    fn gc_roots(&self) -> gc::Roots {
        let mut roots = gc::Roots {
            envs: vec![self.root.clone(), self.globals.clone()],
            classes: self.class_stack.clone(),
            ..Default::default()
        };
        for cls in self.error_classes.values() {
            roots.classes.push(cls.clone());
        }
        for exports in self.modules.values() {
            roots.values.extend(exports.values().cloned());
        }
        roots.values.extend(self.callbacks.iter().cloned());
        for task in &self.tasks {
            match task {
                Task::Resume(coro, v, _) => {
                    roots.values.push(v.clone());
                    roots.coros.push(coro.result.clone());
                    for e in &coro.scopes {
                        roots.envs.push(e.clone());
                    }
                    roots.values.extend(coro.stack.iter().cloned());
                    roots.values.extend(coro.frame.iter().cloned());
                }
                Task::React(ok, err, down, v, _) => {
                    roots.values.extend(ok.iter().cloned());
                    roots.values.extend(err.iter().cloned());
                    roots.values.push(v.clone());
                    roots.coros.push(down.clone());
                }
            }
        }
        for cell in &self.all_cells {
            roots.coros.push(cell.out.clone());
            roots.values.extend(cell.results.borrow().iter().cloned());
        }
        // A graph paused on a top-level `await` is live: its module's scope
        // holds everything the module has built so far.
        if let Some(p) = &self.pending_graph {
            roots.coros.push(p.promise.clone());
            roots.envs.push(p.env.clone());
        }
        roots
    }

    /// Collect cycles. Only safe at a host boundary — see gc.rs.
    pub fn collect_garbage(&mut self) -> gc::GcStats {
        let roots = self.gc_roots();
        self.gc_pending = false;
        // An explicit request means "reclaim what you can", including
        // old-generation cycles, so it gets the full trace.
        gc::collect_major(&roots)
    }

    /// The routine collection: generational, so the pause is bounded by how
    /// much has been allocated since last time rather than by the heap.
    /// Cross-check the reference-counting cycle collector against the tracing
    /// one, from the real roots. See `gc::verify_cycles`.
    pub fn verify_cycles(&mut self) -> Result<(), String> {
        let roots = self.gc_roots();
        gc::verify_cycles(&roots)
    }

    fn collect_young(&mut self) -> gc::GcStats {
        let roots = self.gc_roots();
        gc::collect(&roots)
    }

    /// Collect if requested or if enough has been allocated. Called only at
    /// host boundaries.
    fn maybe_collect(&mut self) {
        if self.gc_pending {
            // Explicit `gc.collect()`: full trace.
            self.collect_garbage();
        } else if gc::should_collect() {
            self.collect_young();
        }
    }

    /// Run microtasks to completion. Called before control returns to the
    /// host, so a turn always leaves the queue empty.
    pub fn drain_microtasks(&mut self) -> Result<(), Thrown> {
        loop {
            self.drain_tasks()?;
            // A module that suspended on a top-level `await` may now be able to
            // finish — and everything importing it is still waiting. Running
            // those modules can queue more microtasks, so this loops.
            if !self.graph_can_resume() {
                return Ok(());
            }
            self.resume_graph()?;
        }
    }

    /// The microtask queue itself.
    fn drain_tasks(&mut self) -> Result<(), Thrown> {
        // Bounded to catch runaway promise loops in hostile input.
        const MAX: u32 = 1_000_000;
        let mut n = 0;
        while let Some(task) = self.tasks.pop_front() {
            n += 1;
            if n > MAX {
                return self.type_error("microtask queue did not settle");
            }
            match task {
                Task::Resume(coro, value, rejected) => {
                    self.resume(coro, value, rejected)?;
                }
                Task::React(on_ok, on_err, downstream, value, rejected) => {
                    let handler = if rejected { on_err } else { on_ok };
                    match handler {
                        Some(f) => match self.call_value(&f, vec![value]) {
                            Ok(out) => self.settle(&downstream, out, false),
                            Err(t) => self.settle(&downstream, t.0, true),
                        },
                        // No handler: pass the settlement through.
                        None => self.settle(&downstream, value, rejected),
                    }
                }
            }
        }
        Ok(())
    }

    /// Start an async function: run its chunk until it completes or awaits.
    fn start_coro(&mut self, c: &Closure, chunk: Rc<vm::Chunk>, scope: Env) -> VResult {
        let result = PromiseState::pending();
        let coro = Coro {
            gen: None,
            frame: vm::new_frame(&chunk, &scope, c.this.as_ref()),
            chunk,
            pc: 0,
            stack: Vec::new(),
            scopes: vec![scope],
            handlers: Vec::new(),
            cls: c.cls.clone(),
            result: result.clone(),
        };
        self.drive(coro, None)?;
        Ok(Value::PromiseV(result))
    }

    fn resume(&mut self, coro: Coro, value: Value, rejected: bool) -> Result<(), Thrown> {
        self.drive(coro, Some((value, rejected)))
    }

    /// Drive a coroutine until it finishes or suspends on an await.
    fn drive(&mut self, mut coro: Coro, resumed: Option<(Value, bool)>) -> Result<(), Thrown> {
        // A coroutine belonging to an async generator settles that generator's
        // pending `next()` when it yields — not its own result promise.
        if let Some(g) = coro.gen.clone() {
            return self.drive_gen(g, coro, resumed);
        }
        let pushed = coro.cls.clone();
        if let Some(cls) = &pushed {
            self.class_stack.push(cls.clone());
        }
        let outcome = vm::run_coro(self, &mut coro, resumed);
        if pushed.is_some() {
            self.class_stack.pop();
        }
        match outcome {
            Ok(vm::Flow::Done(v)) => {
                let result = coro.result.clone();
                self.settle(&result, v, false);
                Ok(())
            }
            Ok(vm::Flow::Yield(_)) => {
                let result = coro.result.clone();
                let t = self.throw("TypeError", "`yield` inside an async function");
                self.settle(&result, t.0, true);
                Ok(())
            }
            Ok(vm::Flow::Await(awaited)) => {
                let p = self.as_promise(awaited)?;
                let status = p.borrow().status.clone();
                match status {
                    PromiseStatus::Pending => {
                        p.borrow_mut().waiters.push(coro);
                    }
                    PromiseStatus::Fulfilled | PromiseStatus::Rejected => {
                        let v = p.borrow().value.clone();
                        let rejected = status == PromiseStatus::Rejected;
                        self.tasks.push_back(Task::Resume(coro, v, rejected));
                    }
                }
                Ok(())
            }
            Err(t) => {
                // An uncaught throw rejects the async function's promise.
                let result = coro.result.clone();
                self.settle(&result, t.0, true);
                Ok(())
            }
        }
    }

    // ---- universal web bridge -------------------------------------------

    /// Mersey value → tagged JSON. Objects become `{"__ref__":n}`,
    /// closures are registered and become `{"__cb__":id}`.
    /// JSON.stringify, engine-side: serialize a pure value tree straight to
    /// JSON text — no bridge call, no double serialization. Returns false (and
    /// the caller uses the real host JSON) if the tree holds anything that is
    /// not plain data: a host handle, a function, a Map/Set, a class instance.
    /// Output matches JS for the values it accepts: insertion-ordered keys,
    /// shortest round-trip numbers, `null` for non-finite.
    fn pure_json(v: &Value, out: &mut String) -> bool {
        use std::fmt::Write as _;
        match v {
            Value::Null => out.push_str("null"),
            Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            // `write!` formats the integer straight into `out`'s buffer; the
            // `&n.to_string()` it replaces heap-allocated a String per scalar,
            // which in a stringify-heavy loop is the bulk of the allocation.
            Value::I32(n) => {
                let _ = write!(out, "{n}");
            }
            Value::I64(n) => {
                let _ = write!(out, "{n}");
            }
            Value::U32(n) => {
                let _ = write!(out, "{n}");
            }
            Value::U64(n) => {
                let _ = write!(out, "{n}");
            }
            Value::F32(f) => {
                let f = *f as f64;
                if f.is_finite() {
                    let _ = write!(out, "{f}");
                } else {
                    out.push_str("null");
                }
            }
            Value::F64(f) => {
                if f.is_finite() {
                    let _ = write!(out, "{f}");
                } else {
                    out.push_str("null");
                }
            }
            Value::Char(c) => {
                let mut buf = [0u16; 2];
                webjson::write_str_u16(out, c.encode_utf16(&mut buf));
            }
            // Escape straight from the engine's UTF-16, no UTF-8 String first.
            Value::Str(s) => webjson::write_str_u16(out, s),
            Value::Array(a) => {
                out.push('[');
                for (i, item) in a.borrow().iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    if !Self::pure_json(item, out) {
                        return false;
                    }
                }
                out.push(']');
            }
            Value::Record(r) => {
                out.push('{');
                for (i, (k, val)) in r.borrow().iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    webjson::write_str(out, k);
                    out.push(':');
                    if !Self::pure_json(val, out) {
                        return false;
                    }
                }
                out.push('}');
            }
            _ => return false, // handles, functions, instances: real JSON.stringify
        }
        true
    }

    /// JSON.parse, engine-side: parsed JSON straight to values — object keys
    /// are data here, never `__ref__`/`__cb__` wire tags.
    fn json_to_value_plain(j: &Json) -> Value {
        match j {
            Json::Null => Value::Null,
            Json::Bool(b) => Value::Bool(*b),
            Json::Num(n) => {
                if n.fract() == 0.0 && n.abs() <= i32::MAX as f64 {
                    Value::I32(*n as i32)
                } else {
                    Value::F64(*n)
                }
            }
            Json::Str(s) => Value::Str(Rc::new(utf16(s))),
            Json::Arr(items) => new_array(items.iter().map(Self::json_to_value_plain).collect()),
            Json::Obj(fields) => new_record(
                fields
                    .iter()
                    .map(|(k, v)| (k.clone(), Self::json_to_value_plain(v)))
                    .collect(),
            ),
        }
    }

    #[allow(clippy::wrong_self_convention)]
    fn to_web(&mut self, v: &Value) -> Json {
        match v {
            Value::Null => Json::Null,
            Value::Bool(b) => Json::Bool(*b),
            Value::I32(n) => Json::Num(*n as f64),
            Value::I64(n) => Json::Num(*n as f64),
            Value::U32(n) => Json::Num(*n as f64),
            Value::U64(n) => Json::Num(*n as f64),
            Value::F32(f) => Json::Num(*f as f64),
            Value::F64(f) => Json::Num(*f),
            Value::Char(c) => Json::Str(c.to_string()),
            Value::Str(s) => Json::Str(utf16_to_string(s)),
            Value::BigIntV(b) => Json::Str(b.to_decimal()),
            Value::BigDecV(d) => Json::Str(d.to_decimal()),
            Value::JsRef(h) => Json::Obj(vec![("__ref__".into(), Json::Num(*h as f64))]),
            // A host-backed instance IS its host object on the wire.
            Value::Instance(i) if i.borrow().host.is_some() => {
                let h = i.borrow().host.expect("checked");
                Json::Obj(vec![("__ref__".into(), Json::Num(h as f64))])
            }
            Value::Dom(id) => Json::Obj(vec![("__dom__".into(), Json::Str(id.to_string()))]),
            Value::Array(a) => {
                let items: Vec<Value> = a.borrow().clone();
                Json::Arr(items.iter().map(|x| self.to_web(x)).collect())
            }
            Value::Record(r) => {
                // Field order is preserved across the bridge.
                let entries: Vec<(String, Value)> = r.borrow().clone();
                Json::Obj(
                    entries
                        .into_iter()
                        .map(|(k, v)| (k, self.to_web(&v)))
                        .collect(),
                )
            }
            // Durable callables cross with a STABLE id (cached by closure
            // identity): the host caches one wrapper per id, so the same
            // Mersey function is the same JS function on every crossing.
            Value::Closure(_) | Value::Native(_) => {
                let id = self.callback_id_for(v);
                Json::Obj(vec![("__cb__".into(), Json::Num(id as f64))])
            }
            // One-shot promise plumbing: a fresh slot per crossing (each
            // carries per-instance settle state; never re-crosses).
            Value::Resolve(..)
            | Value::Reject(..)
            | Value::AllSlot(..)
            | Value::PromiseExec(..) => {
                let id = self.alloc_callback(v.clone());
                Json::Obj(vec![("__cb__".into(), Json::Num(id as f64))])
            }
            // A Mersey promise crosses as a real host promise: construct one
            // whose executor forwards settlement from ours.
            Value::PromiseV(p) => {
                let exec = Value::PromiseExec(p.clone());
                match self.web_new("Promise", vec![exec]) {
                    Ok(Value::JsRef(h)) => Json::Obj(vec![("__ref__".into(), Json::Num(h as f64))]),
                    _ => Json::Null,
                }
            }
            other => Json::Str(to_display(other)),
        }
    }

    /// Tagged JSON → Mersey value.
    #[allow(clippy::wrong_self_convention)]
    fn from_web(&self, j: &Json) -> Value {
        match j {
            Json::Null => Value::Null,
            Json::Bool(b) => Value::Bool(*b),
            Json::Num(n) => {
                if n.fract() == 0.0 && n.abs() <= i32::MAX as f64 {
                    Value::I32(*n as i32)
                } else {
                    Value::F64(*n)
                }
            }
            Json::Str(s) => Value::Str(Rc::new(utf16(s))),
            Json::Arr(items) => new_array(items.iter().map(|i| self.from_web(i)).collect()),
            Json::Obj(fields) => {
                if let Some(Json::Num(h)) = j.get("__ref__") {
                    return Value::JsRef(*h as i64);
                }
                let entries: Vec<(String, Value)> = fields
                    .iter()
                    .map(|(k, v)| (k.clone(), self.from_web(v)))
                    .collect();
                new_record(entries)
            }
        }
    }

    /// Decode a bridge reply (`{"ok":…}` / `{"err":"…"}`).
    fn web_reply(&self, reply: &str) -> VResult {
        let Some(j) = webjson::parse(reply) else {
            return Err(self.throw("Error", format!("bad bridge reply: {reply}")));
        };
        if let Some(Json::Str(msg)) = j.get("err") {
            return Err(self.throw("Error", msg.clone()));
        }
        match j.get("ok") {
            Some(v) => Ok(self.from_web(v)),
            None => Ok(Value::Null),
        }
    }

    /// Take a callback slot, reusing a freed one when possible.
    fn alloc_callback(&mut self, v: Value) -> u32 {
        match self.free_callbacks.pop() {
            Some(id) => {
                self.callbacks[id as usize] = v;
                id
            }
            None => {
                self.callbacks.push(v);
                (self.callbacks.len() - 1) as u32
            }
        }
    }

    /// The stable callback id for a durable callable — cached by identity,
    /// allocated on first crossing. Shared by the JSON path (`{"__cb__":id}`)
    /// and the wide tier (`WebArg::Cb`), so both spell the same callback.
    fn callback_id_for(&mut self, v: &Value) -> u32 {
        let key = Self::callback_key(v).expect("closure/native has identity");
        match self.callback_ids.get(&key) {
            Some(id) => *id,
            None => {
                let id = self.alloc_callback(v.clone());
                self.callback_ids.insert(key, id);
                id
            }
        }
    }

    /// `value_as_webarg`, extended with the durable-callable case (needs
    /// `self` for the cached callback id). Callers gate on
    /// `is_web_scalar_or_cb`.
    fn value_as_webarg_cb<'a>(&mut self, v: &'a Value) -> WebArg<'a> {
        match v {
            Value::Closure(_) | Value::Native(_) => WebArg::Cb(self.callback_id_for(v)),
            other => value_as_webarg(other),
        }
    }

    /// Identity key for a durable callable: the allocation the value shares
    /// across clones. Stable while any clone is alive — and the callback
    /// table's clone keeps it alive for exactly as long as the id is cached.
    fn callback_key(v: &Value) -> Option<usize> {
        match v {
            Value::Closure(rc) => Some(Rc::as_ptr(rc) as usize),
            Value::Native(p) => Some(*p as *const &'static str as usize),
            _ => None,
        }
    }

    /// Release a callback the host will never invoke again (a removed
    /// listener, a settled promise reaction).
    pub fn release_callback(&mut self, id: u32) {
        if let Some(slot) = self.callbacks.get_mut(id as usize) {
            if !matches!(slot, Value::Null) {
                // Evict a cached identity before the slot's clone (the thing
                // keeping the key pointer unique) is dropped.
                if let Some(key) = Self::callback_key(slot) {
                    if self.callback_ids.get(&key) == Some(&id) {
                        self.callback_ids.remove(&key);
                    }
                }
                *slot = Value::Null;
                self.free_callbacks.push(id);
            }
        }
    }

    fn args_json(&mut self, args: &[Value]) -> String {
        let arr = Json::Arr(args.iter().map(|a| self.to_web(a)).collect());
        let mut s = String::new();
        webjson::write(&mut s, &arr);
        s
    }

    /// Intern a member name once; afterwards only the id crosses the ABI.
    fn intern(&mut self, name: &str) -> Option<u32> {
        if let Some(id) = self.interned.get(name) {
            return if *id == u32::MAX { None } else { Some(*id) };
        }
        let id = self.host.web_intern(name);
        self.interned.insert(name.to_string(), id);
        if id == u32::MAX {
            None
        } else {
            Some(id)
        }
    }

    /// Build a Value straight from a typed reply — no JSON parse.
    fn value_from_reply(&self, r: WebReply) -> VResult {
        match r {
            WebReply::Null => Ok(Value::Null),
            WebReply::Num(n) => Ok(if n.fract() == 0.0 && n.abs() <= i32::MAX as f64 {
                Value::I32(n as i32)
            } else {
                Value::F64(n)
            }),
            WebReply::Str(v) => Ok(Value::Str(Rc::new(v))),
            WebReply::Ref(h) => Ok(Value::JsRef(h)),
            WebReply::Bool(b) => Ok(Value::Bool(b)),
            WebReply::Err(msg) => Err(self.throw("Error", msg)),
            // Rare non-scalar result: parse the tagged JSON, as web_reply would.
            WebReply::Json(s) => match webjson::parse(&s) {
                Some(j) => Ok(self.from_web(&j)),
                None => Err(self.throw("Error", format!("bad bridge reply: {s}"))),
            },
        }
    }

    /// Per-op replay for a host whose `web_apply` is NULL: apply the batch's ops
    /// one at a time through the ordinary reflective web bridge (createElement /
    /// textContent / appendChild / insertBefore / removeChild). Identical result
    /// to the batched path — just no crossing-collapse — so `std:dom.apply` keeps
    /// working on a host that declines it. See mersey.h MSY_DOM_* for the encoding.
    fn dom_apply_replay(&mut self, ops: &[i32], nodes: &[i64], strs: &[String]) -> VResult {
        const OP_CREATE: i32 = 0;
        const OP_SET_TEXT: i32 = 1;
        const OP_APPEND: i32 = 2;
        const OP_INSERT: i32 = 3;
        const OP_REMOVE: i32 = 4;
        const NULL_REF: i32 = i32::MIN;
        let str_at = |i: i32| -> Value {
            Value::Str(Rc::new(utf16(
                strs.get(i as usize).map_or("", |s| s.as_str()),
            )))
        };
        // A node operand -> its live handle: a temp id (>= 0, into the nodes this
        // batch created), a live node (< 0 -> nodes[-r-1]), or None for MSY_DOM_NULL.
        let handle_of = |r: i32, created: &[i64]| -> Option<i64> {
            if r == NULL_REF {
                return None;
            }
            if r >= 0 {
                return created.get(r as usize).copied();
            }
            nodes.get((-r - 1) as usize).copied()
        };
        let mut created: Vec<i64> = Vec::new();
        for group in ops.chunks_exact(4) {
            let (op, a, b, c) = (group[0], group[1], group[2], group[3]);
            match op {
                OP_CREATE => {
                    // `c` names the document (a live node).
                    let h = match handle_of(c, &created) {
                        Some(doc) => match self.web_call(doc, "createElement", vec![str_at(a)])? {
                            Value::JsRef(h) => h,
                            _ => -1,
                        },
                        None => -1,
                    };
                    created.push(h);
                }
                OP_SET_TEXT => {
                    if let Some(t) = handle_of(a, &created) {
                        self.web_set(t, "textContent", str_at(b))?;
                    }
                }
                OP_APPEND => {
                    if let (Some(p), Some(ch)) = (handle_of(a, &created), handle_of(b, &created)) {
                        self.web_call(p, "appendChild", vec![Value::JsRef(ch)])?;
                    }
                }
                OP_INSERT => {
                    if let (Some(p), Some(ch)) = (handle_of(a, &created), handle_of(b, &created)) {
                        let refv = handle_of(c, &created).map_or(Value::Null, Value::JsRef);
                        self.web_call(p, "insertBefore", vec![Value::JsRef(ch), refv])?;
                    }
                }
                OP_REMOVE => {
                    if let (Some(p), Some(ch)) = (handle_of(a, &created), handle_of(b, &created)) {
                        self.web_call(p, "removeChild", vec![Value::JsRef(ch)])?;
                    }
                }
                _ => {}
            }
        }
        Ok(new_array(created.into_iter().map(Value::JsRef).collect()))
    }

    fn web_get(&mut self, target: i64, prop: &str) -> VResult {
        if let Some(id) = self.intern(prop) {
            // Wide-string fast path: UTF-16 reply, no UTF-8, no JSON.
            if let Some(reply) = self.host.web_get_u16(target, id) {
                return self.value_from_reply(reply);
            }
            let reply = self.host.web_get_id(target, id);
            return self.web_reply(&reply);
        }
        let reply = self.host.web_get(target, prop);
        self.web_reply(&reply)
    }

    fn web_set(&mut self, target: i64, prop: &str, v: Value) -> Result<(), Thrown> {
        // Fast paths: a scalar value needs no JSON at all.
        if let Some(id) = self.intern(prop) {
            // Wide-string fast path first (a string value crosses as UTF-32).
            if is_web_scalar(&v) {
                if let Some(reply) = self.host.web_set_u16(target, id, &value_as_webarg(&v)) {
                    return self.value_from_reply(reply).map(|_| ());
                }
            }
            let reply = match &v {
                Value::Str(s) => {
                    let text: String = utf16_to_string(s);
                    Some(self.host.web_set_str(target, id, &text))
                }
                Value::I32(n) => Some(self.host.web_set_num(target, id, *n as f64)),
                Value::F64(f) => Some(self.host.web_set_num(target, id, *f)),
                Value::I64(n) => Some(self.host.web_set_num(target, id, *n as f64)),
                _ => None,
            };
            if let Some(reply) = reply {
                return self.web_reply(&reply).map(|_| ());
            }
        }
        let j = self.to_web(&v);
        let mut s = String::new();
        webjson::write(&mut s, &j);
        let reply = self.host.web_set(target, prop, &s);
        self.web_reply(&reply).map(|_| ())
    }

    fn web_call(&mut self, target: i64, method: &str, args: Vec<Value>) -> VResult {
        // JSON.stringify / JSON.parse on pure data never need the host at all:
        // the engine has its own writer and parser, and going to the page would
        // serialize the value twice (once to ship it, once for the result).
        if target == self.json_handle && self.json_handle >= 0 && args.len() == 1 {
            if method == "stringify" {
                let mut out = String::new();
                if Self::pure_json(&args[0], &mut out) {
                    return Ok(Value::Str(Rc::new(utf16(&out))));
                }
                // Not pure data (a handle, a function): the real JSON.stringify.
            } else if method == "parse" {
                if let Value::Str(s) = &args[0] {
                    return match webjson::parse(&utf16_to_string(s)) {
                        Some(j) => Ok(Self::json_to_value_plain(&j)),
                        None => Err(self.throw("SyntaxError", "invalid JSON")),
                    };
                }
            }
        }
        // Wide fast path: scalar args cross as UTF-16 (zero-copy strings) and
        // durable callables as their stable callback id — no JSON either way.
        // Unlike the tiers below it also carries `method == ""` (calling the
        // handle itself: an imported `setTimeout(cb, ms)`), which crosses as
        // the interned empty name; the host calls the handle (ABI v8).
        if !args.is_empty() && args.iter().all(is_web_scalar_or_cb) {
            if let Some(id) = self.intern(method) {
                // The arguments cross on the stack, not through a fresh heap
                // `Vec` per call — this is the hottest web path (getItem,
                // fillRect, …), run tens of thousands of times a frame.
                let reply = if args.len() <= 8 {
                    let mut buf: [WebArg; 8] = std::array::from_fn(|_| WebArg::Null);
                    for (k, a) in args.iter().enumerate() {
                        buf[k] = self.value_as_webarg_cb(a);
                    }
                    self.host.web_call_u16(target, id, &buf[..args.len()])
                } else {
                    let mut wargs = Vec::with_capacity(args.len());
                    for a in args.iter() {
                        wargs.push(self.value_as_webarg_cb(a));
                    }
                    self.host.web_call_u16(target, id, &wargs)
                };
                if let Some(reply) = reply {
                    return self.value_from_reply(reply);
                }
            }
        }
        // Interned single/multi-scalar tiers (UTF-8): named methods only.
        if !method.is_empty() && !args.is_empty() {
            if let Some(scalars) = as_scalars(&args) {
                if let Some(id) = self.intern(method) {
                    let refs: Vec<WebScalar> = scalars.iter().map(scalar_ref).collect();
                    let reply = self.host.web_call_scalars(target, id, &refs);
                    if !reply.is_empty() {
                        return self.web_reply(&reply);
                    }
                    // Host declined multi-scalar; try the single-string op it may
                    // still support (createElement, getItem, …).
                    if let [OwnedScalar::Str(s)] = scalars.as_slice() {
                        let reply = self.host.web_call_str(target, id, s);
                        return self.web_reply(&reply);
                    }
                }
            }
        }
        let a = self.args_json(&args);
        let reply = self.host.web_call(target, method, &a);
        self.web_reply(&reply)
    }

    /// Release a host handle explicitly (`release(el)`): long-lived pages
    /// that churn through DOM objects can hand them back. Handles are not
    /// GC-tracked yet — this is the documented escape hatch.
    pub(crate) fn web_release_value(&mut self, v: &Value) {
        if let Value::JsRef(h) = v {
            if *h != 0 {
                self.host.web_release(*h);
            }
        }
    }

    /// Elements of a host iterable, as Mersey values.
    pub(crate) fn web_iterate(&mut self, target: i64) -> Result<Vec<Value>, Thrown> {
        let reply = self.host.web_iterate(target);
        match self.web_reply(&reply)? {
            Value::Array(a) => Ok(a.borrow().clone()),
            other => Err(self.throw(
                "TypeError",
                format!("`{}` is not iterable", kind_of(&other)),
            )),
        }
    }

    fn web_new(&mut self, ctor: &str, args: Vec<Value>) -> VResult {
        if !args.is_empty() && args.iter().all(is_web_scalar) {
            if let Some(id) = self.intern(ctor) {
                // Wide-string fast path: UTF-32 args, typed reply (`new URL(s)`).
                let wargs: Vec<WebArg> = args.iter().map(value_as_webarg).collect();
                if let Some(reply) = self.host.web_new_u16(id, &wargs) {
                    return self.value_from_reply(reply);
                }
                // UTF-8 scalar fallback.
                let owned = as_scalars(&args).unwrap_or_default();
                let refs: Vec<WebScalar> = owned.iter().map(scalar_ref).collect();
                let reply = self.host.web_new_scalars(id, &refs);
                if !reply.is_empty() {
                    return self.web_reply(&reply);
                }
            }
        }
        let a = self.args_json(&args);
        let reply = self.host.web_new(ctor, &a);
        self.web_reply(&reply)
    }

    /// Fire a callback with host-supplied arguments (event objects etc.).
    pub fn invoke_callback_json(&mut self, id: u32, args_json: &str) -> Result<(), Thrown> {
        let args = match webjson::parse(args_json) {
            Some(Json::Arr(items)) => items.iter().map(|i| self.from_web(i)).collect(),
            _ => Vec::new(),
        };
        let cb = match self.callbacks.get(id as usize) {
            Some(v) => v.clone(),
            None => return self.type_error(format!("unknown callback #{id}")),
        };
        self.call_value(&cb, args)?;
        self.drain_microtasks()?;
        self.maybe_collect();
        Ok(())
    }

    /// Stable merge sort driven by a Mersey comparator (which may throw).
    fn merge_sort(&mut self, items: Vec<Value>, cmp: &Value) -> Result<Vec<Value>, Thrown> {
        if items.len() <= 1 {
            return Ok(items);
        }
        let mid = items.len() / 2;
        let right = self.merge_sort(items[mid..].to_vec(), cmp)?;
        let left = self.merge_sort(items[..mid].to_vec(), cmp)?;
        let mut out = Vec::with_capacity(left.len() + right.len());
        let (mut i, mut j) = (0, 0);
        while i < left.len() && j < right.len() {
            let ord = self.call_value(cmp, vec![left[i].clone(), right[j].clone()])?;
            let ord = as_i64(&ord).unwrap_or(0);
            if ord <= 0 {
                out.push(left[i].clone());
                i += 1;
            } else {
                out.push(right[j].clone());
                j += 1;
            }
        }
        out.extend_from_slice(&left[i..]);
        out.extend_from_slice(&right[j..]);
        Ok(out)
    }

    fn current_class(&self) -> Result<Rc<ClassDef>, Thrown> {
        self.class_stack
            .last()
            .cloned()
            .ok_or_else(|| self.throw("TypeError", "`super` outside a class"))
    }

    fn super_lookup(&mut self, name: &str, this: Option<Value>) -> VResult {
        let this = this.ok_or_else(|| self.throw("TypeError", "`super` needs `this`"))?;
        let cls = self.current_class()?;
        let parent = cls
            .parent
            .clone()
            .ok_or_else(|| self.throw("TypeError", "class has no base class"))?;
        if let Some((m, defining)) = find_in_chain(&parent, |c| {
            c.methods.get(name).map(|m| (m.clone(), c.clone()))
        }) {
            let env2 = defining.env.clone().unwrap_or_else(|| self.globals.clone());
            return Ok(Value::Closure(Rc::new(Closure {
                data: m,
                env: env2,
                this: Some(this),
                cls: Some(defining),
            })));
        }
        self.type_error(format!("no method `{name}` on the base class"))
    }

    /// The Mersey class `head` names, if it names one. `None` for a host
    /// constructor, a builtin (`Map`, `Set`), or a namespace path — none of which
    /// are classes and none of which can be cached as one.
    pub(crate) fn resolve_class(&mut self, head: &str, env: &Env) -> Option<Rc<ClassDef>> {
        if head.contains('.') {
            return None;
        }
        if (head == "Map" || head == "Set") && env_get(env, head).is_none() {
            return None;
        }
        match env_get(env, head) {
            Some(Value::Class(cls)) => Some(cls),
            _ => None,
        }
    }

    fn new_named(&mut self, head: &str, argv: Vec<Value>, env: &Env) -> VResult {
        // `new geo.Point(…)` — resolve through a namespace import.
        if let Some((ns, member)) = head.split_once('.') {
            if let Some(Value::Namespace(entries)) = env_get(env, ns) {
                return match entries.entries.get(member) {
                    Some(Value::Class(cls)) => {
                        let cls = cls.clone();
                        self.instantiate(&cls, argv)
                    }
                    _ => self.type_error(format!("`{head}` is not a class")),
                };
            }
        }
        if head == "Map" && env_get(env, "Map").is_none() {
            return Ok(new_map(Vec::new()));
        }
        if head == "Set" && env_get(env, "Set").is_none() {
            return Ok(new_set(Vec::new()));
        }
        let bare = head.split('.').next().unwrap_or(head);
        match env_get(env, bare) {
            Some(Value::Class(cls)) if !head.contains('.') => self.instantiate(&cls, argv),
            // `new WebSocket(url)`, `new Uint8Array(n)`, `new Intl.NumberFormat(…)`:
            // any host constructor reachable through the bridge. The *whole* name
            // goes to the host — a namespaced constructor lives at a path, and
            // truncating it to its first segment would ask for the namespace
            // itself, which is not a constructor.
            _ => self.web_new(head, argv),
        }
    }

    /// Drain a generator into a vector (used by `for … of` in the VM).
    pub(crate) fn drain_iter(&mut self, v: &Value) -> Result<Vec<Value>, Thrown> {
        let Value::IterV(g) = v else {
            return self.type_error("not an iterator");
        };
        let g = g.clone();
        let mut out = Vec::new();
        loop {
            match self.gen_next(g.clone())? {
                Value::Null => break,
                item => out.push(item),
            }
        }
        Ok(out)
    }

    /// Resume a generator to its next `yield` (or to completion).
    fn iter_next_adapted(&mut self, g: &Rc<GcCell<GenState>>, a: Adapter) -> VResult {
        // `null` is the end of the sequence, the same signal a plain generator
        // uses.
        match a {
            Adapter::Map(inner, f) => {
                let v = self.gen_next(inner)?;
                if matches!(v, Value::Null) {
                    g.borrow_mut().done = true;
                    return Ok(Value::Null);
                }
                self.call_value(&f, vec![v])
            }
            Adapter::Filter(inner, f) => loop {
                let v = self.gen_next(inner.clone())?;
                if matches!(v, Value::Null) {
                    g.borrow_mut().done = true;
                    return Ok(Value::Null);
                }
                let keep = self.call_value(&f, vec![v.clone()])?;
                if self.value_truthy(&keep)? {
                    return Ok(v);
                }
            },
            Adapter::Take(inner, left) => {
                if left.get() <= 0 {
                    g.borrow_mut().done = true;
                    return Ok(Value::Null);
                }
                let v = self.gen_next(inner)?;
                if matches!(v, Value::Null) {
                    g.borrow_mut().done = true;
                    return Ok(Value::Null);
                }
                left.set(left.get() - 1);
                Ok(v)
            }
        }
    }

    /// `it.map(f)` / `it.filter(f)` / `it.take(n)`: a new iterator that pulls
    /// from this one. Nothing is evaluated until someone calls `next()`.
    fn iter_adapt(&mut self, g: Rc<GcCell<GenState>>, name: &str, args: Vec<Value>) -> VResult {
        let adapter = match name {
            "map" => Adapter::Map(g, args.into_iter().next().unwrap_or(Value::Null)),
            "filter" => Adapter::Filter(g, args.into_iter().next().unwrap_or(Value::Null)),
            _ => {
                let n = args.first().and_then(as_i64).unwrap_or(0).max(0);
                Adapter::Take(g, Rc::new(std::cell::Cell::new(n)))
            }
        };
        let out = Rc::new(GcCell::new(GenState {
            coro: None,
            done: false,
            is_async: false,
            pending: None,
            adapter: Some(adapter),
        }));
        gc::track_gen(&out);
        Ok(Value::IterV(out))
    }

    /// `next()` on an async generator: a promise that settles at the next
    /// `yield` (with the value), at the end (with `null`), or with whatever the
    /// body threw.
    fn gen_next_async(&mut self, g: Rc<GcCell<GenState>>) -> VResult {
        let promise = PromiseState::pending();
        if g.borrow().done {
            self.settle(&promise, Value::Null, false);
            return Ok(Value::PromiseV(promise));
        }
        let Some(mut coro) = g.borrow_mut().coro.take() else {
            g.borrow_mut().done = true;
            self.settle(&promise, Value::Null, false);
            return Ok(Value::PromiseV(promise));
        };
        g.borrow_mut().pending = Some(promise.clone());
        coro.gen = Some(g.clone());
        self.drive_gen(g, coro, None)?;
        Ok(Value::PromiseV(promise))
    }

    /// Drive an async generator's coroutine until it yields, finishes, or
    /// suspends on an `await`.
    fn drive_gen(
        &mut self,
        g: Rc<GcCell<GenState>>,
        mut coro: Coro,
        resumed: Option<(Value, bool)>,
    ) -> Result<(), Thrown> {
        let pushed = coro.cls.clone();
        if let Some(cls) = &pushed {
            self.class_stack.push(cls.clone());
        }
        let outcome = vm::run_coro(self, &mut coro, resumed);
        if pushed.is_some() {
            self.class_stack.pop();
        }
        let pending = g.borrow_mut().pending.take();
        match outcome {
            Ok(vm::Flow::Yield(v)) => {
                // Suspended at a `yield`: keep the coroutine for the next call
                // and hand the value to whoever is awaiting `next()`.
                g.borrow_mut().coro = Some(coro);
                if let Some(p) = pending {
                    self.settle(&p, v, false);
                }
                Ok(())
            }
            Ok(vm::Flow::Done(_)) => {
                g.borrow_mut().discard();
                if let Some(p) = pending {
                    self.settle(&p, Value::Null, false); // exhausted
                }
                Ok(())
            }
            Ok(vm::Flow::Await(awaited)) => {
                // The body awaited something. This `next()` has not settled yet:
                // put its promise back, and resume when the awaited thing does.
                g.borrow_mut().pending = pending;
                let p = self.as_promise(awaited)?;
                let status = p.borrow().status.clone();
                match status {
                    PromiseStatus::Pending => {
                        p.borrow_mut().waiters.push(coro);
                    }
                    PromiseStatus::Fulfilled | PromiseStatus::Rejected => {
                        let v = p.borrow().value.clone();
                        let rejected = status == PromiseStatus::Rejected;
                        self.tasks.push_back(Task::Resume(coro, v, rejected));
                    }
                }
                Ok(())
            }
            Err(t) => {
                g.borrow_mut().discard();
                if let Some(p) = pending {
                    self.settle(&p, t.0, true);
                }
                Ok(())
            }
        }
    }

    /// How a value looks when a *program* shows it — `console.log`, a template
    /// literal, `join`.
    ///
    /// A class that implements `Display` has its `toString()` called. This is
    /// what JavaScript reaches `Symbol.toPrimitive` for; here it is an ordinary
    /// method named by an interface the checker can see, so forgetting it is a
    /// compile error rather than a `<Money>` in your output.
    ///
    /// Containers recurse, so an array of `Display` values shows them properly.
    pub(crate) fn display(&mut self, v: &Value) -> Result<String, Thrown> {
        match v {
            Value::Instance(inst) => {
                let has = find_in_chain(&inst.borrow().class, |c| {
                    c.methods.get("toString").map(|_| ())
                })
                .is_some();
                if !has {
                    return Ok(to_display(v));
                }
                let out = self.call_member(v, "toString", Vec::new())?;
                match out {
                    Value::Str(s) => Ok(utf16_to_string(&s)),
                    other => Ok(to_display(&other)),
                }
            }
            Value::SetV(sv) => {
                let items: Vec<Value> = sv.borrow().iter().map(|k| k.0.clone()).collect();
                let mut parts = Vec::with_capacity(items.len());
                for it in &items {
                    parts.push(self.display(it)?);
                }
                // `[…]`, as before: this arm used to be shared with Array.
                // (`to_display` spells a Set `Set{…}` — they already disagreed,
                // and reconciling them is a separate decision.)
                Ok(format!("[{}]", parts.join(", ")))
            }
            Value::Array(a) => {
                let items = a.borrow().clone();
                let mut parts = Vec::with_capacity(items.len());
                for it in &items {
                    parts.push(self.display(it)?);
                }
                Ok(format!("[{}]", parts.join(", ")))
            }
            Value::Record(r) => {
                let items = r.borrow().clone();
                let mut parts = Vec::with_capacity(items.len());
                for (k, val) in &items {
                    parts.push(format!("{k}: {}", self.display(val)?));
                }
                Ok(format!("{{{}}}", parts.join(", ")))
            }
            other => Ok(to_display(other)),
        }
    }

    /// The async iterator behind a `for await`: an `AsyncIter` is already one; a
    /// class implementing `AsyncIterable<T>` hands one over from `iter()`.
    pub(crate) fn async_iter_of(&mut self, v: &Value) -> VResult {
        match v {
            Value::IterV(_) => Ok(v.clone()),
            Value::Instance(inst) => {
                let has_iter =
                    find_in_chain(&inst.borrow().class, |c| c.methods.get("iter").map(|_| ()))
                        .is_some();
                if !has_iter {
                    return self.type_error(
                        "`for await` needs an `AsyncIter<T>` or a class implementing                          `AsyncIterable<T>`",
                    );
                }
                self.call_member(v, "iter", Vec::new())
            }
            // A host object may be async-iterable on its own terms.
            _ => Ok(v.clone()),
        }
    }

    /// The values a `for … of` will walk: an array, a string, a host iterable, a
    /// generator — or a class that implements `Iterable<T>`, whose `iter()` gives
    /// back the iterator to drain.
    ///
    /// This is what JavaScript reaches `Symbol.iterator` for. Here it is an
    /// ordinary method, named by an interface the checker can see.
    pub(crate) fn iter_values(&mut self, v: &Value) -> Result<Vec<Value>, Thrown> {
        match v {
            Value::Array(a) => Ok(a.borrow().clone()),
            Value::Str(s) => Ok(char::decode_utf16(s.iter().copied()).map(|r| Value::Char(r.unwrap_or('\u{FFFD}'))).collect()),
            Value::JsRef(h) => {
                let h = *h;
                self.web_iterate(h)
            }
            Value::IterV(_) => self.drain_iter(v),
            Value::Instance(inst) => {
                let has_iter = find_in_chain(&inst.borrow().class, |c| {
                    c.methods.get("iter").map(|_| ())
                })
                .is_some();
                if !has_iter {
                    return self.type_error(
                        "`for of` needs an array, string, an iterator, or a class that implements                          `Iterable<T>`",
                    );
                }
                let it = self.call_member(v, "iter", Vec::new())?;
                self.drain_iter(&it)
            }
            _ => self.type_error(
                "`for of` needs an array, string, an iterator, or a class that implements                  `Iterable<T>`",
            ),
        }
    }

    fn gen_next(&mut self, g: Rc<GcCell<GenState>>) -> VResult {
        if g.borrow().is_async {
            return self.gen_next_async(g);
        }
        if g.borrow().done {
            return Ok(Value::Null);
        }
        // A derived iterator has no coroutine: it pulls one element from the
        // one below it, and only as many as it is asked for.
        let adapter = g.borrow().adapter.clone();
        if let Some(a) = adapter {
            return self.iter_next_adapted(&g, a);
        }
        let Some(mut coro) = g.borrow_mut().coro.take() else {
            g.borrow_mut().done = true;
            return Ok(Value::Null);
        };
        let pushed = coro.cls.clone();
        if let Some(cls) = &pushed {
            self.class_stack.push(cls.clone());
        }
        let outcome = vm::run_coro(self, &mut coro, None);
        if pushed.is_some() {
            self.class_stack.pop();
        }
        match outcome {
            Ok(vm::Flow::Yield(v)) => {
                // Suspended: keep the coroutine for the next call.
                g.borrow_mut().coro = Some(coro);
                Ok(v)
            }
            Ok(vm::Flow::Done(_)) => {
                g.borrow_mut().done = true;
                Ok(Value::Null) // exhausted
            }
            Ok(vm::Flow::Await(_)) => {
                g.borrow_mut().done = true;
                self.type_error("`await` inside a generator is not supported")
            }
            Err(t) => {
                g.borrow_mut().done = true;
                Err(t)
            }
        }
    }

    pub(crate) fn super_call(&mut self, argv: Vec<Value>, this: Option<Value>) -> VResult {
        let this = this.ok_or_else(|| self.throw("TypeError", "`super` needs `this`"))?;
        let cls = self.current_class()?;
        let parent = cls
            .parent
            .clone()
            .ok_or_else(|| self.throw("TypeError", "class has no base class"))?;
        let mut search = Some(parent);
        while let Some(c) = search {
            // The builtin errors are constructed by the engine, not by a
            // Mersey constructor — so `super(msg)` in `class X extends Error`
            // would otherwise walk past them and quietly drop the message.
            if c.is_builtin_error {
                if let Value::Instance(inst) = &this {
                    let msg = argv.into_iter().next().unwrap_or(Value::Null);
                    let stack = Value::Str(Rc::new(utf16(&(self.stack_trace()))));
                    let mut i = inst.borrow_mut();
                    if let Some(slot) = i.class.slot_of("message") {
                        i.slots[slot as usize] = msg;
                    }
                    if let Some(slot) = i.class.slot_of("stack") {
                        i.slots[slot as usize] = stack;
                    }
                }
                return Ok(Value::Null);
            }
            if let Some(ctor) = &c.ctor {
                let env2 = c.env.clone().unwrap_or_else(|| self.globals.clone());
                let closure = Closure {
                    data: ctor.clone(),
                    env: env2,
                    this: Some(this.clone()),
                    cls: Some(c.clone()),
                };
                return self.call_closure(&closure, argv);
            }
            search = c.parent.clone();
        }
        Ok(Value::Null) // no ctor anywhere up the chain: nothing to do
    }

    pub(crate) fn call_super_method(
        &mut self,
        name: &str,
        args: Vec<Value>,
        this: Option<Value>,
    ) -> VResult {
        let f = self.super_lookup(name, this)?;
        self.call_value(&f, args)
    }

    fn assign_to(&mut self, target: &'static Expr, value: Value, env: &Env) -> Result<(), Thrown> {
        match target {
            Expr::Ident(n) => {
                if env_set(env, &n.text, value) {
                    Ok(())
                } else {
                    self.type_error(format!("`{}` is not defined", n.text))
                }
            }
            Expr::Member { obj, name, .. } => {
                let o = self.eval(obj, env)?;
                self.set_member(&o, name, value)
            }
            Expr::Index { obj, index, .. } => {
                let o = self.eval(obj, env)?;
                let i = self.eval(index, env)?;
                self.index_set(&o, &i, value)
            }
            _ => self.type_error("invalid assignment target"),
        }
    }

    // ---- literals, numerics, casts ------------------------------------------------

    fn eval_literal(&self, kind: LitKind, text: &str) -> VResult {
        parse_literal(kind, text).map_err(|(class, msg)| self.throw(class, msg))
    }

    fn eval_unary(&mut self, op: UnaryOp, v: Value) -> VResult {
        match op {
            UnaryOp::Not => Ok(Value::Bool(!self.value_truthy(&v)?)),
            UnaryOp::Plus => match v {
                Value::I32(_)
                | Value::I64(_)
                | Value::U32(_)
                | Value::U64(_)
                | Value::F32(_)
                | Value::F64(_) => Ok(v),
                _ => self.type_error("unary `+` needs a number"),
            },
            UnaryOp::Neg => match v {
                Value::BigIntV(b) => Ok(Value::BigIntV(Rc::new(b.negate()))),
                Value::BigDecV(d) => Ok(Value::BigDecV(Rc::new(BigDec {
                    coef: d.coef.negate(),
                    scale: d.scale,
                }))),
                Value::I32(n) => Ok(Value::I32(n.wrapping_neg())),
                Value::I64(n) => Ok(Value::I64(n.wrapping_neg())),
                Value::U32(n) => Ok(Value::U32(n.wrapping_neg())),
                Value::U64(n) => Ok(Value::U64(n.wrapping_neg())),
                Value::F32(f) => Ok(Value::F32(-f)),
                Value::F64(f) => Ok(Value::F64(-f)),
                _ => self.type_error("unary `-` needs a number"),
            },
            UnaryOp::BitNot => match v {
                Value::I32(n) => Ok(Value::I32(!n)),
                Value::I64(n) => Ok(Value::I64(!n)),
                Value::U32(n) => Ok(Value::U32(!n)),
                Value::U64(n) => Ok(Value::U64(!n)),
                _ => self.type_error("`~` needs an integer"),
            },
            UnaryOp::Await => self.type_error("`await` is not in the MVP"),
        }
    }

    fn values_equal(&self, a: &Value, b: &Value) -> Result<bool, Thrown> {
        Ok(match (a, b) {
            (Value::Null, Value::Null) => true,
            (Value::Null, _) | (_, Value::Null) => false,
            (Value::Bool(x), Value::Bool(y)) => x == y,
            (Value::Char(x), Value::Char(y)) => x == y,
            (Value::Str(x), Value::Str(y)) => x == y,
            (Value::BigIntV(x), Value::BigIntV(y)) => x.cmp(y) == std::cmp::Ordering::Equal,
            (Value::BigDecV(x), Value::BigDecV(y)) => x.cmp(y) == std::cmp::Ordering::Equal,
            // Host objects compare by identity: the bridge's handle table
            // dedups by object, so equal handles are the same object.
            (Value::JsRef(x), Value::JsRef(y)) => x == y,
            (Value::Bytes(x), Value::Bytes(y)) => Rc::ptr_eq(x, y),
            (Value::MapV(x), Value::MapV(y)) => Rc::ptr_eq(x, y),
            (Value::SetV(x), Value::SetV(y)) => Rc::ptr_eq(x, y),
            (Value::Array(x), Value::Array(y)) => Rc::ptr_eq(x, y),
            (Value::Record(x), Value::Record(y)) => Rc::ptr_eq(x, y),
            (Value::Instance(x), Value::Instance(y)) => Rc::ptr_eq(x, y),
            (Value::Closure(x), Value::Closure(y)) => Rc::ptr_eq(x, y),
            _ => {
                if let (Some(_), Some(_)) = (as_num(a), as_num(b)) {
                    let (x, y) = promote_pair(a, b)
                        .ok_or_else(|| self.throw("TypeError", "cannot compare these values"))?;
                    return Ok(num_eq(&x, &y));
                }
                return Err(self.throw(
                    "TypeError",
                    format!(
                        "`==` between {} and {} (no coercion, §3.3)",
                        kind_of(a),
                        kind_of(b)
                    ),
                ));
            }
        })
    }

    fn numeric_binop(&mut self, op: BinOp, l: Value, r: Value) -> VResult {
        // String / char concatenation and comparisons first.
        match (&l, &r, op) {
            (Value::Str(a), Value::Str(b), BinOp::Add) => {
                let mut s: Vec<u16> = a.as_ref().clone();
                s.extend(b.iter());
                return Ok(Value::Str(Rc::new(s)));
            }
            (Value::Str(a), Value::Str(b), BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge) => {
                let c = a.cmp(b);
                return Ok(Value::Bool(match op {
                    BinOp::Lt => c.is_lt(),
                    BinOp::Gt => c.is_gt(),
                    BinOp::Le => c.is_le(),
                    _ => c.is_ge(),
                }));
            }
            (Value::Char(a), Value::Char(b), BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge) => {
                let c = a.cmp(b);
                return Ok(Value::Bool(match op {
                    BinOp::Lt => c.is_lt(),
                    BinOp::Gt => c.is_gt(),
                    BinOp::Le => c.is_le(),
                    _ => c.is_ge(),
                }));
            }
            _ => {}
        }
        match (&l, &r) {
            (Value::BigIntV(a), Value::BigIntV(b)) => return self.bigint_op(op, a, b),
            (Value::BigDecV(a), Value::BigDecV(b)) => return self.bigdec_op(op, a, b),
            _ => {}
        }
        let (a, b) = promote_pair(&l, &r).ok_or_else(|| {
            self.throw(
                "TypeError",
                format!(
                    "`{}` needs numeric operands, got {} and {}",
                    op.as_str(),
                    kind_of(&l),
                    kind_of(&r)
                ),
            )
        })?;
        self.promoted_binop(op, a, b)
    }

    fn promoted_binop(&mut self, op: BinOp, a: Value, b: Value) -> VResult {
        use BinOp::*;
        macro_rules! int_ops {
            ($x:expr, $y:expr, $wrap:ident, $t:ty, $mk:expr) => {{
                let (x, y) = ($x, $y);
                match op {
                    Add => $mk(x.wrapping_add(y)),
                    Sub => $mk(x.wrapping_sub(y)),
                    Mul => $mk(x.wrapping_mul(y)),
                    Div => {
                        if y == 0 {
                            return Err(self.throw("RangeError", "division by zero"));
                        }
                        match x.checked_div(y) {
                            Some(q) => $mk(q),
                            None => {
                                return Err(self.throw("RangeError", "integer overflow in division"))
                            }
                        }
                    }
                    Rem => {
                        if y == 0 {
                            return Err(self.throw("RangeError", "division by zero"));
                        }
                        match x.checked_rem(y) {
                            Some(q) => $mk(q),
                            None => $mk(0 as $t),
                        }
                    }
                    Pow => {
                        let mut acc: $t = 1 as $t;
                        let mut i = 0i64;
                        let n = y as i64;
                        if n < 0 {
                            return Err(self.throw("RangeError", "negative integer exponent"));
                        }
                        while i < n {
                            acc = acc.wrapping_mul(x);
                            i += 1;
                        }
                        $mk(acc)
                    }
                    Shl => $mk(x.wrapping_shl(y as u32)),
                    Shr => $mk(x.wrapping_shr(y as u32)),
                    BitAnd => $mk(x & y),
                    BitOr => $mk(x | y),
                    BitXor => $mk(x ^ y),
                    Lt => Value::Bool(x < y),
                    Gt => Value::Bool(x > y),
                    Le => Value::Bool(x <= y),
                    Ge => Value::Bool(x >= y),
                    _ => return self.type_error("bad operator"),
                }
            }};
        }
        macro_rules! float_ops {
            ($x:expr, $y:expr, $mk:expr) => {{
                let (x, y) = ($x, $y);
                match op {
                    Add => $mk(x + y),
                    Sub => $mk(x - y),
                    Mul => $mk(x * y),
                    Div => $mk(x / y),
                    Rem => $mk(x % y),
                    Pow => $mk(x.powf(y)),
                    Lt => Value::Bool(x < y),
                    Gt => Value::Bool(x > y),
                    Le => Value::Bool(x <= y),
                    Ge => Value::Bool(x >= y),
                    _ => return self.type_error("floats do not support this operator"),
                }
            }};
        }
        Ok(match (a, b) {
            (Value::I32(x), Value::I32(y)) => int_ops!(x, y, wrapping, i32, Value::I32),
            (Value::U32(x), Value::U32(y)) => int_ops!(x, y, wrapping, u32, Value::U32),
            (Value::I64(x), Value::I64(y)) => int_ops!(x, y, wrapping, i64, Value::I64),
            (Value::U64(x), Value::U64(y)) => int_ops!(x, y, wrapping, u64, Value::U64),
            (Value::F32(x), Value::F32(y)) => float_ops!(x, y, Value::F32),
            (Value::F64(x), Value::F64(y)) => float_ops!(x, y, Value::F64),
            _ => return self.type_error("operands did not promote to a common type"),
        })
    }

    fn bigint_op(&mut self, op: BinOp, a: &BigInt, b: &BigInt) -> VResult {
        use std::cmp::Ordering as O;
        use BinOp::*;
        Ok(match op {
            Add => Value::BigIntV(Rc::new(a.add(b))),
            Sub => Value::BigIntV(Rc::new(a.sub(b))),
            Mul => Value::BigIntV(Rc::new(a.mul(b))),
            Div | Rem => {
                let (q, r) = a
                    .divmod(b)
                    .ok_or_else(|| self.throw("RangeError", "division by zero"))?;
                Value::BigIntV(Rc::new(if op == Div { q } else { r }))
            }
            Lt => Value::Bool(a.cmp(b) == O::Less),
            Gt => Value::Bool(a.cmp(b) == O::Greater),
            Le => Value::Bool(a.cmp(b) != O::Greater),
            Ge => Value::Bool(a.cmp(b) != O::Less),
            _ => return self.type_error("operator not defined for bigint"),
        })
    }

    fn bigdec_op(&mut self, op: BinOp, a: &BigDec, b: &BigDec) -> VResult {
        use std::cmp::Ordering as O;
        use BinOp::*;
        Ok(match op {
            Add => Value::BigDecV(Rc::new(a.add(b))),
            Sub => Value::BigDecV(Rc::new(a.sub(b))),
            Mul => Value::BigDecV(Rc::new(a.mul(b))),
            Div => match a.div_exact(b) {
                Some(q) => Value::BigDecV(Rc::new(q)),
                None => {
                    return Err(self.throw(
                        "RangeError",
                        "inexact bigdec division needs a rounding context (§3.7)",
                    ))
                }
            },
            Lt => Value::Bool(a.cmp(b) == O::Less),
            Gt => Value::Bool(a.cmp(b) == O::Greater),
            Le => Value::Bool(a.cmp(b) != O::Greater),
            Ge => Value::Bool(a.cmp(b) != O::Less),
            _ => return self.type_error("operator not defined for bigdec"),
        })
    }

    /// `x is T` — does this value hold a `T`?
    ///
    /// The same question the checked cast asks, answered instead of thrown. It
    /// is a *value* test, not type reflection: nothing here hands a type back to
    /// the program to compute with (§1.2).
    pub(crate) fn value_is(&self, v: &Value, ty: &TypeExpr) -> bool {
        let TypeExpr::ArrayOf(inner) = ty else {
            let TypeExpr::Named { name, .. } = ty else {
                return false;
            };
            return self.value_is_named(v, name);
        };
        // `xs is int32[]`: every element has to hold, or the answer is a lie the
        // first time someone reads one.
        match v {
            Value::Array(a) => a.borrow().iter().all(|e| self.value_is(e, inner)),
            _ => false,
        }
    }

    fn value_is_named(&self, v: &Value, name: &str) -> bool {
        match (name, v) {
            ("string", Value::Str(_))
            | ("bool", Value::Bool(_))
            | ("char", Value::Char(_))
            | ("bigint", Value::BigIntV(_))
            | ("bigdec", Value::BigDecV(_))
            | ("float64", Value::F64(_))
            | ("float32", Value::F32(_)) => true,
            // The width is the value's own: an int64 does not hold an int32,
            // even when the number would fit. `is` reports what a value *is*.
            ("int32" | "int", Value::I32(_)) => true,
            ("int64", Value::I64(_)) => true,
            ("uint32" | "uint", Value::U32(_)) => true,
            ("uint64", Value::U64(_)) => true,
            // A class, or any of its bases.
            (_, Value::Instance(inst)) => {
                let mut cls = Some(inst.borrow().class.clone());
                while let Some(c) = cls {
                    if c.name() == name {
                        return true;
                    }
                    cls = c.parent.clone();
                }
                false
            }
            _ => false,
        }
    }

    pub(crate) fn eval_cast(&mut self, v: Value, wrapping: bool, ty: &TypeExpr) -> VResult {
        let TypeExpr::Named { name, .. } = ty else {
            return Ok(v); // casts to complex types: checker's concern
        };
        let out_of_range = || {
            self.throw(
                "RangeError",
                format!("value does not fit `{name}` (use `as wrapping`)"),
            )
        };

        // A cast is how a value of type `unknown` — a JSON document, a host
        // object — becomes something the checker will let you use. So the cast
        // is the one place where the claim can still be checked, and it *is*
        // checked: a cast that is wrong throws here, at the cast, rather than
        // letting a record travel inside an `int32` and fail somewhere else
        // (§: no undefined behaviour).
        match (name.as_str(), &v) {
            ("string", Value::Str(_))
            | ("bool", Value::Bool(_))
            | ("char", Value::Char(_))
            | ("bigint", Value::BigIntV(_))
            | ("bigdec", Value::BigDecV(_)) => return Ok(v),
            ("string" | "bool" | "char" | "bigint" | "bigdec", other) => {
                return Err(self.throw(
                    "TypeError",
                    format!("cannot cast {} to `{name}`", kind_of(other)),
                ))
            }
            _ => {}
        }

        let as_f = match as_num(&v) {
            Some(f) => f,
            None => {
                // A numeric cast of something that is not a number is a lie the
                // checker could not catch; the runtime can, and does.
                if is_numeric_type_name(name) {
                    return Err(self.throw(
                        "TypeError",
                        format!("cannot cast {} to `{name}`", kind_of(&v)),
                    ));
                }
                // A reference cast (`x as Element`, `x as MyClass`): check what
                // can be checked, and pass a host object through — the host owns
                // its own types.
                if let Value::Instance(inst) = &v {
                    let mut cls = Some(inst.borrow().class.clone());
                    while let Some(c) = cls {
                        if c.name() == name {
                            return Ok(v);
                        }
                        cls = c.parent.clone();
                    }
                    return Err(self.throw(
                        "TypeError",
                        format!("cannot cast a `{}` to `{name}`", inst.borrow().class.name()),
                    ));
                }
                return Ok(v);
            }
        };
        let as_i = as_i64(&v);
        macro_rules! to_int {
            ($t:ty, $mk:expr) => {{
                if wrapping {
                    match as_i {
                        Some(i) => $mk(i as $t),
                        None => $mk(as_f as $t), // saturating from float, defined
                    }
                } else {
                    match as_i {
                        Some(i) => match <$t>::try_from(i) {
                            Ok(x) => $mk(x),
                            Err(_) => return Err(out_of_range()),
                        },
                        None => {
                            let t = as_f.trunc();
                            if t >= <$t>::MIN as f64 && t <= <$t>::MAX as f64 && t == as_f {
                                $mk(t as $t)
                            } else if t >= <$t>::MIN as f64 && t <= <$t>::MAX as f64 {
                                $mk(t as $t) // fractional part dropped, in range
                            } else {
                                return Err(out_of_range());
                            }
                        }
                    }
                }
            }};
        }
        Ok(match name.as_str() {
            "int32" | "int" => to_int!(i32, Value::I32),
            "uint32" | "uint" => to_int!(u32, Value::U32),
            "int64" => to_int!(i64, Value::I64),
            "uint64" => {
                if wrapping {
                    match &v {
                        Value::I64(i) => Value::U64(*i as u64),
                        Value::I32(i) => Value::U64(*i as i64 as u64),
                        _ => Value::U64(as_f as u64),
                    }
                } else {
                    match as_i {
                        Some(i) if i >= 0 => Value::U64(i as u64),
                        Some(_) => return Err(out_of_range()),
                        None => match &v {
                            Value::U64(u) => Value::U64(*u),
                            _ => Value::U64(as_f as u64),
                        },
                    }
                }
            }
            "int8" => to_int!(i8, |x: i8| Value::I32(x as i32)),
            "int16" => to_int!(i16, |x: i16| Value::I32(x as i32)),
            "uint8" => to_int!(u8, |x: u8| Value::I32(x as i32)),
            "uint16" => to_int!(u16, |x: u16| Value::I32(x as i32)),
            "float64" | "float" => Value::F64(as_f),
            "float32" => Value::F32(as_f as f32),
            _ => v, // class/interface cast: dynamic checks arrive with the checker
        })
    }
}

fn graph_is_module(spec: &str) -> bool {
    mersey_front::graph::is_module(spec)
}

fn walk_pattern<'a>(p: &'a Pattern, out: &mut Vec<&'a str>) {
    match p {
        Pattern::Name(n) => out.push(&n.text),
        Pattern::Array { elems, rest } => {
            for e in elems {
                walk_pattern(&e.target, out);
            }
            if let Some(r) = rest {
                walk_pattern(r, out);
            }
        }
        Pattern::Record(fields) => {
            for f in fields {
                match &f.target {
                    Some(t) => walk_pattern(t, out),
                    None => out.push(&f.name.text),
                }
            }
        }
    }
}

/// Values a module exports, read out of its scope after evaluation.
fn collect_exports(module: &'static Module, env: &Env) -> HashMap<String, Value> {
    let mut out = HashMap::default();
    let mut take = |name: &str, exported: &str| {
        if let Some(v) = env_get(env, name) {
            out.insert(exported.to_string(), v);
        }
    };
    for item in &module.items {
        let Item::Export(ex) = item else { continue };
        match &ex.kind {
            ExportKind::Decl(d) => {
                let name = match d {
                    Decl::Function(f) => &f.name.text,
                    Decl::Class(c) => &c.name.text,
                    Decl::Enum(e) => &e.name.text,
                    // Interfaces and aliases are types only: no runtime value.
                    Decl::Interface(_) | Decl::TypeAlias(_) => continue,
                };
                take(name, name);
            }
            ExportKind::Var(v) => {
                for b in &v.bindings {
                    let mut names = Vec::new();
                    walk_pattern(&b.target, &mut names);
                    for n in names {
                        take(n, n);
                    }
                }
            }
            ExportKind::Named { specs, .. } => {
                // Re-exports (`export { x } from "./y"`) work because the
                // import already bound `x` into this module's scope.
                for s in specs {
                    let exported = s.alias.as_ref().unwrap_or(&s.name);
                    take(&s.name.text, &exported.text);
                }
            }
        }
    }
    out
}

/// RAII-ish frame for tracking the class whose method is executing (for
/// `super`). Kept tiny: a manual stack with a guard.
struct Frame<'a> {
    i: &'a mut Interp,
    pushed: bool,
    framed: bool,
}

impl<'a> Frame<'a> {
    fn enter(i: &'a mut Interp, c: &Closure, env: &Env) -> Frame<'a> {
        let pushed = if let Some(cls) = &c.cls {
            i.class_stack.push(cls.clone());
            true
        } else {
            false
        };
        // The VM keeps the diagnostic call stack itself; tree-walked calls
        // only need it when a debugger is watching (`DebugPause::frames`).
        // The env rides along so outer frames can serve their variables.
        let framed = i.debug_hook.is_some();
        if framed {
            let module = i.current_module.clone();
            i.push_frame(&c.data.name, &module.as_str().into());
            i.debug_envs.push(env.clone());
        }
        Frame { i, pushed, framed }
    }
}

impl Drop for Frame<'_> {
    fn drop(&mut self) {
        if self.pushed {
            self.i.class_stack.pop();
        }
        if self.framed {
            self.i.pop_frame();
            self.i.debug_envs.pop();
        }
    }
}

// ---- helpers ------------------------------------------------------------------------

fn find_in_chain<T>(class: &Rc<ClassDef>, f: impl Fn(&Rc<ClassDef>) -> Option<T>) -> Option<T> {
    let mut cls = Some(class.clone());
    while let Some(c) = cls {
        if let Some(t) = f(&c) {
            return Some(t);
        }
        cls = c.parent.clone();
    }
    None
}

/// Every name a pattern binds (`let [a, b] = …`, `let {x} = …`).
/// A hard cap on Mersey call depth: the single limit every tier enforces, so a
/// program throws `RangeError` at the same depth on the tree-walker, the VM, and
/// the JIT alike. The tree-walker recurses on the Rust stack and grows it on
/// demand (`stacker::maybe_grow` in `call_closure`) to actually reach this depth;
/// the VM/JIT loop and count frames directly. It is deterministic by design —
/// the same program throws at the same depth regardless of build or platform.
const MAX_CALL_DEPTH: usize = 3_000;

/// Resolve an `at`-style index against a length: negative counts from the end,
/// and anything outside the range is `None` rather than a panic or a wrap.
fn resolve_at(i: i64, len: usize) -> Option<usize> {
    let i = if i < 0 { i + len as i64 } else { i };
    (i >= 0 && (i as usize) < len).then_some(i as usize)
}

pub(crate) fn pattern_names_of(p: &Pattern, out: &mut Vec<String>) {
    match p {
        Pattern::Name(n) => out.push(n.text.clone()),
        Pattern::Array { elems, rest } => {
            for e in elems {
                pattern_names_of(&e.target, out);
            }
            if let Some(r) = rest {
                pattern_names_of(r, out);
            }
        }
        Pattern::Record(fields) => {
            for f in fields {
                match &f.target {
                    Some(p) => pattern_names_of(p, out),
                    None => out.push(f.name.text.clone()),
                }
            }
        }
    }
}

#[allow(dead_code)]
fn class_has_field(class: &Rc<ClassDef>, name: &str) -> bool {
    find_in_chain(class, |c| {
        c.fields.iter().any(|(n, _)| n == name).then_some(())
    })
    .is_some()
}

/// Allocate a tracked array (the collector must know about it).
pub(crate) fn new_array(items: Vec<Value>) -> Value {
    let a = Rc::new(GcCell::new(items));
    gc::track_array(&a);
    Value::Array(a)
}

pub(crate) fn new_record(fields: Vec<(String, Value)>) -> Value {
    let r = Rc::new(GcCell::new(fields));
    gc::track_record(&r);
    Value::Record(r)
}

pub(crate) fn new_map(entries: Vec<(Value, Value)>) -> Value {
    let mut data = MapData::default();
    for (k, v) in entries {
        data.insert(Key(k), v);
    }
    let m = Rc::new(GcCell::new(data));
    gc::track_map(&m);
    Value::MapV(m)
}

pub(crate) fn new_set(items: Vec<Value>) -> Value {
    let mut data = SetData::default();
    for v in items {
        data.insert(Key(v));
    }
    let sset = Rc::new(GcCell::new(data));
    gc::track_set(&sset);
    Value::SetV(sset)
}

/// Field lookup in an insertion-ordered record (records are small).
pub(crate) fn rec_get(fields: &[(String, Value)], name: &str) -> Option<Value> {
    fields
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.clone())
}

/// Set a field, preserving its original position if it already exists.
pub(crate) fn rec_set(fields: &mut Vec<(String, Value)>, name: &str, value: Value) {
    match fields.iter_mut().find(|(k, _)| k == name) {
        Some(slot) => slot.1 = value,
        None => fields.push((name.to_string(), value)),
    }
}

/// Howard Hinnant's civil-from-days / days-from-civil (proleptic Gregorian).
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `YYYY-MM-DDTHH:MM:SS[.mmm]Z` -> milliseconds since the epoch.
///
/// Strict on purpose: a date parser that guesses is how "01/02/03" becomes
/// three different days in three different places.
fn parse_iso8601(t: &str) -> Option<f64> {
    let b: Vec<char> = t.chars().collect();
    if b.len() < 20 || b[4] != '-' || b[7] != '-' || b[10] != 'T' || b[13] != ':' || b[16] != ':' {
        return None;
    }
    if *b.last()? != 'Z' {
        return None; // UTC only: an offset is a different value, not a format
    }
    let num = |a: usize, z: usize| -> Option<i64> { t.get(a..z)?.parse::<i64>().ok() };
    let (y, mo, d) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (h, mi, sec) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || sec > 60 {
        return None;
    }
    let millis = if b[19] == '.' {
        num(20, 23)?
    } else if b.len() == 20 {
        0
    } else {
        return None;
    };
    let days = days_from_civil(y, mo, d);
    let secs = days * 86_400 + h * 3600 + mi * 60 + sec;
    Some(secs as f64 * 1000.0 + millis as f64)
}

/// Names of the numeric types a cast can produce.
fn is_numeric_type_name(name: &str) -> bool {
    matches!(
        name,
        "int8"
            | "int16"
            | "int32"
            | "int"
            | "int64"
            | "uint8"
            | "uint16"
            | "uint32"
            | "uint"
            | "uint64"
            | "float32"
            | "float64"
    )
}

fn as_num(v: &Value) -> Option<f64> {
    Some(match v {
        Value::I32(n) => *n as f64,
        Value::I64(n) => *n as f64,
        Value::U32(n) => *n as f64,
        Value::U64(n) => *n as f64,
        Value::F32(f) => *f as f64,
        Value::F64(f) => *f,
        _ => return None,
    })
}

fn as_i64(v: &Value) -> Option<i64> {
    Some(match v {
        Value::I32(n) => *n as i64,
        Value::I64(n) => *n,
        Value::U32(n) => *n as i64,
        Value::U64(n) => *n as i64,
        _ => return None,
    })
}

/// Usual arithmetic conversions (§3.3): float wins; wider rank wins;
/// unsigned wins at equal rank.
fn promote_pair(a: &Value, b: &Value) -> Option<(Value, Value)> {
    use Value::*;
    let rank = |v: &Value| match v {
        I32(_) => Some(0),
        U32(_) => Some(1),
        I64(_) => Some(2),
        U64(_) => Some(3),
        F32(_) => Some(4),
        F64(_) => Some(5),
        _ => None,
    };
    let (ra, rb) = (rank(a)?, rank(b)?);
    let target = ra.max(rb);
    let conv = |v: &Value| -> Value {
        match (v, target) {
            (I32(x), 0) => I32(*x),
            (v, 1) => U32(match v {
                I32(x) => *x as u32,
                U32(x) => *x,
                _ => unreachable!(),
            }),
            (v, 2) => I64(match v {
                I32(x) => *x as i64,
                U32(x) => *x as i64,
                I64(x) => *x,
                _ => unreachable!(),
            }),
            (v, 3) => U64(match v {
                I32(x) => *x as i64 as u64,
                U32(x) => *x as u64,
                I64(x) => *x as u64,
                U64(x) => *x,
                _ => unreachable!(),
            }),
            (v, 4) => F32(as_num(v).unwrap() as f32),
            (v, _) => F64(as_num(v).unwrap()),
        }
    };
    Some((conv(a), conv(b)))
}

fn num_eq(a: &Value, b: &Value) -> bool {
    use Value::*;
    match (a, b) {
        (I32(x), I32(y)) => x == y,
        (U32(x), U32(y)) => x == y,
        (I64(x), I64(y)) => x == y,
        (U64(x), U64(y)) => x == y,
        (F32(x), F32(y)) => x == y,
        (F64(x), F64(y)) => x == y,
        _ => false,
    }
}

fn kind_of(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::I32(_) => "int32",
        Value::I64(_) => "int64",
        Value::U32(_) => "uint32",
        Value::U64(_) => "uint64",
        Value::F32(_) => "float32",
        Value::F64(_) => "float64",
        Value::Char(_) => "char",
        Value::Str(_) => "string",
        Value::BigIntV(_) => "bigint",
        Value::BigDecV(_) => "bigdec",
        Value::MapV(_) => "Map",
        Value::SetV(_) => "Set",
        Value::Array(_) => "array",
        Value::Record(_) => "record",
        Value::Closure(_) => "function",
        Value::Class(_) => "class",
        Value::Instance(_) => "object",
        Value::Namespace(_) => "namespace",
        Value::Dom(_) => "dom element",
        Value::JsRef(_) => "web object",
        Value::Bytes(_) => "Bytes",
        Value::RegexV(_) => "Regex",
        Value::UrlV(_) => "Url",
        Value::IterV(_) => "Iter",
        Value::PromiseV(_) => "Promise",
        Value::Resolve(..) | Value::Reject(..) | Value::AllSlot(..) | Value::PromiseExec(..) => {
            "function"
        }
        Value::Native(_) => "native function",
    }
}

pub fn to_display(v: &Value) -> String {
    match v {
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::I32(n) => n.to_string(),
        Value::I64(n) => n.to_string(),
        Value::U32(n) => n.to_string(),
        Value::U64(n) => n.to_string(),
        Value::F32(f) => f.to_string(),
        Value::F64(f) => f.to_string(),
        Value::Char(c) => c.to_string(),
        Value::Str(s) => utf16_to_string(s),
        Value::BigIntV(b) => b.to_decimal(),
        Value::BigDecV(d) => d.to_decimal(),
        Value::MapV(m) => {
            let items: Vec<String> = m
                .borrow()
                .iter()
                .map(|(k, v)| format!("{} => {}", to_display(&k.0), to_display(v)))
                .collect();
            format!("Map{{{}}}", items.join(", "))
        }
        Value::SetV(m) => {
            let items: Vec<String> = m.borrow().iter().map(|k| to_display(&k.0)).collect();
            format!("Set{{{}}}", items.join(", "))
        }
        Value::Array(a) => {
            let items: Vec<String> = a.borrow().iter().map(to_display).collect();
            format!("[{}]", items.join(", "))
        }
        Value::Record(r) => {
            let fields: Vec<String> = r
                .borrow()
                .iter()
                .map(|(k, v)| format!("{k}: {}", to_display(v)))
                .collect();
            format!("{{{}}}", fields.join(", "))
        }
        Value::Closure(_) | Value::Native(_) => "<function>".to_string(),
        Value::Class(c) => format!("<class {}>", c.name),
        Value::Instance(i) => format!("<{}>", i.borrow().class.name),
        Value::Namespace(ns) => format!("<{}>", ns.name),
        Value::Dom(id) => format!("<#{id}>"),
        Value::JsRef(h) => format!("<web:{h}>"),
        Value::Bytes(b) => format!("<Bytes[{}]>", b.borrow().len()),
        Value::RegexV(_) => "<Regex>".to_string(),
        Value::UrlV(u) => u.as_str().to_string(),
        Value::IterV(_) => "<Iter>".to_string(),
        Value::PromiseV(_) => "<Promise>".to_string(),
        Value::Resolve(..) | Value::Reject(..) | Value::AllSlot(..) | Value::PromiseExec(..) => {
            "<function>".to_string()
        }
    }
}

/// Literal text → runtime value; pure so the bytecode compiler can bake
/// constants at compile time. Err = (error class, message).
pub(crate) fn parse_literal(kind: LitKind, text: &str) -> Result<Value, (&'static str, String)> {
    match kind {
        LitKind::Null => Ok(Value::Null),
        LitKind::Bool => Ok(Value::Bool(text == "true")),
        LitKind::Str => {
            let inner = &text[1..text.len() - 1];
            Ok(Value::Str(Rc::new(utf16(&(unescape(inner))))))
        }
        LitKind::Char => {
            let inner = &text[2..text.len() - 1]; // strip c' and '
            let s = unescape(inner);
            s.chars()
                .next()
                .map(Value::Char)
                .ok_or_else(|| ("TypeError", "empty char literal".to_string()))
        }
        LitKind::Int => parse_int_literal(text),
        LitKind::Float => {
            let is_f32 = text.ends_with('f');
            let core: String = text.trim_end_matches('f').replace('_', "");
            let v: f64 = core
                .parse()
                .map_err(|_| ("TypeError", format!("bad float literal `{text}`")))?;
            Ok(if is_f32 {
                Value::F32(v as f32)
            } else {
                Value::F64(v)
            })
        }
        LitKind::BigInt => {
            let t = text.replace('_', "");
            let body = t.trim_end_matches('n');
            let (radix, body) = if let Some(b) = body.strip_prefix("0x") {
                (16, b)
            } else if let Some(b) = body.strip_prefix("0o") {
                (8, b)
            } else if let Some(b) = body.strip_prefix("0b") {
                (2, b)
            } else {
                (10, body)
            };
            match BigInt::parse(body, radix) {
                Some(b) => Ok(Value::BigIntV(Rc::new(b))),
                None => Err(("TypeError", format!("bad bigint literal `{text}`"))),
            }
        }
        LitKind::BigDec => {
            let t = text.replace('_', "");
            match BigDec::parse(t.trim_end_matches('m')) {
                Some(b) => Ok(Value::BigDecV(Rc::new(b))),
                None => Err(("TypeError", format!("bad bigdec literal `{text}`"))),
            }
        }
    }
}

/// `-2147483648` is a perfectly good int32, but `2147483648` is not — so a
/// minus sign in front of an integer literal has to be *part of the literal*,
/// not an operation applied to it afterwards. Both tiers fold it here.
pub(crate) fn negated_int_literal(text: &str) -> Result<Value, (&'static str, String)> {
    parse_int_literal_signed(text, true)
}

fn parse_int_literal(text: &str) -> Result<Value, (&'static str, String)> {
    parse_int_literal_signed(text, false)
}

fn parse_int_literal_signed(text: &str, neg: bool) -> Result<Value, (&'static str, String)> {
    let t = text.replace('_', "");
    const SUFFIXES: &[&str] = &[
        "u64", "u32", "u16", "ul", "u8", "i64", "i32", "i16", "i8", "l", "u",
    ];
    let suffix = SUFFIXES
        .iter()
        .find(|s| t.ends_with(**s))
        .copied()
        .unwrap_or("");
    let digits = &t[..t.len() - suffix.len()];
    let (radix, body) = if let Some(b) = digits.strip_prefix("0x") {
        (16, b)
    } else if let Some(b) = digits.strip_prefix("0o") {
        (8, b)
    } else if let Some(b) = digits.strip_prefix("0b") {
        (2, b)
    } else {
        (10, digits)
    };
    let raw = u64::from_str_radix(body, radix)
        .map_err(|_| ("RangeError", format!("integer literal `{text}` overflows")))?;
    let sign = if neg { "-" } else { "" };
    let out_of = || {
        (
            "RangeError",
            format!("literal `{sign}{text}` does not fit its type"),
        )
    };
    // Widen before applying the sign, so `-2147483648` is representable even
    // though `2147483648` is not.
    let v: i128 = if neg { -(raw as i128) } else { raw as i128 };
    let fit = |lo: i128, hi: i128| -> Result<i128, (&'static str, String)> {
        if v >= lo && v <= hi {
            Ok(v)
        } else {
            Err(out_of())
        }
    };
    Ok(match suffix {
        "" | "i32" => Value::I32(fit(i32::MIN as i128, i32::MAX as i128)? as i32),
        "u" | "u32" => Value::U32(fit(0, u32::MAX as i128)? as u32),
        "l" | "i64" => Value::I64(fit(i64::MIN as i128, i64::MAX as i128)? as i64),
        "ul" | "u64" => Value::U64(fit(0, u64::MAX as i128)? as u64),
        // Small types promote to int32 immediately (§3.3 rule 1).
        "i8" => Value::I32(fit(i8::MIN as i128, i8::MAX as i128)? as i32),
        "i16" => Value::I32(fit(i16::MIN as i128, i16::MAX as i128)? as i32),
        "u8" => Value::I32(fit(0, u8::MAX as i128)? as i32),
        "u16" => Value::I32(fit(0, u16::MAX as i128)? as i32),
        _ => return Err(("TypeError", format!("unsupported suffix on `{text}`"))),
    })
}

fn unescape(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some('0') => out.push('\0'),
            Some('u') => {
                // \u{XXXX}
                let mut val = 0u32;
                for c in chars.by_ref() {
                    if c == '{' {
                        continue;
                    }
                    if c == '}' {
                        break;
                    }
                    val = val * 16 + c.to_digit(16).unwrap_or(0);
                }
                if let Some(c) = char::from_u32(val) {
                    out.push(c);
                }
            }
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

#[cfg(test)]
mod json_bench {
    //! Isolates the per-iteration engine cost of the `json` web benchmark
    //! (`JSON.stringify({ lang: "mersey", version: i, ok: true })`) so the
    //! serialization path can be tuned without a fork rebuild. Run with:
    //!   cargo test -p mersey_interp --release json_bench -- --nocapture --ignored
    use super::*;
    use std::time::Instant;

    #[test]
    #[ignore]
    fn json_hot_loop() {
        const N: i32 = 20000;
        const OUTER: u32 = 40;
        // The string literal `"mersey"` is const-pooled in the real workload —
        // a single shared Rc, not re-encoded each iteration.
        let mersey: Rc<Vec<u16>> = Rc::new(utf16("mersey"));
        let phase = |label: &str, mut body: Box<dyn FnMut() -> usize>| {
            let mut best = f64::INFINITY;
            let mut sum = 0usize;
            for _ in 0..OUTER {
                let t = Instant::now();
                for _ in 0..N {
                    sum = sum.wrapping_add(body());
                }
                best = best.min(t.elapsed().as_nanos() as f64 / N as f64);
            }
            println!(
                "  {label:22} best {best:7.1} ns/iter  ({:5.2} ms/{N})  sum={sum}",
                best * N as f64 / 1.0e6
            );
        };

        // (a) record build only.
        {
            let m = mersey.clone();
            let mut i = 0i32;
            phase(
                "record build",
                Box::new(move || {
                    let rec = new_record(vec![
                        ("lang".to_string(), Value::Str(m.clone())),
                        ("version".to_string(), Value::I32(i)),
                        ("ok".to_string(), Value::Bool(true)),
                    ]);
                    i = i.wrapping_add(1);
                    match &rec {
                        Value::Record(r) => r.borrow().len(),
                        _ => 0,
                    }
                }),
            );
        }
        // (b) serialize a pre-built record to a String.
        {
            let rec = new_record(vec![
                ("lang".to_string(), Value::Str(mersey.clone())),
                ("version".to_string(), Value::I32(12345)),
                ("ok".to_string(), Value::Bool(true)),
            ]);
            phase(
                "serialize (prebuilt)",
                Box::new(move || {
                    let mut out = String::new();
                    Interp::pure_json(&rec, &mut out);
                    out.len()
                }),
            );
        }
        // (c) utf8 -> utf16 conversion of the output.
        {
            let out = String::from("{\"lang\":\"mersey\",\"version\":12345,\"ok\":true}");
            phase(
                "utf16 convert",
                Box::new(move || Rc::new(utf16(&out)).len()),
            );
        }
        // (d) the whole loop, as the workload runs it.
        {
            let m = mersey.clone();
            let mut i = 0i32;
            phase(
                "full (build+ser+u16)",
                Box::new(move || {
                    let rec = new_record(vec![
                        ("lang".to_string(), Value::Str(m.clone())),
                        ("version".to_string(), Value::I32(i)),
                        ("ok".to_string(), Value::Bool(true)),
                    ]);
                    i = i.wrapping_add(1);
                    let mut out = String::new();
                    Interp::pure_json(&rec, &mut out);
                    Rc::new(utf16(&out)).len()
                }),
            );
        }
        let _ = mersey;
    }
}

// ---- the REPL session (host-agnostic) --------------------------------------

/// What one REPL turn produced. `Rejected` turns were refused by
/// decode/parse/bind/check and are NOT part of the session; `Threw` turns
/// were accepted (their declarations may have taken effect) and are kept,
/// the same contract as a script that threw.
pub enum ReplOutcome {
    /// Ran; the display of a trailing bare expression, if any.
    Ran(Option<String>),
    Rejected(String),
    Threw(String),
}

/// A REPL session: the accumulated program and how much of it has executed.
/// Host-agnostic — the CLI, the WASM build, and the C ABI all drive this one
/// implementation. Each turn appends the fragment, re-parses and re-checks
/// the WHOLE program (the checker must see every prior declaration; the
/// session's program always typechecks), then executes only the new items in
/// the given interpreter (`Interp::run_repl_turn`). Semicolons are appended
/// for bare fragments — the module grammar's business, not the user's.
#[derive(Default)]
pub struct ReplSession {
    accumulated: String,
    executed_items: usize,
}

impl ReplSession {
    pub fn new() -> Self {
        Self::default()
    }

    /// The names the session can see at its top level — what a console's
    /// completion should offer in Mersey mode: imported bindings, declared
    /// functions/classes/enums/types, and top-level variable bindings. The
    /// accumulated program always parses (rejected turns never enter it).
    pub fn completions(&self) -> Vec<String> {
        fn pattern_names(p: &Pattern, out: &mut Vec<String>) {
            match p {
                Pattern::Name(n) => out.push(n.text.clone()),
                Pattern::Array { elems, rest } => {
                    for e in elems {
                        pattern_names(&e.target, out);
                    }
                    if let Some(r) = rest {
                        pattern_names(r, out);
                    }
                }
                Pattern::Record(fields) => {
                    for f in fields {
                        match &f.target {
                            Some(t) => pattern_names(t, out),
                            None => out.push(f.name.text.clone()),
                        }
                    }
                }
            }
        }
        let mut out = Vec::new();
        let src = match mersey_front::source::decode("<repl>", self.accumulated.as_bytes()) {
            Ok(s) => s,
            Err(_) => return out,
        };
        let parsed = mersey_front::parser::parse(&src);
        for item in &parsed.module.items {
            match item {
                Item::Import(im) => {
                    if let Some(ImportClause::Named(list)) = &im.clause {
                        for spec in list {
                            out.push(spec.alias.as_ref().unwrap_or(&spec.name).text.clone());
                        }
                    } else if let Some(ImportClause::Namespace(n)) = &im.clause {
                        out.push(n.text.clone());
                    }
                }
                Item::Decl(d)
                | Item::Export(ExportDecl {
                    kind: ExportKind::Decl(d),
                    ..
                }) => match d {
                    Decl::Function(f) => out.push(f.name.text.clone()),
                    Decl::Class(c) => out.push(c.name.text.clone()),
                    Decl::Interface(i) => out.push(i.name.text.clone()),
                    Decl::Enum(e) => out.push(e.name.text.clone()),
                    Decl::TypeAlias(t) => out.push(t.name.text.clone()),
                },
                Item::Stmt(Stmt::Var(v))
                | Item::Export(ExportDecl {
                    kind: ExportKind::Var(v),
                    ..
                }) => {
                    for b in &v.bindings {
                        pattern_names(&b.target, &mut out);
                    }
                }
                _ => {}
            }
        }
        out.sort();
        out.dedup();
        out
    }

    pub fn turn(&mut self, interp: &mut Interp, fragment: &str) -> ReplOutcome {
        // Every session begins with the console import — seeded lazily so all
        // hosts (CLI, WASM, C ABI) share the behavior without each arranging
        // a prelude turn of its own.
        if self.accumulated.is_empty() {
            self.accumulated = "import { console } from \"std:console\";\n".to_string();
            let src = mersey_front::source::decode("<repl>", self.accumulated.as_bytes())
                .expect("prelude decodes");
            let parsed = mersey_front::parser::parse(&src);
            let module: &'static Module = Box::leak(Box::new(parsed.module));
            self.executed_items = module.items.len();
            let _ = interp.run_repl_turn(module, 0);
        }
        let mut fragment = fragment.trim_end().to_string();
        if !(fragment.ends_with(';') || fragment.ends_with('}')) {
            fragment.push(';');
        }
        let candidate = format!("{}{}\n", self.accumulated, fragment);
        let src = match mersey_front::source::decode("<repl>", candidate.as_bytes()) {
            Ok(s) => s,
            Err(d) => return ReplOutcome::Rejected(d.to_string()),
        };
        let parsed = mersey_front::parser::parse(&src);
        if !parsed.diagnostics.is_empty() {
            return ReplOutcome::Rejected(
                parsed
                    .diagnostics
                    .iter()
                    .map(|d| d.to_string())
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
        }
        let module: &'static Module = Box::leak(Box::new(parsed.module));
        let bound = mersey_front::bind::bind(module);
        if !bound.diagnostics.is_empty() {
            return ReplOutcome::Rejected(
                bound
                    .diagnostics
                    .iter()
                    .map(|d| d.to_string())
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
        }
        let checked = check::check(module);
        if !checked.diagnostics.is_empty() {
            return ReplOutcome::Rejected(
                checked
                    .diagnostics
                    .iter()
                    .map(|d| d.to_string())
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
        }
        let out = interp.run_repl_turn(module, self.executed_items);
        self.accumulated = candidate;
        self.executed_items = module.items.len();
        match out {
            Ok(echo) => ReplOutcome::Ran(echo),
            Err(t) => ReplOutcome::Threw(format!("runtime error: {}", interp.describe_thrown(&t))),
        }
    }
}
