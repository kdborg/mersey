/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! The Mersey debugger actor: a Firefox-RDP actor that arms Mersey breakpoints
//! and drives pause/resume against the page's Mersey engine. It never touches
//! the SpiderMonkey debugger — Mersey pauses are a separate path (like Firefox's
//! own fork). Breakpoints arm via `DevtoolScriptControlMsg::MerseyDebugArm`; a
//! pause arrives as `ScriptToDevtoolsControlMsg::MerseyPaused` (routed by the
//! devtools loop, which stores the reply sender here and emits a `merseyPaused`
//! event), and resume/step send the action back over that reply sender.

use std::sync::Arc;

use atomic_refcell::AtomicRefCell;
use servo_base::id::PipelineId;
use devtools_traits::{DevtoolScriptControlMsg, MerseyDebugAction};
use malloc_size_of_derive::MallocSizeOf;
use serde::Serialize;
use serde_json::{Map, Value};
use servo_base::generic_channel::{channel, GenericSender};

use crate::actor::{Actor, ActorError, ActorRegistry, new_actor_name};
use crate::protocol::ClientRequest;
use crate::{EmptyReplyMsg, StreamId};

/// The unsolicited `merseyPaused` event this actor emits when the engine hits a
/// breakpoint (the devtools loop writes it to the connected clients).
#[derive(Serialize)]
pub(crate) struct MerseyPausedMsg {
    pub from: String,
    #[serde(rename = "type")]
    pub type_: String,
    pub snapshot: String,
}

/// Reply to `evaluateInFrame`: the value's display text, and whether it errored.
#[derive(Serialize)]
struct EvaluateReplyMsg {
    from: String,
    result: String,
    #[serde(rename = "isError")]
    is_error: bool,
}

#[derive(MallocSizeOf)]
pub(crate) struct MerseyDebuggerActor {
    name: String,
    #[ignore_malloc_size_of = "channel"]
    script_sender: GenericSender<DevtoolScriptControlMsg>,
    #[ignore_malloc_size_of = "id"]
    pipeline: PipelineId,
    /// The reply sender from the current pause; set by the devtools loop when a
    /// `MerseyPaused` arrives, taken when the client resumes.
    #[ignore_malloc_size_of = "channel"]
    resume: AtomicRefCell<Option<GenericSender<MerseyDebugAction>>>,
}

impl MerseyDebuggerActor {
    pub(crate) fn register(
        registry: &ActorRegistry,
        script_sender: GenericSender<DevtoolScriptControlMsg>,
        pipeline: PipelineId,
    ) -> Arc<Self> {
        let name = new_actor_name::<Self>();
        registry.register::<Self>(MerseyDebuggerActor {
            name,
            script_sender,
            pipeline,
            resume: AtomicRefCell::new(None),
        })
    }

    /// Called by the devtools loop when the engine pauses: remember how to
    /// resume it.
    pub(crate) fn store_resume(&self, resume: GenericSender<MerseyDebugAction>) {
        *self.resume.borrow_mut() = Some(resume);
    }
}

impl Actor for MerseyDebuggerActor {
    fn name(&self) -> &str {
        &self.name
    }

    fn handle_message(
        &self,
        request: ClientRequest,
        _registry: &ActorRegistry,
        msg_type: &str,
        msg: &Map<String, Value>,
        _id: StreamId,
    ) -> Result<(), ActorError> {
        match msg_type {
            "setBreakpoint" => {
                let source = msg
                    .get("source")
                    .and_then(|v| v.as_str())
                    .unwrap_or("<repl>")
                    .to_owned();
                let lines: Vec<u32> = msg
                    .get("lines")
                    .and_then(|v| v.as_array())
                    .map(|a| a.iter().filter_map(|v| v.as_u64().map(|n| n as u32)).collect())
                    .unwrap_or_default();
                let _ = self.script_sender.send(DevtoolScriptControlMsg::MerseyDebugArm(
                    self.pipeline,
                    source,
                    lines,
                ));
                request.reply_final(&EmptyReplyMsg {
                    from: self.name().into(),
                })?
            },
            "resume" | "stepOver" | "stepIn" | "stepOut" => {
                let action = match msg_type {
                    "stepOver" => MerseyDebugAction::StepOver,
                    "stepIn" => MerseyDebugAction::StepIn,
                    "stepOut" => MerseyDebugAction::StepOut,
                    _ => MerseyDebugAction::Resume,
                };
                if let Some(tx) = self.resume.borrow_mut().take() {
                    let _ = tx.send(action);
                }
                request.reply_final(&EmptyReplyMsg {
                    from: self.name().into(),
                })?
            },
            "evaluateInFrame" => {
                let frame = msg.get("frame").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                let expression = msg
                    .get("expression")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_owned();
                // Send the request through the stored pause sender (cloned, so the
                // pause stays open) and wait for the engine's reply. The engine is
                // blocked in its on-paused loop, which serves the Evaluate action.
                let sender = self.resume.borrow().as_ref().cloned();
                let raw = match sender.and_then(|tx| channel::<String>().map(|c| (tx, c))) {
                    Some((tx, (rtx, rrx))) => {
                        if tx
                            .send(MerseyDebugAction::Evaluate(frame, expression, rtx))
                            .is_ok()
                        {
                            rrx.recv().unwrap_or_else(|_| "!evaluate failed".to_owned())
                        } else {
                            "!not paused".to_owned()
                        }
                    },
                    None => "!not paused".to_owned(),
                };
                let (result, is_error) = match raw.strip_prefix('!') {
                    Some(rest) => (rest.to_owned(), true),
                    None => (raw, false),
                };
                request.reply_final(&EvaluateReplyMsg {
                    from: self.name().into(),
                    result,
                    is_error,
                })?
            },
            _ => return Err(ActorError::UnrecognizedPacketType),
        }
        Ok(())
    }
}
