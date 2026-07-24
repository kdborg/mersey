# Mersey engine (//third_party/mersey)

The Mersey language engine behind its C embedding ABI (`include/mersey.h`),
consumed by Blink's `MerseyScriptRunner` to execute
`<script type="text/mersey">`.

`lib/libmersey_capi.a` is a prebuilt Rust staticlib for the host/target arch,
refreshed by `./refresh.sh` from the engine repository. The engine's own test
matrix (conformance suites on three execution tiers, a differential fuzzer,
and an ABI test that drives these exact symbols) runs in that repository —
this directory is a drop, not a source of truth.

Initial-fork wiring; the planned follow-up builds the crate graph with GN's
rust rules so all six platform/arch combinations come from one build.

## Building on an arm64 Linux host

Google ships no hermetic Chromium toolchain for linux-arm64, and this VM's
emulation cannot run the x86-64 prebuilts either — every hermetic binary in
`third_party` is x86-64. The fork therefore substitutes native tools. What
had to be replaced, and why (each was a hard failure, not a preference):

| hermetic tool | replacement | failure without it |
|---|---|---|
| `third_party/llvm-build` clang | system clang 21 via `clang_base_path` | x86-64 ELF; won't exec |
| clang resource dir layout | shadow tree at `~/opt/llvm21` | Ubuntu uses `lib/linux/…-aarch64.a`; Chromium expects the per-triple layout |
| `third_party/rust-toolchain` | rustup nightly via `rust_sysroot_absolute` | x86-64 |
| `bindgen` / `rustfmt` | `cargo install bindgen-cli` + rustup rustfmt, via `rust_bindgen_root` | x86-64; also needs a **real** `libclang.so` (not a symlink) beside it |
| `mold` | lld (`use_mold = false`) | x86-64 |
| `siso` | ninja (`use_siso = false`) | spawn helper traps on this kernel |
| `third_party/node` | nvm node **v24.12.0** (the version `update_node_binaries` pins) | x86-64; version check is strict |
| `third_party/gperf/cipd` | GNU gperf 3.1 built from source | no linux-arm64 CIPD package exists at all (DEPS carries the condition) |

Plus `use_tot_clang_flags = false` (added by this fork in
`build/config/clang/clang.gni`), which gates three flags only tip-of-tree
clang understands: `-fdiagnostics-show-inlining-chain`, `-fno-lifetime-dse`,
and `-fsanitize-ignore-for-ubsan-feature=`.

None of this is needed on x86-64 Linux, macOS or Windows, where the hermetic
toolchain works — which is the point of writing it down rather than carrying
it as tribal knowledge. See `out/mersey-arm64/args.gn` for the working set.

### And one that is not a toolchain substitution

`rust_std_in_executable_only = true`. Chromium's Rust allocator crate — which
defines `__rustc::__rust_alloc` and friends, referenced by *every* std rlib —
is linked into the final executable, not into each component `.so`. With the
hermetic Rust toolchain that is invisible. With a system one it leaves those
symbols unresolved in every DSO, and `-Wl,-z,defs` fails the link.

They resolve at load time from the binary, which is what a component build is —
the same exemption `no_unresolved_symbols` already makes for sanitizer
instrumentation, whose comment describes this exact situation.

Also note: the Rust toolchain must be **rustc 1.98**, matching Chromium's own.
A newer nightly namespaces the allocator symbols differently and nothing links.
`rustup toolchain install nightly-2026-06-15` is the one that works.

### The one that is not a workaround: `is_component_build = false`

`build/rust/std/BUILD.gn` says it plainly, in a comment about how the prebuilt
stdlib's rlibs are passed as **ldflags** rather than as dependencies:

> "This doesn't work for all types of build because ldflags propagate
> differently from actual dependencies and therefore can end up in different
> targets from the remap_alloc.cc above. For example, in a component build, we
> might apply the remap_alloc.cc file and these ldflags to shared object A,
> while shared object B (that depends upon A) might get only the ldflags but not
> remap_alloc.cc, and thus the build will fail. **There is currently no known
> solution to this for the prebuilt stdlib** — this problem does not apply with
> configurations where we build the stdlib ourselves, which is what we'll use in
> production."

A prebuilt Rust stdlib in a component build is unsupported, and an arm64 Linux
host has no choice about the prebuilt stdlib — so it has no choice about this
either. A **static build** puts the allocator and the stdlib ldflags in one
binary, where they cannot land apart.

The two symptoms, in the order they appear: `__rustc::__rust_alloc` undefined in
every component `.so` (which `-Wl,-z,defs` catches), and then — if you silence
that — the same symbols undefined in the executables that *load* those `.so`s,
via `--no-allow-shlib-undefined`. Both are the same missing allocator. Neither
is fixable by moving the flag around; the configuration is the problem.

x86-64 Linux, macOS and Windows use Chromium's own Rust toolchain, build the
stdlib from source, and can use a component build normally.
