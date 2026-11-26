# 0236. The release gate against a kernel's interface file

Status: accepted

## Context

A nanos program using its kernel's capability kinds depends on the kernel's packages that define them through relative-path sources, so `nan verify` of the program gated those packages too: their API snapshots, seals and pending weakenings. The kernel's packages need not pass the gate, since the machine trusts them by running the kernel's image, so no such bundle could pass the release gate that NaN OS's admission runs. A bundle build already requires each package of the closure that the kernel's interface file lists to have the code root it records, so a bundle reaches exactly the kernel's code there.

## Decision

- **`nan verify <package> --against <interface file>`** reads the file as a bundle build does, refusing it with the same errors, and judges each package of the gate whose current state has the code root the file records under its name by that code root alone, as a named release is judged by its release check. Every other package passes the gate as before, one whose code root differs from the file's included.
- The form takes the packages a bundle build takes, and the same file, so a bundle checked by the gate is checked against the kernel it is then built against.

Alternatives rejected:

- **Sealing the kernel's packages:** it repeats the trust the machine already gives the kernel's image, and every kernel change touching them lapses the seals.
- **Publishing the kernel's packages as named releases:** publishing runs the gate on them, the same requirement.
- **Leaving kernel packages ungated by their target or a manifest directive:** a package a kernel shares with its bundles would then escape the gate wherever it is used, a policy the single gate rules out ([0011](0011-one-release-gate.md)); a code root ties the exemption to the kernel's exact code.

## Consequences

- A bundle passes the release gate with the kernel's packages unsealed, while its own packages and any it reaches whose code differs from the kernel's are gated as before.
- What the gate takes from the kernel is only as trustworthy as the interface file passed, which a kernel's final build writes beside its image.
