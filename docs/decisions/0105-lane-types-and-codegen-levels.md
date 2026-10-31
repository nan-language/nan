# 0105. Lane types and codegen levels

Status: accepted

## Context

NaN OS hashes, checksums, encrypts and runs inference on every core, and scalar code runs BLAKE3 at a fraction of a drive's speed ([0062](0062-startup-and-cold-load-costs.md)). Language 1 had no vector types, reserved `core::simd`, and fixed every descriptor's codegen to the base instruction set: SSE2 on x86_64 and Armv8.0-A with Advanced SIMD. Results must stay identical in the interpreter and on every target ([09 Numbers](../spec/09-numbers.md)), machine targets use no vector register ([0096](0096-machine-code-without-extended-state.md)), and the NaN toolchain must give the bootstrap's IR and generate its own code for it ([0046](0046-the-nan-code-generator.md)). Cranelift 0.135 lowers 128-bit vectors on both architectures and enables extensions by ISA flags; its `has_fma` only affects the explicit `fma` opcode, never contracting `fmul` and `fadd`, and its `has_dotprod` folds the i8 dot product tree into `sdot`.

## Decision

- **Fixed 128-bit lane types in `core::simd`**: eight integer lane types, `F32x4`, `F16x8` as binary16 storage, and four mask types. Each is a struct over an array of its lanes, and each member has a body computing lane by lane: that text is the specification the interpreter runs, the prover reads and a machine target lowers, so lanes there need no lowering of their own and stay in general registers. Reductions have a fixed order: integer sums wrap, `F32x4.sum` adds `(l0 + l1) + (l2 + l3)`. Float lanes follow the scalar float rules, IEEE 754-2019 minimum and maximum included; binary16 conversions are integer arithmetic.
- **Vector lowering replaces the bodies on host targets**: a fixed set of members lowers to Cranelift vector instructions on lane values loaded from and stored to memory, which is where lane values live, so no vector crosses a call or a block. The i8 dot product lowers to the tree `has_dotprod` folds. `store` is supplied, as the prover cannot follow a slice through writes, and copies the lanes on every target. The same IR at every level keeps results level-independent.
- **Codegen levels by manifest directive**: a program's `codegen` directive selects at most one level per architecture, `x86_64-v2`, `x86_64-v3`, `x86_64-v4` or `aarch64-dotprod`, adding only Cranelift ISA flags. A library cannot choose one, so artefacts stay level-free. The image checks its level before anything runs, through CPUID and `XGETBV` or `getauxval`, and on a machine target through `ID_AA64ISAR0_EL1`, and exits with status 71.
- **The NaN code generator selects per level**: SSE2 sequences at the base level, SSE4.1 and SSE4.2 instructions at `x86_64-v2`, three-operand VEX forms and AVX2 broadcasts at `x86_64-v3`, EVEX `vpmullq` and `vpsraq` at `x86_64-v4`, Advanced SIMD on AArch64 and `sdot` at `aarch64-dotprod`. Functions holding vectors are never inlined, so they hold fewer vectors than registers and never spill one.
- **The integer lane types implement `core::SecretBits`**: `Secret` of one adds, subtracts, multiplies, combines bits and shifts lane by lane, compares the whole value and is chosen by `select`, each without a branch, vector instructions being constant-time where scalar ones are.
- **Kernel float use extends to calls of std functions with float use**, so a kernel calling an `F32x4` member is rejected instead of reaching a float instruction on a machine target.

Rejected alternatives:

- **Generic `Lanes[T, N]`**: one declaration per member, but every valid shape and per-shape member would need rules of its own; the fixed types read as what they are.
- **Bodiless supplied members**: two interpreters and two machine-target lowerings per member to keep in step with the vector lowering.
- **Function multiversioning by runtime dispatch**: indirect calls per lane operation, and a level per call site instead of per image.
- **Vector registers in the call ABI**: lane values in registers across calls would need vector spills and a calling convention for both code generators.

## Consequences

- BLAKE3 and quantized inference can use 128-bit lanes on hosts and the same code scalarised in kernels; lane members are calls, so their speed is bounded by call overhead until lowering inlines them.
- Wider vectors, swizzles and lane shuffles remain absent; adding one means a body, a vector lowering in both toolchains and selection in both code generators.
- Each level's build is a separate image; a program needing several levels ships several builds.
