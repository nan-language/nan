# Targets and program model

This chapter defines targets and the dependencies they allow, target descriptors and their environments, the entry function and how a program runs, programs and libraries, final builds, the support routines with the host I/O backend, and images.

## Targets

A **target** is a platform a package is checked and built for: `x86_64-host`, `x86_64-uefi`, `x86_64-bare`, `aarch64-host`, `aarch64-uefi`, `aarch64-bare` or `any`. The `targets` [directive](03-packages-modules.md#manifest) lists a package's declared targets; `x86_64-nanos` and `aarch64-nanos` are reserved there.

- The targets other than `any` are the **concrete targets**, each fixed by its [descriptor](#descriptors). A package declaring `any` is valid on every concrete target and is checked once, under the `any` descriptor.
- A concrete target's **architecture** is x86_64 or AArch64, as its name starts with `x86_64` or `aarch64`. `x86_64-host` and `aarch64-host` are the **host targets**, `x86_64-uefi` and `aarch64-uefi` the **UEFI targets**, and `x86_64-bare` and `aarch64-bare` the **bare targets**; the UEFI and bare targets are the **machine targets**.
- A package **covers** a target when it declares that target or `any`. A `dep` directive naming a package that does not cover every target the declaring package declares, `any` included, is an error (`target.dependency-uncovered`).

```nan
// file: ring/package.nan
//! Drop counters for every target.

package ring {
    language 1
    targets any
} ring

// file: ring/ring.nan
//! Counts dropped items.

pub closed struct Drops {
    pub count: u32
} Drops

pub fn Drops.add(mut self) {
    self.count = self.count.saturating_add(1)
} Drops.add

// file: kernel/package.nan
//! A bare kernel counting dropped bytes.

package kernel {
    language 1
    targets x86_64-bare
    dep ring "../ring"
} kernel

// file: kernel/kernel.nan
//! Counts drops at boot.

pub fn drop_twice() -> ring::Drops {
    var drops = ring::Drops(count: 0)
    mut drops.add()
    mut drops.add()
    return drops
} drop_twice
```

```nan
// file: hostinfo/package.nan
//! Facts about the host process.

package hostinfo {
    language 1
    targets x86_64-host
} hostinfo

// file: hostinfo/hostinfo.nan
//! The size of a host page.

pub const PAGE_BYTES: usize = 4_096

// file: kernel/package.nan
//! A bare kernel.

package kernel {
    language 1
    targets x86_64-bare
    dep hostinfo "../hostinfo" // error: target.dependency-uncovered
} kernel

// file: kernel/kernel.nan
//! Page arithmetic.

pub fn pages(bytes: usize) -> usize {
    return bytes / hostinfo::PAGE_BYTES
} pages
```

## Descriptors

A **descriptor** is the fixed definition of a target. The language version fixes every descriptor; no option, file or variable changes one. A concrete target's descriptor fixes its [environment](#environments), [root grant](14-authority.md#root-grants), [platform package](28-stdlib.md#platform-packages) (whose entry [landing fn](23-low-level.md#landing-functions) mints the root grant), [calling conventions](23-low-level.md#calling-conventions), [privileged intrinsics](23-low-level.md#privileged-intrinsics), [compiler stubs](23-low-level.md#compiler-stubs), codegen, [image](#images) format, [support routine](#runtime-support) names and these values:

| Field | `x86_64-host` | `x86_64-uefi` | `x86_64-bare` |
| --- | --- | --- | --- |
| Platform | Linux 6.1 or later on x86_64, with glibc 2.35 or later | UEFI firmware with boot services, on x86_64 | an x86_64 machine started by the PVH boot protocol |
| Default task stack ([task stacks](17-suspension-io.md#task-stacks)) | 64 KiB | none | none |
| Entry task stack ([task stacks](17-suspension-io.md#task-stacks)) | 64 MiB | none | none |
| Stub call bound | 256 bytes | 256 bytes | 256 bytes |
| Seam frame | none | 4 KiB | 4 KiB |
| Boot heap ([anchor](24-kernel.md#anchor)) | none | 16 MiB | 16 MiB |
| Serial port | none | the 16550 UART at I/O port `0x3F8` | the 16550 UART at I/O port `0x3F8` |
| Console port | none | the 16550 UART at I/O port `0x2F8` | the 16550 UART at I/O port `0x2F8` |
| Exit port | none | I/O port `0xF4` | I/O port `0xF4` |
| Interrupt controller | none | each CPU's local APIC at `0xFEE0_0000` and the I/O APIC at `0xFEC0_0000` | each CPU's local APIC at `0xFEE0_0000` and the I/O APIC at `0xFEC0_0000` |
| Timer | none | each CPU's local APIC timer, firing vector `0x20` one-shot ([kernel time](24-kernel.md#kernel-time)) | each CPU's local APIC timer, firing vector `0x20` one-shot ([kernel time](24-kernel.md#kernel-time)) |
| Clock | none | the time-stamp counter, its rate measured against 11,932 periods of the 1,193,182 Hz legacy PIT's channel 2 | the time-stamp counter, its rate measured against 11,932 periods of the 1,193,182 Hz legacy PIT's channel 2 |
| Wall clock | none | the firmware's `GetTime` | the CMOS clock, its year read in the century from 2000 |
| Emulator | none | QEMU with OVMF firmware | QEMU |

| Field | `aarch64-host` | `aarch64-uefi` | `aarch64-bare` |
| --- | --- | --- | --- |
| Platform | Linux 6.1 or later on AArch64, with glibc 2.35 or later | UEFI firmware with boot services, on AArch64 at EL1, with PSCI reached by `hvc` | an AArch64 machine whose memory starts at `0x4000_0000`, with PSCI reached by `hvc`, entering the image at EL1 with a flattened device tree at the start of memory |
| Default task stack ([task stacks](17-suspension-io.md#task-stacks)) | 64 KiB | none | none |
| Entry task stack ([task stacks](17-suspension-io.md#task-stacks)) | 64 MiB | none | none |
| Stub call bound | 256 bytes | 256 bytes | 256 bytes |
| Seam frame | none | 4 KiB | 4 KiB |
| Boot heap ([anchor](24-kernel.md#anchor)) | none | 16 MiB | 16 MiB |
| Serial port | none | the PL011 UART at `0x0900_0000` | the PL011 UART at `0x0900_0000` |
| Console port | none | the PL011 UART at `0x0904_0000` | the PL011 UART at `0x0904_0000` |
| Exit port | none | the semihosting exit call | the semihosting exit call |
| Interrupt controller | none | a GICv3, its distributor at `0x0800_0000` and its redistributors from `0x080A_0000` | a GICv3, its distributor at `0x0800_0000` and its redistributors from `0x080A_0000` |
| Timer | none | each CPU's virtual timer, firing vector 27 one-shot ([kernel time](24-kernel.md#kernel-time)) | each CPU's virtual timer, firing vector 27 one-shot ([kernel time](24-kernel.md#kernel-time)) |
| Clock | none | the virtual counter, at the rate `CNTFRQ_EL0` gives | the virtual counter, at the rate `CNTFRQ_EL0` gives |
| Wall clock | none | the firmware's `GetTime` | the PL031 at `0x0901_0000` |
| Emulator | none | QEMU with AAVMF firmware | QEMU |

- Codegen uses, on x86_64, the x86-64 base instruction set with SSE2 and, on AArch64, the A64 instruction set of Armv8.0-A with floating point and Advanced SIMD, and no later extension. On the machine targets, lowered code never stores below the stack pointer. The codegen field has exactly this one value per architecture; other values are reserved.
- A call of a compiler stub or a support routine uses at most the stub call bound of its caller's stack, switching to a stack the image reserves when it needs more. A **seam frame** is the most stack a vector seam uses for the state it saves.
- The emulator of the x86_64 machine targets is `qemu-system-x86_64` emulating a `q35` machine with an Intel IOMMU remapping interrupts, 4 processors, 512 MiB of memory, a `virtio-rng-pci` device, an `isa-debug-exit` device of width 1 byte at I/O port `0xF4`, a TPM 2.0 with its CRB interface at `0xFED4_0000`, and the devices every machine has. Its processors are those of the host's CPU model with invariant TSC under KVM when the machine running the toolchain is x86_64 and lets it read and write `/dev/kvm`, and those of QEMU's `max` model under TCG otherwise. OVMF loads a `x86_64-uefi` image from a FAT volume holding it as `/EFI/BOOT/BOOTX64.EFI`; QEMU boots a `x86_64-bare` image directly by the PVH boot protocol.
- The emulator of the AArch64 machine targets is `qemu-system-aarch64` emulating a `virt` machine with a GICv3 and its ITS, an SMMUv3 translating its PCI devices' accesses, 4 Cortex-A72 processors under TCG, 512 MiB of memory, a `virtio-rng-device` on its virtio-mmio transports, semihosting, a TPM 2.0 with its TIS interface on its platform bus, and the devices every machine has. AAVMF loads an `aarch64-uefi` image from a FAT volume holding it as `/EFI/BOOT/BOOTAA64.EFI`; QEMU boots an `aarch64-bare` image by loading its segments and entering it with the device tree it generates at `0x4000_0000`.
- Every machine also has an `i6300esb` watchdog, which resets it when it expires, a `virtio-blk-pci` disk of 256 MiB, a `virtio-net-pci` network device on QEMU's user network, no display, and its serial and console ports connected to the toolchain. Each virtio PCI device is a modern device whose accesses the IOMMU translates, so a driver accepts `VIRTIO_F_ACCESS_PLATFORM`. `swtpm` emulates the TPM. A UEFI image's volume is the first boot device, and the firmware keeps its variables in a flash beside its code. A reset ends the emulator, unless `nan run --keep-resets` keeps the machine running ([commands](33-toolchain.md#commands)).
- Under `nan run` the volume of a UEFI target's image also holds, at `/` followed by its file name, each other image file in the workspace's `.nan/images/` directories of the machine targets of its architecture ([commands](33-toolchain.md#commands)).
- On the emulator, writing a byte v to the **exit port** ends the emulator with exit status (2v + 1) mod 256; on AArch64 the write is a semihosting `SYS_EXIT` call with reason `ADP_Stopped_ApplicationExit` and subcode 2v + 1. On other machines of the platform, writing to it has no effect on x86_64 and stops the CPU with maskable interrupts disabled on AArch64.
- On `x86_64-uefi` and `x86_64-bare`, the vectors of [`hw::vectors`](25-registries.md#platform-registries) have these classes: `Trap` for 0 to 7, 16, 18 to 20 and 28; `TrapWithCode` for 8, 10 to 13, 17, 21, 29 and 30; `PageFault` for 14; `Irq` for 32 to 255; none for 9, 15, 22 to 27 and 31. Vectors 0 to 31 are the exception vectors and 32 to 255 the interrupt vectors. Vectors 2, 8 and 18 need a nonzero `ist`.
- On `aarch64-uefi` and `aarch64-bare`, vector n from 16 to 255 is interrupt n of the interrupt controller, of class `Irq`, and vectors 0 to 15 are the exception vectors: vector 0, a synchronous exception other than an instruction or data abort, and vector 2, an SError interrupt, have class `TrapWithCode`; vector 1, an instruction or data abort, has class `PageFault`; vectors 3 to 15 have none. Vectors 0 to 15 are the exception vectors and 16 to 255 the interrupt vectors. Every vector's seam runs on its CPU's interrupt stack of `ist` 0, the only `ist` a vector takes.
- The `any` descriptor fixes only its environment, which restricts nothing.

```nan
// file: kernel/package.nan
//! Keyboard interrupts for a bare kernel.

package kernel {
    language 1
    targets x86_64-bare
} kernel

// file: kernel/kernel.nan
//! Binds the keyboard vector.

interrupt fn on_key(take irq: hw::IrqContext) -> hw::IrqExit limits stack 1_024 {
    take irq.complete()
    return hw::IrqExit.Resume
} on_key

contribute hw::vectors keyboard = hw::Vector(vector: 0x21, ist: 0, handler: hw::VectorHandler.Irq(on_key), user: None)
```

```nan
// file: kernel/package.nan
//! Double faults for a bare kernel.

package kernel {
    language 1
    targets x86_64-bare
} kernel

// file: kernel/kernel.nan
//! Binds the double-fault vector on the current stack.

interrupt fn on_double_fault(mut frame: hw::TrapFrame[hw::ErrorCode]) -> hw::IrqExit limits stack 1_024 {
    return hw::IrqExit.Resume
} on_double_fault

contribute hw::vectors double_fault = hw::Vector(vector: 8, ist: 0, handler: hw::VectorHandler.TrapWithCode(on_double_fault), user: None) // error: registry.platform-entry
```

```nan
// file: kernel/package.nan
//! Instruction and data aborts for an AArch64 bare kernel.

package kernel {
    language 1
    targets aarch64-bare
} kernel

// file: kernel/kernel.nan
//! Binds vector 1, instruction and data aborts, whose frame holds the syndrome and the faulting address.

interrupt fn on_abort(mut frame: hw::TrapFrame[hw::PageFault]) -> hw::IrqExit limits stack 1_024 {
    return hw::IrqExit.Resume
} on_abort

contribute hw::vectors abort = hw::Vector(vector: 1, ist: 0, handler: hw::VectorHandler.PageFault(on_abort), user: None)
```

```nan
// file: kernel/package.nan
//! Instruction and data aborts for an AArch64 bare kernel.

package kernel {
    language 1
    targets aarch64-bare
} kernel

// file: kernel/kernel.nan
//! Binds the abort handler to the x86_64 page-fault vector.

interrupt fn on_abort(mut frame: hw::TrapFrame[hw::PageFault]) -> hw::IrqExit limits stack 1_024 {
    return hw::IrqExit.Resume
} on_abort

contribute hw::vectors abort = hw::Vector(vector: 14, ist: 0, handler: hw::VectorHandler.PageFault(on_abort), user: None) // error: registry.platform-entry
```

```nan
// file: kernel/package.nan
//! Instruction and data aborts for an AArch64 bare kernel.

package kernel {
    language 1
    targets aarch64-bare
} kernel

// file: kernel/kernel.nan
//! Asks an interrupt stack of its own for the abort handler.

interrupt fn on_abort(mut frame: hw::TrapFrame[hw::PageFault]) -> hw::IrqExit limits stack 1_024 {
    return hw::IrqExit.Resume
} on_abort

contribute hw::vectors abort = hw::Vector(vector: 1, ist: 1, handler: hw::VectorHandler.PageFault(on_abort), user: None) // error: registry.platform-entry
```

### Environments

An **environment** is the set of restrictions a target places on programs: the rules that name the target, such as [floating point](24-kernel.md#floating-point) on the machine targets. An environment only rejects programs: a program valid on several targets has the same meaning on each, and every type, operation, trap and rendering behaves alike on all of them.

A rule judging reachable code judges all code of a package declaring its target, and code of a package declaring `any` only within the reach of each [final build](#final-builds) for that target.

## Entry

The `entry main` directive names the **entry function**: the function item `main` of the root module, which receives the root grant. A directive naming another identifier, and a root module without a function item `main`, are errors (`target.entry-missing`).

| Target | Head |
| --- | --- |
| `x86_64-host`, `aarch64-host` | `fn main(take P: host::World) fails E uses wait` or `fn main(take P: host::World) -> u8 fails E uses wait` |
| `x86_64-uefi`, `aarch64-uefi` | `fn main(take P: boot::Machine) -> Never` |
| `x86_64-bare`, `aarch64-bare` | `fn main(take P: boot::Kernel) -> Never` |

The head is one of its target's forms, P being any parameter name and E either an enum implementing `Render` or `task::Cancelled`; it may add a `limits` clause and a clause region. Any other head, including one with a visibility, a modifier, a generic parameter or another parameter, is an error (`target.entry-head`), whose diagnostic quotes the expected forms. Whether the body may fail or wait is judged by [heads](02-items.md#heads); `target.entry-head` judges only the head's shape.

The entry function of a bare target starts kernel tasks only by ending in `take k.anchor(take state, run: f)` ([anchor](24-kernel.md#anchor)); one whose body diverges otherwise runs no task.

```nan
// file: status/package.nan
//! Reports a fixed exit status.

package status {
    language 1
    targets x86_64-host
    entry main
} status

// file: status/status.nan
//! Entry of the status program.

fn main(take world: host::World) -> u8 fails task::Cancelled uses wait {
    world.log.info("exiting", value: 3u8)
    return 3
} main
```

```nan
// file: status/package.nan
//! Logs a start line.

package status {
    language 1
    targets x86_64-host
    entry main
} status

// file: status/status.nan
//! Entry of the status program.

fn main(take world: host::World) fails task::Cancelled { // error: target.entry-head
    world.log.info("starting", value: 1u32)
} main
```

### Host runs

A program run on a host target is one Linux process:

1. glibc's start-up code calls the program entry seam, whose symbol is `main`. The seam calls the host entry landing fn, which mints `host::World`.
2. The executor runs tasks on one thread per processor in the process's affinity set at start. The seam starts the entry function, given the `host::World`, as the **entry task**: the first task of the process [domain](11-errors-domains.md#domains).
3. The process exits when the entry task ends, with the status of the first row of this table matching the outcome.

| Outcome | Exit status |
| --- | --- |
| the executor threads, the thread reading termination requests, their io_uring instances or the entry task's stack cannot be created | 71, after writing `nan: runtime unavailable` and a line feed to standard error |
| the entry task traps | 70, after writing the [trap report](11-errors-domains.md#trap-reports) to standard error |
| the entry task returns or fails after a termination request with signal number n | 128 + n |
| it fails with `e` | 1, after writing `error: `, the rendering of `e` within a render budget of 4,096 bytes, and a line feed to standard error; a trap while rendering `e` is a trap of the entry task, with status 70 |
| it returns, `Unit` form | 0 |
| it returns `s`, `u8` form | `s` |

- A **termination request** is the delivery of `SIGINT`, `SIGTERM` or `SIGHUP` to the process. The first one is a cancelling construct ([origin and absorption](11-errors-domains.md#origin-and-absorption)) whose region is the entry task: it cancels that task with [acknowledged cancellation](17-suspension-io.md#acknowledged-cancellation) and absorbs the cancellation. A later one ends the process at once with status 128 + n.
- The runtime ignores `SIGPIPE`.

### Machine runs

A program run on a machine target is one boot of one machine. The program entry seam switches to the **boot stack**, which the image reserves, on a bare target installs the [entry root](24-kernel.md#physical-memory), places the [boot area](24-kernel.md#physical-memory), extends the entry root over usable memory above 4 GiB and, on `x86_64-bare`, loads the boot CPU's [CPU tables](24-kernel.md#anchor), calls the descriptor's entry landing fn, and calls the entry function with the root grant that landing fn returns. The boot stack holds the larger of the two functions' `stack N` limits plus one seam frame. The entry function never returns; [Kernel execution model](24-kernel.md) defines the run from there.

## Programs and libraries

A **program** is a package with an `entry` directive; a **library** is a package without one. Std packages are libraries.

- A program declares exactly one concrete target. Any other `targets` directive in a program is an error (`target.program-targets`), reported on the `entry` directive; the package is then checked as a library.
- A `dep` directive naming a program is an error (`target.dep-program`).

```nan
// file: hello/package.nan
//! Greets on the host log.

package hello {
    language 1
    targets x86_64-bare, x86_64-host
    entry main // error: target.program-targets
} hello

// file: hello/hello.nan
//! Entry of the greeting program.

fn main(take world: host::World) fails task::Cancelled uses wait {
    world.log.info("answer", value: 42u32)
} main
```

```nan
// file: hello/package.nan
//! Greets on the host log.

package hello {
    language 1
    targets x86_64-hots // error: manifest.unknown-target
    entry main
} hello

// file: hello/hello.nan
//! Entry of the greeting program.

fn main(take world: host::World) fails task::Cancelled uses wait {
    world.log.info("answer", value: 42u32)
} main
```

```nan
// file: hello/package.nan
//! Greets on the host log.

package hello {
    language 1
    targets x86_64-host
    entry main
} hello

// file: hello/hello.nan
//! Entry of the greeting program.

pub const ANSWER: u32 = 42

fn main(take world: host::World) fails task::Cancelled uses wait {
    world.log.info("answer", value: ANSWER)
} main

// file: echo/package.nan
//! Repeats the greeting's answer.

package echo {
    language 1
    targets x86_64-host
    dep hello "../hello" // error: target.dep-program
} echo

// file: echo/echo.nan
//! The doubled answer.

pub fn doubled() -> u32 {
    return hello::ANSWER * 2
} doubled
```

## Final builds

The **dependency closure** of a package is that package together with every package it depends on, through `dep` directives and the std aliases its paths use, transitively. A **final-link root** is a program, the package that `nan build` and `nan run` turn into an image; it admits [contributions](25-registries.md#contributors) and approves [widenings](14-authority.md#authority-summaries) for its final closure. Its **final closure** is its dependency closure together with that of the platform package its descriptor names, and the build producing its image is its **final build**.

The **reach** of a final build is the smallest set of items holding the entry function, the descriptor's entry landing fn and every admitted contribution to a platform registry, and, for each item it holds, every item that item names, the impl lines and derived methods of each type it names, and the admitted contributions to each registry it names.

A final build proceeds in this order:

1. It checks every package of the final closure, a package declaring `any` under `any` and every other package for the root's target, reusing checked NIR as [Artefacts](30-artefacts.md#reuse) defines.
2. It computes the reach and applies to it the rules of its target's environment that judge reachable code.
3. It applies every other rule stated for final builds, such as those of [registries](25-registries.md#tables), [stack limits](10-effects-digests.md#stack-limits) and the [closure summary](14-authority.md#authority-summaries).
4. It [lowers](30-artefacts.md#lowering) the reach and writes the image.

Each step runs only when the steps before it reported no error, and a final build reporting an error writes no image; incompleteness stops nothing.

```nan
// file: stats/package.nan
//! Ratios for every target.

package stats {
    language 1
    targets any
} stats

// file: stats/stats.nan
//! Integer and float ratios.

pub fn percent_of(part: u32, whole: u32) -> u32
    requires part <= whole and whole > 0
{
    return u32.fit(u64.widen(part) * 100 / u64.widen(whole))
} percent_of

pub fn fraction(part: u32, whole: u32) -> f64 {
    return f64.widen(part) / f64.widen(whole)
} fraction

// file: kernel/package.nan
//! A bare kernel counting from a ratio.

package kernel {
    language 1
    targets x86_64-bare
    entry main
    dep stats "../stats"
} kernel

// file: kernel/kernel.nan
//! Entry of the kernel; it never reaches the float ratio.

fn main(take k: boot::Kernel) -> Never limits stack 16_384 {
    var ticks: u32 = stats::percent_of(1, whole: 4)
    loop {
        ticks = ticks.wrapping_add(1)
    }
} main
```

## Runtime support

No mandatory runtime exists. Before the entry landing fn, only the program entry seam runs, preceded on the host targets by glibc's start-up code. Lowered code has no writable static data: consts, literals and registry tables are read-only data of the image, never initialised at run time. An image holds exactly the support routines that its lowered code, compiler stubs and seams call.

A **support routine** is one of these fixed routines of the implementation, each with the name its descriptor gives it:

| Name | Does | Targets |
| --- | --- | --- |
| `stack_switch` | creates, switches and frees task stacks, and runs each executor's choice of ready tasks | every concrete target |
| `trap` | stops a trapping domain's tasks, writes the trap report and ends the domain as its kind gives; on the host targets it receives a fault in a stack's guard region as `SIGSEGV` on an alternate signal stack and reports `trap.stack-overflow` | every concrete target |
| `settle` | performs [settlement](11-errors-domains.md#settlement) | the host targets |
| `cap_table` | mints capabilities, revokes, keeps cells, chunk draws and the outbox table, and dispatches each target-dependent operation to the platform function its table entry records ([platform packages](28-stdlib.md#platform-packages)) | every concrete target |
| `mem_op` | copies, fills and compares byte ranges for lowering | every concrete target |
| `replay` | runs the seeded scheduler and records [replay tokens](22-testing.md#replay), in test runs only | every concrete target |
| `anchor` | installs and reaches the [anchor](24-kernel.md#anchor) | the machine targets |

Each function of a routine is named by the routine's name, `_` and a suffix that belongs to the ABI inside the final closure.

The runtime turns on the data-independent timing mode that [Secret values](15-untrusted.md#constant-time-lowering) rely on where a program can: on the machine targets, on each CPU as it starts, DOITM on x86_64 when `IA32_ARCH_CAPABILITIES` enumerates it and `PSTATE.DIT` on AArch64 when `ID_AA64PFR0_EL1` does; on `aarch64-host`, `PSTATE.DIT` for every thread of the process before the entry landing fn, when the kernel reports `HWCAP_DIT`. On `x86_64-host` the mode is the kernel's, which a user process cannot set.

### Host I/O backend

On the host targets every waiting primitive of the platform package performs its operation through io_uring:

- Each executor thread owns one io_uring instance. A primitive submits its operation as submission queue entries on the ring of the thread running its task, and suspends the task until the matching completion queue entries arrive on that ring.
- Cancelling an in-flight operation submits `IORING_OP_ASYNC_CANCEL` for it on the ring holding it; the operation's own completion entry, whatever its result, is the acknowledgement.
- Sleeps and deadlines are `IORING_OP_TIMEOUT` operations on the monotonic clock.
- Every thread blocks the termination request signals. A thread of the runtime that runs no task reads them from a `signalfd` and passes the first to an executor thread's ring with `IORING_OP_MSG_RING`, so a later one ends the process whatever the executor threads run.
- An executor thread with no ready task waits in `io_uring_enter` for a completion; making a task ready wakes such a thread with `IORING_OP_MSG_RING`.

## Images

An **image** is the output of a final build:

- On a host target, a position-independent ELF64 executable for Linux on the target's architecture, dynamically linked against `libc.so.6` alone and named after the package. The target's **linker** links it from the objects lowering produced: `cc` from the `PATH` when the machine running the toolchain has the target's architecture, and otherwise `x86_64-linux-gnu-gcc` for `x86_64-host` and `aarch64-linux-gnu-gcc` for `aarch64-host`, from the `PATH`. It carries the [host ELF section](30-artefacts.md#host-elf-section). A failing linker run is an error (`target.link-failed`), reported on the root's `entry` directive and quoting the linker's output in an [envelope](33-toolchain.md#envelopes).
- On `x86_64-uefi`, a PE32+ image for machine type `0x8664`, subsystem EFI application, with base relocations, named after the package with `.efi`. The firmware calls its entry point, the program entry seam, with the image handle and system table under `"win64"`, and the seam passes both to the entry landing fn.
- On `aarch64-uefi`, the same with machine type `0xAA64`, its entry point called under `"aapcs64"`.
- On `x86_64-bare`, a statically linked ELF64 executable with no interpreter or dynamic segment, its segments loaded at their physical addresses from `0x0010_0000`, named after the package with `.elf`. A `XEN_ELFNOTE_PHYS32_ENTRY` note names the program entry seam, which a PVH loader enters in 32-bit protected mode with paging disabled and `ebx` holding the physical address of the start information. The seam maps all physical memory the start information lists at its physical addresses, in page tables the image reserves, enables long mode and passes the `ebx` value to the entry landing fn.
- On `aarch64-bare`, a statically linked ELF64 executable for AArch64 with no interpreter or dynamic segment, its segments loaded at their physical addresses from `0x4020_0000`, named after the package with `.elf`. Its entry point is the program entry seam, which the platform enters at EL1 with the MMU off. The seam enables floating point, installs the entry root, turns the MMU and caches on and passes the device tree's address, `0x4000_0000`, to the entry landing fn.

The implementation's own image writer builds the images of the machine targets.

Every image keeps frame pointers in every function, stubs and support routines included, and holds line tables mapping each instruction of lowered code to its source path, line and item. The trap routine reads both to write [trap reports](11-errors-domains.md#trap-reports); their encoding belongs to the ABI inside the final closure, which is unspecified.

## Diagnostics

| Code | Class | Rule |
| --- | --- | --- |
| `target.dep-program` | error | [Programs and libraries](#programs-and-libraries) |
| `target.dependency-uncovered` | error | [Targets](#targets) |
| `target.entry-head` | error | [Entry](#entry) |
| `target.entry-missing` | error | [Entry](#entry) |
| `target.link-failed` | error | [Images](#images) |
| `target.program-targets` | error | [Programs and libraries](#programs-and-libraries) |
