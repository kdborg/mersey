//! Language server (LSP over JSON-RPC on stdin/stdout).
//!
//! Diagnostics on open/change (full decode → lex → parse → bind → typecheck,
//! with exact ranges and our stable error codes), plus hover, go-to-definition
//! and completion.
//!
//! All four answers come from the checker itself rather than from a parallel
//! model of the language — `check::analyze` records what it already worked out
//! (the type of every expression, where every name was declared), and
//! completion asks the checker what a member access would resolve to. An
//! editor that suggests a member the compiler rejects is worse than one that
//! suggests nothing, so the two cannot be allowed to drift apart.

use std::collections::HashMap;
use std::io::{self, BufRead, Read, Write};
use std::sync::Mutex;

use mersey_front::check::COMPLETION_MARKER;
use mersey_front::diag::Pos;
use mersey_front::{bind, check, parser, source};

/// Open documents, so hover/definition/completion can be answered against the
/// buffer the editor actually has (they carry a position, not the text).
static DOCS: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);

fn put_doc(uri: &str, text: &str) {
    let mut docs = DOCS.lock().unwrap();
    docs.get_or_insert_with(HashMap::new)
        .insert(uri.to_string(), text.to_string());
}

fn get_doc(uri: &str) -> Option<String> {
    let docs = DOCS.lock().unwrap();
    docs.as_ref()?.get(uri).cloned()
}

pub fn serve() -> std::process::ExitCode {
    let stdin = io::stdin();
    let mut reader = stdin.lock();
    loop {
        let Some(msg) = read_message(&mut reader) else {
            break;
        };
        if let Some(response) = handle(&msg) {
            write_message(&response);
        }
    }
    std::process::ExitCode::SUCCESS
}

/// Read one `Content-Length`-framed JSON-RPC message.
fn read_message(reader: &mut impl BufRead) -> Option<String> {
    let mut len = 0usize;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some(v) = line.strip_prefix("Content-Length:") {
            len = v.trim().parse().ok()?;
        }
    }
    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf).ok()?;
    String::from_utf8(buf).ok()
}

fn write_message(body: &str) {
    let out = io::stdout();
    let mut out = out.lock();
    let _ = write!(out, "Content-Length: {}\r\n\r\n{}", body.len(), body);
    let _ = out.flush();
}

/// Minimal JSON field extraction — enough for the LSP messages we handle,
/// and dependency-free (the engine ships no JSON crate).
fn field<'a>(json: &'a str, key: &str) -> Option<&'a str> {
    let pat = format!("\"{key}\"");
    let i = json.find(&pat)? + pat.len();
    let rest = json[i..].trim_start();
    let rest = rest.strip_prefix(':')?.trim_start();
    if let Some(r) = rest.strip_prefix('"') {
        let end = find_string_end(r)?;
        Some(&r[..end])
    } else {
        let end = rest.find([',', '}']).unwrap_or(rest.len());
        Some(rest[..end].trim())
    }
}

fn find_string_end(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b'"' => return Some(i),
            _ => i += 1,
        }
    }
    None
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
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some('u') => {
                let hex: String = chars.by_ref().take(4).collect();
                if let Some(c) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                    out.push(c);
                }
            }
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

fn escape(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

fn handle(msg: &str) -> Option<String> {
    let method = field(msg, "method")?;
    match method {
        "initialize" => {
            let id = field(msg, "id").unwrap_or("1");
            Some(format!(
                r#"{{"jsonrpc":"2.0","id":{id},"result":{{"capabilities":{{"textDocumentSync":1,"hoverProvider":true,"definitionProvider":true,"completionProvider":{{"triggerCharacters":["."]}}}},"serverInfo":{{"name":"mersey-lsp","version":"0.1.0"}}}}}}"#
            ))
        }
        "textDocument/didOpen" | "textDocument/didChange" => {
            let uri = field(msg, "uri")?;
            // didOpen carries "text"; didChange carries it in the change.
            let text = unescape(field(msg, "text")?);
            put_doc(uri, &text);
            Some(diagnostics_notification(uri, &text))
        }
        "textDocument/hover" => {
            let id = field(msg, "id").unwrap_or("1");
            let uri = field(msg, "uri")?;
            let pos = request_pos(msg)?;
            let result = get_doc(uri)
                .and_then(|text| hover(&text, pos))
                .map(|h| {
                    format!(
                        r#"{{"contents":{{"kind":"markdown","value":"```mersey\n{}\n```"}}}}"#,
                        escape(&h)
                    )
                })
                .unwrap_or_else(|| "null".to_string());
            Some(format!(
                r#"{{"jsonrpc":"2.0","id":{id},"result":{result}}}"#
            ))
        }
        "textDocument/definition" => {
            let id = field(msg, "id").unwrap_or("1");
            let uri = field(msg, "uri")?;
            let pos = request_pos(msg)?;
            let result = get_doc(uri)
                .and_then(|text| definition(&text, pos))
                .map(|d| {
                    // LSP is 0-based; our positions are 1-based code points.
                    let line = d.line.saturating_sub(1);
                    let col = d.col.saturating_sub(1);
                    format!(
                        r#"{{"uri":"{}","range":{{"start":{{"line":{line},"character":{col}}},"end":{{"line":{line},"character":{col}}}}}}}"#,
                        escape(uri)
                    )
                })
                .unwrap_or_else(|| "null".to_string());
            Some(format!(
                r#"{{"jsonrpc":"2.0","id":{id},"result":{result}}}"#
            ))
        }
        "textDocument/completion" => {
            let id = field(msg, "id").unwrap_or("1");
            let uri = field(msg, "uri")?;
            let pos = request_pos(msg)?;
            let items = get_doc(uri)
                .map(|text| complete(&text, pos))
                .unwrap_or_default();
            let items: Vec<String> = items
                .iter()
                .map(|c| {
                    format!(
                        r#"{{"label":"{}","kind":{},"detail":"{}"}}"#,
                        escape(&c.label),
                        c.kind,
                        escape(&c.detail)
                    )
                })
                .collect();
            Some(format!(
                r#"{{"jsonrpc":"2.0","id":{id},"result":{{"isIncomplete":false,"items":[{}]}}}}"#,
                items.join(",")
            ))
        }
        "shutdown" => {
            let id = field(msg, "id").unwrap_or("1");
            Some(format!(r#"{{"jsonrpc":"2.0","id":{id},"result":null}}"#))
        }
        _ => None,
    }
}

/// Run the whole frontend and publish the diagnostics.
fn diagnostics_notification(uri: &str, text: &str) -> String {
    let mut items: Vec<String> = Vec::new();
    match source::decode(uri, text.as_bytes()) {
        Err(d) => items.push(lsp_diagnostic(&d)),
        Ok(src) => {
            let parsed = parser::parse(&src);
            let mut diags = parsed.diagnostics;
            if diags.is_empty() {
                diags = bind::bind(&parsed.module).diagnostics;
            }
            if diags.is_empty() {
                diags = check::check(&parsed.module).diagnostics;
            }
            for d in &diags {
                items.push(lsp_diagnostic(d));
            }
        }
    }
    format!(
        r#"{{"jsonrpc":"2.0","method":"textDocument/publishDiagnostics","params":{{"uri":"{}","diagnostics":[{}]}}}}"#,
        escape(uri),
        items.join(",")
    )
}

fn lsp_diagnostic(d: &mersey_front::diag::Diagnostic) -> String {
    // LSP positions are 0-based; ours are 1-based code-point positions.
    let line = d.pos.line.saturating_sub(1);
    let col = d.pos.col.saturating_sub(1);
    format!(
        r#"{{"range":{{"start":{{"line":{line},"character":{col}}},"end":{{"line":{line},"character":{}}}}},"severity":1,"code":"{}","source":"mersey","message":"{}"}}"#,
        col + 1,
        d.code.as_str(),
        escape(&d.message)
    )
}

/// The `position` of a request, as our 1-based code-point Pos.
fn request_pos(msg: &str) -> Option<Pos> {
    let line: u32 = field(msg, "line")?.parse().ok()?;
    let col: u32 = field(msg, "character")?.parse().ok()?;
    Some(Pos {
        line: line + 1,
        col: col + 1,
    })
}

/// The checked module, or None if it does not even parse.
fn analyze(text: &str) -> Option<check::Analysis> {
    let src = source::decode("<buffer>", text.as_bytes()).ok()?;
    let parsed = parser::parse(&src);
    if !parsed.diagnostics.is_empty() {
        return None;
    }
    // The AST must outlive the analysis; an editor session is short and this
    // is bounded by keystrokes that produce a *parsing* buffer.
    let module: &'static _ = Box::leak(Box::new(parsed.module));
    Some(check::analyze(module))
}

fn hover(text: &str, pos: Pos) -> Option<String> {
    let a = analyze(text)?;
    // The cursor is rarely on the first character of the name it is in, so
    // hover asks about the token under it, not the exact column.
    let start = token_start(text, pos)?;
    a.hover(start)
}

fn definition(text: &str, pos: Pos) -> Option<Pos> {
    let a = analyze(text)?;
    let start = token_start(text, pos)?;
    a.definition(start)
}

/// Completion. After a `.` this is a *member* completion, which needs the
/// receiver's type — so the buffer is repaired into something that parses
/// (`foo.` → `foo.MERSEY__COMPLETE`) and the checker reports what the receiver
/// turned out to be. Otherwise it is the names in scope.
fn complete(text: &str, pos: Pos) -> Vec<check::Completion> {
    if let Some(repaired) = repair_member_access(text, pos) {
        if let Ok(src) = source::decode("<buffer>", repaired.as_bytes()) {
            let parsed = parser::parse(&src);
            if parsed.diagnostics.is_empty() {
                let module: &'static _ = Box::leak(Box::new(parsed.module));
                let items = check::member_completions(module);
                if !items.is_empty() {
                    return items;
                }
            }
        }
        // A dot with no resolvable receiver: suggesting locals here would be
        // wrong (they are not valid after a `.`), so suggest nothing.
        return Vec::new();
    }
    match analyze(text) {
        Some(a) => a.scope_completions(pos),
        None => Vec::new(),
    }
}

/// If the cursor sits just after a `.` (possibly with a partial member name
/// typed), rewrite that member name to the completion marker.
fn repair_member_access(text: &str, pos: Pos) -> Option<String> {
    let lines: Vec<&str> = text.split('\n').collect();
    let line = *lines.get(pos.line.checked_sub(1)? as usize)?;
    let chars: Vec<char> = line.chars().collect();
    let cursor = (pos.col.checked_sub(1)? as usize).min(chars.len());

    // Walk back over the partial member name the user has typed so far.
    let mut i = cursor;
    while i > 0 && (chars[i - 1].is_alphanumeric() || chars[i - 1] == '_') {
        i -= 1;
    }
    if i == 0 || chars[i - 1] != '.' {
        return None;
    }
    // Also drop the rest of the identifier to the right of the cursor.
    let mut j = cursor;
    while j < chars.len() && (chars[j].is_alphanumeric() || chars[j] == '_') {
        j += 1;
    }

    let mut repaired_line: String = chars[..i].iter().collect();
    repaired_line.push_str(COMPLETION_MARKER);
    let rest: String = chars[j..].iter().collect();
    // Mid-keystroke, the statement the user is typing has no terminator yet,
    // and the parser rightly insists on one.
    if rest.trim().is_empty() {
        repaired_line.push(';');
    }
    repaired_line.push_str(&rest);

    let mut out: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
    out[pos.line as usize - 1] = repaired_line;
    Some(out.join("\n"))
}

/// The start of the identifier the cursor is inside (or on the edge of), which
/// is the position the checker recorded a type against.
fn token_start(text: &str, pos: Pos) -> Option<Pos> {
    let lines: Vec<&str> = text.split('\n').collect();
    let line = *lines.get(pos.line.checked_sub(1)? as usize)?;
    let chars: Vec<char> = line.chars().collect();
    let mut i = (pos.col.checked_sub(1)? as usize).min(chars.len());
    // A cursor just past the end of a name still means that name.
    if i > 0 && i == chars.len() {
        i -= 1;
    }
    if i < chars.len() && !(chars[i].is_alphanumeric() || chars[i] == '_') && i > 0 {
        i -= 1;
    }
    while i > 0 && (chars[i - 1].is_alphanumeric() || chars[i - 1] == '_') {
        i -= 1;
    }
    Some(Pos {
        line: pos.line,
        col: i as u32 + 1,
    })
}
