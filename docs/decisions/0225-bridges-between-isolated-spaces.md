# 0225. Bridges between isolated spaces

Status: accepted; waiting operations of crossing kinds always reaching the kernel superseded by 0228; frames moved but never mapped kept until the isolation returns superseded by 0234; handles carried but never claimed kept until the isolation returns superseded by 0233

## Context

An instance in an isolated space calls a port through its runner, and the kernel relays the call up one link and down another, its tasks copying it at each hop ([0134](0134-component-instances-in-isolated-spaces.md), [0136](0136-runner-capabilities-of-instances-in-isolated-spaces.md)): no encoding past 1 MiB crosses, a large payload is always copied, and the kernel learns nothing of a space that writes a malformed frame but to stop hearing it. NaN OS gives each pair of a calling instance and a port whose target lies in another space a channel mapped into both. A link's frames are its caller's, lent to the call holding the instance until no space maps them; each isolation's space is owned by the one task running it; a cown holds only droppable values and frames are linear; a door holding the asks of other isolations, which name doors, would release itself recursively; and a space's runtime wakes one doorbell waiter at a time, the others waiting out a second.

## Decision

- **A bridge is a channel of the calling instance's link.** `Link.open_port(port, to, inbound, outbound)` names the instance answering `port`; `hold_isolated` maps the channel into both spaces before the instance starts, and returns once no space maps it, so the frames keep their owner and no call races the bridge's opening.
- **Isolations reach each other through doors.** Each isolation's asks (placing, lending its space, opening, ringing and closing ends, ending a breaching instance) sit behind a capability of a kind whose state is the door, so no door holds a door and asks to an isolation that has returned are dropped. A space is lent across isolations as within one: the task holding the link maps the frames, and the tasks running spaces never wait on each other.
- **The runtimes exchange records over the rings.** A pump per end writes queued records and reads the other end's, the end serving them giving each call to its instance and writing its reply. A waiting side sets a word of the frame it waits on, and the other side asks its kernel to wake it only when that word was set; the wake reaches the pump as an order of its kernel rather than through the doorbell.
- **Bytes from 256 KiB move as frames.** The writing side's kernel takes the pages holding them out of its space; a task per isolation keeps the frames until the reading side's kernel maps them in place of pages of its own (`Space.adopt`).
- **Handles cross by token.** The writing side's kernel adds what crosses to the other space's table and keeps it under a token the record names, which only the reading side claims, so a writer names no entry of the reader's table.
- **A breach is the writer's fault.** Counts no reader and writer leave, an oversized or undecodable record and a token naming nothing end the writing side's instance `Trapped` with `trap.bounds`, and close the reading side's end.

Alternatives rejected:

- **A `connect` operation on held instances, the frames given to the kernel:** the frames would need an owner outliving both spaces, and the instance's first calls through the port would race the connection.
- **Waking a bridge's side through the doorbell:** its pump would wait behind the link's readers for the second-long recheck.
- **Streaming every payload through the rings:** the reader would buffer whatever length the writer claims.
- **Records naming entries of the reader's table:** a writer could hand the reader capabilities its neighbours hold.

## Consequences

- Only calls cross a bridge; messages sent through the port and waiting operations of crossing kinds still reach the kernel.
- `hold_isolated` of the calling instance returns only once the other space's runtime has closed its end, which a space whose thread never yields holds up.
- Frames moved into a space stay with its isolation, and those moved but never mapped, like handles carried but never claimed, are kept until that isolation returns.
- Each opening takes an entry of each space's table.
