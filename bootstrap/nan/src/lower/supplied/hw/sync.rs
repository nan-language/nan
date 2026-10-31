//! The synchronisation primitives of `hw`: `hw::without_irqs`, spin locks, devices behind an
//! IRQ-safe lock, per-CPU values, one-slot signals and the anchor. A lock word holds the number of
//! the CPU holding it plus one, each executor thread being a CPU on `x86_64-host`, so a CPU
//! acquiring a lock it holds halts. Only a machine target has maskable interrupts to disable and
//! interrupts to install a device on.

use cranelift_codegen::ir::{
    AbiParam, InstBuilder, MemFlagsData, Signature, Value, condcodes::IntCC, types,
};
use cranelift_module::FuncId;

use super::super::super::body::Held;
use super::super::super::fx::{Fx, PTR};
use super::super::super::machine::{Stub, device, msi};
use super::super::super::runtime::regions::WAITING;
use super::super::super::runtime::tasks::{self, thread};
use super::super::super::runtime::{Routine, holdings, report, settle, state};
use super::super::super::{Glue, Lowering};
use super::super::{Supply, tasks as task_ops};
use super::forward;
use crate::check::ty::{FnTy, Ty};

/// `hw::without_irqs(body)`: calls `body` with a token, maskable interrupts disabled around it on
/// a machine target and their state restored after.
pub fn without_irqs(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let flags = irq_save(lo, fx);
    let body_ty = super::super::mem::lists::fn_param(supply, 0);
    let called = lo.call_value(fx, &body_ty, supply.param(0).address(), &[Held::Empty]);
    irq_restore(lo, fx, flags);
    forward(lo, fx, supply, called);
}

/// Disables maskable interrupts on a machine target, giving the flags to restore.
fn irq_save(lo: &mut Lowering, fx: &mut Fx) -> Option<Value> {
    lo.machine()
        .then(|| lo.call_stub(fx, Stub::IrqSave, &[])[0])
}

fn irq_restore(lo: &mut Lowering, fx: &mut Fx, flags: Option<Value>) {
    if let Some(flags) = flags {
        lo.call_stub(fx, Stub::IrqRestore, &[flags]);
    }
}

/// Acquires a lock word for the running CPU, spinning while another CPU holds it; when the
/// running CPU holds it already the kernel halts, located at the operation's call when `located`
/// holds.
fn acquire(lo: &mut Lowering, fx: &mut Fx, word: Value, located: bool) {
    let index = running_index(lo, fx);
    let holder = fx.b.ins().iadd_imm_s(index, 1);
    fx.repeat(&[], |fx, _| {
        let free = fx.ptr(0);
        let seen =
            fx.b.ins()
                .atomic_cas(MemFlagsData::trusted(), word, free, holder);
        let held = fx.b.ins().icmp_imm_s(IntCC::NotEqual, seen, 0);
        fx.when(held, |fx| {
            let again = fx.b.ins().icmp(IntCC::Equal, seen, holder);
            fx.when(again, |fx| {
                let location = match located {
                    true => report::caller_location(lo, fx),
                    false => fx.ptr(0),
                };
                lo.call_routine(fx, Routine::Reacquired, &[location]);
            });
            lo.relax(fx);
        });
        (held, Vec::new())
    });
}

fn release_word(fx: &mut Fx, word: Value) {
    let free = fx.ptr(0);
    fx.b.ins().atomic_store(MemFlagsData::trusted(), free, word);
}

/// The number of the CPU, or executor thread, running the caller.
fn running_index(lo: &mut Lowering, fx: &mut Fx) -> Value {
    let record = lo.thread(fx);
    fx.load(PTR, record, thread::INDEX)
}

/// The offset of a field of the operation's owner type.
fn owner_field(lo: &mut Lowering, supply: &Supply, name: &str) -> u64 {
    let ty = supply.owner.clone().expect("an owner type");
    lo.layouts.field(lo.ck, &ty, None, name).offset
}

/// The spin lock: its lock word, then its value.
pub fn spin_lock(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let value_ty = supply.type_arg(0);
    let word = owner_field(lo, supply, "lock");
    let value_at = owner_field(lo, supply, "value");
    match supply.member() {
        "of" => {
            let sret = supply.sret.expect("a lock's address");
            let free = fx.ptr(0);
            fx.store(free, sret, word);
            let value = fx.offset(sret, value_at);
            lo.store_held(fx, value, supply.param(0), &value_ty);
            supply.done(lo, fx);
        }
        "with" => {
            let lock = supply.param(0).address();
            let word = fx.offset(lock, word);
            let value = fx.offset(lock, value_at);
            acquire(lo, fx, word, true);
            let body_ty = super::super::mem::lists::fn_param(supply, 2);
            let called = lo.call_value(
                fx,
                &body_ty,
                supply.param(2).address(),
                &[Held::Address(value)],
            );
            release_word(fx, word);
            forward(lo, fx, supply, called);
        }
        member => panic!("no lowering of hw::SpinLock.{member}"),
    }
}

/// The offset of a device's value in its record, past the record's fields at its alignment.
pub fn device_value(lo: &mut Lowering, value_ty: &Ty) -> u64 {
    let align = lo.layout(value_ty).align;
    device::VALUE.next_multiple_of(align.max(1))
}

/// A device behind an IRQ-safe lock, which is the address of a runtime record holding it:
/// `install`, `install_on` and `install_msi` move the device into a new record, and on a machine
/// target install it on its interrupt, `install_msi` giving the device back in its refusal
/// otherwise; the running task's domain then owns the record as a holding. `message` reads the
/// message the record holds; `with` takes the record's lock around its body; `uninstall` ends
/// the holding, removes the device from its interrupt and waits until no CPU runs its interrupt
/// fn, then moves it out of the freed record. A CPU number past the CPUs traps.
pub fn irq_shared(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let value_ty = supply.type_arg(0);
    let value_at = device_value(lo, &value_ty);
    match supply.member() {
        "install" => {
            let record = device_record(lo, fx, supply, &value_ty, 2);
            if lo.machine() {
                let vector = supply.param(1).scalar();
                let vector = fx.b.ins().uextend(PTR, vector);
                lo.install_device(fx, record, None, vector);
            }
            return_shared(lo, fx, supply, record);
        }
        "install_on" => {
            let cpu = cpu_number(lo, fx, supply);
            let record = device_record(lo, fx, supply, &value_ty, 3);
            if lo.machine() {
                let vector = supply.param(2).scalar();
                let vector = fx.b.ins().uextend(PTR, vector);
                lo.install_device(fx, record, Some(cpu), vector);
            }
            return_shared(lo, fx, supply, record);
        }
        "install_msi" => install_msi(lo, fx, supply, &value_ty, false),
        "install_validated" => install_msi(lo, fx, supply, &value_ty, true),
        "message" => {
            let record = fx.load(PTR, supply.param(0).address(), 0);
            let option = supply.abi.result.clone();
            let address = fx.load(PTR, record, device::MESSAGE);
            let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, address, 0);
            fx.when(present, |fx| {
                let message_ty = option.type_arg(0).cloned().expect("a message type");
                let layout = lo.layout(&message_ty);
                let message = fx.slot(layout.size, layout.align);
                let at = lo.layouts.field(lo.ck, &message_ty, None, "address").offset;
                fx.store(address, message, at);
                let data = fx.load(types::I32, record, device::DATA);
                let at = lo.layouts.field(lo.ck, &message_ty, None, "data").offset;
                fx.store(data, message, at);
                let some = lo.variant_value(fx, &option, "Some", &[Held::Address(message)], None);
                supply.ret(lo, fx, some);
            });
            let none = lo.none_value(fx, &option);
            supply.ret(lo, fx, none);
        }
        "with" => {
            let record = fx.load(PTR, supply.param(0).address(), 0);
            let word = fx.offset(record, device::LOCK);
            let value = fx.offset(record, value_at);
            acquire(lo, fx, word, true);
            let body_ty = super::super::mem::lists::fn_param(supply, 2);
            let called = lo.call_value(
                fx,
                &body_ty,
                supply.param(2).address(),
                &[Held::Address(value)],
            );
            release_word(fx, word);
            forward(lo, fx, supply, called);
        }
        "uninstall" => {
            let record = fx.load(PTR, supply.param(0).address(), 0);
            let holding = fx.load(PTR, record, device::HOLDING);
            lo.call_routine(fx, Routine::Unhold, &[holding]);
            if lo.machine() {
                lo.uninstall_device(fx, record);
            }
            let held = take_device(lo, fx, &value_ty, record);
            supply.ret(lo, fx, held);
        }
        member => panic!("no lowering of hw::IrqShared.{member}"),
    }
}

/// The CPU number an install names, as a word, trapping when it is not below the CPU count.
fn cpu_number(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) -> Value {
    let cpu = supply.param(1).scalar();
    let cpu = fx.b.ins().uextend(PTR, cpu);
    let state_address = lo.state(fx);
    let count = fx.load(PTR, state_address, state::THREAD_COUNT);
    let outside =
        fx.b.ins()
            .icmp(IntCC::UnsignedGreaterThanOrEqual, cpu, count);
    fx.when(outside, |fx| report::raise_kind(lo, fx, "trap.bounds"));
    cpu
}

/// A new device record holding the device an install takes, with the interrupt fn its parameter
/// `handler` gives and the glue calling it.
fn device_record(
    lo: &mut Lowering,
    fx: &mut Fx,
    supply: &Supply,
    value_ty: &Ty,
    handler: usize,
) -> Value {
    let value_at = device_value(lo, value_ty);
    let size = value_at + lo.layout(value_ty).size;
    let size = fx.ptr(size);
    let record = lo.record_alloc(fx, size);
    let missing = fx.b.ins().icmp_imm_s(IntCC::Equal, record, 0);
    fx.when(missing, |fx| report::raise_kind(lo, fx, "trap.exhausted"));
    let value = fx.offset(record, value_at);
    lo.store_held(fx, value, supply.param(0), value_ty);
    let Ty::Fn(handler_ty) = supply.abi.params[handler].0.ty() else {
        panic!("an interrupt fn handling the device");
    };
    let handler_layout = lo.layout(&Ty::Fn(handler_ty.clone()));
    let handler_at = fx.offset(record, device::HANDLER);
    lo.copy(
        fx,
        handler_at,
        supply.param(handler).address(),
        handler_layout.size,
        handler_layout.align,
    );
    let glue = device_glue(lo, value_ty, handler_ty);
    let glue = lo.func_ref(fx, glue);
    let glue = fx.b.ins().func_addr(PTR, glue);
    fx.store(glue, record, device::GLUE);
    record
}

/// Returns the `hw::IrqShared` naming a device record installed.
fn return_shared(lo: &mut Lowering, fx: &mut Fx, supply: &Supply, record: Value) {
    hold(lo, fx, record);
    let sret = supply.sret.expect("a shared device's slot");
    fx.store(record, sret, 0);
    supply.ret(lo, fx, Held::Address(sret));
}

/// Makes a device record installed a holding of the running task's domain.
fn hold(lo: &mut Lowering, fx: &mut Fx, record: Value) {
    let kind = fx.ptr(holdings::kind::UNINSTALL as u64);
    let holding = lo.call_routine(fx, Routine::Hold, &[kind, record])[0];
    fx.store(holding, record, device::HOLDING);
}

/// Moves the device out of a device record, which it frees.
fn take_device(lo: &mut Lowering, fx: &mut Fx, value_ty: &Ty, record: Value) -> Held {
    let layout = lo.layout(value_ty);
    let kept = fx.slot(layout.size, layout.align);
    let value_at = device_value(lo, value_ty);
    let value = fx.offset(record, value_at);
    lo.copy(fx, kept, value, layout.size, layout.align);
    lo.record_free(fx, record);
    lo.held_at(fx, value_ty, kept)
}

/// `install_msi`, and `install_validated` whose interrupt its source alone raises: `Ok` of the
/// shared device once installed on a message-signalled interrupt of a machine target, or `Err`
/// of a refusal giving back the device with the `hw::MsiError` variant of the outcome,
/// `Unsupported` on the host.
fn install_msi(lo: &mut Lowering, fx: &mut Fx, supply: &Supply, value_ty: &Ty, validated: bool) {
    let cpu = cpu_number(lo, fx, supply);
    let record = device_record(lo, fx, supply, value_ty, 3);
    let outcome = match lo.machine() {
        true => {
            let source = supply.param(2).scalar();
            let source = fx.b.ins().uextend(PTR, source);
            lo.install_msi(fx, record, cpu, source, validated)
        }
        false => fx.ptr(msi::UNSUPPORTED as u64),
    };
    let result_ty = supply.abi.result.clone();
    let installed = fx.b.ins().icmp_imm_s(IntCC::Equal, outcome, msi::INSTALLED);
    fx.when(installed, |fx| {
        hold(lo, fx, record);
        let shared = fx.slot(8, 8);
        fx.store(record, shared, 0);
        let ok = lo.variant_value(fx, &result_ty, "Ok", &[Held::Address(shared)], None);
        supply.ret(lo, fx, ok);
    });
    let refusal_ty = result_ty.type_arg(1).cloned().expect("a refusal type");
    let error_ty = lo.layouts.field(lo.ck, &refusal_ty, None, "error").ty;
    let layout = lo.layout(&refusal_ty);
    let refusal = fx.slot(layout.size, layout.align);
    let dev_at = lo.layouts.field(lo.ck, &refusal_ty, None, "dev").offset;
    let error_at = lo.layouts.field(lo.ck, &refusal_ty, None, "error").offset;
    let dev = take_device(lo, fx, value_ty, record);
    let dev_address = fx.offset(refusal, dev_at);
    lo.store_held(fx, dev_address, dev, value_ty);
    let error_address = fx.offset(refusal, error_at);
    for (variant, kind) in [
        ("Exhausted", msi::EXHAUSTED),
        ("Unreachable", msi::UNREACHABLE),
    ] {
        let this = fx.b.ins().icmp_imm_s(IntCC::Equal, outcome, kind);
        fx.when(this, |fx| {
            let error = lo.variant_value(fx, &error_ty, variant, &[], None);
            lo.store_held(fx, error_address, error, &error_ty);
            let err = lo.variant_value(fx, &result_ty, "Err", &[Held::Address(refusal)], None);
            supply.ret(lo, fx, err);
        });
    }
    let error = lo.variant_value(fx, &error_ty, "Unsupported", &[], None);
    lo.store_held(fx, error_address, error, &error_ty);
    let err = lo.variant_value(fx, &result_ty, "Err", &[Held::Address(refusal)], None);
    supply.ret(lo, fx, err);
}

/// The glue the dispatcher calls with a device's record when its interrupt fires: it takes the
/// record's lock, calls the interrupt fn with the device and a new `hw::IrqContext`, releases
/// the lock and gives whether the fn's `hw::IrqExit` is `Reschedule`.
fn device_glue(lo: &mut Lowering, value_ty: &Ty, handler_ty: &FnTy) -> FuncId {
    let mut signature = Signature::new(super::super::super::CALL_CONV);
    signature.params.push(AbiParam::new(PTR));
    signature.returns.push(AbiParam::new(types::I8));
    let glue = Glue::IrqDevice(value_ty.clone(), handler_ty.clone());
    let value_ty = value_ty.clone();
    let handler_ty = handler_ty.clone();
    lo.glue_function(glue, signature, |lo, fx| {
        let record = fx.entry()[0];
        let value_at = device_value(lo, &value_ty);
        let word = fx.offset(record, device::LOCK);
        acquire(lo, fx, word, false);
        let handler = fx.offset(record, device::HANDLER);
        let value = fx.offset(record, value_at);
        let called = lo.call_value(
            fx,
            &handler_ty,
            handler,
            &[Held::Address(value), Held::Empty],
        );
        let reschedule = lo.reschedules(fx, &handler_ty.result, called.result);
        release_word(fx, word);
        fx.b.ins().return_(&[reschedule]);
    })
}

/// The bytes each CPU's slot of a per-CPU value takes, and the offset of its value in the slot,
/// after a word set while its CPU is inside its `with`.
fn per_cpu_slot(lo: &mut Lowering, value_ty: &Ty) -> (u64, u64) {
    let layout = lo.layout(value_ty);
    let value_at = layout.align.max(8);
    let stride = (value_at + layout.size).next_multiple_of(value_at);
    (stride, value_at)
}

/// A per-CPU value: a slot for each CPU, drawn through the allocator, holding its value.
pub fn per_cpu(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let value_ty = supply.type_arg(0);
    let (stride, value_at) = per_cpu_slot(lo, &value_ty);
    match supply.member() {
        "of" => {
            let state_address = lo.state(fx);
            let count = fx.load(PTR, state_address, state::THREAD_COUNT);
            let cell = fx.load(PTR, supply.param(1).address(), 0);
            let size = fx.b.ins().imul_imm_s(count, stride as i64);
            let align = fx.ptr(value_at);
            let storage = lo.call_routine(fx, Routine::Alloc, &[cell, size, align])[0];
            let missing = fx.b.ins().icmp_imm_s(IntCC::Equal, storage, 0);
            fx.when(missing, |fx| report::raise_kind(lo, fx, "trap.exhausted"));
            let init_ty = super::super::mem::lists::fn_param(supply, 0);
            let zero = fx.ptr(0);
            fx.each(zero, count, |fx, index| {
                let slot = fx.b.ins().imul_imm_s(index, stride as i64);
                let slot = fx.b.ins().iadd(storage, slot);
                let outside = fx.ptr(0);
                fx.store(outside, slot, 0);
                let cpu = fx.b.ins().ireduce(types::I32, index);
                let called = lo.call_value(
                    fx,
                    &init_ty,
                    supply.param(0).address(),
                    &[Held::Scalar(cpu)],
                );
                let value = fx.offset(slot, value_at);
                lo.store_held(fx, value, called.result, &value_ty);
            });
            let sret = supply.sret.expect("a per-CPU value's slot");
            fx.store(storage, sret, 0);
            supply.ret(lo, fx, Held::Address(sret));
        }
        "with" => {
            let storage = fx.load(PTR, supply.param(0).address(), 0);
            let index = running_index(lo, fx);
            let slot = fx.b.ins().imul_imm_s(index, stride as i64);
            let slot = fx.b.ins().iadd(storage, slot);
            let inside = fx.load(PTR, slot, 0);
            let again = fx.b.ins().icmp_imm_s(IntCC::NotEqual, inside, 0);
            fx.when(again, |fx| {
                let location = report::caller_location(lo, fx);
                lo.call_routine(fx, Routine::Reacquired, &[location]);
            });
            let one = fx.ptr(1);
            fx.store(one, slot, 0);
            let value = fx.offset(slot, value_at);
            let body_ty = super::super::mem::lists::fn_param(supply, 2);
            let called = lo.call_value(
                fx,
                &body_ty,
                supply.param(2).address(),
                &[Held::Address(value)],
            );
            let outside = fx.ptr(0);
            fx.store(outside, slot, 0);
            forward(lo, fx, supply, called);
        }
        member => panic!("no lowering of hw::PerCpu.{member}"),
    }
}

/// Calls `each` with the address of every CPU's value of the per-CPU value at `address`, and
/// gives its storage.
fn each_per_cpu(
    lo: &mut Lowering,
    fx: &mut Fx,
    value_ty: &Ty,
    address: Value,
    mut each: impl FnMut(&mut Lowering, &mut Fx, Value),
) -> Value {
    let (stride, value_at) = per_cpu_slot(lo, value_ty);
    let storage = fx.load(PTR, address, 0);
    let state_address = lo.state(fx);
    let count = fx.load(PTR, state_address, state::THREAD_COUNT);
    let zero = fx.ptr(0);
    fx.each(zero, count, |fx, index| {
        let slot = fx.b.ins().imul_imm_s(index, stride as i64);
        let slot = fx.b.ins().iadd(storage, slot);
        let value = fx.offset(slot, value_at);
        each(lo, fx, value);
    });
    storage
}

/// Releases a per-CPU value at an address: each CPU's value, then its storage.
pub fn release_per_cpu(lo: &mut Lowering, fx: &mut Fx, value_ty: &Ty, address: Value) {
    let storage = each_per_cpu(lo, fx, value_ty, address, |lo, fx, value| {
        lo.release(fx, value_ty, value);
    });
    lo.call_routine(fx, Routine::Free, &[storage]);
}

/// Moves a per-CPU value at an address into another domain's reach: its storage, then each
/// CPU's value.
pub fn transfer_per_cpu(
    lo: &mut Lowering,
    fx: &mut Fx,
    value_ty: &Ty,
    address: Value,
    lift: Value,
    owner: Value,
) {
    let storage = fx.load(PTR, address, 0);
    super::super::super::transfer::lift(lo, fx, storage, lift);
    each_per_cpu(lo, fx, value_ty, address, |lo, fx, value| {
        lo.transfer(fx, value_ty, value, lift, owner);
    });
}

/// `hw::Anchor[S].with(body)`: calls `body` with the installed anchor and returns `Some` of its
/// result, or `None` when none is installed, which on the host is always. An anchor is installed
/// only on a machine kernel, in storage the image reserves, whose address the runtime records.
pub fn anchor_with(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let option = supply.abi.result.clone();
    if lo.machine() {
        let state_address = lo.state(fx);
        let anchor = fx.load(PTR, state_address, state::ANCHOR);
        let installed = fx.b.ins().icmp_imm_s(IntCC::NotEqual, anchor, 0);
        fx.when(installed, |fx| {
            let body_ty = super::super::mem::lists::fn_param(supply, 0);
            let called = lo.call_value(
                fx,
                &body_ty,
                supply.param(0).address(),
                &[Held::Address(anchor)],
            );
            supply.forward_failure(lo, fx, &called);
            let some = lo.variant_value(fx, &option, "Some", &[called.result], None);
            supply.ret(lo, fx, some);
        });
    }
    let none = lo.none_value(fx, &option);
    supply.ret(lo, fx, none);
}

/// The field offsets of a `hw::Signal[T]`: its lock word, whether it holds a value, its waiting
/// task and its value.
struct SignalFields {
    lock: u64,
    stored: u64,
    waiter: u64,
    value: u64,
}

fn signal_fields(lo: &mut Lowering, ty: &Ty) -> SignalFields {
    let mut field = |name: &str| lo.layouts.field(lo.ck, ty, None, name).offset;
    SignalFields {
        lock: field("lock"),
        stored: field("stored"),
        waiter: field("waiter"),
        value: field("value"),
    }
}

/// A one-slot signal: `post` stores a value, replacing one it holds, and wakes the task waiting
/// on it; `wait` takes the value, or else becomes the signal's waiter, reachable by
/// cancellation, until a post or a cancellation wakes it. A wait cancelled at its start fails at
/// once, and a second task waiting traps.
pub fn signal(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let value_ty = supply.type_arg(0);
    let signal_ty = supply.owner.clone().expect("a signal type");
    let fields = signal_fields(lo, &signal_ty);
    match supply.member() {
        "empty" => {
            let sret = supply.sret.expect("a signal's address");
            let zero = fx.ptr(0);
            fx.store(zero, sret, fields.lock);
            fx.store(zero, sret, fields.waiter);
            let absent = fx.int(types::I8, 0);
            fx.store(absent, sret, fields.stored);
            supply.ret(lo, fx, Held::Address(sret));
        }
        "post" => {
            let address = supply.param(0).address();
            let word = fx.offset(address, fields.lock);
            let flags = irq_save(lo, fx);
            acquire(lo, fx, word, true);
            let value_at = fx.offset(address, fields.value);
            if lo.needs_release(&value_ty) {
                let stored = fx.load(types::I8, address, fields.stored);
                let replaced = fx.b.ins().icmp_imm_s(IntCC::NotEqual, stored, 0);
                fx.when(replaced, |fx| lo.release(fx, &value_ty, value_at));
            }
            lo.store_held(fx, value_at, supply.param(2), &value_ty);
            let present = fx.int(types::I8, 1);
            fx.store(present, address, fields.stored);
            let waiter = fx.load(PTR, address, fields.waiter);
            let none = fx.ptr(0);
            fx.store(none, address, fields.waiter);
            release_word(fx, word);
            irq_restore(lo, fx, flags);
            let waiting = fx.b.ins().icmp_imm_s(IntCC::NotEqual, waiter, 0);
            fx.when(waiting, |fx| {
                lo.call_routine(fx, Routine::Wake, &[waiter]);
            });
            supply.done(lo, fx);
        }
        "wait" => wait(lo, fx, supply, &signal_ty, &fields),
        member => panic!("no lowering of hw::Signal.{member}"),
    }
}

/// `signal.wait()`, as `signal` describes it.
fn wait(lo: &mut Lowering, fx: &mut Fx, supply: &Supply, signal_ty: &Ty, fields: &SignalFields) {
    let value_ty = supply.type_arg(0);
    let address = supply.param(0).address();
    let word = fx.offset(address, fields.lock);
    let early = task_ops::origin(lo, fx);
    let cancelled = fx.b.ins().icmp_imm_s(IntCC::NotEqual, early, 0);
    fx.when(cancelled, |fx| {
        task_ops::fail_cancelled(lo, fx, supply, early)
    });
    let current = tasks::current_task(lo, fx);
    let layout = lo.layout(&value_ty);
    let taken = fx.slot(layout.size.max(1), layout.align.max(1));
    let withdraw = withdraw_waiter(lo, signal_ty);
    fx.repeat(&[], |fx, _| {
        let flags = irq_save(lo, fx);
        acquire(lo, fx, word, true);
        let stored = fx.load(types::I8, address, fields.stored);
        let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, stored, 0);
        fx.when(present, |fx| {
            let value_at = fx.offset(address, fields.value);
            lo.copy(fx, taken, value_at, layout.size, layout.align);
            let absent = fx.int(types::I8, 0);
            fx.store(absent, address, fields.stored);
            release_word(fx, word);
            irq_restore(lo, fx, flags);
            let held = lo.held_at(fx, &value_ty, taken);
            supply.ret(lo, fx, held);
        });
        let waiter = fx.load(PTR, address, fields.waiter);
        let other = fx.b.ins().icmp_imm_s(IntCC::NotEqual, waiter, 0);
        fx.when(other, |fx| {
            release_word(fx, word);
            irq_restore(lo, fx, flags);
            report::raise_kind(lo, fx, "trap.duplicate-access");
        });
        fx.store(current, address, fields.waiter);
        tasks::waiting_on(lo, fx, WAITING);
        release_word(fx, word);
        irq_restore(lo, fx, flags);
        settle::enter_wait(lo, fx, withdraw, address, current);
        lo.call_routine(fx, Routine::Park, &[]);
        settle::leave_wait(lo, fx);
        tasks::waiting_on(lo, fx, 0);
        let origin = task_ops::origin(lo, fx);
        let cancelled = fx.b.ins().icmp_imm_s(IntCC::NotEqual, origin, 0);
        fx.when(cancelled, |fx| {
            let flags = irq_save(lo, fx);
            acquire(lo, fx, word, true);
            let waiter = fx.load(PTR, address, fields.waiter);
            let ours = fx.b.ins().icmp(IntCC::Equal, waiter, current);
            fx.when(ours, |fx| {
                let none = fx.ptr(0);
                fx.store(none, address, fields.waiter);
            });
            release_word(fx, word);
            irq_restore(lo, fx, flags);
            task_ops::fail_cancelled(lo, fx, supply, origin);
        });
        let again = fx.int(types::I8, 1);
        (again, Vec::new())
    });
    fx.b.ins()
        .trap(cranelift_codegen::ir::TrapCode::user(1).expect("a trap code"));
}

/// The glue withdrawing a stopped task's wait on a signal of a type, which the wait records.
fn withdraw_waiter(lo: &mut Lowering, signal_ty: &Ty) -> FuncId {
    let signal_ty = signal_ty.clone();
    lo.glue_function(
        Glue::WithdrawSignal(signal_ty.clone()),
        settle::withdraw_signature(),
        move |lo, fx| {
            let params = fx.entry();
            let fields = signal_fields(lo, &signal_ty);
            let address = fx.load(PTR, params[0], settle::wait::OBJECT);
            let word = fx.offset(address, fields.lock);
            let flags = irq_save(lo, fx);
            acquire(lo, fx, word, false);
            let waiter = fx.load(PTR, address, fields.waiter);
            let stopped = fx.b.ins().icmp(IntCC::Equal, waiter, params[1]);
            fx.when(stopped, |fx| {
                let none = fx.ptr(0);
                fx.store(none, address, fields.waiter);
            });
            release_word(fx, word);
            irq_restore(lo, fx, flags);
            fx.b.ins().return_(&[]);
        },
    )
}

/// Releases a signal at an address: its value, when it holds one.
pub fn release_signal(lo: &mut Lowering, fx: &mut Fx, ty: &Ty, address: Value) {
    let value_ty = ty.type_arg(0).cloned().expect("a signalled type");
    let fields = signal_fields(lo, ty);
    let stored = fx.load(types::I8, address, fields.stored);
    let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, stored, 0);
    fx.when(present, |fx| {
        let value = fx.offset(address, fields.value);
        lo.release(fx, &value_ty, value);
    });
}

/// Moves a signal at an address into another domain's reach: its value, when it holds one.
pub fn transfer_signal(
    lo: &mut Lowering,
    fx: &mut Fx,
    ty: &Ty,
    address: Value,
    lift: Value,
    owner: Value,
) {
    let value_ty = ty.type_arg(0).cloned().expect("a signalled type");
    let fields = signal_fields(lo, ty);
    let stored = fx.load(types::I8, address, fields.stored);
    let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, stored, 0);
    fx.when(present, |fx| {
        let value = fx.offset(address, fields.value);
        lo.transfer(fx, &value_ty, value, lift, owner);
    });
}
