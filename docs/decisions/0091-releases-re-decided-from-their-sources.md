# 0091. Releases re-decided from their sources

Status: accepted

## Context

The specification ([0010](0010-computed-identity-and-versioning.md)) let a consumer reuse a dependency's units from its artefact when the closure satisfied the dependency's requires-set, so that a status recorded proven removed the obligation's run-time check. NaN OS admits artefacts that other machines and agents built: one recording proven for a bounds obligation never proved would ship safe code without its check. Since [0026](0026-releases-checked-from-stored-sources.md) both compilers check a release from the source tree its store entry holds and decide every obligation anew, but nothing bound that tree to the artefact it was stored with, and an error in it went unreported: checking skipped it, a final build failed internally on a function left without a unit, and `nan verify` never looked at named releases.

## Decision

- No status, digest, unit or example outcome is ever read from an artefact; reuse and requires-set satisfaction leave the language. Requires-sets stay for compatibility and approvals.
- A store entry's source tree must have the source tree hash its artefact's attestation records (`artefact.source-mismatch`), checked with the lock like the code root and the signature. For a signed artefact, whose publisher key the lock pins, the source tree checked is then the one the publisher signed, and the image section can keep holding the stored artefact.
- A release check checks a release whole, examples included, in the checker of each final build, test image and release gate run; its errors are `artefact.recheck-failed` on the reaching directive, each as a `diagnostic` reason. `nan check` keeps checking only what it reads.
- The release check also requires each schema tree the release's code root records to be the tree its type has in the closure, since images carry that code root and devices judge records by it. This takes over the reliance [0088](0088-schema-reflection-and-erased-decoders.md) placed in reuse; the foreign schema hashes its requires-sets record stay for readers of code roots alone.

Alternatives rejected:

- **An opt-in verify mode:** a toolchain that trusts recorded statuses by default is unsound for every consumer who forgets the option.
- **Re-deriving each release's code root and comparing it with the lock:** a code root depends on its closure and std, so a release locked beside a newer dependency than it was published against, or checked by a toolchain with another std, derives another root without being altered; the comparison is decisive only when those match, which an unsigned forger controls through the stored lock and attestation.

## Consequences

- Every final build, test run and release gate checks each release it reaches whole, once per run; the proof cache keeps the prover's share small.
- An unsigned release's source tree is trusted as the local store is; a release from an untrusted source is trusted for its code only once signed by a key its consumer pins.
- As 0088 intended of reuse, a compatible dependency release changing the tree of an embedded type fails the release check of each consumer release recording that tree, so resolution does not lock them together.
- Advisories take effect on releases as soon as a toolchain carries them, with no retraction of recorded statuses.
