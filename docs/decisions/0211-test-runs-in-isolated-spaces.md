# 0211. Test runs in isolated spaces

Status: accepted

## Context

NaN OS runs the NaN toolchain as the job of an isolated space and runs `nan test` and `nan verify` there to admit packages. A test run is a process of the system running the toolchain or a boot of the emulator, and a job in an isolated space can start neither: its platform library started no program, and every run on a nanos target was `test.emulator-unavailable`. Every run must be on the seeded scheduler, so a test cannot reuse the kernel's user-mode runtime, which bundles import. The interpreter, the one way the toolchain runs code in its own process, does not run awaited code.

## Decision

- **A nanos target's test image is a bundle that runs alone in an isolated space.** It is lowered as the user-mode runtime is, with the seeded scheduler. It imports nothing and names no kernel's interface, so the job does not need its kernel's interface file. Its start takes the texts naming the run from a start record, writes its frames to its standard output through the space's calls, and ends its space with its exit status.
- **Every isolated space can ask its kernel for test runs**, through a capability at a fixed handle of its table. `run_isolated` reads the image from the job's file system, maps it with `sys::load::Isolated.of_test`, runs it in a new space of the job's frames, and writes its standard output to a file the request names. It ends the run after 10 seconds of the kernel's clock. A test space holds no capability, and the job could already run any user-mode code in its own space, so the run grants nothing new and needs no signature or opt-in.
- **On NaN OS the toolchain's system is the nanos target of its architecture.** `any` tests run there, and its platform's `run_output` makes a test run of the program it is given. On every other system, nanos runs stay `test.emulator-unavailable`.

Alternatives rejected:

- **Interpreting awaited code on the seeded scheduler:** the interpreter would need tasks, cowns and the simulated world, and its outcomes would have to match compiled runs. It would also leave nanos-target packages untestable.
- **Test bundles built against the running kernel's interface:** they would run on the kernel's real scheduler, and the job would need the kernel's interface file.
- **Shipping test output back in the call's result:** a call copies at most 1 MiB, while a test trace has no bound.

## Consequences

- A NaN OS kernel serves test runs by running `run_isolated` as it is. The space's channels move from handle 4 to handle 5.
- The bootstrap never runs as such a job, so only the NaN toolchain makes nanos test runs; the self-check compares its `nan test` in `tests/nanos_toolchain`'s kernel with the bootstrap's on Linux.
