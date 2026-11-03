# 0111. System interfaces through steppers

Status: accepted

## Context

An operating system steps its components through `sys::Stepper` and keeps their states as checkpoints ([0106](0106-component-steppers.md)). Some events of a component's life need the component's own code: after a resume from a checkpoint, which holds no keyed task and no delayed message, it must re-arm them; under memory pressure it should drop what it can rebuild; a supervisor asks whether it is alive; a driver quiets its device before the device's power drops. The holder reaches the state only through `Stepper.state`, read-only, and cannot build a message of a generic M, so it has no way to run component code for these events. `sys::Component` cannot gain methods in a later version without breaking every implementation, and a method there would make every component implement every event.

## Decision

- Four std interfaces, `sys::Resume`, `sys::Pressure`, `sys::Health` and `sys::Suspend`, each parameterised by the Env and M of a component, whose methods are handlers: they receive the state and the Env by `mut` and a component context, so they act through the same effects as `on`.
- The holder calls them through stepper operations, which call nothing, `health` replying `Healthy`, when the state does not implement the interface for the stepper's Env and M. Lowering decides this per instantiation, so a generic holder needs no bound and a component implements only the interfaces it cares about.
- An impl line naming a system interface for a state without a matching `sys::Component` is an error, since no stepper would ever call it.
- The holder decides when each event occurs and what reason or level it passes; the specification names what each reason and level means to the component, not when an operating system calls them.

Alternatives rejected:

- **Methods of `sys::Component`:** every component would implement all of them, and no later version can add one.
- **Stepper operations requiring the interface:** a generic holder would need all four bounds, forcing every component to implement them.
- **Events as messages:** M belongs to the component, so a holder cannot build such a message generically, and `health` needs a reply.

## Consequences

- An operation for an interface the state lacks lowers to nothing beyond its return.
- The interfaces are available on every target, wherever steppers are.
