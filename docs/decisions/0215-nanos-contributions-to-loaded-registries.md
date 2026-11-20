# 0215. Nanos contributions to loaded registries

Status: accepted

## Context

[0116](0116-nanos-jobs-in-a-kernels-shared-space.md) let a kernel call nanos code only through its job entry, and later its component entries, because a function field of a loaded registry would enter nanos code without owning extended state and carry float arguments in registers kernel code does not use. NaN OS admits code only as nanos bundles, yet loads its kernel policies and drivers as entries of loaded registries, which loading publishes, replacing swaps and unloading retracts with their bundle.

## Decision

- **A nanos program contributes to its kernel's loaded registries** as a bare bundle does: its entries are published, replaced and retracted with it, each function field holding a gate.
- **Each function field enters the nanos code through a wrapper** the bundle lowers for the function's value: it owns extended state of its own in its frame while it calls the function, as a job or component entry does, so the gate stays the kernel's own and needs no knowledge of the target it calls.
- **A field whose types hold a float is refused at the bundle build** (`target.bundle-interface`), naming the field: kernel code and nanos code pass floats differently, and any other type crosses in the kernel's ABI unchanged.

Alternatives rejected:

- **Gates that own extended state:** every gate of a kernel would pay for a save area, and bare bundles need none.
- **Converting floats at the wrapper:** the kernel never computes in floats, so a field passing one serves no kernel, and converting would fix an ABI detail into the language.
- **Components for policies and drivers:** a kernel calls a policy on every scheduling decision and a driver from its own tasks; a message to an instance and its reply cost far more than a call.

## Consequences

- The 4,160 bytes of extended state a wrapper saves count against the `stack N` of the field's type, which a registry of nanos policies states accordingly.
- A trap in a contributed function abandons the calling task's domain, as it does for a bare bundle.
