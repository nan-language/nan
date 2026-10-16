# 0024. Test runs as processes of a test image

Status: accepted

## Context

[Testing](../spec/22-testing.md) runs each test many times: up to 64 schedules, or 256 cases and a shrinking of up to 4,096 candidates. Every run must depend only on its index and seed, so that a replay token reproduces it, and must end in a pass, a trap, an escaped failure, a stall or a timeout without disturbing the other runs. Code under test is compiled as in a build and differs only through the capabilities it receives and the scheduler's choices. The [interpreter](../spec/21-interpreter.md) never runs tests, and `nan test --affected` must know, across invocations, which tests passed against which reach.

## Decision

- **One image, one process per run.** A package's tests for a run target lower into one test image. Each run is a process of it, given its test, index, seed, replay token and any forced draws on the command line. It writes its test trace, its draws and its outcome to standard output as framed records, and a CPU limit ends a run that does not finish.
- **Seeded executor in the image.** The test image's support routines replace the io_uring executor with the seeded scheduler and its virtual clock, and the capability operations with sims, generated as IR like every other routine ([0023](0023-host-runtime-and-domain-ownership.md)).
- **Draws recorded, shrinking outside.** The image holds each property test's constraints as tables its draw routine narrows bounds from. A run records every draw with its bounds; shrinking reruns the image with forced draws, and the toolchain rebuilds the setup forms from the draws it kept.
- **`dbg` as an instruction.** `dbg` is a NIR instruction, so that the interpreter, the prover and the lowering each give it its meaning, and only test images write its line.
- **Pass records compared whole.** A pass record holds the toolchain hash and the canonical path and item hash of each item of the test's reach. A test is unaffected when its record equals the one its current reach gives, so the toolchain never decodes a record.

Alternatives rejected:

- **All runs of a test in one process:** a trap's settlement, a crash or a timeout in one run would end or taint the others, and the runtime would need resetting between runs.
- **Threads with recorded schedules:** a run would depend on the machine, so a replay token could not reproduce it.
- **Shrinking inside the image:** the image would carry the shrinking procedure and the setup forms, and a crashing candidate would end the shrinking.

## Consequences

- A run costs a process start, and the test image is built once for all runs of a package's tests.
- A run that ends without an outcome is an internal error of the toolchain.
- Build images never hold the seeded scheduler or sims, and test images never reach the host's network, files or clock.
