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
