# 0035. The kernel runtime

Status: accepted; its entry root superseded by 0074, its periodic timer by 0081

## Context

On `x86_64-uefi` and `x86_64-bare` the support routines run a kernel ([kernel execution model](../spec/24-kernel.md)): the anchor starts the scheduler on every CPU, interrupt fns run on per-CPU interrupt stacks, the timer's `Reschedule` preempts tasks, signals wake tasks from interrupt fns, devices sit behind IRQ-safe locks, and a halt stops every CPU. The specification fixes what code observes, not how the routines find CPUs, reach per-CPU state, pace preemption or lay out memory. The host runtime already provides tasks, a run queue, cells and capability tables ([0023](0023-host-runtime-and-domain-ownership.md)), with no C library or system to lean on in a machine image.

## Decision

- **Per-CPU records through GS.** Each CPU has a record in an array the image reserves; its GS base addresses it, and its first word is the CPU's executor thread record, which the `thread` stub reads in one instruction. The record holds the CPU's descriptor tables, task-state segment, interrupt stack bounds and page-table root.
- **CPUs from the MADT.** `Init` counts the CPUs the ACPI MADT reports usable, the RSDP coming from the PVH start information, the BIOS area or the UEFI configuration table. The anchor starts each other CPU with INIT and startup IPIs through a trampoline it copies to physical page `0x8000`, which climbs to long mode on the entry root.
- **One run queue.** Every CPU's scheduler takes tasks from the one run queue the host runtime uses, behind its IRQ-saving lock, so a task made ready runs on whichever CPU next looks. A CPU with no ready task halts until an interrupt.
- **The timer paces the kernel.** The anchor starts each CPU's local APIC timer, periodic on vector `0x20`. A bound interrupt fn serves it like any vector and its `Reschedule` preempts; unbound, the dispatcher completes it, so idle CPUs still wake to find work. The APIC's spurious vector `0xFF` is ignored unless bound.
- **Interrupt stacks in software.** Gates of `ist` 0 enter the seam, which switches to the CPU's interrupt stack unless already on it; other `ist` values use the task-state segment. To preempt, the seam copies the saved frame and extended state below the task's stack pointer and calls the `hw::preempt` landing fn there, so the interrupt stack is free while the task waits elsewhere.
- **Locks know their holder.** Runtime locks disable interrupts while held. Every lock word, the runtime's, an `hw::SpinLock`'s or a device's, holds its CPU's number plus one, so a CPU acquiring a lock it holds halts with `lock-reacquired` through a support routine, off the caller's stack budget. A device installed by `hw::IrqShared.install` lives in a runtime record that a per-vector binding table names, with a count of CPUs running its interrupt fn that `uninstall` waits on.
- **Halts stop CPUs by NMI.** The first CPU to halt sends every other started CPU a non-maskable interrupt, whose seam stops it, and waits for them before writing its report.
- **Memory.** Carved allocations are 16-byte aligned, which suits devices reaching DMA buffers. A store over an `hw::Region` gives its root cell a supply of the region's chunks, which the cells under it draw from instead of the boot heap. `exit_services` installs the same entry root as the `x86_64-bare` seam: the first 4 GiB identity-mapped, the last uncached. The image writer records the image's extent in the image, from which `k.regions()` excludes it.
- **No user-mode seams.** No operation of language 1 enters privilege level 3, so images hold no syscall seam and no user-origin exception path, and never enable `syscall`.

Alternatives rejected:

- **Per-CPU run queues with stealing:** more code and locks for a kernel that runs a handful of tasks, and a woken task's CPU would need choosing.
- **Waking idle CPUs by IPI:** an IPI needs a vector, and every vector from 32 up belongs to bindings; the timer already wakes idle CPUs.
- **The local APIC id indexing a table:** an MMIO read on every runtime lock.
- **Hardware IST for `ist` 0:** an exception inside an interrupt fn would restart the stack from its top and overwrite the frames in use.

## Consequences

- A task made ready while every CPU idles waits up to one timer period.
- A kernel without a timer binding is never preempted, as the specification requires, while its CPUs still wake each period.
- The trampoline page must be free memory below 1 MiB on both platforms; QEMU and OVMF leave `0x8000` free.
- The cell table grows a supply word per cell on every target.
- The entry root reaches no memory above 4 GiB, so machines with more memory see only their first 4 GiB.
