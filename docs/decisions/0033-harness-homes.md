# 0033. Harnesses run with homes of their own

Status: accepted

## Context

The local store and the build cache live in the [user directory](../spec/36-builds.md#cache), in the home directory the user database gives, and no environment variable may change what the toolchain outputs. The conformance harness publishes the packages of an example into [a local store holding nothing else](../spec/00-charter.md#example-workspaces), and no test may write the store of the developer running it.

## Decision

- The library takes, once per process, a home directory in place of the user database's and, optionally, a directory for its local store in place of the user directory's; the `nan` binary never gives them.
- The harnesses run each command in-process through the library, sharing one home under the target directory, whose build cache they all use; each specification chapter and milestone has a local store of its own there, emptied before each example or scenario. Unit tests share one home under the temporary directory.

Alternatives rejected:

- **An environment variable naming the user directory:** the binary's outputs would depend on it.
- **The harnesses running the `nan` binary:** a process cannot be given another home without one.

## Consequences

- The harnesses' build cache stays warm across runs, and across the chapters running at once; deleting the target directory resets it.
- A test process gives its home and store once, before its first command; nextest runs each test in a process of its own.
- Harness commands run in the test's process, so a panic outside a command's own handling ends the test.
