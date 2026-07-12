//! Regular expressions: a compact backtracking engine.
//!
//! Supported: literals, `.`, character classes `[a-z]` / `[^…]`, escapes
//! (`\d \D \w \W \s \S \b \B` and the usual `\n \t …`), anchors `^ $`,
//! groups `(…)` (capturing) and `(?:…)` (not), alternation `|`, and the
//! quantifiers `* + ? {n} {n,} {n,m}` in greedy and lazy (`?`-suffixed)
//! forms.
//!
//! Matching is over **code points**, not bytes (spec §3.4), so `.` and
//! character classes behave the way a Mersey string indexes.
//!
//! Not supported (and rejected at compile time rather than silently
//! mis-matched): lookaround, backreferences, named groups, unicode property
//! classes.

#[derive(Debug, Clone)]
enum Node {
    Empty,
    Char(char),
    Any,
    Class {
        negated: bool,
        items: Vec<ClassItem>,
    },
    Start,
    End,
    WordBoundary(bool),
    Group {
        index: Option<usize>,
        inner: Box<Node>,
    },
    Concat(Vec<Node>),
    Alt(Vec<Node>),
    Repeat {
        inner: Box<Node>,
        min: u32,
        max: u32,
        greedy: bool,
    },
}

#[derive(Debug, Clone)]
enum ClassItem {
    Ch(char),
    Range(char, char),
    Digit(bool),
    Word(bool),
    Space(bool),
}

pub struct Regex {
    root: Node,
    pub group_count: usize,
    ignore_case: bool,
}

pub struct Match {
    pub start: usize,
    pub end: usize,
    /// Capture groups, 1-based; `None` when the group did not participate.
    pub groups: Vec<Option<(usize, usize)>>,
}

impl Regex {
    /// Compile a pattern. `flags` may contain `i` (ignore case).
    pub fn new(pattern: &str, flags: &str) -> Result<Regex, String> {
        let ignore_case = flags.contains('i');
        for f in flags.chars() {
            if f != 'i' && f != 'g' {
                return Err(format!("unknown regex flag `{f}`"));
            }
        }
        let mut p = Parser {
            chars: pattern.chars().collect(),
            i: 0,
            groups: 0,
        };
        let root = p.alternation()?;
        if p.i != p.chars.len() {
            return Err(format!("unexpected `{}` in pattern", p.chars[p.i]));
        }
        Ok(Regex {
            root,
            group_count: p.groups,
            ignore_case,
        })
    }

    /// First match at or after `from` (a code-point index).
    pub fn find_at(&self, text: &[char], from: usize) -> Option<Match> {
        for start in from..=text.len() {
            let mut caps: Vec<Option<(usize, usize)>> = vec![None; self.group_count];
            let mut ctx = Ctx {
                text,
                caps: &mut caps,
                ignore_case: self.ignore_case,
                steps: 0,
            };
            if let Some(end) = match_node(&self.root, &mut ctx, start, &mut |_, pos| Some(pos)) {
                return Some(Match {
                    start,
                    end,
                    groups: caps,
                });
            }
        }
        None
    }

    pub fn is_match(&self, text: &[char]) -> bool {
        self.find_at(text, 0).is_some()
    }
}

// ---- parsing ------------------------------------------------------------------

struct Parser {
    chars: Vec<char>,
    i: usize,
    groups: usize,
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.i).copied()
    }
    fn eat(&mut self, c: char) -> bool {
        if self.peek() == Some(c) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn alternation(&mut self) -> Result<Node, String> {
        let mut arms = vec![self.concat()?];
        while self.eat('|') {
            arms.push(self.concat()?);
        }
        Ok(if arms.len() == 1 {
            arms.pop().expect("one")
        } else {
            Node::Alt(arms)
        })
    }

    fn concat(&mut self) -> Result<Node, String> {
        let mut parts = Vec::new();
        while let Some(c) = self.peek() {
            if c == '|' || c == ')' {
                break;
            }
            parts.push(self.quantified()?);
        }
        Ok(match parts.len() {
            0 => Node::Empty,
            1 => parts.pop().expect("one"),
            _ => Node::Concat(parts),
        })
    }

    fn quantified(&mut self) -> Result<Node, String> {
        let atom = self.atom()?;
        let (min, max) = match self.peek() {
            Some('*') => {
                self.i += 1;
                (0, u32::MAX)
            }
            Some('+') => {
                self.i += 1;
                (1, u32::MAX)
            }
            Some('?') => {
                self.i += 1;
                (0, 1)
            }
            Some('{') => {
                // {n} {n,} {n,m} — otherwise a literal brace.
                let save = self.i;
                self.i += 1;
                match self.bounds() {
                    Some(b) => b,
                    None => {
                        self.i = save;
                        return Ok(atom);
                    }
                }
            }
            _ => return Ok(atom),
        };
        let greedy = !self.eat('?');
        Ok(Node::Repeat {
            inner: Box::new(atom),
            min,
            max,
            greedy,
        })
    }

    fn bounds(&mut self) -> Option<(u32, u32)> {
        let mut n = String::new();
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            n.push(self.chars[self.i]);
            self.i += 1;
        }
        if n.is_empty() {
            return None;
        }
        let min: u32 = n.parse().ok()?;
        if self.eat('}') {
            return Some((min, min));
        }
        if !self.eat(',') {
            return None;
        }
        if self.eat('}') {
            return Some((min, u32::MAX));
        }
        let mut m = String::new();
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            m.push(self.chars[self.i]);
            self.i += 1;
        }
        let max: u32 = m.parse().ok()?;
        if !self.eat('}') {
            return None;
        }
        Some((min, max))
    }

    fn atom(&mut self) -> Result<Node, String> {
        let c = self.peek().ok_or("unexpected end of pattern")?;
        self.i += 1;
        Ok(match c {
            '.' => Node::Any,
            '^' => Node::Start,
            '$' => Node::End,
            '(' => {
                let index = if self.chars.get(self.i) == Some(&'?') {
                    if self.chars.get(self.i + 1) == Some(&':') {
                        self.i += 2;
                        None
                    } else {
                        return Err("lookaround and named groups are not supported".into());
                    }
                } else {
                    self.groups += 1;
                    Some(self.groups)
                };
                let inner = self.alternation()?;
                if !self.eat(')') {
                    return Err("unclosed group".into());
                }
                Node::Group {
                    index,
                    inner: Box::new(inner),
                }
            }
            '[' => self.class()?,
            '\\' => self.escape()?,
            ')' => return Err("unmatched `)`".into()),
            '*' | '+' | '?' => return Err(format!("nothing to repeat before `{c}`")),
            c => Node::Char(c),
        })
    }

    fn escape(&mut self) -> Result<Node, String> {
        let c = self.peek().ok_or("trailing backslash")?;
        self.i += 1;
        Ok(match c {
            'd' => Node::Class {
                negated: false,
                items: vec![ClassItem::Digit(true)],
            },
            'D' => Node::Class {
                negated: false,
                items: vec![ClassItem::Digit(false)],
            },
            'w' => Node::Class {
                negated: false,
                items: vec![ClassItem::Word(true)],
            },
            'W' => Node::Class {
                negated: false,
                items: vec![ClassItem::Word(false)],
            },
            's' => Node::Class {
                negated: false,
                items: vec![ClassItem::Space(true)],
            },
            'S' => Node::Class {
                negated: false,
                items: vec![ClassItem::Space(false)],
            },
            'b' => Node::WordBoundary(true),
            'B' => Node::WordBoundary(false),
            'n' => Node::Char('\n'),
            'r' => Node::Char('\r'),
            't' => Node::Char('\t'),
            '0' => Node::Char('\0'),
            '1'..='9' => return Err("backreferences are not supported".into()),
            c => Node::Char(c),
        })
    }

    fn class(&mut self) -> Result<Node, String> {
        let negated = self.eat('^');
        let mut items = Vec::new();
        let mut first = true;
        loop {
            let c = self.peek().ok_or("unclosed character class")?;
            if c == ']' && !first {
                self.i += 1;
                break;
            }
            first = false;
            self.i += 1;
            let lo = if c == '\\' {
                let e = self.peek().ok_or("trailing backslash")?;
                self.i += 1;
                match e {
                    'd' => {
                        items.push(ClassItem::Digit(true));
                        continue;
                    }
                    'D' => {
                        items.push(ClassItem::Digit(false));
                        continue;
                    }
                    'w' => {
                        items.push(ClassItem::Word(true));
                        continue;
                    }
                    'W' => {
                        items.push(ClassItem::Word(false));
                        continue;
                    }
                    's' => {
                        items.push(ClassItem::Space(true));
                        continue;
                    }
                    'S' => {
                        items.push(ClassItem::Space(false));
                        continue;
                    }
                    'n' => '\n',
                    'r' => '\r',
                    't' => '\t',
                    other => other,
                }
            } else {
                c
            };
            // A range?
            if self.peek() == Some('-') && self.chars.get(self.i + 1).is_some_and(|c| *c != ']') {
                self.i += 1;
                let hi = self.peek().ok_or("unclosed character class")?;
                self.i += 1;
                let hi = if hi == '\\' {
                    let e = self.peek().ok_or("trailing backslash")?;
                    self.i += 1;
                    e
                } else {
                    hi
                };
                items.push(ClassItem::Range(lo, hi));
            } else {
                items.push(ClassItem::Ch(lo));
            }
        }
        Ok(Node::Class { negated, items })
    }
}

// ---- matching -----------------------------------------------------------------

struct Ctx<'a> {
    text: &'a [char],
    caps: &'a mut Vec<Option<(usize, usize)>>,
    ignore_case: bool,
    steps: u32,
}

/// Backtracking limit — hostile patterns must fail, not hang.
const MAX_STEPS: u32 = 2_000_000;

fn eq(a: char, b: char, ignore_case: bool) -> bool {
    a == b || (ignore_case && a.to_lowercase().eq(b.to_lowercase()))
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn class_matches(negated: bool, items: &[ClassItem], c: char, ignore_case: bool) -> bool {
    let mut hit = items.iter().any(|item| match item {
        ClassItem::Ch(x) => eq(*x, c, ignore_case),
        ClassItem::Range(lo, hi) => {
            (*lo..=*hi).contains(&c)
                || (ignore_case && c.to_lowercase().any(|lc| (*lo..=*hi).contains(&lc))
                    || ignore_case && c.to_uppercase().any(|uc| (*lo..=*hi).contains(&uc)))
        }
        ClassItem::Digit(want) => c.is_ascii_digit() == *want,
        ClassItem::Word(want) => is_word(c) == *want,
        ClassItem::Space(want) => c.is_whitespace() == *want,
    });
    if negated {
        hit = !hit;
    }
    hit
}

/// Continuation-passing matcher: `k` is "what to do with the rest".
type Cont<'k> = dyn FnMut(&mut Ctx, usize) -> Option<usize> + 'k;

fn match_node(node: &Node, ctx: &mut Ctx, pos: usize, k: &mut Cont) -> Option<usize> {
    ctx.steps += 1;
    if ctx.steps > MAX_STEPS {
        return None; // catastrophic backtracking: give up rather than hang
    }
    match node {
        Node::Empty => k(ctx, pos),
        Node::Char(c) => {
            if pos < ctx.text.len() && eq(*c, ctx.text[pos], ctx.ignore_case) {
                k(ctx, pos + 1)
            } else {
                None
            }
        }
        Node::Any => {
            if pos < ctx.text.len() && ctx.text[pos] != '\n' {
                k(ctx, pos + 1)
            } else {
                None
            }
        }
        Node::Class { negated, items } => {
            if pos < ctx.text.len()
                && class_matches(*negated, items, ctx.text[pos], ctx.ignore_case)
            {
                k(ctx, pos + 1)
            } else {
                None
            }
        }
        Node::Start => {
            if pos == 0 {
                k(ctx, pos)
            } else {
                None
            }
        }
        Node::End => {
            if pos == ctx.text.len() {
                k(ctx, pos)
            } else {
                None
            }
        }
        Node::WordBoundary(want) => {
            let before = pos > 0 && is_word(ctx.text[pos - 1]);
            let after = pos < ctx.text.len() && is_word(ctx.text[pos]);
            if (before != after) == *want {
                k(ctx, pos)
            } else {
                None
            }
        }
        Node::Group { index, inner } => {
            let idx = *index;
            let saved = idx.and_then(|i| ctx.caps[i - 1]);
            let start = pos;
            let out = match_node(inner, ctx, pos, &mut |ctx: &mut Ctx, end: usize| {
                if let Some(i) = idx {
                    ctx.caps[i - 1] = Some((start, end));
                }
                k(ctx, end)
            });
            if out.is_none() {
                if let Some(i) = idx {
                    ctx.caps[i - 1] = saved; // undo on backtrack
                }
            }
            out
        }
        Node::Concat(parts) => match_seq(parts, ctx, pos, k),
        Node::Alt(arms) => {
            for arm in arms {
                if let Some(end) = match_node(arm, ctx, pos, k) {
                    return Some(end);
                }
            }
            None
        }
        Node::Repeat {
            inner,
            min,
            max,
            greedy,
        } => match_repeat(inner, *min, *max, *greedy, ctx, pos, 0, k),
    }
}

fn match_seq(parts: &[Node], ctx: &mut Ctx, pos: usize, k: &mut Cont) -> Option<usize> {
    match parts.split_first() {
        None => k(ctx, pos),
        Some((head, tail)) => match_node(head, ctx, pos, &mut |ctx: &mut Ctx, p: usize| {
            match_seq(tail, ctx, p, k)
        }),
    }
}

#[allow(clippy::too_many_arguments)]
fn match_repeat(
    inner: &Node,
    min: u32,
    max: u32,
    greedy: bool,
    ctx: &mut Ctx,
    pos: usize,
    done: u32,
    k: &mut Cont,
) -> Option<usize> {
    let can_more = done < max;
    let can_stop = done >= min;

    if greedy {
        if can_more {
            let more = match_node(inner, ctx, pos, &mut |ctx: &mut Ctx, p: usize| {
                if p == pos {
                    return None; // empty iteration: stop, or we'd loop forever
                }
                match_repeat(inner, min, max, greedy, ctx, p, done + 1, k)
            });
            if let Some(end) = more {
                return Some(end);
            }
        }
        if can_stop {
            return k(ctx, pos);
        }
        None
    } else {
        if can_stop {
            if let Some(end) = k(ctx, pos) {
                return Some(end);
            }
        }
        if !can_more {
            return None;
        }
        match_node(inner, ctx, pos, &mut |ctx: &mut Ctx, p: usize| {
            if p == pos {
                return None;
            }
            match_repeat(inner, min, max, greedy, ctx, p, done + 1, k)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(pattern: &str, text: &str) -> Option<(usize, usize)> {
        let re = Regex::new(pattern, "").expect("compile");
        let chars: Vec<char> = text.chars().collect();
        re.find_at(&chars, 0).map(|m| (m.start, m.end))
    }

    #[test]
    fn basics() {
        assert_eq!(m("a+b", "xaaab"), Some((1, 5)));
        assert_eq!(m("^abc$", "abc"), Some((0, 3)));
        assert_eq!(m("^abc$", "abcd"), None);
        assert_eq!(m("a|bc", "zbc"), Some((1, 3)));
        assert_eq!(m("[a-c]+", "xxbca!"), Some((2, 5)));
        assert_eq!(m("[^a-c]+", "abcxy"), Some((3, 5)));
        assert_eq!(m(r"\d{2,3}", "ab1234"), Some((2, 5)));
        assert_eq!(m("a.c", "a\nc"), None); // `.` does not cross a newline
        assert_eq!(m(r"\bword\b", "a word here"), Some((2, 6)));
    }

    #[test]
    fn greedy_vs_lazy() {
        assert_eq!(m("<.*>", "<a><b>"), Some((0, 6)));
        assert_eq!(m("<.*?>", "<a><b>"), Some((0, 3)));
    }

    #[test]
    fn groups_and_captures() {
        let re = Regex::new(r"(\w+)@(\w+)\.com", "").expect("compile");
        let chars: Vec<char> = "mail: ada@example.com!".chars().collect();
        let hit = re.find_at(&chars, 0).expect("match");
        assert_eq!((hit.start, hit.end), (6, 21));
        let g: Vec<String> = hit
            .groups
            .iter()
            .map(|g| {
                g.map(|(a, b)| chars[a..b].iter().collect())
                    .unwrap_or_default()
            })
            .collect();
        assert_eq!(g, vec!["ada".to_string(), "example".to_string()]);
    }

    #[test]
    fn unicode_is_code_points() {
        // `.` counts code points, not bytes (§3.4).
        assert_eq!(m("^.$", "🌊"), Some((0, 1)));
        assert_eq!(m("é+", "café"), Some((3, 4)));
    }

    #[test]
    fn case_insensitive_and_errors() {
        let re = Regex::new("hello", "i").expect("compile");
        let chars: Vec<char> = "say HELLO".chars().collect();
        assert!(re.is_match(&chars));
        assert!(Regex::new("(?=x)", "").is_err()); // lookahead rejected
        assert!(Regex::new(r"(a)\1", "").is_err()); // backreference rejected
        assert!(Regex::new("*", "").is_err());
    }

    #[test]
    fn hostile_pattern_terminates() {
        // Catastrophic backtracking must fail, not hang.
        let re = Regex::new("(a+)+b", "").expect("compile");
        let chars: Vec<char> = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa!".chars().collect();
        assert!(!re.is_match(&chars));
    }
}
