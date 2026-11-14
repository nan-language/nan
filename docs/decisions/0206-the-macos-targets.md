# 0206. The macOS targets

Status: superseded by 0208

## Context

Agents and the people they work for run macOS as well as Linux and Windows, on Apple silicon and on Intel Macs, and a host program should be checkable for macOS without rewriting it. The host targets were those of Linux and Windows ([0201](0201-host-targets-named-after-their-system.md), [0202](0202-the-windows-targets.md)). macOS has no stable system call interface: a macOS program calls the functions `libSystem` exports, waits on kqueues and takes faults as signals. Apple's AArch64 calling convention is not the Procedure Call Standard the Linux targets use: it reserves `x18`, has the caller extend an argument narrower than 32 bits, aligns stacked arguments to their own size and passes variadic arguments on the stack.

## Decision

- **Two host targets of the kind `macos`.** `x86_64-macos` and `aarch64-macos` are concrete targets, selected by the variant selector `macos`. Their programs take `host::World` with the host entry heads, and the rules that name the host targets hold on them, so a host program checks on macOS as on Linux and Windows.
- **`"darwin"` on both macOS targets.** It names the convention of macOS for the target's architecture: the System V AMD64 one on x86_64 and Apple's on AArch64. A library declaring `libSystem` functions then holds on both macOS architectures, as one declaring Windows functions does under `"win64"`.
- **No image is built yet.** A final build of a macOS program is `target.image-unavailable`, reported before checking, and every test run on a macOS target fails with `test.emulator-unavailable`. Their descriptors name no platform package.

Alternatives rejected:

- **`"sysv"` on the macOS targets:** on AArch64 it names the Procedure Call Standard, under which a call of a `libSystem` function would pass narrow, stacked and variadic arguments where Apple's convention does not read them.
- **`"sysv"` on `x86_64-macos` and an Apple convention on `aarch64-macos`:** every library declaring `libSystem` functions would need a variant module per architecture.

## Consequences

- Host programs and libraries declare the macOS targets and are checked against them; they build and test nowhere.
- A library declaring Linux and macOS targets holds each system's foreign fns in its variants ([0204](0204-calling-conventions-judged-per-variant.md)).
