# 0117. Stacks grown at cycles

Status: accepted

## Context

[0114](0114-nanos-targets-checked-without-images.md) made every task and entry of the nanos targets state a static stack bound, as on the machine targets, because a kernel draws stacks of static size. NaN OS asks for stacks the runtime grows for code without bounds: such a task starts with 64 KiB, a grown stack is drawn through the cell of the task's domain like any memory, and exhausting the domain's limit traps the task (`trap.exhausted`) instead of overflowing. The code runs in a kernel, in bundles [0116](0116-nanos-jobs-in-a-kernels-shared-space.md) lowers against the support routines the kernel exports, where the runtime owns no page fault: its vectors are the kernel's, so a guard page cannot be how a stack learns it is full. Lowering already knows, at each final build, every frame's size and every call site, and most code calls nothing that can recurse. Cranelift has no split-stack prologue: its stack limit check traps, and a function's frame exists before its first instruction runs. The NaN toolchain must lower to the same Cranelift IR.

## Decision

- **Growth happens at calls that can be unbounded.** In a nanos bundle, a direct call closing a cycle of the calls lowering has emitted so far, and a call of a function value whose type demands no `stack N`, is a **grown call**. Every cycle of lowered calls then holds a grown call: its last call emitted is one. Each other stretch of code is bounded by the frames lowering already measures.
- **A grown call goes through a wrapper and a thunk.** The wrapper, of the callee's signature, writes the arguments into a record and calls the support routine `Grow` with the thunk, the record and the thunk's need; the thunk makes the call from the record and writes its results back. The need is the stack the thunk's code uses up to the grown calls it makes, from a table the lowering fills once frames are known, and a function value's code needs the most of any lowered function.
- **`Grow` checks against a limit in the task record.** A kernel exporting its load ABI exports `Grow`, and its task records hold the limit and top of the stack each task runs on and the newest segment it grew. When the stack the call runs on is the running task's and lacks the need above the task's limit, `Grow` draws a segment through the task's stack cell, of 64 KiB or the need if more, links it in the task record, switches to it, runs the thunk and returns the segment once the thunk returns. A segment that cannot be drawn traps the task. Any other stack, an interrupt's, the scheduler's or a routine's reserve, runs the thunk where it is.
- **Segments stay walkable.** The segment's first frame links to `Grow`'s, so trap reports and stop checks walk through every segment to the task's own stack, and an ended task returns the segments a trap left it on.

Alternatives rejected:

- **Split-stack prologues in every function:** they need code before the frame is allocated, which neither code generator can emit from IR; a check after the prologue needs every caller to leave room for the largest frame, and costs every call a read of the thread record.
- **Contiguous stacks moved and copied:** references into frames are interior pointers that lowering cannot find and fix.
- **Guard pages and demand paging:** the runtime would need to own page faults that a kernel's vectors serve.

## Consequences

- Code of the nanos targets needs no stack bound: a task without one starts with 64 KiB and grows, and a job whose entry states none starts with the stack its job entry uses up to its grown calls, at least 64 KiB. A stated bound is still judged, through each grown call as through the call it makes, and still sizes the task's stack.
- A recursion pays a wrapper, `Grow` and a thunk per recursive call; code without cycles or unbounded function values pays nothing.
- The check runs in lowered code and the kernel's routines, so it holds on any kernel executor, for a job's task and every task nanos code starts, their segments drawn through the cell of the domain `nanos::run` creates.
- Supersedes the decision of 0114 that every task and entry of the nanos targets states its stack bound; interrupt fns and landing fns still state theirs.
