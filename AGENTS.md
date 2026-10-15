# AGENTS.md

## Project

NaN is a programming language made for agentic coding: performant, fast and safe, with strong versioning so that compiled NaN artefacts can reuse each other. It will compile itself and build NaN OS, an operating system for AI, from bootloader to user interface; its Rust bootstrap compiler, targeting Linux on x86_64, then stays frozen and serves only to verify the self-hosted compiler.

## Architecture

`bootstrap/nan` is the bootstrap compiler, a Rust binary named `nan` that implements the language specified in `docs/spec`. It generates machine code with Cranelift: the system `cc` links Linux x86_64 executables, and the compiler's own image writer builds UEFI and bare-metal images. Everything Linux-specific (linking, program entry, calls into the system) stays in one target module.

Its parser runs on tables that `build.rs` generates from the `ebnf` blocks of `docs/spec/41-grammar.md`, so the grammar changes only in the specification. Its checker computes every fact as a query over one item and one target, recording what each query reads; context packs come from those records. Once every package is checked, each item without errors elaborates to NIR, which the reference interpreter runs for examples and refutations and over which the prover decides each obligation; the rules needing them run then. Checking runs a const's evaluation when it first reads the const's value. The conformance harness runs every `nan` example of the specification against its marker comments and checks each item's context pack against what checking it reads.

Only what an agent needs to place a change correctly. The why is in ADRs.

## Layout

| Path | Holds |
| --- | --- |
| `bootstrap/` | The bootstrap compiler, one Cargo workspace crate per directory |
| `bootstrap/nan/src/` | Source of the `nan` binary; a module's unit tests live in `<module>/tests.rs` beside it |
| `bootstrap/nan/tests/` | The conformance harness |
| `docs/spec/` | The NaN 1.0 language specification, one chapter per file |
| `docs/decisions/` | Architecture decision records |
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
