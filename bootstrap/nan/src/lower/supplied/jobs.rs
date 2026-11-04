//! Jobs a kernel runs from the bundles of nanos programs: the call of a loaded bundle's job entry,
//! the rendering of a job's failure into a buffer, and the `host::World` a host-compatible entry
//! receives; in an isolated space, the runtime's own grants, its calls of the job entry and the
//! system calls through which it reaches its kernel; and for its kernel, the registers of those
//! calls and the traps a space reports.

use cranelift_codegen::ir::{AbiParam, InstBuilder, Signature, Value, condcodes::IntCC};

use super::super::Lowering;
use super::super::body::Held;
use super::super::fx::{Fx, PTR};
use super::super::runtime::tables::mint_base;
use super::super::runtime::tasks::trap_record;
use super::super::runtime::{Routine, report, space};
use super::super::trap::{KINDS, kind_number};
use super::sys::view;
use super::{Supply, render};
use crate::check::ty::Ty;
use crate::workspace::Arch;

/// The fields of a `host::World` that `nanos::host_world` receives, in its parameters' order.
const HOST_FIELDS: [&str; 11] = [
    "fs",
    "net",
    "clock",
    "wall",
    "random",
    "log",
    "stdio",
    "alloc",
    "exec",
    "args",
    "environment",
];

pub fn lower(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) -> bool {
    match supply.path.as_str() {
        "nanos::call" => call(lo, fx, supply),
        "nanos::render_into" => {
            let (buffer, len) = view(fx, supply.param(1));
            let renderer = render::new_renderer(fx, buffer, len);
            let ty = supply.type_arg(0);
            let args = [supply.param(0), Held::Address(renderer)];
            lo.call_method(fx, &ty, "Render", Vec::new(), &args);
            let written = render::finish(lo, fx, renderer);
            supply.ret(lo, fx, Held::Scalar(written));
        }
        "nanos::space_base" => {
            let base = mint_base(lo, fx);
            let ty = supply.abi.result.clone();
            let sret = supply.sret.expect("a base's address");
            for (name, value) in [
                ("alloc", base.cell),
                ("exec", base.exec),
                ("clock", base.clock),
            ] {
                let at = lo.layouts.field(lo.ck, &ty, None, name).offset;
                fx.store(value, sret, at);
            }
            supply.ret(lo, fx, Held::Address(sret));
        }
        "nanos::invoke" => {
            let (args, args_len) = view(fx, supply.param(2));
            let (result, capacity) = view(fx, supply.param(3));
            let call = [
                supply.param(0).scalar(),
                supply.param(1).scalar(),
                args,
                args_len,
                result,
                capacity,
            ];
            let given = space::call(lo, fx, space::INVOKE, &call);
            supply.ret(lo, fx, Held::Scalar(given));
        }
        "nanos::end" => {
            let (bytes, len) = view(fx, supply.param(2));
            let call = [
                supply.param(0).scalar(),
                supply.param(1).scalar(),
                bytes,
                len,
            ];
            space::call(lo, fx, space::END, &call);
            supply.done(lo, fx);
        }
        "nanos::trap_number" => {
            let record = word_of(fx, supply.param(0));
            let kind = fx.load(PTR, record, trap_record::KIND);
            supply.ret(lo, fx, Held::Scalar(kind));
        }
        "nanos::trap_of" => trap_of(lo, fx, supply),
        "nanos::register" => {
            let slots = match lo.ck.db.target.arch() {
                Some(Arch::Aarch64) => A64_REGISTERS,
                _ => X64_REGISTERS,
            };
            let role = supply.param(0).scalar();
            let mut register = fx.ptr(slots[0]);
            for (index, slot) in slots.iter().enumerate().skip(1) {
                let this = fx.b.ins().icmp_imm_s(IntCC::Equal, role, index as i64);
                let value = fx.ptr(*slot);
                register = fx.b.ins().select(this, value, register);
            }
            supply.ret(lo, fx, Held::Scalar(register));
        }
        "nanos::host_world" => {
            let world = supply.abi.result.clone();
            let sret = supply.sret.expect("a world's address");
            for (index, name) in HOST_FIELDS.iter().enumerate() {
                let field = lo.layouts.field(lo.ck, &world, None, name);
                let at = fx.offset(sret, field.offset);
                lo.store_held(fx, at, supply.param(index), &field.ty);
            }
            supply.ret(lo, fx, Held::Address(sret));
        }
        _ => return false,
    }
    true
}

/// The index in an `hw::UserState`'s registers of each argument of a system call, of its number
/// and of its result: on x86_64 `rdi`, `rsi`, `rdx`, `r10`, `r8`, `r9`, then `rax` for both; on
/// AArch64 `x0` to `x5`, `x8`, then `x0`.
const X64_REGISTERS: [u64; 8] = [6, 5, 2, 9, 7, 8, 0, 0];
const A64_REGISTERS: [u64; 8] = [0, 1, 2, 3, 4, 5, 8, 0];

/// `nanos::trap_of(kind, alloc)`: a trap of the kind its number names, `trap.abandoned` for a
/// number naming none, its record drawn through `alloc` and its report locating the call.
fn trap_of(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let kind = supply.param(0).scalar();
    let known =
        fx.b.ins()
            .icmp_imm_s(IntCC::UnsignedLessThan, kind, KINDS.len() as i64);
    let abandoned = fx.ptr(kind_number("trap.abandoned") as u64);
    let kind = fx.b.ins().select(known, kind, abandoned);
    let cell = word_of(fx, supply.param(1));
    let size = fx.ptr(trap_record::BYTES + report::REPORT);
    let align = fx.ptr(8);
    let record = lo.call_routine(fx, Routine::Alloc, &[cell, size, align])[0];
    let missing = fx.b.ins().icmp_imm_s(IntCC::Equal, record, 0);
    fx.when(missing, |fx| report::raise_kind(lo, fx, "trap.exhausted"));
    lo.call_routine(fx, Routine::TrapReport, &[kind, record]);
    let slot = fx.slot(8, 8);
    fx.store(record, slot, 0);
    let trap = lo.held_at(fx, &supply.abi.result, slot);
    supply.ret(lo, fx, trap);
}

/// The word a one-word value holds.
fn word_of(fx: &mut Fx, held: Held) -> Value {
    match held {
        Held::Scalar(word) => word,
        held => fx.load(PTR, held.address(), 0),
    }
}

/// `nanos::call(job, world, input)`: calls the job entry's code with the addresses of the world,
/// of the input and of the slot it writes how the job ended into.
fn call(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let code = fx.load(PTR, supply.param(0).address(), 0);
    let ended: Ty = supply.abi.result.clone();
    let layout = lo.layout(&ended);
    let slot = supply
        .sret
        .unwrap_or_else(|| fx.slot(layout.size, layout.align));
    let mut signature = Signature::new(super::super::CALL_CONV);
    signature.params.extend([AbiParam::new(PTR); 3]);
    let signature = fx.b.import_signature(signature);
    let world = supply.param(1).address();
    let input = supply.param(2).address();
    fx.b.ins()
        .call_indirect(signature, code, &[world, input, slot]);
    let held = lo.held_at(fx, &ended, slot);
    supply.ret(lo, fx, held);
}
