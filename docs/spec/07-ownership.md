# Ownership and access

This chapter defines ownership, moves and the end of scope; modes and the marking of arguments; places and exclusivity; projections, their results and indexing; view bindings; holds and the reports of their conflicts; the consumption of linear values; iteration through cursors; closures, captures and function values; result locations; and `mem::Box`.

## Ownership

The **owner** of a value is what holds it: a binding, a `take` parameter, a value that has an owner and holds it as a field, element or payload, a [store](12-memory.md#stores) slot, a [box](#box), a closure holding a `take` capture, a `for` loop holding its cursor, a [task](16-concurrency.md#task-scopes), or the statement evaluating it. A value owned by the statement evaluating it is a **temporary**. Every value has exactly one owner.

A **move** transfers a non-Copy value to a new owner. A value moves when it initialises a binding, is bound by a pattern from an owned scrutinee ([pattern bindings](04-types.md#pattern-bindings)), is assigned to a place, is passed to a `take` parameter or receiver, is a construction value or payload, is returned from a function other than a projection, is raised by `fail`, or is captured by `take`; [projection results](#projection-results) lists the values that are none of these. A [Copy](05-properties.md#copy) value is copied instead, and its source stays usable.

- After a move, the source place is moved. A use of a place is an error (`own.use-after-move`) when some path reaching the use, loop back edges included, moves the place and does not assign it afterwards.
- The operands of a deferred call pending at an exit are not uses for this rule; [deferred places](17-suspension-io.md#deferred-places) judges them.
- Assigning a moved place that [modes](#modes) allow to be assigned makes it usable again; that assignment releases nothing.
- A **partial move** moves a field out of a place whose root is a `let` or `var` binding or a `take` parameter, by field access or by destructuring. The other fields stay usable. A use of the whole value while one of its fields is moved is an error (`own.use-after-move`); assigning every moved field makes the whole usable again.
- Moving out of a place that does not own its value is an error (`own.move-not-owned`): a read or `mut` parameter, a [view binding](#view-bindings), a [part](#projection-results), a capture as [captures](#captures) states, a name a `for` pattern binds to an element it does not own ([iteration](#iteration)), or a field path from one of these.

The **end of scope** of an owner is where the values it still holds are released:

- for a binding, the exit of the block declaring it; for a parameter, the exit of its function;
- for a temporary, the end of its statement, which for a `for` statement is the exit of the loop; for one the value of a branch of an `if` or `match` creates, the end of that branch; and for one a `while` condition, a guard or the right operand of `and` or `or` creates, the end of its evaluation;
- for the value a place held before an assignment, that assignment, after the new value is evaluated;
- for a `take` capture of a closure that is not a task closure, the return of the call the closure is passed to.

At an exit, the pending [deferred calls](17-suspension-io.md#defer) of every block being left run first, as [registration and exits](17-suspension-io.md#registration-and-exits) orders them; then every value those blocks' owners still hold is released. Releasing a value releases its unmoved fields, elements and payloads and returns its memory to the store it came from ([stores](12-memory.md#stores)). Release runs no user code and never waits, fails or traps. A value moved on some paths is released exactly on the paths where it was not moved. A [trap](11-errors-domains.md#traps) releases nothing.

```nan
pub closed struct Frame {
    pub seq: u32
    pub payload: mem::List[u8]
} Frame

pub fn split_frame(take frame: Frame, mut seqs: mem::List[u32]) -> mem::List[u8] {
    mut seqs.push(frame.seq)
    let payload = frame.payload
    return payload
} split_frame
```

```nan
pub closed struct Frame {
    pub seq: u32
    pub payload: mem::List[u8]
} Frame

pub fn queue_frame(take frame: Frame, mut queue: mem::List[Frame]) -> u32 {
    mut queue.push(take frame)
    return frame.seq // error: own.use-after-move
} queue_frame
```

```nan
pub closed struct Frame {
    pub seq: u32
    pub payload: mem::List[u8]
} Frame

pub fn payload_of(frame: Frame) -> mem::List[u8] {
    return frame.payload // error: own.move-not-owned
} payload_of
```

## Modes

A **mode** is how a parameter receives its argument: read, the default, `mut` or `take`. The mode is written alike before a parameter in a [head](02-items.md#heads), before an argument at a call and before a capture.

```ebnf
mode = "mut" | "take" ;
```

| Mode | The callee | The caller after the call |
| --- | --- | --- |
| read | reads the argument; never assigns it, accesses it by `mut` or moves it | keeps the value unchanged |
| `mut` | reads, assigns and accesses by `mut` the caller's place; never moves it | keeps the value, possibly changed |
| `take` | owns the value | has a moved place, unless the value is Copy |

What a binding allows:

| Binding | Assigned or accessed by `mut` | Moved |
| --- | --- | --- |
| `let` binding, [pattern binding](06-statements-expressions.md#bindings), a name the pattern of `for p in e` binds | no | when it owns its value |
| read parameter | no | no |
| `var` binding, `take` parameter, a name the pattern of `for p in take e` binds | yes | yes |
| `mut` parameter, a name the pattern of `for p in mut e` binds | yes | no |

- Assigning a binding the table marks "no", or a field path from one, or accessing either by `mut`, is an error (`mode.immutable-binding`). When the binding is the one name of a `let` statement whose pattern is that name, the exact fix replaces `let` with `var`.
- Moving a binding the table marks "no" is an error (`own.move-not-owned`).
- `self`, `mut self` and `take self` are a read, a `mut` and a `take` parameter.

### Marking

The [markers](06-statements-expressions.md#markers) `mut` and `take` are the modes written before an argument or a receiver. Markers fixes where a mode stands, including before a chain of projection calls; this section fixes which mode each argument needs.

- An argument or receiver passed to a `mut` parameter is marked `mut`, Copy or not.
- An argument or receiver passed to a `take` parameter is marked `take` when its type is not Copy. A Copy value passed to a `take` parameter is copied and unmarked; marking it is an error (`mode.copy-taken`), whose exact fix deletes the marker. A generic parameter's type is Copy only when bounded by `Copy`.
- An argument or receiver passed to a read parameter is unmarked.
- An argument or receiver that is not a [place](#places), such as a call result, construction, literal, operator result, closure or path naming an item, is unmarked whatever the parameter's mode. Passed by `mut`, a [part](#projection-results) is the place it names, and any other such value is a temporary.
- A marker before a chain of projection calls states the access to the place at the chain's root: `mut` when a call of the chain takes `mut self` of that place or of a part of it, and no marker otherwise.
- A missing marker is an error (`mode.unmarked`), whose exact fix inserts it. Any other marker that these rules do not require is an error (`mode.mismatch`), whose exact fix replaces or deletes it.
- Construction values and payloads are marked like `take` arguments ([construction](04-types.md#construction)), except the values that [projection results](#projection-results) leaves unmarked.

```nan
pub closed struct Gauge {
    pub level: u32
    pub history: mem::List[u32]
} Gauge

pub fn Gauge.record(mut self, level: u32) {
    mut self.history.push(level)
    self.level = level
} Gauge.record

pub fn Gauge.into_history(take self) -> mem::List[u32] {
    return self.history
} Gauge.into_history

pub fn final_history(take gauge: Gauge, level: u32) -> mem::List[u32] {
    mut gauge.record(level)
    return take gauge.into_history()
} final_history
```

```nan
pub closed struct Frame {
    pub bytes: Array[u8, 4]
} Frame

pub fn Frame.clear(mut self) {
    mut self.bytes.view_mut().fill(0)
} Frame.clear

pub fn clear_first(mut frames: mem::List[Frame]) {
    mut frames.at_mut(0).clear()
} clear_first
```

```nan
pub closed struct Gauge {
    pub level: u32
} Gauge

pub fn Gauge.set(mut self, level: u32) {
    self.level = level
} Gauge.set

pub fn reset(mut gauge: Gauge) {
    gauge.set(0) // error: mode.unmarked
} reset
```

```nan
pub fn keep_level(take level: u32) -> u32 {
    return level
} keep_level

pub fn relay(level: u32) -> u32 {
    return keep_level(take level) // error: mode.copy-taken
} relay
```

```nan
pub closed struct Gauge {
    pub level: u32
} Gauge

pub fn Gauge.set(mut self, level: u32) {
    self.level = level
} Gauge.set

pub fn reset_local(level: u32) -> u32 {
    let gauge = Gauge(level: level)
    mut gauge.set(0) // error: mode.immutable-binding
    return gauge.level
} reset_local
```

## Places

A **place** is a storage location named by a root and a path of fields and indexes from it. The root is a local binding, a parameter, `self`, a capture or a name a `for` pattern binds; items, consts and call results are never places.

```ebnf
place = place_root place_step* ;
place_root = IDENT | "self" ;
place_step = "." IDENT | "[" expr "]" ;
```

- The **place path** of a place is its root followed by the field names before its first index step. A place with an index step names the element that [indexing](#indexing) projects, and its accesses are accesses to its place path through that projection.
- Two place paths **overlap** when one is a prefix of the other, and are **disjoint** otherwise: `a.x` and `a.y` are disjoint, `a` and `a.x` overlap, and `xs[0]` and `xs[1]` both have the place path `xs`.

## Exclusivity

**Exclusivity** is the static rule that a `mut` or `take` access to a place excludes every overlapping access. An **access** is a read, a `mut` access, a move or an assignment of a place path. Each access is active over a span:

- A place passed as an argument or receiver is resolved where the [evaluation order](06-statements-expressions.md#evaluation-order) evaluates it: its index expressions run, with the projections they call. Its access, and the move of a `take` argument, become active when the function starts running, after every argument is evaluated, and the access lasts until the function returns.
- A Copy value passed by read or `take` is copied where it is evaluated, and its access ends there.
- An assignment's access is active when it stores, after its value is evaluated.
- Any other read or move is active where it is evaluated.
- A [hold](#holds) is active while its projection result is live.
- A closure's captures are accessed while the closure is live ([captures](#captures)).

Two active accesses to overlapping place paths, at least one of them not a read, are an error (`own.conflict`). Disjoint fields are independent. The checker decides exclusivity per item; no run-time check of it exists. Inside a function or closure body, a `mut` parameter or capture overlaps no other parameter or capture.

```nan
pub closed struct Totals {
    pub sent: u64
    pub received: u64
} Totals

pub fn shift(mut from: u64, mut to: u64, amount: u64)
    requires amount <= from
{
    from -= amount
    to += amount
} shift

pub fn settle(mut totals: Totals) {
    let pending = totals.sent
    shift(mut totals.sent, to: mut totals.received, amount: pending)
} settle

pub fn record_count(mut counts: mem::List[usize]) {
    mut counts.push(counts.len())
} record_count
```

```nan
pub closed struct Totals {
    pub sent: u64
    pub received: u64
} Totals

pub fn shift(mut from: u64, mut to: u64, amount: u64)
    requires amount <= from
{
    from -= amount
    to += amount
} shift

pub fn drain(mut totals: Totals) {
    shift(mut totals.sent, to: mut totals.sent, amount: 1) // error: own.conflict
} drain
```

## Projections

A **projection** is a function with the `proj` [modifier](02-items.md#modifiers), whose result derives only from its receiver. A read projection takes `self` and a mut projection `mut self`; a `proj fn` whose first parameter is neither is an error (`own.proj-receiver`).

### Projection results

The result of a projection has one of three forms, fixed by the result type `R` of its head, judged with every type parameter counted as no view:

- a **view value** when `R` is a [view](05-properties.md#views) type: a value of `R` that the projection builds and that its holder owns;
- a **wrapped part** when `R` is another `Option` or `Result` type: an `Option` or `Result` whose non-Copy payloads are parts;
- a **part** otherwise: a part of the receiver, named in place.

A value **derives from** the receiver when it is the receiver, a field of a value that derives from it, a result of a projection whose receiver derives from it, a name a pattern binds to a part of a value that derives from it, `Some`, `Ok` or `Err` of a part that derives from it, a construction of a view type whose non-Copy values derive from it, or a value-yielding `if` or `match` whose branches each yield a value that derives from it or diverge. A trusted expression's result in a projection derives from the receiver, resting on its [premise](23-low-level.md#trusted-code).

In the body of a projection:

- A result that is not Copy, and each non-Copy value a result holds, derive from the receiver; one that does not is an error (`own.proj-source`). The result of a mut projection whose form is a part derives from the receiver even when its type is Copy. Other Copy values are unrestricted. Returning a value from a projection never moves it.
- A field path of a view type that derives from the receiver, the receiver alone included, gives a new view of the memory it refers to where it is returned or is a construction value of the returned value; it is then neither moved nor marked. Its type is `Slice`, `Str`, a state token or, in a mut projection, `SliceMut` or `Dyn[I]`. A `SliceMut` or `Dyn[I]` field path there in a read projection is an error (`own.read-only-view`), and one of any other view type is an error (`own.proj-source`).
- In a mut projection, each value that these rules require to derive from the receiver derives from it through fields, constructions, `Some`, `Ok`, `Err`, mut projections and trusted expressions only. Deriving one through a read projection, through a `next` that yields read-only parts, or through a pattern binding is an error (`own.read-only-view`).

At a call of a projection:

- The call is a read access to the receiver for a read projection and a `mut` access for a mut projection. A non-Copy result keeps that access active as a [hold](#holds).
- A result whose type is Copy at the call holds nothing and is copied when used as a value. The exception is a part written through [indexing](#indexing) or passed to a `mut` parameter: it is the place it projects, and its hold lasts until the end of its statement.
- A part is never moved (`own.move-not-owned`). It is bound as a [view binding](#view-bindings), used within its statement as a receiver, the object of a field access, a read or `mut` argument or a matched value, discarded, returned from a projection, or wrapped, unmarked, in `Some`, `Ok` or `Err`, which builds a wrapped part.
- A wrapped part is bound as a view binding, used within its statement as a read receiver, a read argument or an operand of an operator, used as a matched value, discarded or returned from a projection; any other use is an error (`own.wrapped-part-use`). A pattern matching a part or a wrapped part is matching a scrutinee that is not owned, so the names it binds are view bindings ([pattern bindings](04-types.md#pattern-bindings)).
- A view value is owned and moved like any value, and every value it moves into derives from it.
- A part is **writable** when it is the result of a mut projection, a payload of a wrapped part that a mut projection returns, or a field of a writable part, except a part that the `next` of a [read cursor](#iteration) yields or that `core::Cursor.next` yields through a bound or `Dyn[I]`. Every other part is **read-only**. Assigning a read-only part or a field of one, or accessing either by `mut`, is an error (`own.read-only-view`).
- A view value that the `next` of a read cursor yields, or that `core::Cursor.next` yields through a bound or `Dyn[I]`, is read-only, and so is every value derived from it: accessing one by `mut` is an error (`own.read-only-view`).
- A non-Copy result of a projection whose receiver is a temporary, and every value derived from it, is used only before that temporary's end of scope; binding one to a name whose scope extends past it, or yielding one as the value of the branch that creates the temporary, is an error (`own.temporary-source`).

A function without `proj` may implement a `proj` interface method when its result type is droppable; otherwise the implementing function is an error (`interface.head-mismatch`, [conformance](08-generics-interfaces.md#conformance)). Called through its own type, its result is owned. Called through the interface, a bound or `Dyn[I]`, its result has the form the interface method's head fixes, and the caller releases it when it is no longer live.

```nan
pub closed struct Frame {
    pub seq: u32
    pub payload: mem::List[u8]
} Frame

pub closed struct Inbox {
    pub frames: mem::List[Frame]
    pub dropped: u32
} Inbox

pub proj fn Inbox.newest(self) -> Option[Frame] {
    let count = self.frames.len()
    if count == 0 {
        return None
    }
    return Some(self.frames[count - 1])
} Inbox.newest

pub fn newest_seq(inbox: Inbox) -> Option[u32] {
    let Some(frame) = inbox.newest() else {
        return None
    }
    return Some(frame.seq)
} newest_seq
```

```nan
pub closed struct Frame {
    pub seq: u32
    pub payload: mem::List[u8]
} Frame

pub closed struct Inbox {
    pub frames: mem::List[Frame]
} Inbox

pub proj fn Inbox.payload_from(self, other: Frame) -> mem::List[u8] {
    return other.payload // error: own.proj-source
} Inbox.payload_from
```

```nan
pub closed struct Frame {
    pub seq: u32
    pub payload: mem::List[u8]
} Frame

pub fn first_seq(alloc: mem::Alloc) -> u32 {
    let head = mem::List[Frame].empty(alloc).at(0) // error: own.temporary-source
    return head.seq
} first_seq
```

```nan
pub closed struct Frame {
    pub seq: u32
    pub payload: mem::List[u8]
} Frame

pub fn seq_of(frame: Frame) -> u32 = frame.seq

pub fn first_seq(empty: bool, alloc: mem::Alloc, frames: mem::List[Frame]) -> u32 {
    return seq_of(match empty {
        true => mem::List[Frame].empty(alloc).at(0) // error: own.temporary-source
        false => frames.at(0)
    })
} first_seq
```

### Indexing

An index expression `x[i]` is a call to the projection `at`, or to `at_mut` where the element is written: where it, or a field path from it, is an assignment or compound assignment target, a `mut` argument, the place of a `mut` receiver or the iterated operand of the `mut` form of `for`, and, in a mut projection, where it is returned or is a value of the returned value.

- `x[i]` calls the `at` or `at_mut` that [method resolution](08-generics-interfaces.md#method-resolution) finds on the type of `x`, with `i` as its second argument. That function is a `proj fn` taking `self` (for `at`) or `mut self` (for `at_mut`) and exactly one more parameter; otherwise the index expression is an error (`own.not-indexable`).
- `x[i] = v` stores through `at_mut` in the order [evaluation order](06-statements-expressions.md#evaluation-order) fixes: the old element is released and `v` moved in.
- Each call of `at` or `at_mut` of an `Array`, `Slice` or `SliceMut`, or of a list, bytes or bounded list ([collections](12-memory.md#collections)), written `x[i]` or as a method call, has the index-bound [obligation](18-contracts.md#obligations) `i < x.len()` at the call, after its arguments are evaluated; a kept check traps (`trap.bounds`). The functions themselves never trap.

```nan-api
// module: core

pub proj fn Array.at(self, i: usize) -> T limits pure, notrap, terminates

pub proj fn Array.at_mut(mut self, i: usize) -> T limits pure, notrap, terminates

pub proj fn Slice.at(self, i: usize) -> T limits pure, notrap, terminates

pub proj fn SliceMut.at(self, i: usize) -> T limits pure, notrap, terminates

pub proj fn SliceMut.at_mut(mut self, i: usize) -> T limits pure, notrap, terminates
```

```nan
pub fn count_hit(mut counts: Array[u32, 4], slot: usize)
    requires slot < 4
{
    counts[slot] += 1
} count_hit
```

```nan
pub fn third(data: Slice[u8]) -> u8 {
    return data[2] // traps: trap.bounds
} third

test short_input for third {
    expect third(b"ab") == 0
} short_input
```

## View bindings

A **view binding** is a local bound to a part or a wrapped part, or a name a pattern binds to a non-Copy part of a scrutinee that is not owned: a part, a wrapped part, or a place that does not own its value as [ownership](#ownership) lists. It names what it is bound to and is never its owner: it is never moved (`own.move-not-owned`) and never released.

- A binding initialised from a view binding is another view binding of the same part, derived from the first.
- Assigning a view binding writes the part it names, and accessing it by `mut` accesses that part: the binding is a `var` or a name of the pattern of `for p in mut e` (`mode.immutable-binding`), and the part is writable (`own.read-only-view`). A view binding of a wrapped part is never assigned or accessed by `mut` (`own.read-only-view`).
- A local bound to a view value is an ordinary binding that owns that value.

```nan
pub closed struct Frame {
    pub seq: u32
    pub payload: mem::List[u8]
} Frame

pub closed struct Inbox {
    pub frames: mem::List[Frame]
    pub dropped: u32
} Inbox

pub fn renumber_first(mut inbox: Inbox, seq: u32) {
    if inbox.frames.len() == 0 {
        return
    }
    var head = mut inbox.frames.at_mut(0)
    inbox.dropped = 0
    head.seq = seq
} renumber_first
```

```nan
pub closed struct Frame {
    pub seq: u32
    pub payload: mem::List[u8]
} Frame

pub fn renumber_first(frames: mem::List[Frame], seq: u32) {
    if frames.len() == 0 {
        return
    }
    var head = frames.at(0)
    head.seq = seq // error: own.read-only-view
} renumber_first
```

A payload of a writable wrapped part is written through a `var` view binding initialised from the name a pattern binds to it.

```nan
pub fn append_reading(mut readings: mem::Map[u32, mem::List[u16]], sensor: u32, level: u16) -> bool {
    let Some(found) = mut readings.get_mut(sensor) else {
        return false
    }
    var history = found
    mut history.push(level)
    return true
} append_reading
```

## Holds

A **hold** is the restriction a live projection result places on its **source**, the place path of the projection's receiver. When the receiver is a projection result or derives from one, that result's hold stays active as well. After a read projection the source is **frozen**: it is only read. After a mut projection it is **held**: it is reached only through the result.

A value is derived from a projection result when it derives from it as [projection results](#projection-results) define with the result as receiver, is a name a pattern binds to a value it holds, is a value it moves or is built into, is a call result that [view positions](05-properties.md#view-positions) derives from it, or is a closure capturing it; a copy of a Copy part is not derived. A projection result is **live** at a point of its item's control-flow graph when a path from that point reaches a use of it or of a value derived from it. The graph includes loop back edges, the span of each call that a closure capturing it is passed to, and each scope exit whose deferred calls use it. Releasing a value is not a use.

- A hold is active exactly while its projection result is live; block structure does not extend it.
- A `mut` access, move or assignment of a place path overlapping a frozen source, and any access to a place path overlapping a held source other than through the result, is an error (`own.conflict`).
- The end of scope of an owner whose place path overlaps the source of an active hold is an error (`own.conflict`).
- Assigning a value derived from a projection result to a place whose root is a `mut` parameter, `self` received by `mut`, or a `mut` capture, a lent `mut` place of a [task closure](16-concurrency.md#lending) included, is an error (`own.view-escape`).
- A non-Copy projection result used without a binding is live until the end of its statement.

```nan
pub closed struct Frame {
    pub seq: u32
    pub payload: mem::List[u8]
} Frame

pub closed struct Inbox {
    pub frames: mem::List[Frame]
} Inbox

pub proj fn Inbox.newest(self) -> Option[Frame] {
    let count = self.frames.len()
    if count == 0 {
        return None
    }
    return Some(self.frames[count - 1])
} Inbox.newest

pub fn add_after_newest(mut inbox: Inbox, take extra: Frame) -> usize {
    let Some(newest) = inbox.newest() else {
        return 0
    }
    let size = newest.payload.len()
    mut inbox.frames.push(take extra)
    return size
} add_after_newest
```

```nan
pub closed struct Frame {
    pub seq: u32
    pub payload: mem::List[u8]
} Frame

pub closed struct Inbox {
    pub frames: mem::List[Frame]
} Inbox

pub proj fn Inbox.newest(self) -> Option[Frame] {
    let count = self.frames.len()
    if count == 0 {
        return None
    }
    return Some(self.frames[count - 1])
} Inbox.newest

pub fn add_after_newest(mut inbox: Inbox, take extra: Frame) -> usize {
    let Some(newest) = inbox.newest() else {
        return 0
    }
    mut inbox.frames.push(take extra) // error: own.conflict
    return newest.payload.len()
} add_after_newest
```

### Conflict reports

An `own.conflict` diagnostic is reported on the line of the access and quotes three lines in its [because-chain](34-diagnostics.md#because-chain), in this order:

1. the hold: a `frozen` or `held` reason, its subject the source, at the line where the projection result, capture or earlier argument began its access;
2. the use keeping the hold live: a `used` reason at the first use of the result or of a value derived from it on a path from the access, reached through a back edge when the path takes one; for two arguments or captures of one call, at the call itself;
3. the access: an `accessed` reason, its subject written with the access's mode; an end of scope is quoted at the line closing the scope.

The report for `note_drops` quotes `seen += newest.payload.len()` as the keeping use, reached from the access through the loop's back edge.

```nan
pub closed struct Frame {
    pub seq: u32
    pub payload: mem::List[u8]
} Frame

pub closed struct Inbox {
    pub frames: mem::List[Frame]
    pub dropped: u32
} Inbox

pub proj fn Inbox.newest(self) -> Option[Frame] {
    let count = self.frames.len()
    if count == 0 {
        return None
    }
    return Some(self.frames[count - 1])
} Inbox.newest

pub fn note_drops(mut inbox: Inbox, rounds: u32) -> usize {
    let Some(newest) = inbox.newest() else {
        return 0
    }
    var seen: usize = 0
    for _ in 0..<rounds {
        seen += newest.payload.len()
        inbox.dropped += 1 // error: own.conflict
    }
    return seen
} note_drops
```

## Linear consumption

A [linear](05-properties.md#linear-types) value is consumed as that section defines. A linear value dropped on some path is an error (`own.linear-dropped`), reported where it is dropped, with a `bound` reason for the line that bound or created it. Paths that end in a trap are not considered. A linear value is dropped when, on some path:

- its owner's scope ends with the value still owned, by a block's end, `break`, `continue`, `return`, `fail` or a `try` that propagates a failure, and no [deferred call](17-suspension-io.md#defer) pending at that exit consumes it; the owner may be a binding, a parameter, a temporary or a `take` capture;
- `discard` ([expression statements](06-statements-expressions.md#expression-statements)) receives it;
- a `_` pattern or an `else =>` arm matching an owned scrutinee receives it without binding it;
- an assignment overwrites the place holding it;
- a `for` loop whose cursor is linear exits other than by its cursor yielding `None`. A loop consumes its linear cursor when `next` yields `None`.

A value moved or consumed on some paths only is dropped on the others.

```nan
pub closed linear struct Lease {
    pub slot: u32
} Lease

pub fn Lease.release(take self) -> u32 {
    let Lease(slot: slot) = self
    return slot
} Lease.release

pub fn release_all(take leases: Array[Lease, 4]) -> u32 {
    var freed: u32 = 0
    for lease in take leases {
        freed += take lease.release()
    }
    return freed
} release_all
```

```nan
pub closed linear struct Lease {
    pub slot: u32
} Lease

pub fn halt() -> Never {
    loop {
    }
} halt

pub fn hold_until_halt(take lease: Lease) -> Never {
    halt()
} hold_until_halt
```

```nan
pub closed linear struct Lease {
    pub slot: u32
} Lease

pub closed enum SlotError derives Eq, Render {
    Busy
} SlotError

pub fn Lease.release(take self) -> u32 {
    let Lease(slot: slot) = self
    return slot
} Lease.release

pub fn check_slot(slot: u32) fails SlotError {
    if slot > 7 {
        fail SlotError.Busy
    }
} check_slot

pub fn reserve(slot: u32) -> u32 fails SlotError {
    let lease = Lease(slot: slot)
    try check_slot(slot) // error: own.linear-dropped
    return take lease.release()
} reserve
```

## Iteration

A **cursor type** is a type with a method `next` whose head is `next(mut self) -> Option[T]`, with or without `proj`, and states neither `fails` nor `uses wait`. A **cursor** is a value of a cursor type; its `next` yields the elements of an iteration one at a time, and `None` after the last. `core::Cursor[T]` is the interface of cursor types.

```nan-api
// module: core

pub interface Cursor[T: Any] {
    proj fn next(mut self) -> Option[T]
} Cursor

pub open view struct SliceCursor[T: Any] {
} SliceCursor

impl SliceCursor: Cursor[T]

pub proj fn SliceCursor.next(mut self) -> Option[T] limits pure, notrap, terminates

pub open view struct SliceCursorMut[T: Any] {
} SliceCursorMut

impl SliceCursorMut: Cursor[T]

pub proj fn SliceCursorMut.next(mut self) -> Option[T] limits pure, notrap, terminates

pub open struct ArrayCursor[T: Any, N: usize] {
} ArrayCursor

impl ArrayCursor[T, N]: Cursor[T]

pub fn ArrayCursor.next(mut self) -> Option[T] limits pure, notrap, terminates

pub proj fn Array.cursor(self) -> SliceCursor[T] limits pure, notrap, terminates

pub proj fn Array.cursor_mut(mut self) -> SliceCursorMut[T] limits pure, notrap, terminates

pub fn Array.into_cursor(take self) -> ArrayCursor[T, N] limits pure, notrap, terminates

pub proj fn Slice.cursor(self) -> SliceCursor[T] limits pure, notrap, terminates

pub proj fn SliceMut.cursor(self) -> SliceCursor[T] limits pure, notrap, terminates

pub proj fn SliceMut.cursor_mut(mut self) -> SliceCursorMut[T] limits pure, notrap, terminates
```

- `SliceCursor[T]` yields the elements of the array or slice it was made from, in index order, as parts. It is a **read cursor**: the parts its `next` yields are read-only ([projection results](#projection-results)). Each std cursor type that its owning chapter declares a read cursor is one too.
- `SliceCursorMut[T]` yields the same elements as writable parts.
- `ArrayCursor[T, N]` owns the elements of the array it was made from and yields them, owned, in index order; releasing it releases the elements not yet yielded. Its impl line covers droppable `T` only ([impl lines](08-generics-interfaces.md#impl-lines)).
- These are [base types](05-properties.md#structural-properties). `SliceCursor[T]` and `SliceCursorMut[T]` are views, never linear, Copy or AnyBits, and Send and Inert exactly when `T` is. `ArrayCursor[T, N]` is linear, a view, Send and Inert exactly when `T` is, and never Copy or AnyBits.
- `core::Range[T]` is a cursor type for integer T and `char` ([ranges](09-numbers.md#ranges)).

A `for` statement ([conditionals and loops](06-statements-expressions.md#conditionals-and-loops)) obtains its cursor from its iterated expression. A mode marker at the start of the iterated expression selects the form of the loop, and `e` is the rest of the expression:

- `for p in e`: when the type of `e` is a cursor type, the cursor is `e` itself, moved or copied into the loop. Otherwise the cursor is the result of `e.cursor()`, so `e` stays frozen while the loop runs.
- `for p in mut e`: the cursor is the result of `mut e.cursor_mut()`, so `e` stays held while the loop runs. That cursor's `next` is a projection.
- `for p in take e`: the cursor is `take e.into_cursor()`, so `e` moves into it. That cursor's `next` is not a projection.

Each call is checked as if written there ([marking](#marking)). An iterated expression whose type lacks the function its form calls, or whose cursor is a part, a wrapped part, not of a cursor type, or of a type whose `next` its form excludes, is an error (`own.not-iterable`). The loop owns its cursor until it exits and releases it then. Each iteration calls the `next` of the cursor's own type: `None` ends the loop, and `Some(v)` matches `v` against the irrefutable pattern `p` ([refutability](04-types.md#refutability)) and runs the body. What the names of `p` bind:

- in the `mut` form, writable places of the parts of `v`, Copy or not, so assigning one writes into `e`;
- in the other forms, when `next` is a projection, copies of Copy parts and read-only view bindings of other parts;
- when `next` is not a projection, the parts of `v`, which the loop owns and the pattern moves; an owned element still held at the end of its iteration is released there.

[Modes](#modes) states what each name allows.

```nan
pub fn total(values: Slice[u32]) -> u64 {
    var sum: u64 = 0
    for value in values {
        sum += u64.widen(value)
    }
    return sum
} total

pub fn clamp_all(mut levels: Array[u8, 8], ceiling: u8) {
    for level in mut levels {
        if level > ceiling {
            level = ceiling
        }
    }
} clamp_all
```

```nan
pub fn clamp_all(levels: Array[u8, 8], ceiling: u8) {
    for level in mut levels { // error: mode.immutable-binding
        if level > ceiling {
            level = ceiling
        }
    }
} clamp_all
```

## Closures

A **closure** is a function expression; a **capture** is a place of the enclosing body that a closure's body names. A closure has a short form `fn(x: T) => e` or a long form with a head and a block, each with an optional capture list.

```ebnf
closure_expr = "fn" capture_list? "(" fn_type_param_list? ")" closure_body ;
closure_body = "=>" expr | fn_result? fails_clause? uses_clause? limits_clause? block ;
capture_list = "[" capture ( "," capture )* "]" ;
capture = mode place_root ;
```

- Parameters are written as in [function types](10-effects-digests.md#function-types), each with its mode, name and type.
- In the short form, the result type of the parameter's function type is the [expected type](04-types.md#expected-types-and-literals) of `e`, and the closure's result type is the type of `e`. An `e` that may fail or wait is an error (`closure.short-effect`), whose exact fix writes the long form.
- In the short form, `e` is a whole `expr` and extends as far as an expression continues ([one-token lookahead](41-grammar.md#one-token-lookahead)): `fn(x: u32) => x > floor` returns `x > floor`.
- The long form's head follows the rules of [heads](02-items.md#heads) for `-> R`, `fails` and `uses wait`, with the same codes, applied to the closure's own body. `return` in a closure body returns from the closure.
- A closure's type is the function type of its head. A closure is written only as an argument whose parameter has a function type; passed for a `task fn` parameter it is a [task closure](16-concurrency.md#task-closures). A closure anywhere else is an error (`closure.second-class`).

### Captures

- A capture absent from the list is a read. A Copy read capture is copied when the closure is created. Any other read capture is a read access to each place path the body reads through it, while the closure is live.
- `mut a` is a `mut` access to `a` while the closure is live; the body uses `a` as a `mut` parameter's place.
- `take b` moves `b` into the closure when it is created. In a closure that is not a task closure, the body uses `b` as a `mut` parameter's place. A task closure's captures follow [task closures](16-concurrency.md#task-closures) and [lending](16-concurrency.md#lending).
- A closure that is not a task closure is live from its creation until the call it is passed to returns, and its capture accesses conflict with that call's receiver and other arguments as [exclusivity](#exclusivity) states.
- A closure nested in another closure reaches a place of the outer closure's enclosing body through the outer closure's capture of it, which these rules judge as a capture of the outer closure accessed by the inner one.
- A name appearing twice in a capture list is an error (`closure.capture-duplicate`). A listed capture the body never uses is an error (`closure.capture-unused`), whose exact fix deletes it.
- A body that assigns or accesses by `mut` a read capture is an error (`closure.capture-mode`), whose exact fix lists it with `mut`. A task closure's body that moves a read capture is an error (`closure.capture-mode`), whose exact fix lists it with `take`. Any other move of a capture, except of a task closure's `take` capture, is an error (`own.move-not-owned`).
- A `mut` capture that the body uses but never assigns or accesses by `mut` is an error (`closure.capture-mode`), whose exact fix deletes it from the list. A `take` capture of a Copy value is an error (`mode.copy-taken`).
- Captures obey the binding table of [modes](#modes) (`mode.immutable-binding`, `own.move-not-owned`).

### Function values

A path or member path naming a function item other than a projection is a value of that function's [type](10-effects-digests.md#function-types), whose properties [Structural properties](05-properties.md#structural-properties) states; it is bound, stored, passed and returned like any value. A path or member path naming a projection used as a value is an error (`closure.projection-value`).

A closure that is not a task closure, and a parameter of a function type other than `task fn`, is **second-class**: it is called, captured by read, or passed as an argument for a read parameter of a function type, and nothing else. Binding one, storing it in a field or a construction, returning it, capturing it by `mut` or `take`, or passing it to a `mut` or `take` parameter is an error (`closure.second-class`). A call through a function value is marked as [marking](#marking) states.

```nan
pub fn each_value(values: Slice[u32], visit: fn(value: u32)) {
    for value in values {
        visit(value)
    }
} each_value

pub fn sum_above(values: Slice[u32], floor: u32) -> u64 {
    var total: u64 = 0
    each_value(values, visit: fn[mut total](value: u32) {
        if value > floor {
            total += u64.widen(value)
        }
    })
    return total
} sum_above

pub fn count_where(values: Slice[u32], keep: fn(x: u32) -> bool) -> usize {
    var count: usize = 0
    for value in values {
        if keep(value) {
            count += 1
        }
    }
    return count
} count_where

pub fn count_above(values: Slice[u32], floor: u32) -> usize {
    return count_where(values, keep: fn(x: u32) => x > floor)
} count_above

pub fn double(value: u32) -> u32 {
    return value.saturating_mul(2)
} double

pub closed struct Stage {
    pub apply: fn(value: u32) -> u32
} Stage

pub fn doubling_stage() -> Stage {
    return Stage(apply: double)
} doubling_stage
```

```nan
pub fn each_value(values: Slice[u32], visit: fn(value: u32)) {
    for value in values {
        visit(value)
    }
} each_value

pub fn count_nonzero(values: Slice[u32]) -> u32 {
    var count: u32 = 0
    each_value(values, visit: fn(value: u32) {
        if value > 0 {
            count += 1 // error: closure.capture-mode
        }
    })
    return count
} count_nonzero
```

```nan
pub closed struct Stage {
    pub apply: fn(value: u32) -> u32
} Stage

pub fn lowering_stage(floor: u32) -> Stage {
    return Stage(apply: fn(value: u32) => value.saturating_sub(floor)) // error: closure.second-class
} lowering_stage
```

## Result location

The **result location** of a value is the destination it is built in: the binding it initialises, the caller's destination for a returned value, the parameter of a `take` argument, a field of a construction, or a store slot.

- A construction, variant construction or call of a function other than a projection whose value goes to a destination builds it there: its fields are written in place, and the callee builds its returned value in the caller's destination. No intermediate copy exists.
- A place moved to a destination is moved into it.
- Assigning a place that holds a value is not a destination: the value is built, then the old value is released and the new one moved in.
- When a failure propagates while a construction's values are evaluated, the values already evaluated are released, which [linear consumption](#linear-consumption) rejects when one is linear, and the destination stays uninitialised.

```nan
pub closed struct Frame {
    pub seq: u32
    pub payload: mem::List[u8]
} Frame

pub fn Frame.empty(seq: u32, alloc: mem::Alloc) -> Frame {
    return Frame(seq: seq, payload: mem::List[u8].empty(alloc))
} Frame.empty
```

## Box

A **box** is a `mem::Box[T]`, which owns one `T` in memory drawn from the store that made it ([stores](12-memory.md#stores)). It is the only owned indirection.

```nan-api
// module: mem

pub open struct Box[T: Any] {
} Box

pub proj fn Box.contents(self) -> T limits pure, notrap, terminates

pub proj fn Box.contents_mut(mut self) -> T limits pure, notrap, terminates

pub fn Box.into_contents(take self) -> T limits pure, notrap, terminates
```

- A store's `boxed` makes a box.
- The contents of a box never move while the box exists: moving the box leaves its contents where they are, and their address is fixed from the box's creation until its release.
- Releasing a box releases its contents and returns their memory. `into_contents` moves the contents out and returns the memory.
- `mem::Box[T]` is a base type: linear, Send and Inert exactly when `T` is, and never a view, Copy or AnyBits. `mem::Box[Dyn[I]]` is defined in [dynamic dispatch](08-generics-interfaces.md#dynamic-dispatch).

```nan
pub closed struct Table {
    pub slots: Array[u64, 512]
    pub used: usize
} Table

pub fn mark_used(mut table: mem::Box[Table]) {
    var contents = mut table.contents_mut()
    contents.used += 1
} mark_used

pub fn unbox_used(take table: mem::Box[Table]) -> usize {
    let inner = take table.into_contents()
    return inner.used
} unbox_used
```

## Diagnostics

| Code | Class | Rule |
| --- | --- | --- |
| `closure.capture-duplicate` | error | [Captures](#captures) |
| `closure.capture-mode` | error | [Captures](#captures) |
| `closure.capture-unused` | error | [Captures](#captures) |
| `closure.projection-value` | error | [Function values](#function-values) |
| `closure.second-class` | error | [Closures](#closures), [Function values](#function-values) |
| `closure.short-effect` | error | [Closures](#closures) |
| `mode.copy-taken` | error | [Marking](#marking), [Captures](#captures) |
| `mode.immutable-binding` | error | [Modes](#modes), [View bindings](#view-bindings), [Captures](#captures), [Mutable views](05-properties.md#mutable-views) |
| `mode.mismatch` | error | [Marking](#marking), [Templates](13-text-encode.md#templates) |
| `mode.unmarked` | error | [Marking](#marking) |
| `own.conflict` | error | [Exclusivity](#exclusivity), [Holds](#holds), [Lending](16-concurrency.md#lending) |
| `own.linear-dropped` | error | [Linear consumption](#linear-consumption), [Linear types](05-properties.md#linear-types), [Expression statements](06-statements-expressions.md#expression-statements), [Match arms](06-statements-expressions.md#match-arms) |
| `own.move-not-owned` | error | [Ownership](#ownership), [Modes](#modes), [Projection results](#projection-results), [View bindings](#view-bindings), [Captures](#captures) |
| `own.not-indexable` | error | [Indexing](#indexing) |
| `own.not-iterable` | error | [Iteration](#iteration) |
| `own.proj-receiver` | error | [Projections](#projections) |
| `own.proj-source` | error | [Projection results](#projection-results) |
| `own.read-only-view` | error | [Projection results](#projection-results), [View bindings](#view-bindings) |
| `own.temporary-source` | error | [Projection results](#projection-results) |
| `own.use-after-move` | error | [Ownership](#ownership) |
| `own.view-escape` | error | [Holds](#holds) |
| `own.wrapped-part-use` | error | [Projection results](#projection-results) |
