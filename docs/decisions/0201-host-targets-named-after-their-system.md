# 0201. Host targets named after their operating system

Status: accepted

## Context

The host targets were `x86_64-host` and `aarch64-host`. Their programs run as Linux processes: their descriptors fix Linux, glibc, ELF and io_uring, and `host_platform` implements the sys capabilities through Linux system calls. NaN is to build programs for other operating systems, whose targets are host targets too: their programs run as processes and take `host::World`. A target named `host` names no operating system, so a second one could not be named beside it, and the variant selector `host` could not tell one system's code from another's.

## Decision

- **The Linux targets are `x86_64-linux` and `aarch64-linux`.** Their kind is `linux`, which the variant selector `linux` matches in place of `host`.
- **Their platform package is `linux_platform`**, named after its kind as `uefi_platform` and `bare_platform` are.
- **"Host target" keeps its meaning:** a target whose programs run as processes of an operating system and take `host::World`. Rules that hold for every such process keep naming the host targets, and `host::World` and `host::Runner` keep their names.

## Consequences

- A manifest declaring `x86_64-host` or `aarch64-host` fails with `manifest.unknown-target`, and a variant directory `name@host` matches no target.
- A host target of another operating system adds its own kind, selector and platform package beside the Linux ones.
