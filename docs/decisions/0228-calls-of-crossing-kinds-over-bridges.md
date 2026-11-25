# 0228. Calls of crossing kinds over bridges

Status: accepted

## Context

NaN OS gives an instance its ports as capabilities of a crossing kind whose state holds the target, the operations its grant allows and a credit, and components call them from any task, holding no `sys::Cx` ([0224](0224-capability-kinds-crossing-into-isolated-spaces.md)). A waiting operation of such a capability in an isolated space went up to a kernel task running the kind's method, whose call to an instance of another space went down that instance's link: two kernel relays where a call through a runner port rides a bridge ([0225](0225-bridges-between-isolated-spaces.md)). The kernel knows a kind only through `sys::Crossing`, so it cannot read a capability's target or protocol from its state, and a space writing into a bridge is untrusted: a check its own runtime makes before writing a call is one it can skip. Jobs hold such capabilities too ([0227](0227-services-of-each-job-and-instance-of-a-space.md)), and a job's entry runs as soon as its runtime starts it.

## Decision

- **A channel carries the calls of one operation of a capability of the world.** `Link.open_crossing(key, operation, hash, to, inbound, outbound)` names the capability by its key in the `caps` of `run_isolated` or `hold_isolated`, the waiting operation whose calls ride, the protocol they are calls of and the instance answering them; the kernel bridges it as a port's channel, for a job as for an instance. A `call` of that operation whose arguments a system call could copy passes over the bridge, its arguments the call's bytes and its reply's bytes its result; any other reaches the kernel as before.
- **The serving side has its kernel admit each call.** Before giving a call to its instance, the answering space's runtime asks its kernel, through its end of the bridge, to run the kind's `invoke` of that operation on the call's bytes with the kernel's own capability; a refusal or a stale capability fails the call. The kernel thus decides each call on the bytes its instance serves, whatever the calling space writes, and revoking the capability stops the bridge's use at the next call. A call carrying handles over such a bridge breaches it, as the kind never saw them.
- **The ends a job or instance calls through start with it.** They travel in the grants of its start order, so its runtime reaches them before its entry runs, and the world's capabilities enter the space's table before its task maps the link, so its bridge names the entry of the capability it carries.

Alternatives rejected:

- **The calling side's runtime admitting each call by `invoke` before writing it:** a space skipping it would pass calls its kernel never admitted, or admitted with other bytes.
- **The kernel writing admitted calls into the bridge:** the space maps the frame writable, so the reader could not tell the kernel's records from the space's, and every call would be copied through the kernel.
- **A method of `sys::Crossing` naming the operations that ride and their protocol:** every crossing kind would implement it, while only the holder wiring the instances knows the protocol.
- **Every waiting operation of a bridged capability riding:** the calling space would choose the operation its call is admitted as.
- **The runtime learning of the ends a job calls through by notices:** a job's entry would race them.

## Consequences

- A kind whose calls ride bridges admits them through `invoke` of the riding operation, which its `call` makes first on the kernel's path.
- Each call over such a bridge costs the serving side a system call copying its bytes, and a call past 1 MiB goes through the kernel.
- The kernel never learns that a call over a bridge completed, so a kind cannot give back at the reply a credit it spends at admission.
- A reply carrying handles fails the call, a crossing kind's result being bytes.
- Supersedes 0225's consequence that waiting operations of crossing kinds always reach the kernel.
