# 0087. Capability entry state read by snapshot

Status: accepted

## Context

An operation through a capability checks its handle without the runtime lock ([0072](0072-capability-checks-inlined-in-lowering.md)) and then reads the entry's state: an `Fs` entry's directory, prefix and flags, a clock's origin and resolution, an executor's cell. Revocation, under the lock, frees the entry's text and its slot, which the next new entry may reuse. An operation past its check could therefore read freed text or the state of the slot's next occupant, though 14 §Capability tables says it completes as if the entry were live. Attenuations wrote their restriction into the derived entry after `Derive` had released the lock, so a revocation of the source in between let them write into a freed or reused slot.

## Decision

- **Entry state is fixed once its handle is returned.** `Derive` takes the attenuation, a state word raised to a floor and a text extending an `Fs` prefix or replacing a `Net` host, and applies it under the lock before returning the handle.
- **Words are read through a snapshot.** An operation loads the four state words, then the generation and live word, each with an atomic load, and the words are the entry's only when the handle is still live after them: freeing advances the generation, under the lock, before a later entry writes the slot's state.
- **Texts are read only under the lock.** An `Fs` operation takes a copy of its prefix under the lock and frees it once the platform function or the sim has joined it to the path; `Net.connect` compares a copy of its host. Revocation frees texts and slots at once.

Alternatives rejected:

- **The lock for every operation:** every clock reading, spawn and outbox post of every thread would contend on the lock that also guards cells and chunks.
- **Deferred reclamation, by reference counts on texts or epochs of the threads:** every operation would pay atomic updates, or the scheduler would need quiescent points on every host thread and CPU, to save copies only `Fs` and `Net` operations make, beside system calls that dwarf them.

## Consequences

- A check reading state costs four loads more than one reading liveness alone; hostbench's `generation` is unchanged.
- Each `Fs` operation allocates and frees a copy of its prefix, and each `connect` through a restricted `Net` one of its host.
- An operation whose handle is revoked between its snapshot and its copy of a text fails with `Revoked.Stale`, as if it had checked at the copy: nothing observable happens in between.
