# 0114. Nanos targets checked without images

Status: accepted

## Context

NaN OS runs programs and components on its own kernel and names the targets `x86_64-nanos` and `aarch64-nanos`, which language 1 reserved. Its code is written and reviewed against those targets' rules: floats and lane types are allowed, a job's entry takes a typed input and gives a typed result, a host program may be built for them, and a root grant holds the capabilities a kernel gives. An image of those targets runs on a kernel's runtime, in its shared space or in a space of its own, through support routines the kernel exports; that binding needs threads owning extended state in kernel executors, capabilities that a kernel implements for separately built code, messages between closures and the user-mode runtime, none of which language 1 defines.

## Decision

- **The targets exist.** `x86_64-nanos` and `aarch64-nanos` are concrete targets of their architectures, of the kind `nanos`, selected by the variant selector `nanos`. Packages declare, cover and depend on them as on any target.
- **Their environment is a host's under a kernel's stack rules.** Code holds floats and lane types in the floating-point and vector registers, so `kernel.float-use` does not judge it, except an interrupt fn whose digest uses floats: interrupt fns run on a kernel's interrupt stacks, where no task's extended state is live. Every task and entry states its stack bound, and `host::Runner` is unavailable, as on the machine targets, since a kernel draws stacks of static size.
- **The entry has a job's form or a host program's.** A typed entry takes `nanos::World` and a taken input and gives a result, both `Encode`, failing and waiting as a host entry does; a host program's entry is accepted unchanged.
- **The root grant is `nanos::World`.** It holds a host program's capabilities, but the wall clock, and a `nanos::Caps` in place of arguments and environment: Copy capabilities held under text keys, read back only as the type they were held as, which lowering tells apart by a tag of the type's normalised text, so that separately built closures agree.
- **No image is built for them.** A final build for a nanos target is `target.image-unavailable`, and a test run on one fails with `test.emulator-unavailable`. Their descriptors name no platform package.

Alternatives rejected:

- **Keeping the reservation until images can be built:** NaN OS code could not be checked against the rules it will run under, and its entry heads and root grant would stay unwritten.
- **Images bound to the shared space alone:** the runtime binding would fix an ABI for floats, served capabilities and stacks in kernel executors before the isolated spaces that run the same images exist.
- **Lane-by-lane and bit-pattern floats as on the machine targets:** NaN OS code computes with floats and vectors, and the rules would then differ from those its images will run under.

## Consequences

- Programs and libraries for the nanos targets check, and a NaN OS kernel's packages can share libraries with them; they build and test nowhere.
- A nanos program's tasks and entry state stack bounds, even where a host program's need none.
