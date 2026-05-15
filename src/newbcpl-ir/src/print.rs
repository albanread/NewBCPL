//! Stable textual dump of a NewBCPL IR `Module`.
//!
//! Output shape is roughly LLVM-flavoured but BCPL-specific. One
//! function block per `Function`, each block labelled, instructions
//! indented, terminator on its own line. SSA values render as
//! `%N`, blocks as `bbN`.
//!
//! This format is what `dump-ir` emits — it's the canonical artifact
//! for review and regression testing.

use std::fmt::Write as _;

use crate::ir::*;

pub fn render(module: &Module) -> String {
    let mut out = String::new();
    writeln!(out, "module {}", module.name).unwrap();
    if !module.layouts.is_empty() {
        writeln!(out, "  layouts: {}", module.layouts.len()).unwrap();
    }
    for f in &module.functions {
        out.push('\n');
        render_function(f, &mut out);
    }
    out
}

fn render_function(f: &Function, out: &mut String) {
    let params: Vec<String> = f
        .params
        .iter()
        .map(|p| format!("%{} : {}  ({})", p.in_value.0, p.hint.as_str(), p.name))
        .collect();
    writeln!(
        out,
        "function {} ({}) -> {}",
        f.name,
        params.join(", "),
        f.return_hint.as_str()
    )
    .unwrap();
    for block in &f.blocks {
        render_block(block, out);
    }
}

fn render_block(block: &BasicBlock, out: &mut String) {
    writeln!(out, "  bb{}: {}", block.id.0, block.label).unwrap();
    for instr in &block.instrs {
        writeln!(out, "    {}", render_instr(instr)).unwrap();
    }
    writeln!(out, "    {}", render_terminator(&block.terminator)).unwrap();
}

fn render_instr(i: &Instr) -> String {
    match i {
        Instr::Const { dst, value, hint } => {
            format!(
                "%{} = const {} : {}",
                dst.0,
                render_const(value),
                hint.as_str()
            )
        }
        Instr::Alloca { dst, hint, name } => {
            format!("%{} = alloca {} ({})", dst.0, hint.as_str(), name)
        }
        Instr::Load { dst, slot, hint } => {
            format!("%{} = load %{} : {}", dst.0, slot.0, hint.as_str())
        }
        Instr::Store { slot, value } => {
            format!("store {} -> %{}", render_value(value), slot.0)
        }
        Instr::BinOp {
            dst,
            op,
            lhs,
            rhs,
            hint,
        } => {
            format!(
                "%{} = {} {}, {} : {}",
                dst.0,
                op.as_str(),
                render_value(lhs),
                render_value(rhs),
                hint.as_str()
            )
        }
        Instr::UnaryOp {
            dst,
            op,
            operand,
            hint,
        } => {
            format!(
                "%{} = {} {} : {}",
                dst.0,
                op.as_str(),
                render_value(operand),
                hint.as_str()
            )
        }
        Instr::Call {
            dst,
            callee,
            args,
            hint,
        } => {
            let args_str = args
                .iter()
                .map(render_value)
                .collect::<Vec<_>>()
                .join(", ");
            match dst {
                Some(d) => format!(
                    "%{} = call {}({}) : {}",
                    d.0,
                    render_value(callee),
                    args_str,
                    hint.as_str()
                ),
                None => format!("call {}({})", render_value(callee), args_str),
            }
        }
        Instr::New {
            dst,
            class_name,
            args,
        } => {
            let args_str = args
                .iter()
                .map(render_value)
                .collect::<Vec<_>>()
                .join(", ");
            format!("%{} = new {}({})", dst.0, class_name, args_str)
        }
        Instr::FieldLoad {
            dst,
            base,
            byte_offset,
            hint,
        } => format!(
            "%{} = field.load {}, +{} : {}",
            dst.0,
            render_value(base),
            byte_offset,
            hint.as_str()
        ),
        Instr::FieldStore {
            base,
            byte_offset,
            value,
        } => format!(
            "field.store {}, +{}, {}",
            render_value(base),
            byte_offset,
            render_value(value)
        ),
        Instr::IndirectLoad {
            dst,
            addr,
            hint,
            byte_width,
        } => format!(
            "%{} = iload.{}b [{}] : {}",
            dst.0,
            byte_width,
            render_value(addr),
            hint.as_str()
        ),
        Instr::IndirectStore {
            addr,
            value,
            byte_width,
        } => format!(
            "istore.{}b [{}], {}",
            byte_width,
            render_value(addr),
            render_value(value)
        ),
        Instr::GlobalLoad { dst, name, hint } => format!(
            "%{} = gload @{} : {}",
            dst.0,
            name,
            hint.as_str()
        ),
        Instr::GlobalStore { name, value } => format!(
            "gstore @{}, {}",
            name,
            render_value(value)
        ),
        Instr::Gep {
            dst,
            base,
            index,
            element_bytes,
        } => format!(
            "%{} = gep {}, {} * {}",
            dst.0,
            render_value(base),
            render_value(index),
            element_bytes
        ),
        Instr::TypedConstruct {
            dst,
            kind,
            args,
            hint,
        } => {
            let args_str = args
                .iter()
                .map(render_value)
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "%{} = construct {} ({}) : {}",
                dst.0,
                kind.as_str(),
                args_str,
                hint.as_str()
            )
        }
        Instr::LaneExtract {
            dst,
            vector,
            lane,
            kind,
            hint,
        } => format!(
            "%{} = lane {} of {}[{}] : {}",
            dst.0,
            kind.as_str(),
            render_value(vector),
            render_value(lane),
            hint.as_str()
        ),
        Instr::LaneInsert {
            dst,
            vector,
            lane,
            value,
            kind,
        } => format!(
            "%{} = lane_insert {} into {}[{}] := {}",
            dst.0,
            kind.as_str(),
            render_value(vector),
            render_value(lane),
            render_value(value)
        ),
        Instr::MethodCall {
            dst,
            receiver,
            class_name,
            vtable_slot,
            method_name,
            args,
            hint,
        } => {
            let args_str = args
                .iter()
                .map(render_value)
                .collect::<Vec<_>>()
                .join(", ");
            match dst {
                Some(d) => format!(
                    "%{} = vcall {}.{}@{}::slot{}({}) : {}",
                    d.0,
                    render_value(receiver),
                    method_name,
                    class_name,
                    vtable_slot,
                    args_str,
                    hint.as_str()
                ),
                None => format!(
                    "vcall {}.{}@{}::slot{}({})",
                    render_value(receiver),
                    method_name,
                    class_name,
                    vtable_slot,
                    args_str
                ),
            }
        }
        Instr::IndirectMethodCall {
            dst,
            receiver,
            method_name,
            args,
            hint,
        } => {
            let args_str = args
                .iter()
                .map(render_value)
                .collect::<Vec<_>>()
                .join(", ");
            match dst {
                Some(d) => format!(
                    "%{} = vcall_dyn {}.{}({}) : {}",
                    d.0,
                    render_value(receiver),
                    method_name,
                    args_str,
                    hint.as_str()
                ),
                None => format!(
                    "vcall_dyn {}.{}({})",
                    render_value(receiver),
                    method_name,
                    args_str
                ),
            }
        }
    }
}

fn render_terminator(t: &Terminator) -> String {
    match t {
        Terminator::Return(None) => "return".to_string(),
        Terminator::Return(Some(v)) => format!("return {}", render_value(v)),
        Terminator::Branch(b) => format!("br bb{}", b.0),
        Terminator::CondBranch {
            cond,
            then_block,
            else_block,
        } => {
            format!(
                "br {} ? bb{} : bb{}",
                render_value(cond),
                then_block.0,
                else_block.0
            )
        }
        Terminator::Switch {
            value,
            cases,
            default,
        } => {
            let case_strs: Vec<String> = cases
                .iter()
                .map(|(v, b)| format!("{} => bb{}", render_value(v), b.0))
                .collect();
            format!(
                "switch {} {{ {}, default => bb{} }}",
                render_value(value),
                case_strs.join(", "),
                default.0
            )
        }
        Terminator::Unreachable => "unreachable".to_string(),
    }
}

fn render_value(v: &Value) -> String {
    match v {
        Value::Const(c) => render_const(c),
        Value::Local(id) => format!("%{}", id.0),
        Value::Function(name) => format!("@{name}"),
        Value::Unit => "<unit>".to_string(),
    }
}

fn render_const(c: &Const) -> String {
    match c {
        Const::Int(v) => format!("{v}"),
        Const::Float(v) => format!("{v}"),
        Const::Bool(b) => format!("{b}"),
        Const::Null => "?".to_string(),
        Const::String(s) => s.clone(),
    }
}
