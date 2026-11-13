# 0202. The Windows targets

Status: accepted

## Context

Agents and the people they work for run Windows as well as Linux, and a host program should be checkable for either system without rewriting it. The host targets were the Linux ones ([0201](0201-host-targets-named-after-their-system.md)). Windows has no stable system call interface, no io_uring and no signals: a Windows program calls the functions its system libraries export, waits on I/O completion ports and takes faults as exceptions. A Windows image therefore needs a runtime, a platform package and an image format of its own, which the bootstrap and the NaN toolchain must both implement before either builds one.

## Decision

- **Two host targets of the kind `windows`.** `x86_64-windows` and `aarch64-windows` are concrete targets, selected by the variant selector `windows`. Their programs take `host::World` with the host entry heads, and the rules that name the host targets hold on them, so a host program checks on Windows as on Linux.
- **`"win64"` on both Windows targets.** It names the convention of 64-bit Windows for the target's architecture: the Microsoft x64 one on x86_64, and on AArch64 the Procedure Call Standard with `x18` reserved for the platform. A library declaring Windows functions then holds on both Windows architectures, as one declaring C functions does on both Linux ones under `"sysv"`. `x86_64-uefi` keeps `"win64"` for its firmware, and `"aapcs64"` stays the firmware convention of `aarch64-uefi`.
- **No image is built.** A final build of a Windows program is `target.image-unavailable`, reported before checking as `target.bundle-interface` is for a nanos program, and every test run on a Windows target fails with `test.emulator-unavailable`. Their descriptors name no platform package.

Alternatives rejected:

- **One kind `host` covering both systems:** a variant selector could not tell Linux code from Windows code, and the rules that hold only for Linux processes (glibc, ELF, io_uring, signals) would need a second dimension beside the kind.
- **`"aapcs64"` on `aarch64-windows`:** every library declaring Windows functions would need a variant module per architecture, and the variants of a `pkg` or `pub` item must share its interface, convention included.

## Consequences

- Host programs and libraries declare the Windows targets and are checked against them; they build and test nowhere.
- `"win64"` passes arguments differently on each architecture, as `"sysv"` does.
