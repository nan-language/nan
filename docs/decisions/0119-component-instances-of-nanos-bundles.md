# 0119. Component instances of nanos bundles

Status: accepted; the bundle borrowed for the instance's life superseded by 0129

## Context

NaN OS runs components as instances: the kernel holds each one's state and Env as a stepper's holder does ([0106](0106-component-steppers.md)), calls its handlers and system interfaces ([0111](0111-system-interfaces-through-steppers.md)) in a domain of its own, checkpoints it at quiescence and restores it without `start`, and delivers calls from other bundles encoded ([0115](0115-wire-encoding-of-protocols.md)). A component lives in a nanos bundle that a bare kernel loads ([0116](0116-nanos-jobs-in-a-kernels-shared-space.md)). The kernel and the bundle are separate closures: the kernel cannot name the component's state, Env or messages, so it can build none of them, call no handler and hold no `sys::Stepper` of them. Only std types have one layout in both, and only the entries a bundle records are code the kernel may call. A domain run ends when its first task ends, and a handler's trap must abandon the instance's domain, not the kernel's.

## Decision

- **A component is an entry.** On a nanos target, the manifest's `entry` directive lists component states in place of `main`, the directive taking a name list. A component entry serves a protocol and its Env implements `nanos::Environment`, which builds the Env from the `nanos::World` a kernel grants. A bundle records each component entry by name beside its offset and the stack its task needs, read by `sys::load::Bundle.component` as a `Job`, which already is an entry with its stack.
- **One entry per component runs the instance task.** The entry has the job entry's ABI, a world, an input and an `Ended` by address, and owns extended state for its call. It runs `nanos::Hosted.run`, a generic adapter written in NaN: decode the checkpoint, build the Env, start or resume a stepper, then carry out the holder's requests until released, and end the stepper, giving the encoding of the last state.
- **The kernel holds the instance through channels.** `nanos::hold` runs the instance task as the first task of a domain run beside a body holding the `nanos::Instance`, and borrows the bundle for the instance's life. Each instance operation sends a request over a std channel and waits for its answer, so the kernel steps, starts keyed tasks, checkpoints and calls system interfaces when it chooses, as with a stepper, while every handler runs on the instance task. A wait the holder abandons ends through a channel whose sender it releases.
- **Calls cross as wire bytes and handles.** An `Inbox` sends a `sys::Encoded` with its protocol hash; a task of the instance's domain serves it through the stepper's outbox with `sys::Wire.serve`, which checks the hash and the operation's `requires`, and the reply returns encoded. A call is answered once the holder steps it, as an outbox post is.
- **The body's stack is bounded.** The body runs inside the scope that runs the instance task, where no call-site binding judges a function parameter, so its type demands `stack 65_536`.

Alternatives rejected:

- **An entry point per operation:** the kernel would need a function type per operation, and every handler would run on the calling kernel task, outside the instance's domain.
- **Running the stepper on the kernel's task, one entry call per operation:** handler traps would abandon the kernel's domain, and keyed tasks would belong to whichever domain the call ran in.
- **A manifest directive of its own:** a new context word and grammar rule, where NaN OS names a component an entry.
- **Building the Env from fields by name and type:** lowering would construct arbitrary records, and an Env could not derive or check capabilities as code.

## Consequences

- A component entry's Env is never linear: the adapter releases it as its end of scope does.
- An instance costs a task for its requests, one serving its inboxes and one per call in flight, and a channel per request.
- The adapter decodes the protocol's calls for other closures, so a component entry serving a protocol that is no parsing type needs a `validate` grant on its impl line, which no use in the package's own code would demand.
