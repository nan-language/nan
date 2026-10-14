# 0015. Conformance harness and continuous integration

Status: accepted

## Context

The [charter](../spec/00-charter.md#conformance) makes every specification example a conformance test whose expectations its marker comments state, and the bootstrap must pass them all. The bootstrap implements language 1 in stages, so at any stage many examples mark codes it does not report yet, and only the commands it implements can run. Agents need to run the examples of one chapter alone, and every change needs the same checks before it lands.

## Decision

- **One test per chapter.** `bootstrap/nan/tests/conformance.rs` is a `datatest-stable` harness ([0002](0002-bootstrap-stack-and-lifetime.md)) with one test per file of `docs/spec`, so nextest runs a chapter by its name. A failing test lists every failing example of its chapter by line.
- **Example workspaces.** For each `nan` block the harness writes the workspace the charter defines under Cargo's temporary directory for tests, and runs the built `nan` binary in it.
- **Expectations follow the implementation.** The harness performs the runs of the commands the bootstrap implements: `nan check` must report exactly the marked diagnostic when the implementation reports its code, which the harness reads from the crate's code catalogue, and none otherwise; `nan fmt` must rewrite no file beyond what the charter allows. A code reported for only part of its rules is listed in the harness, whose markers it does not enforce.
- **CI.** A GitHub Actions workflow runs on pushes to `main` and on pull requests: it installs the toolchain pinned in `rust-toolchain.toml` and the pinned nextest, then runs `cargo fmt --check`, clippy with warnings denied, and every test.

Alternatives rejected:

- **One test per example**: `datatest-stable` maps tests to files, and a custom test harness would be more code to keep than the chapter granularity costs.
- **A list of expected failures**: hundreds of entries, each removed by hand; reading the code catalogue makes each newly reported code binding on its examples at once.
- **Extracting the examples into files**: two copies of each example, one of them stale.

## Consequences

- Implementing a code makes every example marked with it binding, with no change to the harness.
- The examples are the oracle of the parser and of `nan fmt`: each must parse to its markers and stay in canonical format.
- The list of partly reported codes must shrink to nothing as the rules it covers are implemented.
- CI depends on GitHub Actions and on the nextest download site.
