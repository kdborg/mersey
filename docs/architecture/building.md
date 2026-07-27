# Building

One entry point builds everything, on every platform: `scripts/build.sh`. It
detects the host OS and architecture and builds the requested targets.

```
scripts/build.sh              # engine + CLI + C-ABI staticlib (the default)
scripts/build.sh wasm         # the WASM engine -> web/mersey_wasm.wasm
scripts/build.sh servo        # a browser fork (gecko | chromium | servo | ladybird)
scripts/build.sh all          # everything with a recipe on this platform
```

The **portable core** — the engine, the `mersey` CLI, the C-ABI staticlib, and
the WASM build — is plain `cargo`, so it builds natively on Linux, macOS and
Windows across arm64 and x86_64. The `build.yml` CI matrix compiles and tests it
on all five of those runners on every push, which is what makes the
cross-platform claim real rather than aspirational.

## Prerequisites (all platforms)

- **Rust** (stable, via rustup). For the WASM build:
  `rustup target add wasm32-unknown-unknown`.
- **Node 22+** — only for the web tests (`web/build-and-test.sh`, which also
  needs Playwright) and the benchmarks; the engine itself needs no Node.
- **A C compiler** — only for the native C-ABI host demo (`native/`), which is
  POSIX C (Linux/macOS).

## Engine, CLI, staticlib

```
scripts/build.sh engine       # target/release/mersey and libmersey_capi.{a,dylib,so,lib}
cargo test                    # Rust tests + the *.test.mersey standard-library suite
```

`cargo build --release -p mersey_capi` alone produces the staticlib that the
browser forks link.

## The browser forks

A fork is not stored in full; it is reconstructed by applying this repo's
overlay onto a pinned upstream checkout (see `browser-integration.md` and
`scripts/fork-overlay.sh`). `scripts/build.sh <fork>` uses a per-platform
fast-path recipe (`scripts/build-<os>-<arch>.sh`) where one exists — macOS arm64
does — and otherwise the portable path: `scripts/fork-overlay.sh bootstrap
<fork>` (or `chromium/bootstrap.sh`), which runs the fork's own build commands
from its `BASELINE`. So `build.sh servo` / `ladybird` / `gecko` / `chromium`
work on Linux and Windows too, without a bespoke recipe per platform.

Per-fork build dependencies:

- **Servo** — a Rust toolchain and Servo's own dependencies (`./mach bootstrap`
  on a fresh checkout installs them); Python 3 for `mach`. Built with
  `--media-stack dummy` unless GStreamer is present (macOS wants the official
  `GStreamer.framework`, which it never finds via Homebrew/pkg-config); Mersey's
  workloads use no audio/video, so dummy is the default.
- **Ladybird** — CMake and a recent C++ compiler (Clang/GCC); Qt for the GUI.
  On macOS install these with Homebrew and make sure `/opt/homebrew/bin` and the
  GNU coreutils/libtool come *before* any depot_tools shims on `PATH`. Built via
  `./Meta/ladybird.py build`.
- **Chromium** — depot_tools and a gclient sync (~29 GB, and hours the first
  time). `chromium/bootstrap.sh` seeds and builds it; the hermetic toolchains
  mean only `target_os` / `target_cpu` change between platforms. macOS needs
  Xcode.
- **Gecko / Firefox** — `./mach bootstrap` for dependencies, then `./mach
  build` with `--enable-artifact-builds` turned OFF in `mozconfig` (artifact
  builds download prebuilt C++ that contains no `dom/mersey`). The vendored
  Cranelift crates are refreshed at bootstrap.

Browser builds are heavy — minutes to hours — so they run on the self-hosted
`browsers.yml` runners, not on the portable-core matrix.

### macOS arm64 fast path

`scripts/build-macos-arm64.sh [all|staticlib|gecko|chromium|servo|ladybird]`
builds the staticlib and then each fork with its native toolchain. Override
checkout locations with `GECKO_SRC` / `CHROMIUM_SRC` / `SERVO_SRC` /
`LADYBIRD_SRC`.

## Continuous integration

- **`build.yml`** — the portable core on Linux / macOS / Windows × x64 / arm64,
  plus the native C-ABI host and per-platform engine perf numbers.
- **`ci.yml`** — `fmt`, `clippy`, the full `cargo test` (including the
  `*.test.mersey` suite), the generated docs, a GC-verify pass, a fuzz round,
  the web tests, and the fork-overlay integrity checks.
- **`browsers.yml`** — the fork builds and native benchmarks, on self-hosted
  runners provisioned with the fork checkouts.
