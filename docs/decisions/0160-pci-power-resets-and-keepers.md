# 0160. PCI power states, resets and keepers

Status: accepted

## Context

Only the runtime writes a PCI function's configuration ([0108](0108-io-domains-and-confined-device-grants.md)), so a kernel could neither suspend a function nor reset it, and NaN OS's device manager suspends idle devices, leaves functions without a driver in D3hot and resets the function of a driver whose domain is abandoned before the driver's next incarnation claims it. Settlement freed a cut-off function as `release` did, so the manager had nothing to reset or hand on. A function given to a new holder also reaches its registers again, and a window over its BAR stayed taken until the function's release ([0151](0151-register-windows-at-offsets-within-a-bar.md)) yet could outlive it: moved out of a domain before its trap, or kept by whoever released the function, it would reach a later holder's registers.

## Decision

- **Power and reset through std.** `set_power` writes the power management capability's state, and `reset` initiates a function level reset where the device capabilities report one, else a D3hot to D0 transition where No_Soft_Reset is clear. Both save what the runtime wrote (BARs, PCIe device control, ACS, MSI, MSI-X and its table, command) and write it back once the function answers, so attachments, windows and installed interrupts survive; a function that does not answer within 1 s counts as removed. The waits read the kernel clock through a supplied `clock_ns` and spin: the operations complete before a stopping task stops, as every PCI operation does, and the PCI lock is released while they wait.
- **A suspended function is cut off by the runtime, not by the device.** Suspending ends decoding, bus mastering and INTx in the command register, whatever the device does in D3hot; later operations change the saved command and table, which reach the device when it is on.
- **Keepers.** `keeper` gives a linear `hw::PciKeeper`; while it lives, release and settlement cut the function off (MSI and MSI-X included, and detached from its space) and hand its record back instead of freeing it, and `reclaim` gives it to the keeper's holder. The keeper is the holder the kernel names, a value of its own domain like any other resource, so no settlement runs user code and no domain's trap reaches another's state.
- **Windows over BARs are resources.** A window `of_bar` gives carries a record held by its domain, which `unmap` and settlement end; its bytes, in CPU addresses, stay taken until then, whichever function or holder later owns the BAR. `hw::Mmio` carries the record beside its address, and moving it rehomes the holding as a DMA buffer's does.

Alternatives rejected:

- **Relisting cut-off functions through `boot::Kernel.pci_functions`:** the kernel value is consumed by `anchor` before any domain runs.
- **Settlement resetting the function:** settlement releases without waiting, and a reset waits up to a second.
- **Windows freed with their function, as before:** a window outliving its function would reach the registers of the function's next holder.
- **Refusing every operation on a suspended function:** a driver would track the power state the runtime already knows.

## Consequences

- A trapped driver's function can be reset and handed to its next incarnation, which maps the same registers once the trapped domain's windows have ended.
- Supersedes 0151's decision that a window's bytes stay taken for its function's life: they are freed when the window ends.
- A window moved out of an abandoned domain keeps its bytes taken until it is unmapped, so the function's next holder is refused those bytes.
- `hw::Mmio` is two words, and moving one into another domain's reach rehomes its window's holding.
- Restoring a large MSI-X table holds the PCI lock, with interrupts disabled, for one write per table word.
