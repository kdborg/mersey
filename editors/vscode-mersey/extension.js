// Mersey Debug: point VS Code's DAP client at `mersey dap`. The adapter does
// everything (crates/mersey_cli/src/dap.rs); this is only the wiring.
const vscode = require("vscode");

exports.activate = (context) => {
  context.subscriptions.push(
    vscode.debug.registerDebugAdapterDescriptorFactory("mersey", {
      createDebugAdapterDescriptor() {
        // `mersey` must be on PATH (cargo build --release; add target/release).
        return new vscode.DebugAdapterExecutable("mersey", ["dap"]);
      },
    }),
  );
};

exports.deactivate = () => {};
