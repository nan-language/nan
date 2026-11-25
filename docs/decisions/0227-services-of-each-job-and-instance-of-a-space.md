# 0227. Services of each job and instance of a space

Status: accepted; a random source revoked while its job runs filling nothing in the space superseded by 0229; `isolate` taking its clock apart and `Services` holding none superseded by 0232

## Context

NaN OS places every instance of one principal and provenance class in one isolated space ([0222](0222-isolated-spaces-shared-by-jobs-and-instances.md)), but each instance's world is its own: its log names it in each record, it draws randomness only under a grant, a recorded instance runs on a taped random source, and each job writes its own standard streams and file view. The runtime built every world of a space from the handles 0 to 3 of its table, the services `isolate` was given, and only instances took capabilities, so a job reached no capability of a crossing kind ([0224](0224-capability-kinds-crossing-into-isolated-spaces.md)). Code running in a space reaches those handles through `SpaceCalls` without waiting, the self-hosted toolchain among it. A crossing kind's waiting operation goes up the first channel of an instance's link, which a job does not have, and a space's runtime learns of its kernel only through system calls and the orders it takes. A `sys::RandomService` cannot fail a fill, so a space's runtime can tell a random source is stale only from its kernel's answer.

## Decision

- **`run_isolated` and `hold_isolated` take the `Services` of their job or instance, and `run_isolated` takes its `Caps`.** The kernel adds the four capabilities to the table as entries of that job or instance, beside its runner capability and link's channels, and sends their handles with what crossed for its world; the runtime builds the world from them.
- **The space keeps services of its own.** `isolate` still holds a `Services` by the handles 0 to 3, which `SpaceCalls` and test runs reach, and takes its clock apart, since every task of the space waits on the space's thread; `Services` no longer holds a clock.
- **A job's waiting operations of crossing kinds go up as a system call and come down as an order.** The runtime sends the message an instance's first channel would carry through an operation of the job's runner capability that returns at once; the job's kernel task calls the capability on a task of its own and the reply reaches the runtime as an order beside those telling an instance of its bridges.
- **A random source stale as its job starts is stale in the space.** The runtime fills nothing through the kernel's random source when it builds the world, and holds a revoked capability in its place when the kernel answers stale.

Alternatives rejected:

- **The world's services as capabilities of `caps` under fixed keys:** a typo would silently give a job the space's services, and a world would hold each twice.
- **Services keeping a clock per job:** a sleep on a job's own clock would wait for its kernel on every call, while the space's timers already serve every task.
- **`SpaceCalls` reaching the calling job's services:** the calls name fixed handles and never wait, and the runtime knows no job from its thread.
- **A channel the isolation maps for each job's waiting calls:** frames of its own per job and a relay mirroring an instance's, for messages its runner capability already carries.

## Consequences

- Each job or instance takes four more entries of its isolation's table, which never shrinks, so an isolation places about fifty jobs before its later worlds reach no services.
- A kernel's random source revoked while its job runs still fills nothing silently in the space.
- A job's waiting call costs a system call, a kernel task and an order.
