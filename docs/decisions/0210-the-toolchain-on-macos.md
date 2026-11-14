# 0210. The toolchain on macOS

Status: accepted

## Context

[0208](0208-macos-images.md) builds macOS images, but the NaN toolchain reaches the system it runs on through `toolchain/platform`, whose module `os` had variants for Linux and Windows only, and the release published Linux and Windows toolchains. A foreign fn's symbol is its name, while `libSystem` exports on x86_64 the `stat` and `readdir` families with 32-bit inode numbers under their plain names and their current forms under names ending in `$INODE64`, and the conforming `realpath` under `realpath$DARWIN_EXTSN`. macOS has no `pipe2` and no `execvpe`, and a NaN process on macOS ignores the termination request signals, which the programs it starts inherit. GitHub offers Mac runners of both architectures.

## Decision

- **An `os@macos` variant of `toolchain/platform` over `libSystem`**, declaring both macOS targets, through foreign fns of `"darwin"`.
- **Calls whose layouts are the same on both architectures:** metadata through `getattrlist`, directory entries through `getattrlistbulk`, a canonical path through `fcntl` with `F_GETPATH`, sizes through `lseek`, none of which `libSystem` exports under suffixed names.
- **Every program started through `posix_spawnp`:** `POSIX_SPAWN_CLOEXEC_DEFAULT` closes in it each descriptor its file actions do not give it, in place of `pipe2`'s close-on-exec pipes; `POSIX_SPAWN_SETEXEC` replaces this process, in place of `execvpe`; and the termination request signals start at their default action with `SIGPIPE`.
- **Each macOS toolchain builds on a Mac runner of its architecture**, in CI and in the release, as each Windows one does on Windows: the bootstrap builds the first stage, the second and third are equal byte for byte, and the bootstrap's stage comparison reads the second's Mach-O section `__nan`. CI also runs the macOS programs there.

Alternatives rejected:

- **`stat`, `readdir` and `realpath` with an architecture-specific layout or symbol name per call:** x86_64 would get the 32-bit inode forms, and a foreign fn cannot name the suffixed ones.
- **Cross-building the macOS toolchains on Linux:** their double compile and their programs need a Mac to run.

## Consequences

- The release publishes `nan-<version>-<arch>-macos` for x86_64 and AArch64, which `install.sh` installs on a Mac.
- `canonicalize` on macOS opens the path, so it needs the right to read what it names.
- A toolchain on macOS resets `SIGHUP`, `SIGINT` and `SIGTERM` to their default action before `run_attached` runs a program, so that a termination request ends both.
