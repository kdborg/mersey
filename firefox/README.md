# Mersey Gecko — Firefox/Gecko integration (Stage B)

> ⚠️ **Mersey Gecko (Experimental).** This is an experimental Mersey-engine fork
> of Firefox, **not for production use**. It is not affiliated with, endorsed by,
> or supported by Mozilla or the Firefox project; "Firefox" and "Gecko" here name
> the upstream engine this fork is built on, not the shipping product. The build
> is renamed to **Mersey Gecko** (product/app display name, `about:` box, startup
> banner, console notice) so it can't be confused with a real Firefox install.

`<script type="text/mersey">` in a page is compiled and executed by the Mersey
engine inside Gecko — no SpiderMonkey for the Mersey code, no WASM in the stack —
through the same C ABI (`crates/mersey_capi/include/mersey.h`) that
`native/host_demo.c` and `crates/mersey_capi/tests/abi.rs` drive in this
repository, and that the Chromium, Servo and Ladybird forks drive too.

## The rename

The fork ships under the `unofficial` branding, retargeted to Mersey:

| where | file | value |
|---|---|---|
| app / bundle display name | `browser/branding/unofficial/configure.sh` | `MOZ_APP_DISPLAYNAME="Mersey Gecko (Experimental)"` |
| `about:` box, UI brand strings (Fluent) | `browser/branding/unofficial/locales/en-US/brand.ftl` | `Mersey Gecko` / `Mersey Gecko (Experimental)` |
| legacy brand strings | `browser/branding/unofficial/locales/en-US/brand.properties` | same |
| startup terminal banner + first-script console notice | `dom/mersey/MerseyScriptRunner.cpp` | emitted from the engine runner |

The macOS bundle is therefore `obj-mersey/dist/Mersey Gecko (Experimental).app`
(the native bench runner, `bench/web/run-native.mjs`, launches that path).

The overlay is applied and the fork rebuilt with `scripts/fork-overlay.sh
{apply,bootstrap} firefox` — see `firefox/BASELINE` for the pinned upstream
revision and the regen/build hooks.
