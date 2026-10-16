# 0023. Host runtime and domain ownership

Status: accepted

## Context

`x86_64-host` images need the support routines of [27](../spec/27-program-model.md#runtime-support): task stacks and switching, the io_uring executor, acknowledged cancellation, capability tables, domains and trap reports. Cranelift 0.135 provides what [0002](0002-bootstrap-stack-and-lifetime.md) relies on: `stack_switch` under the `basic` model, which depends on no operating system, frame layouts, the `win64` convention and object emission.

Abandoning a domain D must stop D's running tasks once the operations the [abandoning steps](../spec/11-errors-domains.md#abandoning-a-domain) name have completed, and [settlement](../spec/11-errors-domains.md#settlement) must release every resource, revoker, budget and channel end a task of D owns, directly or through the values holding it, then free D's memory. Values cross domains at a few std operations ([domain memory](../spec/11-errors-domains.md#domain-memory)); everywhere else a domain's values stay its own. Images carry no stack maps.

## Decision

- **Routines lowered with the program.** The support routines are Cranelift functions the lowering emits into each image, beside glue specialised per type: no runtime library is built or linked.
- **Stopping by signal.** A running task of an abandoned domain is stopped through a signal to its thread. The handler stops it only outside critical code, which the line tables mark and a per-thread count of held locks extends: support routines, the std operations the abandoning steps name, and `sys::Domain.run`, whose end passes or frees a domain's holdings and memory as one step. Otherwise the task stops when it next switches away.
- **Overflow through the guard.** Frames over 4 KiB probe each page, so an overflow faults on the guard region; the fault handler, on a signal stack, resumes the task on its thread's trap stack to report `trap.stack-overflow`. Every task stack reserves 32 KiB beyond its usable size for support routines and the C library functions they call.
- **Holdings.** Each resource, revoker, budget and channel end has a holding record in its owner domain's list. Settlement releases the abandoned domain's list; a domain ending normally passes its list to its parent.
- **Transfer walks.** At each point where a value moves into another domain's reach, a walk specialised per type passes the holdings it owns to their new owner and lifts the chunks holding its memory, when domains nested in the target own them, to the target. A lift moves a whole chunk run.
- **Reach of shared storage.** A cown, a shared value and a channel record the innermost domain enclosing every domain that reaches them. A walk enters their contents only when that reach widens, raising the holdings inside to it; a cown held by an `await when` is entered by that body as it ends, and a receiver's buffered values are walked from a copy taken under the channel's lock.
- **Freeing by ownership.** When a domain ends, normally or by settlement, the chunks it still owns are freed.

Alternatives rejected:

- **A runtime library in Rust or C:** unsafe code would enter the toolchain or a second toolchain would build the images, and the routines could not be specialised to the program's layouts.
- **Safepoint polls:** every loop of every function would pay to let settlement stop a task that rarely needs stopping.
- **Scanning the abandoned domain's stacks:** without stack maps a scan is conservative, so it could neither release exactly what D owns nor free memory safely.
- **Never freeing a domain's memory:** each trapping domain would leave its chunks charged, exhausting budgets in a program that isolates work in domains.
- **Ownership per allocation:** every allocation would carry and update an owner, while values cross domains only at a few operations.

## Consequences

- Every std operation that moves a value between domains calls the walk; `transfers` refuses to lower a std base type it does not list, so a new base type must state what its walk does.
- Moving a value between domains costs time proportional to the parts it holds; work inside one domain pays nothing for ownership.
- A lift keeps a chunk's other allocations alive with the moved value, so an abandoned domain can leave some memory allocated in its parent's chunks, never memory another domain reaches freed.
- A value taken out of shared storage into a nested domain stays owned by the storage's reach until it next crosses a domain, so that domain's settlement does not release it.
- Signals, guard faults and the trap stack are Linux mechanisms and live in the host module.
