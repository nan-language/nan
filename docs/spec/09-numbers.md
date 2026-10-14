# Numbers

This chapter defines the operators on number types, integer overflow and its named alternatives, division, shifts, bit counts, wide multiplication, the iteration of ranges, named conversions with bit patterns, floats and their total order.

## Number operators

The integer types are the unsigned [scalars](04-types.md#scalars) `u8`, `u16`, `u32`, `u64` and `usize` and the signed `i8`, `i16`, `i32`, `i64` and `isize`. With the float types `f32` and `f64`, they are the number types. The operators of [Statements and expressions](06-statements-expressions.md#operators) apply to them as this table states.

| Operator | Unsigned integers | Signed integers | Floats |
| --- | --- | --- | --- |
| `a + b`, `a - b`, `a * b` | [Overflow](#overflow) | [Overflow](#overflow) | [Floats](#floats) |
| `-a` | `num.unsigned-negation` | [Overflow](#overflow) | [Floats](#floats) |
| `a / b`, `a % b` | [Division](#division) | `num.signed-division` | [Floats](#floats) |
| `a & b`, `a \| b`, `a ^ b`, `~a` | bitwise | bitwise | no |
| `a << n` | [Shifts](#shifts) | [Shifts](#shifts) | no |
| `a >> n` | [Shifts](#shifts) | `num.signed-shift-right` | no |
| `==`, `!=`, `<`, `<=`, `>`, `>=` | [`Eq` and `Ord`](08-generics-interfaces.md#equality-ordering-hashing-and-defaults) | [`Eq` and `Ord`](08-generics-interfaces.md#equality-ordering-hashing-and-defaults) | [built in](08-generics-interfaces.md#float-comparison) |

- A number type T implements the [operator interface](08-generics-interfaces.md#operator-interfaces) of each arithmetic, bitwise and shift operator whose cell links a section or reads bitwise, with `R` and `O` both T, except that `R` is `u32` for `core::Shl` and `core::Shr`. For a cell holding a code or reading "no", T implements no such interface. A cell holding a code reports that code in place of `interface.not-implemented`.
- A compound assignment `p op= e` follows the row of `op`, codes included. When `p` is a binding or a field path, its exact fix rewrites it to `p = ` followed by the fixed form of `p op e`; otherwise it has none.
- In the exact fixes of `num.signed-division` and `num.signed-shift-right`, a left operand that is not a postfix expression is parenthesised: `-a >> 1` becomes `(-a).shr_arith(1)`.
- `&`, `|` and `^` compute bitwise and, or and exclusive or, and `~` inverts every bit, over the representation: two's complement for signed integers. They never trap.
- Each trap this chapter states is an [obligation](18-contracts.md#obligations) at the operation's site. The [digest facts](10-effects-digests.md#digests) of every operation and function of this chapter are exactly terminates, may-trap where it keeps a trap site, and float use where it involves a float type.
- Every result in this chapter is the same in every build and on every target; the [interpreter](21-interpreter.md#interpreter) and [lowering](30-artefacts.md#lowering) produce identical bit patterns.

```nan
pub const READY: u8 = 0b0000_0001

pub fn is_ready(status: u8) -> bool
    example is_ready(0b1000_0001)
    example not is_ready(0b1000_0000)
{
    return (status & READY) == READY
} is_ready
```

## Overflow

`a + b`, `a - b` and `a * b` on integers, and `-a` on signed integers, compute the mathematical result r and trap when r is not a value of the operand type (`trap.overflow`). `-a` on an unsigned integer type is an error (`num.unsigned-negation`); a [negative literal](04-types.md#expected-types-and-literals) is no negation.

Four method families name the alternatives, for `add`, `sub` and `mul` on every integer type and for `neg` on signed integer types. For a type of width w (the Bits column of [scalars](04-types.md#scalars)), none of them traps:

- `wrapping_op` returns the value of the type congruent to r modulo 2^w.
- `saturating_op` returns r clamped to the type's range.
- `checked_op` returns `Some(r)` when r is a value of the type, and `None` otherwise.
- `overflowing_op` returns `core::Overflowing(value: v, overflowed: o)`, where v is the `wrapping_op` result and o is `true` exactly when r is not a value of the type.

```nan-api
// module: core

pub closed struct Overflowing[T: Copy and Eq] derives Copy, Eq {
    pub value: T
    pub overflowed: bool
} Overflowing
```

Core declares these items for `u32`, and the same items for `u8`, `u16`, `u64` and `usize` with that type's name in place of `u32` everywhere, literal suffixes included.

```nan-api
// module: core

pub fn u32.wrapping_add(self, other: u32) -> u32 limits pure, notrap, terminates
    example (~0u32).wrapping_add(1) == 0

pub fn u32.saturating_add(self, other: u32) -> u32 limits pure, notrap, terminates
    example (~0u32).saturating_add(1) == (~0u32)

pub fn u32.checked_add(self, other: u32) -> Option[u32] limits pure, notrap, terminates
    example (~0u32).checked_add(1) == None

pub fn u32.overflowing_add(self, other: u32) -> Overflowing[u32] limits pure, notrap, terminates
    example (~0u32).overflowing_add(1) == Overflowing(value: 0, overflowed: true)

pub fn u32.wrapping_sub(self, other: u32) -> u32 limits pure, notrap, terminates
    example 0u32.wrapping_sub(1) == (~0u32)

pub fn u32.saturating_sub(self, other: u32) -> u32 limits pure, notrap, terminates
    example 0u32.saturating_sub(1) == 0

pub fn u32.checked_sub(self, other: u32) -> Option[u32] limits pure, notrap, terminates
    example 0u32.checked_sub(1) == None

pub fn u32.overflowing_sub(self, other: u32) -> Overflowing[u32] limits pure, notrap, terminates
    example 0u32.overflowing_sub(1) == Overflowing(value: ~0u32, overflowed: true)

pub fn u32.wrapping_mul(self, other: u32) -> u32 limits pure, notrap, terminates
    example (~0u32).wrapping_mul(2) == (~1u32)

pub fn u32.saturating_mul(self, other: u32) -> u32 limits pure, notrap, terminates
    example (~0u32).saturating_mul(2) == (~0u32)

pub fn u32.checked_mul(self, other: u32) -> Option[u32] limits pure, notrap, terminates
    example (~0u32).checked_mul(2) == None

pub fn u32.overflowing_mul(self, other: u32) -> Overflowing[u32] limits pure, notrap, terminates
    example (~0u32).overflowing_mul(2) == Overflowing(value: ~1u32, overflowed: true)
```

Core declares these items for `i32`, and the same items for `i8`, `i16`, `i64` and `isize` with that type's name in place of `i32` everywhere, literal suffixes included.

```nan-api
// module: core

pub fn i32.wrapping_add(self, other: i32) -> i32 limits pure, notrap, terminates
    example (-1i32).wrapping_add(1) == 0

pub fn i32.saturating_add(self, other: i32) -> i32 limits pure, notrap, terminates
    example (-1i32).saturating_add(-1) == -2

pub fn i32.checked_add(self, other: i32) -> Option[i32] limits pure, notrap, terminates
    example (-1i32).checked_add(1) == Some(0)

pub fn i32.overflowing_add(self, other: i32) -> Overflowing[i32] limits pure, notrap, terminates
    example (-1i32).overflowing_add(1) == Overflowing(value: 0, overflowed: false)

pub fn i32.wrapping_sub(self, other: i32) -> i32 limits pure, notrap, terminates
    example 0i32.wrapping_sub(1) == -1

pub fn i32.saturating_sub(self, other: i32) -> i32 limits pure, notrap, terminates
    example 0i32.saturating_sub(1) == -1

pub fn i32.checked_sub(self, other: i32) -> Option[i32] limits pure, notrap, terminates
    example 0i32.checked_sub(1) == Some(-1)

pub fn i32.overflowing_sub(self, other: i32) -> Overflowing[i32] limits pure, notrap, terminates
    example 0i32.overflowing_sub(1) == Overflowing(value: -1, overflowed: false)

pub fn i32.wrapping_mul(self, other: i32) -> i32 limits pure, notrap, terminates
    example (-2i32).wrapping_mul(3) == -6

pub fn i32.saturating_mul(self, other: i32) -> i32 limits pure, notrap, terminates
    example (-2i32).saturating_mul(3) == -6

pub fn i32.checked_mul(self, other: i32) -> Option[i32] limits pure, notrap, terminates
    example (-2i32).checked_mul(3) == Some(-6)

pub fn i32.overflowing_mul(self, other: i32) -> Overflowing[i32] limits pure, notrap, terminates
    example (-2i32).overflowing_mul(3) == Overflowing(value: -6, overflowed: false)

pub fn i32.wrapping_neg(self) -> i32 limits pure, notrap, terminates
    example (-5i32).wrapping_neg() == 5

pub fn i32.saturating_neg(self) -> i32 limits pure, notrap, terminates
    example (-5i32).saturating_neg() == 5

pub fn i32.checked_neg(self) -> Option[i32] limits pure, notrap, terminates
    example (-5i32).checked_neg() == Some(5)

pub fn i32.overflowing_neg(self) -> Overflowing[i32] limits pure, notrap, terminates
    example (-5i32).overflowing_neg() == Overflowing(value: 5, overflowed: false)
```

```nan
pub fn next_sequence(seq: u16) -> u16
    example next_sequence(65_535) == 0
{
    return seq.wrapping_add(1)
} next_sequence

pub fn opposite(delta: i8) -> Option[i8]
    example opposite(-128) == None
    example opposite(5) == Some(-5)
{
    return delta.checked_neg()
} opposite

pub fn clamped_gain(level: i8, gain: i8) -> i8
    example clamped_gain(100, gain: 100) == 127
    example clamped_gain(-100, gain: -100) == -128
{
    return level.saturating_add(gain)
} clamped_gain
```

```nan
pub fn total_frames(frames: u8, extra: u8) -> u8 {
    return frames + extra // traps: trap.overflow
} total_frames

test total_frames_past_capacity for total_frames {
    expect total_frames(200, extra: 100) == 44
} total_frames_past_capacity
```

```nan
pub fn reversed(offset: u32) -> u32 {
    return -offset // error: num.unsigned-negation
} reversed
```

## Division

On unsigned integers, `a / b` is the quotient rounded toward zero and `a % b` is a − b × (a / b). On signed integers, `/` and `%` are errors (`num.signed-division`), whose exact fix rewrites `a / b` to `a.div_trunc(b)` and `a % b` to `a.rem_trunc(b)`.

Signed integer types declare four division methods. With q the exact quotient a ÷ b:

- `a.div_trunc(b)` is q rounded toward zero, and `a.rem_trunc(b)` is a − b × `a.div_trunc(b)`, which is 0 or has the sign of a.
- `a.div_floor(b)` is q rounded toward negative infinity, and `a.mod_floor(b)` is a − b × `a.div_floor(b)`, which is 0 or has the sign of b.

`/` and `%` on unsigned integers and the four methods trap when b is 0 (`trap.division-by-zero`). `div_trunc` and `div_floor` of the type's minimum by −1 trap (`trap.overflow`); `rem_trunc` and `mod_floor` return 0 for those operands.

Core declares these items for `i32`, and the same items for `i8`, `i16`, `i64` and `isize` with that type's name in place of `i32` everywhere, literal suffixes included.

```nan-api
// module: core

pub fn i32.div_trunc(self, divisor: i32) -> i32 limits pure, terminates
    example (-7i32).div_trunc(2) == -3

pub fn i32.rem_trunc(self, divisor: i32) -> i32 limits pure, terminates
    example (-7i32).rem_trunc(2) == -1

pub fn i32.div_floor(self, divisor: i32) -> i32 limits pure, terminates
    example (-7i32).div_floor(2) == -4

pub fn i32.mod_floor(self, divisor: i32) -> i32 limits pure, terminates
    example (-7i32).mod_floor(2) == 1
```

```nan
pub fn bucket(offset: i32, width: i32) -> i32
    requires width > 0
    example bucket(-1, width: 16) == -1
    example bucket(17, width: 16) == 1
{
    return offset.div_floor(width)
} bucket

pub fn average(total: u64, count: u64) -> u64
    requires count > 0
    example average(10, count: 4) == 2
{
    return total / count
} average
```

```nan
pub fn bucket(offset: i32, width: i32) -> i32 {
    return offset / width // error: num.signed-division
} bucket
```

```nan
pub fn per_item(total: u32, items: u32) -> u32 {
    return total / items // traps: trap.division-by-zero
} per_item

test per_item_without_items for per_item {
    expect per_item(10, items: 0) == 0
} per_item_without_items
```

## Shifts

`<<`, `>>`, `shr_arith` and `wrapping_shl` trap when the amount n is at least the width w of the shifted type (`trap.shift`), before any other rule of this section applies. For n below w:

- On an unsigned integer, `a << n` is a × 2^n modulo 2^w, and `a >> n` is a ÷ 2^n rounded toward zero.
- On a signed integer, `a << n` is a × 2^n, and traps when that value is not a value of the type (`trap.overflow`).
- On a signed integer, `>>` is an error (`num.signed-shift-right`), whose exact fix rewrites `a >> n` to `a.shr_arith(n)`. `a.shr_arith(n)` is a ÷ 2^n rounded toward negative infinity (copies of the sign bit shift in).
- On a signed integer, `a.wrapping_shl(n)` is the value of the type congruent to a × 2^n modulo 2^w.

On a signed integer, `a.checked_shl(n)` is `None` when n is at least w or a × 2^n is not a value of the type, and `Some(a × 2^n)` otherwise; it never traps.

Core declares these items for `i32`, and the same items for `i8`, `i16`, `i64` and `isize` with that type's name in place of `i32` everywhere, literal suffixes included.

```nan-api
// module: core

pub fn i32.shr_arith(self, amount: u32) -> i32 limits pure, terminates
    example (-8i32).shr_arith(1) == -4

pub fn i32.wrapping_shl(self, amount: u32) -> i32 limits pure, terminates
    example (-3i32).wrapping_shl(2) == -12

pub fn i32.checked_shl(self, amount: u32) -> Option[i32] limits pure, notrap, terminates
    example (-3i32).checked_shl(64) == None
```

```nan
pub fn page_number(address: u64) -> u64
    example page_number(0x3000) == 3
{
    return address >> 12
} page_number

pub fn half_down(value: i32) -> i32
    example half_down(-3) == -2
{
    return value.shr_arith(1)
} half_down
```

```nan
pub fn half_down(value: i32) -> i32 {
    return value >> 1 // error: num.signed-shift-right
} half_down
```

```nan
pub fn bit(index: u32) -> u32 {
    return 1u32 << index // traps: trap.shift
} bit

test bit_past_width for bit {
    expect bit(32) == 0
} bit_past_width
```

## Bit counts

Every integer type declares three bit counts over its representation: `count_ones` counts the 1 bits, `leading_zeros` the 0 bits above the highest 1 bit, and `trailing_zeros` the 0 bits below the lowest 1 bit. For the value 0, `leading_zeros` and `trailing_zeros` are the width.

Core declares these items for `u64`, and the same items for every other integer type with that type's name in place of `u64` and its width in place of `64`; every bit count is a `u32`.

```nan-api
// module: core

pub fn u64.count_ones(self) -> u32 limits pure, notrap, terminates
    ensures result <= 64
    example 0b1011u64.count_ones() == 3

pub fn u64.leading_zeros(self) -> u32 limits pure, notrap, terminates
    ensures result <= 64
    example 0u64.leading_zeros() == 64

pub fn u64.trailing_zeros(self) -> u32 limits pure, notrap, terminates
    ensures result <= 64
    example 8u64.trailing_zeros() == 3
```

## Wide multiplication

`a.wide_mul(b)` on `u64` and on `i64` returns the exact product p as a `core::WideProduct[T]`, where p = `high` × 2^64 + `low` and `low` lies in 0 to 2^64 − 1. No integer type is wider than 64 bits.

```nan-api
// module: core

pub closed struct WideProduct[T: Copy and Eq] derives Copy, Eq {
    pub high: T
    pub low: u64
} WideProduct

pub fn u64.wide_mul(self, other: u64) -> WideProduct[u64] limits pure, notrap, terminates
    example (~0u64).wide_mul(2) == WideProduct(high: 1, low: ~1u64)

pub fn i64.wide_mul(self, other: i64) -> WideProduct[i64] limits pure, notrap, terminates
    example (-1i64).wide_mul(1) == WideProduct(high: -1, low: ~0u64)
```

```nan
pub fn mul_high(left: u64, right: u64) -> u64
    example mul_high(0x1_0000_0000, right: 0x1_0000_0000) == 1
{
    return left.wide_mul(right).high
} mul_high
```

## Ranges

For T an integer type or `char`, core declares `core::Range.next` with this head, and `core::Range[T]` ([ranges](06-statements-expressions.md#ranges)) implements `core::Cursor[T]` ([iteration](07-ownership.md#iteration)) and is its own cursor. For any other T, `core::Range[T]` has no member `next` and is no cursor.

```nan-api
// module: core

pub fn Range.next(mut self) -> Option[T] limits pure, notrap, terminates
```

With the values of T ordered by `Ord`:

- `a..<b` yields every value x of T with a ≤ x < b, and `a..=b` every value with a ≤ x ≤ b, in increasing order.
- `core::Range.next` returns `None` and leaves the range unchanged when the range is empty. Otherwise it returns `Some(low)` and then, when `low` is below `high`, sets `low` to the least value of T above it, and when they are equal, sets `inclusive` to `false`.
- Iteration never computes a value outside the bounds and never traps, including when `high` is the maximum of T.
- In the body of `for x in r`, with a and b the bounds of the range `r` when the loop starts, x satisfies a ≤ x < b, or a ≤ x ≤ b for an inclusive range, as a fact for the [prover](20-prover.md#queries).

```nan
pub fn byte_values() -> u32
    example byte_values() == 256
{
    var count: u32 = 0
    for _ in 0u8..=255 {
        count += 1
    }
    return count
} byte_values
```

## Conversions

A **named conversion** is one of the built-in functions that change a value's type: `widen`, `fit`, `checked_fit`, `truncate`, `saturate` and `nearest`, which convert between number types, and the bit conversions of [Bit patterns](#bit-patterns).

Each of the six is an associated function `T.name(value)` of a number type T, the target, whose argument has any number type S, the source, and no expected type. It is pure and terminating, and it is notrap except `fit`. `T.name` naming one of the six without an argument list, for T a scalar, is an error (`num.conversion-value`).

A **widening pair** is a source S and a target T, distinct, such that every value of S is exactly a value of T by the value table of [scalars](04-types.md#scalars), such as `u8` to `i16`, `usize` to `u64`, `i32` to `f64` and `f32` to `f64`. `widen` accepts exactly the widening pairs. The other five accept only distinct pairs that are not widening pairs, as this table gives:

| Conversion | integer to integer | integer to float | float to integer | `f64` to `f32` |
| --- | --- | --- | --- | --- |
| `fit`, `checked_fit` | yes | yes | yes | yes |
| `truncate` | yes | no | no | no |
| `saturate` | yes | no | yes | no |
| `nearest` | no | yes | yes | yes |

The checker applies the first of these rules that matches a call `T.name(x)` of one of the six with T a scalar:

1. A call where T or S is not a number type is an error (`num.conversion-undefined`).
2. A call where S is T is an error (`num.same-type-conversion`), whose exact fix replaces it with `x`.
3. `widen` on a pair that is not a widening pair is an error (`num.widen-lossy`), whose exact fix writes `T.fit(x)`.
4. Another conversion on a widening pair is an error (`num.widen-applies`), whose exact fix writes `T.widen(x)`.
5. A pair the table marks "no" is an error (`num.conversion-undefined`).

The conversions compute, for w the width of T:

- `T.widen(x)` is x, except that a NaN gives the [canonical NaN](#floats).
- `T.fit(x)` is x when x is exactly a value of T, and traps otherwise (`trap.conversion`). A float converts to an integer type only when it is finite and integral, `-0.0` giving 0. From `f64` to `f32`, infinities and NaNs fit, a NaN giving the canonical NaN.
- `T.checked_fit(x)` is `Some(T.fit(x))` when `T.fit(x)` does not trap, and `None` otherwise.
- `T.truncate(x)` is the value of T congruent to x modulo 2^w: the low w bits.
- `T.saturate(x)` is x clamped to the range of T; a float x is first rounded toward zero, and a NaN gives 0.
- `T.nearest(x)` for a float T is x rounded to T by roundTiesToEven, as [Floats](#floats) defines. For an integer T, it is x rounded to the nearest integer, ties to the even one, then clamped to the range of T; a NaN gives 0.

```nan
pub fn frame_bytes(frames: u32, size: u16) -> u64 {
    return u64.widen(frames) * u64.widen(size)
} frame_bytes

pub fn level_byte(level: u32) -> Option[u8]
    example level_byte(300) == None
{
    return u8.checked_fit(level)
} level_byte

pub fn low_byte(word: u32) -> u8
    example low_byte(0x1234) == 0x34
{
    return u8.truncate(word)
} low_byte

pub fn clamp_byte(value: i32) -> u8
    example clamp_byte(-5) == 0
    example clamp_byte(300) == 255
{
    return u8.saturate(value)
} clamp_byte

pub fn rounded(value: f64) -> i64
    example rounded(2.5) == 2
    example rounded(-1.7) == -2
{
    return i64.nearest(value)
} rounded
```

```nan
pub fn level_byte(level: u32) -> u8 {
    return u8.widen(level) // error: num.widen-lossy
} level_byte
```

```nan
pub fn frame_count(frames: u16) -> u32 {
    return u32.fit(frames) // error: num.widen-applies
} frame_count
```

```nan
pub fn apply_to_byte(byte: u8, step: fn(value: u8) -> u64) -> u64 {
    return step(byte)
} apply_to_byte

pub fn byte_as_word(byte: u8) -> u64 {
    return apply_to_byte(byte, step: u64.widen) // error: num.conversion-value
} byte_as_word
```

```nan
pub fn channel(raw: u32) -> u8 {
    return u8.fit(raw) // traps: trap.conversion
} channel

test channel_past_range for channel {
    expect channel(256) == 0
} channel_past_range
```

### Bit patterns

The **bits type** of a scalar is the unsigned integer type of its Bits width: `u8` for `bool`, `u32` for `char` and `f32`, `u64` for `f64`, `usize` and `isize`, and the unsigned type of equal width for a signed integer. The bits type of a [layout](23-low-level.md#layouts) is the one that section states.

For a scalar or layout type T with bits type B, the bit conversions are the built-in members `T.to_bits(self) -> B`, `T.from_bits(bits: B) -> T` and `T.checked_from_bits(bits: B) -> Option[T]`, each pure, notrap and terminating.

- `x.to_bits()` returns the bit pattern of x as its bits type: two's complement for signed integers, the IEEE 754 encoding for floats, 0 or 1 for `bool`, the scalar value for `char`, and its bits for a layout.
- `T.from_bits(bits)` returns the value of T whose pattern is `bits`. A use of `T.from_bits` where T is not [AnyBits](05-properties.md#anybits) is an error (`num.from-bits-not-anybits`), whose exact fix writes `T.checked_from_bits` in its place.
- `T.checked_from_bits(bits)` returns `None` when `bits` is a [niche](04-types.md#niches) of T, and `Some` of the value whose pattern is `bits` otherwise.
- On `u8`, `u16`, `u32` and `u64`, each its own bits type, a call of a bit conversion is an error (`num.same-type-conversion`), whose exact fix replaces the call with its operand.
- No bit conversion changes a bit: `f64.from_bits(x.to_bits())` keeps a NaN's payload.

```nan
pub fn is_negative_zero(value: f32) -> bool
    example is_negative_zero(-0.0)
    example not is_negative_zero(0.0)
{
    return value.to_bits() == 0x8000_0000
} is_negative_zero
```

```nan
pub fn flag(raw: u8) -> bool {
    return bool.from_bits(raw) // error: num.from-bits-not-anybits
} flag
```

## Floats

`f32` and `f64` are the IEEE 754 binary32 and binary64 formats, with strict semantics:

- `a + b`, `a - b`, `a * b`, `a / b`, `sqrt`, `fused_mul_add` and every named conversion producing a float return the IEEE 754 result correctly rounded by roundTiesToEven, the only rounding mode.
- Subnormal operands and results are kept. No operation is contracted, reassociated or computed at a wider precision: `a * b + c` rounds twice, and `a.fused_mul_add(b, addend: c)` rounds a × b + c once.
- `a % b` is a − b × n, for n the exact quotient a ÷ b rounded toward zero, computed exactly; the result has the sign of a. It is NaN when a is infinite, b is zero or either is NaN, and a when b is infinite and a finite.
- An operation or named conversion whose result is a NaN returns the **canonical NaN**, of sign 0 and quiet with a zero payload: `0x7FC0_0000` in `f32` and `0x7FF8_0000_0000_0000` in `f64`. The exceptions are `-a`, which inverts only the sign bit, `abs`, which clears only the sign bit, and the bit conversions.
- Float arithmetic and comparison never trap: division by zero gives an infinity or NaN as IEEE 754 states, and exception flags are not observable.

Core declares these items for `f64`, and the same items for `f32` with `f32` in place of `f64` everywhere. `core::Ordering` is defined in [Generics and interfaces](08-generics-interfaces.md#equality-ordering-hashing-and-defaults).

```nan-api
// module: core

pub fn f64.abs(self) -> f64 limits pure, notrap, terminates
    example (-2.5f64).abs() == 2.5

pub fn f64.sqrt(self) -> f64 limits pure, notrap, terminates
    example 2.25f64.sqrt() == 1.5

pub fn f64.fused_mul_add(self, factor: f64, addend: f64) -> f64 limits pure, notrap, terminates
    example 2.0f64.fused_mul_add(3.0, addend: 1.0) == 7.0

pub fn f64.is_nan(self) -> bool limits pure, notrap, terminates
    example (0.0f64 / 0.0).is_nan()

pub fn f64.total_cmp(self, other: f64) -> Ordering limits pure, notrap, terminates
    example (-0.0f64).total_cmp(0.0) == Ordering.Less
```

```nan
pub fn lerp(start: f64, end: f64, t: f64) -> f64
    example lerp(1.0, end: 3.0, t: 0.5) == 2.0
{
    return (end - start).fused_mul_add(t, addend: start)
} lerp

pub fn below_limit(reading: f64, limit: f64) -> bool
    example below_limit(1.0, limit: 2.0)
    example not below_limit(0.0 / 0.0, limit: 2.0)
{
    return reading < limit
} below_limit
```

## Total order

`a.total_cmp(b)` orders floats by their bit patterns: a pattern with sign bit 1 precedes one with sign bit 0; between two with sign bit 0, the smaller pattern as an unsigned integer precedes; between two with sign bit 1, the larger precedes. This is the IEEE 754 totalOrder predicate, with `-0.0` before `0.0` and each NaN beyond the infinity of its sign. Two floats compare equal exactly when their bit patterns are identical.

`core::TotalF64` and `core::TotalF32` wrap one float each to give it `Eq`, `Ord` and `Hash`. Core declares these items for `TotalF64`, and the same items for `TotalF32` with `f32` in place of `f64` and `TotalF32` in place of `TotalF64` everywhere. `hash` writes what the `hash` of `self.value.to_bits()` writes.

```nan-api
// module: core

pub closed struct TotalF64 derives Copy {
    pub value: f64
} TotalF64

impl TotalF64: Eq

impl TotalF64: Ord

impl TotalF64: Hash

pub fn TotalF64.eq(self, other: TotalF64) -> bool limits pure, notrap, terminates
    ensures result == (self.value.to_bits() == other.value.to_bits())
    example not TotalF64(value: -0.0).eq(TotalF64(value: 0.0))

pub fn TotalF64.cmp(self, other: TotalF64) -> Ordering limits pure, notrap, terminates
    ensures result == self.value.total_cmp(other.value)
    example TotalF64(value: 1.0).cmp(TotalF64(value: 2.0)) == Ordering.Less

pub fn TotalF64.hash[H: Hasher](self, mut hasher: H) limits pure, notrap, terminates
```

```nan
pub fn precedes(left: f64, right: f64) -> bool
    example precedes(-0.0, right: 0.0)
    example precedes(1.0, right: 0.0 / 0.0)
{
    return core::TotalF64(value: left) < core::TotalF64(value: right)
} precedes
```

## Diagnostics

| Code | Class | Rule |
| --- | --- | --- |
| `num.conversion-undefined` | error | [Conversions](#conversions) |
| `num.conversion-value` | error | [Conversions](#conversions) |
| `num.from-bits-not-anybits` | error | [Bit patterns](#bit-patterns) |
| `num.same-type-conversion` | error | [Conversions](#conversions), [Bit patterns](#bit-patterns) |
| `num.signed-division` | error | [Division](#division), [Number operators](#number-operators) |
| `num.signed-shift-right` | error | [Shifts](#shifts), [Number operators](#number-operators) |
| `num.unsigned-negation` | error | [Overflow](#overflow), [Number operators](#number-operators) |
| `num.widen-applies` | error | [Conversions](#conversions) |
| `num.widen-lossy` | error | [Conversions](#conversions) |
