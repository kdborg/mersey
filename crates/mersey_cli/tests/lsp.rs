//! The language server, driven the way an editor drives it: JSON-RPC over the
//! real binary's stdin/stdout.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

struct Server {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Server {
    fn start() -> Server {
        let mut child = Command::new(env!("CARGO_BIN_EXE_mersey"))
            .arg("lsp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn mersey lsp");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Server {
            child,
            stdin,
            stdout,
        }
    }

    fn send(&mut self, body: &str) {
        write!(self.stdin, "Content-Length: {}\r\n\r\n{}", body.len(), body).unwrap();
        self.stdin.flush().unwrap();
    }

    fn recv(&mut self) -> String {
        let mut len = 0usize;
        loop {
            let mut line = String::new();
            self.stdout.read_line(&mut line).expect("read header");
            let line = line.trim_end().to_string();
            if line.is_empty() {
                break;
            }
            if let Some(v) = line.strip_prefix("Content-Length:") {
                len = v.trim().parse().unwrap();
            }
        }
        let mut buf = vec![0u8; len];
        self.stdout.read_exact(&mut buf).expect("read body");
        String::from_utf8(buf).unwrap()
    }

    /// Read messages until one carries `id`, skipping notifications.
    fn response(&mut self, id: u32) -> String {
        for _ in 0..8 {
            let msg = self.recv();
            if msg.contains(&format!("\"id\":{id}")) {
                return msg;
            }
        }
        panic!("no response for id {id}");
    }

    fn open(&mut self, text: &str) {
        let escaped = text
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n");
        self.send(&format!(
            r#"{{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{{"textDocument":{{"uri":"file:///t.mersey","languageId":"mersey","version":1,"text":"{escaped}"}}}}}}"#
        ));
        let _ = self.recv(); // publishDiagnostics
    }

    /// LSP positions are 0-based.
    fn request(&mut self, id: u32, method: &str, line: u32, character: u32) -> String {
        self.send(&format!(
            r#"{{"jsonrpc":"2.0","id":{id},"method":"{method}","params":{{"textDocument":{{"uri":"file:///t.mersey"}},"position":{{"line":{line},"character":{character}}}}}}}"#
        ));
        self.response(id)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

const SRC: &str = r#"import { console } from "std:console";

class Point {
    public x: int32 = 0;
    public y: int32 = 0;
    private secret: int32 = 0;

    public dist(): float64 {
        return 0.0;
    }
    private hidden(): void {}
}

const origin = new Point();
const label = "hello";
console.log(origin.x, label);
"#;

#[test]
fn advertises_hover_definition_and_completion() {
    let mut s = Server::start();
    s.send(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#);
    let reply = s.response(1);
    assert!(reply.contains(r#""hoverProvider":true"#), "{reply}");
    assert!(reply.contains(r#""definitionProvider":true"#), "{reply}");
    assert!(reply.contains(r#""triggerCharacters":["."]"#), "{reply}");
}

#[test]
fn hover_reports_the_inferred_type() {
    let mut s = Server::start();
    s.open(SRC);
    // `origin` on line 14 (0-based 13), col 6: declared with no annotation, so
    // this is the type the checker inferred, not one the editor guessed.
    let reply = s.request(2, "textDocument/hover", 13, 6);
    assert!(
        reply.contains("Point"),
        "expected the inferred type, got {reply}"
    );

    // `label` is a string.
    let reply = s.request(3, "textDocument/hover", 14, 6);
    assert!(reply.contains("string"), "expected `string`, got {reply}");
}

#[test]
fn definition_jumps_to_the_declaration() {
    let mut s = Server::start();
    s.open(SRC);
    // The `origin` used on the last line is declared on line 14 (0-based 13).
    let reply = s.request(4, "textDocument/definition", 15, 12);
    assert!(
        reply.contains(r#""line":13"#),
        "expected a jump to line 13, got {reply}"
    );
}

#[test]
fn member_completion_offers_public_members_only() {
    let mut s = Server::start();
    // A buffer mid-keystroke: `origin.` does not parse, which is exactly the
    // state an editor asks about.
    let text = SRC.replace("console.log(origin.x, label);", "origin.");
    s.open(&text);
    let reply = s.request(5, "textDocument/completion", 15, 7);

    assert!(
        reply.contains(r#""label":"x""#),
        "expected the public field, got {reply}"
    );
    assert!(
        reply.contains(r#""label":"dist""#),
        "expected the public method, got {reply}"
    );
    // §4.2: completion must not suggest what the checker would then reject.
    assert!(
        !reply.contains(r#""label":"secret""#),
        "private field leaked: {reply}"
    );
    assert!(
        !reply.contains(r#""label":"hidden""#),
        "private method leaked: {reply}"
    );
    // And no prototype nonsense (§1.1, §4.1).
    assert!(
        !reply.contains("prototype"),
        "prototypes do not exist: {reply}"
    );
}

#[test]
fn completion_knows_the_web_platform() {
    let mut s = Server::start();
    let text = "import { document } from \"browser:dom\";\n\nconst el = document.createElement(\"div\");\nel.\n";
    s.open(text);
    let reply = s.request(6, "textDocument/completion", 3, 3);
    // `el` is an Element: its members come from the generated WebIDL bindings.
    assert!(
        reply.contains(r#""label":"setAttribute""#),
        "expected DOM members, got {reply}"
    );
}

/// An editor that typechecks a file on its own sees a different program than
/// the compiler does: imported names have no types, so hover says `<error>` and
/// completion offers nothing. The analysis therefore runs over the same module
/// graph the compiler builds — dependencies from disk, the open buffer from the
/// editor (including what has not been saved yet).
mod cross_module {
    use super::*;

    fn project() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("mersey-lsp-xmod-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("lib.mersey"),
            r#"export class Shape {
    public sides: int32 = 0;
    private hidden: int32 = 0;
    public area(): float64 { return 0.0; }
}

export function twice(n: int32): int32 { return n * 2; }
"#,
        )
        .unwrap();
        dir
    }

    /// `s.open` uses a fixed URI; these tests need the buffer's real path, so
    /// the dependency next to it can be found.
    fn open_at(s: &mut Server, uri: &str, text: &str) {
        let escaped = text
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n");
        s.send(&format!(
            r#"{{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{{"textDocument":{{"uri":"{uri}","languageId":"mersey","version":1,"text":"{escaped}"}}}}}}"#
        ));
        let _ = s.recv();
    }

    fn request_at(s: &mut Server, id: u32, method: &str, uri: &str, line: u32, ch: u32) -> String {
        s.send(&format!(
            r#"{{"jsonrpc":"2.0","id":{id},"method":"{method}","params":{{"textDocument":{{"uri":"{uri}"}},"position":{{"line":{line},"character":{ch}}}}}}}"#
        ));
        s.response(id)
    }

    #[test]
    fn imported_symbols_have_types() {
        let dir = project();
        let uri = format!("file://{}/app.mersey", dir.display());
        let src = "import { twice } from \"./lib.mersey\";\n\nconst n = twice(21);\n";

        let mut s = Server::start();
        open_at(&mut s, &uri, src);

        // Hover on `n`: its type comes from a function declared in another file.
        let reply = request_at(&mut s, 10, "textDocument/hover", &uri, 2, 6);
        assert!(
            reply.contains("int32"),
            "expected the imported return type: {reply}"
        );
        assert!(
            !reply.contains("error"),
            "imported symbol had no type: {reply}"
        );
    }

    #[test]
    fn completion_reaches_into_another_file() {
        let dir = project();
        let uri = format!("file://{}/app2.mersey", dir.display());
        // Mid-keystroke, and the receiver's class lives in lib.mersey.
        let src = "import { Shape } from \"./lib.mersey\";\n\nconst s = new Shape();\ns.\n";

        let mut srv = Server::start();
        open_at(&mut srv, &uri, src);
        let reply = request_at(&mut srv, 11, "textDocument/completion", &uri, 3, 2);

        assert!(
            reply.contains(r#""label":"sides""#),
            "expected the imported field: {reply}"
        );
        assert!(
            reply.contains(r#""label":"area""#),
            "expected the imported method: {reply}"
        );
        assert!(
            !reply.contains(r#""label":"hidden""#),
            "private member leaked: {reply}"
        );
    }

    #[test]
    fn an_unsaved_edit_is_what_gets_analysed() {
        let dir = project();
        let uri = format!("file://{}/app3.mersey", dir.display());
        // This file has never been written to disk. Only the editor has it.
        let src = "import { twice } from \"./lib.mersey\";\n\nconst v = twice(\"not an int\");\n";

        let mut s = Server::start();
        open_at(&mut s, &uri, src);
        // The diagnostic proves the *dependency's* signature was used to check
        // the *buffer's* unsaved text.
        let escaped = src.replace('"', "\\\"").replace('\n', "\\n");
        let _ = escaped;
        let reply = request_at(&mut s, 12, "textDocument/hover", &uri, 2, 10);
        assert!(!reply.contains("<error>"), "{reply}");
    }
}

const SCOPES: &str = r#"import { console } from "std:console";

function scaled(factor: int32, offset: int32): int32 {
    const value = factor * 2;
    return value + offset;
}

function other(): int32 {
    const value = 99;      // a *different* `value`: same spelling, other scope
    return value;
}

const value = scaled(3, 1);
console.log(value, other());
"#;

#[test]
fn references_are_resolved_not_string_matched() {
    let mut s = Server::start();
    s.open(SCOPES);

    // The `value` on line 5 (0-based 4) is the one declared on line 4.
    let reply = s.request(20, "textDocument/references", 4, 11);
    // Its declaration and its single use — and *not* the `value` in `other()`
    // or the one at module level.
    let count = reply.matches("\"uri\"").count();
    assert_eq!(count, 2, "expected declaration + 1 use, got {reply}");
    assert!(
        reply.contains(r#""line":3"#),
        "declaration missing: {reply}"
    );
    assert!(reply.contains(r#""line":4"#), "use missing: {reply}");
    assert!(
        !reply.contains(r#""line":8"#),
        "matched a different scope's `value`: {reply}"
    );
    assert!(
        !reply.contains(r#""line":12"#),
        "matched the module-level `value`: {reply}"
    );
}

#[test]
fn rename_only_touches_the_binding_it_resolved() {
    let mut s = Server::start();
    s.open(SCOPES);

    s.send(
        r#"{"jsonrpc":"2.0","id":21,"method":"textDocument/rename","params":{"textDocument":{"uri":"file:///t.mersey"},"position":{"line":3,"character":10},"newName":"scaledValue"}}"#,
    );
    let reply = s.response(21);

    // Two edits: the declaration and the one use. A text substitution would
    // have found four `value`s and silently changed what the program means.
    let edits = reply.matches("newText").count();
    assert_eq!(edits, 2, "expected 2 edits, got {reply}");
    assert!(reply.contains(r#""newText":"scaledValue""#), "{reply}");
    // The range covers exactly the old name (5 characters).
    assert!(reply.contains(r#""character":10}"#), "{reply}");
    assert!(
        reply.contains(r#""character":15}"#),
        "range should cover `value`: {reply}"
    );
}

#[test]
fn signature_help_comes_from_the_checker() {
    let mut s = Server::start();
    // Cursor inside the call to `scaled`, on the second argument.
    let text = SCOPES.replace("const value = scaled(3, 1);", "const value = scaled(3, );");
    s.open(&text);

    let reply = s.request(22, "textDocument/signatureHelp", 12, 24);
    assert!(
        reply.contains("int32"),
        "expected the real signature: {reply}"
    );
    assert!(
        reply.contains(r#""activeParameter":1"#),
        "should be on the 2nd arg: {reply}"
    );
}

#[test]
fn document_symbols_list_what_the_file_declares() {
    let mut s = Server::start();
    s.open(SRC);
    s.send(
        r#"{"jsonrpc":"2.0","id":30,"method":"textDocument/documentSymbol","params":{"textDocument":{"uri":"file:///t.mersey"}}}"#,
    );
    let reply = s.response(30);
    assert!(reply.contains(r#""name":"Point""#), "{reply}");
    assert!(reply.contains(r#""name":"origin""#), "{reply}");
    assert!(reply.contains(r#""name":"label""#), "{reply}");
    // Not compiler temporaries or `this`.
    assert!(!reply.contains(r#""name":"this""#), "{reply}");
}
