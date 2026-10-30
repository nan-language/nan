# 0082. Signed seals and artefact sections

Status: accepted

## Context

Seals were unsigned and the author rule read git authors ([0011](0011-one-release-gate.md), [0032](0032-author-rule-reads-git.md)): whoever could write a snapshot could claim any principal, and one person with two git identities defeated the rule. A NaN OS device has no git history and no toolchain, yet must decide which releases to run from the artefact alone, whose signature, certificate and provenance sections were reserved. The bootstrap may depend only on std and Cranelift ([0001](0001-stack.md), [0002](0002-bootstrap-stack-and-lifetime.md), [0064](0064-the-rust-code-under-bootstrap.md)), and the NaN toolchain must print what the bootstrap prints.

## Decision

- **Ed25519 over SHA-512, verified strictly.** RFC 8032 signing is deterministic, so both toolchains write the same bytes. Verification rejects non-canonical encodings of the key and of R, an S not below the group order and a small-order key, and checks the cofactorless equation, so every implementation accepts exactly the same signatures. The bootstrap writes it by hand in Rust; std's `core::crypto` writes it in NaN, the one implementation the interpreter, both lowerings and the NaN toolchain share, as plain code without supplied operations.
- **Trust is a package's file; secrets are the user's.** The machine file `.nan/principals.nan` gives principals their public keys; it is in the source tree hash and not in the code root, so trusting a key changes no release, and `nan key trust` writes it. A principal's secret key is a 32-byte seed in `~/.nan/keys/<principal>.key`, accessible to its user alone, which `nan key new` draws from the system's random source. A seal by a principal the file keys is signed over its principal, path, sealed item hash and closure hash, and is current only while that signature verifies; a seal of a principal without a key stays unsigned and current by hashes alone, so existing seals keep their meaning.
- **The author rule gains principals.** `nan publish --as <publisher> [--author <principal>...]` names the release's authors, the publisher by default, and the gate then requires a current seal whose principal is none of them, in a VCS workspace beside the git condition. A device judges the same rule from the provenance and the attested seals.
- **Three sections, written together.** The publisher signs its principal, its key and the hashes of the code root, the attestation file and the provenance; the certificate is the builder's signature over its principal, its key, the toolchain hash and the code root of each package of the checked closure, the builder being the publisher; the provenance holds the author principals and the lock. Each signed part carries its public key and signed attested seals carry theirs, so an artefact verifies from its bytes alone. Native cache and availability stay reserved.
- **Signatures bind identities only through the consumer's trust, and the lock pins the publisher.** A carried key proves integrity, not identity: anyone can re-sign under any name. So a consuming package reads a locked release only when its principals file gives the publisher exactly the carried key, gives each signed seal's principal that key or none, and its lock line records that publisher key, which `nan dep sync` pins, or none for an unsigned release. A release signed by an untrusted publisher is refused rather than read as unsigned, since its signatures promise an identity nothing confirms; a seal by an untrusted principal counts as unsigned, since the publisher's signature already covers it. Anything else is `artefact.bad-signature`, so stripping sections, adding them, or re-signing with another key changes nothing a consumer reads.

Alternatives rejected:

- **An Ed25519 or SHA-2 crate:** a dependency the bootstrap excludes.
- **Signing whenever the sealer holds a key:** whether a seal is signed would depend on the user directory, which no output may depend on.
- **Trusted keys in the user directory:** `nan verify` would give each user another verdict.
- **Toolchain hash and source tree hash again in the provenance:** the attestation file holds them, and the publisher's signature covers it.
- **Reading a release signed by an untrusted publisher as unsigned:** the consumer would run a release whose signer it never chose, looking signed.

## Consequences

- A seal's signature is checked wherever its currency is, in the gate and `nan api update`, and again with the artefact's attested seals on every reuse.
- A package depending on a signed release trusts its publisher's key with `nan key trust` before reading it.
- A key identifies a key, not a person: provenance authors are the publisher's claim, and which keys to trust is each verifier's choice.
- Secret keys are stored unencrypted, guarded only by file modes.
- Signing does not yet run in constant time.
