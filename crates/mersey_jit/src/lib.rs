//! Tier 1: Cranelift JIT for hot MBC kernels (ROADMAP Phase 4, at
//! deopt-free scale).
//!
//! Accepted subset — chosen so a compiled function can only ever fail in
//! ways it declares up front, which is what keeps the design deopt-free
//! (engine.md):
//!
//! - **int32** kernels: locals/constants, wrapping `+ - *`, masked shifts,
//!   bitwise ops, comparisons, control flow;
//! - **float64** kernels: locals/constants, `+ - * /`, comparisons;
//! - **integer division/remainder**, which *can* fault (`x / 0`,
//!   `INT_MIN / -1` — spec §3.6 says both trap). Rather than deoptimize
//!   mid-function, the compiled code checks the divisor and returns a
//!   `TRAP` tag; the interpreter re-runs that call and raises the proper
//!   `RangeError` with a stack trace. Guard at entry, trap at the edge,
//!   never deopt in the middle.
//!
//! No calls and no heap values. The entry guard in `mersey_interp::try_jit`
//! re-interprets any call whose arguments don't match the compiled kernel's
//! parameter types.
//!
//! Translation: the stack machine becomes SSA by keeping an abstract stack
//! of Cranelift values; jump targets become blocks whose parameters carry
//! the operand stack across edges (depths from the bytecode verifier).
//!
//! Code memory is W^X: cranelift-jit maps pages writable, then flips them
//! to read-execute at finalize (spec §5.2), and the code we ask it to emit is
//! hardened — see `hardened_isa`.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use cranelift_codegen::ir::{types, AbiParam, InstBuilder, MemFlags, Value as ClValue};
use cranelift_codegen::isa::TargetIsa;
use cranelift_codegen::settings::{self, Configurable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{Linkage, Module};
use mersey_front::ast::{BinOp, UnaryOp};
use mersey_interp::vm::{analyze, Chunk, Op};
use mersey_interp::{JitArg, JitFn, JitResult, Value};

/// Numeric world a kernel operates in. A kernel is homogeneous: either all
/// int32 or all float64 (mixed kernels would need per-value types, which is
/// the typed-bytecode work).
#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    I32,
    I64,
    F64,
}

/// The hook registered on the interpreter by native hosts.
pub fn hook(chunk: &Chunk, params: &[String]) -> Option<JitFn> {
    // Try an int32 kernel, then a float64 one.
    for kind in [Kind::I32, Kind::I64, Kind::F64] {
        if let Some(slots) = plan_slots(chunk, params, kind) {
            let depths = analyze(chunk).ok()?;
            if let Some(f) = compile(chunk, params.len(), &slots, &depths, kind) {
                return Some(f);
            }
        }
    }
    None
}

/// Map every name to a flat local slot; reject anything outside the subset
/// (reads of undeclared non-param names, redeclaration, unsupported ops).
fn plan_slots(chunk: &Chunk, params: &[String], kind: Kind) -> Option<HashMap<u16, usize>> {
    let mut slots: HashMap<u16, usize> = HashMap::new();
    let mut by_name: HashMap<&str, usize> = HashMap::new();
    for (i, p) in params.iter().enumerate() {
        by_name.insert(p.as_str(), i);
    }
    let mut next = params.len();
    // Pre-map name indices used by ops.
    for op in &chunk.code {
        match *op {
            Op::DeclareName(ni) => {
                let name = chunk.names[ni as usize].as_str();
                if let Some(&slot) = by_name.get(name) {
                    // Re-declaration (shadowing) — conservative reject,
                    // except a param being re-declared is also a reject.
                    let _ = slot;
                    return None;
                }
                by_name.insert(name, next);
                slots.insert(ni, next);
                next += 1;
            }
            Op::LoadName(ni) | Op::StoreName(ni) => {
                let name = chunk.names[ni as usize].as_str();
                match by_name.get(name) {
                    Some(&slot) => {
                        slots.insert(ni, slot);
                    }
                    None => {
                        // Forward use before declare: handled on a later
                        // pass? Bytecode is linear; a load before declare
                        // means a free variable — reject below unless a
                        // later DeclareName maps it first in program order.
                    }
                }
            }
            _ => {}
        }
    }
    // Second pass in program order to catch use-before-declare / frees.
    let mut declared: HashMap<&str, usize> = HashMap::new();
    for (i, p) in params.iter().enumerate() {
        declared.insert(p.as_str(), i);
    }
    for op in &chunk.code {
        match *op {
            Op::DeclareName(ni) => {
                let name = chunk.names[ni as usize].as_str();
                let slot = *slots.get(&ni)?;
                declared.insert(name, slot);
            }
            Op::LoadName(ni) | Op::StoreName(ni) => {
                let name = chunk.names[ni as usize].as_str();
                let slot = *declared.get(name)?;
                slots.insert(ni, slot);
            }
            // Whole-op acceptance check happens here too.
            Op::Const(ci) => match (&chunk.consts[ci as usize], kind) {
                (Value::I32(_) | Value::Bool(_), Kind::I32) => {}
                // An int64 kernel's literals are still int32 in the bytecode
                // (`let i = 0` is an int32 literal that widens), so both are
                // fine — they are materialised at the kernel's width.
                (Value::I64(_) | Value::I32(_) | Value::Bool(_), Kind::I64) => {}
                // A float kernel may use int constants (loop counters are
                // still int32 in the bytecode) — but only as float literals.
                (Value::F64(_), Kind::F64) => {}
                _ => return None,
            },
            Op::Bin(op) => match (op, kind) {
                (
                    BinOp::Add
                    | BinOp::Sub
                    | BinOp::Mul
                    | BinOp::Lt
                    | BinOp::Gt
                    | BinOp::Le
                    | BinOp::Ge
                    | BinOp::Eq
                    | BinOp::Ne,
                    _,
                ) => {}
                (
                    BinOp::Shl
                    | BinOp::Shr
                    | BinOp::BitAnd
                    | BinOp::BitOr
                    | BinOp::BitXor
                    | BinOp::Div
                    | BinOp::Rem,
                    Kind::I32 | Kind::I64,
                ) => {}
                (BinOp::Div, Kind::F64) => {} // IEEE: no trap
                _ => return None,
            },
            Op::Un(op) => match (op, kind) {
                (UnaryOp::Neg, _) => {}
                (UnaryOp::BitNot | UnaryOp::Not, Kind::I32 | Kind::I64) => {}
                _ => return None,
            },
            Op::Truthy
            | Op::Jump(_)
            | Op::JumpIfFalse(_)
            | Op::JumpIfTrue(_)
            | Op::Pop
            | Op::Dup
            | Op::PushScope
            | Op::PopScope
            | Op::Return
            | Op::ReturnNull => {}
            _ => return None,
        }
    }
    Some(slots)
}

/// Result tags packed into the i64 return value.
/// A kernel returns only a *tag*; the value itself is written to an out-slot.
///
/// It used to pack `(tag << 32) | payload`, which works for an i32 payload and
/// nothing else: an i64 result fills the word the tag needs, and an f64 result
/// aliases the tags with its own bit patterns. Separating the two is what lets
/// int64 kernels exist at all, and it removes the NaN caveat floats had.
const TAG_VALUE: i64 = 0;
const TAG_NULL: i64 = 1;
/// The kernel hit a condition the spec says must throw (`x / 0`,
/// `INT_MIN / -1`): the interpreter re-runs the call and raises it properly.
const TAG_TRAP: i64 = 3;

fn compile(
    chunk: &Chunk,
    n_params: usize,
    slots: &HashMap<u16, usize>,
    depths: &[Option<i32>],
    kind: Kind,
) -> Option<JitFn> {
    let isa = hardened_isa()?;
    let builder = JITBuilder::with_isa(isa, cranelift_module::default_libcall_names());
    let mut module = JITModule::new(builder);

    let mut ctx = module.make_context();
    let ptr_ty = module.target_config().pointer_type();
    // extern "C" fn(args: *const u8, len: usize, out: *mut u8) -> i64
    // The i64 is a tag (value / null / trap); the value itself is written to
    // `out`, so its width and type are the kernel's business, not the tag's.
    ctx.func.signature.params.push(AbiParam::new(ptr_ty));
    ctx.func.signature.params.push(AbiParam::new(ptr_ty));
    ctx.func.signature.params.push(AbiParam::new(ptr_ty));
    ctx.func.signature.returns.push(AbiParam::new(types::I64));
    let val_ty = match kind {
        Kind::I32 => types::I32,
        Kind::I64 => types::I64,
        Kind::F64 => types::F64,
    };

    let mut fbc = FunctionBuilderContext::new();
    {
        let mut b = FunctionBuilder::new(&mut ctx.func, &mut fbc);
        let entry = b.create_block();
        b.append_block_params_for_function_params(entry);
        b.switch_to_block(entry);

        // Locals as Cranelift variables.
        let n_slots = slots
            .values()
            .copied()
            .max()
            .map(|m| m + 1)
            .unwrap_or(n_params);
        let n_slots = n_slots.max(n_params);
        for i in 0..n_slots {
            b.declare_var(cranelift_frontend::Variable::from_u32(i as u32), val_ty);
        }
        let args_ptr = b.block_params(entry)[0];
        let out_ptr = b.block_params(entry)[2];
        let width = if kind == Kind::I32 { 4 } else { 8 };
        for i in 0..n_params {
            let v = b
                .ins()
                .load(val_ty, MemFlags::trusted(), args_ptr, (i * width) as i32);
            b.def_var(cranelift_frontend::Variable::from_u32(i as u32), v);
        }
        for i in n_params..n_slots {
            let zero = match kind {
                Kind::I32 => b.ins().iconst(types::I32, 0),
                Kind::I64 => b.ins().iconst(types::I64, 0),
                Kind::F64 => b.ins().f64const(0.0),
            };
            b.def_var(cranelift_frontend::Variable::from_u32(i as u32), zero);
        }

        // Blocks at every jump target, with operand-stack block params.
        let mut targets: Vec<usize> = chunk
            .code
            .iter()
            .filter_map(|op| match *op {
                Op::Jump(t) | Op::JumpIfFalse(t) | Op::JumpIfTrue(t) => Some(t),
                _ => None,
            })
            .collect();
        targets.sort_unstable();
        targets.dedup();
        let mut blocks: HashMap<usize, cranelift_codegen::ir::Block> = HashMap::new();
        for &t in &targets {
            let blk = b.create_block();
            let depth = depths.get(t).copied().flatten().unwrap_or(0);
            for _ in 0..depth {
                b.append_block_param(blk, val_ty);
            }
            blocks.insert(t, blk);
        }

        let mut stack: Vec<ClValue> = Vec::new();
        let mut reachable = true;
        let var = |ni: u16| cranelift_frontend::Variable::from_u32(slots[&ni] as u32);

        for (pc, op) in chunk.code.iter().enumerate() {
            // Block boundary?
            if let Some(&blk) = blocks.get(&pc) {
                if reachable {
                    let args: Vec<ClValue> = stack.clone();
                    b.ins().jump(blk, &args);
                }
                b.switch_to_block(blk);
                stack = b.block_params(blk).to_vec();
                reachable = true;
            }
            if !reachable {
                continue;
            }
            match *op {
                Op::Const(ci) => {
                    let c = match (&chunk.consts[ci as usize], kind) {
                        (Value::I32(n), Kind::I32) => b.ins().iconst(types::I32, *n as i64),
                        (Value::Bool(t), Kind::I32) => b.ins().iconst(types::I32, *t as i64),
                        (Value::I64(n), Kind::I64) => b.ins().iconst(types::I64, *n),
                        (Value::I32(n), Kind::I64) => b.ins().iconst(types::I64, *n as i64),
                        (Value::Bool(t), Kind::I64) => b.ins().iconst(types::I64, *t as i64),
                        (Value::F64(f), Kind::F64) => b.ins().f64const(*f),
                        _ => unreachable!("plan_slots"),
                    };
                    stack.push(c);
                }
                Op::LoadName(ni) => {
                    let v = b.use_var(var(ni));
                    stack.push(v);
                }
                Op::StoreName(ni) | Op::DeclareName(ni) => {
                    let v = stack.pop()?;
                    b.def_var(var(ni), v);
                }
                Op::Pop => {
                    stack.pop()?;
                }
                Op::Dup => {
                    let v = *stack.last()?;
                    stack.push(v);
                }
                Op::PushScope | Op::PopScope => {}
                Op::Bin(binop) => {
                    let r = stack.pop()?;
                    let l = stack.pop()?;
                    // Integer division can fault (spec §3.6): check the
                    // divisor and bail to the interpreter, which raises the
                    // RangeError with a proper stack trace.
                    if matches!(kind, Kind::I32 | Kind::I64)
                        && matches!(binop, BinOp::Div | BinOp::Rem)
                    {
                        let safe = b.create_block();
                        let trap = b.create_block();
                        // divisor == 0  ||  (l == INT_MIN && r == -1)
                        let zero =
                            b.ins()
                                .icmp_imm(cranelift_codegen::ir::condcodes::IntCC::Equal, r, 0);
                        let int_min = if kind == Kind::I32 {
                            i32::MIN as i64
                        } else {
                            i64::MIN
                        };
                        let min = b.ins().icmp_imm(
                            cranelift_codegen::ir::condcodes::IntCC::Equal,
                            l,
                            int_min,
                        );
                        let neg1 =
                            b.ins()
                                .icmp_imm(cranelift_codegen::ir::condcodes::IntCC::Equal, r, -1);
                        let overflow = b.ins().band(min, neg1);
                        let faulting = b.ins().bor(zero, overflow);
                        b.ins().brif(faulting, trap, &[], safe, &[]);

                        b.switch_to_block(trap);
                        let tag = b.ins().iconst(types::I64, TAG_TRAP);
                        b.ins().return_(&[tag]);

                        b.switch_to_block(safe);
                        let v = if binop == BinOp::Div {
                            b.ins().sdiv(l, r)
                        } else {
                            b.ins().srem(l, r)
                        };
                        stack.push(v);
                    } else {
                        let v = lower_bin(&mut b, binop, l, r, kind);
                        stack.push(v);
                    }
                }
                Op::Un(u) => {
                    let v = stack.pop()?;
                    let int_ty = if kind == Kind::I64 {
                        types::I64
                    } else {
                        types::I32
                    };
                    let out = match (u, kind) {
                        (UnaryOp::Neg, Kind::I32 | Kind::I64) => b.ins().ineg(v),
                        (UnaryOp::Neg, Kind::F64) => b.ins().fneg(v),
                        (UnaryOp::BitNot, Kind::I32 | Kind::I64) => b.ins().bnot(v),
                        (UnaryOp::Not, Kind::I32 | Kind::I64) => {
                            let c = b.ins().icmp_imm(
                                cranelift_codegen::ir::condcodes::IntCC::Equal,
                                v,
                                0,
                            );
                            b.ins().uextend(int_ty, c)
                        }
                        _ => unreachable!("plan_slots"),
                    };
                    stack.push(out);
                }
                Op::Truthy => {
                    let v = stack.pop()?;
                    let out = match kind {
                        Kind::I32 | Kind::I64 => {
                            let int_ty = if kind == Kind::I64 {
                                types::I64
                            } else {
                                types::I32
                            };
                            let c = b.ins().icmp_imm(
                                cranelift_codegen::ir::condcodes::IntCC::NotEqual,
                                v,
                                0,
                            );
                            b.ins().uextend(int_ty, c)
                        }
                        Kind::F64 => {
                            let z = b.ins().f64const(0.0);
                            let c = b.ins().fcmp(
                                cranelift_codegen::ir::condcodes::FloatCC::NotEqual,
                                v,
                                z,
                            );
                            let i = b.ins().uextend(types::I64, c);
                            b.ins().fcvt_from_uint(types::F64, i)
                        }
                    };
                    stack.push(out);
                }
                Op::Jump(t) => {
                    let args: Vec<ClValue> = stack.clone();
                    b.ins().jump(blocks[&t], &args);
                    reachable = false;
                }
                Op::JumpIfFalse(t) | Op::JumpIfTrue(t) => {
                    let v = stack.pop()?;
                    let cond = match kind {
                        Kind::I32 | Kind::I64 => v,
                        Kind::F64 => {
                            let z = b.ins().f64const(0.0);
                            let c = b.ins().fcmp(
                                cranelift_codegen::ir::condcodes::FloatCC::NotEqual,
                                v,
                                z,
                            );
                            b.ins().uextend(types::I32, c)
                        }
                    };
                    let fall = b.create_block();
                    let taken: Vec<ClValue> = stack.clone();
                    if matches!(op, Op::JumpIfFalse(_)) {
                        // nonzero -> fall through, zero -> target
                        b.ins().brif(cond, fall, &[], blocks[&t], &taken);
                    } else {
                        b.ins().brif(cond, blocks[&t], &taken, fall, &[]);
                    }
                    b.switch_to_block(fall);
                }
                Op::Return => {
                    let v = stack.pop()?;
                    b.ins().store(MemFlags::trusted(), v, out_ptr, 0);
                    let tag = b.ins().iconst(types::I64, TAG_VALUE);
                    b.ins().return_(&[tag]);
                    reachable = false;
                }
                Op::ReturnNull => {
                    let null = b.ins().iconst(types::I64, TAG_NULL);
                    b.ins().return_(&[null]);
                    reachable = false;
                }
                _ => unreachable!("plan_slots filtered"),
            }
        }
        if reachable {
            let null = b.ins().iconst(types::I64, TAG_NULL);
            b.ins().return_(&[null]);
        }
        b.seal_all_blocks();
        b.finalize();
    }

    let id = module
        .declare_function("kernel", Linkage::Export, &ctx.func.signature)
        .ok()?;
    module.define_function(id, &mut ctx).ok()?;
    module.clear_context(&mut ctx);
    module.finalize_definitions().ok()?; // W^X flip happens here
    let ptr = module.get_finalized_function(id);
    // The module owns the code pages; keep it alive for the process.
    Box::leak(Box::new(module));
    let f: extern "C" fn(*const u8, usize, *mut u8) -> i64 = unsafe { std::mem::transmute(ptr) };
    Some(Rc::new(move |args: &[JitArg]| {
        // Marshal the arguments into the kernel's flat frame.
        let mut buf: Vec<u8> = Vec::with_capacity(args.len() * 8);
        for a in args {
            match (a, kind) {
                (JitArg::I32(v), Kind::I32) => buf.extend_from_slice(&v.to_ne_bytes()),
                (JitArg::I64(v), Kind::I64) => buf.extend_from_slice(&v.to_ne_bytes()),
                (JitArg::F64(v), Kind::F64) => buf.extend_from_slice(&v.to_ne_bytes()),
                // The entry guard already checked the types.
                _ => return JitResult::Bail,
            }
        }
        let mut out = [0u8; 8];
        let tag = f(buf.as_ptr(), args.len(), out.as_mut_ptr());
        match tag {
            TAG_NULL => JitResult::Null,
            TAG_TRAP => JitResult::Bail, // the interpreter re-runs and throws
            _ => match kind {
                Kind::I32 => {
                    JitResult::I32(i32::from_ne_bytes(out[..4].try_into().expect("4 bytes")))
                }
                Kind::I64 => JitResult::I64(i64::from_ne_bytes(out)),
                Kind::F64 => JitResult::F64(f64::from_ne_bytes(out)),
            },
        }
    }))
}

fn lower_bin(b: &mut FunctionBuilder, op: BinOp, l: ClValue, r: ClValue, kind: Kind) -> ClValue {
    use cranelift_codegen::ir::condcodes::{FloatCC, IntCC};
    if kind == Kind::F64 {
        let fcmp = |b: &mut FunctionBuilder, cc: FloatCC, l, r| {
            let c = b.ins().fcmp(cc, l, r);
            let i = b.ins().uextend(types::I64, c);
            b.ins().fcvt_from_uint(types::F64, i)
        };
        return match op {
            BinOp::Add => b.ins().fadd(l, r),
            BinOp::Sub => b.ins().fsub(l, r),
            BinOp::Mul => b.ins().fmul(l, r),
            BinOp::Div => b.ins().fdiv(l, r), // IEEE: inf/NaN, never traps
            BinOp::Lt => fcmp(b, FloatCC::LessThan, l, r),
            BinOp::Gt => fcmp(b, FloatCC::GreaterThan, l, r),
            BinOp::Le => fcmp(b, FloatCC::LessThanOrEqual, l, r),
            BinOp::Ge => fcmp(b, FloatCC::GreaterThanOrEqual, l, r),
            BinOp::Eq => fcmp(b, FloatCC::Equal, l, r),
            BinOp::Ne => fcmp(b, FloatCC::NotEqual, l, r),
            _ => unreachable!("plan_slots filtered"),
        };
    }
    // A comparison yields the kernel's own integer width, so it can flow into
    // the same slots and block params as everything else.
    let int_ty = if kind == Kind::I64 {
        types::I64
    } else {
        types::I32
    };
    let cmp = move |b: &mut FunctionBuilder, cc: IntCC, l, r| {
        let c = b.ins().icmp(cc, l, r);
        b.ins().uextend(int_ty, c)
    };
    match op {
        BinOp::Add => b.ins().iadd(l, r),
        BinOp::Sub => b.ins().isub(l, r),
        BinOp::Mul => b.ins().imul(l, r),
        BinOp::BitAnd => b.ins().band(l, r),
        BinOp::BitOr => b.ins().bor(l, r),
        BinOp::BitXor => b.ins().bxor(l, r),
        // Shift counts are masked to the width (§3.6) — ishl/sshr do that.
        BinOp::Shl => b.ins().ishl(l, r),
        BinOp::Shr => b.ins().sshr(l, r),
        BinOp::Lt => cmp(b, IntCC::SignedLessThan, l, r),
        BinOp::Gt => cmp(b, IntCC::SignedGreaterThan, l, r),
        BinOp::Le => cmp(b, IntCC::SignedLessThanOrEqual, l, r),
        BinOp::Ge => cmp(b, IntCC::SignedGreaterThanOrEqual, l, r),
        BinOp::Eq => cmp(b, IntCC::Equal, l, r),
        BinOp::Ne => cmp(b, IntCC::NotEqual, l, r),
        _ => unreachable!("plan_slots filtered"),
    }
}

/// The ISA the JIT compiles for, with the hardening spec §5.2 asks for.
///
/// A JIT is the softest target an engine has: it turns attacker-influenced
/// input into executable memory. W^X (cranelift-jit maps pages writable, then
/// flips them to read-execute at finalize) stops the pages from being rewritten
/// after the fact; these settings harden the code that lands in them.
///
/// * **Stack probes.** A function with a large frame otherwise moves the stack
///   pointer past the guard page in one step and writes *beyond* it, turning a
///   clean fault into memory corruption. Probing touches each page in turn, so
///   the guard page is always the first thing hit. This is what makes a guard
///   page a guarantee rather than a hope.
/// * **Pointer authentication (aarch64).** Return addresses are signed on entry
///   and authenticated on return, so an overwritten return address faults
///   instead of transferring control — backward-edge CFI, the ROP defence.
/// * **Branch Target Identification (aarch64).** An indirect branch may only
///   land on a `bti` instruction, so a corrupted pointer cannot jump into the
///   middle of a function and use its tail as a gadget — forward-edge CFI.
///
/// Both PAC and BTI live in ARM's hint space: on a CPU without them the
/// instructions are NOPs, so this is safe to enable unconditionally and costs
/// nothing where it is not supported.
///
/// On x86-64 the equivalent (CET/`endbr64`) is not exposed as a Cranelift
/// setting in the version we build against, so forward-edge CFI there is
/// honestly *not* in place yet — see SECURITY-REVIEW.md rather than assume it.
fn hardened_isa() -> Option<Arc<dyn TargetIsa>> {
    // Non-PIC, no colocated libcalls: required for JIT on aarch64 (no PLT).
    let mut flags = settings::builder();
    flags.set("use_colocated_libcalls", "false").ok()?;
    flags.set("is_pic", "false").ok()?;
    flags.set("opt_level", "speed").ok()?;

    // Guard pages: never step over one.
    flags.set("enable_probestack", "true").ok()?;
    flags.set("probestack_strategy", "inline").ok()?;

    let mut isa = cranelift_native::builder().ok()?;
    if cfg!(target_arch = "aarch64") {
        // Backward-edge CFI (PAC) and forward-edge CFI (BTI).
        let _ = isa.set("sign_return_address", "true");
        let _ = isa.set("sign_return_address_all", "true");
        let _ = isa.set("use_bti", "true");
    }
    isa.finish(settings::Flags::new(flags)).ok()
}

/// Which hardening is actually on, for the security review and its test.
pub fn hardening() -> Vec<(&'static str, bool)> {
    let Some(isa) = hardened_isa() else {
        return Vec::new();
    };
    let flags = isa.flags();
    let mut out = vec![
        ("W^X code pages", true), // cranelift-jit flips at finalize
        ("stack probes (guard pages)", flags.enable_probestack()),
    ];
    // The ISA-specific ones are reported by name in the ISA's flag list.
    let isa_flags: Vec<String> = isa.isa_flags().iter().map(|f| f.to_string()).collect();
    let on = |name: &str| isa_flags.iter().any(|f| f == &format!("{name}=1"));
    if cfg!(target_arch = "aarch64") {
        out.push((
            "pointer authentication (backward-edge CFI)",
            on("sign_return_address"),
        ));
        out.push((
            "branch target identification (forward-edge CFI)",
            on("use_bti"),
        ));
    }
    if cfg!(target_arch = "x86_64") {
        // Reported as a *row that is off*, not left out. A gap that nothing
        // mentions is indistinguishable from a gap nobody noticed, and this one
        // is real: Cranelift does not expose CET/`endbr64` as a setting (checked
        // against 0.116 and 0.123), so forward-edge CFI is genuinely not in
        // place on x86-64. See `KNOWN_GAPS` and SECURITY-REVIEW.md.
        out.push(("forward-edge CFI (CET/endbr64)", false));
    }
    out
}

/// Hardening that is knowingly absent, and why. A gap listed here is one we
/// have looked at; anything else being off is a regression.
pub const KNOWN_GAPS: &[(&str, &str)] = &[(
    "forward-edge CFI (CET/endbr64)",
    "Cranelift exposes no CET setting (checked 0.116, 0.123); x86-64 only",
)];
