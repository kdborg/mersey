# Mersey Debug (VS Code)

Debug `.mersey` programs with breakpoints, stepping, a call stack whose outer
frames show their call-site lines, and per-frame variables.

The extension is a thin wrapper: everything lives in the `mersey dap` adapter
(`crates/mersey_cli/src/dap.rs`). Requirements: `mersey` on your `PATH`
(`cargo build --release`, then add `target/release`).

Install from source:

```sh
cd editors/vscode-mersey
npx @vscode/vsce package        # -> mersey-debug-0.1.0.vsix
code --install-extension mersey-debug-0.1.0.vsix
```

Then open a `.mersey` file, set a breakpoint, and run the "Debug current
Mersey file" configuration (F5).

Async and generator bodies execute on the bytecode VM and report through
its line callouts — they break and step like sync code (their slot-resolved
locals are best-effort).
