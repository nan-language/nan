# 0204. Calling conventions judged per variant

Status: accepted

## Context

A foreign fn names its calling convention, and a convention serves only some targets: `"sysv"` the Linux targets, `"win64"` the Windows ones. The check rejected a convention clause in a package declaring any target the convention does not serve, whatever module held it. A library running on Linux and on Windows therefore could not call either system: target variants hold per-target code, but a Linux variant's `"sysv"` fn was rejected because the package also declared Windows targets, though the checker never checks that variant for them. Only std escaped this, through the platform package each target's descriptor names, which no other package can use.

## Decision

- **A convention clause is judged under each target it is checked for**, and a clause in a target variant is checked only under the targets the variant matches. A package without variants, or declaring `any`, is judged as before.

Alternatives rejected:

- **One package per system, picked by the consumer's dependency:** a library's consumers would choose its system for it, and a library declaring both systems could not depend on either.
- **A convention per system kind in every package:** what a variant already expresses, written again in every head.

## Consequences

- A library declaring Linux and Windows targets holds each system's foreign fns in its variants, which export identical items, so its consumers see one interface.
- A convention clause in a plain module of such a package is reported once per target it does not serve, naming those targets.
