# 0044. Nanbench and the measurements of the freeze

Status: accepted

## Context

Language 1 freezes on measurements taken over the milestones' scenarios and published with it ([measurements](../spec/38-self-hosting.md#measurements)), among them nanbench's metrics and each primer tier's token count under three tokenizers ([nanbench](../spec/37-evolution.md#nanbench)). Nanbench did not exist. Most measurements read what only the bootstrap knows: obligation statuses, typed closures, the frames of a final build. Some are times of runtime mechanisms that no NaN program isolates by itself: no NaN code issues the instruction that saves extended state. Measurements are taken once per language version on one machine, and a model run costs money and needs an account; neither belongs in the test suite.

## Decision

- **A crate beside the bootstrap.** `nanbench/` is a Rust binary of the Cargo workspace that reads the bootstrap through its library: `nan::measure` for what checking gives, `nan::task_stacks` for the stack use of each task a final build or test image starts, judged as a `stack N` demand is, and `nan explain` and `nan check --radius` for the ledger and the closure radii. Each command writes its results under `nanbench/results/`, which the repository keeps.
- **Benchmark programs for times.** `nanbench/bench/` holds a host program timing allocations and generation checks, a bare kernel timing generation checks, seams and thread switches under the descriptor's emulator with the HPET, and a C program timing `xsave64` and `xrstor64`, which a guest under KVM runs natively. All run on 8 hardware threads, the reference machine's count.
- **Tokenizers.** `o200k_base` and `cl100k_base` come from `tiktoken-rs`, which holds OpenAI's ranks. Llama 3's ranks are fetched once from Meta's repository at a fixed commit and checked against their SHA-256, so the repository never redistributes them.
- **Models through a text protocol.** A model gets the `nan primer` core and a protocol of write, run, answer and submit blocks, and its `nan` commands run in-process in a fresh copy of its task's workspace. Models are reached through the Messages API with `curl`, the key read from the environment and given on `curl`'s standard input, never as an argument. A reference solver that submits each task's solution checks the corpus and the harness without a model.
- **Bounds in the suite.** The measurements harness asserts the 90% discharge bound on M4 and M5 at every run; `nanbench tokens` fails when a tier passes its cap.

Alternatives rejected:

- **A Python harness**, the usual home of tokenizers and model clients: a second toolchain in the repository, while the measurements need the bootstrap's internals in-process anyway.
- **Driving models through an agent CLI**: it adds its own system prompt and tools, so a model would learn from more than the primer and the toolchain's output.
- **Instrumenting the kernel runtime to time its own save**: a measurement build of the runtime would no longer be the build on which the milestones pass.
- **Committing Llama 3's ranks**: its licence binds every redistribution.

## Consequences

- Taking the measurements needs QEMU with KVM, `cc` and network access once; a model run needs an API key, so its metrics are published only once someone with a key has run it.
- A seam's round trip and a thread switch are timed under the emulator with what virtualisation adds; the save itself is timed natively, as the guest runs it.
- The bootstrap's lowering records each task entry it writes, for `nan::task_stacks`.
