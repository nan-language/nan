# 0126. Kernel-served files and networks

Status: accepted

## Context

[0116](0116-nanos-jobs-in-a-kernels-shared-space.md) let a kernel serve `Log`, `Random` and `Stdio` to its jobs through capability kinds, but left `Fs` and `Net` denied on a kernel's runtime, since their files, listeners and connections are resources a bare kernel had no way to hold. NaN OS gives a job a view of a store directory as its file system and the egress gate as its network. A served resource must outlive the call that opened it, take the attenuations of the capability that opened it, and be released by settlement when its domain is abandoned, as a host resource is.

## Decision

- **`Fs.served` and `Net.served` take kinds implementing `FsService` and `NetService`.** Each interface mirrors the operations of its capability and of the resources it opens; a kind names each resource it opens by a `u64` it chooses, which every later call on the resource passes back.
- **Attenuation stays in std.** A served entry is an `Fs` or `Net` entry like a host one, the kind's handle in place of its directory or network and its method table beside its state words, so `subdir`, `read_only`, `only_host` and `revocable` derive from it unchanged and are applied before any method is called: a `Fs` method receives the path joined to its tree's prefix, and a refused operation reaches no kind.
- **A served resource holds its own record of its kind.** Its holding holds the kind's handle, method table, number and releasing method, so a release, an abandon and settlement call the kind even once the entry the resource was opened through is revoked, and settlement needs no knowledge of which interface opened it.
- **Resource methods have names of their own.** `NetService.reset` stands for `Conn.abandon`, whose name a method of a kind cannot take, and the release methods share one signature.

Alternatives rejected:

- **Resources of a kind's own type:** `sys::fs::File`, `Listener` and `Conn` are std base types whose layout std fixes, and jobs written against them must run unchanged on the host.
- **Composing `read_all` and `write_all` from `open`, `read` and `write` in lowering:** a loop growing `mem::Bytes` in Cranelift IR, written twice for the bootstrap and the toolchain, where a kind can do it in NaN.
- **Passing a kind its prefix and path apart:** every kind would join them, and one that forgets reaches outside its tree.

## Consequences

- Releasing a file or listener on any target calls a kind's method through code a 4,096-byte stack bounds, so dropping one counts that bound in its caller's stack.
- Jobs in isolated spaces still receive denied file systems and networks: their system calls carry no resources yet.
