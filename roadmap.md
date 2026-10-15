# Roadmap

Tasks in order, one agent each. Chapter numbers refer to `docs/spec/`; milestones are defined in `docs/spec/38-self-hosting.md`.

## 1. Front end foundation

Implement the syntax layer of the bootstrap compiler: workspace, package and module loading (03), the lexer and parser of the grammar (41), `nan fmt` (35) and the diagnostic output contract (33, 34). Add a conformance harness that runs every NaN example of the specification against its marker comments, and a CI pipeline running format, lint and tests.

## 2. Semantic checker

Implement the checking rules of language 1 in `nan check` on an item-level query engine: names, types, generics and interfaces, structural properties, ownership and holds, effects, digests and limits, errors, authority, Untrusted data and registries (03 to 17, 23 to 25). Include exact fixes, `nan fix`, `nan holes`, prefix checking and the reading and editing commands (33).

## 3. NIR, interpreter and prover

Complete `nan check`: elaboration to NIR (19), the reference interpreter (21), contracts and check elimination (18) and the normative prover (20). Done when M1 passes.

## 4. Standard library

Write the std packages in NaN (28), every pub item with its contracts and examples, checked by `nan check`.

## 5. Host code generation and runtime

Lower NIR to `x86_64-host` executables through Cranelift with `nan build` and `nan run` (27, 30), with the runtime support routines (stackful tasks, io_uring executor, acknowledged cancellation, domains and settlement, capability tables, trap reports) and the host platform package. First confirm that Cranelift 0.135 provides stack switching, frame sizes, the win64 convention and freestanding objects; record an ADR if anything is missing.

## 6. Testing and components

Implement `nan test` (22) with generators, simulated capabilities, the seeded scheduler and replay, and components with `host::Runner` (26). Done when M2 and M6 pass.

## 7. Trust, packages and merging

Implement `nan verify` with the ledger, seals, API snapshots, doc binding and `nan diff` (31, 32); identity hashes, requires-sets, artefacts, the local store, `nan dep`, `nan publish --local` and `nan upgrade` (29, 30, 36); and the `nan merge` driver (33). Done when M3 passes.

## 8. Machine targets and kernel

Add the `x86_64-uefi` and `x86_64-bare` targets: compiler stubs, the image writer, registries as data, the QEMU runner, the hw and boot std packages and both platform packages (23 to 25, 27). Then write the M4 scenario: a UEFI loader and an SMP kernel with preemption, a UART reader task woken by a signal and a virtio driver. Done when M4 passes.

## 9. Front end in NaN

Write the NaN front end in NaN: `nan check` with the prover and the interpreter, printing exactly what the bootstrap prints (38). Done when M5 passes.

## 10. Freeze language 1

Take the measurements of the freeze (38), count the primer tiers with three tokenizers and build nanbench (37). Fix whatever misses its bound, recording every language change in an ADR. Done when all six milestones pass on one build and every bounded measurement meets its bound.

## 11. Self-hosting

Make the NaN toolchain build itself: write the code generator in NaN over NIR (instruction selection, register allocation, x86-64 encoder, ELF and PE32+ writers, code without floating point for machine targets) and port the remaining commands. Then check the first self-built toolchain against the frozen bootstrap by diverse double-compiling and stage comparison (ADR 0002).
