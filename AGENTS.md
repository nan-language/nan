# AGENTS.md

## Project

NaN is a programming language made for agentic coding: performant, fast and safe, with strong versioning so that compiled NaN artefacts can reuse each other. It will compile itself and build NaN OS, an operating system for AI, from bootloader to user interface; its Rust bootstrap compiler, targeting Linux on x86_64, then stays frozen and serves only to verify the self-hosted compiler.

## Architecture

`bootstrap/nan` is the bootstrap compiler, a Rust binary named `nan` that implements the language specified in `docs/spec`. It generates machine code with Cranelift: the system `cc` links Linux x86_64 executables, and the compiler's own image writer builds UEFI and bare-metal images. Everything Linux-specific (linking, program entry, calls into the system) stays in one target module, and what the machine targets alone need (seams, compiler stubs, CPUs, interrupt delivery) in another.

Its parser runs on tables that `build.rs` generates from the `ebnf` blocks of `docs/spec/41-grammar.md`, so the grammar changes only in the specification. Its checker computes every fact as a query over one item and one target, recording what each query reads; context packs come from those records. Once every package is checked, each item without errors elaborates to NIR, which the reference interpreter runs for examples and refutations and over which the prover decides each obligation; the rules needing them run then. Checking runs a const's evaluation when it first reads the const's value. The build cache of the user directory keeps the results the prover proves, or runs out of fuel on, and the code root of each package state, which symbol names and images read. The workspace reads each package's machine files (lock, history, API snapshots) and its dependency closure from its lock: the local store of the user directory keeps each release's artefact and source tree, and a locked release is checked from its stored sources as a quiet package of the consumer's closure, packages whose closures agree sharing one checker. Resolution, publishing and the release gate run over these closures; in a git working tree the release gate reads the history through the `git` program for the author rule. A final build lowers the NIR of its closure to Cranelift IR together with the support routines its image runs. `nan test` lowers a package's tests the same way into one test image, whose support routines run the seeded scheduler and the simulated capabilities, runs each test run as a process of it, or as a boot of the emulator on a machine target, and records passes in the build cache of the user directory. The std packages are NaN sources that `build.rs` embeds in the binary; a std function without a body is a supplied operation, whose facts the checker derives and which the interpreter runs natively. The conformance harness runs every `nan` example of the specification against its marker comments, and each command of the `text` blocks of its section, after locking, snapshotting and publishing its packages into an empty local store; it checks each item's context pack against what checking it reads, compares the obligation outcomes of host examples with the proof cache's, and checks that std declares each `nan-api` item of the specification exactly. The program harness builds each program of `tests/programs` and compares what its image writes with the output it expects. The milestone harness runs the commands each milestone states in its scenario workspace of `tests/milestones`, for M4 each program under the emulator until its serial output shows the behaviour, then `nan verify`. The measurements harness checks the bound on the share of bounds and overflow obligations the prover proves, on the M4 scenario and on the NaN toolchain's workspace. The harnesses run `nan` in-process, sharing a home directory under the target directory, each chapter and milestone with a local store of its own.

`toolchain/nan` is the NaN toolchain: a NaN program that implements every command, its modules mirroring those of the bootstrap (syntax, workspace, check with elaboration, the interpreter and the prover, fixes, output, the commands, final builds, test runs and lowering), so that it prints what the bootstrap prints, gives the same NIR and builds each function into the same Cranelift IR; a change to the bootstrap is ported there in the same change. Each command runs in a domain of its own, so that a trap of the toolchain's code is an internal error whose reproducer the command's caller writes. Worker tasks, one per other processor up to seven, share with the command's task the per-item work its checks post as batches, and the parsing ahead of its workspace load. Its build cache also keeps a snapshot of each check of a closure under a target, from which a check of a state differing in function bodies checks again only the items those reach. Its code generator stands in for Cranelift: `clif` holds the IR and its builder with Cranelift's numbering and text, `codegen` selects x86-64 instructions, allocates registers and lays out frames, `x64` encodes them, `object` writes the relocatable object as cranelift-object does, and `image` ports the image writer. The self-check harness builds it with a root file of its own, which gives it the harnesses' home directory and makes it dump its NIR or a program's lowering, and compares it with the bootstrap on every command the conformance harness runs for each `nan` example of the specification, with each final build's lowering and what each `x86_64-host` program writes, on every std package with its tests, on the programs of `tests/programs`, and on the toolchain workspace, which must check without a diagnostic and pass `nan verify`. Its toolchain test then checks the first self-built toolchain: the NaN toolchain the bootstrap built builds the toolchain, which builds it again into the same bytes, and the bootstrap's stage comparison accepts it.

`nanbench` is the benchmark that gates language versions, a Rust binary of the workspace that reads the bootstrap through its library. It counts each primer tier's `nan primer` text under three tokenizers, Llama 3's ranks fetched once into the target directory; takes the measurements of the freeze over each milestone's scenario, through what checking, final builds and test images give and through the benchmark programs of `nanbench/bench`; and runs named models through the Messages API over the tasks of `nanbench/tasks`, each in a fresh copy of its workspace, by a text protocol whose `nan` commands run in-process, then scores first-compile success, fix rates per diagnostic code and comprehension. Each command writes its results under `nanbench/results`; a reference solver checks the corpus.

Only what an agent needs to place a change correctly. The why is in ADRs.

## Layout

| Path | Holds |
| --- | --- |
| `bootstrap/` | The bootstrap compiler, one Cargo workspace crate per directory |
| `bootstrap/nan/src/` | Source of the `nan` binary; a module's unit tests live in `<module>/tests.rs` beside it |
| `bootstrap/nan/tests/` | The conformance, program, milestone and self-check harnesses; `programs/` holds one program per directory with its `expected` output, `milestones/` one scenario workspace per milestone |
| `std/` | The std packages, one directory per package |
| `toolchain/` | The NaN toolchain, a workspace of one package, `nan` |
| `nanbench/` | The benchmark gating language versions: its crate in `src/`, the task corpus in `tasks/` (one directory per task), the benchmark programs in `bench/` and the published results in `results/` |
| `docs/spec/` | The NaN 1.0 language specification, one chapter per file |
| `docs/decisions/` | Architecture decision records |
| `editors/` | Editor support: the TextMate grammar of `.nan` files in `textmate/` |
| `.config/` | nextest configuration |
| `.github/` | The CI workflow |
| `.agents/skills/` | Agent skills; `.claude/skills` links here |

One row per top-level directory; deeper only where the tree alone would not tell an agent where a change belongs.

## Commands

| Command | Purpose |
| --- | --- |
| `rustup toolchain install` | Install the Rust toolchain pinned in `rust-toolchain.toml` |
| `cargo install --locked cargo-nextest@0.9.148` | Install the pinned test runner |
| `cargo build -q` | Build the compiler to `target/debug/nan` |
| `cargo fmt --all` | Format all code |
| `cargo clippy -q --workspace --all-targets -- -D warnings` | Lint; any warning fails |
| `cargo nextest run --cargo-quiet -p nan -E 'test(/^<module>::/)'` | Run the tests of one module, given by its path in the crate (`a::b`) |
| `cargo nextest run --cargo-quiet -p nan --test conformance -E 'test(/<chapter>/)'` | Run the conformance tests of one specification chapter, given by its file name (`01-lexical`) |
| `cargo nextest run --cargo-quiet -p nan --test programs -E 'test(/<program>/)'` | Build and run one program of `bootstrap/nan/tests/programs`, given by its directory name |
| `cargo nextest run --cargo-quiet -p nan --test milestones -E 'test(/<milestone>/)'` | Run the scenario of one milestone of `bootstrap/nan/tests/milestones`, given by its directory name (`m2`) |
| `cargo nextest run --cargo-quiet -p nan --test self_check -E 'test(/<scope>/)'` | Compare the NaN toolchain with the bootstrap on one chapter (`06-statements`), std package (`std_package::mem`), program (`program::files`) or the toolchain workspace, then double-compile it (`toolchain`); or check that a trap of its own code is an internal error (`internal_error`) |
| `cargo nextest run --cargo-quiet -p nan --test measurements` | Check the prover's discharge of bounds and overflow obligations on M4 and M5 against its bound |
| `cargo run -q -p nanbench -- tokens` | Count each primer tier's tokens into `nanbench/results/primer.md`; fails when a tier passes its cap |
| `cargo run -q -p nanbench -- measure` | Take the measurements of the freeze into `nanbench/results/` |
| `cargo run -q -p nanbench -- corpus` | Run every task of `nanbench/tasks` with the reference solver; fails on an unsound task |
| `cargo run -q -p nanbench -- run <model> [<task>...]` | Run tasks with a model of the Messages API, the key in `ANTHROPIC_API_KEY`, into `nanbench/results/runs/` |
| `cargo run -q -p nanbench -- report` | Write `nanbench/results/nanbench.md` from the recorded runs |
| `../target/debug/nan check <package>` | Check one std package, from `std/`, with the compiler built last |
| `../target/debug/nan test <package>` | Run the tests of one std package, from `std/`, with the compiler built last |
| `../target/debug/nan check nan` | Check the NaN toolchain, from `toolchain/`, with the compiler built last |

Design each command for who runs it: agent-facing commands run without interaction, accept a scope where one applies and print only what an agent needs. Agent-facing commands are listed here, human-facing ones in README.md, shared ones in both.

## Economy

**Context.** Everything committed to this repository is read by every agent that works here later: code, comments, docs, tests, ADRs and this file. Every token that carries no necessary information costs money and dilutes attention on the information that matters. Each kind of knowledge has one home: the **why** in ADRs, the **what** in documentation, the **how** in the code.

**Time.** Every check, run and read costs time. Do each once, at the narrowest scope that proves what you need, and skip anything your change cannot affect.

## Git

Never run a command that alters git history or the remote (commit, amend, rebase, merge, reset, cherry-pick, push, tag, branch deletion) without explicit approval from the user for that specific command. Approval covers one action and never carries over to the next.

## Code

- Follow industry best practice. No over-engineering, no quick fixes.
- The project will grow a lot. Structure it so the next change is easy, not so future features are pre-built. Be smart, not exhaustive.
- Keep the tree, the files and the code organised and clear without being verbose. Code explains the how by itself.
- When the change you are making tips the existing structure into being less maintainable, refactoring is part of that change. Do it now.
- Leave nothing unused: no dead code, no dormant logic, no remnant of a replaced approach. The codebase reflects exactly what it does today.

## Verification

Never guess: run the code and observe. Verify the full behaviour of what you change, including its callers. Never deliver untested work.

## Tests

Write tests that block regressions. Each test costs context, maintenance and pipeline time, so every test must protect a behaviour that matters and run fast; nothing for the sake of coverage.

Lay out tests so one module's or feature's tests run alone through a scoped command. Run only the tests for your change; the full suite belongs to the pipeline.

## Comments

A comment is re-read by every agent that passes through its file and costs context each time. Once the why, the what and the how have their homes, very few comments are legitimate.

- Comment only what cannot be understood from the code itself.
- Describe the code as it is now: no former behaviour, no plans.
- Keep comments within the scope where they appear: a comment inside a function describes only that function; a file-level comment describes only that file. Never reference code or behaviour outside that scope, including as a reason for the local implementation.
- What is in your context is not what is in the code. Never record a decision abandoned along the way, an alternative you considered, or what your change replaced: after replacing A with B, do not comment that A is not used.
- Create, update and delete comments in the same change that alters the code they describe.
- If you hesitate over a comment, leave it out.

## Documentation

Documentation explains the what, in plain human English, structured for a reader. Most work does not need documentation. Add documentation only when the information is important for future readers to understand or use the project; treat every addition, even a word, as a deliberate choice. Create a new document only when that information cannot be covered clearly in an existing one.

- Describe the present state: no history, no plans.
- What is in your context is not what the reader needs. Never document a decision abandoned along the way, an alternative you considered, or what your change replaced: after replacing A with B, document B and never mention A.
- Keep all documentation at one uniform level of detail. What you have just built looms large in your context; do not detail it more than the rest.
- Create, update and delete documentation in the same change that alters what it describes.
- If you hesitate over a sentence, leave it out.

Every document except ADRs is listed here. Read one only when the task needs it.

| Document | Covers |
| --- | --- |
| `docs/spec/00-charter.md` | The NaN 1.0 specification: its guarantees, glossary and the map of every chapter in `docs/spec/` |
| `docs/spec/39-primer.md` | The primer: what to read before writing or reviewing NaN code |
| `nanbench/results/` | The primer tiers' token counts, the measurements of the freeze and the nanbench metrics published with language 1 |

## ADR

Record every architecturally significant decision (structure, dependencies, interfaces, non-functional trade-offs) in `docs/decisions/NNNN-short-title.md`, NNNN incrementing. An ADR holds the why: exactly what is needed to understand the decision, nothing more.

```markdown
# NNNN. Title

Status: accepted | superseded by NNNN

## Context
## Decision
## Consequences
```

Never edit an accepted ADR beyond its status line; a new ADR supersedes it.

## Skills

Create a skill at `.agents/skills/<name>/SKILL.md` only for a task agents will repeat frequently over the life of the repository and that a written procedure makes faster or safer. Keep it current; delete it when the task disappears.

## AGENTS.md and README.md

You may edit this file only to: refresh the project description when it is no longer accurate (never to add detail); update Architecture and Layout when a change alters them; add, update or remove commands; add, update or remove a documentation entry. Everything else here is owned by the user.

README.md is the landing page for humans: description, installation, usage, commands, in clear sections. Keep it accurate and strictly minimal.
