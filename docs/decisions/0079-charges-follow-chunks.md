# 0079. Charges follow chunks

Status: accepted

## Context

A value leaving its domain passes the chunks it occupies to the receiving domain, but each chunk stayed charged to the cell it was drawn through. NaN OS passes messages between components that are domains with budgets of their own, so a receiver keeping a message left its sender paying for it, and long-lived data accumulated on the sender's budget, which safe code cannot re-parent.

## Decision

- **Every domain has a cell:** the cell of the allocator its domain run was given, the test world's for a test, the root grant's for the process and the kernel. The runtime records it in the domain's record.
- **A chunk passing to a domain is charged to that domain's cell from then on, when that cell's chain has room for it.** The passing, done under the lock that guards draws and returns, subtracts the chunk's bytes from every open cell of its old chain, then adds them to the new chain when every cell of it is open and stays within its limit, as a draw would, and otherwise back to the old chain. Checking after the subtraction lets a chain that shares cells with the old one take the chunk without counting it twice. A chunk's later passing or return then works from its current cell, so the charges of a cell always match the chunks charged to it.
- **Charges pass only within one supply.** A return gives a chunk back to the supply of the chain it is charged to, so a chunk drawn from a region's supply never becomes charged to a chain drawing from another; it then stays charged where it was.
- **The move never fails; only the charge may stay.** Ownership passes whatever the charges, since moving a value cannot be fallible and its memory cannot be left behind in the domain being freed. A charge exceeding the receiver's limits stays with the sender's cell: otherwise a sender could push a receiver past its limits, for instance by posting large messages to another component's intake, and starve the receiver's own draws.

## Consequences

- A receiver pays for what it keeps whenever its budget has room, whoever built it, and no sender can push a receiver's cells past their limits.
- A value too large for the receiver's budget stays charged to the sender's, which keeps paying for it until it is returned.
- Each pass walks both chains, in time proportional to their depth.
