# 0194. Thread-locals accessed from the thread pointer

Status: accepted

## Context

The runtime keeps the running executor thread's record in a thread-local variable, which the allocator's routines read on every allocation and free, three times and twice. x86-64 code took a thread-local variable's address through `__tls_get_addr`, as Cranelift does for the general-dynamic model, and the linker rewrote each call into two instructions for the executable. Selection still treated each access as a call clobbering every caller-saved register, so its function laid out a frame for calls and kept the values live across it in callee-saved registers or slots.

## Decision

- **On x86-64, a load, store or atomic operation at a thread-local variable's address plus a constant addresses it from the `fs` segment**, the thread pointer, with an `R_X86_64_TPOFF32` relocation of the variable's offset from it, as the local-exec model of an executable allows.
- A thread-local address used otherwise is still taken through `__tls_get_addr`.

## Consequences

- Reading the thread's record takes one instruction; the binary trees program runs 2.8% fewer instructions.
- Every image the NaN toolchain builds for an x86-64 host is an executable, the only kind of object whose thread-local variables lie at offsets fixed when it is linked; a shared library built from its code would need the general-dynamic model.
- AArch64 code keeps taking thread-local addresses through TLS descriptors.
