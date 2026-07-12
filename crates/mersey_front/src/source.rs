//! Source decoding, spec §2.1: `.mersey` files are UTF-8 only. A UTF-8 BOM
//! is accepted and ignored. UTF-16/UTF-32 files are rejected with a
//! diagnostic naming the detected encoding. Invalid UTF-8 is a hard error,
//! never replaced with U+FFFD.

use crate::diag::{Code, Diagnostic, Pos};

pub struct SourceFile {
    pub name: String,
    pub text: String,
}

pub fn decode(name: &str, bytes: &[u8]) -> Result<SourceFile, Diagnostic> {
    let pos = Pos { line: 1, col: 1 };
    let detected = match bytes {
        [0xFF, 0xFE, 0x00, 0x00, ..] => Some("UTF-32LE"),
        [0x00, 0x00, 0xFE, 0xFF, ..] => Some("UTF-32BE"),
        [0xFF, 0xFE, ..] => Some("UTF-16LE"),
        [0xFE, 0xFF, ..] => Some("UTF-16BE"),
        _ => None,
    };
    if let Some(enc) = detected {
        return Err(Diagnostic::error(
            Code::WrongEncoding,
            format!(
                "{name}: source files must be UTF-8, but a {enc} byte-order mark \
                 was found; run `mersey convert` to transcode"
            ),
            pos,
        ));
    }

    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    match std::str::from_utf8(bytes) {
        Ok(text) => Ok(SourceFile { name: name.to_string(), text: text.to_string() }),
        Err(e) => {
            // Report the line/column of the offending byte, counting the
            // valid prefix; the prefix is guaranteed valid UTF-8.
            let valid = &bytes[..e.valid_up_to()];
            // SAFETY-free: valid_up_to guarantees this slice is UTF-8.
            let prefix = std::str::from_utf8(valid).expect("valid prefix");
            let pos = end_pos(prefix);
            Err(Diagnostic::error(
                Code::InvalidUtf8,
                format!("{name}: invalid UTF-8 byte sequence at offset {}", e.valid_up_to()),
                pos,
            ))
        }
    }
}

/// Position one past the end of `text` (1-based line, code-point column).
fn end_pos(text: &str) -> Pos {
    let mut line = 1u32;
    let mut col = 1u32;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                line += 1;
                col = 1;
            }
            '\n' | '\u{2028}' | '\u{2029}' => {
                line += 1;
                col = 1;
            }
            _ => col += 1,
        }
    }
    Pos { line, col }
}
