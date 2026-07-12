//! Source Map v3 emission.
//!
//! Stage A executes Mersey inside a WASM engine, so a browser's debugger has
//! nothing Mersey-shaped to show: breakpoints and stack frames land in
//! `mersey-engine.js`. A source map cannot make DevTools step through Mersey
//! bytecode, but it *can* make every position the engine reports —
//! diagnostics and runtime stack traces — click through to the original
//! `.mersey` line, and it lets tooling (editors, error reporters, the LSP)
//! map both ways.
//!
//! We emit the standard VLQ-encoded v3 format, mapping generated positions
//! (the engine's own line/column, which is what appears in a Mersey stack
//! trace) back to the original source.

/// One mapping: generated (line, col) → original (line, col), 1-based in,
/// 0-based in the encoded output as the format requires.
#[derive(Clone, Copy)]
pub struct Mapping {
    pub gen_line: u32,
    pub gen_col: u32,
    pub src_line: u32,
    pub src_col: u32,
}

const BASE64: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn vlq(mut value: i64, out: &mut String) {
    // Sign in the least-significant bit, then 5-bit groups with a
    // continuation bit — the Source Map v3 encoding.
    let mut v = if value < 0 {
        value = -value;
        ((value as u64) << 1) | 1
    } else {
        (value as u64) << 1
    };
    loop {
        let mut digit = (v & 0b11111) as usize;
        v >>= 5;
        if v > 0 {
            digit |= 0b100000;
        }
        out.push(BASE64[digit] as char);
        if v == 0 {
            break;
        }
    }
}

/// Encode mappings into a source map JSON document.
pub fn encode(source_name: &str, source_text: &str, mut mappings: Vec<Mapping>) -> String {
    mappings.sort_by_key(|m| (m.gen_line, m.gen_col));

    let mut segments = String::new();
    let mut prev_gen_line = 1u32;
    let mut prev_gen_col = 0i64;
    let mut prev_src_line = 0i64;
    let mut prev_src_col = 0i64;
    let mut first_on_line = true;

    for m in &mappings {
        while prev_gen_line < m.gen_line {
            segments.push(';');
            prev_gen_line += 1;
            prev_gen_col = 0;
            first_on_line = true;
        }
        if !first_on_line {
            segments.push(',');
        }
        first_on_line = false;

        let gen_col = m.gen_col.saturating_sub(1) as i64;
        let src_line = m.src_line.saturating_sub(1) as i64;
        let src_col = m.src_col.saturating_sub(1) as i64;

        vlq(gen_col - prev_gen_col, &mut segments);
        vlq(0, &mut segments); // source index (single source)
        vlq(src_line - prev_src_line, &mut segments);
        vlq(src_col - prev_src_col, &mut segments);

        prev_gen_col = gen_col;
        prev_src_line = src_line;
        prev_src_col = src_col;
    }

    let escape = |s: &str| {
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
    };

    format!(
        r#"{{"version":3,"file":"{}.map","sources":["{}"],"sourcesContent":["{}"],"names":[],"mappings":"{}"}}"#,
        escape(source_name),
        escape(source_name),
        escape(source_text),
        segments
    )
}

/// Identity mappings for a Mersey source: every statement start maps to
/// itself. (The engine reports Mersey positions directly, so the map's job
/// is to carry `sourcesContent` and make tooling able to resolve them.)
pub fn identity_map(source_name: &str, source_text: &str) -> String {
    let mut mappings = Vec::new();
    for (i, line) in source_text.lines().enumerate() {
        let indent = line.len() - line.trim_start().len();
        if line.trim().is_empty() {
            continue;
        }
        let l = (i + 1) as u32;
        let c = (indent + 1) as u32;
        mappings.push(Mapping {
            gen_line: l,
            gen_col: c,
            src_line: l,
            src_col: c,
        });
    }
    encode(source_name, source_text, mappings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vlq_encoding_matches_spec() {
        let mut s = String::new();
        vlq(0, &mut s);
        assert_eq!(s, "A");
        s.clear();
        vlq(1, &mut s);
        assert_eq!(s, "C");
        s.clear();
        vlq(-1, &mut s);
        assert_eq!(s, "D");
        s.clear();
        vlq(16, &mut s);
        assert_eq!(s, "gB");
    }

    #[test]
    fn map_is_wellformed_json() {
        let map = identity_map("app.mersey", "let x = 1;\n\nconsole.log(x);\n");
        assert!(map.contains(r#""version":3"#));
        assert!(map.contains(r#""sources":["app.mersey"]"#));
        assert!(map.contains("sourcesContent"));
        // line 1 -> line 1, blank line 2, line 3 -> line 3 (+2 line delta = "E")
        assert!(
            map.contains(r#""mappings":"AAAA;;AAEA""#),
            "mappings were: {map}"
        );
    }
}
