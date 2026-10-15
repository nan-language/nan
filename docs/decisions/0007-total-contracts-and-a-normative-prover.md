# 0007. Total contracts and a normative prover

Status: accepted

## Context

Contracts in NaN do three jobs. They let callers rely on a function without reading its body. They remove run-time checks, which kernel code needs: interrupt fns and spin-lock bodies must never trap, so their bounds and overflow obligations must be proved. And they decide compatibility and weakening between releases, which compare contracts by entailment.

Each job fails if a fact can be false or a proof can vary. A contract term that traps, loops or reads mutable state is a false premise, and every check deleted on its strength is a hole. A requires removed at direct calls is skipped when the function is reached through a function value or a registry. A proof that depends on a solver's version, heuristics or timeout makes acceptance differ between machines and toolchains, and an agent cannot tell a solver's whim from its own mistake. Tests are written by the same agents whose code they judge, so they are easy to make pass without exercising anything.

## Decision

- **Total contract terms.** [Contract terms](../spec/18-contracts.md#contract-terms) call only functions whose digest is pure, terminating and notrap, quantify only over bounded ranges, and carry a well-definedness obligation for each partial operation, discharged under the conditions that guard it. A vacuous contract is an error.
- **Checks leave only by proof.** A [run-time check](../spec/18-contracts.md#run-time-checks) is removed only by a proof whose premises are facts: proven, checked or assumed. An unproven requires is checked at each direct call and in a [checking entry](../spec/18-contracts.md#checking-entries) for every indirect reach; interface requires are checked by callers. Checks run in every build of every target.
- **Statuses are computed.** Each obligation is proven, checked, assumed or open. Tools show the status and source never states it, so no one can claim a proof and a body edit never changes text elsewhere. `expect P` is a checked assertion that becomes a fact; `assume P because "why"` exists only in trusted code, where it is ledgered and sealed.
- **A normative prover.** [The prover](../spec/20-prover.md) is part of the language: a specified, deterministic procedure over NIR, implemented in the bootstrap and again in NaN. It decides a fixed fragment (linear integer arithmetic with constant multiplication, division by known positive divisors, bit intervals, booleans, tags, fields, lengths, `old`, bounded quantifiers) by canonicalisation, propagation and case splits to depth 2, within fuel counted in steps. Its outcome depends only on the query's canonical encoding, so it is cached by obligation hash and toolchain hash. A refutation counts only after the [interpreter](../spec/21-interpreter.md) confirms the counterexample, so it is never false.
- **Proof demanded only where needed.** An unknown outcome is an error only where notrap is demanded: `limits notrap`, interrupt fns, spin-lock bodies, transparent functions and contract terms. Elsewhere it keeps the check, so a weaker prover costs speed, never acceptance.
- **Monotonic provers.** Each language version proves at least what the previous one proved ([prover monotonicity](../spec/37-evolution.md#prover-monotonicity)); a proof found unsound is retracted by an advisory that later toolchains carry.
- **Testing as evidence.** A [test](../spec/22-testing.md) names its target (`test name for target`) and counts only when that target runs during it. Property tests draw inputs constructively from types, predicates and requires, with boundary values, and shrink failures to NaN literals with an exact regression-test edit. Tests run against `testing::world()`, a simulated world with a virtual clock and a seeded scheduler, which code cannot detect. Tests targeting a dependency's items are reliance tests that veto its upgrades.

Rejected alternatives:

- **Partial contract terms:** they turn traps and divergence into axioms.
- **Tested facts as premises:** a test samples behaviour and does not establish it.
- **Dropping unproven checks on hot paths:** the cost of a kept check is reported, never waived.
- **SMT solvers:** their results vary with version, term order, seeds and time, and they are large dependencies NaN would have to rewrite in NaN.
- **Wall-clock budgets:** a proof would depend on the machine and its load.
- **Untargeted tests:** they cannot be scoped, counted as evidence or checked to run anything.
- **Rejection sampling:** tight predicates and requires starve it.

## Consequences

- A fact a caller relies on is either proved, checked at run time, or assumed in a listed, sealed place; tests never stand in for any of them.
- Some contracts cannot be written: unbounded quantifiers and calls to functions that may trap are out. Eq, Ord and Hash methods are notrap, so comparisons stay usable in contracts.
- The fragment is small. Its sufficiency for kernel code is measured on milestones M4 and M5 before the freeze: below 90% of bounds and overflow obligations discharged, rules are added by ADR. Certificates and stronger procedures are reserved for later versions.
- Two implementations of one prover must agree exactly, and a soundness bug in it invalidates proofs. Cached proofs are keyed by toolchain hash, so a toolchain carrying an advisory proves again instead of reusing them. The proof cache is as trusted as the toolchain that writes it.
- `ensures` holds at returns only; a failing call guarantees only the predicates of the values it received by `mut` or `take`.
- Kept checks cost time in every build, which rewards stating the requires and invariants that let the prover remove them.
- Tests are reproducible from their replay tokens and are the evidence that seals and reliance rest on, but they never remove a check.
