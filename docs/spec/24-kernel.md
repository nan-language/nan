# Kernel execution model

This chapter defines how NaN code runs as a kernel on the machine targets: the anchor and the entry contexts that reach it; interrupt fns with their contexts, frames, exits, implied limits, binding and stacks; user mode, which kernel tasks run user threads in, with split tables and user frames; interrupt state and `hw::without_irqs`; signals; spin locks and installed devices; PCI functions; the CPU signal and shootdowns; thread context, per-CPU state, scheduling policies, the kernel lowering of waiting, preemption and the kernel clock; physical memory; firmware services, the hand-off to a loaded bare image and the start information; loading bundles into a running kernel; floating point and extended state; kernel halts; and the properties of the kernel types.

## Anchor

The **anchor** is the one value per kernel that `boot::Kernel.anchor` installs. An **entry context** is code entered directly through a [machine seam](23-low-level.md#machine-seams): the body of an interrupt fn or of a [landing fn](23-low-level.md#landing-functions), closures written in it included. Only entry contexts reach the anchor; every other value reaches kernel code as a parameter or a capture ([authority](14-authority.md#authority)).

```nan-api
// module: boot

pub fn Kernel.alloc(self) -> mem::Alloc

pub fn Kernel.clock(self) -> sys::Clock

pub fn Kernel.wall_clock(self) -> Option[sys::WallClock]

pub fn Kernel.regions(mut self) -> mem::List[hw::Region]

pub fn Kernel.ring0(mut self) -> Option[hw::Ring0]

pub fn Kernel.iommu(mut self) -> Option[hw::Iommu]

pub fn Kernel.pci_functions(mut self) -> mem::List[hw::PciFunction]

pub fn Kernel.anchor[S: Any and Send](take self, take state: S, take run: task fn(exec: task::Executor) fails task::Cancelled uses wait) -> Never
```

```nan-api
// module: hw

pub open struct Anchor[S: Any and Send] {
} Anchor

pub fn Anchor.with[R: Any, E](body: fn(state: S) -> R fails E) -> Option[R] fails E limits notrap, terminates, noalloc

pub fn cpu_count() -> u32 limits pure, notrap, terminates

pub fn cpu_hardware_id(cpu: u32) -> Option[u64] limits pure, notrap, terminates
```

- The [entry function](27-program-model.md#entry) of a bare target's image starts on the boot CPU with maskable interrupts disabled and every other CPU stopped, on `x86_64-bare` with SSE enabled and AVX enabled when the CPU has it, and on `aarch64-bare` with floating point and Advanced SIMD enabled.
- `k.alloc()` is the root allocator of `boot::Kernel` ([allocators](12-memory.md#allocators)). Its root cell's supply is the **boot heap**, which the [boot area](#physical-memory) holds in the size its target's [descriptor](27-program-model.md#descriptors) states. Every call returns the same handle.
- `k.clock()` is the kernel's clock ([kernel time](#kernel-time)), whose nanoseconds count from the minting of the root grant. Every call returns the same handle.
- `k.wall_clock()` returns `Some` of the kernel's wall clock when the wall clock source of its target's [descriptor](27-program-model.md#descriptors) gave a time as the root grant was minted, the same handle on every call, and `None` otherwise. Its reading is that time, taken as UTC, advanced by the kernel clock.
- The first call of `k.regions()` returns, in address order, a region for each range of usable memory below 128 TiB outside the image, which holds the boot stack, outside the [boot area](#physical-memory) and the [entry root](#physical-memory)'s tables, outside the first MiB on x86_64, outside the [start information](#start-information) with its module list, memory map, command line, modules and their names on a bare target, and outside the device tree and the ranges its memory reservation block lists on `aarch64-bare`; the list is drawn through `k.alloc()` and traps on exhaustion (`trap.exhausted`). Usable memory is, on a bare target with a start information, the entries of type 1 of its memory map, on `aarch64-bare` without one, the ranges of the device tree's memory nodes and, on the UEFI targets, the `EfiConventionalMemory`, `EfiBootServicesCode` and `EfiBootServicesData` entries of the firmware's final memory map. Later calls return an empty list.
- The first call of `k.ring0()` returns `Some` of the machine's one `hw::Ring0` ([privileged intrinsics](23-low-level.md#privileged-intrinsics)); later calls return `None`.
- The first call of `k.iommu()` enables the machine's IOMMU and returns `Some` of its `hw::Iommu` ([I/O domains](23-low-level.md#io-domains)) when the runtime drives it, and `None` otherwise; later calls return `None`. From then on the DMA of every PCI function the IOMMU translates reaches no memory until the function is attached ([PCI functions](#pci-functions)). The runtime drives:
    - on x86_64, Intel VT-d when the ACPI DMAR table lists one remapping unit, in segment 0, and no reserved memory region, the platform remaps interrupts, and the unit queues invalidations, remaps interrupts and walks four-level tables. It translates the functions of bus 0 that the unit includes, and enables interrupt remapping: the machine then delivers no message-signalled interrupt in the format it delivers without remapping, such as those `install_msi` gave before ([locks](#locks));
    - on AArch64, the SMMUv3 of the device tree's `arm,smmu-v3` node when it translates stage 1 through AArch64 tables of 4 KiB pages, walks them coherently and has 8 stream id bits or more. It translates every function of bus 0, its stream id being its requester id.
- The first call of `k.pci_functions()` returns the functions of PCI bus 0 of segment 0 that are endpoints, those of header type 0 and not of the bridge class, in the order of their requester ids, drawn through `k.alloc()`; later calls return an empty list. Each has bus mastering and memory decoding off and INTx disabled. Configuration goes through the configuration mechanism #1 ports on x86_64 and, on AArch64, through the ECAM of the device tree's `pci-host-ecam-generic` node, which lists no function when it lies past the first gigabyte. On AArch64, every memory BAR that holds no address is given one from the 32-bit memory window of that node's `ranges`, aligned to its size, above every BAR the window holds.
- `hw::cpu_count()` is the number of CPUs the platform reports as usable, on AArch64 those [machine discovery](27-program-model.md#descriptors) finds. CPUs are numbered from 0, the boot CPU being 0.
- `hw::cpu_hardware_id(n)` returns `Some` of the hardware id of CPU n, its local APIC id on x86_64 and its affinity on AArch64, the `MPIDR_EL1` affinity fields at the positions `GICD_IROUTER` holds them, and `None` when n is not below `hw::cpu_count()`. On the host targets the hardware id of CPU n is n.
- `hw::Anchor[S]` has no values; it names the anchor of type S.
- On x86_64, each CPU's **CPU tables** are a global descriptor table holding a null descriptor, 64-bit code and data segments of privilege level 0, data and 64-bit code segments of privilege level 3, and the CPU's task-state segment, whose interrupt stack table entries 1 to 7 hold the tops of the CPU's interrupt stacks. On `x86_64-bare` the program entry seam loads the boot CPU's tables; on `x86_64-uefi`, `exit_services` does ([firmware services](#firmware-services)). Step 2 of `anchor` loads those of every other CPU. AArch64 has no CPU tables.

`take k.anchor(take state, run: f)` performs these steps and never returns:

1. It moves `state` into storage the image reserves and installs it as the anchor, visible to every CPU from then on.
2. On each CPU, the boot CPU included, it installs the vector seams and starts the kernel executor, disabling the CPU's [extended state](#floating-point) and enabling maskable interrupts as that executor starts. On x86_64 it loads the CPU's tables where they are not loaded, enables XSAVE with the x87, SSE and AVX state components where the CPU has AVX, and loads an interrupt descriptor table equal to `hw::Idt.of_vectors()`, held in storage the image reserves, and enables `syscall`, with the syscall seam as its target and the trap, interrupt, direction, I/O privilege, nested task and alignment check flags cleared on entry. On AArch64 it makes the vector seams the CPU's exception vectors, which an exception enters on the CPU's interrupt stack, and enables the CPU's interface of the interrupt controller. Each other CPU starts in long mode on x86_64 and at EL1 on AArch64, on the [entry root](#physical-memory).
3. It runs `f` as the first kernel task on that executor, whose value it passes as `exec`. The executor draws task stacks and records through `k.alloc()` ([executors](16-concurrency.md#executors)), and `f` is a [task closure](16-concurrency.md#task-closures), capturing by `take` and Copy only; the executor records `k.clock()` ([capability tables](14-authority.md#capability-tables)).
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

contribute hw::vectors timer = hw::Vector(vector: 0x20, ist: 0, handler: hw::VectorHandler.Irq(on_timer))
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
    Abandon
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
- An interrupt fn's digest is checked as if its head stated `limits notrap, terminates, noalloc` ([limits](10-effects-digests.md#limits)). Its limits clause states only the `stack N` that [static bounds](17-suspension-io.md#static-bounds) requires and a [class](10-effects-digests.md#reversibility); any other limit there is an error (`kernel.irq-limit`) whose exact fix deletes it.
- An `interrupt fn` [function type](10-effects-digests.md#function-types) has the parameters and result of one form and no `fails` or `uses wait` (`kernel.irq-head`), and its limits clause states at most `stack N` (`kernel.irq-limit`). Its values carry the implied limits.
- A call of an interrupt fn, or of a value of an `interrupt fn` type, is an error (`kernel.irq-call`).
- `irq.vector()` is the vector of the interrupt it serves, 0 for a message-signalled interrupt on AArch64, which has no vector ([locks](#locks)).
- `irq.complete()` ends the interrupt it serves, writing the end-of-interrupt register of the CPU's local APIC on x86_64 and of the CPU's interface of the interrupt controller on AArch64. For a device that `hw::IrqShared` installs, the seam ends the interrupt instead, once the interrupt fn of every device the interrupt reaches has returned. `hw::IrqContext` is [linear](05-properties.md#linear-types), so every path completes it.
- `frame.set_ip(ip)` makes the interrupted code resume at `ip`. It is a [trusted primitive](23-low-level.md#trusted-primitives) whose soundness condition is that execution may continue at `ip` with the saved state.

Contributions bind the interrupt and exception forms through `hw::vectors`, whose entries [platform registries](25-registries.md#platform-registries) check against the class of their vector (`registry.platform-entry`).

When a vector fires while the CPU runs in **kernel mode**, at privilege level 0 on x86_64 and at EL1 on AArch64, the CPU enters its seam with maskable interrupts disabled, and:

1. The seam saves the interrupted code's general registers on its interrupt stack, and no [extended state](#floating-point).
2. For an interrupt that reaches devices `hw::IrqShared` installs ([locks](#locks)), it calls the interrupt fn of each in turn, acquiring that device's lock around the call, with the device and a new `hw::IrqContext`, then ends the interrupt. For a vector bound by contribution, it calls the interrupt fn with a new `hw::IrqContext` or with the `hw::TrapFrame` of the interrupted code. An unbound exception vector other than an abort vector and x86_64's vector 2, the non-maskable interrupt's, makes the interrupted code trap when it may; any other unbound vector, and one whose code may not trap, halts the kernel.
3. On `hw::IrqExit.Reschedule` from any interrupt fn it called, when the interrupted code ran in [thread context](#scheduling) with maskable interrupts enabled, outside `hw::Ring0.wait_for_interrupt`, and the anchor is installed, the interrupted thread is [preempted](#preemption); under the same conditions it is also preempted, whatever the interrupt fns gave, once its CPU's policy switches from it. Otherwise `Reschedule` acts as `Resume`. On `hw::IrqExit.Abandon` from an interrupt fn of an exception vector, the interrupted code traps when it may, and the kernel halts otherwise; from any other interrupt fn, `Abandon` acts as `Resume`.
4. The seam restores the saved state and resumes the interrupted code. After an interrupt fn for an abort vector returns, the kernel halts instead.

Code **may trap** where a vector interrupted it when it is a kernel task's that ran with maskable interrupts enabled on the task's own stack, and the anchor is installed. Its trap there, of kind `trap.exception`, abandons the task's domain ([abandoning a domain](11-errors-domains.md#abandoning-a-domain)); its report locates the interrupted instruction as if it called the trap, with the operand line `vector = v, code = c`, followed by `, address = a` for a page fault, v being the vector, c the error code the CPU pushed on x86_64 or the exception's syndrome on AArch64, and a the faulting address. An exception in a kernel task means that trusted code broke a soundness condition or that the hardware faulted: abandoning the task's domain restores none of the guarantees that broke.

Each CPU has an interrupt stack, reserved by the image, for each `ist` value that its bindings use, those of `hw::IrqShared.install` using 0 ([platform registries](25-registries.md#platform-registries)). A seam entered while its stack is in use on its CPU continues on it below the frames in use.

- The stack of `ist` 0 holds at least the largest stack use among the lowered interrupt fns that run on it for interrupt vectors, plus the largest among those for exception vectors, plus two [seam frames](27-program-model.md#descriptors), plus a call of a [policy](#policies) hook.
- The stack of a nonzero `ist` holds at least the largest stack use among the lowered interrupt fns that run on it, plus one seam frame, plus a call of a policy hook.

On AArch64 the interrupt controller delivers an interrupt only while it is enabled: the anchor enables the timer's vector and each interrupt vector a contribution binds, on every CPU, routing those from 32 up to the boot CPU; installing the first device on a vector enables it and routes it to that device's CPU ([locks](#locks)), and uninstalling the last one disables it.

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

## User mode

**User mode** is execution at privilege level 3 on x86_64 and at EL0 on AArch64, which reaches only the memory the running CPU's root maps for it ([privileged intrinsics](23-low-level.md#privileged-intrinsics)). A kernel task runs a **user thread**, user code in an `hw::UserState`, through `hw::Ring0.run_user`, which returns at the thread's next system call, exception or interrupt. The task then chooses which thread runs next: the same one, another one, or none.

```nan-api
// module: hw

pub closed struct UserState derives Copy {
    pub regs: Array[Untrusted[u64], 31]
    pub sp: Untrusted[u64]
    pub ip: Untrusted[u64]
    pub flags: Untrusted[u64]
} UserState

pub closed enum UserExit derives Copy, Eq {
    Syscall
    Fault(UserFault)
    Interrupted
} UserExit

pub closed struct UserFault derives Copy, Eq {
    pub vector: u8
    pub code: u64
    pub address: u64
} UserFault
```

- `state.regs` holds the general registers: on x86_64 `rax`, `rcx`, `rdx`, `rbx`, `rbp`, `rsi`, `rdi` and `r8` to `r15` in its first 15 entries, the others unused, and on AArch64 `x0` to `x30`. `sp` is the stack pointer, `ip` the address of the next instruction, and `flags` the flags: `RFLAGS` on x86_64, and on AArch64 the condition flags at their `SPSR_EL1` positions. User code sets them, so they are [untrusted](15-untrusted.md#sources).
- `cpu.run_user(mut state, mut extended)` runs the thread on the running CPU, under its root, from `state`, with maskable interrupts enabled and of its flags only the arithmetic and direction flags on x86_64 and the condition flags on AArch64. At the thread's next exit it writes the thread's state into `state` and returns:
    - `Syscall` for the system call instruction, `syscall` on x86_64 and `svc` on AArch64. `state.ip` addresses the instruction after it; on x86_64 `rcx` and `r11` hold what `syscall` leaves in them, that address and the flags.
    - `Fault(f)` for an exception the thread raises: `f.vector` is its vector, `f.code` the error code the CPU pushed or 0 on x86_64 and the syndrome on AArch64, and `f.address` the faulting address for a vector of class `PageFault` and 0 for other vectors ([interrupt functions](#interrupt-functions)). On x86_64, vectors 2, 8 and 18 are interrupts for this rule, and an `ip` outside the canonical lower half gives `Fault` with vector 13 and code and address 0 without running the thread.
    - `Interrupted` for an interrupt, once its seam has served it as one interrupting the calling task in the call, with the interrupt state the task called with, where the task may be [preempted](#preemption).
- `run_user` returns `Interrupted` at once inside an interrupt fn and before the anchor is installed. It restores the caller's interrupt state as it returns.
- Until the call returns, the calling task owns the [extended state](#floating-point) `extended` holds, as `hw::ExtendedState.with` makes it, and the thread runs with it: its registers start as the state holds them, and the state holds them as the thread leaves them. With `None` the task owns none, so the thread runs with the extended state disabled and its first floating-point or vector instruction raises an exception: vector 7 on x86_64, and on AArch64 vector 0 with a syndrome of class `0x07`. On x86_64 a thread runs at I/O privilege level 0 with the CPU tables' privilege-level-3 segments. The seams save a thread's state and serve its interrupts on the kernel stack of the task running it, and no register or flag of kernel code reaches the thread.

```nan
// file: kernel/package.nan
//! User threads for a bare kernel.

package kernel {
    language 1
    targets x86_64-bare
    allow priv in kernel
} kernel

// file: kernel/kernel.nan
//! Runs two user threads, each with the extended state it may own, in turn until one faults, answering each system call with 0.

pub fn run_pair(cpu: hw::Ring0, mut threads: Array[hw::UserState, 2], mut extended: Array[Option[hw::ExtendedState], 2]) -> hw::UserFault {
    var next: usize = 0
    loop {
        match cpu.run_user(mut threads[next], extended: mut extended[next]) {
            hw::UserExit.Syscall => {
                threads[next].regs[0] = Untrusted.of(0)
            }
            hw::UserExit.Fault(fault) => return fault
            hw::UserExit.Interrupted => {
            }
        }
        next = 1 - next
    }
} run_pair
```

```nan
// file: kernel/package.nan
//! User threads for a bare kernel.

package kernel {
    language 1
    targets x86_64-bare
} kernel

// file: kernel/kernel.nan
//! Runs a user thread until it faults.

pub fn run_thread(cpu: hw::Ring0, mut thread: hw::UserState, mut extended: Option[hw::ExtendedState]) {
    while cpu.run_user(mut thread, mut extended) != hw::UserExit.Fault(hw::UserFault(vector: 6, code: 0, address: 0)) { // error: authority.ungranted
    }
} run_thread
```

### Split tables

A **user root** is a page-table root under which `hw::Ring0.run_user_split` runs a thread, in place of the running root, which stays installed for kernel code. A CPU affected by Meltdown reads through the running root's supervisor mappings speculatively from user mode; under a user root that maps little beyond the thread's own pages it reads nothing more.

```nan-api
// module: hw

pub closed struct EntryArea derives Copy, Eq {
    pub base: u64
    pub len: u64
} EntryArea
```

- The **entry area** is the memory the seams use before they reinstall the running root: the image and the memory it reserves, which hold the vector seams and their user paths, the CPU records with each CPU's tables and entry stack, the interrupt descriptor table and the stacks of nonzero `ist`. `cpu.entry_area()` gives its first address as `base` and its length in bytes as `len`.
- `cpu.run_user_split(mut state, mut extended, root)` runs the thread as `run_user` does, but the user path installs `root` just before the thread runs and reinstalls the running root at the thread's exit before any other kernel code runs. On x86_64 with PCIDs enabled, translations the CPU caches from `root` are tagged with the running root's PCID with bit 11 flipped, and both roots keep their cached translations across the switches; on AArch64, with the running root's ASID with bit 7 flipped, and none are invalidated.
- `run_user_split` is a trusted primitive whose soundness condition is that `root` maps the entry area at the addresses and with the attributes the running root maps it; that no page `root` maps for user mode holds what the soundness condition of `write_cr3` keeps out of such pages ([privileged intrinsics](23-low-level.md#privileged-intrinsics)); and that each translation cached under the user root's tag maps as `root` does. `root` need map nothing else: unlike a root `write_cr3` installs, it need not map the frames, regions and windows of the kernel.
- A translation that the running root marks global on x86_64, or does not mark non-global on AArch64, stays usable under the user root, so the running root keeps out of the user root's reach only what it maps non-globally.

### User frames

A **user frame** is an `hw::UserFrame`, a physical frame of 4 KiB that the kernel holds while user threads map it, such as a doorbell or a ring shared with them. User code may change its words at any time, so the kernel reads them as untrusted.

```nan-api
// module: hw

pub open linear struct UserFrame {
} UserFrame

pub fn UserFrame.of(take frame: PhysFrame[4_096]) -> UserFrame

pub fn UserFrame.base(self) -> u64 limits pure, notrap, terminates

pub fn UserFrame.load(self, at: Offset[u64]) -> Untrusted[u64]

pub fn UserFrame.store(self, at: Offset[u64], value: u64)

pub fn UserFrame.fetch_or(self, at: Offset[u64], bits: u64) -> Untrusted[u64]

pub fn UserFrame.into_frame(take self) -> PhysFrame[4_096]
```

- `hw::UserFrame.of(take frame)` makes `frame` a user frame, and `base()` gives its physical address.
- `frame.load(at)` reads the word `at.bytes` bytes into the frame with acquire ordering, `frame.store(at, value)` writes it with release ordering, and `frame.fetch_or(at, bits)` sets `bits` in it with acquire and release ordering in one atomic step and gives the word it held. Each traps (`trap.bounds`) when `at.bytes` is not a multiple of 8 or the word does not lie inside the frame, and has the sync fact ([digests](10-effects-digests.md#digests)).
- `take frame.into_frame()` gives the frame back. It is a trusted primitive whose soundness condition is that no root maps the frame for user mode and no CPU caches a translation of it for user mode.

## Interrupt state

`hw::IrqsDisabled` is the [state token](05-properties.md#state-tokens) standing for maskable interrupts being disabled on the CPU running its holder, which therefore never changes CPU. `hw::Ring0.wait_for_interrupt` enables them while its CPU halts, so interrupt fns may run there meanwhile, but no preemption moves the holder.

```nan-api
// module: hw

pub open view struct IrqsDisabled {
} IrqsDisabled

pub fn without_irqs[T: Any, E](body: fn(t: IrqsDisabled) -> T fails E limits notrap, terminates) -> T fails E
```

- Its values come only from `IrqContext.irqs_disabled`, `TrapFrame.irqs_disabled` and `hw::without_irqs`.
- `hw::without_irqs(body)` saves whether maskable interrupts are enabled on the current CPU, disables them, calls `body` with a token, restores the saved state and returns what `body` returned or fails with its failure. As `body` terminates, a task asked to stop reaches a point where it stops ([abandoning a domain](11-errors-domains.md#abandoning-a-domain)).
- On the host targets, each executor thread is a CPU for this chapter and has no maskable interrupts: `hw::cpu_count()` is the number of executor threads, `without_irqs` only calls `body`, `IrqContext.complete` does nothing and `hw::IrqShared` installs devices on no interrupt ([locks](#locks)).

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

pub open linear struct IrqShared[T: Any and Send] {
} IrqShared

pub fn IrqShared.install(take dev: T, vector: u8, on_irq: interrupt fn(mut dev: T, take irq: IrqContext) -> IrqExit) -> IrqShared[T]
    requires vector >= 32

pub fn IrqShared.install_on(take dev: T, cpu: u32, vector: u8, on_irq: interrupt fn(mut dev: T, take irq: IrqContext) -> IrqExit) -> IrqShared[T]
    requires vector >= 32

pub fn IrqShared.install_msi(take dev: T, cpu: u32, source: u32, on_irq: interrupt fn(mut dev: T, take irq: IrqContext) -> IrqExit) -> Result[IrqShared[T], MsiRefusal[T]]

pub fn IrqShared.message(self) -> Option[MsiMessage]

pub fn IrqShared.with[R: Any](self, t: IrqsDisabled, body: fn(mut dev: T) -> R limits notrap, terminates, noalloc) -> R limits notrap, terminates, noalloc

pub fn IrqShared.uninstall(take self) -> T

pub closed struct MsiMessage derives Copy, Eq {
    pub address: u64
    pub data: u32
} MsiMessage

pub closed enum MsiError derives Copy, Eq, Render {
    Exhausted
    Unreachable
    Unsupported
} MsiError

pub closed struct MsiRefusal[T: Any and Send] {
    pub dev: T
    pub error: MsiError
} MsiRefusal
```

- `with` spins until it acquires the lock, calls `body` with exclusive `mut` access to the value, releases the lock and returns `body`'s result. Acquisition orders `body` after every earlier holder's accesses.
- A `with` on a lock that the calling CPU already holds never returns: the kernel halts, and on the host targets the innermost process or test [domain](11-errors-domains.md#domains) holding the call is [abandoned](11-errors-domains.md#abandoning-a-domain), with the halt report ([kernel halts](#kernel-halts)) in place of a trap report.
- A spin lock is never [poisoned](11-errors-domains.md#poisoning): its bodies cannot trap and run with maskable interrupts disabled, where no task stops.

`hw::IrqShared[T]` keeps a device of type T behind an IRQ-safe lock that the interrupt path also takes:

- `install(take dev, vector: v, on_irq: f)` keeps `dev`, installs it with f on interrupt vector v of every CPU and returns the linear `hw::IrqShared[T]`. `install_on(take dev, cpu: n, vector: v, on_irq: f)` installs it on vector v of CPU n alone, and traps when n is not below `hw::cpu_count()` (`trap.bounds`).
- Installing on a vector a contribution binds, or on `hw::SIGNAL_VECTOR`, traps (`trap.duplicate-access`). Devices installed on the same vector of a CPU share it, as devices on one level-triggered line do: when it fires there, the interrupt fn of each runs, those `install_on` installed on that CPU first, each group in the order of installation.
- On AArch64 a vector from 32 up is a shared peripheral interrupt, which the interrupt controller routes to one CPU: CPU n for `install_on`, the boot CPU for `install`. Installing on a vector whose devices are installed for another CPU traps (`trap.duplicate-access`).
- `install_msi(take dev, cpu: n, source: s, on_irq: f)` installs `dev` with f on a new **message-signalled interrupt** delivered to CPU n and returns `Ok` of the `hw::IrqShared[T]`, whose `message()` returns `Some` of the address and data a device writes to raise it; for every other installed device it returns `None`. Otherwise it returns `Err` of an `hw::MsiRefusal[T]` holding `dev` and an `hw::MsiError`: `Exhausted` when no such interrupt is free, `Unreachable` when no message reaches CPU n, and `Unsupported` when the machine delivers none, as on the host targets. It traps when n is not below `hw::cpu_count()` (`trap.bounds`).
- On x86_64 the interrupt is the lowest vector from `0x21` to `0xFD` that no contribution binds and no device is installed on for CPU n; its message is a fixed, edge-triggered interrupt to CPU n's local APIC in physical destination mode, with address `0xFEE0_0000 | id << 12` and the vector as data, and is `Unreachable` when that APIC id exceeds 255. `source` is unused. Once `boot::Kernel.iommu` has enabled interrupt remapping, the message names instead the remapping entry of CPU n and the vector, in the remapped format with data 0, which reaches APIC ids past 255 when the CPUs run in x2APIC mode.
- On AArch64 the interrupt is one of the 8,192 LPIs of the interrupt controller's ITS, which translates the event the data names, for device id s, to it; `source` is the device id the ITS receives with the device's writes, a PCI function's requester id on the emulator's machine. Its message's address is that of the ITS's `GITS_TRANSLATER` and its data the event. A device id the ITS does not accept, or a machine without an ITS, is `Unsupported`; an ITS keeps at most 256 device ids.
- `with` reaches the device from other code, as `SpinLock.with` does.
- `take shared.uninstall()` removes the device from its interrupt, spins until no CPU runs f for it, and returns the device.
- An installed device is a resource of the domain that holds its `hw::IrqShared`, which the domain's [settlement](11-errors-domains.md#settlement) uninstalls.
- `install`, `install_on` and `install_msi` are device operations and `uninstall` a confined one ([manifest grants](14-authority.md#manifest-grants)).

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

```nan
// file: queues/package.nan
//! Per-CPU completion queues for a bare kernel.

package queues {
    language 1
    targets x86_64-bare
    allow device in queues
} queues

// file: queues/queues.nan
//! A completion queue whose interrupt reaches the CPU that owns it.

const QUEUE_VECTOR: u8 = 0x40

pub closed struct Queue {
    pub completed: u64
} Queue

/// Installs CPU `cpu`'s queue on a message-signalled interrupt of that CPU, whose message the
/// device `source` is then given, or on vector `QUEUE_VECTOR` of that CPU without one.
pub fn attach(cpu: u32, source: u32) -> hw::IrqShared[Queue] {
    match hw::IrqShared.install_msi(Queue(completed: 0), cpu, source, on_irq: Queue.on_irq) {
        Ok(shared) => return shared
        Err(refusal) => return hw::IrqShared.install_on(take refusal.dev, cpu, vector: QUEUE_VECTOR, on_irq: Queue.on_irq)
    }
} attach

interrupt fn Queue.on_irq(mut self, take irq: hw::IrqContext) -> hw::IrqExit limits stack 256 {
    self.completed = self.completed.wrapping_add(1)
    take irq.complete()
    return hw::IrqExit.Resume
} Queue.on_irq
```

## PCI functions

A **PCI function** is an `hw::PciFunction`, a function of PCI bus 0 that `boot::Kernel.pci_functions` lists ([anchor](#anchor)). Its DMA reaches the memory of the space it is attached to, through the IOMMU when one is enabled ([I/O domains](23-low-level.md#io-domains)), and its register windows and message-signalled interrupts derive from it with no trusted premise.

```nan-api
// module: hw

pub open linear struct PciFunction {
} PciFunction

pub closed struct PciAddress derives Copy, Eq, Ord, Hash, Render {
    pub bus: u8
    pub device: u8
    pub function: u8
} PciAddress

pub closed enum PciError derives Copy, Eq, Render {
    NotMemory
    TooSmall
    Taken
    Unmapped
} PciError

pub fn PciFunction.address(self) -> PciAddress

pub fn PciFunction.config_read(self, offset: u8) -> u32
    requires offset % 4 == 0

pub fn Mmio.of_bar(mut function: PciFunction, index: u8) -> Mmio[R] fails PciError

pub fn PciFunction.attach(mut self, space: IoSpace) fails IommuError

pub fn PciFunction.attach_physical(mut self) fails IommuError

pub fn PciFunction.msix[T: Any and Send](mut self, entry: u16, cpu: u32, take dev: T, on_irq: interrupt fn(mut dev: T, take irq: IrqContext) -> IrqExit) -> Result[IrqShared[T], MsiRefusal[T]]

pub fn PciFunction.release(take self)
```

- `function.address()` is its bus, device and function numbers; its **requester id** is `device << 3 | function` on bus 0. `function.config_read(offset)` reads the double word at `offset` of its configuration space. Nothing writes its configuration but the runtime.
- `hw::Mmio[R].of_bar(mut function, index: i)` enables the function's memory decoding and returns a window over R at the start of its BAR i, the window `hw::Mmio.map` would give at that address ([register windows](23-low-level.md#register-windows)). It fails with `NotMemory` when BAR i is not a memory BAR, with `TooSmall` when the BAR holds fewer bytes than R spans from offset 0 to past its last register, with `Taken` when an earlier call gave a window over the BAR, and with `Unmapped` when the BAR lies outside the memory the entry root maps uncached, the last gigabyte below 4 GiB on x86_64 and the first gigabyte on AArch64 ([physical memory](#physical-memory)).
- `mut function.attach(space)` attaches the function to the domain of a space and enables its bus mastering: from then on its DMA reaches the domain's pages at their own addresses, and on AArch64 the page of the ITS's `GITS_TRANSLATER`, which its message-signalled interrupts write. It fails with `Physical` for the physical space, with `Closed` for a stale one, and with `Unsupported` when the IOMMU does not translate the function. `mut function.attach_physical()` attaches it to physical memory, which its DMA then reaches whole, and enables its bus mastering; it fails with `Unsupported` when the IOMMU translates the function and cannot pass its accesses through. A function attached anew leaves the space it was attached to.
- `mut function.msix(entry: e, cpu: n, take dev, on_irq: f)` installs `dev` as `install_msi` does with the function's requester id as `source` ([locks](#locks)), then writes the interrupt's message to entry e of the function's MSI-X table, unmasked, and enables MSI-X. Once the IOMMU remaps interrupts on x86_64, the remapping entry accepts the function's messages alone; on AArch64 the ITS translates the events of the function's device id alone. It refuses with `Unsupported` when the function is not attached, has no MSI-X capability or entry e, or its table lies outside the memory the entry root maps uncached, and otherwise as `install_msi` does.
- `take function.release()` cuts the function off: its bus mastering and memory decoding end, and its DMA reaches no memory once the IOMMU no longer caches its attachment.
- An `hw::PciFunction` is a resource of the domain run holding it, which its [settlement](11-errors-domains.md#settlement) cuts off as `release` does.
- Every operation of this section is a confined operation ([I/O domains](23-low-level.md#io-domains)) but `attach_physical`, which is a device operation (`authority.ungranted`). Each has the device fact.

```nan
// file: driver/package.nan
//! An entropy driver confined to an I/O domain.

package driver {
    language 1
    targets x86_64-bare
    allow confined in driver
} driver

// file: driver/driver.nan
//! Finds a modern virtio entropy device and maps its common configuration.

const VIRTIO_ENTROPY: u32 = 0x1044_1AF4

pub registers Common {
    device_status: u8 at 0x14 read write
} Common

pub fn is_entropy(function: hw::PciFunction) -> bool {
    return function.config_read(0) == VIRTIO_ENTROPY
} is_entropy

pub fn reset(mut function: hw::PciFunction, bar: u8) fails hw::PciError {
    var common = try hw::Mmio[Common].of_bar(mut function, index: bar)
    mut common.write(Common.device_status, value: 0)
    take common.unmap()
} reset
```

```nan
// file: driver/package.nan
//! An entropy driver confined to an I/O domain.

package driver {
    language 1
    targets x86_64-bare
    allow confined in driver
} driver

// file: driver/driver.nan
//! Lets a device reach all physical memory.

pub fn unconfine(mut function: hw::PciFunction) -> hw::PciAddress fails hw::IommuError {
    try mut function.attach_physical() // error: authority.ungranted
    return function.address()
} unconfine
```

## Cross-CPU operations

The **CPU signal** is the interrupt one CPU sends another on `hw::SIGNAL_VECTOR`, which the runtime keeps on every machine target ([descriptors](27-program-model.md#descriptors)).

```nan-api
// module: hw

pub const SIGNAL_VECTOR: u8 = 0xFE
```

- `cpu.signal_cpu(n)` sends the CPU signal to CPU n when it is a started CPU, the calling one included, and does nothing otherwise. A CPU takes the signal once it has maskable interrupts enabled; signals sent to it meanwhile may arrive as one.
- When the signal arrives, the CPU serves the shootdowns posted to it, then calls the interrupt fn that an `hw::vectors` contribution binds to `hw::SIGNAL_VECTOR`, or only ends the interrupt without one. The settlement of an abandoned domain sends it to the CPU of each running task it stops ([preemption](#preemption)), and placing a ready task sends it to a CPU that idles or runs a later task ([policies](#policies)).
- `cpu.invalidate_everywhere(scope)` removes, on every started CPU, the cached translations of the page that `Page` holds under every PCID or ASID, or every cached translation for `All`, and returns once each CPU has. On x86_64 the calling CPU removes its own, posts a **shootdown** of the scope to each other started CPU, sends it the CPU signal and waits, with maskable interrupts enabled, until each has served it; one shootdown runs at a time, and a CPU waiting to start one serves the others meanwhile. On AArch64 one broadcast invalidation reaches every CPU.
- `invalidate_everywhere` is a cancellation point ([acknowledged cancellation](17-suspension-io.md#acknowledged-cancellation)) whose invalidations acknowledge a cancellation only once done: called cancelled, it fails at once and removes nothing; once started, it completes.
- Interrupt fns and bodies with maskable interrupts disabled cannot wait, so no CPU waits for a shootdown that waits for it. A CPU that keeps maskable interrupts disabled delays every signal to it and every shootdown waiting for it.

```nan
// file: kernel/package.nan
//! Address spaces shared by every CPU of a bare kernel.

package kernel {
    language 1
    targets x86_64-bare
    allow priv in kernel
} kernel

// file: kernel/kernel.nan
//! Ends each CPU signal and drops a page's translations on every CPU.

pub interrupt fn on_signal(take irq: hw::IrqContext) -> hw::IrqExit limits stack 512 {
    take irq.complete()
    return hw::IrqExit.Resume
} on_signal

contribute hw::vectors signal = hw::Vector(vector: hw::SIGNAL_VECTOR, ist: 0, handler: hw::VectorHandler.Irq(on_signal))

pub fn unmap_everywhere(cpu: hw::Ring0, page: hw::VirtAddr) fails task::Cancelled uses wait {
    try await cpu.invalidate_everywhere(hw::TlbScope.Page(page))
} unmap_everywhere
```

## Scheduling

**Thread context** is kernel execution in a schedulable thread: every kernel task. Each kernel task runs as one **thread**, whose stack and saved registers the kernel executor owns. The boot context and interrupt fns are not thread context. A CPU **idles** while it finds no task to run.

```nan-api
// module: hw

pub open struct PerCpu[T: Send] {
} PerCpu

pub fn PerCpu.of(init: fn(cpu: u32) -> T, alloc: mem::Alloc) -> PerCpu[T]

pub fn PerCpu.with[R: Any](self, t: IrqsDisabled, body: fn(mut value: T) -> R limits notrap, terminates, noalloc) -> R limits notrap, terminates, noalloc
```

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

### Policies

Each CPU has a **run queue** of ready tasks, ordered by key, tasks of equal keys in the order they joined it. A kernel's **policy** decides on which CPU's queue and with which key a task that becomes ready is placed, from which CPUs a CPU with an empty queue takes a task, how long a task runs before it is preempted, and how a CPU idles.

```nan-api
// module: boot

pub fn Kernel.policy[P: hw::Policy and Send](mut self, take policy: P)
```

```nan-api
// module: hw

pub interface Policy {
    fn place(self, t: IrqsDisabled, task: TaskView) -> Placement limits notrap, terminates, noalloc, stack 4_096
    fn steal(self, t: IrqsDisabled, cpu: u32, round: u32) -> Option[u32] limits notrap, terminates, noalloc, stack 4_096
    fn slice(self, t: IrqsDisabled, cpu: u32, task: TaskView) -> u64 limits notrap, terminates, noalloc, stack 4_096
    fn idle(self, t: IrqsDisabled, cpu: u32, until_ns: Option[u64]) -> Idle limits notrap, terminates, noalloc, stack 4_096
} Policy

pub closed struct TaskView derives Copy, Eq {
    pub task: u64
    pub executor: u64
    pub share: task::Share
    pub last_cpu: Option[u32]
    pub ran_ns: u64
    pub deadline_ns: Option[u64]
} TaskView

pub closed struct Placement derives Copy, Eq {
    pub cpu: u32
    pub key: u64
} Placement

pub closed enum Idle derives Copy, Eq {
    Halt
    Poll
} Idle
```

- `k.policy(take p)` makes p the kernel's policy for the rest of the run, keeping p in memory of the support routines; a second call traps (`trap.duplicate-access`), and so does a call when that memory is exhausted (`trap.exhausted`). Without a call the **default policy** applies.
- The executor calls each method of the policy, a **hook**, on the CPU it concerns, with maskable interrupts disabled and its `t` witnessing that. A task made ready while a hook runs on its CPU joins that CPU's queue last, without a call of `place`. Neither the host targets nor the seeded scheduler call hooks.
- An `hw::TaskView` shows a hook a task: `task` distinguishes it from every other live task; `executor` distinguishes the executor it was started on from every other live executor; `share` is that executor's [share](16-concurrency.md#executors); `last_cpu` is the CPU it last ran on, `None` before it first runs; `ran_ns` is the nanoseconds of the kernel clock it has run; and `deadline_ns` is the earliest end, in nanoseconds of the kernel clock, of the deadlines of the `task::within` and `task::within_ns` constructs it runs, `None` without one.
- When a task becomes ready, as it starts, as an event wakes it, as it is preempted and as a [serial executor](16-concurrency.md#executors)'s turn passes to it, `place(t, task)` gives its placement: the task joins the queue of the CPU `cpu` names, of the running CPU when `cpu` is not below `hw::cpu_count()`, with the key `key`. The CPU signal ([cross-CPU operations](#cross-cpu-operations)) then goes to the CPU it names when that is another CPU that idles or runs a task placed with a greater key, or when it is the running CPU, outside an interrupt fn, running a task placed with a greater key.
- A CPU runs the first task of its queue. When its queue is empty it calls `steal(t, cpu, round)` for rounds 0, 1 and on, at most `hw::cpu_count()` − 1 of them, until a round gives `None` or names another CPU whose queue holds a task, the first of which it then runs.
- As a CPU switches to a task, `slice(t, cpu, task)` gives the task's **slice**: it is preempted once that many nanoseconds of the kernel clock have passed since, unless it suspends before; 0 gives no slice.
- A CPU finding no task calls `idle(t, cpu, until_ns)`, `until_ns` being the earliest deadline of the kernel clock, `None` when none is armed. `Halt` halts the CPU until an interrupt arrives, which the CPU signal and its timer bring; `Poll` returns at once. Either way the CPU then looks for a task again.
- The default policy places a task on the CPU it last ran on when that CPU idles, otherwise on the lowest-numbered CPU that idles, otherwise on the running CPU, with a key that grows with the order in which tasks become ready; steals in round r from CPU `(cpu + 1 + r) % hw::cpu_count()`; gives no slice; and halts.
- Every method of a policy type is judged against the `stack 4_096` its interface method demands ([stack limits](10-effects-digests.md#stack-limits)), and every stack a hook runs on holds a call of it ([static bounds](17-suspension-io.md#static-bounds)).

```nan
// file: earliest/package.nan
//! A bare kernel running its tasks earliest deadline first.

package earliest {
    language 1
    targets x86_64-bare
    entry main
} earliest

// file: earliest/earliest.nan
//! Places every task on CPU 0, keyed by its earliest deadline, in slices of 1 ms.

struct Earliest {
} Earliest

impl Earliest: hw::Policy

fn Earliest.place(self, t: hw::IrqsDisabled, task: hw::TaskView) -> hw::Placement {
    return hw::Placement(cpu: 0, key: task.deadline_ns.value_or(0))
} Earliest.place

fn Earliest.steal(self, t: hw::IrqsDisabled, cpu: u32, round: u32) -> Option[u32] {
    return None
} Earliest.steal

fn Earliest.slice(self, t: hw::IrqsDisabled, cpu: u32, task: hw::TaskView) -> u64 {
    return 1_000_000
} Earliest.slice

fn Earliest.idle(self, t: hw::IrqsDisabled, cpu: u32, until_ns: Option[u64]) -> hw::Idle {
    return hw::Idle.Halt
} Earliest.idle

fn main(take k: boot::Kernel) -> Never limits stack 16_384 {
    let clock = k.clock()
    mut k.policy(Earliest())
    take k.anchor(Unit, run: fn(exec: task::Executor) fails task::Cancelled uses wait limits stack 16_384 {
        loop {
            discard try await task::within_ns(clock, ns: 1_000_000, body: fn() fails task::Cancelled uses wait {
                try await clock.sleep_ns(500_000)
            })
        }
    })
} main
```

```nan
// file: weighted/package.nan
//! A bare kernel weighing its tasks.

package weighted {
    language 1
    targets x86_64-bare
    entry main
} weighted

// file: weighted/weighted.nan
//! Places every task on CPU 0, keyed by the time it ran over its executor's weight.

struct Weighted {
} Weighted

impl Weighted: hw::Policy

fn Weighted.place(self, t: hw::IrqsDisabled, task: hw::TaskView) -> hw::Placement {
    return hw::Placement(cpu: 0, key: task.ran_ns / u64.widen(task.share.weight))
} Weighted.place

fn Weighted.steal(self, t: hw::IrqsDisabled, cpu: u32, round: u32) -> Option[u32] {
    return None
} Weighted.steal

fn Weighted.slice(self, t: hw::IrqsDisabled, cpu: u32, task: hw::TaskView) -> u64 { // error: limit.stack-exceeded
    let lengths = Array[u64, 1_024].filled(1_000_000)
    return lengths[usize.widen(cpu) % 1_024]
} Weighted.slice

fn Weighted.idle(self, t: hw::IrqsDisabled, cpu: u32, until_ns: Option[u64]) -> hw::Idle {
    return hw::Idle.Halt
} Weighted.idle

fn main(take k: boot::Kernel) -> Never limits stack 16_384 {
    let clock = k.clock()
    mut k.policy(Weighted())
    take k.anchor(Unit, run: fn(exec: task::Executor) fails task::Cancelled uses wait limits stack 16_384 {
        loop {
            try await clock.sleep_ns(1_000_000)
        }
    })
} main
```

### Kernel suspension

On the machine targets, a task suspends ([stackful tasks](17-suspension-io.md#stackful-tasks)) through the kernel executor's suspend routine, part of the stack-switching [support routine](27-program-model.md#runtime-support):

1. The waiting primitive registers the task as a waiter of the event it awaits.
2. The routine disables maskable interrupts and switches the CPU to the next task it runs, idling until it finds one ([policies](#policies)).
3. When the event occurs, its operation makes the task ready, which places it ([policies](#policies)).
4. On resumption the routine restores the saved interrupt state.

Interrupt fns make tasks ready only through `hw::Signal.post`; a task waiting for a device waits on a signal that the device's interrupt fn posts.

### Preemption

On the machine targets, the [platform package](28-stdlib.md#platform-packages) contributes the one `hw::preempt` entry ([platform registries](25-registries.md#platform-registries)). **Preemption** of a thread proceeds in this order:

1. The seam moves the thread's saved state from the interrupt stack onto the thread's own stack.
2. It calls the landing fn that the `handler` of the `hw::preempt` entry holds there, with maskable interrupts disabled. That landing fn makes the thread ready and switches the CPU to its next task as [kernel suspension](#kernel-suspension) step 2 does.
3. When a later switch resumes the thread, the landing fn returns, and the seam restores the saved state and resumes the thread.

A kernel task running with maskable interrupts enabled on its own stack, outside `hw::Ring0.wait_for_interrupt`, is preempted at an interrupt reaching its CPU when its interrupt fn gives `hw::IrqExit.Reschedule`, when its slice has ended, or when the first task of its CPU's queue was placed with a smaller key than it ([policies](#policies)), and never otherwise.

A kernel task asked to stop ([abandoning a domain](11-errors-domains.md#abandoning-a-domain)) is preempted at every interrupt reaching its CPU while it runs with maskable interrupts enabled on its own stack, outside `hw::Ring0.wait_for_interrupt` and outside every operation it completes before stopping, whatever its interrupt fn gave, and is never preempted elsewhere. It then stops instead of becoming ready. Each pacing tick of its CPU's timer is such an interrupt.

A kernel task's stack is drawn through the executor's allocator as the task starts. It holds at least the N of its entry's `limits stack N` ([task stacks](17-suspension-io.md#task-stacks)), plus the stack use of the `hw::preempt` landing fn, plus one [seam frame](27-program-model.md#descriptors), plus a call of a policy hook.

### Kernel time

The **kernel clock** is the counter the clock field of the target's [descriptor](27-program-model.md#descriptors) names, in nanoseconds since the runtime started; a reading on any CPU is never below one any CPU made before. A **deadline** of the kernel clock is the end of a `sys::Clock` sleep or of a `task::within` or `task::within_ns` deadline; every CPU sees every deadline.

- Each CPU's timer fires at the earliest deadline, at the end of the slice of the task it runs and at its **pacing ticks**, 10 ms apart, whichever comes first. A CPU has pacing ticks while it runs a task, and when idle only if a contribution binds the timer's vector, so an idle CPU of another kernel sleeps until a deadline or the CPU signal. Each firing passes the deadlines due, then calls the interrupt fn bound to the timer's vector, if any, at a pacing tick alone.
- A deadline armed on a CPU ahead of every other deadline programs that CPU's timer to fire at it.
- Cancelling a task that sleeps ends its sleep at once, and a deadline that its construct no longer needs leaves the deadlines at once.

```nan
// file: pacer/package.nan
//! A bare kernel pausing on its clock.

package pacer {
    language 1
    targets x86_64-bare
    entry main
} pacer

// file: pacer/pacer.nan
//! The first task pauses for 500 µs at a time, each pause under a 1 ms deadline.

fn main(take k: boot::Kernel) -> Never limits stack 16_384 {
    let clock = k.clock()
    take k.anchor(Unit, run: fn(exec: task::Executor) fails task::Cancelled uses wait limits stack 16_384 {
        loop {
            discard try await task::within_ns(clock, ns: 1_000_000, body: fn() fails task::Cancelled uses wait {
                try await clock.sleep_ns(500_000)
            })
        }
    })
} main
```

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

- On the machine targets, the **entry root** is the page-table root in use when the entry function starts on a bare target, and the one that `exit_services` installs on a UEFI target. Some of its tables lie in memory the image reserves: on x86_64 those mapping the first 4 GiB, on AArch64 its level-0 table and first level-1 table. On AArch64 it maps, at their physical addresses, each 2 MiB block that holds memory the platform lists or the image as normal memory, and every other block of the first 512 GiB and of each further 512 GiB below the top of that memory as device memory that no code runs from; the memory is that of the device tree's memory nodes, the usable, ACPI reclaimable and ACPI NVS entries of the start information's memory map, or the entries of the firmware's final memory map other than reserved, unusable and memory-mapped I/O ones. Before the entry landing fn on a bare target, and in `exit_services` before installing it on a UEFI target, the runtime extends it in place over the usable memory above 4 GiB on x86_64, and fills it on AArch64, with tables that lie at the end of the boot area on a bare target and, on a UEFI target, at the lowest 64 KiB-aligned address of usable memory outside the image from which they lie wholly in that usable memory below 4 GiB on x86_64 and below 512 GiB on AArch64, `exit_services` trapping (`trap.exhausted`) when none holds them. It maps the image, the memory the image reserves, its tables and all usable memory below 128 TiB writable at their physical addresses, where code reaches the memory of frames and regions. The soundness condition of `write_cr3` and `write_ttbr0` keeps the image, the memory it reserves and every live frame and region mapped there ([privileged intrinsics](23-low-level.md#privileged-intrinsics)).
- On the machine targets, the **boot area** holds the boot heap and the runtime's tables, then on a bare target the entry root's other tables. On a UEFI target it is memory the image reserves. On a bare target it lies outside the image's loadable segments: before the entry landing fn, the runtime places it at the lowest 64 KiB-aligned address of usable memory outside the image from which it lies wholly in that usable memory below 4 GiB on x86_64 and below 512 GiB on AArch64, leaving out the first MiB on x86_64, the [start information](#start-information) and what it holds, and on `aarch64-bare` the device tree and the ranges its memory reservation block lists ([anchor](#anchor)). When no usable memory holds it, the run writes `nan: runtime unavailable` and a line feed to the serial port and writes the byte 71 to the [exit port](27-program-model.md#descriptors).
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

On the UEFI targets, the [root grant](14-authority.md#root-grants) `boot::Machine` reaches the firmware's services only during the [protocol phase](14-authority.md#protocol-phases) before they exit. It leaves that phase by `exit_services`, continuing as a kernel, or by `enter`, handing the machine to a loaded bare image ([hand-off](#hand-off)).

```nan-api
// module: boot

pub fn Machine.alloc(self) -> mem::Alloc

pub fn Machine.ring0(mut self) -> Option[hw::Ring0]

pub fn Machine.with_services[T: Any, E](mut self, body: fn(services: Services) -> T fails E) -> T fails E

pub fn Machine.exit_services(take self) -> Kernel

pub open view struct Services {
} Services

pub fn Services.write_console(self, text: Str) fails ServiceError

pub fn Services.read_key(self, timeout_ms: u64) -> Option[Key] fails ServiceError

pub fn Services.read_file(self, path: Str, alloc: mem::Alloc) -> Untrusted[mem::Bytes] fails ServiceError

pub fn Services.write_file(self, path: Str, data: Slice[u8]) fails ServiceError

pub fn Services.delete_file(self, path: Str) fails ServiceError

pub fn Services.list_directory(self, path: Str, alloc: mem::Alloc) -> mem::List[DirectoryEntry] fails ServiceError

pub fn Services.boot_device(self, alloc: mem::Alloc) -> BootDevice fails ServiceError

pub fn Services.get_variable(self, name: Str, vendor: Guid, alloc: mem::Alloc) -> Variable fails ServiceError

pub fn Services.set_variable(self, name: Str, vendor: Guid, attributes: VariableAttributes, data: Slice[u8]) fails ServiceError

pub fn Services.measure(self, pcr: u32, event_type: u32, data: Slice[u8], event: Slice[u8]) fails ServiceError

pub fn Services.event_log(self, alloc: mem::Alloc) -> EventLog fails ServiceError

pub fn Services.submit_tpm(self, command: Slice[u8], alloc: mem::Alloc) -> Untrusted[mem::Bytes] fails ServiceError

pub fn Services.random(self, mut buf: SliceMut[u8]) fails ServiceError

pub fn Services.memory_map(self, alloc: mem::Alloc) -> mem::List[MemoryRange] fails ServiceError

pub fn Services.allocate(self, base: u64, byte_len: u64) -> hw::Region fails ServiceError

pub fn Services.free(self, take region: hw::Region) fails ServiceError

pub fn Services.config_table(self, table: ConfigTable, alloc: mem::Alloc) -> Untrusted[mem::Bytes] fails ServiceError

pub fn Services.acpi_table(self, signature: Array[u8, 4], alloc: mem::Alloc) -> Untrusted[mem::Bytes] fails ServiceError

pub open enum ServiceError derives Render {
    NotFound
    DeviceError
    OutOfMemory
    Denied
    Unsupported
    Invalid
} ServiceError

pub closed struct Key derives Copy, Eq {
    pub text: Option[char]
    pub scan: u16
} Key

pub closed struct Guid derives Copy, Eq {
    pub data1: u32
    pub data2: u16
    pub data3: u16
    pub data4: u64
} Guid

pub layout VariableAttributes: u32 le {
    non_volatile: bit 0
    boot_service_access: bit 1
    runtime_access: bit 2
    reserved: rest
} VariableAttributes

pub closed struct Variable {
    pub attributes: VariableAttributes
    pub data: Untrusted[mem::Bytes]
} Variable

pub closed struct EventLog {
    pub bytes: Untrusted[mem::Bytes]
    pub truncated: bool
} EventLog

pub closed enum ConfigTable derives Copy, Eq {
    Acpi
    Smbios
    DeviceTree
} ConfigTable

pub closed struct DirectoryEntry {
    pub name: Untrusted[mem::Bytes]
    pub byte_len: u64
    pub directory: bool
} DirectoryEntry

pub closed struct BootDevice {
    pub file: Untrusted[mem::Bytes]
    pub partition: Option[Partition]
} BootDevice

pub closed struct Partition derives Copy, Eq {
    pub number: u32
    pub start: u64
    pub blocks: u64
    pub signature: PartitionSignature
} Partition

pub closed enum PartitionSignature derives Copy, Eq {
    Gpt(Guid)
    Mbr(u32)
    Unsigned
} PartitionSignature

pub closed struct MemoryRange derives Copy, Eq {
    pub base: u64
    pub byte_len: u64
    pub kind: u32
    pub attributes: u64
} MemoryRange
```

- `m.alloc()` is the root allocator of `boot::Machine`, whose supply is the boot heap; `exit_services` passes it on unchanged as the `boot::Kernel`'s.
- `m.ring0()` and `k.ring0()` share the machine's one `hw::Ring0`: the first call of either, before or after `exit_services`, returns `Some` of it, and every later call `None`.
- `with_services(body)` calls `body` with [`boot::Services`](#kernel-types), a view, so no capability derived from it outlives the call.
- A **path** names a file or directory from the root directory of the volume the image was loaded from, with `/` between components; the empty path names the root directory. A path or variable name holds at most 255 UTF-16 code units. A GUID's fields are the groups of its text form, `data4` the last two.
- `write_console` writes `text` to the firmware's console output, each line feed as a carriage return and a line feed. `read_key(timeout_ms)` waits at most `timeout_ms` milliseconds for a key at the firmware's console input and returns `Some` of its character, when it has one, and its scan code, or `None` when no key arrives.
- `read_file` reads a file. `write_file` makes `data` the whole contents of a file, creating it in its existing directory, and flushes it. `delete_file` deletes a file. `list_directory` returns the entries of a directory other than `.` and `..`, in the firmware's order: each one's name in UTF-8, size and whether it is a directory; it fails with `Invalid` when `path` names a file.
- `boot_device` returns the path of the image's own file, without a leading `/`, and, when the device path of its volume names a hard drive partition, that partition's number, first block, block count and signature: the unique GUID of a GPT partition or the disk signature of an MBR one.
- `get_variable` returns a firmware variable's attributes and data, and `set_variable` writes one, an empty `data` deleting it. A variable whose attributes lack `runtime_access` is reached only before boot services end.
- `measure` extends PCR `pcr` of the TPM with the digest of `data` in each of its active banks and logs an event of type `event_type` whose data is `event`, of at most 1,024 bytes, through the firmware's TCG2 protocol. `event_log` returns the firmware's crypto-agile event log, from its first entry to the end of its last, and whether the firmware truncated it; it fails with `Unsupported` when an entry's digest uses an algorithm other than SHA-1, SHA-256, SHA-384, SHA-512 and SM3-256. `submit_tpm` sends a TPM 2.0 command and returns its response, of at most the largest response the TPM states.
- `random` fills `buf` from the default algorithm of the firmware's RNG protocol.
- `memory_map` returns the firmware's current memory map, a range for each descriptor in its order, `kind` being the UEFI memory type. `allocate` allocates the pages from `base`, a multiple of 4 KiB, that hold `byte_len` bytes, as `EfiLoaderData`, and returns them as a region of whole pages, which `regions()` never gives; another `base` or an empty length fails with `Invalid`. `free` returns a region's pages to the firmware.
- `config_table` copies a table the firmware's configuration table lists: the ACPI 2.0 RSDP, the SMBIOS 3.0 entry point followed by the structure table it addresses in its stated maximum size, or the flattened device tree. `acpi_table` copies the ACPI table with a signature: the DSDT and the FACS through the FADT, any other table the XSDT lists, or the RSDT when the RSDP's revision is 0 or it has no XSDT.
- Each fails with `NotFound` when the firmware returns `EFI_NOT_FOUND` or, for `config_table` and `acpi_table`, has no such table; with `OutOfMemory` when it returns `EFI_OUT_OF_RESOURCES` or a draw through `alloc` is exhausted; with `Denied` for `EFI_ACCESS_DENIED`, `EFI_WRITE_PROTECTED` and `EFI_SECURITY_VIOLATION`; with `Unsupported` for `EFI_UNSUPPORTED` and when the firmware has no protocol the operation calls; with `Invalid` for `EFI_INVALID_PARAMETER`, `EFI_BAD_BUFFER_SIZE` and a path, name or event longer than it takes; and with `DeviceError` on any other error status.
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

```nan
// file: loader/package.nan
//! Kernel loading for a UEFI loader.

package loader {
    language 1
    targets x86_64-uefi
} loader

// file: loader/loader.nan
//! Reads the kernel image under limits.

pub fn kernel_image(services: boot::Services, alloc: mem::Alloc) -> Untrusted[mem::Bytes] fails boot::ServiceError limits notrap, noalloc {
    return try services.read_file("kernel.elf", alloc) // error: limit.allocates
} kernel_image
```

```nan
// file: fwwrite/package.nan
//! Firmware writes from a loader.

package fwwrite {
    language 1
    targets x86_64-uefi
} fwwrite

// file: fwwrite/fwwrite.nan
//! Records a boot attempt on the boot volume.

pub fn record_attempt(services: boot::Services, record: Slice[u8]) fails boot::ServiceError {
    try services.write_file("nanos/boot.rec", data: record)
} record_attempt
```

### Hand-off

```nan-api
// module: boot

pub fn Machine.enter(take self, take image: Image) -> Never

pub closed linear struct Image {
    pub entry: u64
    pub segments: mem::List[hw::Region]
    pub modules: mem::List[ModuleSource]
    pub command_line: mem::Bytes
} Image

pub closed struct ModuleSource {
    pub name: mem::Bytes
    pub contents: Untrusted[mem::Bytes]
} ModuleSource
```

`take m.enter(take image)` hands the machine to a loaded bare image of its architecture and never returns. It is a [trusted primitive](23-low-level.md#trusted-primitives) whose soundness condition is that `image.segments` hold such an image, placed for execution, which its target's boot protocol enters at `image.entry`: the PVH protocol's 32-bit entry on x86_64 and the image's entry on AArch64 ([descriptors](27-program-model.md#descriptors)).

1. In pages below 4 GiB that it allocates from the firmware, it lays out a [start information](#start-information) of version 1 naming the RSDP the configuration table lists and the command line, and listing each module, its contents copied and its name as the entry's command line; on AArch64 it also copies the device tree the configuration table lists. It traps (`trap.exhausted`) when the firmware has no such pages.
2. It ends boot services as `exit_services` does and fills the start information's memory map from the firmware's final memory map: type 1 for conventional memory and the code and data of boot services and of loaders, 3 for ACPI reclaimable memory, 4 for ACPI NVS, 5 for unusable memory and 2 for any other.
3. With maskable interrupts disabled, it enters `image.entry`: on x86_64 in 32-bit protected mode with paging off, `CR4` and `EFER` clear, flat 4 GiB code and data segments and the start information's address in `ebx`; on AArch64 at EL1 with the MMU and caches off, those pages and the segments cleaned from the data caches, the device tree's address in `x0`, zero without one, and the start information's in `x1`.

```nan
// file: chain/package.nan
//! A loader entering a bare kernel.

package chain {
    language 1
    targets x86_64-uefi
    allow trusted in chain
} chain

// file: chain/chain.nan
//! Enters a kernel its caller placed.

pub fn boot_kernel(take m: boot::Machine, take image: boot::Image) -> Never {
    trusted "the image's segments hold an x86_64-bare kernel entered at its PVH entry" { take m.enter(take image) }
} boot_kernel
```

### Start information

```nan-api
// module: boot

pub fn Kernel.modules(mut self) -> mem::List[Module]

pub fn Kernel.command_line(self, alloc: mem::Alloc) -> Untrusted[mem::Bytes]

pub fn Kernel.acpi_table(self, signature: Array[u8, 4], alloc: mem::Alloc) -> Option[Untrusted[mem::Bytes]]

pub fn Kernel.device_tree(self, alloc: mem::Alloc) -> Option[Untrusted[mem::Bytes]]

pub open linear struct Module {
} Module

pub fn Module.name(self, alloc: mem::Alloc) -> Untrusted[mem::Bytes]

pub proj fn Module.contents(self) -> Untrusted[Slice[u8]]

pub fn Module.into_region(take self) -> hw::Region
```

- A bare image's **start information** is a structure in the layout of the PVH boot protocol's naming its modules, command line, RSDP and memory map: on `x86_64-bare` the one the PVH protocol passes, and on `aarch64-bare` the one whose address `x1` holds when it is not zero ([descriptors](27-program-model.md#descriptors)). A UEFI image has none.
- The first call of `k.modules()` returns a module for each entry of the start information's module list, in its order, drawn through `k.alloc()`; later calls, and calls without a start information, return an empty list. A module owns its memory: `name` copies the NUL-terminated text its entry names, `contents` views its bytes, and `into_region` returns its memory as a region.
- `k.command_line(alloc)` copies the start information's command line, or on `aarch64-bare` without one the `bootargs` property of the device tree's `/chosen` node; it is empty when neither gives one.
- `k.acpi_table(signature, alloc)` copies the ACPI table with a signature as `Services.acpi_table` finds it, from the RSDP that the start information names, on `x86_64-bare` otherwise the one in the BIOS area, and on a UEFI target the one the configuration table lists; it is `None` when there is none. `k.device_tree(alloc)` copies the device tree the image entered with on `aarch64-bare`, and is `None` on the other targets.
- Each draw traps on exhaustion (`trap.exhausted`).

## Loading

A kernel of a bare target **exports its load ABI** when its final closure declares a [loaded registry](25-registries.md#loaded-registries). Its final build then also writes its [interface file](30-artefacts.md#interface-files), and a [bundle](30-artefacts.md#bundles) built against that file adds entries to its loaded registries while it runs.

```nan-api
// module: boot

pub fn Kernel.loader(self) -> Option[sys::load::Loader]
```

```nan-api
// module: sys::load

pub open struct Loader {
} Loader

pub fn Loader.prepare(self, bundle: Slice[u8], keys: Slice[core::crypto::PublicKey], alloc: mem::Alloc) -> Prepared fails LoadError

pub fn Loader.prepare_unverified(self, bundle: Slice[u8], alloc: mem::Alloc) -> Prepared fails LoadError

pub fn Loader.reclaim(self) -> usize fails task::Cancelled uses wait

pub open linear struct Prepared {
} Prepared

pub fn Prepared.load(take self) -> Bundle fails LoadError

pub open linear struct Bundle {
} Bundle

pub fn Bundle.domain(self) -> mem::Shared[sys::Abandoner]

pub fn Bundle.replace(mut self, take next: Prepared) fails LoadError

pub fn Bundle.unload(take self)

pub closed enum LoadError derives Copy, Eq, Render {
    Malformed
    Signature
    Interface
    DuplicateKey
    DuplicateUnique
} LoadError
```

- `k.loader()` returns `Some` of a loader when the image exports its load ABI, and `None` otherwise. `sys::load::Loader` is a primitive capability.
- A kernel exporting its load ABI has a **load window**, a range of 512 GiB of addresses outside the [entry root](#physical-memory)'s identity map, from `0xFFFF_8000_0000_0000` on x86_64, which the entry root's top-level entry 256 maps, and from `0xFFFF_0000_0000_0000` on AArch64, which `TTBR1_EL1` maps on each CPU. Every CPU of such a kernel enforces read-only pages for kernel writes and execute-never bits, `CR0.WP` and `EFER.NXE` on x86_64.
- `loader.prepare(bundle, keys, alloc)` places a bundle's image without publishing its entries. It fails with `Malformed` when `bundle` is not a bundle's encoding, with `Signature` when its sections hold no signature that one of `keys` verifies, and with `Interface` when it was built against another interface than the kernel's. It then copies the image into runtime memory, applies its relocations for the image's address in the load window, binds each import to the kernel's export, maps the image's pages into the window, its code readable and executable, its read-only data readable and its writable data readable and writable, never executable, makes the code visible to every CPU's instruction fetch and adds its line tables to those the trap routine reads; it traps (`trap.exhausted`) when runtime memory cannot hold the image. The prepared bundle's bookkeeping is drawn through `alloc`. A store through a mapped page of a bundle's code or read-only data raises a page fault ([interrupt functions](#interrupt-functions)).
- `loader.prepare_unverified(bundle, alloc)` is `prepare` without the signature check. It is a trusted primitive whose soundness condition is that `bundle` is a bundle the toolchain built against the kernel's interface file.
- `take prepared.load()` publishes the bundle's entries: for each loaded registry it contributes to, a new version holding the current version's entries and the bundle's, each of these inserted, in the bundle's order, before the first entry whose key is greater ([loaded registries](25-registries.md#loaded-registries)). It fails with `DuplicateKey` when two entries of a new version have equal keys and with `DuplicateUnique` when they have equal values of a unique field, publishing nothing and retiring the placement.
- Each function field of an entry a bundle adds holds a **gate**: a function value of the kernel's calling the bundle's function while the gate is live, and trapping (`trap.stale-handle`) once it is stale. While it calls the function, the call **pins** the function's code, a pin its domain's [settlement](11-errors-domains.md#settlement) releases when the call never returns. A call through a gate is the call of its function in every other respect; the gate itself uses at most 1 KiB of the stack ([stack limits](10-effects-digests.md#stack-limits)) that the `stack N` of the field's type bounds.
- `bundle.domain()` shares the bundle's **domain**, an abandoner ([domain run](11-errors-domains.md#domain-run)) that runs no domain when the bundle is loaded. A kernel contains the calls it makes into a bundle, and what they leave running or installed, by making them in a domain run of that abandoner.
- `bundle.replace(take next)` publishes in one change versions without the entries of `bundle` and with those of `next`, failing as `load` does and then changing nothing and retiring `next`'s placement. Each gate of `bundle` whose registry, key and field an entry of `next` shares then calls `next`'s function, and every other gate of `bundle` goes stale; `bundle` holds `next` from then on, with the same domain, and retires the placement it held.
- `take bundle.unload()` triggers the bundle's domain, then publishes versions without the bundle's entries; every gate of the bundle goes stale and its placement is retired.
- Loads, replacements, unloads and reclamations take one lock of the kernel's, each publishing all its versions before the next starts.
- A bundle's code runs on the kernel's runtime: its tasks, cells, capabilities and domains are the kernel's.
- **Retired** memory is reclaimed by `loader.reclaim()`: each version a publication replaced once no [read](25-registries.md#loaded-registries) holds it, and each retired placement once no older retired version is held and no call pins its code. Reclaiming a placement unlinks its line tables, clears its window entries and the window tables they leave empty, invalidates every CPU's translations as `hw::Ring0.invalidate_everywhere(hw::TlbScope.All)` does ([cross-CPU operations](#cross-cpu-operations)), then frees its runtime memory. `reclaim` gives the number of retired placements it leaves, and fails at once, keeping what it had cleared for the next call, when cancelled as its invalidation starts. A gate's function value and the records a gate reads, a few words per function field and load, are never reclaimed, so a value holding a gate stays valid after its bundle is reclaimed.
- No value returned from a gate's call refers to its bundle's memory, since the types crossing a gate are [boundary types](25-registries.md#loaded-registries); what a bundle's code leaves running or installed beyond a call belongs to the domain the call runs in.

## Floating point

A function, closure or const whose own operations or calls of std functions have the float use [digest fact](10-effects-digests.md#digests) is an error (`kernel.float-use`) wherever the environments of the machine targets judge reachable code ([environments](27-program-model.md#environments)), reported at each such operation or call.

Code lowered for a machine target uses none of the **extended state**: on x86_64 the x87 and SSE registers and the other state components `XCR0` enables, and on AArch64 the floating-point and Advanced SIMD registers with FPCR and FPSR. A thread **owns** extended state while it runs the body of `hw::ExtendedState.with` or a user thread through `hw::Ring0.run_user` given one ([user mode](#user-mode)), and owns none otherwise.

```nan-api
// module: hw

pub open linear struct ExtendedState {
} ExtendedState

pub fn ExtendedState.of(take frame: PhysFrame[4_096]) -> ExtendedState

pub fn ExtendedState.vector(self, index: u8) -> core::simd::U64x2
    requires index < 16

pub fn ExtendedState.set_vector(mut self, index: u8, value: core::simd::U64x2)
    requires index < 16

pub fn ExtendedState.with[T: Any, E](mut self, body: fn() -> T fails E uses wait) -> T fails E uses wait

pub fn ExtendedState.into_frame(take self) -> PhysFrame[4_096]
```

- `hw::ExtendedState.of(take frame)` makes the frame the save area of extended state in its initial state, every vector register zero, and returns it; `take state.into_frame()` returns the frame. On x86_64 the area holds the state in the form that the first of `xsaves`, `xsavec`, `xsaveopt` and `xsave` the CPU has writes where the anchor enabled XSAVE, and in that of `fxsave` otherwise.
- `state.vector(index)` gives the 128 bits of vector register `index` that `state` holds, `xmm` on x86_64 and `v` on AArch64, and `set_vector(index, value)` sets them, leaving the rest of the state unchanged.
- `state.with(body)` runs `body` with the running thread owning `state`, then returns what `body` returns or fails as it fails. The thread's registers start as `state` holds them, `state` holds them as `body` leaves them, and the thread owns again the extended state it owned before the call, as it held it then.
- Each CPU runs its kernel executor with the extended state disabled, `CR0.TS` set on x86_64 and `CPACR_EL1.FPEN` clear on AArch64, while it runs a thread owning none, so that an instruction using it raises an exception ([interrupt functions](#interrupt-functions)). The executor restores a thread's extended state into the CPU, enabling it, before the thread runs, and saves it once the thread switches away. Before a thread owning none runs on a CPU holding another thread's extended state, it puts that state in its initial state, then disables it. A thread owning none pays no save or restore, and no thread runs while its CPU holds another thread's extended state.
- No seam saves extended state: interrupt fns use none, and a preempted thread switches away as any other. The entry function starts with the extended state enabled ([anchor](#anchor)).
- A thread's saved state holds the state components the `XCR0` of the CPU that saved it enables, and `write_xcr0` disables none ([privileged intrinsics](23-low-level.md#privileged-intrinsics)). Restoring it on a CPU whose `XCR0` lacks one of them raises a general-protection exception in the executor ([interrupt functions](#interrupt-functions)).

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

```nan
// file: kernel/package.nan
//! Load totals for a bare kernel.

package kernel {
    language 1
    targets x86_64-bare
} kernel

// file: kernel/kernel.nan
//! Totals load figures.

pub fn total(levels: core::simd::F32x4) -> f32 {
    return levels.sum() // error: kernel.float-use
} total
```

```nan
// file: vectors/package.nan
//! Vector registers a bare kernel's task keeps across switches.

package vectors {
    language 1
    targets x86_64-bare
} vectors

// file: vectors/vectors.nan
//! Pauses while owning extended state whose first vector register holds a mark.

pub fn pause_owning(take frame: hw::PhysFrame[4_096], clock: sys::Clock) -> hw::PhysFrame[4_096] fails task::Cancelled uses wait {
    var state = hw::ExtendedState.of(take frame)
    mut state.set_vector(0, value: core::simd::U64x2.splat(7))
    discard attempt await mut state.with(fn() fails task::Cancelled uses wait {
        try await clock.sleep_ns(1_000)
    })
    return take state.into_frame()
} pause_owning
```

## Kernel halts

A **kernel halt** ends a kernel domain ([domains](11-errors-domains.md#domains)). The kernel halts on a trap in the kernel domain, and in these other cases, each named by its cause:

| Cause | Event | Located at |
| --- | --- | --- |
| `first-task-ended` | the first kernel task returns or fails | the `anchor` call |
| `lock-reacquired` | a CPU calls `with` on a spin lock or per-CPU value it is already inside, or reaches its run-queue lock or the cell state of a draw or return while it already holds it | that call, or the interrupted instruction |
| `unbound-vector` | a vector fires with no binding for its origin | the interrupted instruction |
| `abort-exception` | an interrupt fn for an abort vector returns | the interrupted instruction |
| `unabandonable` | an interrupt fn of an exception vector gives `hw::IrqExit.Abandon` for code that may not trap ([interrupt functions](#interrupt-functions)) | the interrupted instruction |

The first CPU to begin a halt performs these steps; a CPU beginning a halt after it stops as step 1 stops the others.

1. It sends every other running CPU an interrupt that disabled maskable interrupts do not hold back, a non-maskable interrupt on x86_64 and a fast interrupt request on AArch64, whose seam then stops that CPU with maskable interrupts disabled instead of calling an interrupt fn, and waits until each has stopped.
2. It programs the UART at the serial port the descriptor names to 8 data bits, no parity and 1 stop bit, at 115200 baud on x86_64 and keeping its baud rate on AArch64, where a PL011 without a divisor gets 115200 baud from a 24 MHz clock, then writes the report there, by polling and without allocating; an AArch64 machine without a UART gets no report. For a trap this is the [trap report](11-errors-domains.md#trap-reports) without a replay line. For another cause it has the same form, with `halt` and the cause in place of the trap kind, the location the table gives, and, for `unbound-vector`, the operand line `vector = ` and the vector number.
3. It writes the byte 70 to the [exit port](27-program-model.md#descriptors), then stops with maskable interrupts disabled. No deferred call or other user code runs.

When the first task started by the `anchor` call on line 24 of `kernel/kernel.nan` in [Anchor](#anchor) ends, the kernel halts with this report:

```text
halt first-task-ended at kernel/kernel.nan:24 in kernel::main
    take k.anchor(take state, run: fn[take wake](exec: task::Executor) fails task::Cancelled uses wait limits stack 16_384 {
```

## Kernel types

The types of this table are [base types](05-properties.md#structural-properties) with these properties. The primitive capabilities among them are `hw::Region`, `hw::PhysFrame[S]`, `hw::UserFrame`, `hw::IrqShared[T]`, `boot::Services` and `sys::load::Loader`.

| Type | linear | view | Copy | Send | Inert | AnyBits |
| --- | --- | --- | --- | --- | --- | --- |
| `hw::IrqContext` | yes | no | no | no | no | no |
| `hw::TrapFrame[V]` | no | no | no | no | no | no |
| `hw::Region`, `hw::PhysFrame[S]`, `hw::UserFrame`, `hw::IrqShared[T]`, `hw::ExtendedState` | yes | no | no | yes | no | no |
| `hw::Signal[T]`, `hw::SpinLock[T]`, `hw::PerCpu[T]`, `sys::load::Loader` | no | no | no | yes | no | no |
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
