# Measurements of the freeze

Taken by `nanbench measure` over the workspace packages of each milestone's scenario, M5's being the NaN front end's workspace. Times are taken on 8 hardware threads of a machine with AMD Ryzen 7 2700 Eight-Core Processor at up to 4.1 GHz, 16 hardware threads, 31 GiB of memory.

## Share of bounds and overflow obligations that the prover proves

| Milestone | Proved | Obligations | Share | Bound |
| --- | ---: | ---: | ---: | --- |
| M4 | 36 | 37 | 97.3% | at least 90% |
| M5 | 3984 | 4123 | 96.6% | at least 90% |

## Predicate run-time checks kept inside their defining modules

| Milestone | Checks | Sites |
| --- | ---: | --- |
| M4 | 0 |  |
| M5 | 0 |  |

## Items whose code names `mem::Arena`

| Milestone | Items |
| --- | --- |
| M2 | none |
| M3 | none |
| M4 | none |
| M5 | nan::cell::Swap, nan::cell::Swap.of, nan::check::CheckerState, nan::check::Listener, nan::check::Checker.new, nan::check::Checker.heard, nan::check::Reads, nan::check::Reads.empty, nan::check::Db.note, nan::check::Reads.on_line_opened, nan::check::Checker.record_label, nan::ty::Types, nan::ty::Types.empty |
| M6 | none |

## Ledger entries, per kind and package

| Milestone | Package | Entries |
| --- | --- | --- |
| M2 | settle | none |
| M3 | gauge | none |
| M3 | panel | none |
| M4 | kernel | grant 1, trusted 5 |
| M4 | loader | grant 1, trusted 2, validator 3 |
| M5 | nan | foreign 24, trusted 31, validator 1, waiver 19 |
| M6 | tally | none |

## Deepest stack use of each task against its task stack size

Each task's stack use is its entry's, judged as a final build judges a `stack N` demand; an unbounded one names the call that leaves it unbounded.

| Milestone | Task entry | Stack use | Task stack |
| --- | --- | ---: | ---: |
| M2 | settle/settle.nan:29 | unbounded: function value in sys::Domain.run at settle/settle.nan:15 | 65536 |
| M2 | settle/settle.nan:15 | unbounded: function parameter in task::scope at settle/settle.nan:16 | 65536 |
| M2 | settle/settle.nan:17 | unbounded: function value in sys::net::Conn.read at settle/settle.nan:21 | 65536 |
| M4 | kernel/kernel.nan:34 | 2752 | 32768 |
| M4 | kernel/kernel.nan:55 | 1520 | 8192 |
| M4 | kernel/kernel.nan:63 | 752 | 8192 |
| M4 | kernel/kernel.nan:67 | 1664 | 16384 |
| M4 | loader/elf.nan:131 | 944 | 65536 |
| M4 | kernel/kernel.nan:127 | 1056 | 65536 |

## Time of an allocation through `mem::Alloc`, its cell charge included

A box drawn through a heap over an allocator, every box kept so that the heap draws chunks and charges its cell, by an `x86_64-host` image.

| Milestone | Time |
| --- | ---: |
| M5 | 64.5 ns |

## Waivers, per threshold, and the closure radius of each item

Each item's radius is in `radii.tsv`.

| Milestone | Waivers | Items | Median radius | 90th percentile | Largest |
| --- | --- | ---: | ---: | ---: | --- |
| M4 | none | 91 | 300 | 2967 | 5954 (kernel::run) |
| M5 | length 19 | 5358 | 22894 | 257667 | 271692 (check::Checker.encoded_unit) |

## Parameter lists with more than 4 parameters, `self` included

| Milestone | Heads | Parameters |
| --- | ---: | --- |
| M5 | 557 | 285 with 5, 140 with 6, 81 with 7, 31 with 8, 12 with 9, 7 with 10, 1 with 11 |

## Task closures outside lexical spawns, with their `take` captures

| Milestone | Site | `take` captures |
| --- | --- | --- |
| M2 | settle/settle.nan:15 |  |
| M6 | tally/tally.nan:29 | listener |

## Time of the extended-state save of a machine seam and of `hw::switch_to`

Both save every state component with `xsave64` and restore it with `xrstor64`, which a guest under KVM runs natively: their native times are their times under the emulator. A seam's round trip, a self-interrupt sent through the local APIC and taken, and a thread switch, half of a round trip between two tasks each waking the other with a signal, are timed under the emulator.

| Milestone | Save | Restore | Seam round trip | Thread switch |
| --- | ---: | ---: | ---: | ---: |
| M4 | 52.9 ns | 95.8 ns | 4622.7 ns | 346.2 ns |

## Time of a generation check in a capability table

An operation failing at a stale capability's generation check: on `x86_64-host` for M2, on `x86_64-bare` under the emulator for M4.

| Milestone | Time |
| --- | ---: |
| M2 | 12.2 ns |
| M4 | 12.5 ns |

## Nanbench's metrics

In `nanbench.md`.

## Token count of each primer tier

In `primer.md`.
