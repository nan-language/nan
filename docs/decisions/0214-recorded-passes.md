# 0214. Recorded passes

Status: accepted

## Context

A package's tests run on each target it declares, and a run on a target whose images the machine cannot run fails with `test.emulator-unavailable`. No machine runs both a Linux and a macOS target, or both architectures of one system, so a package declaring such targets fails the release gate once it holds a test: multi-system packages such as `toolchain/platform` and std's platform packages carried none.

## Decision

- **`nan test --as <principal>` records a pass** of each test whose every run on a run target passes, in the package's machine file `.nan/passes.nan`; on a run target the machine cannot run, a test with a current pass there passes without running. The gate is unchanged: every test passes under `nan test`, each target's runs taken from a machine able to run them.
- **A pass binds the test's reach**, the item hashes of every item it reaches, std included, **and no toolchain hash**, as a seal binds none: a pass lapses when the code it ran changes, not with every toolchain, so a repository whose toolchain changes with most commits keeps the passes its other machines recorded.
- **A pass is signed as a seal is**, by the principal's key when the package's principals file gives it one, so that a pass in a committed file attests who ran it. No author rule applies: running a test reviews nothing.

Alternatives rejected:

- **Binding the toolchain hash**, as the build cache keys passes: every toolchain change would lapse every pass, so no pass recorded on another machine would stay current where the toolchain is built from the same repository.
- **A gate per run target**, `nan verify` stating the targets it verified and publishing requiring all: publishing from one machine would still need the other machines' results, which is what recorded passes carry.
- **A separate recording command**: `nan test` already runs and judges the tests, so recording is one option of it.

## Consequences

- `nan test` can write files, so its summary counts them.
- A pass recorded with a faulty toolchain stands until the code it reached changes.
- Recording passes on another system means running `nan test --as` there and committing the passes file.
