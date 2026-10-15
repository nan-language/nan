# 0004. Value semantics without references

Status: accepted

## Context

NaN builds a kernel, drivers and a compiler, so it needs in-place mutation, address-stable memory and no undefined behaviour in safe code, without a garbage collector or a mandatory runtime.

Rust reaches this with references and lifetimes. Lifetime variables enter heads and tie items together, and borrow errors name regions and code outside the window. That breaks closure ([0003](0003-agent-first-guarantees.md)) and makes such errors hard to repair from a window. An agent reading one line must also see whether a call reads, changes or consumes each argument.

A kernel also holds resources that must never be dropped silently (device handles, DMA buffers, join handles, page-table roots) and needs every byte of memory accounted to an owner.

## Decision

- **Ownership.** Every value has one owner; passing or assigning a non-Copy value moves it. End of scope returns memory to its store and runs no user code: no destructors ([ownership](../spec/07-ownership.md#ownership)).
- **Three modes**, read (the default), `mut` and `take`, spelled alike in heads and at calls ([modes](../spec/07-ownership.md#modes)). Every `mut` argument is marked, and so is every `take` argument that is not Copy; a Copy value passes unmarked, since its source stays usable. Exclusivity is static per place path: no aliasing and no dynamic borrow checks.
- **Projections replace references.** A `proj fn` returns a part of its receiver, and its result derives only from that receiver. Binding a non-Copy projection result makes a view; its source stays frozen (read) or held (`mut`) while the view or anything derived from it is live over the item's control-flow graph ([holds](../spec/07-ownership.md#holds)). A view never outlives its source, and a conflict report quotes the hold, the access and the use that keeps the hold live.
- **Structural properties.** Linear, view, Copy, Send, Inert and AnyBits are computed from a type's structure ([structural properties](../spec/05-properties.md#structural-properties)). Source states only those that change how code is written: `linear`, `view`, `derives Copy`, and the promises of open types. Views are second-class: parameters, locals, projection results and fields of views, never ordinary fields, results, Send or Encode. Linear values must be consumed, and a type holding one is linear.
- **Result location.** Every value is built in its destination ([result location](../spec/07-ownership.md#result-location)), so returning a large kernel structure copies nothing.
- **Box.** `mem::Box[T]` is the only owned indirection, and its contents never move while it exists ([box](../spec/07-ownership.md#box)). Memory that hardware points at lives in a Box.
- **Allocation is authority.** No global allocator exists, and `core` never allocates ([allocators](../spec/12-memory.md#allocators)). `mem::Alloc` is a Copy generational handle naming a cell; a linear `mem::Budget` owns a sub-cell charged to its parent, and closing it makes every copy stale. Containers store their allocator; `push` traps on exhaustion and `try_push` fails. Graphs use stores and their ids or generational handles instead of pointers ([stores](../spec/12-memory.md#stores)).
- **Interior mutability** exists only in trusted std types, all thread-safe, which never move or free entries they hand out views of.

Rejected alternatives:

- **References with lifetimes:** non-local reasoning in heads and errors, against closure.
- **A garbage collector:** a kernel without a runtime cannot host one, and collection hides when memory returns.
- **Holds lasting to the end of the block, with `with` blocks to narrow them:** spurious conflicts and a second scoping construct. Holds ending at the textual last use are unsound in loops and through derived views; control-flow liveness is not.
- **An `out` mode:** a C idiom with an initialised-on-success rule, which result location makes unnecessary.
- **Fixed types, `mem::Place` and pinning:** address stability belongs to the owning slot, and one never-moving Box gives it.
- **Destructors:** hidden code at every scope exit, cleanup that could block or fail, and code that a trap, which unwinds nothing, would skip.
- **Opt-in linearity and declared Send:** a wrapper, generic or container would leak the property. Computing them structurally closes those leaks.
- **A separate Share property and a non-atomically counted `mem::Frozen`:** Frozen was the only type that set the two apart.
- **A global allocator, ownerless sub-budgets and allocation grants in the manifest:** allocation would be ambient, a sub-budget would have no lifetime, and capabilities already govern allocation.

## Consequences

- Agents write no lifetimes, and every mutation and move is visible and greppable at its call site.
- A function returns a view only through a projection of its receiver. Code that would return a reference into another argument returns an index, an id, a handle or a copy instead.
- Hold conflicts depend on later uses of a view, so diagnostics must quote the use that keeps a hold live.
- A method that needs disjoint fields of `self` calls a helper taking those fields as separate parameters. Field grants in heads stay reserved: `self.{a, b}` parses and is rejected with a fix.
- Linear values need an explicit consuming call on every path; early exits need `defer`. Their soundness depends on traps never unwinding ([0005](0005-only-waiting-is-written.md)).
- Every container and store names an allocator, so heads that build data carry an allocator parameter, and memory is charged per budget and per domain. Exhaustion is a trap or a failure chosen by the operation's name.
- Allocation pays for a generation check and for cell charges in 64 KiB chunks; its cost is one of the freeze [measurements](../spec/38-self-hosting.md#measurements).
