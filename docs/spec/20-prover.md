# The normative prover

This chapter defines the prover's queries over NIR, callee knowledge, the frame rule, the decidable fragment, the procedure, fuel, outcomes with refutation and entailment, and determinism with the proof cache.

## Queries

The [prover](#procedure) decides each [obligation site](19-nir.md#obligations) s of a [NIR function](19-nir.md#functions) through a **query**: a list of premises and a goal.

- A **term** denotes a value. A **symbol** is a term standing for an unknown value: a parameter, the content of a place at entry, or a fresh, skolem or bound symbol the prover introduces. An **application** applies a function, an operator or a field step to argument terms.
- Every site has its own query and outcome.
- Visibility and [unfolding](02-items.md#transparent-functions) are judged from the item holding s.

### Weakest preconditions

The query is the weakest precondition of the obligation over the function's control-flow graph with every back edge removed, a back edge being an edge to a block that dominates its source. Walking the blocks so that each follows its predecessors, the prover gives a term to each value and operand, and to the content of each place at each point:

- A function parameter is a symbol. The place that a `ref` or `ref mut` parameter designates has its own symbol as content at entry. A place within a place whose content is t has the field of t.
- An operand `read P`, `mut P` or `take P`, and `load P` and `move P`, have the content of P. `constant C` has C's value when C has an integer, `bool` or `char` type.
- An instruction or call of an interpreted form of the [decidable fragment](#decidable-fragment) has that form's term, and any other call the term that [callee knowledge](#callee-knowledge) gives. A `when` is a fresh symbol. Every other instruction with a result is the application of its opcode to its operand terms.
- `init P, o` and `assign P, o` give P the term of o, and a place containing P its former content with P's field replaced. A guard slot's content is the literal boolean last stored there on the path.
- A call returning `ref T` or `ref mut T` designates a place whose content is the call's term. `ref P` and `ref mut P` designate P.
- A call changes contents as the [frame rule](#frame-rule) states.
- At a join, a block parameter, and the content of each place whose terms differ along the incoming edges, is ite(c1, a1, ite(c2, a2, ... an)): e1 to en are the incoming edges in the order of their source blocks, ai is the term along ei and ci the reach condition of ei.
- A loop's blocks are its header, the target of its back edges, and each block from which the source of one of them is reachable without passing through the header. At the header, each place whose content an instruction or call of the loop's blocks changes under these rules has a fresh symbol as content. An invariant's fact holds only on paths from its site; no premise of a query holds it at the header.
- `all lo ..< hi R` and `any lo ..< hi R` are the [bounded quantifiers](#decidable-fragment) over the term that region R yields, its integer parameter being a bound symbol x. Within R, x has the facts lo ≤ x and x < hi, or x ≤ hi for `..=`. The other parameters of a region are fresh symbols, and its first block has the reach condition of its instruction's block.
- The blocks of a `when` region are ordinary blocks of the walk, and a `jump` out of a region is an edge to its target.
- A call of a function of role `requires`, `ensures`, `old`, `decreases`, `invariant`, `assume`, `predicate` or `where` has the term that its returned operand has under these rules, its parameters standing for the argument terms.

The **reach condition** of the entry block is `true`, and that of any other block the disjunction of the reach conditions of its incoming edges. The reach condition of an edge is the conjunction of its source's reach condition and the edge's condition: `true` for `jump`; o and `not o` for the edges of `branch o`; a fresh boolean symbol and its negation for the `ok` and `fail` edges of a call that may fail. A conjunction omits its `true` operands and a disjunction its `false` operands.

The goal is the obligation's condition: for a `check`, the term of its operand; for any other site, the condition under which its operation does not trap by the rule defining the site: [Numbers](09-numbers.md) for arithmetic, shift, division and conversion sites, [indexing](07-ownership.md#indexing) for `bounds` sites, [layout values](23-low-level.md#layout-values) for the `conversion` sites of layout fields, and the owning section of each other built-in operation, each value the condition names having its term. For a `bounds` site, the goal is i < len(x), with x the indexed value's term, i the index's term and len the `len` application of the indexed value's type. For a layout field store, it is that the value is below 2^w, w the field's bit count, or for a `rest` field that its bitwise and with the covered bits is 0. A trapping condition that the decidable fragment does not interpret is a boolean application of the operation to its operand terms.

The premises are exactly, in this order:

1. The entry facts: for a function of role `body` or `default`, the requires of its item or interface method that are not [disputed](18-contracts.md#disputed-clauses), in clause order, and the predicates that are facts at entry ([predicate obligations](04-types.md#obligations)); for a `closure` function, the predicates that are facts at its entry; for an `example` function, none; for an `invariant` or `assume` function, the premises of the query of its call in the body, its parameters standing for the call's argument terms; for an `entry` function, the predicates that are facts at the item's entry and no requires; for a function of another role, the premises that [Well-definedness](18-contracts.md#well-definedness) gives its clause.
2. The reach condition of the block of s.
3. For each block B from which s is reachable, in NIR order, and each point of B, before s in the block of s, these facts, guarded by the reach condition of B unless B dominates the block of s:
    - the facts of the instruction or call there;
    - the condition of the obligation site there unless its obligation is [open](18-contracts.md#statuses), and the operand of an `assume`;
    - each fact that another chapter states there, such as the predicates that predicate obligations make facts and the element range of a `for` over a range ([ranges](09-numbers.md#ranges)).
4. The range premises of the symbols and applications of the query.

Facts of calls within a region that does not hold s, or within a contract function the prover unfolds, are not premises.

`and` and `or` are branches in NIR ([logical operators](06-statements-expressions.md#logical-operators)), so the left operand is a premise of the right operand's obligations.

```nan
pub fn largest(values: Slice[u32]) -> u32 limits notrap {
    var best: u32 = 0
    for i in 0..<values.len() {
        if values[i] > best {
            best = values[i]
        }
    }
    return best
} largest
```

### Relevance

After [canonicalisation](#canonicalisation), the negated goal is relevant, and so is each premise that shares a symbol or an application with a relevant premise. The procedure and the [canonical encoding](#determinism) use the relevant premises alone.

## Callee knowledge

The prover knows a callee only through its head, its contract and, where the item holding s unfolds it, its transparent body.

A call to a function f has these facts:

- After a normal return, on the `ok` edge of a call that may fail: each ensures of f, with each read or `take` parameter standing for its argument's term at the call, each `mut` parameter for the content of its place after the call, each `old(e)` for the term of e at the call, and `result` for the call's term.
- After the call on every edge: the predicates that [predicate obligations](04-types.md#obligations) make facts after it.
- A call through a [bound](08-generics-interfaces.md#generic-parameters) or [through `Dyn`](18-contracts.md#clauses) has the ensures of the [interface](08-generics-interfaces.md#interfaces) method.
- A call through a function value or a registry field has no facts ([indirect calls](10-effects-digests.md#indirect-calls)).
- The requires of f are obligation sites before the call.
- Each [opaque](18-contracts.md#opacity) subterm is a fresh symbol, one for each distinct subterm within the facts of one call and within one requires site.

The term of a call is given by the first rule that applies:

1. A call that the item unfolds is the term the transparent body returns, its parameters standing for the argument terms.
2. A direct call to a [pure](10-effects-digests.md#purity) function, or a call through a bound or `Dyn` whose interface method head demands `pure`, is the application of the callee to its argument terms, provided every function-typed argument is pure and no argument has a type with [interior mutability](12-memory.md#interior-mutability).
3. Any other call is a fresh symbol.

```nan
pub fn clamp_level(value: u32) -> u32
    ensures result <= 255
{
    if value > 255 {
        return 255
    }
    return value
} clamp_level

pub fn level_byte(value: u32) -> u8 limits notrap {
    return u8.fit(clamp_level(value))
} level_byte
```

```nan
pub closed struct Slot {
    pub used: u32
    pub size: u32
    invariant used <= size
} Slot

pub fn Slot.free(self) -> u32 = self.size - self.used

pub fn Slot.capacity(self) -> u32 limits notrap {
    return self.used + self.free()
} Slot.capacity
```

```nan
pub closed struct Slot {
    pub used: u32
    pub size: u32
    invariant used <= size
} Slot

pub fn Slot.free(self) -> u32 {
    return self.size - self.used
} Slot.free

pub fn Slot.capacity(self) -> u32 limits notrap {
    return self.used + self.free() // error: proof.unknown
} Slot.capacity
```

## Frame rule

The **frame rule** keeps the term of a [place](07-ownership.md#places) across a call unless one of these holds:

1. The call receives the place, or an overlapping place, by `mut` or `take`: as an argument, as the receiver, or as a `mut` or `take` capture of a closure argument.
2. The place lies within the contents of a value with [interior mutability](12-memory.md#interior-mutability), and the call's [digest](10-effects-digests.md#digests) has the sync fact.
3. The place lies in memory that a `hw::Ptr`, `hw::Dma` or `hw::DmaShared` value reachable from the call's arguments or captures designates, and the call's [digest](10-effects-digests.md#digests) has the sync or device fact.

A place the rule does not keep has a fresh symbol as content after the call. A write through the result of a `mut` [projection](07-ownership.md#projections) whose receiver is a place P, or through a value derived from that result ([holds](07-ownership.md#holds)), also changes P: after each `init` or `assign` through such a value, and after each call receiving it, or a place holding it, by `mut` or `take`, P has a fresh symbol as content. Every other place keeps its term, across awaited calls as across others.

```nan
pub closed struct Credit {
    pub spent: u32
    pub sends: u32
} Credit

fn count_send(mut sends: u32) {
    sends = sends.wrapping_add(1)
} count_send

pub fn headroom(mut credit: Credit, limit: u32) -> u32 limits notrap
    requires credit.spent <= limit
{
    count_send(mut credit.sends)
    return limit - credit.spent
} headroom
```

```nan
pub closed struct Credit {
    pub spent: u32
    pub sends: u32
} Credit

fn count_send(mut sends: u32) {
    sends = sends.wrapping_add(1)
} count_send

pub fn headroom(mut credit: Credit, limit: u32) -> u32 limits notrap
    requires credit.spent <= limit
{
    count_send(mut credit.spent)
    return limit - credit.spent // error: proof.refuted
} headroom
```

## Decidable fragment

The sorts are mathematical integers, booleans, and one uninterpreted sort per other type.

The integer terms are the values of integer scalars, a `char` as its scalar value, the tag of an enum value, and the quotients, remainders and skolem and bound symbols the prover introduces. A tag is the index of the value's variant in declaration order. Each integer symbol, and each integer application other than those this section excludes, has the range of its type as a **range premise**: the type's minimum to maximum for an integer type, 0 to 0x10FFFF for `char`, 0 to n − 1 for the tag of an enum with n variants, and at least 0 for the tag of an open enum of another package. The value of an operation that traps when its mathematical value lies outside its type ([Numbers](09-numbers.md)) is that mathematical value, without a range premise; its range is the operation's obligation.

The prover interprets exactly these forms:

- integer literals, `add`, `sub`, `neg`, `mul` when one operand is a literal, and signed `shl a, k` for a literal k, which is `mul a, 2^k`;
- `inv`, which is MAX − a on an unsigned type with maximum MAX and −a − 1 on a signed type;
- a call of the [named conversion](09-numbers.md#conversions) `widen` or `fit` whose source and target types are both integer types, whose term is its argument's;
- on unsigned operands, with c a literal of at least 1: `div a, c` and `rem a, c`, the quotient q and remainder r, applications with their type's range, with the facts a = c × q + r and r ≤ c − 1; `shr a, k` for a literal k, which is `div a, 2^k`; and `band` of a and a literal 2^k − 1 in either order, which is `rem a, 2^k`;
- `eq`, `ne`, `lt`, `le`, `gt` and `ge` on integers and `char`; `eq a, b` and `ne a, b` on `bool`, which are ite(a, b, `not b`) and its negation; `not`; `is P, E.V`, the atom that the tag of P's content equals V's index;
- ite terms;
- a field step of a term; the field of a `construct` or the payload field of a `variant`, which is its operand, and the tag of a `variant`, which is its variant's index; the field of a term with one field replaced, which is the new term at that field and the former field elsewhere;
- bounded quantifiers `all x in a..<b: P` and `any x in a..<b: P`, and their `..=` forms.

These instructions are applications known through facts on their result r only:

- on unsigned operands, `div a, m` and `rem a, m` with m not a literal: 0 ≤ r ≤ a, and for `rem` also r ≤ m − 1;
- on unsigned operands, `band a, b`: 0 ≤ r ≤ a and r ≤ b; `shr a, n` with n not a literal: 0 ≤ r ≤ a;
- `mul a, b` with neither operand a literal: no range premise; its interval is the product of its operands' intervals ([Propagation](#propagation)).

Signed `shl a, n` with n not a literal is an application without a range premise. Every other integer instruction is an application with its result type's range. Every float operation and comparison is an application without facts, and so is every conversion from or to a float type. [Bit counts](09-numbers.md#bit-counts) and lengths are known through the contracts of their std functions.

```nan
pub fn middle(low: u32, high: u32) -> u32 limits notrap
    requires low <= high
{
    return low + (high - low) / 2
} middle

pub fn low_byte(word: u32) -> u8 limits notrap {
    return u8.fit(word & 0xFF)
} low_byte

pub fn one_below(values: Slice[i32], index: usize) -> i32 limits notrap
    requires index < values.len()
    requires all i in 0..<values.len(): values[i] > 0
{
    return values[index] - 1
} one_below
```

## Procedure

The **prover** is the specified deterministic procedure that decides whether the relevant premises of a query, its negated goal included, have no model. Its stages are canonicalisation, propagation and case splits.

### Canonicalisation

Canonicalisation takes the premises in their order, the negated goal last, and keeps that order: the parts of a premise replace it from left to right, and the clauses defining a fresh boolean symbol follow the clause that introduces it.

1. The goal is negated and added as a premise.
2. A guarded premise `g` ⇒ `P` becomes `not g or P`. A boolean ite(c, p, q) becomes `(not c or p) and (c or q)`. `not` is pushed onto atoms, a negated comparison becoming its complement, turning a negated `all` into `any` of the negated body and a negated `any` into `all`.
3. Each `any x in a..<b: P` that lies within no `all` becomes P with x replaced by a fresh skolem symbol k, and the atoms a ≤ k and k < b, or k ≤ b for `..=`.
4. Conjunctions split into separate premises. A disjunction is a clause, whose disjuncts are its literals; an `all` is a literal. A conjunction inside a clause, and an ite condition that is not an atom, is replaced by a fresh boolean symbol, with clauses defining the symbol as that formula.
5. Each integer comparison becomes an atom Σ ci × ti ≤ c or Σ ci × ti = c: like terms combined and coefficients divided by their greatest common divisor, c being rounded toward negative infinity for ≤. An equality whose c the divisor does not divide is false. `<` becomes ≤ c − 1, `>` and `>=` negate the coefficients, and `!=` is a negated equality.
6. Literals are folded. An atom without terms is `true` or `false`. A clause with a `true` literal is dropped, and `false` literals are removed.
7. Identical terms are one term.

[Relevance](#relevance) then applies. The fresh, skolem and bound symbols are numbered from 0 in order of first occurrence, reading the relevant premises in order and each from left to right; symbols the procedure creates later continue the numbering in the order of creation. Last, the terms of each atom, the literals of each clause and the premises are sorted into term order, and an equality whose first coefficient is negative has its coefficients and constant negated.

The **term order** compares terms, atoms and clauses by their [canonical encoding](#determinism), byte by byte; every choice of a first item follows it.

### Propagation

The state holds equivalence classes of terms; for each integer class an interval [lo, hi] and a congruence x ≡ r (mod m), initially unbounded and m = 1; the disequalities; a difference graph whose nodes are the integer classes and a zero node; the asserted literals; the clauses; and the quantifier instances made. An interval with lo > hi is a contradiction. Propagation runs in rounds. A round applies these rules in order, each to its items in term order:

1. Congruence closure. Two applications of the same function or operator whose arguments lie pairwise in the same classes merge. Two integer classes whose intervals are the same single value merge. A merge intersects the classes' intervals and congruences and unites their disequalities. Merging `true` with `false`, or two classes that a disequality separates, is a contradiction.
2. Ite terms. An ite term whose condition is decided merges with the arm it selects. An undecided ite term's interval narrows to the smallest interval holding both arms' intervals. An ite term whose class excludes one arm, by disjoint intervals or a disequality, decides its condition toward the other arm.
3. Intervals. Each asserted atom narrows the interval of each of its terms from the intervals of the others, rounding toward the inside. A `mul` application's interval narrows to the smallest interval holding the products of its operands' bounds.
4. Congruences. For each asserted equality Σ cj × tj = c and each of its terms ti with ci equal to 1 or −1, let rj and mj be the residue and modulus of each other term tj, a single-value interval giving its value and modulus 0, and m the greatest common divisor of the |cj| × mj. When m is at least 2, ti's congruence is intersected with ti ≡ ci × (c − Σ cj × rj) (mod m) by the Chinese remainder theorem, modulo the least common multiple of the moduli; no solution is a contradiction. Each interval then narrows to the nearest bounds that satisfy its congruence.
5. Difference bounds. Each asserted atom with exactly two terms, of coefficients 1 and −1, and each interval bound, as a bound against the zero node, is an edge of the difference graph. The graph is then replaced by its shortest-path closure. A negative cycle is a contradiction. A closed edge to or from the zero node narrows an interval, and two classes bounded by 0 in both directions merge.
6. Clauses. A literal is true when it is asserted or the state entails it, and false when the state entails its negation: a boolean atom by its class; an integer atom by the intervals of its terms, by the difference graph when it has two terms of coefficients 1 and −1, or, for an equality, by class identity. A clause with a true literal is satisfied. A clause whose literals are all false, the empty clause included, is a contradiction. A clause with exactly one literal that is not false asserts it. Asserting a boolean atom merges it with `true` or `false`; asserting an integer atom adds it to rules 3 to 5; asserting a negated equality records a disequality, which narrows the interval of its one term whose bound it excludes once every other term has a single-value interval.
7. Quantifiers. A trigger of a clause holding the literal `all x in a..<b: P` is an application in P that has x itself as exactly one argument and no bound symbol in its other arguments. When a term of the state applies the trigger's function to arguments in the classes of the trigger's other arguments, the clause is instantiated with the term t at x's position: a copy of the clause in which that literal becomes `not (a <= t) or not (t < b) or P` with t for x, with `not (t <= b)` for a `..=` range, is canonicalised and added. A clause is instantiated at most once per class of t.

Propagation ends at a contradiction, or after a round that changes nothing.

### Case splits

When propagation ends without contradiction at a depth below 2, the prover splits on one atom: the condition of the first ite term whose condition is undecided or, when there is none, the atom of the first undecided literal other than an `all` literal in the first clause that is neither satisfied nor asserting. The first branch asserts the atom and the second its negation. Each branch continues from a copy of the state at the next depth, with propagation and then case splits. The root is at depth 0.

The query is proved when every branch ends in a contradiction. The procedure stops at the first branch, in depth-first order with the first branch first, that ends without contradiction, at depth 2 or with no atom left to split on. That branch's state is the **open state**.

```nan
pub fn span(left: u32, right: u32) -> u32 limits notrap {
    var low = left
    var high = right
    if left > right {
        low = right
        high = left
    }
    return high - low
} span
```

## Fuel

**Fuel** is the number of steps a query may take: 10,000, shared by all its branches and its candidate. A step is each merge of two classes, each change of a class's interval or congruence by one rule applied to one item, each edge of the difference graph that a closure adds or lowers, each literal asserted, each quantifier instance, each branch opened, and each term fixed for a candidate. Canonicalisation and relevance take no fuel. A query that needs a step after its 10,000th stops at once; its outcome is unknown and it has no candidate.

## Outcomes

An **outcome** is the prover's result for an obligation:

- **proved**: the procedure proves the query. Its [status](18-contracts.md#statuses) is assumed when the query's relevant premises include the operand of an `assume` or the condition of an earlier obligation site of the same function whose status is assumed, the sites of a function being decided in site order, and proven otherwise. A callee's contract never makes a status assumed.
- **refuted**: the query is not proved, and a **refutation**, a counterexample confirmed by the [interpreter](21-interpreter.md#refutation-confirmation), exists for it ([Refutation](#refutation)).
- **unknown**: every other case.

Proof is demanded exactly for:

- the obligations in the text of a function or closure, closure bodies within it included, whose limits demand notrap ([limits](10-effects-digests.md#limits)) through its own `limits` clause, the interface method it implements, its kind ([interrupt fns](24-kernel.md#interrupt-functions)) or, for a closure, the function type it [fits](10-effects-digests.md#fit), as a spin-lock body does;
- the obligations in a [transparent function](02-items.md#transparent-functions) body;
- the well-definedness obligations of [contract terms](18-contracts.md#well-definedness).

Where proof is demanded, an unknown obligation is an error (`proof.unknown`), and so is a refuted one (`proof.refuted`); elsewhere neither outcome is a diagnostic. A rule of another chapter that requires any other condition to be proved names its own code.

- The because-chain of `proof.unknown` holds a `raised` reason for the obligation's source, then `note` reasons for the reason: the fuel ran out, or the open state, at depth 2 or with nothing left to split on, gave no candidate or one that is not confirmed. Further `note` reasons list the fresh symbols and applications that stand for calls in the open state.
- The because-chain of `proof.refuted` holds one `note` reason per parameter, its subject the labelled NaN literal of the counterexample, such as `credit: Credit(spent: 0, sends: 0)`.

```nan
pub fn last_index(count: u32) -> u32 limits notrap {
    return count - 1 // error: proof.refuted
} last_index
```

```nan
pub fn last_index(count: u32) -> u32 {
    return count - 1 // traps: trap.overflow
} last_index

test last_index_of_nothing for last_index {
    expect last_index(0) == 0
} last_index_of_nothing
```

### Refutation

The prover builds a candidate from the open state:

1. An **input** is a parameter, the content of a parameter's place at entry, and a field, payload field, tag, length or element of an input: a length is an application of a `len` function of `Slice`, `SliceMut` or `Array` to it, and an element an application of their `at` function to it and an index that is an input or a literal. The entry terms are the integer and boolean inputs of the open state. Each const parameter of the item is an input.
2. The prover fixes each entry term, in term order, to the least value its class allows: for an integer, the least value of its interval that satisfies its congruence and that no disequality excludes; for a boolean, `false` unless its class holds `true`. After each fixing it propagates without case splits. A contradiction leaves no candidate.
3. It builds each parameter's value from the fixed terms. A part that no fixed term determines takes the least value of its type: 0, `false`, the `char` of value 0, the first variant in declaration order, and for a slice no element. An element whose index is not below its slice's length is left out.
4. No candidate exists for an obligation in a function of a role other than `body`, `requires`, `old` and `decreases`, for an item with generic type parameters, nor for one with a parameter whose type is none of these: a scalar; an `Option`, `Result`, `Array`, `Slice` or `SliceMut` over such types; a struct or enum that the item's module may construct, whose fields and payloads have such types.

The interpreter's [confirmation](21-interpreter.md#refutation-confirmation) of the candidate decides between refuted and unknown.

### Entailment

An **entailment** asks whether contract terms P entail contract terms Q, as [vacuity](18-contracts.md#vacuous-contracts), impl refinement, [compatibility](29-identity-versioning.md#compatibility) and [weakening](31-trust.md#weakening) do. Its query has P as premises and Q as goal, over the parameters, matched by position, and the `result` and `old` values of the heads involved, with their range premises. Unfolding is judged from the item whose rule asks. The same procedure and fuel decide it; it holds exactly when the query is proved.

## Determinism

The outcome of a query depends only on its canonical encoding and the prover of its language version, and a confirmation only on the candidate and the NIR the interpreter runs; never on time, memory, parallelism, the order of checking, a cache or the machine. An implementation proves exactly what this procedure, amended by the advisories its toolchain carries, proves ([prover monotonicity](37-evolution.md#prover-monotonicity)).

The **canonical encoding** of a query is the concatenation of its relevant premises in term order after canonicalisation, the negated goal included, written with the forms of the [NIR encoding](19-nir.md#encoding):

- A term is a uvar kind and its content: 0, a parameter, by its value number; 1, the content of a place at entry, by the place; 2, a literal, as NIR encodes it; 3, an application of an instruction, by its opcode and argument terms; 4, an application of a function, by the `call` opcode, the callee's function reference and the argument terms; 5, a field step, by its term and the step as a text; 6, an ite, quotient, remainder or tag, by its query opcode and its parts; 7, a fresh, skolem or bound symbol, by its query opcode and its number.
- An integer atom is a uvar 0 for ≤ or 1 for =, the list of its coefficients as svars paired with its terms, and its constant as an svar. A boolean atom is a uvar 2 and its term. An `all` literal is a uvar 3, its bounds, a uvar 0 for `..<` or 1 for `..=`, its bound symbol and its body's clauses.
- A literal is a uvar 0 when positive or 1 when negated, then its atom. A premise is the list of its clause's literals.

### Proof cache

The **obligation hash** is the BLAKE3-256 hash, with its algorithm tag ([hashes](29-identity-versioning.md#hashes)), of a query's canonical encoding. The proof cache maps the pair of the obligation hash and the [toolchain hash](36-builds.md#toolchain-hash) to the procedure's result: proved; the fuel ran out; or the open state's reason with the values fixed for its entry terms, or the absence of a fixing. A cached result equals the result of running the procedure. Building and confirming the candidate happen at each use, since both depend on NIR outside the query.

## Diagnostics

| Code | Class | Rule |
| --- | --- | --- |
| `proof.refuted` | error | [Outcomes](#outcomes), [Transparent functions](02-items.md#transparent-functions), [Limits](10-effects-digests.md#limits), [Well-definedness](18-contracts.md#well-definedness) |
| `proof.unknown` | error | [Outcomes](#outcomes), [Transparent functions](02-items.md#transparent-functions), [Limits](10-effects-digests.md#limits), [Well-definedness](18-contracts.md#well-definedness) |
