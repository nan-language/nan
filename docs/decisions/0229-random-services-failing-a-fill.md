# 0229. Random services failing a fill

Status: accepted

## Context

A served capability's operations call its kind's methods ([0116](0116-nanos-jobs-in-a-kernels-shared-space.md)) and fail as they fail, and a `sys::Random` operation through a stale capability fails with `sys::Revoked.Stale`. `sys::RandomService.fill` could not fail, so a kind serving a random source over another, such as an isolated space's runtime over its kernel's, could not report that the source it reaches went stale: a draw through it returned with its buffer untouched, which the caller took as random bytes ([0227](0227-services-of-each-job-and-instance-of-a-space.md)).

## Decision

- **`sys::RandomService.fill` fails with `sys::Revoked`,** which a served `sys::Random` gives as its own failure. A space's runtime fails a fill with `Revoked.Stale` once its kernel answers that the random source is stale, so every draw from a source revoked while its job or instance runs fails.

Alternatives rejected:

- **The runtime revoking its own capability on a stale answer:** the draw that met the answer would still return its buffer untouched, and the runtime would keep a revoker per random source it serves.
- **A served capability checking its entry again once its method returns:** it needs the same revoker, and every operation would pay a second check.

## Consequences

- A kind serving a random source over another passes on that source's failure.
- Code calling `fill` of a `RandomService` directly handles its failure; kinds whose fill cannot fail keep a head without `fails`.
- Supersedes 0227's consequence that a random source revoked while its job runs fills nothing silently in the space.
