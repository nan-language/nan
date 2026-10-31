# Text, rendering and Encode

This chapter defines the text types, their lengths and slicing, `text::utf8`, `text::Interner`, `Render` and render budgets, `text::Writer`, the `text::fmt` template intrinsic, the `Encode` derive with its encoders, decoders and keys, decoding rules and flexible structs, schema trees and hashes, the formats `mem::wire` and `text::json`, erased decoders, migrations, and the construction text that `text::construction` decodes.

## Text types

**Text** is UTF-8 character data: a value of `Str`, `text::String` or `text::Fixed[N]`. Every text value holds well-formed UTF-8: shortest-form sequences of 1 to 4 bytes encoding scalar values, with no surrogate and nothing above U+10FFFF. No safe operation produces text that is not well-formed.

- `char` is the [scalar](04-types.md#scalars) holding one Unicode scalar value.
- `Str` is a core [view](05-properties.md#views) of the bytes of a text. A text literal or text run is a `Str` with static storage ([text literals](01-lexical.md#text-literals)); every other `Str` is a projection of a value owning text.
- `text::String` is owned, growable text: a [growable container](12-memory.md#growth) whose capacity grows as that of `mem::Bytes` does.
- `text::Fixed[N]` is owned text of at most `N` bytes stored inline, which never allocates. `of`, `push` and `append` require that the result fit, and their `try_` forms fail with `mem::OutOfMemory.Exhausted` there, leaving the value unchanged. `room` is `N` minus the byte length.
- `text::String`, `text::Fixed[N]` and `text::Interner` are [base types](05-properties.md#structural-properties) with these properties:

| Type | linear | view | Copy | Send | Inert | AnyBits |
| --- | --- | --- | --- | --- | --- | --- |
| `text::String` | no | no | no | yes | yes | no |
| `text::Fixed[N]` | no | no | yes | yes | yes | no |
| `text::Interner` | no | no | no | yes | no | no |

- `text::String` and `text::Fixed[N]` implement `Eq`, `Ord` and `Hash` exactly as `Str` does on their bytes ([equality, ordering, hashing and defaults](08-generics-interfaces.md#equality-ordering-hashing-and-defaults)). `text::Fixed[N]` implements `Default`, the empty text.
- Each read method of `Str` in this chapter also exists on `text::String` and `text::Fixed[N]` with the same head, and behaves as the same call on `as_str()`.
- `for c in s` over a text iterates through `cursor`, whose `core::CharCursor` yields the chars in order ([iteration](07-ownership.md#iteration)).
- `to_ascii_lowercase` and `to_ascii_uppercase` map each ASCII letter to its other case and return every other char unchanged.
- `find(needle)` is `Some` of the byte index where `needle` first occurs, 0 for an empty `needle`, or `None`.

```nan-api
// module: core

pub fn char.utf8_len(self) -> usize limits pure, notrap, terminates
    example 'é'.utf8_len() == 2

pub fn char.is_ascii(self) -> bool limits pure, notrap, terminates
    ensures result == (self <= '\x7F')

pub fn char.is_ascii_digit(self) -> bool limits pure, notrap, terminates
    ensures result == (self >= '0' and self <= '9')

pub fn char.is_ascii_alphabetic(self) -> bool limits pure, notrap, terminates
    ensures result == ((self >= 'A' and self <= 'Z') or (self >= 'a' and self <= 'z'))

pub fn char.is_ascii_whitespace(self) -> bool limits pure, notrap, terminates
    ensures result == (self == ' ' or self == '\t' or self == '\n' or self == '\x0C' or self == '\r')

pub fn char.to_ascii_lowercase(self) -> char limits pure, notrap, terminates
    example 'Q'.to_ascii_lowercase() == 'q'

pub fn char.to_ascii_uppercase(self) -> char limits pure, notrap, terminates

pub proj fn Str.as_bytes(self) -> Slice[u8] limits pure, notrap, terminates

pub proj fn Str.cursor(self) -> CharCursor limits pure, notrap, terminates

pub fn Str.is_empty(self) -> bool limits pure, notrap, terminates
    ensures result == (self.byte_len() == 0)

pub fn Str.starts_with(self, prefix: Str) -> bool limits pure, notrap, terminates

pub fn Str.ends_with(self, suffix: Str) -> bool limits pure, notrap, terminates

pub fn Str.find(self, needle: Str) -> Option[usize] limits pure, notrap, terminates
    example "port=80".find("=") == Some(4)

pub open view struct CharCursor {
} CharCursor

impl CharCursor: Cursor[char]

pub fn CharCursor.next(mut self) -> Option[char] limits pure, notrap, terminates
```

```nan-api
// module: text

pub open struct String {
} String

pub fn String.empty(alloc: mem::Alloc) -> String

pub fn String.of(text: Str, alloc: mem::Alloc) -> String

pub fn String.try_of(text: Str, alloc: mem::Alloc) -> String fails mem::OutOfMemory

pub fn String.try_from_utf8(bytes: Slice[u8], alloc: mem::Alloc) -> String fails utf8::Utf8Error

pub proj fn String.as_str(self) -> Str

pub fn String.push(mut self, c: char)

pub fn String.try_push(mut self, c: char) fails mem::OutOfMemory

pub fn String.append(mut self, text: Str)

pub fn String.try_append(mut self, text: Str) fails mem::OutOfMemory

pub fn String.clear(mut self)

pub fn String.into_bytes(take self) -> mem::Bytes

pub open struct Fixed[N: usize where N > 0] {
} Fixed

pub fn Fixed.empty() -> Fixed[N] limits pure, notrap, terminates

pub fn Fixed.of(text: Str) -> Fixed[N] limits notrap
    requires text.byte_len() <= N

pub fn Fixed.try_of(text: Str) -> Fixed[N] fails mem::OutOfMemory limits notrap

pub proj fn Fixed.as_str(self) -> Str

pub fn Fixed.room(self) -> usize limits pure, notrap, terminates
    example Fixed[8].of("abc").room() == 5

pub fn Fixed.push(mut self, c: char) limits notrap
    requires c.utf8_len() <= self.room()

pub fn Fixed.try_push(mut self, c: char) fails mem::OutOfMemory limits notrap

pub fn Fixed.append(mut self, text: Str) limits notrap
    requires text.byte_len() <= self.room()

pub fn Fixed.try_append(mut self, text: Str) fails mem::OutOfMemory limits notrap

pub fn Fixed.clear(mut self)
```

- `push` appends a char's UTF-8 bytes; `clear` empties the value and keeps its storage; `into_bytes` returns the text's bytes without copying them.

```nan
pub fn initials(name: Str, alloc: mem::Alloc) -> text::String {
    var out = text::String.empty(alloc)
    var at_start = true
    for c in name {
        if c == ' ' {
            at_start = true
        } else if at_start {
            mut out.push(c.to_ascii_uppercase())
            at_start = false
        }
    }
    return out
} initials

test initials_of_two_words for initials {
    let world = testing::world()
    let short = initials("ada lovelace", alloc: world.alloc)
    expect short.as_str() == "AL"
} initials_of_two_words
```

## Lengths and slicing

```nan-api
// module: core

pub fn Str.byte_len(self) -> usize limits pure, notrap, terminates
    example "café".byte_len() == 5

pub fn Str.char_count(self) -> usize limits pure, notrap, terminates
    example "café".char_count() == 4

pub fn Str.is_char_boundary(self, i: usize) -> bool limits pure, notrap, terminates
    example not "café".is_char_boundary(4)

pub proj fn Str.slice_bytes(self, start: usize, end: usize) -> Option[Str] limits pure, notrap, terminates
    example "café".slice_bytes(0, end: 3) == Some("caf")
    example "café".slice_bytes(0, end: 4) == None
```

- `byte_len` is the number of bytes; `char_count` the number of chars.
- `is_char_boundary(i)` is true exactly when `i` is 0, `byte_len()`, or the index of the first byte of a char.
- `slice_bytes(start, end: e)` is `Some` of the view of bytes `start` up to but excluding `e` when `start <= e`, `e <= byte_len()` and both are char boundaries, and `None` otherwise.
- Text types have no `at`, `at_mut` or `len`. An index expression `s[i]` on a text value is an error (`text.integer-index`), reported in place of `own.not-indexable`, whose exact fix writes `s.as_bytes()[i]`. A call `s.len()` on a text value is an error (`text.bare-len`), reported in place of `name.unknown`, whose exact fix writes `s.byte_len()`.

```nan
pub fn has_key(line: Str, key: Str) -> bool
    example has_key("port=80", key: "port")
    example not has_key("café=1", key: "caf")
{
    let Some(at) = line.find("=") else {
        return false
    }
    let Some(head) = line.slice_bytes(0, end: at) else {
        return false
    }
    return head == key
} has_key
```

```nan
pub fn first_byte(name: Str) -> u8 {
    return name[0] // error: text.integer-index
} first_byte
```

```nan
pub fn is_short(name: Str) -> bool {
    return name.len() < 8 // error: text.bare-len
} is_short
```

## UTF-8

`text::utf8` converts between bytes and text.

```nan-api
// module: text::utf8

pub closed enum Utf8Error derives Eq, Render {
    Invalid(at: usize)
    OutOfMemory(mem::OutOfMemory) from
} Utf8Error

pub fn valid_prefix(bytes: Slice[u8]) -> usize limits pure, notrap, terminates
    example valid_prefix(b"ok\xFF") == 2

pub fn is_valid(bytes: Slice[u8]) -> bool limits pure, notrap, terminates
    ensures result == (valid_prefix(bytes) == bytes.len())
    example not is_valid(b"\xC3")

pub fn char_at(bytes: Slice[u8], at: usize) -> Option[char] limits pure, notrap, terminates
    example char_at(b"\xC3\xA9", at: 0) == Some('é')

pub fn write_char(c: char, mut out: SliceMut[u8]) -> usize limits pure, notrap, terminates
    requires out.len() >= c.utf8_len()
```

- `valid_prefix` is the byte length of the longest prefix of `bytes` that is well-formed UTF-8.
- `char_at(bytes, at: i)` is `Some(c)` when a well-formed encoding of `c` starts at byte `i`, and `None` otherwise.
- `write_char` writes the UTF-8 bytes of `c` at the start of `out` and returns their count.
- `text::String.try_from_utf8(bytes, alloc)` copies `bytes` into a new `text::String` when they are well-formed. Otherwise it fails with `Utf8Error.Invalid(at: n)`, where `n` is `valid_prefix(bytes)`. Exhausted memory fails with `Utf8Error.OutOfMemory`.

```nan
pub closed enum LabelError derives Eq, Render {
    Utf8(text::utf8::Utf8Error) from
    Empty
} LabelError

pub fn label_of(bytes: Slice[u8], alloc: mem::Alloc) -> text::String fails LabelError {
    let label = try text::String.try_from_utf8(bytes, alloc)
    if label.is_empty() {
        fail LabelError.Empty
    }
    return label
} label_of
```

## Interning

`text::Interner` maps texts to `text::Symbol` values: interning equal texts gives equal symbols, and each symbol resolves to its text. It has [interior mutability](12-memory.md#interior-mutability), so `intern` takes it by read and a `Str` from `resolve` stays valid across later interning.

```nan-api
// module: text

pub closed struct Symbol derives Copy, Eq, Ord, Hash, Render {
    pub index: u32
} Symbol

pub open struct Interner {
} Interner

pub fn Interner.empty(alloc: mem::Alloc) -> Interner

pub fn Interner.intern(self, text: Str) -> Symbol

pub fn Interner.try_intern(self, text: Str) -> Symbol fails mem::OutOfMemory

pub fn Interner.lookup(self, text: Str) -> Option[Symbol]

pub proj fn Interner.resolve(self, symbol: Symbol) -> Str

pub fn Interner.len(self) -> usize
```

- The n-th distinct text interned gets index n − 1, and `len` is their count. Interning a text already present returns its symbol and allocates nothing.
- `intern` traps when memory is exhausted or every `u32` index is used (`trap.exhausted`); `try_intern` fails with `mem::OutOfMemory.Exhausted` instead.
- `lookup` returns the symbol of an interned text, or `None`, without allocating.
- `resolve` returns the text of a symbol; an index at or past `len()` traps (`trap.bounds`).

```nan
pub fn same_symbol(names: text::Interner, name: Str, other: Str) -> bool {
    let a = names.intern(name)
    let b = names.intern(other)
    return a == b
} same_symbol
```

## Render

`Render` is the core interface writing a value as text. A **render budget** is the number of bytes a rendering may occupy, fixed by the operation that starts it, such as a [writer](#writers). A rendering starts with a new `core::Renderer` passed to one call of `render`; only std operations create renderers.

```nan-api
// module: core

pub interface Render {
    fn render(self, mut out: Renderer) limits pure, terminates
} Render

pub open view struct Renderer {
} Renderer

pub fn Renderer.write(mut self, text: Str) limits pure, notrap, terminates

pub fn Renderer.write_char(mut self, c: char) limits pure, notrap, terminates

pub fn Renderer.remaining(self) -> usize limits pure, notrap, terminates

pub fn Renderer.is_elided(self) -> bool limits pure, notrap, terminates

pub fn Renderer.write_nested[T: Render](mut self, value: T) limits pure, terminates
```

The full rendering R is the concatenation, in call order, of every text and char written to one renderer. With render budget B:

- When R holds at most B bytes, the output is R.
- Otherwise the output is P followed by D, where D is `...` cut to its first `min(3, B)` bytes and P is the longest prefix of R ending at a char boundary with at most B − |D| bytes. The rendering is then elided.
- `remaining()` is B minus the bytes written so far, or 0 when that is negative. `is_elided()` is true once more than B bytes have been written.
- `write_nested(value)` writes the **nested rendering** of `value`, the form it takes inside the rendering of another value. A `char` is written as a char literal and a `Str`, `text::String` or `text::Fixed[N]` as a text literal ([text literals](01-lexical.md#text-literals)): each character as itself, except `\` and the literal's quote, written `\\` and `\'` or `\"`; U+000A, U+000D, U+0009 and U+0000, written `\n`, `\r`, `\t` and `\0`; and every other character outside `CHARACTER`, written `\u{H}` with `H` in uppercase hexadecimal digits without leading zeros. The contents of a `mem::Box[T]` or `mem::Shared[T]` are written in their nested rendering, and any other value as its `render` writes it.

Core and std types render as follows:

| Type | Rendering |
| --- | --- |
| integers | decimal digits, with `-` before a negative value: `-42` |
| `f32`, `f64` | `NaN`, `inf`, `-inf`, or a finite decimal form |
| `bool` | `true`, `false` |
| `char`, `Str`, `text::String`, `text::Fixed[N]` | their characters |
| `Unit` | `Unit` |
| `Option[T]`, `Result[T, E]` | `None`, `Some(v)`, `Ok(v)`, `Err(e)` |
| `Array[T, N]`, `Slice[T]`, `SliceMut[T]`, `mem::List[T]`, `mem::Bounded[T, N]`, `mem::Set[T]` | `[1, 2]`: the nested renderings of the elements, separated by `, `, in brackets |
| `mem::Map[K, V]`, `mem::HashMap[K, V]` | `{k: v, k: v}`: the nested renderings of the keys and values, in iteration order |
| `mem::Bytes` | `b"`, each byte, then `"`: bytes `0x20` to `0x7E` as themselves, except `"` and `\` written `\"` and `\\`; `\n`, `\r`, `\t` and `\0` for their bytes; uppercase `\xHH` for the rest |
| `mem::Box[T]`, `mem::Shared[T]` | the rendering of the contents |

- A finite float renders as `-` when its sign bit is set, then the shortest digits d1…dk and exponent e for which d1.d2…dk × 10^e rounds to its magnitude m in its own type by roundTiesToEven, choosing the nearest to m, then an even last digit. For m 0 or in [10^-5, 10^16) the digits are positional with at least one each side of `.` (`0.1`, `100.0`, `-0.0`); otherwise they are d1, `.`, d2…dk or `0`, `e`, then e with `-` when negative (`1.5e-7`, `2.0e16`).
- The generic types of the table implement `Render` when their type arguments do. Other core and std types implement it only as their declarations state; `Untrusted[T]` renders as [Untrusted and secret data](15-untrusted.md#opacity) defines.

### Derived Render

`derives Render` writes `render` from the type's structure; [derives](08-generics-interfaces.md#derives) checks it against the constituent types and fixes the instantiations it applies to.

- A struct renders as its name, `(`, then each field as `name: value` in declaration order separated by `, `, then `)`: `Header(kind: Kind.Ack, seq: 7)`. A struct without fields renders as `Name()`.
- An enum value renders as `Name.Variant`, followed by `(value)` for a `V(T)` payload or by `(label: value, ...)` for labelled payloads in declaration order. The variants of `Option` and `Result` render without `Name.`.
- Each field and payload value is written in its nested rendering: `Link(name: "eth0", mtu: 1500)`.
- Generic arguments do not appear. Private fields render like public ones.

```nan
pub closed enum Kind derives Copy, Eq, Render {
    Data
    Ack
} Kind

pub closed struct Header derives Copy, Eq, Render {
    pub kind: Kind
    pub seq: u32
} Header

pub fn header_line(header: Header, mut out: text::String) {
    mut out.write(text::fmt("{header}", header), budget: 64)
} header_line

test header_line_shows_fields for header_line {
    let world = testing::world()
    var out = text::String.empty(world.alloc)
    header_line(Header(kind: Kind.Ack, seq: 7), mut out)
    expect out.as_str() == "Header(kind: Kind.Ack, seq: 7)"
} header_line_shows_fields
```

```nan
pub closed struct Link derives Eq, Render {
    pub name: text::String
    pub mtu: u32
} Link

pub fn link_line(link: Link, mut out: text::String) {
    mut out.write(text::fmt("{link} {name}", link, name: link.name), budget: 64)
} link_line

test link_line_quotes_nested_text for link_line {
    let world = testing::world()
    var out = text::String.empty(world.alloc)
    link_line(Link(name: text::String.of("eth\t0", alloc: world.alloc), mtu: 1_500), mut out)
    expect out.as_str() == "Link(name: \"eth\\t0\", mtu: 1500) eth\t0"
} link_line_quotes_nested_text
```

```nan
pub closed struct Celsius derives Copy {
    pub tenths: u32
} Celsius

impl Celsius: Render

pub fn Celsius.render(self, mut out: core::Renderer) {
    text::fmt("{whole}.{tenth} C", whole: self.tenths / 10, tenth: self.tenths % 10).render(mut out)
} Celsius.render
```

## Writers

`text::Writer` is the interface of text destinations that start renderings.

```nan-api
// module: text

pub interface Writer {
    fn write(mut self, value: Dyn[Render], budget: usize)
    fn try_write(mut self, value: Dyn[Render], budget: usize) fails mem::OutOfMemory
} Writer

impl String: Writer

impl Fixed: Writer
```

- `write` appends the output of `value.render` with render budget `budget`.
- `text::String.write` calls `value.render` twice, first to measure the output, then to write it into memory drawn for exactly that output. It traps when that memory is exhausted (`trap.exhausted`); `try_write` fails with `mem::OutOfMemory.Exhausted` there, leaving the receiver unchanged.
- On `text::Fixed[N]` the render budget is the smaller of `budget` and `room()`; neither method traps or fails.

```nan
pub fn clipped(line: Str, mut out: text::String) {
    mut out.write(text::fmt("{line}", line), budget: 8)
} clipped

test clipping_marks_elision for clipped {
    let world = testing::world()
    var out = text::String.empty(world.alloc)
    clipped("a long line", mut out)
    expect out.as_str() == "a lon..."
} clipping_marks_elision
```

## Templates

`text::fmt` is the one template intrinsic. A call `text::fmt("template", label: value, ...)` is a [call expression](06-statements-expressions.md#calls) whose first argument is the template and whose later arguments fill its holes.

- The template is one `TEXT` literal; anything else, a text run included, is an error (`text.template-not-literal`).
- In the template's value, `{`, an identifier that is not a [hard keyword](01-lexical.md#keywords), and `}` form a hole named by that identifier, and `{{` and `}}` stand for `{` and `}`. Any other `{` or `}` is an error (`text.template-malformed`).
- The template carries no label. Each later argument carries a hole's name as its label, or is punned as [arguments and labels](06-statements-expressions.md#arguments-and-labels) states; any other form is an error (`expr.argument-label`).
- A hole naming no argument is an error (`text.hole-unbound`). An argument named by no hole is an error (`text.argument-unused`). A label given to two arguments is an error (`text.argument-duplicate`). A hole may appear several times.
- Each argument is an unmarked read argument (`mode.mismatch`) without an expected type, whose type implements `Render` or is `Dyn[Render]`; another type is an error (`generic.unsatisfied-bound`).
- The call is a `Dyn[Render]` of a value holding the template and a read access to each argument, which lasts until the end of its statement. Evaluating it is pure, notrap and terminating.
- `text::fmt` appears only as the callee of such a call, and the call only as an argument or receiver of another call; anything else is an error (`text.template-position`).
- Rendering the call writes the template's text between holes and each hole's argument through its `Render`, in template order, to the same renderer.

```nan
pub fn describe(mut out: text::String, seq: u32, total: u32) {
    mut out.write(text::fmt("frame {seq} of {total}, {{ok}}", seq, total), budget: 64)
} describe

test describe_counts for describe {
    let world = testing::world()
    var out = text::String.empty(world.alloc)
    describe(mut out, seq: 3, total: 9)
    expect out.as_str() == "frame 3 of 9, {ok}"
} describe_counts
```

```nan
pub fn describe(mut out: text::String, seq: u32) {
    mut out.write(text::fmt("frame {seq} of {total}", seq), budget: 64) // error: text.hole-unbound
} describe
```

```nan
pub fn describe(mut out: text::String, seq: u32, total: u32) {
    mut out.write(text::fmt("frame {seq}", seq, total), budget: 64) // error: text.argument-unused
} describe
```

```nan
pub fn describe(seq: u32) {
    discard text::fmt("frame {seq}", seq) // error: text.template-position
} describe
```

## Encode

`Encode` is the core interface converting values to and from structured data. A **key** is the name identifying a field, variant or labelled payload in encoded data. A **decoder** is the function `T.decode` that `derives Encode` writes.

```nan-api
// module: core

pub interface Encode {
    fn encode[E: Encoder](self, mut e: E) limits terminates
    fn decode[D: Decoder](mut d: D, alloc: mem::Alloc) -> Self fails DecodeError limits terminates
} Encode
```

### Requirements

- A written impl line naming `Encode` outside std is an error (`encode.manual-impl`).
- `derives Encode` is checked in this order, and only the first failing check is reported, on the type's first line:
  1. On a [capability type](05-properties.md#capability-types) it is `prop.capability-derive`.
  2. On a type declared `view` it is an error (`encode.view`).
  3. On a type declared `linear` it is an error (`encode.linear`).
  4. On a type that is not [Inert](05-properties.md#inert), its type parameters counted as Inert, it is an error (`encode.not-inert`).
- When none of these checks fails, [derives](08-generics-interfaces.md#derives) checks the derive against the constituent types and fixes the instantiations it applies to.
- The allocator stored by a growable container is never encoded. Decoding gives `alloc` to every container, box and shared value it rebuilds.

```nan
pub closed struct Reading derives Copy, Eq, Encode {
    pub sensor: u16
    pub level: i32
} Reading
```

```nan
pub closed struct Glossary derives Encode { // error: encode.not-inert
    pub words: text::Interner
} Glossary
```

```nan
pub closed linear struct Ticket derives Encode { // error: encode.linear
    pub number: u32
} Ticket
```

### Encoders and decoders

An encoded value has one of these kinds, which `core::Encoder` writes and `core::Decoder` reads:

| Kind | Encoder | Decoder |
| --- | --- | --- |
| bool | `write_bool` | `read_bool` |
| unsigned | `write_unsigned` | `read_unsigned` |
| signed | `write_signed` | `read_signed` |
| float | `write_float` | `read_float` |
| char | `write_char` | `read_char` |
| text, bytes | `write_text`, `write_bytes` | `begin_text` or `begin_bytes`, then `read_chunk` |
| sequence, map | `begin_sequence` or `begin_map`, the elements (a map's as key then value), `end` | `begin_sequence` or `begin_map`, `next_follows` before each element, `end` |
| record | `begin_record`, then `write_key` and a value per field, `end` | `begin_record`, then `next_follows`, `read_key` and a value or `skip` per field, `end` |
| variant | `begin_variant`, the payload when it has one, `end` | `read_variant`, `payload_follows`, the payload when present, `end` |

```nan-api
// module: core

pub interface Encoder {
    fn write_bool(mut self, value: bool) limits terminates
    fn write_unsigned(mut self, value: u64) limits terminates
    fn write_signed(mut self, value: i64) limits terminates
    fn write_float(mut self, value: f64) limits terminates
    fn write_char(mut self, value: char) limits terminates
    fn write_text(mut self, value: Str) limits terminates
    fn write_bytes(mut self, value: Slice[u8]) limits terminates
    fn begin_sequence(mut self, count: usize) limits terminates
    fn begin_map(mut self, count: usize) limits terminates
    fn begin_record(mut self, count: usize) limits terminates
    fn write_key(mut self, key: Str) limits terminates
    fn begin_variant(mut self, key: Str, payload: bool) limits terminates
    fn end(mut self) limits terminates
} Encoder

pub interface Decoder {
    fn read_bool(mut self) -> bool fails DecodeError limits terminates
    fn read_unsigned(mut self) -> u64 fails DecodeError limits terminates
    fn read_signed(mut self) -> i64 fails DecodeError limits terminates
    fn read_float(mut self) -> f64 fails DecodeError limits terminates
    fn read_char(mut self) -> char fails DecodeError limits terminates
    fn begin_text(mut self) -> usize fails DecodeError limits terminates
    fn begin_bytes(mut self) -> usize fails DecodeError limits terminates
    fn read_chunk(mut self, mut out: SliceMut[u8]) fails DecodeError limits terminates
    fn begin_sequence(mut self) fails DecodeError limits terminates
    fn begin_map(mut self) fails DecodeError limits terminates
    fn begin_record(mut self) fails DecodeError limits terminates
    fn next_follows(mut self) -> bool fails DecodeError limits terminates
    fn read_key(mut self, keys: Slice[Str]) -> Option[usize] fails DecodeError limits terminates
    fn read_variant(mut self, keys: Slice[Str]) -> Option[usize] fails DecodeError limits terminates
    fn payload_follows(mut self) -> bool fails DecodeError limits terminates
    fn skip(mut self) fails DecodeError limits terminates
    fn end(mut self) fails DecodeError limits terminates
} Decoder

pub open enum DecodeError derives Copy, Eq, Hash, Render {
    Truncated
    Malformed
    Trailing
    TooDeep
    UnknownKey
    DuplicateKey
    MissingField
    UnknownVariant
    OutOfRange
    InvalidText
    Invalid
    OutOfMemory
    UnknownSchema
} DecodeError
```

- `begin_text` and `begin_bytes` return the value's byte length n. Each `read_chunk(out)` then fills `out` with the next bytes of the value; the value is read once the chunks total n, and a chunk reaching past n fails with `Malformed`. `read_key` and `read_variant` read the next key and return its index in `keys`, or `None`. `skip` passes over the next value whatever its kind.
- The implementing functions of a std impl line of `core::Decoder` fail with `Malformed` when the input at a call does not encode a value of the kind the call reads or the calls depart from the order of the kind table; with `Truncated` on input ending inside a value, and on a count or byte length exceeding the bytes left, before allocating for it; with `TooDeep` on nesting deeper than 128 sequences, maps, records and variants; and with `OutOfMemory` on exhausted memory.

### Derived encode and decode

`derives Encode` writes `encode` and `decode`.

- A struct encodes as a record of all its fields, private ones included, written in ascending byte order of their keys.
- An enum value encodes as a variant with its key. A unit variant has no payload, a `V(T)` variant has its value as payload, and a variant with labelled payloads has a record of them as payload.
- A struct decodes in these steps:
  1. Each field present is read with `read_key` and decoded with `alloc`, as [Decoding rules](#decoding-rules) state for unknown, repeated and missing keys.
  2. Each field `where` predicate in declaration order, then each `invariant`, is evaluated; a false one fails with `Invalid`.
  3. The value is constructed.
- An enum decodes the variant `read_variant` names; a payload present or absent against the variant's form fails with `Malformed`. A labelled payload decodes as the fields of a closed struct do.
- A decoder gives `read_key` and `read_variant` the keys of the fields or variants in declaration order, then the old keys of their [aliases](#keys); an index past them reads as `None`.
- A decoder reads text and bytes with one `read_chunk` of the length `begin_text` or `begin_bytes` gives, after failing with `OutOfRange` past a fixed text's bound or with `OutOfMemory` when it cannot draw that length.
- A decoder fails with `TooDeep` on nesting deeper than 128 sequences, maps, records and variants, and with the failure of each call of `d` that fails.

```nan
pub closed struct Reading derives Encode {
    pub sensor: u16
    pub level: i32
} Reading

pub closed struct KeyCount {
    pub keys: u32
} KeyCount

impl KeyCount: core::Encoder

pub fn KeyCount.write_key(mut self, key: Str) limits terminates {
    self.keys += 1
} KeyCount.write_key

pub fn KeyCount.write_bool(mut self, value: bool) = Unit

pub fn KeyCount.write_unsigned(mut self, value: u64) = Unit

pub fn KeyCount.write_signed(mut self, value: i64) = Unit

pub fn KeyCount.write_float(mut self, value: f64) = Unit

pub fn KeyCount.write_char(mut self, value: char) = Unit

pub fn KeyCount.write_text(mut self, value: Str) = Unit

pub fn KeyCount.write_bytes(mut self, value: Slice[u8]) = Unit

pub fn KeyCount.begin_sequence(mut self, count: usize) = Unit

pub fn KeyCount.begin_map(mut self, count: usize) = Unit

pub fn KeyCount.begin_record(mut self, count: usize) = Unit

pub fn KeyCount.begin_variant(mut self, key: Str, payload: bool) = Unit

pub fn KeyCount.end(mut self) = Unit

pub fn key_count(reading: Reading) -> u32
    example key_count(Reading(sensor: 3, level: -1)) == 2
{
    var count = KeyCount(keys: 0)
    reading.encode(e: mut count)
    return count.keys
} key_count

pub fn read_reading[D: core::Decoder](mut d: D, alloc: mem::Alloc) -> Reading fails core::DecodeError {
    return try Reading.decode(mut d, alloc)
} read_reading
```

### Decoders as validators

A decoder is a validator of data arriving [Untrusted](15-untrusted.md#untrusted-values), classified and granted as [validator classes](15-untrusted.md#validator-classes) and [validate grants](15-untrusted.md#validate-grants) state. It re-checks every variant key, [niche](04-types.md#niches) and predicate and fails with a `core::DecodeError` on every invalid input: its own code holds no trap site, and it draws memory only through `try_` forms, failing with `OutOfMemory` when one fails.

### Std encodings

| Type | Encoding |
| --- | --- |
| `u8` to `u64`, `usize` | unsigned |
| `i8` to `i64`, `isize` | signed |
| `f64`, `f32` | float; an `f32` is widened, and decodes as `f32.nearest` of the value read |
| `bool` | bool |
| `char` | char; a value that is not a scalar value fails with `Invalid` |
| `Unit` | a record without fields |
| `Array[T, N]` | a sequence of N elements; another count fails with `Malformed` |
| `Option[T]`, `Result[T, E]` | a variant: `None`, `Some` with a `T`, `Ok` with a `T`, `Err` with an `E` |
| `text::String` | text |
| `text::Fixed[N]` | text; more than N bytes fails with `OutOfRange` |
| `mem::Bytes` | bytes |
| `mem::List[T]`, `mem::Set[T]`, `mem::Bounded[T, N]` | a sequence in iteration order; more than N elements fails with `OutOfRange` |
| `mem::Map[K, V]` | a map in iteration order |
| `mem::Box[T]`, `mem::Shared[T]` | the contents, decoded into a new box or shared value |
| `Untrusted[T]` | the contents, decoded into a new Untrusted value |
| `mem::Id[T]` | as the `u32` of its index |

- Decoding an integer outside its type fails with `OutOfRange`, and text that is not well-formed UTF-8 with `InvalidText`.
- Decoding a `mem::Set[T]` or `mem::Map[K, V]` fails with `DuplicateKey` when an element or key equals the previous one, and with `Malformed` when it is less by `Ord`.
- No other core or std type implements `Encode`.

## Keys

Keys compare by bytes.

- A [tombstone](29-identity-versioning.md#tombstones) `retired T.old => T.new preserves`, with `T` deriving `Encode`, makes `old` an alias of the key of the field or variant that `T.new` reaches, directly or through further such tombstones: decoders accept `old` wherever they accept that key, and encoders write only that key. Only fields and variants have aliases.

```nan
pub closed struct Probe derives Eq, Encode {
    pub seq: u32
    pub hops: u8
} Probe

retired Probe.ttl => Probe.hops preserves

pub fn load_probe(raw: Untrusted[Slice[u8]], alloc: mem::Alloc) -> Probe fails core::DecodeError {
    return try mem::wire::Codec[Probe].decode(raw, alloc)
} load_probe

test probe_reads_old_key for load_probe {
    let world = testing::world()
    let current = attempt load_probe(Untrusted.of(b"\x02\x04hops\x01\x03\x03seq\x01\x05"), alloc: world.alloc)
    let legacy = attempt load_probe(Untrusted.of(b"\x02\x03seq\x01\x05\x03ttl\x01\x03"), alloc: world.alloc)
    expect current == Ok(Probe(seq: 5, hops: 3))
    expect legacy == current
} probe_reads_old_key
```

## Decoding rules

A struct or enum declared `open` and a struct declared `flexible` decode as open; every other type, and every labelled payload, decodes as closed.

- A record key that is neither a field's key nor an alias fails a closed type with `UnknownKey`. An open type skips the field: `mem::wire` keeps it in a [flexible struct](#flexible-structs), and every other decoding discards it.
- A field given twice, by its key or its aliases, fails with `DuplicateKey`.
- A missing field of an open type whose field type implements `Default` takes that default. Any other missing field fails with `MissingField`.
- An unknown variant key fails closed and open types alike with `UnknownVariant`.
- `text::json` and `text::construction` frame a record field by its key. This framing is the format space reserved for keeping skipped fields in those formats, which no language 1 decoder keeps.

```nan
pub open struct Config derives Eq, Encode {
    pub retries: u8
    pub verbose: bool
} Config

pub fn load_config(raw: Untrusted[Slice[u8]], alloc: mem::Alloc) -> Config fails core::DecodeError {
    return try mem::wire::Codec[Config].decode(raw, alloc)
} load_config

test config_tolerates_new_fields for load_config {
    let world = testing::world()
    let loaded = attempt load_config(Untrusted.of(b"\x02\x04mode\x01\x07\x07retries\x01\x03"), alloc: world.alloc)
    expect loaded == Ok(Config(retries: 3, verbose: false))
} config_tolerates_new_fields
```

## Flexible structs

A **flexible** struct keeps the record fields that decoding it from `mem::wire` skips. Beside its declared fields, each of its values holds its **kept fields** in a field of type `mem::wire::Kept` that no source names: `Empty`, or `Fields` holding them as `mem::wire` writes the fields of a record, in ascending byte order of keys and without their count.

```nan-api
// module: mem::wire

pub open enum Kept {
    Empty
    Fields(Untrusted[mem::Bytes])
} Kept
```

- `flexible` on an enum, or on a struct that does not derive `Encode`, is an error (`encode.flexible`).
- A construction, `Default` and a generator give a value holding `Kept.Empty`, and patterns and derived `Render` pass over its kept fields. `Kept` implements `Eq`, `Ord` and `Hash` with all its values equal, so the derived ones of a flexible struct compare and hash its declared fields alone. A flexible struct holds an Untrusted instantiation, so encoding it is declassifying, and it is not Copy.
- `Codec.decode` of `mem::wire` keeps each field it skips, its key and the bytes of its value, without decoding the value. `Codec.encode` writes the kept fields among the declared ones, all in ascending byte order of keys. Encoding a value that `mem::wire` decoded thus gives back the bytes decoded, except for aliased keys, missing fields taking defaults and `f32` values.
- Every other format, and the calls of an encoder or decoder, treat a flexible struct as open: decoding discards the fields it skips, and encoding writes its declared fields alone.
- A flexible struct has the schema tree of the same struct declared `open`.

```nan
// file: settings/package.nan
//! Settings that older releases rewrite.

package settings {
    language 1
    targets any
    allow validate in settings
} settings

// file: settings/settings.nan
//! A configuration record re-encoded with the fields of newer releases.

pub flexible struct Config derives Eq, Encode {
    pub retries: u8
} Config

pub fn reencode(raw: Untrusted[Slice[u8]], alloc: mem::Alloc) -> mem::Bytes fails core::DecodeError {
    let config = try mem::wire::Codec[Config].decode(raw, alloc)
    var out = mem::Bytes.empty(alloc)
    mem::wire::Codec[Config].encode(config, mut out)
    return out
} reencode

test newer_field_survives for reencode {
    let world = testing::world()
    let out = attempt reencode(Untrusted.of(b"\x02\x04mode\x01\x07\x07retries\x01\x03"), alloc: world.alloc)
    let Ok(bytes) = out else {
        expect false
        return
    }
    expect bytes.view() == b"\x02\x04mode\x01\x07\x07retries\x01\x03"
} newer_field_survives
```

```nan
pub flexible enum Mode derives Encode { // error: encode.flexible
    Idle
    Busy
} Mode
```

## Schema hashes

The **schema tree** of a type implementing `Encode` is the `mem::wire::Schema` describing its encoded structure, and its **schema hash** is the [hash](29-identity-versioning.md#hash-values) of the tree's `mem::wire` encoding.

```nan-api
// module: mem::wire

pub closed struct SchemaHash derives Copy, Eq, Ord, Hash, Encode {
    pub bytes: Array[u8, 33]
} SchemaHash

pub closed enum Schema derives Encode {
    Bool
    Char
    Text
    Bytes
    Unsigned(width: u8)
    Signed(width: u8)
    Float(width: u8)
    BoundedText(capacity: u64)
    Sequence(element: mem::Box[Schema], length: Option[u64], capacity: Option[u64], unique: bool)
    Map(key: mem::Box[Schema], value: mem::Box[Schema])
    Record(open: bool, fields: mem::List[SchemaField])
    Variant(open: bool, variants: mem::List[SchemaVariant])
    Back(depth: u64)
} Schema

pub closed struct SchemaField derives Encode {
    pub key: text::String
    pub aliases: mem::List[text::String]
    pub defaulted: bool
    pub value: Schema
} SchemaField

pub closed struct SchemaVariant derives Encode {
    pub key: text::String
    pub aliases: mem::List[text::String]
    pub payload: Option[Schema]
} SchemaVariant

pub fn Codec.schema_hash() -> SchemaHash limits pure, notrap, terminates

pub fn Codec.schema(alloc: mem::Alloc) -> Schema
```

| Node | Made for |
| --- | --- |
| `Bool`, `Char`, `Text`, `Bytes` | `bool`, `char`, `text::String`, `mem::Bytes` |
| `Unsigned`, `Signed`, `Float` | integers and floats, `width` in bits, 64 for `usize` and `isize` |
| `BoundedText` | `text::Fixed[N]`, of `capacity` N |
| `Sequence` | `Array` with its `length`, `mem::Bounded` with its `capacity`, `mem::Set`, which is `unique`, and `mem::List` |
| `Map` | `mem::Map` |
| `Record` | structs, labelled payloads, `Unit` |
| `Variant` | enums, `Option`, `Result` |
| `Back` | recursion |

- A `Record` or `Variant` node is `open` when its type is declared `open`. Its fields, one per field of the struct or label of the payload, and its variants, one per variant of the enum, appear in ascending byte order of keys, each with the aliases of its key in byte order. A field is `defaulted` when it takes a default when missing; a variant's `payload` is the node of its payload.
- `mem::Box[T]`, `mem::Shared[T]` and `Untrusted[T]` give the node of `T`; `mem::Id[T]` gives `Unsigned` of width 32.
- Where a node would be made for a struct or enum instantiation inside a node made for the same instantiation, a `Back` node is made instead. Its `depth` is the number of `Record` and `Variant` nodes on the path from it up to the nearest such enclosing node, that node included.
- `schema_hash` gives the schema hash of `T`, and `schema` its schema tree drawn through `alloc`, trapping when memory is exhausted (`trap.exhausted`).

```nan
pub closed struct Probe derives Encode {
    pub seq: u32
    pub hops: u8
} Probe

pub const PROBE_SCHEMA: mem::wire::SchemaHash = mem::wire::Codec[Probe].schema_hash()

pub fn hop_width(alloc: mem::Alloc) -> u8
    example hop_width(testing::world().alloc) == 8
    example PROBE_SCHEMA != mem::wire::Codec[u32].schema_hash()
{
    let mem::wire::Schema.Record(open: _, fields: fields) = mem::wire::Codec[Probe].schema(alloc) else {
        return 0
    }
    let mem::wire::Schema.Unsigned(width: width) = fields[0].value else {
        return 0
    }
    return width
} hop_width
```

## Wire format

`mem::wire` is the canonical binary format. `Codec.encode` writes exactly one byte sequence for each value, and `Codec.decode` rejects every sequence `Codec.encode` never writes, except unknown, aliased and missing fields where the [decoding rules](#decoding-rules) accept them, and float values for `f32`.

```nan-api
// module: mem::wire

pub open struct Codec[T: Encode] {
} Codec

pub fn Codec.encode(value: T, mut out: mem::Bytes)

pub fn Codec.try_encode(value: T, mut out: mem::Bytes) fails mem::OutOfMemory

pub fn Codec.decode(raw: Untrusted[Slice[u8]], alloc: mem::Alloc) -> T fails core::DecodeError
```

- `encode` appends to `out` and traps when memory is exhausted (`trap.exhausted`); `try_encode` fails with `mem::OutOfMemory.Exhausted` and leaves `out` unchanged.
- `decode` reads exactly one value; bytes left after it fail with `Trailing`.

A **uvar** is an unsigned integer in LEB128: 7 bits per byte, least significant group first, the high bit set on every byte but the last. A uvar is minimal: its last byte is not 0 unless it is the only byte. A uvar that is not minimal or whose value exceeds 2^64 − 1 fails with `Malformed`.

| Kind | Bytes |
| --- | --- |
| bool | `0x00` or `0x01`; another byte fails with `Malformed` |
| unsigned | the value as a uvar |
| signed | the uvar of `(n << 1) ^ (n >> 63)` on the 64-bit two's complement pattern of n, the right shift arithmetic |
| float | the 8 bytes of the `f64` bit pattern, little-endian |
| char | the scalar value as a uvar |
| text, bytes | the byte length as a uvar, then the bytes |
| sequence | the count as a uvar, then each element |
| map | the count as a uvar, then each key followed by its value |
| record | the field count as a uvar, then per field its key as text, the byte length of its value as a uvar, and the value, fields in strictly ascending byte order of keys |
| variant | the key as text, then `0x00` without payload, or `0x01`, the payload's byte length as a uvar and the payload; another byte fails with `Malformed` |

- A record whose keys are not in ascending order, or a field value or variant payload not filling exactly its stated length, fails with `Malformed`; a repeated key fails with `DuplicateKey`.

```nan
pub closed enum Signal derives Eq, Encode {
    Idle
    Level(u16)
} Signal

pub fn load_signal(raw: Untrusted[Slice[u8]], alloc: mem::Alloc) -> Signal fails core::DecodeError {
    return try mem::wire::Codec[Signal].decode(raw, alloc)
} load_signal

test signal_bytes for load_signal {
    let world = testing::world()
    let idle = attempt load_signal(Untrusted.of(b"\x04Idle\x00"), alloc: world.alloc)
    let level = attempt load_signal(Untrusted.of(b"\x05Level\x01\x02\xAC\x02"), alloc: world.alloc)
    let trailing = attempt load_signal(Untrusted.of(b"\x04Idle\x00\x00"), alloc: world.alloc)
    expect idle == Ok(Signal.Idle)
    expect level == Ok(Signal.Level(300))
    expect trailing == Err(core::DecodeError.Trailing)
} signal_bytes
```

## Erased decoders

An **erased decoder** is a `mem::wire::Erased`: the schema hash of a type and a function checking `mem::wire` bytes against the type, whose caller need not name it.

```nan-api
// module: mem::wire

pub open struct Erased derives Copy {
    pub schema: SchemaHash
    pub canonical: fn(raw: Untrusted[Slice[u8]], alloc: mem::Alloc) -> Untrusted[mem::Bytes] fails core::DecodeError limits terminates
} Erased

pub fn Codec.canonical(raw: Untrusted[Slice[u8]], alloc: mem::Alloc) -> Untrusted[mem::Bytes] fails core::DecodeError

pub fn Codec.erased() -> Erased limits pure, notrap, terminates
```

- `canonical` reads one value as `decode` does, then gives the bytes `encode` writes for it, drawn through `alloc`, failing with `OutOfMemory` when it cannot draw them. Its result stays Untrusted, so it reads no contents for its caller.
- `erased` gives the schema hash of `T` and `Codec[T].canonical`.
- A package exports the erased decoders of its types by contributing them to a [registry](25-registries.md#registries), through which code checks records by their schema hashes without naming their types.

```nan
pub closed struct Probe derives Encode {
    pub seq: u32
    pub hops: u8 where hops < 16
} Probe

retired Probe.ttl => Probe.hops preserves

pub registry decoders: mem::wire::Erased keyed by schema

contribute decoders probe_decoder = mem::wire::Codec[Probe].erased()

pub fn checked(schema: mem::wire::SchemaHash, raw: Untrusted[Slice[u8]], alloc: mem::Alloc) -> Untrusted[mem::Bytes] fails core::DecodeError {
    for decoder in decoders {
        if decoder.schema == schema {
            return try (decoder.canonical)(raw, alloc)
        }
    }
    fail core::DecodeError.Invalid
} checked

test probes_are_checked for checked {
    let world = testing::world()
    let schema = mem::wire::Codec[Probe].schema_hash()
    let legacy = attempt checked(schema, raw: Untrusted.of(b"\x02\x03seq\x01\x05\x03ttl\x01\x03"), alloc: world.alloc)
    let far = attempt checked(schema, raw: Untrusted.of(b"\x02\x04hops\x01\x13\x03seq\x01\x05"), alloc: world.alloc)
    let Ok(canonical) = legacy else {
        expect false
        return
    }
    let current = attempt mem::wire::Codec[Probe].decode(canonical.view(), alloc: world.alloc)
    expect canonical.byte_len() == 14
    expect current.is_ok()
    expect far.is_err()
} probes_are_checked
```

## Migrations

A **migration** is a function declared `migrate` converting the values of a struct or enum `T` from or to a **frozen shape**: a type whose schema tree is one that `T` had, recorded in its package's [history](29-identity-versioning.md#history). A **forward** migration converts from a frozen shape, an **inverse** to one.

- A forward migration has the parameters `take old: S` and optionally `alloc: mem::Alloc` and the result `T`; an inverse has the parameters `self` and optionally `alloc: mem::Alloc` and the result `S`. Either may fail with `mem::OutOfMemory`, and the names of its parameters are free. `T` and the frozen shape `S` are distinct structs or enums of the migration's package without type parameters, each deriving `Encode`. A migration of another form is an error (`encode.migrate-form`).
- A migration demands `notrap` and `terminates`; one that does not is an error (`encode.migrate-limits`), whose exact fix adds them. One whose digest has an effect fact other than alloc, a capability or validator, or a class other than `traceless` is an error (`encode.migrate-effect`).
- The schema hash of `S` is one that the history records for `T` and not `T`'s own; another is an error (`encode.migrate-schema`).
- Of the forward migrations of `T` from frozen shapes of one schema hash, and of its inverses to such shapes, each but the first by file path and line is an error (`encode.migrate-duplicate`). An inverse to a frozen shape whose schema hash no forward migration of `T` converts from is an error (`encode.migrate-inverse-alone`).

```nan-api
// module: mem::wire

pub fn Codec.decode_from(schema: SchemaHash, raw: Untrusted[Slice[u8]], alloc: mem::Alloc) -> T fails core::DecodeError

pub fn Codec.encode_as(value: T, schema: SchemaHash, mut out: mem::Bytes, alloc: mem::Alloc) -> bool
```

- `decode_from` reads one value as `decode` does when `schema` is the schema hash of `T`. When it is that of a frozen shape a forward migration of `T` converts from, it reads one value of that shape as `decode` does and gives the migration's conversion of it, drawn through `alloc`, failing with `OutOfMemory` when the migration fails. Otherwise it fails with `UnknownSchema`.
- `encode_as` appends `value` as `encode` does and gives `true` when `schema` is the schema hash of `T`. When it is that of a frozen shape an inverse of `T` converts to, it appends the encoding of the inverse's conversion of `value`, drawn through `alloc`, and gives `true`. Otherwise it gives `false` and leaves `out` unchanged. It traps when memory is exhausted (`trap.exhausted`).

```nan
// file: settings/package.nan
//! Settings kept across releases.

package settings {
    language 1
    targets any
} settings

// file: settings/.nan/history.nan
nan history 1
name "settings"
module settings
schema Settings "ahp37ganadyzqu6xs3ebh5st5xa5x4g56uumirr2tiglvgkifvyyi43"
check "afih66hobkrodcs6u7hsifjemt3242epjbzeciviyckgq7j5stdv2x5"

// file: settings/settings.nan
//! The settings record and the shape of its first release.

pub closed struct Settings derives Eq, Encode {
    pub retries: u8
    pub backoff: u16
} Settings

struct FirstSettings derives Encode {
    retries: u8
} FirstSettings

migrate fn Settings.from_first(take old: FirstSettings) -> Settings limits notrap, terminates {
    return Settings(retries: old.retries, backoff: 100)
} Settings.from_first

migrate fn Settings.to_first(self) -> FirstSettings limits notrap, terminates {
    return FirstSettings(retries: self.retries)
} Settings.to_first

pub fn load(schema: mem::wire::SchemaHash, raw: Untrusted[Slice[u8]], alloc: mem::Alloc) -> Settings fails core::DecodeError {
    return try mem::wire::Codec[Settings].decode_from(schema, raw, alloc)
} load

test first_records_load for load {
    let world = testing::world()
    let older = mem::wire::Codec[FirstSettings].schema_hash()
    let loaded = attempt load(older, raw: Untrusted.of(b"\x01\x07retries\x01\x03"), alloc: world.alloc)
    let unknown = attempt load(mem::wire::Codec[u8].schema_hash(), raw: Untrusted.of(b"\x03"), alloc: world.alloc)
    var out = mem::Bytes.empty(world.alloc)
    let written = mem::wire::Codec[Settings].encode_as(Settings(retries: 4, backoff: 50), schema: older, mut out, alloc: world.alloc)
    expect loaded == Ok(Settings(retries: 3, backoff: 100))
    expect unknown == Err(core::DecodeError.UnknownSchema)
    expect written
    expect out.view() == b"\x01\x07retries\x01\x04"
} first_records_load
```

```nan
pub closed struct Settings derives Encode {
    pub retries: u8
    pub backoff: u16
} Settings

struct FirstSettings derives Encode {
    retries: u8
} FirstSettings

migrate fn Settings.from_first(take old: FirstSettings) -> Settings limits notrap, terminates { // error: encode.migrate-schema
    return Settings(retries: old.retries, backoff: 100)
} Settings.from_first
```

## JSON

`text::json` encodes values as JSON text (RFC 8259) and decodes JSON bytes.

```nan-api
// module: text::json

pub open struct Codec[T: Encode] {
} Codec

pub fn Codec.encode(value: T, mut out: text::String)

pub fn Codec.try_encode(value: T, mut out: text::String) fails mem::OutOfMemory

pub fn Codec.decode(raw: Untrusted[Slice[u8]], alloc: mem::Alloc) -> T fails core::DecodeError
```

`encode` and `try_encode` behave as in [Wire format](#wire-format). `decode` reads exactly one JSON value; bytes other than whitespace left after it fail with `Trailing`. `encode` writes no whitespace and maps kinds to JSON as follows:

| Kind | JSON |
| --- | --- |
| bool | `true`, `false` |
| unsigned, signed | an integer in decimal, `-` before a negative value |
| float | the number as [Render](#render) writes it; `NaN`, `inf` and `-inf` as JSON strings |
| char, text | a JSON string |
| bytes | a JSON string of the bytes in base64 with padding (RFC 4648, section 4) |
| sequence | an array |
| map | an array of 2-element arrays, key then value |
| record | an object, members in ascending byte order of keys |
| variant | without payload the key as a JSON string, otherwise an object whose one member maps the key to the payload |

- JSON strings escape `"` as `\"`, `\` as `\\`, U+000A as `\n`, U+000D as `\r`, U+0009 as `\t`, U+0008 as `\b`, U+000C as `\f`, and other characters below U+0020 as `\u` with 4 lowercase hexadecimal digits; every other character is written as itself.
- `decode` accepts the whitespace and escapes RFC 8259 allows and object members in any order. Input that is not well-formed UTF-8, or an escape naming a lone surrogate, fails with `InvalidText`; other input outside the RFC 8259 grammar, `null` included, fails with `Malformed`.
- An unsigned or signed value is an integer without fraction or exponent (`-0` is 0), an unsigned one outside `u64` or a signed one outside `i64` failing with `OutOfRange`; a float any number, rounded to `f64` by roundTiesToEven, or one of the three JSON strings; a char a JSON string of exactly one char; bytes base64 exactly as `encode` writes it; a map entry an array of exactly 2 elements; a variant with payload an object of exactly 1 member. Other input for these kinds fails with `Malformed`.

```nan
pub closed struct Probe derives Eq, Encode {
    pub seq: u32
    pub hops: u8
} Probe

pub fn probe_json(probe: Probe, mut out: text::String) {
    text::json::Codec[Probe].encode(probe, mut out)
} probe_json

pub fn load_probe_json(raw: Untrusted[Slice[u8]], alloc: mem::Alloc) -> Probe fails core::DecodeError {
    return try text::json::Codec[Probe].decode(raw, alloc)
} load_probe_json

test probe_json_is_canonical for probe_json {
    let world = testing::world()
    var out = text::String.empty(world.alloc)
    probe_json(Probe(seq: 5, hops: 3), mut out)
    expect out.as_str() == "{\"hops\":3,\"seq\":5}"
} probe_json_is_canonical

test probe_json_accepts_spacing for load_probe_json {
    let world = testing::world()
    let loaded = attempt load_probe_json(Untrusted.of(b"{ \"seq\": 5, \"hops\": 3 }"), alloc: world.alloc)
    expect loaded == Ok(Probe(seq: 5, hops: 3))
} probe_json_accepts_spacing
```

## Construction text

**Construction text** is the text form of values that [derived Render](#derived-render) and the std renderings write, as in `Config(mtu: 1500, dhcp: true)`. `text::construction` decodes it.

```nan-api
// module: text::construction

pub open struct Codec[T: Encode] {
} Codec

pub fn Codec.decode(raw: Untrusted[Slice[u8]], alloc: mem::Alloc) -> T fails core::DecodeError
```

`decode` reads exactly one value; bytes other than whitespace left after it fail with `Trailing`. Whitespace, U+0020, U+0009, U+000A and U+000D, may stand before and after each token: a word, a number, a literal, or one of `-inf`, `(`, `)`, `[`, `]`, `{`, `}`, `,`, `:` and `.`. A name or key is a `WORD` ([identifiers](01-lexical.md#identifiers)). Each kind reads as follows:

| Kind | Construction text |
| --- | --- |
| bool | `true`, `false` |
| unsigned, signed | `-` optionally, then a `DEC_INT` |
| float | `NaN`, `inf`, `-inf`, or `-` optionally, a `DEC_INT`, optionally `.` and `DEC_DIGITS`, then optionally `e`, `-` optionally and one or more `DIGIT` |
| char, text, bytes | a `CHAR`, a `TEXT`, a `BYTES` ([text literals](01-lexical.md#text-literals)) |
| sequence | `[`, the elements separated by `,`, `]` |
| map | `{`, each entry as its key, `:` and its value, entries separated by `,`, `}` |
| record | a name, then optionally `(`, each field as its key, `:` and its value, fields separated by `,`, `)` |
| variant | optionally a name and `.`, the key, then for a payload `(`, the payload, `)` |

- A name before a record or variant key is not checked. A record without `(` has no fields, as in `Unit`. A labelled payload is read as the fields of a record without its name and parentheses: `Shape.Rect(w: 2, h: 3)`.
- An unsigned or signed value is an integer without fraction or exponent (`-0` is 0), an unsigned one outside `u64` or a signed one outside `i64` failing with `OutOfRange`; a float is rounded to `f64` by roundTiesToEven. A char literal holds exactly one char; `_` separates digits and carries no meaning.
- A field that an open type skips is any value of these forms.
- Input that is not well-formed UTF-8 inside a literal, or a `\u{H}` escape naming a surrogate or a value above `10FFFF`, fails with `InvalidText`. Other input outside these forms, `\xHH` above `7F` in a char or text included, fails with `Malformed`.

Decoding the rendering of a value within a render budget that does not elide it gives the value back when its type implements `Encode` and every value it renders, itself included, renders as the tables and derived Render above state, holds no `Untrusted` value, whose `<untrusted>` decodes as no kind, and is not text or a char at the top level, which renders as its characters.

```nan
// file: ports/package.nan
//! Port records read back from their rendering.

package ports {
    language 1
    targets any
    allow validate in ports
} ports

// file: ports/ports.nan
//! Ports and their media.

pub closed enum Medium derives Eq, Render, Encode {
    Copper
    Fiber(lanes: u8)
} Medium

pub closed struct Port derives Eq, Render, Encode {
    pub name: text::String
    pub mtu: u16 where mtu >= 576
    pub medium: Medium
} Port

pub fn load_port(raw: Untrusted[Slice[u8]], alloc: mem::Alloc) -> Port fails core::DecodeError {
    return try text::construction::Codec[Port].decode(raw, alloc)
} load_port

test port_reads_its_rendering for load_port {
    let world = testing::world()
    let port = Port(name: text::String.of("eth0", alloc: world.alloc), mtu: 1_500, medium: Medium.Fiber(lanes: 4))
    var out = text::String.empty(world.alloc)
    mut out.write(text::fmt("{port}", port), budget: 128)
    expect out.as_str() == "Port(name: \"eth0\", mtu: 1500, medium: Medium.Fiber(lanes: 4))"
    let loaded = attempt load_port(Untrusted.of(out.as_bytes()), alloc: world.alloc)
    let expected: Result[Port, core::DecodeError] = Ok(take port)
    expect loaded == expected
} port_reads_its_rendering

test port_text_is_checked for load_port {
    let world = testing::world()
    let spaced = attempt load_port(Untrusted.of(b"Port( mtu: 9_000, name: \"lo\", medium: Copper )"), alloc: world.alloc)
    let small = attempt load_port(Untrusted.of(b"Port(name: \"lo\", mtu: 500, medium: Medium.Copper)"), alloc: world.alloc)
    let unknown = attempt load_port(Untrusted.of(b"Port(name: \"lo\", mtu: 1500, medium: Medium.Radio)"), alloc: world.alloc)
    let expected: Result[Port, core::DecodeError] = Ok(Port(name: text::String.of("lo", alloc: world.alloc), mtu: 9_000, medium: Medium.Copper))
    expect spaced == expected
    expect small == Err(core::DecodeError.Invalid)
    expect unknown == Err(core::DecodeError.UnknownVariant)
} port_text_is_checked
```

```nan
pub fn load_mtus(raw: Untrusted[Slice[u8]], alloc: mem::Alloc) -> Result[mem::List[u16], core::DecodeError] {
    return attempt text::construction::Codec[mem::List[u16]].decode(raw, alloc) // error: authority.ungranted
} load_mtus
```

## Diagnostics

| Code | Class | Rule |
| --- | --- | --- |
| `encode.flexible` | error | [Flexible structs](#flexible-structs) |
| `encode.linear` | error | [Requirements](#requirements) |
| `encode.manual-impl` | error | [Requirements](#requirements) |
| `encode.migrate-duplicate` | error | [Migrations](#migrations) |
| `encode.migrate-effect` | error | [Migrations](#migrations) |
| `encode.migrate-form` | error | [Migrations](#migrations) |
| `encode.migrate-inverse-alone` | error | [Migrations](#migrations) |
| `encode.migrate-limits` | error | [Migrations](#migrations) |
| `encode.migrate-schema` | error | [Migrations](#migrations) |
| `encode.not-inert` | error | [Requirements](#requirements) |
| `encode.view` | error | [Requirements](#requirements) |
| `text.argument-duplicate` | error | [Templates](#templates) |
| `text.argument-unused` | error | [Templates](#templates) |
| `text.bare-len` | error | [Lengths and slicing](#lengths-and-slicing) |
| `text.hole-unbound` | error | [Templates](#templates) |
| `text.integer-index` | error | [Lengths and slicing](#lengths-and-slicing) |
| `text.template-malformed` | error | [Templates](#templates) |
| `text.template-not-literal` | error | [Templates](#templates) |
| `text.template-position` | error | [Templates](#templates) |
