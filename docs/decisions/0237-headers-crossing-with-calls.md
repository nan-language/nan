# 0237. Headers crossing with calls

Status: accepted

## Context

A task's header carries the cause, deadline, account and sender of the work a message starts, and a handler runs under the header of the message it handles. A holder serves an instance's calls through `inbox.call`, but the instance's domain served each call on a task of its own, which carried the instance task's header, not the caller's, and an instance in an isolated space received none at all, the link carrying no header down. The other way, the calls a space's job or instance makes through a port or a crossing kind's capability ran on kernel tasks under the kernel's own headers. So an isolated instance's work lost its account, deadline and trace at every boundary it crossed, whatever the holder or the instance gave.

## Decision

- **A call carries the header of the task making it**, and the task serving it runs it under that header through `sys::run_headed`: `inbox.call` takes the caller's header to the task of the instance's domain serving the call, down the link for an isolated instance; a space's calls through ports and `call`s of crossing kinds, up its link or by system call, and its sends through ports take the space task's header to the kernel task making them; and each call record of a bridge holds its caller's header, which the serving side's admission and handler run under.
- **Only calls and messages carry it.** The other operations a space's kernel serves at once by system call (a crossing kind's `invoke`, logs, streams, files, readings, the runner's bookkeeping) run under the isolation task's header, as they start no work beyond themselves.
- **A call made under no header is served under the serving task's own**, `sys::run_headed` giving no way to clear one.

Alternatives rejected:

- **A header per system call:** every system call would carry 33 more bytes for operations whose cost and trace belong to the isolation serving them.
- **A `sys::run_headed` taking an optional header:** it changes a std API to cover calls made with none, which a runner giving each call its message's header never makes.

## Consequences

- The header a handler sees, and the header a kernel's runner or capability sees for what that handler calls, are the same whether the instance runs in the kernel's domain, in an isolated space, or across a bridge.
- A header a space sends is the space's claim, as one given to `sys::run_headed` is in a kernel's domain: a kernel charging accounts by header trusts its bundles' code for it.
