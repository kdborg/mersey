//! Minimal JSON codec for the universal web bridge (no dependencies).
//! Requests/replies are small and flat; this handles full JSON anyway so
//! event payloads and dictionary arguments survive round trips.

#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Json::Num(n) => Some(*n),
            _ => None,
        }
    }
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }
}

pub fn write(out: &mut String, v: &Json) {
    match v {
        Json::Null => out.push_str("null"),
        Json::Bool(true) => out.push_str("true"),
        Json::Bool(false) => out.push_str("false"),
        Json::Num(n) => {
            if n.is_finite() {
                out.push_str(&format!("{n}"));
            } else {
                out.push_str("null");
            }
        }
        Json::Str(s) => write_str(out, s),
        Json::Arr(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write(out, item);
            }
            out.push(']');
        }
        Json::Obj(fields) => {
            out.push('{');
            for (i, (k, val)) in fields.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_str(out, k);
                out.push(':');
                write(out, val);
            }
            out.push('}');
        }
    }
}

pub fn write_str(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        write_char(out, c);
    }
    out.push('"');
}

/// The escape of one scalar into an already-open JSON string. Factored out so
/// the UTF-8 and UTF-16 writers escape identically.
#[inline]
fn write_char(out: &mut String, c: char) {
    match c {
        '"' => out.push_str("\\\""),
        '\\' => out.push_str("\\\\"),
        '\n' => out.push_str("\\n"),
        '\r' => out.push_str("\\r"),
        '\t' => out.push_str("\\t"),
        '\u{8}' => out.push_str("\\b"),
        '\u{c}' => out.push_str("\\f"),
        c if (c as u32) < 0x20 => {
            const HEX: &[u8; 16] = b"0123456789abcdef";
            let n = c as u32;
            out.push_str("\\u00");
            out.push(HEX[((n >> 4) & 0xf) as usize] as char);
            out.push(HEX[(n & 0xf) as usize] as char);
        }
        c => out.push(c),
    }
}

/// Escape a UTF-16 string straight into `out`, without first materializing it
/// as a `String`. The JSON hot path (`pure_json`) holds its strings as the
/// engine's own `&[u16]`; this saves the per-string UTF-8 allocation that
/// `utf16_to_string` would make. Lone surrogates become U+FFFD, matching the
/// lossy decode the old path used.
pub fn write_str_u16(out: &mut String, s: &[u16]) {
    out.push('"');
    for c in char::decode_utf16(s.iter().copied()) {
        write_char(out, c.unwrap_or('\u{FFFD}'));
    }
    out.push('"');
}

pub fn parse(text: &str) -> Option<Json> {
    let mut p = P {
        chars: text.chars().collect(),
        i: 0,
    };
    let v = p.value()?;
    p.ws();
    if p.i == p.chars.len() {
        Some(v)
    } else {
        None
    }
}

struct P {
    chars: Vec<char>,
    i: usize,
}

impl P {
    fn ws(&mut self) {
        while matches!(self.chars.get(self.i), Some(' ' | '\t' | '\n' | '\r')) {
            self.i += 1;
        }
    }
    fn eat(&mut self, c: char) -> bool {
        self.ws();
        if self.chars.get(self.i) == Some(&c) {
            self.i += 1;
            true
        } else {
            false
        }
    }
    fn lit(&mut self, s: &str) -> bool {
        let n = s.chars().count();
        if self.chars[self.i..].iter().take(n).collect::<String>() == s {
            self.i += n;
            true
        } else {
            false
        }
    }
    fn value(&mut self) -> Option<Json> {
        self.ws();
        match *self.chars.get(self.i)? {
            'n' => self.lit("null").then_some(Json::Null),
            't' => self.lit("true").then_some(Json::Bool(true)),
            'f' => self.lit("false").then_some(Json::Bool(false)),
            '"' => self.string().map(Json::Str),
            '[' => {
                self.i += 1;
                let mut items = Vec::new();
                self.ws();
                if self.eat(']') {
                    return Some(Json::Arr(items));
                }
                loop {
                    items.push(self.value()?);
                    if self.eat(']') {
                        return Some(Json::Arr(items));
                    }
                    if !self.eat(',') {
                        return None;
                    }
                }
            }
            '{' => {
                self.i += 1;
                let mut fields = Vec::new();
                self.ws();
                if self.eat('}') {
                    return Some(Json::Obj(fields));
                }
                loop {
                    self.ws();
                    let k = self.string()?;
                    if !self.eat(':') {
                        return None;
                    }
                    fields.push((k, self.value()?));
                    if self.eat('}') {
                        return Some(Json::Obj(fields));
                    }
                    if !self.eat(',') {
                        return None;
                    }
                }
            }
            _ => self.number(),
        }
    }
    fn string(&mut self) -> Option<String> {
        self.ws();
        if self.chars.get(self.i) != Some(&'"') {
            return None;
        }
        self.i += 1;
        let mut out = String::new();
        loop {
            let c = *self.chars.get(self.i)?;
            self.i += 1;
            match c {
                '"' => return Some(out),
                '\\' => {
                    let e = *self.chars.get(self.i)?;
                    self.i += 1;
                    match e {
                        'n' => out.push('\n'),
                        'r' => out.push('\r'),
                        't' => out.push('\t'),
                        'b' => out.push('\u{8}'),
                        'f' => out.push('\u{c}'),
                        'u' => {
                            let hex: String = self.chars.get(self.i..self.i + 4)?.iter().collect();
                            self.i += 4;
                            let n = u32::from_str_radix(&hex, 16).ok()?;
                            // Surrogate pairs from JS strings.
                            if (0xD800..0xDC00).contains(&n) {
                                if self.chars.get(self.i) == Some(&'\\')
                                    && self.chars.get(self.i + 1) == Some(&'u')
                                {
                                    let hex2: String =
                                        self.chars.get(self.i + 2..self.i + 6)?.iter().collect();
                                    let n2 = u32::from_str_radix(&hex2, 16).ok()?;
                                    if (0xDC00..0xE000).contains(&n2) {
                                        self.i += 6;
                                        let cp = 0x10000 + ((n - 0xD800) << 10) + (n2 - 0xDC00);
                                        out.push(char::from_u32(cp)?);
                                        continue;
                                    }
                                }
                                out.push('\u{FFFD}');
                            } else {
                                out.push(char::from_u32(n).unwrap_or('\u{FFFD}'));
                            }
                        }
                        other => out.push(other),
                    }
                }
                c => out.push(c),
            }
        }
    }
    fn number(&mut self) -> Option<Json> {
        let start = self.i;
        while matches!(
            self.chars.get(self.i),
            Some('0'..='9' | '-' | '+' | '.' | 'e' | 'E')
        ) {
            self.i += 1;
        }
        let s: String = self.chars[start..self.i].iter().collect();
        s.parse().ok().map(Json::Num)
    }
}
