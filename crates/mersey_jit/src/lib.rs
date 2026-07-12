//! Tier 1: Cranelift JIT for hot MBC kernels (ROADMAP Phase 4, at
//! deopt-free scale).
//!
//! Accepted subset — chosen so no runtime error is possible inside compiled
//! code, which is what keeps the design deopt-free (engine.md): int32
//! locals and constants; wrapping `+ - *`, masked shifts, bitwise ops,
//! comparisons; control flow. No calls, no division (would need trap
//! plumbing), no heap values. The entry guard in `mersey_interp::try_jit`
//! re-interprets any call whose arguments aren't all int32 — guard at
//! entry, never deopt in the middle.
//!
//! Translation: the stack machine becomes SSA by keeping an abstract stack
//! of Cranelift values; jump targets become blocks whose parameters carry
//! the operand stack across edges (depths from the bytecode verifier).
//!
//! Code memory is W^X: cranelift-jit maps pages writable, then flips them
//! to read-execute at finalize (spec §5.2).

use std::collections::HashMap;
use std::rc::Rc;

use cranelift_codegen::ir::{types, AbiParam, InstBuilder, MemFlags, Value as ClValue};
use cranelift_codegen::settings::{self, Configurable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{Linkage, Module};
use mersey_front::ast::{BinOp, UnaryOp};
use mersey_interp::vm::{analyze, Chunk, Op};
use mersey_interp::{JitFn, Value};

/// The hook registered on the interpreter by native hosts.
pub fn hook(chunk: &Chunk, params: &[String]) -> Option<JitFn> {
    let slots = plan_slots(chunk, params)?;
    let depths = analyze(chunk).ok()?;
    compile(chunk, params.len(), &slots, &depths)
}

/// Map every name to a flat local slot; reject anything outside the subset
/// (reads of undeclared non-param names, redeclaration, unsupported ops).
fn plan_slots(chunk: &Chunk, params: &[String]) -> Option<HashMap<u16, usize>> {
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
            Op::Const(ci) => match chunk.consts[ci as usize] {
                Value::I32(_) | Value::Bool(_) => {}
                _ => return None,
            },
            Op::Bin(op) => match op {
                BinOp::Add
                | BinOp::Sub
                | BinOp::Mul
                | BinOp::Shl
                | BinOp::Shr
                | BinOp::BitAnd
                | BinOp::BitOr
                | BinOp::BitXor
                | BinOp::Lt
                | BinOp::Gt
                | BinOp::Le
                | BinOp::Ge
                | BinOp::Eq
                | BinOp::Ne => {}
                _ => return None, // Div/Rem/Pow trap or allocate
            },
            Op::Un(op) => match op {
                UnaryOp::Neg | UnaryOp::BitNot | UnaryOp::Not => {}
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

fn compile(
    chunk: &Chunk,
    n_params: usize,
    slots: &HashMap<u16, usize>,
    depths: &[Option<i32>],
) -> Option<JitFn> {
    // Non-PIC, no colocated libcalls: required for JIT on aarch64 (no PLT).
    let mut flags = settings::builder();
    flags.set("use_colocated_libcalls", "false").ok()?;
    flags.set("is_pic", "false").ok()?;
    flags.set("opt_level", "speed").ok()?;
    let isa = cranelift_native::builder()
        .ok()?
        .finish(settings::Flags::new(flags))
        .ok()?;
    let builder = JITBuilder::with_isa(isa, cranelift_module::default_libcall_names());
    let mut module = JITModule::new(builder);

    let mut ctx = module.make_context();
    let ptr_ty = module.target_config().pointer_type();
    // extern "C" fn(args: *const i32, len: usize) -> i64
    ctx.func.signature.params.push(AbiParam::new(ptr_ty));
    ctx.func.signature.params.push(AbiParam::new(ptr_ty));
    ctx.func.signature.returns.push(AbiParam::new(types::I64));

    let mut fbc = FunctionBuilderContext::new();
    {
        let mut b = FunctionBuilder::new(&mut ctx.func, &mut fbc);
        let entry = b.create_block();
        b.append_block_params_for_function_params(entry);
        b.switch_to_block(entry);

        // Locals as Cranelift variables.
        let n_slots = slots.values().copied().max().map(|m| m + 1).unwrap_or(n_params);
        let n_slots = n_slots.max(n_params);
        for i in 0..n_slots {
            b.declare_var(cranelift_frontend::Variable::from_u32(i as u32), types::I32);
        }
        let args_ptr = b.block_params(entry)[0];
        for i in 0..n_params {
            let v = b.ins().load(
                types::I32,
                MemFlags::trusted(),
                args_ptr,
                (i * 4) as i32,
            );
            b.def_var(cranelift_frontend::Variable::from_u32(i as u32), v);
        }
        for i in n_params..n_slots {
            let zero = b.ins().iconst(types::I32, 0);
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
                b.append_block_param(blk, types::I32);
            }
            blocks.insert(t, blk);
        }

        let mut stack: Vec<ClValue> = Vec::new();
        let mut reachable = true;
        let var = |ni: u16| {
            cranelift_frontend::Variable::from_u32(slots[&ni] as u32)
        };

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
                    let v = match chunk.consts[ci as usize] {
                        Value::I32(n) => n as i64,
                        Value::Bool(t) => t as i64,
                        _ => unreachable!("plan_slots"),
                    };
                    let c = b.ins().iconst(types::I32, v);
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
                    let v = lower_bin(&mut b, binop, l, r);
                    stack.push(v);
                }
                Op::Un(u) => {
                    let v = stack.pop()?;
                    let out = match u {
                        UnaryOp::Neg => b.ins().ineg(v),
                        UnaryOp::BitNot => b.ins().bnot(v),
                        UnaryOp::Not => {
                            let c = b.ins().icmp_imm(
                                cranelift_codegen::ir::condcodes::IntCC::Equal,
                                v,
                                0,
                            );
                            b.ins().uextend(types::I32, c)
                        }
                        _ => unreachable!("plan_slots"),
                    };
                    stack.push(out);
                }
                Op::Truthy => {
                    let v = stack.pop()?;
                    let c = b.ins().icmp_imm(
                        cranelift_codegen::ir::condcodes::IntCC::NotEqual,
                        v,
                        0,
                    );
                    let out = b.ins().uextend(types::I32, c);
                    stack.push(out);
                }
                Op::Jump(t) => {
                    let args: Vec<ClValue> = stack.clone();
                    b.ins().jump(blocks[&t], &args);
                    reachable = false;
                }
                Op::JumpIfFalse(t) | Op::JumpIfTrue(t) => {
                    let v = stack.pop()?;
                    let fall = b.create_block();
                    let taken: Vec<ClValue> = stack.clone();
                    if matches!(op, Op::JumpIfFalse(_)) {
                        // nonzero -> fall through, zero -> target
                        b.ins().brif(v, fall, &[], blocks[&t], &taken);
                    } else {
                        b.ins().brif(v, blocks[&t], &taken, fall, &[]);
                    }
                    b.switch_to_block(fall);
                }
                Op::Return => {
                    let v = stack.pop()?;
                    let wide = b.ins().uextend(types::I64, v);
                    // tag 0 in high bits, low 32 = value (mask for safety)
                    let mask = b.ins().band_imm(wide, 0xFFFF_FFFF);
                    b.ins().return_(&[mask]);
                    reachable = false;
                }
                Op::ReturnNull => {
                    let null = b.ins().iconst(types::I64, 1i64 << 32);
                    b.ins().return_(&[null]);
                    reachable = false;
                }
                _ => unreachable!("plan_slots filtered"),
            }
        }
        if reachable {
            let null = b.ins().iconst(types::I64, 1i64 << 32);
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
    let f: extern "C" fn(*const i32, usize) -> i64 = unsafe { std::mem::transmute(ptr) };
    Some(Rc::new(move |args: &[i32]| f(args.as_ptr(), args.len())))
}

fn lower_bin(b: &mut FunctionBuilder, op: BinOp, l: ClValue, r: ClValue) -> ClValue {
    use cranelift_codegen::ir::condcodes::IntCC;
    let cmp = |b: &mut FunctionBuilder, cc: IntCC, l, r| {
        let c = b.ins().icmp(cc, l, r);
        b.ins().uextend(types::I32, c)
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
