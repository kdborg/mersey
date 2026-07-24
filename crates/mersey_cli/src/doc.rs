// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Kirk D. Brown

//! `mersey doc` — the documentation site.
//!
//! The API reference is **generated from the checker**, not written alongside
//! it: every signature on those pages is produced by the same `member_access`
//! that typechecks the call. A hand-written reference is wrong the first time
//! someone adds a method and forgets the docs; this one cannot be.
//!
//! The language reference is the specification in `docs/spec`, rendered — so
//! there is one normative text, not a spec and a "guide" that disagree.

use std::fmt::Write as _;
use std::fs;
use std::path::Path;
use std::process::ExitCode;

use mersey_front::check;

pub fn build(outdir: &str) -> ExitCode {
    let out = Path::new(outdir);
    if let Err(e) = fs::create_dir_all(out) {
        eprintln!("mersey: cannot create {outdir}: {e}");
        return ExitCode::FAILURE;
    }
    if let Err(e) = fs::write(out.join("style.css"), STYLE) {
        eprintln!("mersey: cannot write style.css: {e}");
        return ExitCode::FAILURE;
    }

    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut pages: Vec<(String, String)> = Vec::new(); // (file, title)

    // The language: the specification, rendered.
    let mut spec_pages = Vec::new();
    for entry in read_sorted(&root.join("docs/spec")) {
        let Some(name) = entry.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let Ok(md) = fs::read_to_string(&entry) else {
            continue;
        };
        let title = first_heading(&md).unwrap_or_else(|| name.to_string());
        let file = format!("spec-{name}.html");
        let body = markdown(&md);
        write_page(out, &file, &title, &body, &pages_nav());
        spec_pages.push((file, title));
    }

    // The architecture notes, likewise.
    let mut arch_pages = Vec::new();
    for entry in read_sorted(&root.join("docs/architecture")) {
        let Some(name) = entry.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let Ok(md) = fs::read_to_string(&entry) else {
            continue;
        };
        let title = first_heading(&md).unwrap_or_else(|| name.to_string());
        let file = format!("arch-{name}.html");
        let body = markdown(&md);
        write_page(out, &file, &title, &body, &pages_nav());
        arch_pages.push((file, title));
    }

    // The library: enumerated from the checker.
    //
    // Sorted by title, because a reader who wants `math` looks for it where `m`
    // is — not where the checker happened to register it. The order the groups
    // come back in is an implementation detail of the compiler, and it has no
    // business being the order of a page people read.
    let mut api = check::api_reference();
    api.sort_by_key(|g| g.title.to_lowercase());
    let mut body = String::new();
    body.push_str("<h1>Standard library</h1>\n");
    body.push_str(
        "<p class=\"lede\">Every signature on this page was produced by the compiler's own \
         type checker — the same code that checks the call when you write it. This page \
         cannot describe a member that does not exist, or describe one differently from \
         how it behaves.</p>\n",
    );

    body.push_str("<nav class=\"toc\"><ul>");
    for g in &api {
        let _ = write!(
            body,
            "<li><a href=\"#{}\">{}</a></li>",
            slug(&g.title),
            esc(&g.title)
        );
    }
    body.push_str("</ul></nav>\n");

    for g in &api {
        let _ = write!(
            body,
            "<section id=\"{}\">\n<h2>{}</h2>\n",
            slug(&g.title),
            esc(&g.title)
        );
        // The description and the example come from `docs/examples/<key>.mersey`
        // — a real program, executed by the test suite, shown with the output it
        // actually printed. A documentation example that is not run is a claim
        // nobody checks; it rots the first time the API changes, and the only
        // person who finds out is the reader.
        // A class exported by a `std:` module borrows that module's example: it
        // is the same code, and writing it twice would only mean maintaining it
        // twice.
        let example_key = if root
            .join(format!("docs/examples/{}.mersey", g.key))
            .exists()
        {
            g.key.clone()
        } else {
            g.parent.clone()
        };
        let example = fs::read_to_string(root.join(format!("docs/examples/{example_key}.mersey")));
        let output = fs::read_to_string(root.join(format!("docs/examples/{example_key}.expect")));
        let (about, code) = match &example {
            Ok(src) => split_doc_comment(src),
            Err(_) => (String::new(), String::new()),
        };
        if !about.is_empty() {
            let _ = writeln!(body, "<div class=\"about\">{}</div>", paragraphs(&about));
        }
        if !g.import.is_empty() {
            // What you write to get this group. Usually the title *is* the name
            // you import (`math` from `std:math`). `browser:dom` has no such
            // name — it exports the globals themselves — so it shows a few of
            // them instead of an import statement that would not compile.
            let names = if g.title.contains(':') {
                let shown: Vec<&str> = g.members.iter().take(4).map(|m| m.name.as_str()).collect();
                format!("{}, …", shown.join(", "))
            } else {
                esc(&g.title)
            };
            let _ = writeln!(
                body,
                "<pre class=\"import\"><code>import {{ {} }} from \"{}\";</code></pre>",
                names,
                esc(&g.import)
            );
        }
        if !code.is_empty() {
            let _ = write!(
                body,
                "<details class=\"example\" open><summary>Example</summary>\n<pre><code>{}</code></pre>\n",
                esc(code.trim_end())
            );
            if let Ok(out_text) = &output {
                let _ = write!(
                    body,
                    "<p class=\"note\">Output:</p>\n<pre class=\"output\"><code>{}</code></pre>\n",
                    esc(out_text.trim_end())
                );
            }
            body.push_str("</details>\n");
        }
        if g.members.is_empty() {
            body.push_str("<p class=\"empty\">No members.</p>\n");
        } else {
            body.push_str("<div class=\"table-wrap\"><table class=\"api\">\n<thead><tr><th>Member</th><th>Type</th></tr></thead>\n<tbody>\n");
            for m in &g.members {
                let kind = if m.is_fn { "fn" } else { "value" };
                let _ = writeln!(
                    body,
                    "<tr><td><code class=\"name {kind}\">{}</code></td><td><code>{}</code></td></tr>",
                    esc(&m.name),
                    esc(&m.signature)
                );
            }
            body.push_str("</tbody></table></div>\n");
        }
        body.push_str("</section>\n");
    }
    write_page(out, "library.html", "Standard library", &body, &pages_nav());

    // Worked examples: the conformance suite. Every program on that page is
    // executed by the test suite on both engines and compared against the output
    // shown beneath it — so an example there cannot be wrong, or go stale, or
    // quietly stop compiling.
    let mut ex = String::new();
    ex.push_str("<h1>Examples</h1>\n");
    ex.push_str(
        "<p class=\"lede\">These are the conformance suite. Every program here is run by the \
         test suite on both engines — the tree-walker and the bytecode VM — and its output is \
         compared against what you see below it. An example on this page cannot be wrong, go \
         stale, or quietly stop compiling.</p>\n",
    );
    let mut examples: Vec<(String, String, String)> = Vec::new(); // (name, src, out)
    for entry in read_sorted_ext(&root.join("tests/conformance/runtime"), "mersey") {
        let Some(stem) = entry.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let Ok(src) = fs::read_to_string(&entry) else {
            continue;
        };
        let expect = entry.with_extension("expect");
        let Ok(out_text) = fs::read_to_string(&expect) else {
            continue;
        };
        examples.push((stem.to_string(), src, out_text));
    }
    ex.push_str("<nav class=\"toc\"><ul>");
    for (name, _, _) in &examples {
        let _ = write!(ex, "<li><a href=\"#{}\">{}</a></li>", slug(name), esc(name));
    }
    ex.push_str("</ul></nav>\n");
    for (name, src, out_text) in &examples {
        let _ = write!(
            ex,
            "<section id=\"{}\">\n<h2>{}</h2>\n<pre><code>{}</code></pre>\n             <p class=\"note\">Output:</p>\n<pre class=\"output\"><code>{}</code></pre>\n</section>\n",
            slug(name),
            esc(name),
            esc(src.trim_end()),
            esc(out_text.trim_end())
        );
    }
    write_page(out, "examples.html", "Examples", &ex, &pages_nav());

    // Index.
    let mut idx = String::from(
        "<h1>Mersey</h1>\n<p class=\"lede\">A statically typed, class-based language for the \
         browser. JavaScript syntax; not JavaScript semantics.</p>\n",
    );
    idx.push_str("<h2>The language</h2>\n<ul class=\"cards\">\n");
    for (file, title) in &spec_pages {
        let _ = writeln!(idx, "<li><a href=\"{file}\">{}</a></li>", esc(title));
    }
    idx.push_str("</ul>\n<h2>The library</h2>\n<ul class=\"cards\">\n");
    let _ = writeln!(
        idx,
        "<li><a href=\"library.html\">Standard library</a> <span class=\"note\">({} modules and \
         types, generated from the type checker</span></li>",
        api.len()
    );
    let _ = writeln!(
        idx,
        "<li><a href=\"examples.html\">Examples</a> <span class=\"note\">({} programs, each one \
         executed and checked by the test suite)</span></li>",
        examples.len()
    );
    idx.push_str("</ul>\n<h2>The engine</h2>\n<ul class=\"cards\">\n");
    for (file, title) in &arch_pages {
        let _ = writeln!(idx, "<li><a href=\"{file}\">{}</a></li>", esc(title));
    }
    idx.push_str("</ul>\n");
    write_page(out, "index.html", "Mersey", &idx, &pages_nav());

    pages.push(("index.html".into(), "Mersey".into()));
    println!(
        "wrote {} pages to {outdir}: {} spec chapters, {} library groups, {} examples, \
         {} architecture notes",
        spec_pages.len() + arch_pages.len() + 3,
        spec_pages.len(),
        api.len(),
        examples.len(),
        arch_pages.len()
    );
    ExitCode::SUCCESS
}

/// Split an example into its leading `//` block (what the module *is*) and the
/// program below it (what using it looks like).
fn split_doc_comment(src: &str) -> (String, String) {
    let mut about: Vec<String> = Vec::new();
    let mut rest: Vec<String> = Vec::new();
    let mut in_header = true;
    for line in src.lines() {
        if in_header && line.starts_with("//") {
            let t = line.trim_start_matches('/').trim();
            // Directives to the test runner — which capabilities to grant, and
            // whether the example needs a browser — are not prose, and do not
            // belong in the description they sit above.
            if t.starts_with("caps:") || t == "browser" {
                continue;
            }
            about.push(t.to_string());
            continue;
        }
        if in_header && line.trim().is_empty() && about.is_empty() {
            continue;
        }
        in_header = false;
        rest.push(line.to_string());
    }
    (
        about.join("\n").trim().to_string(),
        rest.join("\n").trim_start().to_string(),
    )
}

/// Blank-line-separated paragraphs, with inline markup.
fn paragraphs(text: &str) -> String {
    let mut out = String::new();
    for para in text.split("\n\n") {
        let joined = para.split('\n').collect::<Vec<_>>().join(" ");
        if !joined.trim().is_empty() {
            let _ = write!(out, "<p>{}</p>", inline(joined.trim()));
        }
    }
    out
}

fn read_sorted_ext(dir: &Path, ext: &str) -> Vec<std::path::PathBuf> {
    let mut v: Vec<_> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == ext))
        .collect();
    v.sort();
    v
}

fn read_sorted(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut v: Vec<_> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "md"))
        .collect();
    v.sort();
    v
}

fn pages_nav() -> String {
    "<a href=\"index.html\">Mersey</a><a href=\"library.html\">Library</a>\
     <a href=\"examples.html\">Examples</a>"
        .to_string()
}

fn first_heading(md: &str) -> Option<String> {
    md.lines().find(|l| l.starts_with("# ")).map(|l| {
        let h = l[2..].trim();
        // "Mersey Language Specification — 3. Types and Conversions" is the title
        // of the *document*; on a page that is already the specification, the
        // part worth reading is what comes after the dash.
        h.split_once(" — ")
            .map(|(_, rest)| rest)
            .unwrap_or(h)
            .to_string()
    })
}

fn slug(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn write_page(out: &Path, file: &str, title: &str, body: &str, nav: &str) {
    let html = format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{t} — Mersey</title>
<link rel="stylesheet" href="style.css">
</head>
<body>
<header><nav>{nav}</nav></header>
<main>
{body}
</main>
<footer>Generated by <code>mersey doc</code>. The library reference is produced by the compiler's
own type checker.</footer>
</body>
</html>
"#,
        t = esc(title),
        nav = nav,
        body = body
    );
    let _ = fs::write(out.join(file), html);
}

/// A small Markdown renderer: headings, paragraphs, lists, fenced and indented
/// code, inline code, bold, links, tables.
///
/// Deliberately not a dependency. The spec is written in a narrow subset, and a
/// renderer that handles exactly that subset is smaller than the argument about
/// which crate to use.
fn markdown(md: &str) -> String {
    let mut out = String::new();
    let mut lines = md.lines().peekable();
    let mut in_code = false;
    let mut para: Vec<String> = Vec::new();
    let mut list: Vec<String> = Vec::new();
    let mut table: Vec<String> = Vec::new();

    fn flush_para(out: &mut String, para: &mut Vec<String>) {
        if !para.is_empty() {
            let _ = writeln!(out, "<p>{}</p>", inline(&para.join(" ")));
            para.clear();
        }
    }
    fn flush_list(out: &mut String, list: &mut Vec<String>) {
        if !list.is_empty() {
            out.push_str("<ul>\n");
            for item in list.iter() {
                let _ = writeln!(out, "<li>{}</li>", inline(item));
            }
            out.push_str("</ul>\n");
            list.clear();
        }
    }
    fn flush_table(out: &mut String, table: &mut Vec<String>) {
        if table.is_empty() {
            return;
        }
        out.push_str("<div class=\"table-wrap\"><table>\n");
        for (i, row) in table.iter().enumerate() {
            // The `|---|---|` separator row carries no content.
            if row.chars().all(|c| matches!(c, '|' | '-' | ':' | ' ')) {
                continue;
            }
            let cells: Vec<&str> = row.trim_matches('|').split('|').collect();
            let tag = if i == 0 { "th" } else { "td" };
            out.push_str("<tr>");
            for c in cells {
                let _ = write!(out, "<{tag}>{}</{tag}>", inline(c.trim()));
            }
            out.push_str("</tr>\n");
        }
        out.push_str("</table></div>\n");
        table.clear();
    }

    while let Some(line) = lines.next() {
        if line.trim_start().starts_with("```") {
            if in_code {
                out.push_str("</code></pre>\n");
                in_code = false;
            } else {
                flush_para(&mut out, &mut para);
                flush_list(&mut out, &mut list);
                out.push_str("<pre><code>");
                in_code = true;
            }
            continue;
        }
        if in_code {
            let _ = writeln!(out, "{}", esc(line));
            continue;
        }
        // An indented block is code too — the spec uses both.
        if line.starts_with("    ") && para.is_empty() && list.is_empty() && table.is_empty() {
            out.push_str("<pre><code>");
            let _ = writeln!(out, "{}", esc(&line[4..]));
            while let Some(next) = lines.peek() {
                if next.starts_with("    ") || next.trim().is_empty() {
                    let l = lines.next().unwrap();
                    let _ = writeln!(out, "{}", esc(l.strip_prefix("    ").unwrap_or("")));
                } else {
                    break;
                }
            }
            out.push_str("</code></pre>\n");
            continue;
        }
        let t = line.trim_end();
        if t.starts_with('|') {
            flush_para(&mut out, &mut para);
            flush_list(&mut out, &mut list);
            table.push(t.to_string());
            continue;
        }
        flush_table(&mut out, &mut table);
        if let Some(rest) = t.strip_prefix("### ") {
            flush_para(&mut out, &mut para);
            flush_list(&mut out, &mut list);
            let _ = writeln!(out, "<h3 id=\"{}\">{}</h3>", slug(rest), inline(rest));
        } else if let Some(rest) = t.strip_prefix("## ") {
            flush_para(&mut out, &mut para);
            flush_list(&mut out, &mut list);
            let _ = writeln!(out, "<h2 id=\"{}\">{}</h2>", slug(rest), inline(rest));
        } else if let Some(rest) = t.strip_prefix("# ") {
            flush_para(&mut out, &mut para);
            flush_list(&mut out, &mut list);
            let _ = writeln!(out, "<h1>{}</h1>", inline(rest));
        } else if let Some(rest) = t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")) {
            flush_para(&mut out, &mut para);
            list.push(rest.to_string());
        } else if t.trim().is_empty() {
            flush_para(&mut out, &mut para);
            flush_list(&mut out, &mut list);
        } else if !list.is_empty() && t.starts_with("  ") {
            // A continuation of the previous bullet.
            if let Some(last) = list.last_mut() {
                last.push(' ');
                last.push_str(t.trim());
            }
        } else {
            para.push(t.to_string());
        }
    }
    flush_para(&mut out, &mut para);
    flush_list(&mut out, &mut list);
    flush_table(&mut out, &mut table);
    if in_code {
        out.push_str("</code></pre>\n");
    }
    out
}

/// Inline markup: `code`, **bold**, *italic*, [text](href).
fn inline(s: &str) -> String {
    let mut out = String::new();
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '`' => {
                if let Some(end) = chars[i + 1..].iter().position(|c| *c == '`') {
                    let code: String = chars[i + 1..i + 1 + end].iter().collect();
                    let _ = write!(out, "<code>{}</code>", esc(&code));
                    i += end + 2;
                    continue;
                }
                out.push_str("&#96;");
                i += 1;
            }
            '*' if i + 1 < chars.len() && chars[i + 1] == '*' => {
                if let Some(end) = find_seq(&chars, i + 2, "**") {
                    let inner: String = chars[i + 2..end].iter().collect();
                    let _ = write!(out, "<strong>{}</strong>", esc(&inner));
                    i = end + 2;
                    continue;
                }
                out.push_str("**");
                i += 2;
            }
            '*' => {
                if let Some(end) = find_seq(&chars, i + 1, "*") {
                    let inner: String = chars[i + 1..end].iter().collect();
                    let _ = write!(out, "<em>{}</em>", esc(&inner));
                    i = end + 1;
                    continue;
                }
                out.push('*');
                i += 1;
            }
            '[' => {
                if let (Some(close), Some(open)) = (
                    chars[i..].iter().position(|c| *c == ']').map(|p| i + p),
                    chars[i..].iter().position(|c| *c == '(').map(|p| i + p),
                ) {
                    if let Some(rp) = chars[open..]
                        .iter()
                        .position(|c| *c == ')')
                        .map(|p| open + p)
                    {
                        if close < open {
                            let text: String = chars[i + 1..close].iter().collect();
                            let href: String = chars[open + 1..rp].iter().collect();
                            let _ = write!(out, "<a href=\"{}\">{}</a>", esc(&href), esc(&text));
                            i = rp + 1;
                            continue;
                        }
                    }
                }
                out.push_str("&#91;");
                i += 1;
            }
            c => {
                match c {
                    '&' => out.push_str("&amp;"),
                    '<' => out.push_str("&lt;"),
                    '>' => out.push_str("&gt;"),
                    '"' => out.push_str("&quot;"),
                    other => out.push(other),
                }
                i += 1;
            }
        }
    }
    out
}

fn find_seq(chars: &[char], from: usize, seq: &str) -> Option<usize> {
    let s: Vec<char> = seq.chars().collect();
    (from..chars.len().saturating_sub(s.len() - 1)).find(|&i| chars[i..i + s.len()] == s[..])
}

const STYLE: &str = include_str!("doc.css");
