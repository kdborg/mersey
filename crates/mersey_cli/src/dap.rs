//! Debug Adapter Protocol server (`mersey dap`): breakpoints, stepping,
//! stack, and locals over stdin/stdout — the standalone half of the ROADMAP
//! Phase 6 debugger ("set a breakpoint in a `.mersey` file", in any DAP
//! editor). The browser half (CDP in a fork) drives the same engine hook.
//!
//! Shape: the interpreter runs on this thread with a `DebugHook` installed;
//! a reader thread feeds parsed client requests through a channel. While the
//! program runs, the hook drains the channel non-blockingly at each
//! statement; a stop (breakpoint, step, pause request) blocks inside the
//! hook, servicing stackTrace/scopes/variables from the paused statement
//! until a resume command arrives. All policy is here — the engine only
//! reports (see `mersey_interp::DebugHook`).
//!
//! Breakpoints are path-matched (suffix/basename, so editor-absolute paths
//! find graph-relative specs); variables are served for every frame (the
//! engine keeps each frame's environment while debugging); async/generator
//! bodies report through the VM loop's line callouts, so they break and step
//! too (their slot-resolved locals are best-effort).

use std::collections::HashSet;
use std::io::{self, BufRead};
use std::process::ExitCode;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;

use mersey_interp as interp;
use mersey_interp::webjson::{self, Json};
use mersey_interp::{DebugHook, DebugPause};

use crate::lsp;

// ---- tiny JSON access/build helpers over webjson ---------------------------

fn get<'a>(j: &'a Json, key: &str) -> Option<&'a Json> {
    match j {
        Json::Obj(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
        _ => None,
    }
}

fn get_str<'a>(j: &'a Json, key: &str) -> Option<&'a str> {
    match get(j, key)? {
        Json::Str(s) => Some(s),
        _ => None,
    }
}

fn get_num(j: &Json, key: &str) -> Option<f64> {
    match get(j, key)? {
        Json::Num(n) => Some(*n),
        _ => None,
    }
}

fn obj(fields: Vec<(&str, Json)>) -> Json {
    Json::Obj(fields.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}

fn s(v: &str) -> Json {
    Json::Str(v.to_string())
}

fn n(v: f64) -> Json {
    Json::Num(v)
}

// ---- protocol plumbing -----------------------------------------------------

static SEQ: AtomicU64 = AtomicU64::new(1);

fn send(mut fields: Vec<(&str, Json)>) {
    fields.push(("seq", n(SEQ.fetch_add(1, Ordering::Relaxed) as f64)));
    let mut out = String::new();
    webjson::write(&mut out, &obj(fields));
    lsp::write_message(&out);
}

fn respond(request: &Json, body: Option<Json>) {
    let mut fields = vec![
        ("type", s("response")),
        ("request_seq", n(get_num(request, "seq").unwrap_or(0.0))),
        ("command", s(get_str(request, "command").unwrap_or(""))),
        ("success", Json::Bool(true)),
    ];
    if let Some(b) = body {
        fields.push(("body", b));
    }
    send(fields);
}

fn respond_err(request: &Json, message: &str) {
    send(vec![
        ("type", s("response")),
        ("request_seq", n(get_num(request, "seq").unwrap_or(0.0))),
        ("command", s(get_str(request, "command").unwrap_or(""))),
        ("success", Json::Bool(false)),
        ("message", s(message)),
    ]);
}

fn event(name: &str, body: Option<Json>) {
    let mut fields = vec![("type", s("event")), ("event", s(name))];
    if let Some(b) = body {
        fields.push(("body", b));
    }
    send(fields);
}

fn output_event(text: &str) {
    event(
        "output",
        Some(obj(vec![
            ("category", s("stdout")),
            ("output", s(&format!("{text}\n"))),
        ])),
    );
}

/// `setBreakpoints` REPLACES the set for its source; reply verifies each.
/// Sources are matched to executing modules by path suffix/basename, so an
/// editor's absolute path finds the graph's relative spec and vice versa.
type Breakpoints = Vec<(String, HashSet<u32>)>;

fn apply_breakpoints(req: &Json, bps: &mut Breakpoints) {
    let path = get(req, "arguments")
        .and_then(|a| get(a, "source"))
        .and_then(|s| get_str(s, "path"))
        .unwrap_or("")
        .to_string();
    let mut lines = HashSet::new();
    let mut verified = Vec::new();
    if let Some(args) = get(req, "arguments") {
        if let Some(Json::Arr(items)) = get(args, "breakpoints") {
            for item in items {
                if let Some(line) = get_num(item, "line") {
                    lines.insert(line as u32);
                    verified.push(obj(vec![
                        ("verified", Json::Bool(true)),
                        ("line", n(line)),
                    ]));
                }
            }
        }
    }
    bps.retain(|(p, _)| *p != path);
    bps.push((path, lines));
    respond(req, Some(obj(vec![("breakpoints", Json::Arr(verified))])));
}

fn basename(p: &str) -> &str {
    p.rsplit(['/', '\\']).next().unwrap_or(p)
}

fn bp_hit(bps: &Breakpoints, module: &str, line: u32) -> bool {
    bps.iter().any(|(path, lines)| {
        lines.contains(&line)
            && (path.is_empty()
                || path == module
                || path.ends_with(module)
                || module.ends_with(path.as_str())
                || basename(path) == basename(module))
    })
}

// ---- the debuggee's host ---------------------------------------------------

/// stdout is the protocol channel, so the program's prints become `output`
/// events. DOM hooks are inert (`mersey dap` debugs standalone programs).
struct DapHost;

impl interp::Host for DapHost {
    fn print(&mut self, text: &str) {
        output_event(text);
    }
    fn dom_set_text(&mut self, _id: &str, _text: &str) {}
    fn dom_get_text(&mut self, _id: &str) -> Option<String> {
        None
    }
    fn dom_add_listener(&mut self, _id: &str, _event: &str, _cb: u32) {}
}

// ---- the hook: all breakpoint/step policy ----------------------------------

enum Step {
    Run,
    In,
    Over(usize),
    Out(usize),
}

struct DapDebugger {
    rx: Rc<mpsc::Receiver<Json>>,
    program: String,
    bps: Breakpoints,
    step: Step,
    pause_pending: bool,
    /// The previous callout's line, so consecutive statements on a
    /// breakpoint's line stop once, not per statement.
    prev_line: u32,
}

impl DapDebugger {
    /// Requests that are legal while the program is running. Returns replies
    /// inline; resume-family commands are meaningless here and get errors.
    fn handle_running(&mut self, req: &Json) {
        match get_str(req, "command").unwrap_or("") {
            "setBreakpoints" => apply_breakpoints(req, &mut self.bps),
            "threads" => respond(
                req,
                Some(obj(vec![(
                    "threads",
                    Json::Arr(vec![obj(vec![("id", n(1.0)), ("name", s("main"))])]),
                )])),
            ),
            "pause" => {
                self.pause_pending = true;
                respond(req, None);
            }
            "disconnect" => {
                respond(req, None);
                std::process::exit(0);
            }
            _ => respond_err(req, "only setBreakpoints/threads/pause/disconnect while running"),
        }
    }
}

impl DebugHook for DapDebugger {
    fn on_stmt(
        &mut self,
        pause: &DebugPause,
        locals: &mut dyn FnMut(usize) -> Vec<Vec<(String, String)>>,
    ) {
        while let Ok(req) = self.rx.try_recv() {
            self.handle_running(&req);
        }
        let line = pause.pos.line;
        let depth = pause.frames.len();
        let module = pause.frames.last().map(|f| f.module.to_string()).unwrap_or_default();
        let hit_bp = bp_hit(&self.bps, &module, line) && self.prev_line != line;
        let hit_step = match self.step {
            Step::Run => false,
            Step::In => true,
            Step::Over(d) => depth <= d,
            Step::Out(d) => depth < d,
        };
        let reason = if self.pause_pending {
            "pause"
        } else if hit_bp {
            "breakpoint"
        } else if hit_step {
            "step"
        } else {
            self.prev_line = line;
            return;
        };
        self.prev_line = line;
        self.pause_pending = false;
        self.step = Step::Run;

        event(
            "stopped",
            Some(obj(vec![
                ("reason", s(reason)),
                ("threadId", n(1.0)),
                ("allThreadsStopped", Json::Bool(true)),
            ])),
        );

        // Per-frame scope snapshots on first use, held for this pause only.
        // variablesReference encodes (frame_from_top, scope): frame*64+scope+1.
        let mut scopes: std::collections::HashMap<usize, Vec<Vec<(String, String)>>> =
            std::collections::HashMap::new();

        loop {
            let Ok(req) = self.rx.recv() else {
                std::process::exit(0); // client hung up
            };
            match get_str(&req, "command").unwrap_or("") {
                "stackTrace" => {
                    let frames: Vec<Json> = pause
                        .frames
                        .iter()
                        .rev()
                        .enumerate()
                        .map(|(i, f)| {
                            let (fline, fcol) = if i == 0 {
                                (pause.pos.line, pause.pos.col)
                            } else {
                                (f.pos.line.max(1), f.pos.col.max(1))
                            };
                            let source = if f.module.is_empty() || &*f.module == "<script>" {
                                self.program.clone()
                            } else {
                                f.module.to_string()
                            };
                            obj(vec![
                                ("id", n(i as f64)),
                                ("name", s(&f.name)),
                                ("source", obj(vec![("path", s(&source))])),
                                ("line", n(fline as f64)),
                                ("column", n(fcol as f64)),
                            ])
                        })
                        .collect();
                    let total = frames.len();
                    respond(
                        &req,
                        Some(obj(vec![
                            ("stackFrames", Json::Arr(frames)),
                            ("totalFrames", n(total as f64)),
                        ])),
                    );
                }
                "scopes" => {
                    let frame_id = get(&req, "arguments")
                        .and_then(|a| get_num(a, "frameId"))
                        .unwrap_or(0.0) as usize;
                    let snap = scopes.entry(frame_id).or_insert_with(|| locals(frame_id));
                    let last = snap.len().saturating_sub(1);
                    let list: Vec<Json> = snap
                        .iter()
                        .enumerate()
                        .map(|(i, _)| {
                            let name = if i == 0 {
                                "Locals".to_string()
                            } else if i == last {
                                "Globals".to_string()
                            } else {
                                format!("Closure {i}")
                            };
                            obj(vec![
                                ("name", s(&name)),
                                ("variablesReference", n((frame_id * 64 + i + 1) as f64)),
                                ("expensive", Json::Bool(i == last)),
                            ])
                        })
                        .collect();
                    respond(&req, Some(obj(vec![("scopes", Json::Arr(list))])));
                }
                "variables" => {
                    let reference = get(&req, "arguments")
                        .and_then(|a| get_num(a, "variablesReference"))
                        .unwrap_or(0.0) as usize;
                    let (frame_id, scope_idx) = ((reference - 1) / 64, (reference - 1) % 64);
                    let snap = scopes.entry(frame_id).or_insert_with(|| locals(frame_id));
                    let vars: Vec<Json> = snap
                        .get(scope_idx)
                        .map(|scope| {
                            scope
                                .iter()
                                .map(|(name, value)| {
                                    obj(vec![
                                        ("name", s(name)),
                                        ("value", s(value)),
                                        ("variablesReference", n(0.0)),
                                    ])
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    respond(&req, Some(obj(vec![("variables", Json::Arr(vars))])));
                }
                "continue" => {
                    respond(
                        &req,
                        Some(obj(vec![("allThreadsContinued", Json::Bool(true))])),
                    );
                    self.step = Step::Run;
                    return;
                }
                "next" => {
                    respond(&req, None);
                    self.step = Step::Over(depth);
                    return;
                }
                "stepIn" => {
                    respond(&req, None);
                    self.step = Step::In;
                    return;
                }
                "stepOut" => {
                    respond(&req, None);
                    self.step = Step::Out(depth);
                    return;
                }
                "setBreakpoints" => apply_breakpoints(&req, &mut self.bps),
                "threads" => respond(
                    &req,
                    Some(obj(vec![(
                        "threads",
                        Json::Arr(vec![obj(vec![("id", n(1.0)), ("name", s("main"))])]),
                    )])),
                ),
                "pause" => respond(&req, None), // already paused
                "disconnect" => {
                    respond(&req, None);
                    std::process::exit(0);
                }
                _ => respond_err(&req, "unsupported while paused"),
            }
        }
    }
}

// ---- the session -----------------------------------------------------------

pub fn serve() -> ExitCode {
    let mut program: Option<String> = None;
    let mut bps: Breakpoints = Vec::new();

    // Configuration phase, on this thread: initialize → launch →
    // setBreakpoints → configurationDone.
    {
        let stdin = io::stdin();
        let mut reader = stdin.lock();
        loop {
            let Some(msg) = lsp::read_message(&mut reader) else {
                return ExitCode::SUCCESS;
            };
            let Some(req) = webjson::parse(&msg) else {
                continue;
            };
            match get_str(&req, "command").unwrap_or("") {
                "initialize" => {
                    respond(
                        &req,
                        Some(obj(vec![(
                            "supportsConfigurationDoneRequest",
                            Json::Bool(true),
                        )])),
                    );
                    event("initialized", None);
                }
                "launch" => {
                    program = get(&req, "arguments")
                        .and_then(|a| get_str(a, "program"))
                        .map(str::to_string);
                    respond(&req, None);
                }
                "setBreakpoints" => apply_breakpoints(&req, &mut bps),
                "configurationDone" => {
                    respond(&req, None);
                    break;
                }
                "disconnect" => {
                    respond(&req, None);
                    return ExitCode::SUCCESS;
                }
                "threads" => respond(
                    &req,
                    Some(obj(vec![(
                        "threads",
                        Json::Arr(vec![obj(vec![("id", n(1.0)), ("name", s("main"))])]),
                    )])),
                ),
                _ => respond_err(&req, "unsupported before configurationDone"),
            }
        }
    }

    let Some(program) = program else {
        output_event("mersey dap: no `program` in launch request");
        event("terminated", None);
        return ExitCode::FAILURE;
    };

    // The reader thread owns stdin from here; requests flow via the channel.
    let (tx, rx) = mpsc::channel::<Json>();
    std::thread::spawn(move || {
        let stdin = io::stdin();
        let mut reader = stdin.lock();
        while let Some(msg) = lsp::read_message(&mut reader) {
            if let Some(req) = webjson::parse(&msg) {
                if tx.send(req).is_err() {
                    break;
                }
            }
        }
    });
    let rx = Rc::new(rx);

    let graph = match crate::load_graph(&program) {
        Ok(g) => g,
        Err(_) => {
            output_event(&format!("mersey dap: cannot load `{program}`"));
            event(
                "exited",
                Some(obj(vec![("exitCode", n(1.0))])),
            );
            event("terminated", None);
            return ExitCode::FAILURE;
        }
    };
    if !crate::check_graph_ok(&graph.all) {
        output_event(&format!("mersey dap: `{program}` has check errors"));
        event("exited", Some(obj(vec![("exitCode", n(1.0))])));
        event("terminated", None);
        return ExitCode::FAILURE;
    }

    let mut interp = interp::new_interp(Box::new(DapHost));
    // Debugging tree-walks (the hook forces it); no JIT tier.
    interp.set_debug_hook(Box::new(DapDebugger {
        rx: rx.clone(),
        program: program.clone(),
        bps,
        step: Step::Run,
        pause_pending: false,
        prev_line: 0,
    }));
    let (eager, lazy) = graph.split();
    for (spec, module) in lazy {
        interp.register_lazy(spec, module);
    }
    let code = match interp.run_graph(eager) {
        Ok(()) => 0.0,
        Err(t) => {
            output_event(&format!(
                "runtime error: {}",
                interp.describe_thrown(&t)
            ));
            1.0
        }
    };
    event("exited", Some(obj(vec![("exitCode", n(code))])));
    event("terminated", None);

    // Serve the tail of the session (the client's disconnect).
    while let Ok(req) = rx.recv() {
        match get_str(&req, "command").unwrap_or("") {
            "disconnect" => {
                respond(&req, None);
                break;
            }
            other if other.is_empty() => {}
            _ => respond_err(&req, "program has exited"),
        }
    }
    ExitCode::SUCCESS
}
