# 0040. Foreign fns are named by their symbols

Status: accepted

## Context

A foreign fn's name is the external symbol it binds ([foreign functions](../spec/23-low-level.md#foreign-functions)), and every function name had to be snake_case ([naming conventions](../spec/03-packages-modules.md#naming-conventions)). Platform symbols follow their own conventions: glibc reaches a thread's error number only through `__errno_location`, whose leading underscores fail `name.case`, so the NaN front end had to look the symbol up at run time through `dlsym` instead of declaring it.

## Decision

The name of a foreign fn is the external symbol it declares and follows no naming convention. Every other rule on names, folded clashes included, still applies to it.

Alternatives rejected:

- **A symbol clause separate from the name:** a second name for one function, for a case the name already covers.
- **Looking symbols up at run time:** it hides the binding from the checker and the ledger.

## Consequences

- Any C symbol that is an identifier can be declared directly.
- A foreign fn's name may break the conventions agents otherwise rely on, which the `foreign` modifier on its head makes visible.
