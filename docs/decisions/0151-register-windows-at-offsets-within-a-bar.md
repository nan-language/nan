# 0151. Register windows at offsets within a BAR

Status: accepted

## Context

`Mmio.of_bar` gave one window per BAR, from its offset 0 ([0108](0108-io-domains-and-confined-device-grants.md)). Virtio 1.x over PCI places its common configuration, notifications, ISR status and device configuration at offsets its vendor capabilities give, usually in one BAR, and NVMe's doorbells sit at an offset too. A registers block has static offsets, so one window from offset 0 cannot follow offsets read at run time, and a driver needed `Mmio.map`, a trusted primitive, for every structure past the first. A window may also have overlapped the MSI-X table the runtime writes.

## Decision

- **One operation with an offset.** `of_bar(mut function, index, offset)` gives a window over R at `offset` of the BAR; offset 0 is the former window. A second name for offset 0 would add surface without meaning.
- **Bytes, not BARs, are taken.** A function records each window it gave, its BAR and the bytes R spans there, and refuses with `Taken` a window overlapping one of them or the function's MSI-X table or pending bit array, so disjoint windows over one BAR coexist and the runtime's MSI-X writes reach bytes no window maps.
- **Taken for the function's life.** `unmap` of a device window does nothing and knows no function, so a window's bytes stay taken until the function is released or cut off, which frees its window records.
- **Aligned to R's widest register.** Register offsets are multiples of their sizes, so an offset that is a multiple of R's widest register keeps every access naturally aligned; any other fails with `Misaligned`, as an unaligned access to device memory faults on AArch64. A supplied `Mmio.alignment` gives that size beside `Mmio.extent`.
- **Offsets from configuration space.** `config_read` already reads the capability list, and a wrong offset can at worst reach other registers of the function's own BAR, so std adds no capability parser.

Alternatives rejected:

- **Releasing a window's bytes at `unmap`:** every window would have to carry its function's record and `unmap` look it up, for remapping a structure, which drivers do not need.
- **`of_bar_at` beside `of_bar`:** two operations for one concept.

## Consequences

- A virtio or NVMe driver reaches each structure a capability places without a trusted premise.
- Each window costs a runtime record of 32 bytes until its function is released.
- A window overlapping the MSI-X table or pending bit array, which `of_bar` from offset 0 gave before, is refused.
