# Self-hosting and milestones

This chapter defines the bootstrap and the nan front end, the milestones on which language 1 freezes, and the measurements the freeze requires.

## Bootstrap

The **bootstrap** is the [implementation](00-charter.md#conformance) written in Rust. It elaborates source to [NIR](19-nir.md#nir) and [lowers](30-artefacts.md#lowering) NIR to machine code through Cranelift.

- The bootstrap implements all of language 1: every rule on every [target](27-program-model.md#targets), every [command](33-toolchain.md#commands), the [prover](20-prover.md#procedure) and the [interpreter](21-interpreter.md#interpreter). It passes every conformance test.
- The bootstrap implements language 1 only; no later language version applies to it. It is the independent compiler against which the first toolchain built by itself is checked by [stage comparison](36-builds.md#stage-comparison).
- Cranelift emits the objects of lowering, freestanding for `x86_64-uefi` and `x86_64-bare`, each function in its [calling convention](23-low-level.md#calling-conventions). `cc` links `x86_64-host` [images](27-program-model.md#images) from them, and the bootstrap's own image writer links the others.
- On `x86_64-host`, the bootstrap's `stack_switch` [support routine](27-program-model.md#runtime-support) switches task stacks with Cranelift's stack switching.
- Cranelift is the backend whose reported frame sizes the [stack limits](10-effects-digests.md#stack-limits) check reads.

### Nan front end

The **nan front end** is a program written in nan that implements `nan check`: lexing, parsing, every checking rule, elaboration to NIR, the prover and the interpreter. It targets `x86_64-host`, is built by the bootstrap and implements no other command.

- For every workspace and scope, the nan front end prints, byte for byte, the output of the bootstrap's `nan check`.
- For every item and target it checks, it produces NIR whose [canonical encoding](19-nir.md#encoding) is identical to the bootstrap's when both carry the same [advisories](37-evolution.md#prover-monotonicity).

## Milestones

A **milestone** is one of M1 to M6, each a set of checks run on the bootstrap. Language 1 **freezes**, becoming the fixed definition of nan that version 1 names ([language versions](37-evolution.md#language-versions)), when all six milestones pass on one build of the bootstrap and every [measurement](#measurements) with a bound meets it on that build.

A **kernel conformance test** is one whose `nan` block, or for a `text` block the nearest earlier `nan` block of its section, has a package declaring `x86_64-uefi` or `x86_64-bare`; the others are **host conformance tests**. A conformance test passes when every run of the [harness](00-charter.md#marker-comments) gives what its marker comments state.

A **scenario** is a workspace written for a milestone with the behaviour the milestone states for it. It passes when `nan verify` accepts the workspace ([release gate](31-trust.md#release-gate)) and the behaviour occurs: in M2 and M6 as a test of the workspace asserts, in M3 and M5 in the output of the stated commands, and in M4 in what each program writes to the serial port under `nan run`, which the harness ends by a termination request once that output shows the behaviour, without a [kernel halt](24-kernel.md#kernel-halts).

| Milestone | Passes when |
| --- | --- |
| M1: front end | For every conformance test, `nan check` reports what its marker comments state and the `nan fmt` condition of [marker comments](00-charter.md#marker-comments) holds. Every exact fix offered in a conformance test re-checks as [exact fixes](34-diagnostics.md#exact-fixes) require. The [completeness test](33-toolchain.md#completeness-test) passes. |
| M2: host programs | Every host conformance test passes, except the `nan verify` runs and the `text` blocks holding commands. In a scenario, the body of `sys::Domain.run` traps while a task of its domain has a `sys::net::Conn.read` in flight; the test asserts that `run` returns `Err` ([settlement](11-errors-domains.md#settlement)), then connects again and completes a read. |
| M3: proof and packages | Every host conformance test passes. Each obligation of the host conformance tests and the M2 scenario has the same outcome with and without the [proof cache](20-prover.md#proof-cache). In a scenario of 2 packages, one naming the other by a store name of the [local store](29-identity-versioning.md#local-store), `nan publish --local` publishes 2 releases of the dependency, then `nan dep sync` locks the second. `nan merge` of a file holding an enum with 2 versions of it, each adding a distinct variant, reports no conflict ([order-free members](33-toolchain.md#order-free-members)). |
| M4: kernel | Every kernel conformance test passes. An `x86_64-uefi` loader reads the image file of the M4 kernel from its boot volume through [firmware services](24-kernel.md#firmware-services) and copies each loadable segment to its physical address, zero-filled to its size in memory. An `x86_64-bare` kernel installs its [anchor](24-kernel.md#anchor), whose scheduler runs on every processor of the [emulator](27-program-model.md#descriptors); a task that never awaits is [preempted](24-kernel.md#preemption) and another task runs; a reader task is woken by a [signal](24-kernel.md#signals) that the 16550 UART's interrupt fn posts on receiving, in loopback mode, a byte the kernel wrote; and a driver of a virtio entropy device keeps its virtqueue in `hw::DmaShared` memory ([DMA](23-low-level.md#dma)) and receives bytes through it. |
| M5: self-check | The nan front end meets every requirement of [nan front end](#nan-front-end) on its own workspace, on every [std package](28-stdlib.md#std-layers) and on the `nan` block of every conformance test, and reports no diagnostic on its own workspace. |
| M6: component | In a scenario, a [component](26-components.md#components) runs on `host::Runner`, its Env holding a `sys::net::Listener` in an `Option` field. A handler moves the listener out with [`Option.extract`](28-stdlib.md#option-and-result-members) into a [keyed task](26-components.md#keyed-tasks), which accepts a connection, owns it while reading it until the peer closes it, and posts each read's byte count through its outbox. The runner's body connects, writes bytes, closes and sleeps for 1 ms; the test asserts that `run` returns a state counting every byte written. |

## Measurements

A **measurement** is a quantity taken, on the build of the bootstrap on which the milestones pass, over what its row names: for milestones, the workspace packages of their scenarios, never std. Times are taken on the [nanbench](37-evolution.md#nanbench) reference machine, those of M4 under the descriptor's emulator running on it. Every measurement is published with language 1, as [nanbench](37-evolution.md#nanbench) states.

| Measurement | Over | Bound |
| --- | --- | --- |
| Share of bounds and overflow [obligations](18-contracts.md#obligations) that the prover proves | M4, and M5 separately | at least 90% for each |
| Predicate [run-time checks](18-contracts.md#run-time-checks) kept inside their defining modules ([module boundary](04-types.md#module-boundary)) | M4, M5 | none |
| Items whose code names `mem::Arena` ([arenas](12-memory.md#arenas)) | every milestone | none |
| [Ledger](31-trust.md#ledger) entries, per kind and package | every milestone | none |
| Deepest stack use of each task against its [task stack](17-suspension-io.md#task-stacks) size | M2, M4 | none |
| Time of an allocation through `mem::Alloc`, its cell charge included | M5 | none |
| [Waivers](06-statements-expressions.md#waivers), per threshold, and the [closure radius](33-toolchain.md#closure-radius) of each item | M4, M5 | none |
| Parameter lists with more than 4 parameters, `self` included ([line breaking](35-format.md#line-breaking)) | M5 | none |
| Task closures outside [lexical spawns](16-concurrency.md#joins), with their `take` captures | M2, M6 | none |
| Time of the extended-state save of a [machine seam](23-low-level.md#machine-seams) and of `hw::switch_to` ([floating point](24-kernel.md#floating-point)) | M4 | none |
| Time of a generation check in a [capability table](14-authority.md#capability-tables) | M2, M4 | none |
| Nanbench's metrics | the bootstrap | none |
| Token count of each [primer tier](39-primer.md#primer-tiers) | the primer | its cap |
