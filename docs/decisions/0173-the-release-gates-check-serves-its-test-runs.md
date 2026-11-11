# 0173. The release gate's checks serve its test runs and release checks

Status: accepted

## Context

`nan verify` checks each gated package alone, as `nan check` checks a scope within it, and judges the package's gate over that check's checkers. Each package's test runs then checked its closure again under each run target, and its release checks checked it again under each target it declares, so a package declaring the host target its tests run on was checked twice, and once more when it reaches releases: on the NaN toolchain's workspace outside git, the second check took 27 s.

## Decision

- **A test run lowers by the check's checker of its closure under its run target** when one checks each package holding the tests it may select and is narrowed to none or to the one such package; otherwise it checks the closure under that target as before. `nan test` and resolution give it no check.
- **Release checks use the package's check** under each target the package declares.
- **The gate keeps a package's check through its test runs and release checks only when those use it:** when one of the package's test runs lowers by it, or its closure holds releases. Any other check is dropped once the package's gate is judged, as a package declaring `any` runs its tests on the host target, which its check does not cover.

## Consequences

- `nan verify` on the NaN toolchain's workspace outside git took 54 s instead of 78.
- A kept check is held through the test runs of every gated package, beside the checkers those runs build for targets no check covers.
