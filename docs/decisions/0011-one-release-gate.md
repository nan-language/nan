# 0011. One release gate

Status: accepted; unsigned seals and the reserved signature field superseded by 0082

## Context

Agents check in a tight loop and need to run unfinished code: a function with a hole, a missing contract, a debug print. None of that may ship. Agents can also game checks without meaning to: delete a failing test, weaken a contract, add an `assume`, or approve their own trusted code. Warnings do not stop any of this; they are ignored and pile up. Named policies make "the checks pass" mean something different in each project, and an agent cannot tell which meaning applies from the code in front of it.

Human review is scarce. It must go to the small set of places where soundness rests on judgement rather than on a rule the checker enforces, and a review must stop counting the moment what it judged changes, including what the reviewed code relies on. Documentation needs the same treatment: stale comments steer models towards old behaviour, and prose from dependencies is a channel for instructions that do not come from the user.

## Decision

- **One incompleteness class.** Holes, missing or disputed specifications, `dbg`, unused items, locals, fields, dependencies and tombstones, uses of forwarding tombstones, unwaived threshold excess, stale API snapshots and docs, generator gaps, pending weakenings and missing seals form one diagnostic class. `nan check` and `nan test` report it and succeed; the release gate, `nan verify` and the `nan publish` that runs it, rejects it. There are no warnings and no policies, and no option, directive or file changes a diagnostic's class ([incompleteness](../spec/31-trust.md#incompleteness)).
- **Ledger.** Each item's trusted blocks, assumes, foreign and landing functions, validators, declassifying decoders, device and privileged grants, waivers, todos and `dbg` uses are computed from source into its ledger, which the code root carries ([ledger](../spec/31-trust.md#ledger)).
- **Seals bound to code and premises.** Every trust escape, every package granting `device` or `priv` and every pending weakening needs a current seal. `nan seal <item> --as <principal>` binds the principal to the item hash and to a closure hash covering the interfaces the item names and the transparent bodies it unfolds; any change to either lapses it. Seals are unsigned in language 1 and a signature field is reserved. In a git workspace, the author of the sealing change must differ from the author of every change that introduced the sealed item hash or a hash its closure records ([seals](../spec/31-trust.md#seals)).
- **API snapshots and weakening.** `.nan/api/<module>.nan` is the machine-written interface of a module, holding per item its accepted spec: contracts, examples and the targeting tests recorded by parameter types and expect-line hashes. `nan api update` advances the accepted spec when the new one keeps it, and otherwise records a pending weakening bound to the item hash; only a seal accepts a weakening. A weakening is a contract the prover finds weaker, an example removed or changed, or a targeting test removed, disputed, stripped of an expect line or given another parameter type. Adding, renaming or moving tests never weakens. In a git workspace the gate checks every acceptance in history, so editing a snapshot by hand accepts nothing ([weakening](../spec/31-trust.md#weakening)).
- **Doc binding.** Public `///` docs are bound to their item's interface hash: once the interface changes, the doc is stale (`doc.stale`) until it is edited or confirmed by `nan api update --confirm-docs`. Doc references must resolve, code in docs exists only as examples, which run, and prose never restates digest facts, which tools compute. Tool output wraps other packages' comments and strings in origin-labelled envelopes ([docs](../spec/32-docs.md#doc-binding)).
- **Review order.** `nan diff` reports each item's change classes (format, move, body, interface, spec, authority, encode, trust) before the text ([change classes](../spec/31-trust.md#change-classes)).

Alternatives rejected:

- **Warnings**: ignored, they accumulate and hide the few that matter.
- **Named policies or profiles** such as dev, release and kernel: a third axis beside targets and commands, and a meaning of "passes" that depends on configuration. Target environments restrict what is valid; the gate restricts what is released.
- **More statuses** (tested, sealed, attested): only the difference between a premise and a reviewed judgement changes behaviour.
- **Seals bound to the item text alone**: a callee whose contract weakens would leave the seal standing on a premise that no longer holds.
- **Signed seals**: they need keys and principal identities that only NaN OS provides.
- **Review in pull requests with no seals**: nothing machine-checked ties the review to the code, so it lapses silently.
- **A separate store of specifications with its own accept command**: it duplicates the API snapshot and seals.
- **Hashing whole tests**: any refactor of a test would read as a weakening, which teaches agents to seal by reflex.
- **Binding inline comments to body hashes**: the check would fire on nearly every body edit, with the same effect on confirmations.
- **Unchecked code in docs**: it drifts from the language and the API without anyone noticing.

## Consequences

- "`nan verify` passes" means one thing in every package, and code with holes still checks, runs and tests.
- Weakening a spec, adding a trust escape or approving one's own change is never silent: it stays incompleteness until another author's current seal accepts it.
- A dependency upgrade that changes an interface a sealed item names lapses that seal; the diagnostic names each changed closure entry.
- Unsigned seals rest on lapse-on-change and the git author check, which one person with two identities defeats. The reserved signature field lets signed seals arrive without a format change.
- The accepted spec moves forward through `nan api update` while nothing weakens, and through a seal past a weakening.
- Trust is reviewable per item through the ledger, the trust change class and the attestation file, without reading every line.
- Primer cost: the core's diagnostics and incompleteness section is a few lines; the packages and trust topic carries the gate, seals, snapshots and docs within its 3k tokens.
