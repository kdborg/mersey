//! Language server (LSP over JSON-RPC on stdin/stdout).
//!
//! Scope: the diagnostics an editor most wants — full decode → lex → parse →
//! bind → typecheck on open/change, published as LSP diagnostics with exact
//! ranges and our stable error codes. Completion and hover are future work;
//! diagnostics are what turn "run the compiler in a terminal" into "the
//! editor tells me".

use std::io::{self, BufRead, Read, Write};

use mersey_front::{bind, check, parser, source};

pub fn serve() -> std::process::ExitCode {
    let stdin = io::stdin();
    let mut reader = stdin.lock();
    loop {
        let Some(msg) = read_message(&mut reader) else { break };
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
                r#"{{"jsonrpc":"2.0","id":{id},"result":{{"capabilities":{{"textDocumentSync":1}},"serverInfo":{{"name":"mersey-lsp","version":"0.1.0"}}}}}}"#
            ))
        }
        "textDocument/didOpen" | "textDocument/didChange" => {
            let uri = field(msg, "uri")?;
            // didOpen carries "text"; didChange carries it in the change.
            let text = unescape(field(msg, "text")?);
            Some(diagnostics_notification(uri, &text))
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
