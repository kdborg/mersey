//! Breakpoint and step *policy*, factored out of any one protocol.
//!
//! The engine reports (`DebugHook::on_stmt`); something has to decide whether
//! a given statement boundary is a stop. That decision is identical for every
//! front-end — DAP in an editor, CDP in Chromium, RDP in Gecko/Servo,
//! Ladybird's own Inspector — so it lives here once instead of four times in
//! four forks' C++.
//!
//! A front-end owns: the wire format, and when to call `resume`/`step_*`.
//! This owns: which line is a breakpoint, whether a step condition is met,
//! and the one-stop-per-line rule. `mersey dap` and the C ABI's
//! `msy_context_debug_*` both drive it.

use crate::DebugPause;
use std::collections::HashSet;

/// What the front-end last asked for. Depths are `DebugPause::frames.len()`
/// at the statement the request was issued from.
enum Step {
    Run,
    In,
    Over(usize),
    Out(usize),
}

/// Why the engine stopped, for the front-end to report.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StopReason {
    Breakpoint,
    Step,
    /// An explicit "pause now" request (DAP `pause`, CDP `Debugger.pause`).
    Pause,
}

impl StopReason {
    /// The DAP `stopped.reason` spelling; CDP's `Debugger.paused.reason` uses
    /// the same three words for these cases.
    pub fn as_str(self) -> &'static str {
        match self {
            StopReason::Breakpoint => "breakpoint",
            StopReason::Step => "step",
            StopReason::Pause => "pause",
        }
    }
}

/// One frame, flattened for a front-end: no engine lifetimes, no `Rc`.
/// Ordered TOP-first (index 0 is the paused statement's own frame), which is
/// the order both DAP `stackTrace` and CDP `callFrames` want.
pub struct FrameInfo {
    pub name: String,
    pub module: String,
    pub line: u32,
    pub col: u32,
}

/// The breakpoint set and the pending step request. Pausing itself is not
/// here — pausing IS blocking inside `on_stmt` (see `DebugHook`), so a
/// front-end calls [`should_stop`](Self::should_stop) and, if it gets a
/// reason, simply does not return until it is resumed.
#[derive(Default)]
pub struct DebugController {
    /// Per-source line sets, replace semantics (DAP `setBreakpoints`).
    bps: Vec<(String, HashSet<u32>)>,
    step: Step,
    pause_pending: bool,
    /// The previous callout's line, so several statements sharing a
    /// breakpoint's line stop once rather than once each.
    prev_line: u32,
}

impl Default for Step {
    fn default() -> Self {
        Step::Run
    }
}

/// Trailing path component, for the source-matching rule below.
fn basename(p: &str) -> &str {
    p.rsplit(['/', '\\']).next().unwrap_or(p)
}

impl DebugController {
    pub fn new() -> Self {
        Self::default()
    }

    /// REPLACES the breakpoint set for `source` (DAP semantics; CDP's
    /// per-breakpoint add/remove is expressed by resending the source's set).
    pub fn set_breakpoints(&mut self, source: &str, lines: &[u32]) {
        self.bps.retain(|(p, _)| p != source);
        self.bps.push((source.to_string(), lines.iter().copied().collect()));
    }

    pub fn clear_breakpoints(&mut self) {
        self.bps.clear();
    }

    /// Stop at the next statement, whatever it is.
    pub fn request_pause(&mut self) {
        self.pause_pending = true;
    }

    pub fn resume(&mut self) {
        self.step = Step::Run;
    }

    /// `depth` is the paused frame count — pass `DebugPause::frames.len()`
    /// from the pause being resumed.
    pub fn step_over(&mut self, depth: usize) {
        self.step = Step::Over(depth);
    }

    pub fn step_in(&mut self) {
        self.step = Step::In;
    }

    pub fn step_out(&mut self, depth: usize) {
        self.step = Step::Out(depth);
    }

    /// A front-end's source path and the engine's module spec rarely match
    /// literally: an editor sends an absolute path, the module graph holds a
    /// relative spec. Either containing the other, or a shared basename, is a
    /// match; an empty path matches everything (a front-end that does not
    /// track sources at all).
    fn bp_hit(&self, module: &str, line: u32) -> bool {
        self.bps.iter().any(|(path, lines)| {
            lines.contains(&line)
                && (path.is_empty()
                    || path == module
                    || path.ends_with(module)
                    || module.ends_with(path.as_str())
                    || basename(path) == basename(module))
        })
    }

    /// The decision at one statement boundary — call this first thing in
    /// `on_stmt`. `Some(reason)` means stop and report; `None` means return
    /// from the callout and keep running.
    ///
    /// Consuming: a stop clears the pending pause and the step request, so
    /// the front-end must re-arm (`step_over`/`resume`/…) before returning.
    pub fn should_stop(&mut self, pause: &DebugPause) -> Option<StopReason> {
        let line = pause.pos.line;
        let depth = pause.frames.len();
        let module = pause.frames.last().map(|f| f.module.to_string()).unwrap_or_default();

        let hit_bp = self.bp_hit(&module, line) && self.prev_line != line;
        let hit_step = match self.step {
            Step::Run => false,
            Step::In => true,
            Step::Over(d) => depth <= d,
            Step::Out(d) => depth < d,
        };

        // Order matters: an explicit pause request outranks a coincident
        // breakpoint so the front-end's own request is what gets reported.
        let reason = if self.pause_pending {
            StopReason::Pause
        } else if hit_bp {
            StopReason::Breakpoint
        } else if hit_step {
            StopReason::Step
        } else {
            self.prev_line = line;
            return None;
        };

        self.prev_line = line;
        self.pause_pending = false;
        self.step = Step::Run;
        Some(reason)
    }
}

/// Flatten a pause's stack, top-first. The innermost frame reports the
/// statement's own position; every outer frame reports its call site, which
/// is what a stack view shows.
pub fn frame_infos(pause: &DebugPause) -> Vec<FrameInfo> {
    pause
        .frames
        .iter()
        .rev()
        .enumerate()
        .map(|(i, f)| {
            let (line, col) = if i == 0 {
                (pause.pos.line, pause.pos.col)
            } else {
                (f.pos.line.max(1), f.pos.col.max(1))
            };
            FrameInfo {
                name: f.name.to_string(),
                module: f.module.to_string(),
                line,
                col,
            }
        })
        .collect()
}

/// The conventional name of scope `i` of `count` in a frame's chain
/// (innermost first, as `locals` returns it). Both DAP's `scopes` and CDP's
/// `scopeChain` label them this way.
pub fn scope_name(i: usize, count: usize) -> String {
    if i == 0 {
        "Locals".to_string()
    } else if i + 1 == count {
        "Globals".to_string()
    } else {
        format!("Closure {i}")
    }
}
