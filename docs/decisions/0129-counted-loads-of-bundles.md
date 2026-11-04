# 0129. Counted loads of bundles

Status: accepted

## Context

`nanos::run` and `nanos::hold` borrowed the loaded `sys::load::Bundle` for the life of a job or instance ([0116](0116-nanos-jobs-in-a-kernels-shared-space.md), [0119](0119-component-instances-of-nanos-bundles.md)): no task could hand the bundle to another, and it could be neither replaced nor unloaded while any of them ran. NaN OS loads a bundle once, runs every job and instance from that load, moves instances to a replacement one by one and frees a bundle nothing uses. A `Bundle` is linear, so that its unloading withdraws its entries when the kernel says. Reclamation already waits for the calls pinning a placement's target records, which settlement releases with their domain ([0109](0109-loading-bundles-into-a-running-kernel.md)).

## Decision

- **A load is a counted handle on one placement.** `Bundle.share` and `Load.share` give a `sys::load::Load`, which reads the job and component entries of its placement; `run` and `hold` borrow a load.
- **A placement counts its loads on a target record of its own**, listed with the targets its gates name, so a retired placement waits for its loads as it waits for calls; the record stays, as every target record does.
- **The count is a private std base type, a pin holding.** Releasing it takes one from the count; settlement of the domain owning it does too; moving it into another domain's reach passes its holding there.
- **The bundle keeps publishing.** `replace` and `unload` retire its placement at once, as before; the loads of a retired placement keep its code until the last ends, so a running instance stays on the load it started from and moves to a replacement by a checkpoint held from a new load.

Alternatives rejected:

- **`mem::Shared[Bundle]`:** a shared value releases no linear contents, and its last release could not publish, which takes the loader's lock and draws memory.
- **Copy handles going stale as gates do:** every run would handle staleness, and nothing would keep a placement for a job between two of its calls.

## Consequences

- A load holds a holding record while it lives, and each placement keeps 16 more bytes for good.
- `reclaim` leaves a retired placement while a load counts on it, whether or not anything runs from it.
- The job and component entries are read from a load, not a bundle.
