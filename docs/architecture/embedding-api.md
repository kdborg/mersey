# Embedding API (C ABI)

The single interface every host (the `mersey` CLI, Chromium, tests, future
embedders) uses to drive the engine. Stability rule: additive-only after 1.0,
versioned entry point, no host ever links engine internals.

## Shape

```c
/* mersey.h — illustrative excerpt; the real header is generated
   from an interface definition so C and Rust never drift. */

typedef struct msy_engine  msy_engine;   /* process-wide: JIT config, code cache */
typedef struct msy_context msy_context;  /* isolate: own heap, own globals     */
typedef struct msy_value   msy_value;    /* handle, valid within a scope       */

msy_engine*  msy_engine_new(const msy_engine_options*);
msy_context* msy_context_new(msy_engine*, const msy_context_options*);

/* Module loading — the host supplies sources; the engine never touches
   the filesystem or network itself (spec §5.2). */
typedef msy_source (*msy_module_resolver)(void* host, msy_str specifier,
                                          msy_str referrer);
void msy_context_set_resolver(msy_context*, msy_module_resolver, void* host);

msy_status msy_context_load(msy_context*, msy_str main_specifier,
                            msy_module** out);
msy_status msy_module_evaluate(msy_context*, msy_module*, msy_value** out);

/* Calls in, calls out */
msy_status msy_call(msy_context*, msy_value* fn,
                    const msy_value* const* args, size_t argc,
                    msy_value** out, msy_error* err);
msy_status msy_context_register_host_fn(msy_context*, msy_str module,
                    msy_str name, msy_signature sig, msy_host_fn, void* data);

/* Scheduling: engine never blocks or spins; host owns the loop. */
bool msy_context_has_pending_tasks(const msy_context*);
void msy_context_run_microtasks(msy_context*);
void msy_context_notify_task(msy_context*, msy_task_id);
```

## Design rules

1. **Host owns I/O and the event loop.** The engine only computes. Timers,
   fetches, file reads are host functions registered per context — this is
   what lets the same engine sit behind Deno-style CLI capabilities and
   behind Blink's fetch stack without either knowing about the other.
2. **Typed boundary.** `msy_signature` describes host-function parameter and
   return types; the engine checks Mersey-side calls against it at load time
   (so a host fn call compiles to a direct trampoline, no per-call
   validation), and the host receives already-typed unboxed scalars.
3. **Handles + scopes**, V8-style: `msy_value` handles are owned by an
   explicit `msy_scope`, so the moving GC can update them; no raw pointers
   to heap objects ever cross the ABI.
4. **Strings at the boundary** are (`ptr`, `len`, `encoding`) with UTF-8,
   UTF-16, and UTF-32 accepted; the engine transcodes inward to UTF-32.
   Chromium will pass UTF-16 (Blink's native), the CLI UTF-8.
5. **Errors are values** (`msy_error`: type name, message, stack), never
   longjmp across the ABI; panics are caught at the boundary and surfaced as
   engine-bug errors.
6. **Access control holds at the ABI.** There is no embedding call that reads
   a `private` field; inspection beyond public surface requires creating the
   context with `debug_capability = true` (DevTools does; a web page's
   context does not).

## Threading model

A context is single-threaded (confined to the thread that created it, checked
in debug builds). Multiple contexts may run on different threads of one
engine. Worker-style parallelism = one context per worker + message passing
of structured-clonable values; `SharedArrayBuffer`-equivalent is exposed to
hosts that opt in.
