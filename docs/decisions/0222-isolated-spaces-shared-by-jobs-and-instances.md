# 0222. Isolated spaces shared by jobs and instances

Status: accepted

## Context

A space of [0118](0118-nanos-jobs-in-isolated-spaces.md) mapped the user-mode runtime and one bundle and ran one job or instance: `nanos::run_isolated` and `hold_isolated` each made a space from the frames they were given and released it when their job ended. NaN OS runs every instance of one principal and provenance class in one space, and counts a space's tables, runtime and copy of each bundle's writable data as the cost of isolation, so a space per instance pays them again for each. The calls that place jobs run on tasks of their own and borrow their bundle and link, while one task must run the space's thread and serve its system calls, and the space's frames must outlive every job in it. The runtime of a space is one thread that learns of nothing its kernel does except through system calls.

## Decision

- **An isolation holds the space.** `nanos::isolate(cpu, tenant, services, mut frames, mut extended, exec, alloc, body)` borrows what a space runs on and calls `body` with an `Isolation`; `run_isolated(space, job, link, input, alloc)` and `hold_isolated(space, job, component, link, checkpoint, runner, alloc, body)` place their job or instance in it. A task of the isolation makes the space for the first job or instance, runs its thread while any runs, and releases it once the last has ended; a later one makes a new space. The services, and so handles 0 to 3 of the table, belong to the space, so `SpaceCalls` keeps its fixed handles.
- **Placement crosses tasks by lending.** The calling task sends its job as owned values, a share of its bundle among them (`Isolated.share`), and the isolation's task maps the bundle with `Isolated.join` and lends it the space through a channel of a linear type to map its link's frames, which only the borrowing task can name. Each call returns once the space no longer maps its link, waiting through `drain`, which no cancellation ends.
- **A space maps bundles after its boot area.** `Isolated.join` maps a bundle's image after the images the space maps, links its line tables after theirs, and gives the bundle with its entries as that space places it; a space maps one image per shared bundle. `Space.unshare` unmaps a link's frames when its job ends, so the 32 pages for user frames are reused.
- **The kernel orders, the runtime takes.** The runtime's first task takes orders through handle 5 and waits for them in a slot of its own, which a `WAIT` answered with bit 2 wakes; each order starts a job or instance on a task and in a domain of its own, or abandons one. `END` names the job; one naming none, as a trap outside every job's domain does, ends them all.
- **Abandoning asks the space.** A holder's abandonment, or a cancelled call, orders the runtime to abandon the domain; when every job left is abandoned, the space stops at its next exit as before.

Alternatives rejected:

- **`run_isolated` and `hold_isolated` taking a space value:** the calls borrow frames, extended state and a link per call, and the first caller could not return while later jobs ran.
- **The runtime spawning a task per order from its idle path:** abandon orders need the domains of running jobs, which only a task holding them can reach.
- **Recognising a bundle by its bytes' address:** a freed bundle's address can be reused while a space still maps its image.

## Consequences

- A job or instance that never waits holds up the others of its space, and one abandoned while spinning ends only when no other runs on.
- An exception the space cannot route to a domain, an exhausted fault and a wait nothing can wake end every job of the space.
- A space's table and the images it maps grow until it ends: at most 256 handles, and the room the window leaves after the boot area.
