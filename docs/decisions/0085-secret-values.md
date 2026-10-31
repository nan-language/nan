# 0085. Secret values

Status: accepted

## Context

NaN OS implements Ed25519, X25519, ChaCha20-Poly1305 and BLAKE3 in NaN. Code handling keys must not let its branches, memory addresses or instruction timings depend on secret values. Whether it does depends on lowering as much as on source: a compiler may turn a conditional expression into a branch or a table lookup, and processors run some instructions in data-dependent time unless a mode says otherwise. Only the toolchain can guarantee the property.

A flow analysis marking a function constant-time with respect to some inputs would have to follow secrets through every construct, and its verdict would change with code far from the secret. A type carries the property through the existing rules: whatever the checker already rejects for a type without `Eq`, `Ord` and `Hash` cannot decide a branch.

## Decision

- **`core::Secret[T]` wraps a value whose operations are constant-time.** It is a base type over `core::SecretBits`, which exactly the integer types and `bool` implement. Floats are excluded: their operations run in data-dependent time on subnormal values, and Cranelift lowers a float select with a branch on x86_64.
- **The type system does the checking.** `Secret[T]` implements no std interface but the property interfaces, so a Secret value cannot be compared with `==`, hash a map key, render or encode. Its members return Secret values: comparisons give `Secret[bool]`, which no `if`, `while`, `and` or index accepts; `select` chooses without a branch. No member divides or traps on a secret, so no obligation, and so no checked trap, depends on one; only the shifts check their public amount. A Secret value where its contents are expected is reported as `secret.public-use` rather than `type.mismatch`, pointing at `select`. `reveal` is the one declassifying step, explicit at its call.
- **Each member is a fixed sequence of instructions.** Lowering gives every member one block of additions, multiplications, bitwise operations, shifts, rotations, comparisons, conditional moves and extensions. Cranelift and the NaN toolchain's code generator lower an integer `select` to `cmov` or `csel`. The slice members `equal` and `copy_if` are written in NaN and loop over public lengths.
- **The runtime turns on the processor's data-independent timing mode** where a program can: DOITM on x86_64 machine targets and `PSTATE.DIT` on AArch64 machine targets as each CPU starts, and `PSTATE.DIT` on `aarch64-host` before the executor threads start, which inherit it, when the kernel reports `HWCAP_DIT`. `PSTATE.DIT` is kept across exception entry and restored by `eret` from `SPSR`, so interrupt delivery leaves it on. An x86_64 host process cannot set DOITM; that mode is the kernel's.
- **No lane types yet.** `SecretBits` holds the scalar types alone until std has vector lane types; adding them extends the interface without changing its users.

## Consequences

- Constant-time code reads as ordinary NaN with method calls, and a mistake that would branch on a secret is a diagnostic at the line that makes it.
- Each member is a call of its own lowered function; the calls keep the property and cost their call overhead.
- The guarantee stops at the instructions lowering emits: speculative execution, power and frequency side channels, and clearing secrets from memory are outside it.
