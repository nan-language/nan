# 0150. Packages declaring `any` judged within the reach of final builds

Status: accepted

## Context

The rules of a target's environment (`suspend.stack-missing`, `kernel.float-use`, `register.port-target`, `hw.intrinsic-target`, `component.unavailable`) judge code of a package declaring `any` only within the reach of each final build for that target. A package declaring `any` was checked under `any` alone, and a final build lowered its reached items under the program's target without reporting what judging them there finds, so a kernel's build accepted a `task fn` body without `limits stack N` passed to `sys::Domain.run_sized`, and a float operation reached the code generator as an internal error. `nan dep sync` judged a stored release whole under the target and rejected the same code, so the commands disagreed. NaN OS also asked whether `run_sized` should run a recursive body in a kernel domain, as chapter 11 promised.

## Decision

- **The check of a closure under a concrete target judges the reach of each program it checks.** From the program's entry items and the admitted contributions to platform registries, it follows the items each reached item's NIR unit names, the impl lines of each reached type and the admitted contributions to each reached registry, and judges under the target each item of a workspace package declaring `any` it reaches. The carried std and quiet releases add only themselves: release checks judge releases whole. `nan check`, `nan dep sync` and final builds report what that judging finds, and a package declaring `any` reports unused code only under `any`.
- **Units, not the names relation, give the reach.** The names relation over-approximates member calls by every function of the member's name, which is harmless for test reaches and unused items but would report errors in code no build lowers.
- **Machine targets keep static stack bounds.** A `run_sized` or `spawn_sized` body there states `limits stack N` like every task entry; unbounded depth runs only where an overflow traps or the stack grows, on the host and nanos targets.

Alternatives rejected:

- **Guard regions on machine stacks:** kernels map their memory with 1 GiB and 2 MiB pages from a root that every CPU and space shares, so a guard page per sized stack would split those pages and shoot down every CPU's TLB at each run and release; the fault on a full stack needs an exception stack of its own and must be recognised before the kernel's own bound page-fault vectors; and lowered code for those targets would need frame probes in both code generators.
- **Judging packages declaring `any` whole under each target of their consumers:** a library shared by a kernel and a host tool could not hold host-only code.

## Consequences

- Checking a closure under a concrete target elaborates the items its programs reach in packages declaring `any` once more, under that target; closures without such packages pay nothing.
- A check of a scope lying in a package declaring `any` runs under `any` alone, so it does not report what a consumer's final build reaches; checking the workspace or the program does.
- Recursion in a kernel needs a bound the stack judge can see; growing machine-target stacks at cycles, as [0117](0117-stacks-grown-at-cycles.md) does for nanos bundles, would be the way to lift that.
