# 0034. Machine test runs boot the emulator

Status: accepted

## Context

[0024](0024-test-runs-as-processes-of-a-test-image.md) runs each test run as a process of a test image, named on its command line, writing framed records to standard output. A test image for `x86_64-uefi` or `x86_64-bare` is no process: it boots a machine, has no command line, and its only output is the serial port ([emulator runs](../spec/22-testing.md#emulator-runs)). A run must still depend only on its index and seed, end without disturbing other runs, and report a pass, a trap, an escaped failure, a stall or a timeout.

## Decision

- **One boot per run.** Each run boots the emulator on the package's test image, on `x86_64-uefi` from a volume made once per test image whose writes, the firmware's included, each boot drops. The toolchain passes the texts naming the run, as a host run's arguments, in the emulator's configuration file `opt/nan/run`, which the image's entry seam reads through the configuration device's ports.
- **Frames on the serial port.** The image writes a start marker, then the same framed records as a host run, to the serial port. The firmware's own output before the marker is not part of the run.
- **The outcome frame ends the run.** After its outcome frame the image writes 0 to the exit port, which ends the emulator; the toolchain reads the outcome from the frames, never from the emulator's exit status.
- **Processor time bounds the boot.** The toolchain reads the emulator's processor time from `/proc` while it runs and ends a boot that uses more than 10 seconds of it as a timeout, as it ends one outlasting a wall-clock bound that only a machine waiting forever reaches.
- **The seeded scheduler unchanged.** The machine test image keeps the host's seeded scheduler, sims and one executor, with maskable interrupts disabled, so a run's schedule is the same on every target.

Alternatives rejected:

- **One boot for all runs of a test:** a trap or a hang in one run would end the others, and the machine would need resetting between runs.
- **Runs on the host through `hw::sim`:** kernel code would never meet its seams, stubs or descriptor ([0002](0002-bootstrap-stack-and-lifetime.md)).
- **A disk image carrying the run's texts:** building a volume per run costs more than a configuration file, which QEMU offers on both targets.
- **A wall-clock bound alone:** a run's outcome would depend on the host's load.

## Consequences

- A machine run costs a boot of the emulator, and of OVMF on `x86_64-uefi`, whose processor time counts towards the run's bound.
- A run's timeout does not depend on how many runs execute at once, but a machine waiting forever ends only at the wall-clock bound.
- Kernel tests never run under interrupts or on more than one CPU; the M4 scenario's programs exercise those.
- A machine test image reads its run from the emulator's configuration device, so it runs only under the emulator.
