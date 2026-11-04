# 0120. Language 1 and the bootstrap freeze with the NaN toolchain 1.0

Status: accepted

## Context

Language 1 was to freeze once the six milestones passed on the bootstrap ([0013](0013-language-cost-and-scope.md)), and the bootstrap was described as frozen already ([0050](0050-host-support-routines-of-the-nan-lowering.md), [0052](0052-the-source-tree-hash-recorded-in-the-image.md)). Yet the bootstrap implements all of language 1 ([0002](0002-bootstrap-stack-and-lifetime.md)), and language 1 keeps gaining what NaN OS needs, each change landing in the bootstrap and ported to the NaN toolchain. Passing the milestones shows the language can build a loader, a kernel and a compiler, not that it holds everything NaN OS needs.

## Decision

- Language 1 and the bootstrap freeze together when the NaN toolchain 1.0 is released.
- The release requires the six milestones to pass on one build of the bootstrap and every bounded measurement to meet its bound on that build.
- Until then, each change to language 1 lands in the bootstrap and the NaN toolchain in the same change. After it, the bootstrap does not change, and language changes land only in the NaN toolchain, under later language versions.

## Consequences

- Reasons resting on the bootstrap being frozen hold only from the NaN toolchain 1.0.
- The bootstrap keeps its Rust toolchain, Cranelift and lowering conventions until then, so the self-check and the double-compiling check keep comparing the NaN toolchain with it.
