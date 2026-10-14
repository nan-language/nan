# 0013. Language cost and scope

Status: accepted

## Context

No model has seen nan in training. An agent learns it in context, from a primer it reads at the start of every session, and reads it through habits formed on C, Rust, Go, Python and TypeScript. Every primer token is paid on every turn and competes for attention with the task, so each concept has a price, and estimates of that price prove unreliable: the full concept inventory, before its scope review, came to about 20.5k primer tokens with a core of about 11.2k against an 8k target. A learnability run, in which six learners wrote library, server, driver, component, parser and command-line programs from the primer alone, found errors that the design itself caused.

nan will build NanOS, but the features only NanOS consumes (IPC, loading, live update, a UI model, signed seals) have nothing to validate them before NanOS exists. Language 1 must still be finishable, and code written for it must keep its meaning when those features arrive.

## Decision

- **Primer tiers.** The primer has a core of at most 8,000 tokens, six topics (low-level, concurrency, kernel, packages and trust, contracts, components) of at most 3,000 each, and a tool card of at most 1,000. A tier's size is the largest count of three tokenizers. The core holds what an agent must write correctly without a diagnostic; std heads come from `nan show --api`, not the primer ([primer tiers](../spec/39-primer.md#primer-tiers)).
- **Trade-in.** Every change to the language or the primer is recorded with its primer delta and its nanbench results. A change that takes a tier over its cap enters only with the removal of concepts that bring the tier back within it ([trade-in](../spec/37-evolution.md#trade-in)).
- **Nanbench.** A benchmark of write, repair and read tasks over library, host, driver and kernel code, run by named models given only `nan primer` and the toolchain's output, measures first-compile success, fix rate per diagnostic code and comprehension. A language version is published only when no metric falls below its predecessor's within the confidence interval. Every sample in the specification is a conformance test, a false-friends chapter covers the five mainstream languages, and the idiom table grows from the errors nanbench logs ([nanbench](../spec/37-evolution.md#nanbench), [false friends](../spec/40-false-friends.md)).
- **One dialect.** A language version fixes the grammar, the formatter, the diagnostic codes, the prover, the target descriptors and the encodings. A later version gives meaning only to source its predecessor rejects, and retires a form only with an exact rewrite that `nan upgrade --language` applies. Artefacts record their version, and packages of different versions link in one closure. Each version's prover proves at least what its predecessor proved; a soundness fix retracts proofs only through a published advisory ([language versions](../spec/37-evolution.md#language-versions)).
- **Scope line.** Language 1 freezes everything that decides whether source is valid and what it means. Each deferred feature arrives additively into space language 1 reserves: item-head words (`stable`, `flexible`, `migrate`, `machine`, `protocol`, `ipc`), rejected syntax such as `self.{a, b}` parameters, artefact sections, lock, digest and seal fields, and names such as `ui`, `sys::infer`, `sys::load` and `core::simd`. Any fact the compatibility rules do not model counts as breaking, except a fact kind a later version introduces, which older artefacts read as unknown ([reservations](../spec/37-evolution.md#reservations)).
- **Milestones.** Language 1 freezes when six milestones pass on the Rust bootstrap: M1 front end, M2 host programs, M3 proof and packages, M4 a UEFI loader and an SMP kernel with a UART reader task and a virtio driver under QEMU, M5 the front end written in nan checking itself and std, M6 a component on `host::Runner`. Measurements taken on them, among them prover discharge, ledger counts and stack use, are published with language 1, and prover discharge of bounds and overflow obligations must reach 90% on M4 and on M5 ([milestones](../spec/38-self-hosting.md#milestones)). After the freeze come a nan backend over NIR, a self-build, diverse double compilation against the bootstrap, frozen at language 1 ([0002](0002-bootstrap-stack-and-lifetime.md)), nofloat code generation, then NanOS.

Alternatives rejected:

- **Editions**: two meanings for one text break the rule that a line is understood from what an agent sees, and models mix dialects. Retired forms with exact rewrites move code forward instead.
- **Admitting concepts on estimated cost**: the estimates above missed the cap by 40%; only measured counts and nanbench results decide.
- **A card of std heads in the core primer**: the core has no room, and `nan show --api` gives the exact head when it is needed.
- **Specifying NanOS features now**: freezing an IPC encoding or a loader format before its consumer exists freezes a guess. Reserved space keeps their arrival additive.
- **A date for the freeze**: real kernel, driver and compiler programs passing is the only evidence the language is complete enough.
- **The nan backend before the kernel milestones**: it would put NanOS behind the hardest toolchain project. Cranelift lowers language 1 until then.
- **Discarding the bootstrap after self-hosting**: diverse double compilation needs an independent compiler of the whole language.

## Consequences

- The core is estimated at about 7.7k of its 8k tokens, so any new core concept needs a trade-in; the counts are measured with the three tokenizers before the freeze.
- Rules that an exact fix always repairs, and std surface, stay out of the core and are learned at the point of error or lookup ([0012](0012-toolchain-as-agent-interface.md)).
- A version that regresses a nanbench metric is not published, however much it adds.
- Deferred features cannot break code written for language 1, at the price of reserved words and fields that do nothing until then and of upgrades judged breaking whenever a fact is not modelled.
- A prover discharge below 90% on M4 or M5 adds prover rules by a new decision before the freeze.
- Components are validated only on `host::Runner` before NanOS; reserved words, inert state and runtime-owned environments keep the loader's additions additive.
