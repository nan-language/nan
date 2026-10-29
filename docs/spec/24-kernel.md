# Kernel execution model

This chapter defines how NaN code runs as a kernel on the machine targets: the anchor and the entry contexts that reach it; interrupt fns with their contexts, frames, exits, implied limits, binding and stacks, and the landing fns that receive user-origin exceptions and syscalls; interrupt state and `hw::without_irqs`; signals; spin locks and installed devices; thread context, thread switching, preemption, per-CPU state and the kernel lowering of waiting; physical memory; firmware services; floating point and extended state; kernel halts; and the properties of the kernel types.

## Anchor

The **anchor** is the one value per kernel that `boot::Kernel.anchor` installs. An **entry context** is code entered directly through a [machine seam](23-low-level.md#machine-seams): the body of an interrupt fn or of a [landing fn](23-low-level.md#landing-functions), closures written in it included. Only entry contexts reach the anchor; every other value reaches kernel code as a parameter or a capture ([authority](14-authority.md#authority)).

```nan-api
// module: boot

pub fn Kernel.alloc(self) -> mem::Alloc

pub fn Kernel.regions(mut self) -> mem::List[hw::Region]

pub fn Kernel.ring0(mut self) -> Option[hw::Ring0]

pub fn Kernel.anchor[S: Any and Send](take self, take state: S, take run: task fn(exec: task::Executor) fails task::Cancelled uses wait) -> Never
```

```nan-api
// module: hw

pub open struct Anchor[S: Any and Send] {
} Anchor

pub fn Anchor.with[R: Any, E](body: fn(state: S) -> R fails E) -> Option[R] fails E limits notrap, terminates, noalloc

pub fn cpu_count() -> u32 limits pure, notrap, terminates
```

- The [entry function](27-program-model.md#entry) of a bare target's image starts on the boot CPU with maskable interrupts disabled and every other CPU stopped, on `x86_64-bare` with SSE enabled and AVX enabled when the CPU has it, and on `aarch64-bare` with floating point and Advanced SIMD enabled.
- `k.alloc()` is the root allocator of `boot::Kernel` ([allocators](12-memory.md#allocators)). Its root cell's supply is the **boot heap**, memory the image reserves in the size its target's [descriptor](27-program-model.md#descriptors) states. Every call returns the same handle.
- The first call of `k.regions()` returns, in address order, a region for each range of usable memory outside the image, which holds the boot heap and the boot stack ([physical memory](#physical-memory)), outside the first MiB on x86_64 and outside the device tree and the ranges its memory reservation block lists on `aarch64-bare`; the list is drawn through `k.alloc()` and traps on exhaustion (`trap.exhausted`). Usable memory is, on `x86_64-bare`, the entries of type 1 of the start-info memory map, on `aarch64-bare`, the ranges of the device tree's memory nodes and, on the UEFI targets, the `EfiConventionalMemory`, `EfiBootServicesCode` and `EfiBootServicesData` entries of the firmware's final memory map. Later calls return an empty list.
- The first call of `k.ring0()` returns `Some` of the machine's one `hw::Ring0` ([privileged intrinsics](23-low-level.md#privileged-intrinsics)); later calls return `None`.
- `hw::cpu_count()` is the number of CPUs the platform reports as usable. CPUs are numbered from 0, the boot CPU being 0.
- `hw::Anchor[S]` has no values; it names the anchor of type S.
- On x86_64, each CPU's **CPU tables** are a global descriptor table holding a null descriptor, 64-bit code and data segments of privilege level 0, data and 64-bit code segments of privilege level 3, and the CPU's task-state segment, whose interrupt stack table entries 1 to 7 hold the tops of the CPU's interrupt stacks. On `x86_64-bare` the program entry seam loads the boot CPU's tables; on `x86_64-uefi`, `exit_services` does ([firmware services](#firmware-services)). Step 2 of `anchor` loads those of every other CPU. AArch64 has no CPU tables.

`take k.anchor(take state, run: f)` performs these steps and never returns:

1. It moves `state` into storage the image reserves and installs it as the anchor, visible to every CPU from then on.
2. On each CPU, the boot CPU included, it installs the vector seams and starts the kernel executor, enabling maskable interrupts as that executor starts. On x86_64 it loads the CPU's tables where they are not loaded and an interrupt descriptor table equal to `hw::Idt.of_vectors()`, held in storage the image reserves, and enables `syscall` when the final build has an `hw::syscall_entry` entry ([platform registries](25-registries.md#platform-registries)), with the syscall seam as its target and the interrupt, direction and trap flags cleared on entry. On AArch64 it makes the vector seams the CPU's exception vectors, which an exception enters on the CPU's interrupt stack, and enables the CPU's interface of the interrupt controller. Each other CPU starts in long mode on x86_64 and at EL1 on AArch64, on the [entry root](#physical-memory).
3. It runs `f` as the first kernel task on that executor, whose value it passes as `exec`. The executor draws task stacks and records through `k.alloc()` ([executors](16-concurrency.md#executors)), and `f` is a [task closure](16-concurrency.md#task-closures), capturing by `take` and Copy only; the executor records no clock ([capability tables](14-authority.md#capability-tables)).
4. When `f` returns or fails, the kernel halts ([kernel halts](#kernel-halts)).

The anchor is never moved, released or accessed by `mut` or `take`. Its parts change only through their own operations, so its mutable parts are values with [interior mutability](12-memory.md#interior-mutability) such as `hw::SpinLock`, `hw::PerCpu` and the `core::atomic` types.

`hw::Anchor[S].with(body)` calls `body` with the anchor and returns `Some` of its result, or returns `None` when no anchor is installed.

- A call of `hw::Anchor.with` outside an entry context is an error (`kernel.anchor-outside-entry`).
- The **anchor type** of a [final build](27-program-model.md#final-builds) is the S of the calls of `boot::Kernel.anchor` it lowers. Lowered calls of `boot::Kernel.anchor` with two or more distinct S are an error (`kernel.anchor-type`), reported at each of them; so is a lowered call of `hw::Anchor.with` whose S is not the anchor type, reported at that call. A final build lowering no call of `boot::Kernel.anchor` has no anchor type, and its calls of `with` return `None`.

```nan
// file: kernel/package.nan
//! A bare kernel counting timer ticks.

package kernel {
    language 1
    targets x86_64-bare
    entry main
} kernel

// file: kernel/kernel.nan
//! Boot, the anchor and the timer interrupt.

struct State {
    ticks: core::atomic::AtomicU64
    tick: mem::Shared[hw::Signal[u64]]
} State

pub interrupt fn on_timer(take irq: hw::IrqContext) -> hw::IrqExit limits stack 1_024 {
    let t = irq.irqs_disabled()
    discard hw::Anchor[State].with(fn(state: State) {
        let Some(count) = state.ticks.fetch_add(1) else {
            return
        }
        state.tick.contents().post(t, value: count)
    })
    take irq.complete()
    return hw::IrqExit.Reschedule
} on_timer

fn main(take k: boot::Kernel) -> Never limits stack 16_384 {
    let tick = mem::Shared.of(hw::Signal[u64].empty(), alloc: k.alloc())
    let wake = tick.share()
    let state = State(ticks: core::atomic::AtomicU64.of(0), tick: take tick)
    take k.anchor(take state, run: fn[take wake](exec: task::Executor) fails task::Cancelled uses wait limits stack 16_384 {
        loop {
            discard try await wake.contents().wait()
        }
    })
} main

contribute hw::vectors timer = hw::Vector(vector: 0x20, ist: 0, handler: hw::VectorHandler.Irq(on_timer), user: None)
```

```nan
// file: kernel/package.nan
//! Tick reporting for a bare kernel.

package kernel {
    language 1
    targets x86_64-bare
} kernel

// file: kernel/kernel.nan
//! Reads the tick count kept in the anchor.

struct State {
    ticks: core::atomic::AtomicU64
} State

pub fn tick_count() -> Option[u64] {
    return hw::Anchor[State].with(fn(state: State) -> u64 { // error: kernel.anchor-outside-entry
        return state.ticks.load()
    })
} tick_count
```

## Interrupt functions

An **interrupt fn** is a function with the `interrupt` [modifier](02-items.md#modifiers), run by a seam when its vector fires. The [descriptor](27-program-model.md#descriptors) of each machine target divides the vectors into **exception vectors** and **interrupt vectors**, and names its **abort vectors**: 8 and 18 on x86_64, 2 on AArch64.

```nan-api
// module: hw

pub open linear struct IrqContext {
} IrqContext

pub fn IrqContext.vector(self) -> u8 limits pure, notrap, terminates

pub proj fn IrqContext.irqs_disabled(self) -> IrqsDisabled limits pure, notrap, terminates

pub fn IrqContext.complete(take self) limits notrap, terminates, noalloc

pub open struct TrapFrame[V] {
} TrapFrame

pub fn TrapFrame.info(self) -> V limits pure, notrap, terminates

pub fn TrapFrame.ip(self) -> u64 limits pure, notrap, terminates

pub proj fn TrapFrame.irqs_disabled(self) -> IrqsDisabled limits pure, notrap, terminates

pub fn TrapFrame.set_ip(mut self, ip: u64) limits notrap, terminates, noalloc

pub closed struct NoCode derives Copy, Eq {
} NoCode

pub closed struct ErrorCode derives Copy, Eq {
    pub code: u64
} ErrorCode

pub closed struct PageFault derives Copy, Eq {
    pub code: u64
    pub address: u64
} PageFault

pub closed enum IrqExit derives Copy, Eq {
    Resume
    Reschedule
} IrqExit
```

A vector's class is the one its target's [descriptor](27-program-model.md#descriptors) assigns. The interrupt fn of an exception vector of class `Trap` receives an `hw::TrapFrame[hw::NoCode]`, of class `TrapWithCode` an `hw::TrapFrame[hw::ErrorCode]` and of class `PageFault` an `hw::TrapFrame[hw::PageFault]`; a vector without a class is never bound. `frame.info()` holds the error code the CPU pushed on x86_64 or the exception's syndrome on AArch64 and, for `hw::PageFault`, the faulting address.

The head of an interrupt fn has no generic parameters, `fails` or `uses wait`, its result is `hw::IrqExit`, and its parameters take one of three forms:

| Form | Parameters | Bound to |
| --- | --- | --- |
| interrupt | `take irq: hw::IrqContext` | an interrupt vector, by contribution |
| device | a `mut` parameter of the device type T, which may be `mut self`, then `take irq: hw::IrqContext` | an interrupt vector, by `hw::IrqShared.install` ([locks](#locks)) |
| exception | `mut frame: hw::TrapFrame[V]`, V being `hw::NoCode`, `hw::ErrorCode` or `hw::PageFault` | an exception vector whose class gives `hw::TrapFrame[V]`, by contribution |

- A head of any other shape is an error (`kernel.irq-head`).
- A body that may wait is an error (`kernel.irq-wait`), reported instead of `item.wait-missing`.
- An interrupt fn's digest is checked as if its head stated `limits notrap, terminates, noalloc` ([limits](10-effects-digests.md#limits)). Its limits clause states only the `stack N` that [static bounds](17-suspension-io.md#static-bounds) requires; any other limit there is an error (`kernel.irq-limit`) whose exact fix deletes it.
- An `interrupt fn` [function type](10-effects-digests.md#function-types) has the parameters and result of one form and no `fails` or `uses wait` (`kernel.irq-head`), and its limits clause states at most `stack N` (`kernel.irq-limit`). Its values carry the implied limits.
- A call of an interrupt fn, or of a value of an `interrupt fn` type, is an error (`kernel.irq-call`).
- `irq.complete()` ends the interrupt it serves, writing the end-of-interrupt register of the CPU's local APIC on x86_64 and of the CPU's interface of the interrupt controller on AArch64. `hw::IrqContext` is [linear](05-properties.md#linear-types), so every path completes it.
- `frame.set_ip(ip)` makes the interrupted code resume at `ip`. It is a [trusted primitive](23-low-level.md#trusted-primitives) whose soundness condition is that execution may continue at `ip` with the saved state.

Contributions bind the interrupt and exception forms through `hw::vectors`, whose entries [platform registries](25-registries.md#platform-registries) check against the class of their vector (`registry.platform-entry`).

When a vector fires while the CPU runs in **kernel mode**, at privilege level 0 on x86_64 and at EL1 on AArch64, the CPU enters its seam with maskable interrupts disabled, and:

1. The seam saves the interrupted code's registers and [extended state](#floating-point) on its interrupt stack.
2. For a vector bound to an `hw::IrqShared`, it acquires that lock, calls the device's interrupt fn with the device and a new `hw::IrqContext`, and releases the lock. For a vector bound by contribution, it calls the interrupt fn with a new `hw::IrqContext` or with the `hw::TrapFrame` of the interrupted code. An unbound vector halts the kernel.
3. On `hw::IrqExit.Reschedule`, when the interrupted code ran in [thread context](#scheduling) with maskable interrupts enabled and the anchor is installed, the interrupted thread is [preempted](#preemption). Otherwise `Reschedule` acts as `Resume`.
4. The seam restores the saved state and resumes the interrupted code. After an interrupt fn for an abort vector returns, the kernel halts instead.

Each CPU has an interrupt stack, reserved by the image, for each `ist` value that its bindings use, those of `hw::IrqShared.install` using 0 ([platform registries](25-registries.md#platform-registries)). A seam entered while its stack is in use on its CPU continues on it below the frames in use.

- The stack of `ist` 0 holds at least the largest stack use among the lowered interrupt fns that run on it for interrupt vectors, plus the largest among those for exception vectors, plus two [seam frames](27-program-model.md#descriptors).
- The stack of a nonzero `ist` holds at least the largest stack use among the lowered interrupt fns that run on it, plus one seam frame.

On AArch64 the interrupt controller delivers an interrupt only while it is enabled: the anchor enables the timer's vector and each interrupt vector a contribution binds, on every CPU, `hw::IrqShared.install` enables its vector and `uninstall` disables it. Every interrupt from 32 up goes to the boot CPU.

```nan
// file: timer/package.nan
//! Spurious interrupt handling for a bare kernel.

package timer {
    language 1
    targets x86_64-bare
} timer

// file: timer/timer.nan
//! Completes spurious interrupts.

pub interrupt fn on_spurious(take irq: hw::IrqContext) -> hw::IrqExit limits stack 256 {
    take irq.complete()
    return hw::IrqExit.Resume
} on_spurious
```

```nan
// file: timer/package.nan
//! Spurious interrupt handling for a bare kernel.

package timer {
    language 1
    targets x86_64-bare
} timer

// file: timer/timer.nan
//! Completes spurious interrupts.

pub interrupt fn on_spurious(take irq: hw::IrqContext) -> hw::IrqExit limits notrap, stack 256 { // error: kernel.irq-limit
    take irq.complete()
    return hw::IrqExit.Resume
} on_spurious
```

```nan
// file: timer/package.nan
//! Spurious interrupt handling for a bare kernel.

package timer {
    language 1
    targets x86_64-bare
} timer

// file: timer/timer.nan
//! Completes spurious interrupts.

pub interrupt fn on_spurious(take irq: hw::IrqContext) limits stack 256 { // error: kernel.irq-head
    take irq.complete()
} on_spurious
```

```nan
// file: timer/package.nan
//! Spurious interrupt handling for a bare kernel.

package timer {
    language 1
    targets x86_64-bare
} timer

// file: timer/timer.nan
//! Completes spurious interrupts.

pub interrupt fn on_spurious(take irq: hw::IrqContext) -> hw::IrqExit limits stack 256 {
    take irq.complete()
    return hw::IrqExit.Resume
} on_spurious

pub fn replay_spurious(take irq: hw::IrqContext) -> hw::IrqExit {
    return on_spurious(take irq) // error: kernel.irq-call
} replay_spurious
```

### User exceptions and syscalls

An exception raised while the CPU runs in **user mode**, at privilege level 3 on x86_64 and at EL0 on AArch64, and the system call instruction, `syscall` on x86_64 and `svc` on AArch64, never reach an interrupt fn. Their seams call landing fns bound in [platform registries](25-registries.md#platform-registries): for an exception, the landing fn that `hw::vectors` binds for user-origin occurrences of its vector; for `syscall`, that of `hw::syscall_entry`.

No operation of language 1 enters user mode, so these seams are never entered; the [reservation register](37-evolution.md#reservations) lists them.

- The seam saves the user state on the kernel stack of the thread that ran the user code, enables maskable interrupts and calls the landing fn there, in thread context as part of that thread.
- The landing fn of an `hw::Vector.user` field receives the vector; the error code the CPU pushed, or 0 when it pushed none, on x86_64 and the syndrome on AArch64; and the faulting address for a vector of class `PageFault` or 0 for other vectors.
- The landing fn of the `hw::SyscallEntry.handler` field receives as `number` and `args`, each an `Untrusted[u64]` ([sources](15-untrusted.md#sources)), the user's `rax` and its `rdi`, `rsi`, `rdx`, `r10`, `r8` and `r9` on x86_64, and its `x8` and its `x0` to `x5` on AArch64, in that order; its result replaces the user's `rax` on x86_64 and `x0` on AArch64.
- When the landing fn returns, the seam restores the user state and resumes the user code.
- A user-origin exception on a vector without such a landing fn halts the kernel.

The kernel stack of a thread that runs user code holds at least the largest stack use among these landing fns plus one [seam frame](27-program-model.md#descriptors).

## Interrupt state

`hw::IrqsDisabled` is the [state token](05-properties.md#state-tokens) standing for maskable interrupts being disabled on the CPU running its holder, which therefore never changes CPU.

```nan-api
// module: hw

pub open view struct IrqsDisabled {
} IrqsDisabled

pub fn without_irqs[T: Any, E](body: fn(t: IrqsDisabled) -> T fails E limits notrap) -> T fails E
```

- Its values come only from `IrqContext.irqs_disabled`, `TrapFrame.irqs_disabled` and `hw::without_irqs`.
- `hw::without_irqs(body)` saves whether maskable interrupts are enabled on the current CPU, disables them, calls `body` with a token, restores the saved state and returns what `body` returned or fails with its failure.
- On the host targets, each executor thread is a CPU for this chapter and has no maskable interrupts: `hw::cpu_count()` is the number of executor threads, `without_irqs` only calls `body`, `IrqContext.complete` does nothing and `hw::IrqShared.install` binds no vector.

```nan
// file: ticker/package.nan
//! Tick counting for a bare kernel.

package ticker {
    language 1
    targets x86_64-bare
} ticker

// file: ticker/ticker.nan
//! An IRQ-safe tick counter.

pub closed struct Ticks {
    pub count: u64
} Ticks

pub fn add_ticks(lock: hw::SpinLock[Ticks], extra: u64) -> u64 {
    return hw::without_irqs(fn(t: hw::IrqsDisabled) -> u64 limits notrap {
        return lock.with(t, body: fn(mut ticks: Ticks) -> u64 limits notrap, terminates, noalloc {
            ticks.count = ticks.count.saturating_add(extra)
            return ticks.count
        })
    })
} add_ticks
```

## Signals

A **signal** is an `hw::Signal[T]`, a one-slot mailbox through which interrupt fns wake tasks. A signal holds at most one value and at most one waiting task.

```nan-api
// module: hw

pub open struct Signal[T: Send] {
} Signal

pub fn Signal.empty() -> Signal[T] limits pure, notrap, terminates

pub fn Signal.post(self, t: IrqsDisabled, take value: T) limits notrap, terminates, noalloc

pub fn Signal.wait(self) -> T fails task::Cancelled uses wait
```

- `post` hands `value` to the waiting task and makes that task ready on its executor when one waits; otherwise it stores `value`, releasing a value already stored.
- `wait` takes the stored value and returns it at once when one is stored; otherwise it suspends until a `post`. A `wait` on a signal another task already waits on traps (`trap.duplicate-access`).
- `wait` is a waiting primitive whose cancellation the runtime acknowledges at once ([acknowledged cancellation](17-suspension-io.md#acknowledged-cancellation)): a `wait` cancelled when it starts fails at once, and when a suspended `wait` becomes cancelled, a value posted before the acknowledgement is returned instead of the cancellation.
- A device's completion operation that waits on a signal handles the cancellation of that wait as [completion I/O](17-suspension-io.md#completion-io) states.
- Both take the signal by read, so a signal shared by an interrupt fn and a task lives in a [`mem::Shared`](12-memory.md#shared-values), one share on each side.

```nan
// file: ticker/package.nan
//! Tick waiting for a bare kernel.

package ticker {
    language 1
    targets x86_64-bare
} ticker

// file: ticker/ticker.nan
//! Waits for the next tick an interrupt fn posts.

pub fn next_tick(tick: mem::Shared[hw::Signal[u64]]) -> u64 fails task::Cancelled uses wait {
    return try await tick.contents().wait()
} next_tick
```

## Locks

An **IRQ-safe lock** is an `hw::SpinLock[T]` taken with an `hw::IrqsDisabled` token, including the lock inside an `hw::IrqShared[T]`.

```nan-api
// module: hw

pub open struct SpinLock[T: Send] {
} SpinLock

pub fn SpinLock.of(take value: T) -> SpinLock[T] limits notrap, terminates, noalloc

pub fn SpinLock.with[R: Any](self, t: IrqsDisabled, body: fn(mut value: T) -> R limits notrap, terminates, noalloc) -> R limits notrap, terminates, noalloc

pub open linear struct SpinGuard[T] {
} SpinGuard

pub proj fn SpinGuard.contents_mut(mut self) -> T

pub open linear struct IrqShared[T: Any and Send] {
} IrqShared

pub fn IrqShared.install(take dev: T, vector: u8, on_irq: interrupt fn(mut dev: T, take irq: IrqContext) -> IrqExit) -> IrqShared[T]
    requires vector >= 32

pub fn IrqShared.with[R: Any](self, t: IrqsDisabled, body: fn(mut dev: T) -> R limits notrap, terminates, noalloc) -> R limits notrap, terminates, noalloc

pub fn IrqShared.uninstall(take self) -> T
```

- `with` spins until it acquires the lock, calls `body` with exclusive `mut` access to the value, releases the lock and returns `body`'s result. Acquisition orders `body` after every earlier holder's accesses.
- A `with` on a lock that the calling CPU already holds never returns: the kernel halts, and on the host targets the innermost process or test [domain](11-errors-domains.md#domains) holding the call is [abandoned](11-errors-domains.md#abandoning-a-domain), with the halt report ([kernel halts](#kernel-halts)) in place of a trap report.
- A spin lock is never [poisoned](11-errors-domains.md#poisoning): its bodies cannot trap, and a trap anywhere in a kernel halts it.
- An `hw::SpinGuard[T]` is the holding of an acquired spin lock outside a `with` body. Only the kernel executor obtains and releases one, on its run-queue lock ([scheduling](#scheduling)).

`hw::IrqShared[T]` keeps a device of type T behind an IRQ-safe lock that the interrupt path also takes:

- `install(take dev, vector: v, on_irq: f)` keeps `dev`, binds interrupt vector v to f on every CPU and returns the linear `hw::IrqShared[T]`. Installing on a vector already bound traps (`trap.duplicate-access`).
- `with` reaches the device from other code, as `SpinLock.with` does.
- `take shared.uninstall()` unbinds the vector, spins until no CPU runs f for it, and returns the device.
- `install` and `uninstall` are the device operations that `allow device` covers ([manifest grants](14-authority.md#manifest-grants)).

```nan
// file: serial/package.nan
//! Interrupt-driven serial receive for a bare kernel.

package serial {
    language 1
    targets x86_64-bare
    allow device in serial
} serial

// file: serial/serial.nan
//! A 16550 receiver installed on its interrupt vector.

const RX_VECTOR: u8 = 0x24

pub layout LineStatus: u8 {
    data_ready: bit 0
    reserved: rest
} LineStatus

pub registers SerialRegs {
    data: u8 at 0x0 read
    line_status: LineStatus at 0x5 read
} SerialRegs

pub open linear struct Serial {
    regs: hw::Mmio[SerialRegs]
    received: mem::Shared[hw::Signal[u8]]
} Serial

pub fn attach(take regs: hw::Mmio[SerialRegs], received: mem::Shared[hw::Signal[u8]]) -> hw::IrqShared[Serial] {
    let serial = Serial(regs: take regs, received: received.share())
    return hw::IrqShared.install(take serial, vector: RX_VECTOR, on_irq: Serial.on_irq)
} attach

pub fn has_byte(shared: hw::IrqShared[Serial]) -> bool {
    return hw::without_irqs(fn(t: hw::IrqsDisabled) -> bool limits notrap {
        return shared.with(t, body: fn(mut serial: Serial) -> bool limits notrap, terminates, noalloc {
            let status = mut serial.regs.read(SerialRegs.line_status)
            return status.data_ready
        })
    })
} has_byte

interrupt fn Serial.on_irq(mut self, take irq: hw::IrqContext) -> hw::IrqExit limits stack 512 {
    let status = mut self.regs.read(SerialRegs.line_status)
    if status.data_ready {
        let byte = mut self.regs.read(SerialRegs.data)
        self.received.contents().post(irq.irqs_disabled(), value: byte)
    }
    take irq.complete()
    return hw::IrqExit.Resume
} Serial.on_irq
```

## Scheduling

**Thread context** is kernel execution in a schedulable thread: every kernel task, and the landing fns of user-origin seams. Each kernel task runs as one **thread**, whose stack and saved registers the kernel executor owns. The boot context and interrupt fns are not thread context. Each CPU also has an idle thread, which halts the CPU until an interrupt arrives.

```nan-api
// module: hw

pub open struct RunQueue {
} RunQueue

pub open linear struct SavedContext {
} SavedContext

pub open struct Resumable {
} Resumable

pub fn SavedContext.resumable(self) -> Resumable

pub fn switch_to(take guard: SpinGuard[RunQueue], take from: SavedContext, to: Resumable) -> SpinGuard[RunQueue]

pub open struct PerCpu[T: Send] {
} PerCpu

pub fn PerCpu.of(init: fn(cpu: u32) -> T, alloc: mem::Alloc) -> PerCpu[T]

pub fn PerCpu.with[R: Any](self, t: IrqsDisabled, body: fn(mut value: T) -> R limits notrap, terminates, noalloc) -> R limits notrap, terminates, noalloc
```

- An `hw::RunQueue` holds the ready threads of one CPU, behind that CPU's run-queue lock. An `hw::SavedContext` is the save slot of the running thread. An `hw::Resumable` is a [generational handle](12-memory.md#generational-handles) naming a thread whose registers are saved; it is live until that thread resumes. Only the kernel executor creates values of these types.
- `from.resumable()` names the thread whose save slot `from` is. When that thread resumes, the executor holds its save slot again.
- `hw::switch_to(take guard, take from, to)` saves the running thread's registers and extended state into `from`, then restores the thread `to` names, whose own pending `switch_to` call returns `guard`: the lock passes to the incoming thread, still held on the same CPU. The outgoing call returns when a later `switch_to` names its thread, with that caller's guard. A stale `to` traps (`trap.stale-handle`) before anything is saved.
- `hw::PerCpu.of(init, alloc)` holds `hw::cpu_count()` values drawn through `alloc`, the value of CPU i being `init(i)`, and traps on exhaustion (`trap.exhausted`). `with` calls `body` with `mut` access to the current CPU's value; a `with` on a value whose `with` the calling CPU is already inside never returns, as for a spin lock.

```nan
// file: ticker/package.nan
//! Per-CPU switch counting for a bare kernel.

package ticker {
    language 1
    targets x86_64-bare
} ticker

// file: ticker/ticker.nan
//! Counts thread switches on each CPU.

pub closed struct Switches {
    pub count: u64
} Switches

pub fn count_switch(switches: hw::PerCpu[Switches]) {
    hw::without_irqs(fn(t: hw::IrqsDisabled) limits notrap {
        switches.with(t, body: fn(mut local: Switches) limits notrap, terminates, noalloc {
            local.count = local.count.wrapping_add(1)
        })
    })
} count_switch
```

### Kernel suspension

On the machine targets, a task suspends ([stackful tasks](17-suspension-io.md#stackful-tasks)) through the kernel executor's suspend routine, part of the stack-switching [support routine](27-program-model.md#runtime-support):

1. The waiting primitive registers the task as a waiter of the event it awaits.
2. The routine disables maskable interrupts, acquires the CPU's run-queue lock and calls `hw::switch_to` with the CPU's next ready thread, or with its idle thread when none is ready.
3. When the event occurs, its operation makes the task ready on a CPU the executor chooses.
4. On resumption the routine releases the run-queue lock and restores the saved interrupt state.

Interrupt fns make tasks ready only through `hw::Signal.post`; a task waiting for a device waits on a signal that the device's interrupt fn posts.

### Preemption

On the machine targets, the [platform package](28-stdlib.md#platform-packages) contributes the one `hw::preempt` entry ([platform registries](25-registries.md#platform-registries)). **Preemption** of a thread proceeds in this order:

1. The seam moves the thread's saved state from the interrupt stack onto the thread's own stack.
2. It calls the landing fn that the `handler` of the `hw::preempt` entry holds there, with maskable interrupts disabled. That landing fn makes the thread ready and switches the CPU to its next ready thread as [kernel suspension](#kernel-suspension) step 2 does.
3. When a later switch resumes the thread, the landing fn returns, and the seam restores the saved state and resumes the thread.

A kernel task's stack is drawn through the executor's allocator as the task starts. It holds at least the N of its entry's `limits stack N` ([task stacks](17-suspension-io.md#task-stacks)), plus the stack use of the `hw::preempt` landing fn, plus one [seam frame](27-program-model.md#descriptors).

## Physical memory

A **physical frame** is an `hw::PhysFrame[S]`, an owned range of S bytes of physical memory aligned to S. An `hw::Region` is an owned range of physical memory.

```nan-api
// module: hw

pub open linear struct Region {
} Region

pub fn Region.base(self) -> u64 limits pure, notrap, terminates

pub fn Region.byte_len(self) -> u64 limits pure, notrap, terminates

pub fn Region.frames[S: u64 where S == 4_096 or S == 2_097_152 or S == 1_073_741_824](take self, mut frames: mem::List[PhysFrame[S]])

pub open linear struct PhysFrame[S: u64 where S == 4_096 or S == 2_097_152 or S == 1_073_741_824] {
} PhysFrame

pub fn PhysFrame.base(self) -> u64 limits pure, notrap, terminates

pub fn PhysFrame.split[T: u64 where (T == 4_096 or T == 2_097_152) and T < S](take self, mut parts: mem::List[PhysFrame[T]])

pub fn PhysFrame.merge[T: u64 where (T == 4_096 or T == 2_097_152) and T < S](take parts: mem::List[PhysFrame[T]]) -> Result[PhysFrame[S], mem::List[PhysFrame[T]]]

pub proj fn PhysFrame.contents_mut[T: AnyBits](mut self, at: Offset[T]) -> T

pub fn PhysFrame.into_region(take self) -> Region

pub closed struct Offset[T: AnyBits] derives Copy, Eq {
    pub bytes: u64
} Offset
```

- On the machine targets, the **entry root** is the page-table root in use when the entry function starts on a bare target, and the one that `exit_services` installs on a UEFI target. Its tables lie in memory the image reserves, and it maps the image, the memory the image reserves and all usable memory writable at their physical addresses, where code reaches the memory of frames and regions. The soundness condition of `write_cr3` and `write_ttbr0` keeps the image, the memory it reserves and every live frame and region mapped there ([privileged intrinsics](23-low-level.md#privileged-intrinsics)).
- By the soundness condition of `write_cr3` and `write_ttbr0`, no value that safe code reaches holds a frame of a page table that a CPU's root reaches: the trusted code linking a frame into such a table consumes it, such as by storing it through `hw::Ptr.write`.
- `take region.frames(mut frames)` appends, in address order, every S-aligned frame lying wholly inside the region. Memory of the region outside those frames is never used again.
- `take frame.split(mut parts)` appends the S ÷ T frames of size T that make up `frame`, in address order. `PhysFrame[S].merge(take parts)` returns `Ok` of the frame they make up when `parts` holds exactly S ÷ T contiguous frames in address order whose first is S-aligned, and otherwise returns `Err(parts)` unchanged. `split` and `frames` grow their list as `push` does ([growth](12-memory.md#growth)).
- `frame.contents_mut(at)` projects the T stored `at.bytes` bytes into the frame. It traps (`trap.bounds`) when T does not fit inside the frame from that offset or the offset is not aligned for T. Being AnyBits, T is valid whatever the frame holds. Its T, like that of `hw::Offset[T]`, has a fixed layout ([DMA](23-low-level.md#dma)).
- `take frame.into_region()` returns the frame's memory as a region, over which a store is created with `over` ([regions](12-memory.md#regions)).

```nan
// file: paging/package.nan
//! Page-table frames for a bare kernel.

package paging {
    language 1
    targets x86_64-bare
} paging

// file: paging/paging.nan
//! Clears page tables and splits large frames.

pub fn clear_entries(mut entries: Array[u64, 512]) {
    for entry in mut entries {
        entry = 0
    }
} clear_entries

pub fn clear_table(mut frame: hw::PhysFrame[4_096]) {
    clear_entries(mut frame.contents_mut(hw::Offset[Array[u64, 512]](bytes: 0)))
} clear_table

pub fn small_frames(take frame: hw::PhysFrame[2_097_152], alloc: mem::Alloc) -> mem::List[hw::PhysFrame[4_096]] {
    var parts = mem::List[hw::PhysFrame[4_096]].empty(alloc)
    take frame.split(mut parts)
    return parts
} small_frames
```

## Firmware services

On the UEFI targets, the [root grant](14-authority.md#root-grants) `boot::Machine` reaches the firmware's services only during the [protocol phase](14-authority.md#protocol-phases) before they exit.

```nan-api
// module: boot

pub fn Machine.alloc(self) -> mem::Alloc

pub fn Machine.with_services[T: Any, E](mut self, body: fn(services: Services) -> T fails E) -> T fails E

pub fn Machine.exit_services(take self) -> Kernel

pub open view struct Services {
} Services

pub fn Services.write_console(self, text: Str) fails ServiceError

pub fn Services.read_file(self, path: Str, alloc: mem::Alloc) -> Untrusted[mem::Bytes] fails ServiceError

pub open enum ServiceError derives Render {
    NotFound
    DeviceError
    OutOfMemory
} ServiceError
```

- `m.alloc()` is the root allocator of `boot::Machine`, whose supply is the boot heap; `exit_services` passes it on unchanged as the `boot::Kernel`'s.
- `with_services(body)` calls `body` with [`boot::Services`](#kernel-types), a view, so no capability derived from it outlives the call.
- `write_console` writes `text` to the firmware's console output, each line feed as a carriage return and a line feed. `read_file` reads the file that `path` names from the root directory of the volume the image was loaded from, with `/` between components.
- Each fails with `NotFound` when the firmware returns `EFI_NOT_FOUND`, with `OutOfMemory` when it returns `EFI_OUT_OF_RESOURCES` or a draw through `alloc` is exhausted, and with `DeviceError` on any other error status.
- `take m.exit_services()` ends the firmware's boot services, installs the entry root and, on x86_64, loads the boot CPU's tables ([anchor](#anchor)), and returns the `boot::Kernel` of the machine, with maskable interrupts disabled. Its regions come from the firmware's final memory map ([anchor](#anchor)).

```nan
// file: loader/package.nan
//! A UEFI loader that announces itself.

package loader {
    language 1
    targets x86_64-uefi
    entry main
} loader

// file: loader/loader.nan
//! Entry of the loader.

fn main(take m: boot::Machine) -> Never limits stack 16_384 {
    discard attempt mut m.with_services(fn(services: boot::Services) fails boot::ServiceError {
        try services.write_console("loader started\n")
    })
    var spins: u64 = 0
    loop {
        spins = spins.wrapping_add(1)
    }
} main
```

## Floating point

A function, closure or const whose own operations have the float use [digest fact](10-effects-digests.md#digests) is an error (`kernel.float-use`) wherever the environments of the machine targets judge reachable code ([environments](27-program-model.md#environments)), reported at each such operation.

Every seam and `hw::switch_to` saves the complete extended CPU state (on x86_64 the x87 and SSE registers, and the AVX registers when the CPU has them; on AArch64 the floating-point and Advanced SIMD registers with FPCR and FPSR) of the code it leaves and restores it before that code resumes. Lowering may use these registers for operations other than float arithmetic.

```nan
// file: kernel/package.nan
//! Load scaling for a bare kernel.

package kernel {
    language 1
    targets x86_64-bare
} kernel

// file: kernel/kernel.nan
//! Scales a load figure.

pub fn doubled(level: f64) -> f64 {
    return level * 2.0 // error: kernel.float-use
} doubled
```

## Kernel halts

A **kernel halt** ends a kernel domain ([domains](11-errors-domains.md#domains)). The kernel halts on a trap in any of its tasks or contexts, and in these other cases, each named by its cause:

| Cause | Event | Located at |
| --- | --- | --- |
| `first-task-ended` | the first kernel task returns or fails | the `anchor` call |
| `lock-reacquired` | a CPU calls `with` on a spin lock or per-CPU value it is already inside, or reaches its run-queue lock or the cell state of a draw or return while it already holds it | that call, or the interrupted instruction |
| `unbound-vector` | a vector fires with no binding for its origin | the interrupted instruction |
| `abort-exception` | an interrupt fn for an abort vector returns | the interrupted instruction |

The first CPU to begin a halt performs these steps; a CPU beginning a halt after it stops as step 1 stops the others.

1. It sends every other running CPU an interrupt that disabled maskable interrupts do not hold back, a non-maskable interrupt on x86_64 and a fast interrupt request on AArch64, whose seam then stops that CPU with maskable interrupts disabled instead of calling an interrupt fn, and waits until each has stopped.
2. It programs the UART at the serial port the descriptor names to 115200 baud, 8 data bits, no parity and 1 stop bit, then writes the report there, by polling and without allocating. For a trap this is the [trap report](11-errors-domains.md#trap-reports) without a replay line. For another cause it has the same form, with `halt` and the cause in place of the trap kind, the location the table gives, and, for `unbound-vector`, the operand line `vector = ` and the vector number.
3. It writes the byte 70 to the [exit port](27-program-model.md#descriptors), then stops with maskable interrupts disabled. No deferred call or other user code runs.

When the first task started by the `anchor` call on line 24 of `kernel/kernel.nan` in [Anchor](#anchor) ends, the kernel halts with this report:

```text
halt first-task-ended at kernel/kernel.nan:24 in kernel::main
    take k.anchor(take state, run: fn[take wake](exec: task::Executor) fails task::Cancelled uses wait limits stack 16_384 {
```

## Kernel types

The types of this table are [base types](05-properties.md#structural-properties) with these properties. The primitive capabilities among them are `hw::Region`, `hw::PhysFrame[S]`, `hw::IrqShared[T]` and `boot::Services`.

| Type | linear | view | Copy | Send | Inert | AnyBits |
| --- | --- | --- | --- | --- | --- | --- |
| `hw::IrqContext`, `hw::SavedContext`, `hw::SpinGuard[T]` | yes | no | no | no | no | no |
| `hw::TrapFrame[V]` | no | no | no | no | no | no |
| `hw::Region`, `hw::PhysFrame[S]`, `hw::IrqShared[T]` | yes | no | no | yes | no | no |
| `hw::Signal[T]`, `hw::SpinLock[T]`, `hw::PerCpu[T]`, `hw::RunQueue` | no | no | no | yes | no | no |
| `hw::Resumable` | no | no | yes | yes | no | no |
| `boot::Services` | no | yes | no | no | no | no |
| `hw::Anchor[S]` | no | no | no | no | no | no |

`hw::Anchor[S]` has no values. `hw::IrqsDisabled` is a [state token](05-properties.md#state-tokens), and the other types this chapter declares are structs and enums whose properties are computed.

## Diagnostics

| Code | Class | Rule |
| --- | --- | --- |
| `kernel.anchor-outside-entry` | error | [Anchor](#anchor) |
| `kernel.anchor-type` | error | [Anchor](#anchor) |
| `kernel.float-use` | error | [Floating point](#floating-point) |
| `kernel.irq-call` | error | [Interrupt functions](#interrupt-functions) |
| `kernel.irq-head` | error | [Interrupt functions](#interrupt-functions) |
| `kernel.irq-limit` | error | [Interrupt functions](#interrupt-functions) |
| `kernel.irq-wait` | error | [Interrupt functions](#interrupt-functions) |
