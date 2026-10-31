# Untrusted and secret data

This chapter defines Untrusted values, `Untrusted.of` and the sources of Untrusted data, their opacity, the content-blind operations on them, validators and `inspect`, the parsing and declassifying classes of validators, decoders and uses of encoders, the `allow validate` grants that validators, decoders and the users of validators, decoders and encoders need, and Secret values with their constant-time operations and lowering.

## Untrusted values

**Untrusted** is the core type `Untrusted[T: Any]`: a value of `T` that the program treats as coming from outside it, and whose contents only a [validator](#validators) reads. It has no fields or variants that any code sees, it holds exactly a `T` value, and its structural properties follow `T` as [structural properties](05-properties.md#structural-properties) give. A **content-blind operation** is an operation on Untrusted values whose effects, traps and results outside Untrusted values depend on the contents only through their byte count.

```nan-api
// module: core

pub fn Untrusted.of(take value: T) -> Untrusted[T] limits pure, notrap, terminates
    example Untrusted.of(b"ok").byte_len() == 2

pub proj fn Untrusted.inspect(self) -> T limits pure, notrap, terminates

pub closed view struct Parts[T] {
    pub head: T
    pub tail: T
} Parts
```

- `Untrusted.of(v)` wraps a value, in any code, without copying it.
- `Untrusted.of` instantiated with a `T` that is or contains `SliceMut`, `Dyn`, a type with [interior mutability](12-memory.md#interior-mutability), `task::Sender` or `sys::Outbox`, or that contains a type parameter that is a [view](05-properties.md#views) or has no bound naming `Inert`, is an error (`untrusted.wrapped-alias`).
- `inspect` yields the contents in place as a read-only [part](07-ownership.md#projection-results), without copying or checking them. [Validators](#validators) restrict where it appears.
- Besides the methods of the interfaces it implements ([opacity](#opacity)), std declares exactly these members of Untrusted: `of` and `inspect`, for every `T`, and the [content-blind operations](#content-blind-operations), each only for the receivers its table row lists. On another instantiation the name is unknown (`name.unknown`).
- `core::Parts[T]` holds the two parts that a split yields.

```nan
pub fn payload_length(frame: Untrusted[Slice[u8]]) -> usize
    example payload_length(Untrusted.of(b"HDR!data")) == 4
    example payload_length(Untrusted.of(b"HD")) == 0
{
    let Some(parts) = frame.split_at(4) else {
        return 0
    }
    return parts.tail.byte_len()
} payload_length
```

```nan
pub fn read_into(mut conn: sys::net::Conn, take window: SliceMut[u8]) -> usize fails sys::net::NetError uses wait {
    var target = Untrusted.of(take window) // error: untrusted.wrapped-alias
    return try await mut conn.read(mut target)
} read_into
```

### Sources

Every std operation that delivers data from outside the program delivers it in an Untrusted value:

- bytes read from a connection, a file, standard input or firmware services, which arrive in the `Untrusted[SliceMut[u8]]` the read writes into or in the Untrusted value it returns ([std capabilities](14-authority.md#std-capabilities), [firmware services](24-kernel.md#firmware-services));
- the `args` and `environment` fields of `host::World` ([root grants](14-authority.md#root-grants));
- the values that `hw::UserAddr[T].copy_in` fetches from user memory ([address types](23-low-level.md#address-types)), and each syscall argument, as an `Untrusted[u64]` ([user exceptions and syscalls](24-kernel.md#user-exceptions-and-syscalls));
- the buffer of a [completion](17-suspension-io.md#completion-io) that a device or the kernel wrote.

Register values and the results of `hw::DmaShared` access calls are typed device state, never Untrusted.

```nan
pub closed enum ReadError derives Render {
    Net(sys::net::NetError) from
    Cancelled(task::Cancelled) from
} ReadError

pub fn read_frame(mut conn: sys::net::Conn, alloc: mem::Alloc) -> Untrusted[mem::Bytes] fails ReadError uses wait {
    var buf = Untrusted.of(mem::Bytes.zeroed(1_500, alloc))
    let count = try await mut conn.read(mut buf.view_mut())
    return buf.copy_prefix(count, alloc)
} read_frame
```

## Opacity

- Apart from the [property interfaces](05-properties.md#property-interfaces), which follow its structural properties, `Untrusted[T]` implements exactly three std interfaces: `Render` for every `T`, `Encode` when `T` implements `Encode`, and `Generate` when `T` implements `Generate` and is Inert. Comparing or ordering an Untrusted value is therefore an error (`interface.not-implemented`), as is passing it where a bound names another std interface (`generic.unsatisfied-bound`) or deriving such an interface for a type holding it (`derive.constituent-missing`).
- Its [render](13-text-encode.md#render) writes the text `<untrusted>`, whatever `T` and the contents are.
- Its [generator](22-testing.md#generation) generates a `T` value and wraps it.
- It [encodes](13-text-encode.md#std-encodings) as its contents, and its decoder decodes a `T` value and wraps it, so that what a decoder reads into it stays opaque.
- Only `inspect` yields the contents. `into_cleared` yields them only after setting every byte to 0, encoders write them out only under a [validate grant](#validate-grants), and `testing::same` compares them only in tests and examples ([simulated capabilities](22-testing.md#simulated-capabilities)).

```nan
pub fn note_frame(log: sys::Log, frame: Untrusted[mem::Bytes]) {
    log.info("frame received", value: frame)
} note_frame
```

```nan
pub fn is_ping(frame: Untrusted[Slice[u8]]) -> bool {
    return frame == Untrusted.of(b"PING") // error: interface.not-implemented
} is_ping
```

## Content-blind operations

The content-blind operations are exactly the members in this table, where `B` is the receiver's type argument. The **byte contents** are `Slice[u8]`, `SliceMut[u8]`, `Array[u8, N]` and `mem::Bytes`; the **writable byte contents** are `SliceMut[u8]`, `Array[u8, N]` and `mem::Bytes`.

| Head | Receivers `Untrusted[B]` |
| --- | --- |
| `fn byte_len(self) -> usize limits pure, notrap, terminates` | byte contents |
| `proj fn view(self) -> Untrusted[Slice[u8]] limits pure, notrap, terminates` | byte contents |
| `proj fn view_mut(mut self) -> Untrusted[SliceMut[u8]] limits pure, notrap, terminates` | writable byte contents |
| `proj fn prefix(self, count: usize) -> Option[Untrusted[Slice[u8]]] limits pure, notrap, terminates` | byte contents |
| `proj fn split_at(self, at: usize) -> Option[Parts[Untrusted[Slice[u8]]]] limits pure, notrap, terminates` | byte contents |
| `fn append(mut self, bytes: Untrusted[Slice[u8]]) limits terminates` | `mem::Bytes` |
| `fn try_append(mut self, bytes: Untrusted[Slice[u8]]) fails mem::OutOfMemory limits notrap, terminates` | `mem::Bytes` |
| `fn copy_prefix(self, count: usize, alloc: mem::Alloc) -> Untrusted[mem::Bytes] limits terminates` | byte contents |
| `fn try_copy_prefix(self, count: usize, alloc: mem::Alloc) -> Untrusted[mem::Bytes] fails mem::OutOfMemory limits terminates` | byte contents |
| `fn into_cleared(take self) -> B limits pure, notrap, terminates` | `Array[u8, N]`, `mem::Bytes`, `hw::Dma[T]` |

The facts below are the requires and ensures of these operations, available to the [prover](20-prover.md#callee-knowledge):

- `byte_len` is the number of bytes of the contents; for `Array[u8, N]` it is `N`.
- `view` and `view_mut` cover every byte, so their results have the receiver's `byte_len`.
- `prefix(count)` is `Some` exactly when `count <= self.byte_len()`, holding the first `count` bytes.
- `split_at(at)` is `Some` exactly when `at <= self.byte_len()`; its `head` holds the bytes before position `at` and its `tail` the rest.
- `append` adds the bytes of its argument after the contents, growing `byte_len` by the argument's `byte_len`. It traps when memory is exhausted (`trap.exhausted`); `try_append` fails with `mem::OutOfMemory.Exhausted` instead and leaves the receiver unchanged ([growth](12-memory.md#growth)).
- `copy_prefix` and `try_copy_prefix` require `count <= self.byte_len()`. They copy the first `count` bytes into a new `mem::Bytes` drawn from `alloc`, so the result's `byte_len` is `count`, exhaustion behaving as for `append` and `try_append`.
- `into_cleared` sets every byte of the contents to 0 and returns them; a `mem::Bytes` keeps its length.

In the clauses of a content-blind operation, its receiver has each content-blind operation whose receivers include all of its own.

```nan
pub fn joined(head: Untrusted[Slice[u8]], tail: Untrusted[Slice[u8]], alloc: mem::Alloc) -> Untrusted[mem::Bytes] {
    var message = head.copy_prefix(head.byte_len(), alloc)
    mut message.append(tail)
    return message
} joined
```

## Validators

A **validator** is a function item declared with the `validator` [modifier](02-items.md#modifiers), or an [implementing function](08-generics-interfaces.md#conformance) of a method of `core::Decoder`, which [decoders](13-text-encode.md#decoders-as-validators) call. Code reads Untrusted contents only through `inspect`, in validators.

- `Untrusted.inspect` appears only as the callee of a call written in the body, or in an `ensures`, `example` or `decreases` clause, of a validator, closures written there included. Any other use, including naming it as a function value, is an error (`untrusted.inspect-outside-validator`).
- A function declared `validator` has at least one parameter whose type is an Untrusted instantiation (`untrusted.validator-no-input`). These parameters are its **Untrusted inputs**.
- The `validator` modifier on an interface method, or on a function implementing an interface method, is an error (`untrusted.validator-interface`).
- The result of `inspect` is ordinary data. The validator may pass it, and values computed from it, to any function; such functions are not validators.
- In every other respect a validator is an ordinary function: it may be generic, fail, wait, allocate and be `pub`.

```nan
// file: wire/package.nan
//! Link header decoding.

package wire {
    language 1
    targets any
    allow validate in wire
} wire

// file: wire/wire.nan
//! Decodes the kind byte of link headers.

pub closed enum Kind derives Eq {
    Data
    Ack
} Kind

pub closed enum KindError derives Eq {
    Empty
    Unknown(u8)
} KindError

fn kind_of(code: u8) -> Option[Kind] {
    return match code {
        0 => Some(Kind.Data)
        1 => Some(Kind.Ack)
        else => None
    }
} kind_of

pub validator fn decode_kind(raw: Untrusted[Slice[u8]]) -> Kind fails KindError
    example attempt decode_kind(Untrusted.of(b"\x01")) == Ok(Kind.Ack)
    example attempt decode_kind(Untrusted.of(b"")) == Err(KindError.Empty)
{
    let bytes = raw.inspect()
    let Some(head) = bytes.prefix(1) else {
        fail KindError.Empty
    }
    let Some(kind) = kind_of(head[0]) else {
        fail KindError.Unknown(head[0])
    }
    return kind
} decode_kind
```

```nan
pub fn first_byte(raw: Untrusted[Slice[u8]]) -> Option[u8] {
    let bytes = raw.inspect() // error: untrusted.inspect-outside-validator
    let Some(head) = bytes.prefix(1) else {
        return None
    }
    return Some(head[0])
} first_byte
```

```nan
// file: wire/package.nan
//! Link header decoding.

package wire {
    language 1
    targets any
    allow validate in wire
} wire

// file: wire/wire.nan
//! Checks link header bytes.

pub validator fn is_marker(code: u8) -> bool { // error: untrusted.validator-no-input
    return code == 0x7E
} is_marker
```

## Validator classes

A **parsing type** is one of these, and no other type is one:

- a scalar, `Unit`, `Never` or an Untrusted instantiation;
- a struct or enum that is not a [base type](05-properties.md#structural-properties) and has no type parameters, whose constituent types are all parsing types;
- a layout whose field types are all parsing types;
- an instantiation of a generic struct or enum that is not a base type, `Option` and `Result` included, whose type arguments are all parsing types and whose definition is a parsing type when its type parameters are counted as parsing types.

The checker decides whether a struct or enum is a parsing type, its type parameters counted as parsing types, from its full definition in the package declaring it. For a `pub` struct or enum this result is an [interface fact](05-properties.md#interface-facts), which consumers read instead of its fields.

The **export types** of a function declared `validator` are:

- its result type and its failure type;
- the type of each parameter other than its Untrusted inputs that it receives by `mut`;
- the type of each other parameter, its Untrusted inputs excepted, that is or contains a type parameter, a function type, `Dyn`, a type with [interior mutability](12-memory.md#interior-mutability), `task::Sender` or `sys::Outbox`.

A validator or decoder is either parsing or declassifying:

- A **parsing validator** is a function declared `validator` whose export types are all parsing types, or a parsing decoder.
- A **declassifying validator** is any other validator or decoder, an [implementing function](08-generics-interfaces.md#conformance) of a method of `core::Decoder` included.
- The decoder of a generic type item is declassifying. The decoder of any other type item is parsing exactly when the item is a parsing type.
- A use of a decoder is parsing exactly when the instantiation it decodes is a parsing type.
- The `decode` method of a std type implementing `Encode` is classified and granted as a decoder of that type.

An **encoder** is the `encode` method of a type implementing `Encode`; it writes out the contents of every Untrusted value it encodes. A use of an encoder is declassifying exactly when the instantiation it encodes holds an Untrusted instantiation: is one, has one as a type argument at any depth, or has one among the constituent types of a struct or enum it instantiates, its type arguments substituted. Whether a `pub` struct or enum deriving `Encode` holds one, decided from its full definition, is an [interface fact](05-properties.md#interface-facts).

Classes are computed, never written. A declassifying validator and the declassifying decoder of a type item are [trust escapes](31-trust.md#trust-escapes); parsing ones are not.

```nan
// file: wire/package.nan
//! Link frame statistics.

package wire {
    language 1
    targets any
    allow validate in wire
} wire

// file: wire/wire.nan
//! Counts zero bytes in link frames.

pub closed struct Tally {
    pub frames: u64
    pub zero_bytes: u64
} Tally

pub validator fn tally_zeros(raw: Untrusted[Slice[u8]], mut tally: Tally) {
    tally.frames += 1
    for byte in raw.inspect() {
        if byte == 0 {
            tally.zero_bytes += 1
        }
    }
} tally_zeros
```

## Validate grants

A module **uses** a validator, decoder or encoder when one of its items:

- calls it or names it as a function value;
- names an instantiation of a function, by a call or as a function value, giving an argument `A` to a type parameter of its codec uses: it uses that codec of `A`, so `mem::wire::Codec[Note].decode` uses `Note.decode` and `mem::wire::Codec[Note].encode` uses `Note.encode`; or
- gives a type `A` that is not a type parameter to a type parameter bounded by an interface `I` other than `Encode` and the property interfaces, calls a method of `I` on `A`, or views `A` as `Dyn[I]`: where `A` implements `I` through an impl line, it uses each implementing function of the line that is a validator, and what naming each implementing function and default body of the line instantiated for `A`, and for the method called with its own type arguments, uses, a use of a codec of a type parameter that this leaves unfixed being declassifying; through a derive, what each constituent type of `A` implementing `I` uses; through the toolchain, what each type argument of `A` implementing `I` uses.

A call through a bound on a type parameter, or through `Dyn`, is not itself a use.

The **codec uses** of a function are the type parameters whose decoders and encoders its code uses, each marked `decode`, `encode` or both. A use, as above, of a codec of a type parameter of the code holding it, a call of its `decode` or `encode` through its bound included, is no use of a decoder or encoder: it marks the parameter with that codec. A use of a codec of another type marks with that codec each type parameter that the type holds as a type argument at any depth. `Codec.decode` of `mem::wire`, `text::json` and `text::construction` and the `canonical`, `erased` and `decode_from` of `mem::wire` mark `T` `decode`, and the `encode` and `try_encode` of `mem::wire` and `text::json` and the `encode_as` of `mem::wire` mark it `encode`. A use of the decoder of a type with [forward migrations](13-text-encode.md#migrations) is also a use of the decoder of each frozen shape they convert from, and a use of the encoder of a type with inverses one of the encoder of each frozen shape they convert to. The codec uses of a function are [interface facts](05-properties.md#interface-facts) of it.

`validate` is the [manifest grant](14-authority.md#manifest-grants) for reading Untrusted contents. A module performs it directly exactly when it holds one of these operations, each located on the line given:

- the declaration of a validator, on its head;
- the declaration of a type item whose decoder is declassifying, on the line of its `derives` clause;
- an impl line naming `core::Decoder`;
- a use of a validator, or a declassifying use of a decoder or encoder, on the line holding the call, function value or instantiation.

`Untrusted.of`, `inspect`, the content-blind operations and the other uses of decoders and encoders need no grant.

```nan
// file: notes/package.nan
//! Note storage.

package notes {
    language 1
    targets any
    allow validate in notes
} notes

// file: notes/notes.nan
//! Decodes stored notes.

pub closed struct Note derives Encode {
    pub title: text::String
    pub pinned: bool
} Note

pub fn load_note(raw: Untrusted[Slice[u8]], alloc: mem::Alloc) -> Note fails core::DecodeError {
    return try mem::wire::Codec[Note].decode(raw, alloc)
} load_note
```

```nan
pub fn load_name(raw: Untrusted[Slice[u8]], alloc: mem::Alloc) -> text::String fails core::DecodeError {
    return try mem::wire::Codec[text::String].decode(raw, alloc) // error: authority.ungranted
} load_name
```

```nan
pub closed struct Lookup derives Encode {
    pub id: u64
    pub path: Untrusted[text::String]
} Lookup

pub fn load_lookup(raw: Untrusted[Slice[u8]], alloc: mem::Alloc) -> Lookup fails core::DecodeError {
    return try mem::wire::Codec[Lookup].decode(raw, alloc)
} load_lookup

test lookup_path_stays_untrusted for load_lookup {
    let world = testing::world()
    let lookup = try load_lookup(Untrusted.of(b"\x02\x02id\x01\x07\x04path\x04\x03a/b"), alloc: world.alloc)
    let expected = Untrusted.of(text::String.of("a/b", alloc: world.alloc))
    expect lookup.id == 7
    expect testing::same(lookup.path, expected)
} lookup_path_stays_untrusted
```

```nan
pub closed struct Lookup derives Encode {
    pub id: u64
    pub path: Untrusted[text::String]
} Lookup

pub fn store_lookup(lookup: Lookup, mut out: mem::Bytes) {
    mem::wire::Codec[Lookup].encode(lookup, mut out) // error: authority.ungranted
} store_lookup
```

```nan
pub closed struct Lookup derives Encode {
    pub id: u64
    pub path: Untrusted[text::String]
} Lookup

pub fn store[T: Encode](value: T, mut out: mem::Bytes) {
    mem::wire::Codec[T].encode(value, mut out)
} store

pub fn store_lookup(lookup: Lookup, mut out: mem::Bytes) {
    store(lookup, mut out) // error: authority.ungranted
} store_lookup
```

```nan
pub fn load[T: Encode](raw: Untrusted[Slice[u8]], alloc: mem::Alloc, mut slot: Option[T]) fails core::DecodeError {
    slot = Some(try mem::wire::Codec[T].decode(raw, alloc))
} load

pub fn load_count(raw: Untrusted[Slice[u8]], alloc: mem::Alloc, mut slot: Option[u32]) fails core::DecodeError {
    try load(raw, alloc, mut slot)
} load_count

pub fn load_name(raw: Untrusted[Slice[u8]], alloc: mem::Alloc, mut slot: Option[text::String]) fails core::DecodeError {
    let read: fn(raw: Untrusted[Slice[u8]], alloc: mem::Alloc, mut slot: Option[text::String]) fails core::DecodeError = load // error: authority.ungranted
    try read(raw, alloc, mut slot)
} load_name
```

```nan
pub closed struct Lookup derives Encode {
    pub id: u64
    pub path: Untrusted[text::String]
} Lookup

pub interface Store {
    fn save(self, mut out: mem::Bytes)
} Store

pub closed struct Kept[T: Encode] {
    pub value: T
} Kept

impl Kept[T: Encode]: Store

pub fn Kept.save(self, mut out: mem::Bytes) {
    mem::wire::Codec[T].encode(self.value, mut out)
} Kept.save

pub fn persist[S: Store](value: S, mut out: mem::Bytes) {
    value.save(mut out)
} persist

pub fn persist_lookup(take lookup: Lookup, mut out: mem::Bytes) {
    persist(Kept(value: take lookup), mut out) // error: authority.ungranted
} persist_lookup
```

```nan
pub closed struct Lookup derives Encode {
    pub id: u64
    pub path: Untrusted[text::String]
} Lookup

pub interface Saver {
    fn save[T: Encode](self, value: T, mut out: mem::Bytes)
} Saver

pub closed struct Shelf derives Copy {
    pub slot: u8
} Shelf

impl Shelf: Saver

pub fn Shelf.save[T: Encode](self, value: T, mut out: mem::Bytes) {
    mem::wire::Codec[T].encode(value, mut out)
} Shelf.save

pub fn save_count(count: u32, mut out: mem::Bytes) {
    Saver.save(Shelf(slot: 0), count, mut out)
} save_count

pub fn save_lookup(lookup: Lookup, mut out: mem::Bytes) {
    Saver.save(Shelf(slot: 0), lookup, mut out) // error: authority.ungranted
} save_lookup
```

```nan
pub interface Saver {
    fn save[T: Encode](self, value: T, mut out: mem::Bytes)
} Saver

pub closed struct Shelf derives Copy {
    pub slot: u8
} Shelf

impl Shelf: Saver

pub fn Shelf.save[T: Encode](self, value: T, mut out: mem::Bytes) {
    mem::wire::Codec[T].encode(value, mut out)
} Shelf.save

pub fn keep[S: Saver](saver: S, count: u32, mut out: mem::Bytes) {
    saver.save(count, mut out)
} keep

pub fn keep_count(count: u32, mut out: mem::Bytes) {
    keep(Shelf(slot: 0), count, mut out) // error: authority.ungranted
} keep_count
```

## Secret values

**Secret** is the core type `core::Secret[T: core::SecretBits]`: a value of `T`, such as a key, whose operations take the same steps and the same time whatever value it holds. `core::SecretBits` is the core interface that exactly the integer types and `bool` implement; an impl line naming it is an error (`secret.bits-impl`).

```nan-api
// module: core

pub interface SecretBits {
} SecretBits

pub fn Secret.of(value: T) -> Secret[T] limits pure, notrap, terminates
    example Secret.of(7u8).reveal() == 7

pub fn Secret.reveal(self) -> T limits pure, notrap, terminates
    example Secret.of(true).reveal()
```

- A Secret value has no fields or variants that any code sees and holds exactly a `T` value. `Secret[T]` is a [base type](05-properties.md#structural-properties) that is Copy, Send and Inert, neither linear nor a view, and not AnyBits.
- Apart from the property interfaces, `Secret[T]` implements no std interface: comparing, ordering, hashing, rendering, encoding or generating a Secret value is an error (`interface.not-implemented`), as is passing one where a bound names another std interface (`generic.unsatisfied-bound`) or deriving such an interface for a type holding one (`derive.constituent-missing`).
- `Secret.of(v)` wraps a value and `reveal` yields it. A value revealed is ordinary data, which may decide branches, indexes and output.
- An expression of type `Secret[T]` where the expected type is `T`, such as the condition of an `if`, an operand of `and`, an index or an argument, is an error (`secret.public-use`), reported in place of `type.mismatch`. Its because-chain is one `note` reason, `core::Secret.select`, the member that chooses between Secret values without a branch.
- Besides `of` and `reveal`, std declares exactly these members of Secret, each only for the receivers `Secret[T]` its table row lists. On another instantiation the name is unknown (`name.unknown`).

| Head | Receivers `Secret[T]` |
| --- | --- |
| `fn wrapping_add(self, other: Secret[T]) -> Secret[T] limits pure, notrap, terminates`, and `wrapping_sub` and `wrapping_mul` alike | integer `T` |
| `fn mul_high(self, other: Secret[T]) -> Secret[T] limits pure, notrap, terminates` | `u64`, `i64` |
| `fn bit_and(self, other: Secret[T]) -> Secret[T] limits pure, notrap, terminates`, and `bit_or` and `bit_xor` alike | every `T` |
| `fn bit_not(self) -> Secret[T] limits pure, notrap, terminates` | every `T` |
| `fn shl(self, amount: u32) -> Secret[T] limits pure, terminates`, and `shr` alike | integer `T` |
| `fn rotate_left(self, amount: u32) -> Secret[T] limits pure, notrap, terminates`, and `rotate_right` alike | integer `T` |
| `fn eq(self, other: Secret[T]) -> Secret[bool] limits pure, notrap, terminates`, and `ne` alike | every `T` |
| `fn lt(self, other: Secret[T]) -> Secret[bool] limits pure, notrap, terminates`, and `le`, `gt` and `ge` alike | integer `T` |
| `fn select[V: SecretBits](self, if_true: Secret[V], if_false: Secret[V]) -> Secret[V] limits pure, notrap, terminates` | `bool` |
| `fn widen[S: SecretBits](value: Secret[S]) -> Secret[T] limits pure, notrap, terminates`, and `truncate` alike | integer `T` |
| `fn equal(a: Slice[Secret[T]], b: Slice[Secret[T]]) -> Secret[bool] limits pure, terminates` | every `T` |
| `fn copy_if(mut target: SliceMut[Secret[T]], source: Slice[Secret[T]], choice: Secret[bool]) limits pure, terminates` | every `T` |

Each member computes on the values its Secret operands hold and wraps its result, for w the width of `T`:

- `wrapping_add`, `wrapping_sub` and `wrapping_mul` compute as the `wrapping_op` of [overflow](09-numbers.md#overflow), and `mul_high` as the `high` of [`wide_mul`](09-numbers.md#wide-multiplication).
- `bit_and`, `bit_or`, `bit_xor` and `bit_not` compute as `&`, `|`, `^` and `~`; on `bool` they are and, or, exclusive or and negation.
- `shl` and `shr` trap when the amount n is at least w (`trap.shift`). Otherwise `shl` is the value congruent to a × 2^n modulo 2^w, and `shr` is a ÷ 2^n rounded toward negative infinity. `rotate_left` and `rotate_right` rotate the w bits by n modulo w.
- `eq`, `ne`, `lt`, `le`, `gt` and `ge` compare as `==`, `!=`, `<`, `<=`, `>` and `>=` on `T`.
- `c.select(a, if_false: b)` is `a` when `c` holds `true`, and `b` otherwise.
- `Secret[T].widen(x)` and `Secret[T].truncate(x)`, for `x` of `Secret[S]`, convert as `T.widen` and `T.truncate` of [conversions](09-numbers.md#conversions). A source `S` that is no integer type is an error (`num.conversion-undefined`), as are `S` equal to `T` (`num.same-type-conversion`), `widen` on a pair that is not a widening pair (`num.widen-lossy`) and `truncate` on a widening pair (`num.widen-applies`).
- `equal` requires `a.len() == b.len()` and holds `true` exactly when every element of `a` equals the element of `b` at its position. `copy_if` requires `target.len() == source.len()` and, when `choice` holds `true`, copies every element of `source` into `target`; otherwise it leaves `target` unchanged.

```nan
pub fn conditional_swap(swap: core::Secret[bool], mut a: core::Secret[u64], mut b: core::Secret[u64]) {
    let mask = swap.select(core::Secret.of(~0u64), if_false: core::Secret.of(0))
    let flip = a.bit_xor(b).bit_and(mask)
    a = a.bit_xor(flip)
    b = b.bit_xor(flip)
} conditional_swap

test swaps_only_when_asked for conditional_swap {
    var a = core::Secret.of(3u64)
    var b = core::Secret.of(9u64)
    conditional_swap(core::Secret.of(true), mut a, mut b)
    expect a.reveal() == 9 and b.reveal() == 3
    conditional_swap(core::Secret.of(false), mut a, mut b)
    expect a.reveal() == 9
} swaps_only_when_asked
```

```nan
pub fn tag_matches(tag: Slice[core::Secret[u8]], expected: Slice[core::Secret[u8]]) -> bool {
    if tag.len() != expected.len() {
        return false
    }
    return core::Secret.equal(tag, b: expected).reveal()
} tag_matches
```

```nan
pub fn key_matches(key: core::Secret[u64], guess: core::Secret[u64]) -> bool {
    if key.eq(guess) { // error: secret.public-use
        return true
    }
    return false
} key_matches
```

```nan
pub closed struct Limb derives Copy {
    pub bits: u64
} Limb

impl Limb: core::SecretBits // error: secret.bits-impl
```

### Constant-time lowering

[Lowering](30-artefacts.md#lowering) gives each member of Secret other than `equal` and `copy_if` a fixed sequence of integer additions, subtractions, multiplications, bitwise operations, shifts, rotations, comparisons giving a flag, conditional moves (`cmov` on x86_64, `csel` on AArch64), extensions and truncations. That sequence holds no branch, no division and no memory access whose address depends on an operand; its only branch is the check of the amount of `shl` and `shr`, which depends on the amount alone. `equal` and `copy_if` branch and index only on the lengths of their slices and the positions within them. A function computing on Secret values through these members thus branches, divides and addresses memory independently of the values they hold.

The time each of these instructions takes is independent of its operands on a processor whose data-independent timing mode is on and that lists the instruction under that mode; the [runtime](27-program-model.md#runtime-support) turns that mode on where the program can. The guarantee covers the instructions lowering emits, not speculative execution, power or frequency variation, nor the clearing of Secret values from registers and memory.

## Diagnostics

| Code | Class | Rule |
| --- | --- | --- |
| `secret.bits-impl` | error | [Secret values](#secret-values) |
| `secret.public-use` | error | [Secret values](#secret-values) |
| `untrusted.inspect-outside-validator` | error | [Validators](#validators) |
| `untrusted.validator-interface` | error | [Validators](#validators) |
| `untrusted.validator-no-input` | error | [Validators](#validators) |
| `untrusted.wrapped-alias` | error | [Untrusted values](#untrusted-values) |
