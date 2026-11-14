# 0205. The NaN toolchain on Windows

Status: accepted

## Context

Windows images build since [0203](0203-windows-images.md), but the NaN toolchain was released for Linux only, and the bootstrap, which roots every release's checks, ran on Linux alone. The toolchain's platform code called glibc and read `/proc` and `/etc/passwd`, and the program `nan` declares one target, so it holds no variant for another system: [0067](0067-the-nan-toolchain-on-aarch64.md) kept that code in the program, retargeted by copies of its manifest, and rejected a library of variants because a path dependency then needed `nan dep sync` in every user directory, which [0154](0154-path-dependencies-read-from-their-working-source.md) has since removed. The toolchain also reads its toolchain hash from its own image's `.nan` section, which Windows images did not hold.

## Decision

- **The toolchain reaches its system through a library, `toolchain/platform`,** declaring the four host targets: its root module holds what every system shares (errors, file kinds, process statuses, paths and texts), and its module `os` a Linux variant over glibc and a Windows variant over kernel32, which the checker keeps identical and checks on every check of the workspace ([0204](0204-calling-conventions-judged-per-variant.md)). `nan` depends on it by path. A copy of the manifest declaring a Windows target runs `nan dep approve`, since a lock approves only the escapes its own target reaches.
- **Paths keep `/` separators.** The Windows variant writes the paths the system gives with `/`, which Windows reads as `\`, so the toolchain joins and splits paths alike on both systems; text crosses the system as UTF-16.
- **A program's captured output goes through temporary files** that the system deletes once closed, since kernel32 cannot wait on several anonymous pipes; `nan run` waits for the program and ends with its exit code, Windows having no `exec`.
- **On Windows the user directory lies in the profile directory `USERPROFILE` names**, and its check rejects symbolic links and junctions only: reading access control lists takes `advapi32.dll`, which no foreign fn reaches.
- **Windows images hold the host sections** as discardable PE sections after the others, the tree section named `.nantree`, since a PE section name holds 8 bytes; stage comparison reads PE images.
- **A toolchain running on Windows runs Windows images as its system's processes**, `any` standing for the Windows target of its machine, and finds Linux images unavailable.
- **The bootstrap runs on Windows too.** What it does differently there, the user directory's owners and modes, the sync of a directory, handing its process over to a program and the signals ending a process, lies in its module `system`, the rest of it being the same on both systems.
- **Windows toolchains are built and released on Windows runners of each architecture, as the Linux ones are on Linux runners:** the bootstrap builds the first, which builds the second, the second builds the third into the same bytes, and the bootstrap's stage comparison accepts the second, which the release publishes. CI runs the same on every change. The repository keeps every file's bytes on checkout, so that a Windows checkout holds the code roots a Linux one does.

Alternatives rejected:

- **Variants inside the program `nan`:** its single target leaves every other variant unchecked, which the language forbids.
- **A package per system chosen by each manifest copy:** nothing would keep the two interfaces identical, and each copy would need its lock written anew.
- **Building the Windows toolchains on Linux under Wine:** each would be published without ever running on Windows, and the AArch64 one without running at all, since Wine there runs no AArch64 Windows image.

## Consequences

- A change to `toolchain/platform` needs `nan dep sync nan` before the release gate accepts the toolchain.
- The Windows toolchain trusts the access the profile directory grants rather than checking its cache's.
- A Windows toolchain cannot run Linux tests, and machine targets' runs need an emulator of its system.
- Releases and CI need Windows runners of both architectures, and a Windows-only path of the bootstrap is linted on Linux for the Windows target.
