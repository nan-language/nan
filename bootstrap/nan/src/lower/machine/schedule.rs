//! A kernel's scheduling: each CPU's queue of ready tasks, ordered by key and first in, first
//! out among equal keys; the placement of a task made ready on a CPU's queue, which sends that
//! CPU the CPU signal when it idles or runs a task of a greater key; the stealing and idling of a
//! CPU whose queue is empty; the slice of a task switched in; and the kernel's policy, whose hooks
//! decide them. `boot::Kernel.policy` installs a policy as a record of the glue calling each hook
//! and the policy's value. Without one the default policy places a task on the CPU it last ran on
//! when that CPU idles, else on the first CPU idling, else on the running one, keyed by the order
//! in which tasks become ready; steals from the next CPUs in turn; gives no slice; and halts.

use cranelift_codegen::ir::{
    AbiParam, InstBuilder, MemFlagsData, Signature, Value, condcodes::IntCC, types,
};
use cranelift_module::FuncId;

use super::super::body::Held;
use super::super::call::Invoked;
use super::super::fx::{Fx, PTR};
use super::super::layout::Shape;
use super::super::runtime::tasks::{operation, task};
use super::super::runtime::{Routine, fetch_add, report, state, timers};
use super::super::supplied::Supply;
use super::super::supplied::tasks::accounts;
use super::super::{CALL_CONV, Glue, Lowering};
use super::cpus::cpu;
use super::stubs::Stub;
use crate::check::ty::{Arg, Ty};
use crate::nir::{Callable, FnRef};

/// A hook of a kernel policy, which the glue the policy record holds at its offset calls.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Hook {
    /// `(policy, task) -> (cpu, key)`.
    Place,
    /// `(policy, cpu, attempt) -> (found, victim)`.
    Steal,
    /// `(policy, cpu, task) -> nanoseconds`.
    Slice,
    /// `(policy, cpu, timed, until) -> polls`.
    Idle,
}

impl Hook {
    const ALL: [Hook; 4] = [Hook::Place, Hook::Steal, Hook::Slice, Hook::Idle];

    fn name(self) -> &'static str {
        match self {
            Hook::Place => "place",
            Hook::Steal => "steal",
            Hook::Slice => "slice",
            Hook::Idle => "idle",
        }
    }

    fn offset(self) -> u64 {
        8 * self as u64
    }

    fn signature(self) -> Signature {
        let (params, returns) = match self {
            Hook::Place => (2, 2),
            Hook::Steal => (3, 2),
            Hook::Slice => (3, 1),
            Hook::Idle => (4, 1),
        };
        let mut signature = Signature::new(CALL_CONV);
        signature
            .params
            .extend((0..params).map(|_| AbiParam::new(PTR)));
        signature
            .returns
            .extend((0..returns).map(|_| AbiParam::new(PTR)));
        signature
    }
}

/// The offset in a policy record of the address of the policy's value, which follows it.
const VALUE: u64 = 32;

/// The most stack a call of a policy hook uses: the 4 KiB its interface method demands, its glue
/// and the routines placing a task.
pub const HOOK_STACK: u64 = 8_192;

impl Lowering<'_, '_> {
    /// `k.policy(take policy)`: installs the kernel's policy, trapping with
    /// `trap.duplicate-access` when one is installed.
    pub fn install_policy(&mut self, fx: &mut Fx, supply: &Supply) {
        let policy_ty = supply.type_arg(0);
        let state_address = self.state(fx);
        let installed = fx.load(PTR, state_address, state::POLICY);
        let installed = fx.b.ins().icmp_imm_s(IntCC::NotEqual, installed, 0);
        fx.when(installed, |fx| {
            report::raise_kind(self, fx, "trap.duplicate-access")
        });
        let layout = self.layout(&policy_ty);
        let at = (VALUE + 8).next_multiple_of(layout.align.max(1));
        let size = fx.ptr(at + layout.size);
        let record = self.record_alloc(fx, size);
        let missing = fx.b.ins().icmp_imm_s(IntCC::Equal, record, 0);
        fx.when(missing, |fx| report::raise_kind(self, fx, "trap.exhausted"));
        for hook in Hook::ALL {
            let glue = self.policy_glue(hook, &policy_ty);
            let glue = self.func_ref(fx, glue);
            let glue = fx.b.ins().func_addr(PTR, glue);
            fx.store(glue, record, hook.offset());
        }
        let value = fx.offset(record, at);
        fx.store(value, record, VALUE);
        self.store_held(fx, value, supply.param(1), &policy_ty);
        let installed = fx.offset(state_address, state::POLICY);
        fx.b.ins()
            .atomic_store(MemFlagsData::trusted(), record, installed);
        supply.done(self, fx);
    }

    /// The glue calling a hook of a policy type with the hook's arguments as the runtime holds
    /// them, giving its result as words.
    fn policy_glue(&mut self, hook: Hook, policy_ty: &Ty) -> FuncId {
        let ty = policy_ty.clone();
        self.glue_function(
            Glue::Policy(hook, ty.clone()),
            hook.signature(),
            move |lo, fx| {
                let params = fx.entry();
                let interface = lo
                    .ck
                    .db
                    .std_item("hw", "Policy")
                    .expect("the interface hw::Policy");
                let method = lo
                    .ck
                    .interface_method(interface, hook.name())
                    .expect("a policy hook") as u32;
                let fn_ref = FnRef {
                    callable: Callable::Method { interface, method },
                    args: vec![Arg::Ty(ty)].into(),
                };
                let instance = lo.callable_instance(&fn_ref, false);
                let abi = lo.abi(&instance);
                let id = lo.instance(instance);
                let param_ty = |index: usize| abi.params[index].0.ty().clone();
                let mut args = vec![Held::Address(params[0]), Held::Empty];
                match hook {
                    Hook::Place => args.push(lo.task_view(fx, &param_ty(2), params[1])),
                    Hook::Steal => {
                        args.push(word32(fx, params[1]));
                        args.push(word32(fx, params[2]));
                    }
                    Hook::Slice => {
                        args.push(word32(fx, params[1]));
                        args.push(lo.task_view(fx, &param_ty(3), params[2]));
                    }
                    Hook::Idle => {
                        args.push(word32(fx, params[1]));
                        let option = param_ty(3);
                        let layout = lo.layout(&option);
                        let slot = fx.slot(layout.size, layout.align);
                        let timed = fx.b.ins().icmp_imm_s(IntCC::NotEqual, params[2], 0);
                        lo.write_option(fx, &option, slot, timed, Held::Scalar(params[3]));
                        args.push(Held::Address(slot));
                    }
                }
                let called = lo.invoke(fx, Invoked::Direct(id), &abi, &args);
                let result_ty = abi.result.clone();
                let result = lo.held_address(fx, called.result, &result_ty);
                let results = match hook {
                    Hook::Place => {
                        let cpu_at = lo.layouts.field(lo.ck, &result_ty, None, "cpu").offset;
                        let key_at = lo.layouts.field(lo.ck, &result_ty, None, "key").offset;
                        let target = fx.load(types::I32, result, cpu_at);
                        vec![
                            fx.b.ins().uextend(PTR, target),
                            fx.load(PTR, result, key_at),
                        ]
                    }
                    Hook::Steal => {
                        let found = lo.variant_is(fx, result, &result_ty, "Some");
                        let payload = some_payload(lo, &result_ty);
                        let victim = fx.load(types::I32, result, payload);
                        vec![
                            fx.b.ins().uextend(PTR, found),
                            fx.b.ins().uextend(PTR, victim),
                        ]
                    }
                    Hook::Slice => vec![fx.load(PTR, result, 0)],
                    Hook::Idle => {
                        let polls = lo.variant_is(fx, result, &result_ty, "Poll");
                        vec![fx.b.ins().uextend(PTR, polls)]
                    }
                };
                fx.b.ins().return_(&results);
            },
        )
    }

    /// The address of a value held as its type is, stored in a slot when it is held otherwise.
    fn held_address(&mut self, fx: &mut Fx, held: Held, ty: &Ty) -> Value {
        match held {
            Held::Address(address) => address,
            other => {
                let layout = self.layout(ty);
                let slot = fx.slot(layout.size.max(1), layout.align.max(1));
                self.store_held(fx, slot, other, ty);
                slot
            }
        }
    }

    /// An `hw::TaskView` of a task in a slot: its record's address, its executor's handle and
    /// share, the CPU it last ran on, the nanoseconds it ran and the earliest of the deadlines
    /// the constructs it runs armed.
    fn task_view(&mut self, fx: &mut Fx, view_ty: &Ty, record: Value) -> Held {
        let layout = self.layout(view_ty);
        let slot = fx.slot(layout.size, layout.align);
        let field = |lo: &mut Self, name: &str| lo.layouts.field(lo.ck, view_ty, None, name);
        let at = field(self, "task").offset;
        fx.store(record, slot, at);
        let executor = fx.load(PTR, record, task::EXECUTOR);
        let at = field(self, "executor").offset;
        fx.store(executor, slot, at);
        let share = field(self, "share");
        let packed = fx.load(PTR, record, task::SHARE);
        let share_at = fx.offset(slot, share.offset);
        accounts::unpack_share(self, fx, &share.ty, packed, share_at);
        let last = field(self, "last_cpu");
        let ran_on = fx.load(PTR, record, task::CPU);
        let ran = fx.b.ins().icmp_imm_s(IntCC::NotEqual, ran_on, 0);
        let index = fx.b.ins().iadd_imm_s(ran_on, -1);
        let index = fx.b.ins().ireduce(types::I32, index);
        let last_at = fx.offset(slot, last.offset);
        self.write_option(fx, &last.ty, last_at, ran, Held::Scalar(index));
        let ran_ns = fx.load(PTR, record, task::RAN);
        let at = field(self, "ran_ns").offset;
        fx.store(ran_ns, slot, at);
        let deadline = field(self, "deadline_ns");
        let first = fx.load(PTR, record, task::DEADLINES);
        let latest = fx.int(PTR, -1);
        let none = fx.int(types::I8, 0);
        let earliest = fx.repeat(&[first, latest, none], |fx, state| {
            let (current, earliest, any) = (state[0], state[1], state[2]);
            let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, current, 0);
            let next = fx.choose(
                present,
                &[PTR, PTR, types::I8],
                |fx| {
                    let due = fx.load(PTR, current, operation::DUE);
                    vec![
                        fx.load(PTR, current, operation::LINK),
                        fx.b.ins().umin(earliest, due),
                        fx.int(types::I8, 1),
                    ]
                },
                |_| vec![current, earliest, any],
            );
            (present, next)
        });
        let deadline_at = fx.offset(slot, deadline.offset);
        self.write_option(
            fx,
            &deadline.ty,
            deadline_at,
            earliest[2],
            Held::Scalar(earliest[1]),
        );
        Held::Address(slot)
    }

    /// Calls a hook of the installed policy, whose record is `policy`, with the running CPU's
    /// record marking the call.
    fn hook(&mut self, fx: &mut Fx, policy: Value, hook: Hook, args: &[Value]) -> Vec<Value> {
        let own = self.running_cpu(fx);
        let one = fx.ptr(1);
        fx.store(one, own, cpu::HOOK);
        let code = fx.load(PTR, policy, hook.offset());
        let value = fx.load(PTR, policy, VALUE);
        let signature = fx.b.import_signature(hook.signature());
        let mut values = vec![value];
        values.extend_from_slice(args);
        let call = fx.b.ins().call_indirect(signature, code, &values);
        let results = fx.b.inst_results(call).to_vec();
        let zero = fx.ptr(0);
        fx.store(zero, own, cpu::HOOK);
        results
    }

    /// The installed policy's record, zero for the default policy.
    fn policy(&mut self, fx: &mut Fx) -> Value {
        let state_address = self.state(fx);
        let installed = fx.offset(state_address, state::POLICY);
        fx.b.ins()
            .atomic_load(PTR, MemFlagsData::trusted(), installed)
    }

    /// Places a ready task on the queue of the CPU its policy names, the running CPU for a CPU
    /// past the last or while a hook runs on it, with maskable interrupts disabled; then sends the
    /// CPU signal to another CPU that idles or runs a task of a greater key, or to the running CPU
    /// outside an interrupt when its task's key is greater.
    pub fn kernel_enqueue(&mut self, fx: &mut Fx, record: Value) {
        let flags = self.call_stub(fx, Stub::IrqSave, &[])[0];
        let own = self.running_index(fx);
        let own_record = self.cpu_record(fx, own);
        let hooked = fx.load(PTR, own_record, cpu::HOOK);
        let hooked = fx.b.ins().icmp_imm_s(IntCC::NotEqual, hooked, 0);
        let placed = fx.choose_in(
            self,
            hooked,
            &[PTR, PTR],
            |_, fx| vec![own, fx.int(PTR, -1)],
            |lo, fx| {
                let policy = lo.policy(fx);
                let installed = fx.b.ins().icmp_imm_s(IntCC::NotEqual, policy, 0);
                fx.choose_in(
                    lo,
                    installed,
                    &[PTR, PTR],
                    |lo, fx| lo.hook(fx, policy, Hook::Place, &[record]),
                    |lo, fx| lo.default_place(fx, record),
                )
            },
        );
        let (target, key) = (placed[0], placed[1]);
        let state_address = self.state(fx);
        let count = fx.load(PTR, state_address, state::THREAD_COUNT);
        let beyond =
            fx.b.ins()
                .icmp(IntCC::UnsignedGreaterThanOrEqual, target, count);
        let target = fx.b.ins().select(beyond, own, target);
        fx.store(key, record, task::KEY);
        let queue = fx.b.ins().iadd_imm_s(target, 1);
        fx.store(queue, record, task::QUEUE);
        let target_record = self.cpu_record(fx, target);
        let lock = fx.offset(target_record, cpu::QUEUE_LOCK);
        self.call_routine(fx, Routine::Lock, &[lock]);
        insert(fx, target_record, record, key);
        let idle = fx.offset(target_record, cpu::IDLE);
        let idle = fx.b.ins().atomic_load(PTR, MemFlagsData::trusted(), idle);
        let running = fx.load(PTR, target_record, cpu::RUNNING_KEY);
        self.call_routine(fx, Routine::Unlock, &[lock]);
        let idles = fx.b.ins().icmp_imm_s(IntCC::NotEqual, idle, 0);
        let earlier = fx.b.ins().icmp(IntCC::UnsignedLessThan, key, running);
        let remote = fx.b.ins().icmp(IntCC::NotEqual, target, own);
        let wakes = fx.b.ins().bor(idles, earlier);
        let remote = fx.b.ins().band(remote, wakes);
        let serving = fx.load(PTR, own_record, cpu::FRAME);
        let outside = fx.b.ins().icmp_imm_s(IntCC::Equal, serving, 0);
        let local = fx.b.ins().icmp(IntCC::Equal, target, own);
        let local = fx.b.ins().band(local, earlier);
        let local = fx.b.ins().band(local, outside);
        let signals = fx.b.ins().bor(remote, local);
        fx.when(signals, |fx| self.signal_cpu(fx, target));
        self.call_stub(fx, Stub::IrqRestore, &[flags]);
    }

    /// The default placement of a ready task: the CPU it last ran on when that CPU idles, else
    /// the first CPU idling, else the running one; its key the next of the order in which tasks
    /// become ready.
    fn default_place(&mut self, fx: &mut Fx, record: Value) -> Vec<Value> {
        let own = self.running_index(fx);
        let state_address = self.state(fx);
        let count = fx.load(PTR, state_address, state::THREAD_COUNT);
        let ran_on = fx.load(PTR, record, task::CPU);
        let ran = fx.b.ins().icmp_imm_s(IntCC::NotEqual, ran_on, 0);
        let last = fx.b.ins().iadd_imm_s(ran_on, -1);
        let last_idles = fx.choose_in(
            self,
            ran,
            &[types::I8],
            |lo, fx| vec![lo.idles(fx, last)],
            |_, fx| vec![fx.int(types::I8, 0)],
        )[0];
        let zero = fx.ptr(0);
        let first = fx.repeat(&[zero], |fx, state| {
            let index = state[0];
            let inside = fx.b.ins().icmp(IntCC::UnsignedLessThan, index, count);
            let busy = fx.choose_in(
                self,
                inside,
                &[types::I8],
                |lo, fx| {
                    let idles = lo.idles(fx, index);
                    vec![fx.b.ins().bxor_imm_s(idles, 1)]
                },
                |_, fx| vec![fx.int(types::I8, 0)],
            )[0];
            let next = fx.b.ins().iadd_imm_s(index, 1);
            let next = fx.b.ins().select(busy, next, index);
            (busy, vec![next])
        })[0];
        let found = fx.b.ins().icmp(IntCC::UnsignedLessThan, first, count);
        let other = fx.b.ins().select(found, first, own);
        let target = fx.b.ins().select(last_idles, last, other);
        let ticket = fx.offset(state_address, state::TICKET);
        let one = fx.ptr(1);
        let key = fetch_add(fx, ticket, one);
        vec![target, key]
    }

    /// Whether a CPU idles.
    fn idles(&mut self, fx: &mut Fx, index: Value) -> Value {
        let record = self.cpu_record(fx, index);
        let idle = fx.offset(record, cpu::IDLE);
        let idle = fx.b.ins().atomic_load(PTR, MemFlagsData::trusted(), idle);
        fx.b.ins().icmp_imm_s(IntCC::NotEqual, idle, 0)
    }

    /// The task the running CPU runs next: the first of its queue, or one stolen from the CPUs
    /// its policy names, at most one attempt per other CPU; zero when none, the CPU then
    /// idling. The caller has disabled maskable interrupts.
    pub fn kernel_pick(&mut self, fx: &mut Fx) -> Value {
        let own = self.running_index(fx);
        let own_record = self.cpu_record(fx, own);
        let lock = fx.offset(own_record, cpu::QUEUE_LOCK);
        self.call_routine(fx, Routine::Lock, &[lock]);
        let head = take(fx, own_record);
        let empty = fx.b.ins().icmp_imm_s(IntCC::Equal, head, 0);
        fx.when(empty, |fx| {
            let idle = fx.offset(own_record, cpu::IDLE);
            let one = fx.ptr(1);
            fx.b.ins().atomic_store(MemFlagsData::trusted(), one, idle);
        });
        self.call_routine(fx, Routine::Unlock, &[lock]);
        fx.choose_in(
            self,
            empty,
            &[PTR],
            |lo, fx| vec![lo.steal(fx, own, own_record)],
            |_, _| vec![head],
        )[0]
    }

    /// A task stolen from another CPU's queue for the running CPU, which stops idling when it
    /// gets one; zero when none.
    fn steal(&mut self, fx: &mut Fx, own: Value, own_record: Value) -> Value {
        let state_address = self.state(fx);
        let count = fx.load(PTR, state_address, state::THREAD_COUNT);
        let policy = self.policy(fx);
        let installed = fx.b.ins().icmp_imm_s(IntCC::NotEqual, policy, 0);
        let zero = fx.ptr(0);
        let stolen = fx.repeat(&[zero, zero], |fx, state| {
            let attempt = state[0];
            let next = fx.b.ins().iadd_imm_s(attempt, 1);
            let allowed = fx.b.ins().icmp(IntCC::UnsignedLessThan, next, count);
            let tried = fx.choose_in(
                self,
                allowed,
                &[types::I8, PTR],
                |lo, fx| {
                    let named = fx.choose_in(
                        lo,
                        installed,
                        &[PTR, PTR],
                        |lo, fx| lo.hook(fx, policy, Hook::Steal, &[own, attempt]),
                        |_, fx| {
                            let after = fx.b.ins().iadd(own, next);
                            vec![fx.ptr(1), fx.b.ins().urem(after, count)]
                        },
                    );
                    let (found, victim) = (named[0], named[1]);
                    let found = fx.b.ins().icmp_imm_s(IntCC::NotEqual, found, 0);
                    let inside = fx.b.ins().icmp(IntCC::UnsignedLessThan, victim, count);
                    let other = fx.b.ins().icmp(IntCC::NotEqual, victim, own);
                    let valid = fx.b.ins().band(found, inside);
                    let valid = fx.b.ins().band(valid, other);
                    let got = fx.choose_in(
                        lo,
                        valid,
                        &[PTR],
                        |lo, fx| {
                            let victim_record = lo.cpu_record(fx, victim);
                            let lock = fx.offset(victim_record, cpu::QUEUE_LOCK);
                            lo.call_routine(fx, Routine::Lock, &[lock]);
                            let got = take(fx, victim_record);
                            lo.call_routine(fx, Routine::Unlock, &[lock]);
                            vec![got]
                        },
                        |_, fx| vec![fx.ptr(0)],
                    )[0];
                    let missed = fx.b.ins().icmp_imm_s(IntCC::Equal, got, 0);
                    vec![fx.b.ins().band(found, missed), got]
                },
                |_, fx| vec![fx.int(types::I8, 0), fx.ptr(0)],
            );
            (tried[0], vec![next, tried[1]])
        })[1];
        let got = fx.b.ins().icmp_imm_s(IntCC::NotEqual, stolen, 0);
        fx.when(got, |fx| {
            let idle = fx.offset(own_record, cpu::IDLE);
            let zero = fx.ptr(0);
            fx.b.ins().atomic_store(MemFlagsData::trusted(), zero, idle);
        });
        stolen
    }

    /// Idles the running CPU as its policy says, given the earliest deadline of the kernel
    /// clock: halts it until an interrupt, or returns at once to look for tasks again.
    pub fn kernel_idle(&mut self, fx: &mut Fx) {
        let own = self.running_index(fx);
        let own_record = self.cpu_record(fx, own);
        let state_address = self.state(fx);
        let lock = fx.offset(state_address, state::TIMER_LOCK);
        self.call_routine(fx, Routine::Lock, &[lock]);
        let first = fx.load(PTR, state_address, state::TIMERS);
        let timed = fx.b.ins().icmp_imm_s(IntCC::NotEqual, first, 0);
        let until = fx.choose(
            timed,
            &[PTR],
            |fx| vec![fx.load(PTR, first, operation::DUE)],
            |fx| vec![fx.ptr(0)],
        )[0];
        self.call_routine(fx, Routine::Unlock, &[lock]);
        let policy = self.policy(fx);
        let installed = fx.b.ins().icmp_imm_s(IntCC::NotEqual, policy, 0);
        let polls = fx.choose_in(
            self,
            installed,
            &[PTR],
            |lo, fx| {
                let timed = fx.b.ins().uextend(PTR, timed);
                lo.hook(fx, policy, Hook::Idle, &[own, timed, until])
            },
            |_, fx| vec![fx.ptr(0)],
        )[0];
        let halts = fx.b.ins().icmp_imm_s(IntCC::Equal, polls, 0);
        fx.when(halts, |fx| {
            self.call_stub(fx, Stub::IdleWait, &[]);
        });
        let idle = fx.offset(own_record, cpu::IDLE);
        let zero = fx.ptr(0);
        fx.b.ins().atomic_store(MemFlagsData::trusted(), zero, idle);
    }

    /// What the running CPU records as it switches a task in at nanosecond `now`: the task ran
    /// on it, its key, and the end of the slice its policy gives it; its timer is programmed
    /// again for that slice and its pacing.
    pub fn kernel_switch_in(&mut self, fx: &mut Fx, record: Value, now: Value) {
        let own = self.running_index(fx);
        let own_record = self.cpu_record(fx, own);
        let ran_on = fx.b.ins().iadd_imm_s(own, 1);
        fx.store(ran_on, record, task::CPU);
        let key = fx.load(PTR, record, task::KEY);
        fx.store(key, own_record, cpu::RUNNING_KEY);
        let policy = self.policy(fx);
        let installed = fx.b.ins().icmp_imm_s(IntCC::NotEqual, policy, 0);
        let slice = fx.choose_in(
            self,
            installed,
            &[PTR],
            |lo, fx| lo.hook(fx, policy, Hook::Slice, &[own, record]),
            |_, fx| vec![fx.ptr(0)],
        )[0];
        let sliced = fx.b.ins().icmp_imm_s(IntCC::NotEqual, slice, 0);
        let end = timers::due_after(fx, now, slice);
        let zero = fx.ptr(0);
        let end = fx.b.ins().select(sliced, end, zero);
        fx.store(end, own_record, cpu::SLICE_END);
        self.reprogram_timer(fx);
    }

    /// What the running CPU records once a task has switched away: it runs no task.
    pub fn kernel_switch_out(&mut self, fx: &mut Fx) {
        let own_record = self.running_cpu(fx);
        let zero = fx.ptr(0);
        fx.store(zero, own_record, cpu::RUNNING_KEY);
        fx.store(zero, own_record, cpu::SLICE_END);
    }

    /// Whether the policy switches the running CPU from its task: the task's slice has ended,
    /// or the first task of the CPU's queue has a smaller key.
    pub(super) fn policy_switches(&mut self, fx: &mut Fx) -> Value {
        let own_record = self.running_cpu(fx);
        let head = fx.load(PTR, own_record, cpu::QUEUE);
        let queued = fx.b.ins().icmp_imm_s(IntCC::NotEqual, head, 0);
        let head_key = fx.load(PTR, own_record, cpu::HEAD_KEY);
        let running = fx.load(PTR, own_record, cpu::RUNNING_KEY);
        let earlier = fx.b.ins().icmp(IntCC::UnsignedLessThan, head_key, running);
        let earlier = fx.b.ins().band(queued, earlier);
        let end = fx.load(PTR, own_record, cpu::SLICE_END);
        let sliced = fx.b.ins().icmp_imm_s(IntCC::NotEqual, end, 0);
        let expired = fx.choose_in(
            self,
            sliced,
            &[types::I8],
            |lo, fx| {
                let now = lo.call_routine(fx, Routine::ClockNow, &[])[0];
                vec![fx.b.ins().icmp(IntCC::UnsignedGreaterThanOrEqual, now, end)]
            },
            |_, fx| vec![fx.int(types::I8, 0)],
        )[0];
        fx.b.ins().bor(earlier, expired)
    }

    /// Takes a ready task out of the CPU queue holding it; gives whether one did.
    pub fn kernel_withdraw(&mut self, fx: &mut Fx, record: Value) -> Value {
        let queue = fx.load(PTR, record, task::QUEUE);
        let queued = fx.b.ins().icmp_imm_s(IntCC::NotEqual, queue, 0);
        fx.choose_in(
            self,
            queued,
            &[types::I8],
            |lo, fx| {
                let index = fx.b.ins().iadd_imm_s(queue, -1);
                let holder = lo.cpu_record(fx, index);
                let lock = fx.offset(holder, cpu::QUEUE_LOCK);
                lo.call_routine(fx, Routine::Lock, &[lock]);
                let first = fx.load(PTR, holder, cpu::QUEUE);
                let zero = fx.ptr(0);
                let found = fx.repeat(&[first, zero], |fx, state| {
                    let (current, previous) = (state[0], state[1]);
                    let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, current, 0);
                    let other = fx.b.ins().icmp(IntCC::NotEqual, current, record);
                    let more = fx.b.ins().band(present, other);
                    let next = fx.choose(
                        more,
                        &[PTR, PTR],
                        |fx| vec![fx.load(PTR, current, task::NEXT), current],
                        |_| vec![current, previous],
                    );
                    (more, next)
                });
                let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, found[0], 0);
                fx.when(present, |fx| {
                    let next = fx.load(PTR, record, task::NEXT);
                    let first = fx.b.ins().icmp_imm_s(IntCC::Equal, found[1], 0);
                    fx.choose(
                        first,
                        &[],
                        |fx| {
                            fx.store(next, holder, cpu::QUEUE);
                            head_key(fx, holder, next);
                            Vec::new()
                        },
                        |fx| {
                            fx.store(next, found[1], task::NEXT);
                            Vec::new()
                        },
                    );
                    let zero = fx.ptr(0);
                    fx.store(zero, record, task::QUEUE);
                });
                lo.call_routine(fx, Routine::Unlock, &[lock]);
                vec![present]
            },
            |_, fx| vec![fx.int(types::I8, 0)],
        )[0]
    }
}

/// A 32-bit argument from a word.
fn word32(fx: &mut Fx, word: Value) -> Held {
    Held::Scalar(fx.b.ins().ireduce(types::I32, word))
}

/// The offset of the payload of an option's `Some`.
fn some_payload(lo: &mut Lowering, option: &Ty) -> u64 {
    match &lo.layout(option).shape {
        Shape::Enum(shape) => shape
            .variants
            .iter()
            .find(|variant| variant.name == "Some")
            .and_then(|variant| variant.fields.first())
            .map_or(0, |field| field.offset),
        _ => 0,
    }
}

/// Inserts a task of a key into a CPU's queue after every task of a key no greater. The caller
/// holds the queue's lock.
fn insert(fx: &mut Fx, holder: Value, record: Value, key: Value) {
    let first = fx.load(PTR, holder, cpu::QUEUE);
    let zero = fx.ptr(0);
    let found = fx.repeat(&[first, zero], |fx, state| {
        let (current, previous) = (state[0], state[1]);
        let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, current, 0);
        let before = fx.choose(
            present,
            &[types::I8],
            |fx| {
                let other = fx.load(PTR, current, task::KEY);
                vec![fx.b.ins().icmp(IntCC::UnsignedLessThanOrEqual, other, key)]
            },
            |fx| vec![fx.int(types::I8, 0)],
        )[0];
        let next = fx.choose(
            before,
            &[PTR, PTR],
            |fx| vec![fx.load(PTR, current, task::NEXT), current],
            |_| vec![current, previous],
        );
        (before, next)
    });
    let (after, previous) = (found[0], found[1]);
    fx.store(after, record, task::NEXT);
    let first = fx.b.ins().icmp_imm_s(IntCC::Equal, previous, 0);
    fx.choose(
        first,
        &[],
        |fx| {
            fx.store(record, holder, cpu::QUEUE);
            fx.store(key, holder, cpu::HEAD_KEY);
            Vec::new()
        },
        |fx| {
            fx.store(record, previous, task::NEXT);
            Vec::new()
        },
    );
}

/// Takes the first task of a CPU's queue, zero when it is empty. The caller holds the queue's
/// lock.
fn take(fx: &mut Fx, holder: Value) -> Value {
    let head = fx.load(PTR, holder, cpu::QUEUE);
    let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, head, 0);
    fx.when(present, |fx| {
        let next = fx.load(PTR, head, task::NEXT);
        fx.store(next, holder, cpu::QUEUE);
        head_key(fx, holder, next);
        let zero = fx.ptr(0);
        fx.store(zero, head, task::QUEUE);
    });
    head
}

/// Records the key of a CPU queue's new first task, when it has one.
fn head_key(fx: &mut Fx, holder: Value, first: Value) {
    let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, first, 0);
    fx.when(present, |fx| {
        let key = fx.load(PTR, first, task::KEY);
        fx.store(key, holder, cpu::HEAD_KEY);
    });
}
