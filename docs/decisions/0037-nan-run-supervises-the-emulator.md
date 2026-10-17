# 0037. nan run supervises the emulator

Status: accepted

## Context

Under the emulator, `nan run` ends with the byte the machine writes to the exit port, with 128 + n after a termination request of signal n, and with 70 after any other end ([commands](../spec/33-toolchain.md#commands)). QEMU's exit device ends QEMU with status (2v + 1) mod 256 after a write of v, and QEMU ends with 0 when a signal stops it. A `nan` that replaced itself by the emulator, as it does with a host image, would end with those statuses. Reading the emulator's end needs `nan` to stay its parent, and passing a termination request on to it would then need a signal handler, which safe Rust over the standard library cannot install ([0002](0002-bootstrap-stack-and-lifetime.md)).

## Decision

- `nan run` of a machine program starts the emulator as a child and ends with the status its end gives: v mod 128 after an exit status (2v + 1) mod 256, and 70 after any other end.
- Every emulator the toolchain starts runs with `-run-with exit-with-parent=on`, so it ends when the process that started it ends. `nan run` installs no signal handler: a termination request ends it with status 128 + n, and its emulator with it.
- The emulator is available only when it offers that option; `nan run` of a machine program ends with status 70 without starting anything when it is not.
- A host image still replaces `nan`, which passes termination requests on and ends with the image's status.

Alternatives rejected:

- **A signal-handling dependency or unsafe code:** contrary to 0002, for what the emulator offers itself.
- **Replacing `nan` with the emulator:** the run's status would be QEMU's.
- **Changing the statuses in the specification:** an emulator's encoding would shape what every program's run reports.

## Consequences

- The toolchain needs QEMU 10.2 or later, the first release with `exit-with-parent`; CI runs on Ubuntu 26.04 for it.
- An emulator ends when the thread that started it ends, so the thread starting one waits for it.
- An emulator that fails after it starts ends with status 1, which reads as a write of 0; the availability check rules out the failures the toolchain can foresee.
- Test runs and the milestone harness leave no emulator behind when their process ends.
