# 0083. Reversibility classes

Status: accepted

## Context

NaN OS gates an agent's actions by what they do that cannot be undone, such as data leaving the machine. A gate fed by declarations drifts from the code, and a run-time classification can bound neither code before it runs nor closures built apart, so the checker must compute the class as a digest fact. Language 1 reserved the eleventh digest field for it without computing it, and no head could bound it.

## Decision

- **A lattice, not a boolean.** The field is a class: `traceless < recorded < restorable < irreversible(K)`, K a non-empty set of the kinds `emit`, `actuate`, `erase` and `spend`, ordered by inclusion. A gate needs to tell a log line from an overwritten file from a sent packet, and the kinds of an irreversible effect from each other, so a single "irreversible" fact would make every gate either refuse logging or allow erasure. Joins take the greater class and union kinds, so the class propagates like the other facts.
- **Classes live in the toolchain, not in std heads.** Supplied operations get their class from one table per target, as their other facts already come from their owning chapters. On the machine targets the sys capabilities are only sims, so their operations are `traceless` there. Everything else is `traceless`: an operation the table omits is a reviewed choice, not a default to fix later.
- **Classes are limits.** `limits recorded` or `limits irreversible(emit)` bounds the class on heads and function types, with the fit rule, interface demands and promises of the other limits; a bare `irreversible` is not a limit, since the unbounded class needs none. Lowering a class limit is a new promise; raising or removing it breaks consumers.
- **Device code states its class.** A register or DMA access cannot be classified by the checker: writing the same register may ring a doorbell or move a motor. Device accesses take the greatest class, unless the function or closure performing them states a class limit, which they then take. That is a premise like a `trusted` block's, so the ledger records it as a trust escape that needs a seal. Only a function's own accesses take its premise, so the claim stays next to the register writes it covers.
- **Compensation is a sealed claim.** An undo clause names a function of the same package that puts back what the claiming function changes. The claim compensates the claimer's class: emission stays `irreversible(emit)`, since data that left the machine cannot be recalled, and every other effect becomes `restorable`. The undoer must itself be at most `restorable`, so running the undo is never worse than what a gate already allowed; inverse operations can name each other. The claim is a trust escape: the checker cannot prove that the undoer undoes.
- **Compensation applies when the digest is read**, after cycle members share their join. A member of a cycle that does not compensate keeps the uncompensated join, which only over-approximates it.
- **The encodings stay stable where the class is `traceless`.** Digest key 11 is omitted then, and digest lines write the class only when it is not `traceless`.

Alternatives rejected:

- **Declared classes on std heads:** they would drift from the operations' code, which is what the issue rules out.
- **Inferring device classes from register names or layouts:** a register's effect is the device's, which no source states.
- **Compensation proven by the checker:** it would need the effects of both functions on outside state, which the language does not model.

## Consequences

- Every digest of code calling an unbounded function value or a foreign fn without a class limit is `irreversible(emit, actuate, erase, spend)`, so code under a class limit must bound the function types it calls through.
- Kernels must state class limits on their driver functions, and seal them, before code above can bound its class.
- Authority summaries do not hold classes; a raised class reaches consumers through the reliance their requires-sets record.
