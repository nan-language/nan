# 0036. The M4 scenario runs outside the working tree

Status: accepted

## Context

The milestone harness copies each scenario workspace into the target directory and runs its commands there ([0033](0033-harness-homes.md)). The target directory lies inside the bootstrap's git working tree, so `nan verify` treats the copy as a VCS workspace and applies the [author rule](../spec/31-trust.md#author-rule). The copy is untracked: every item and every seal belong to one uncommitted change of one author, so every seal breaks the rule. The M4 scenario needs seals: its kernel and loader reach devices, firmware and physical memory through trusted code and grants ([M4](../spec/38-self-hosting.md#milestones)). Its behaviour shows only on the serial port of programs that never end.

## Decision

- The harness copies the M4 scenario into a directory of the system's temporary directory, outside every git working tree, where the author rule does not apply; its seals are written once, outside a working tree, and committed with the scenario.
- It runs each program with `nan run` under the emulator, reads the serial port until each line of the behaviour appears, then ends the emulator with `SIGTERM`, the termination request, and fails on a line of a halt or trap report.
- The other scenarios keep their copies in the target directory.

Alternatives rejected:

- **A git repository made for the copy, with commits by two authors:** the harness would script git identities, and the scenario's seals would be written by the harness instead of the scenario.
- **Exempting the harness from the author rule:** the toolchain's output would depend on who runs it.
- **A scenario without trust escapes:** no kernel or loader reaches hardware without them.

## Consequences

- Changing an item a seal records, or a std item in its closure, lapses the seal; resealing runs `nan seal` in a copy outside the working tree.
- The M4 test needs QEMU and OVMF, as every machine test does.
