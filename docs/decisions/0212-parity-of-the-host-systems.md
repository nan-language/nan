# 0212. Parity of the host systems

Status: accepted

## Context

Linux, Windows and macOS became host systems one after another, and each later system copied what the one before it had, not what Linux had. Linux built and released its toolchain from jobs of its own in the CI and release workflows, while Windows and macOS each had a workflow both call. Each system had its own copy of the program exercising files, streams, sleeps, deadlines, random bytes, loopback connections and traps; only macOS ran it on both architectures, Windows ran it under Wine on x86_64 Linux alone, and AArch64 Linux had no such program. Parity could be extended to everything, but most of the bootstrap and the toolchain behaves alike on every system.

## Decision

- **Every host system has the same structure:** a module of its own wherever code differs by system (the bootstrap's lowering, the toolchain's lowering and platform library, std's `<system>_platform`), and one workflow, which CI and the release both call, building, double-compiling and releasing its toolchain on a runner of each architecture.
- **Code specific to a system runs on that system, on each architecture released.** The program `hosted` of `tests/programs` holds one source and one expected output per host target, and each system's workflow runs it on its runner's architecture. The program harness compiles and runs on Windows, so Windows programs run there rather than under Wine.
- **Code that behaves alike on every system is tested once, on Linux:** the unit tests, the conformance, milestone, measurements and self-check harnesses, and nanbench.
- **Every system installs with one command**, `install.sh` on Linux and macOS and `install.ps1` on Windows. Package managers' channels follow demand, not symmetry: a system gains one when its users ask for it.

Alternatives rejected:

- **Every harness on every system:** CI would cost three times as much without running any more system-specific code.
- **Wine as the Windows run:** the programs would never run on Windows, and AArch64 ones nowhere.
- **One copy of the program per system:** the copies drift, and a system without one goes unnoticed.

## Consequences

- A new host system brings its module in each of those places, its workflow and an expected file of `hosted` per target.
- Machine programs boot only where the program harness reaches the emulator's monitor, on Linux and macOS.
- CI's Windows jobs install cargo-nextest, and each Linux job runs its architecture's Linux programs.
