# 0058. Concurrent runs in the self-check

Status: accepted

## Context

The self-check harness ran each comparison's two toolchains one after the other, and its toolchain test ran its steps in sequence: the image build, the check, NIR and `nan verify` comparisons, the second and third stages' builds, then the stage comparison ([0047](0047-diverse-double-compiling-of-the-nan-toolchain.md)). nextest started that test last, as it lists tests by name, so the full suite took about 90 minutes, 70 of them in that test. Most of these steps read nothing another one writes: the NaN toolchain runs on a copy of the bootstrap's workspace made before either runs, and each stage builds in a workspace and user directory of its own. What only the frozen bootstrap can shorten is its own `nan check` of the toolchain workspace and the `nan verify` that follows it, the latter about seven minutes.

## Decision

- **Each comparison runs both toolchains at once.** The harness copies the workspace and local store, starts the NaN toolchain on the copy, then runs the bootstrap in-process, and compares once both have ended. A NaN toolchain still running when a step fails is killed.
- **The toolchain test runs the bootstrap's commands from its start.** A thread runs the bootstrap's `nan check` then `nan verify` of the workspace, copying the workspace and local store before each command for the NaN toolchain and after it for the comparison of the files left. Once the image is built, the NaN toolchain runs each command on its copy, after the one before, and the harness compares what both gave; the NIR comparison runs on the copy of the workspace the bootstrap's check left, beside its `nan verify`. Meanwhile the first stage builds the second, then the second builds the third while the bootstrap runs the stage comparison. Every comparison of 0047 and of the commands is made, on the same states, and each stage keeps a user directory of its own.
- **The runs of a harness process spread over its processors.** Each run of the NaN toolchain a process starts takes the next processor of its affinity set, so that the runs one test starts at once do not share a processor.
- **The toolchain test starts first.** nextest gives it the highest priority and four threads, the processes it runs at once, so that the other tests fill the remaining threads around its critical path, the bootstrap's side.
- **Lint and tests run as two CI jobs**, each with a build cache of its own, so that the lint does not lengthen the test job.

Rejected alternatives:

- **Building the tests with `opt-level` 3:** the in-process bootstrap runs 15 to 20% fewer instructions for checks and builds, while a cold build of the tests takes longer than the critical path gains.
- **One test per example of a chapter:** a process holds one local store for the in-process bootstrap, so a chapter's examples share a process.

## Consequences

- The toolchain test lasts about as long as the bootstrap's `nan check` and `nan verify` of the toolchain workspace together.
- It runs up to four processes at once, and its copies of the toolchain workspace take a few tens of megabytes.
- A failure is reported once the runs beside it end, up to the end of the bootstrap's command in progress.
- The internal-error test checks the toolchain workspace with an empty user directory of its own, since the toolchain test, now first, leaves the check of that workspace in the shared one.
