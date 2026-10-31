# Lexical structure

This chapter defines source files, the code points they reject, indentation and whitespace, physical and logical lines, the virtual tokens, line-local lexing, the token classes and their lexical grammar, comments, identifiers, the keyword table, number literals, text literals, and punctuation and operator tokens.

## Source files

A **source file** is a file of a [package](03-packages-modules.md#packages) whose name ends in `.nan`, holding UTF-8 text with LF line ends and a final newline.

- Lines end with U+000A alone, and the byte 0x0A always ends a line, so the lexer splits a file into lines before decoding them. U+000D anywhere is an error (`lex.carriage-return`); the exact fix deletes it.
- A byte sequence that is not well-formed UTF-8, including overlong forms, encoded surrogates and values above U+10FFFF, is an error (`lex.invalid-utf8`).
- The last byte of a source file is U+000A. A file ending otherwise, or holding no bytes, is an error (`lex.missing-final-newline`), reported on its last line, line 1 for an empty file. The exact fix appends U+000A.

```nan
// file: meter/package.nan
//! Sample counting.

package meter {
    language 1
    targets any
} meter

// file: meter/meter.nan
//! Counts samples up to a fixed capacity.

pub const MAX_SAMPLES: u32 = 1_000
```

## Rejected code points

The code points of `REJECTED` never appear in a source file: not in code, comments, doc comments or literals.

```ebnf
REJECTED = REJECTED_CONTROL | REJECTED_SPACE | BIDI_CONTROL | INVISIBLE | TAG ;
REJECTED_CONTROL = U+0000 .. U+0009 | U+000B .. U+001F | U+007F .. U+009F ;
REJECTED_SPACE = U+00A0 | U+1680 | U+2000 .. U+200A | U+2028 | U+2029 | U+202F | U+205F | U+3000 ;
BIDI_CONTROL = U+061C | U+200E | U+200F | U+202A .. U+202E | U+2066 .. U+2069 ;
INVISIBLE = U+00AD | U+034F | U+115F | U+1160 | U+17B4 | U+17B5 | U+180B .. U+180F | U+200B .. U+200D | U+2060 .. U+2065
    | U+206A .. U+206F | U+3164 | U+FE00 .. U+FE0F | U+FEFF | U+FFA0 | U+FFF0 .. U+FFFB | U+1BCA0 .. U+1BCA3 | U+1D173 .. U+1D17A ;
TAG = U+E0000 .. U+E0FFF ;
```

- `REJECTED_CONTROL` holds the controls other than LF; `REJECTED_SPACE` the spaces other than U+0020, NBSP included; `BIDI_CONTROL` the bidirectional controls; `INVISIBLE` the zero-width and other invisible code points; `TAG` the Tags block, the supplementary variation selectors and the unassigned code points around them.
- U+0009 is an error with its own code ([Indentation and whitespace](#indentation-and-whitespace)), and so is U+000D ([Source files](#source-files)). Every other rejected code point is an error (`lex.rejected-code-point`) wherever it appears, U+FEFF at the start of a file included. `lex.non-ascii` and `lex.non-ascii-byte` apply only to characters that are not rejected.
- The exact fix of `lex.rejected-code-point` depends on where the code point stands. In a `TEXT`, `CHAR` or `TEXT_LINE`, it writes the code point as a `\u{H}` escape, `H` in uppercase hexadecimal digits without leading zeros. In a `BYTES` or `BYTE`, it writes a code point below U+0080 as a `\xHH` escape in uppercase digits, and no fix exists for another code point. Elsewhere, it replaces a `REJECTED_SPACE` code point with U+0020 and deletes any other.
- An escape may denote a rejected code point ([Text literals](#text-literals)).

```nan
pub const WORD_JOINER: Str = "\u{2060}"

pub const GREETING: Str = "Grüße aus Zürich"
```

## Indentation and whitespace

The **indentation** of a physical line is the run of U+0020 at its start.

- An indentation whose width is not a multiple of 4, on a line holding a character other than U+0020, is an error (`lex.indentation`).
- Indentation has no meaning except whether a token stands at [column 0](#lines). How many spaces a line takes is fixed only by the [canonical format](35-format.md#depth).
- U+0009 anywhere is an error (`lex.tab`). The exact fix replaces each U+0009 of the line: one before the line's first other character with 4 spaces, one inside a `TEXT`, `BYTES`, `CHAR`, `BYTE` or `TEXT_LINE` with `\t`, and any other with one space.
- A physical line ending with U+0020, a line holding only spaces included, is an error (`lex.trailing-whitespace`), so a blank line is empty. The exact fix deletes the trailing spaces; in a `TEXT_LINE`, it replaces the last of them with `\x20` instead.
- After the indentation, spaces separate tokens and carry no other meaning.

```nan
pub fn double(value: u32) -> u32 {
    return value * 2
} double
```

```nan
pub fn double(value: u32) -> u32 {
  return value * 2 // error: lex.indentation
} double
```

## Lines

A **physical line** is one LF-terminated line of a source file; physical lines are numbered from 1. **Column 0** is the position of the first character of a physical line. A **logical line** is a physical line holding a token, joined with the following physical lines holding tokens while the last line joined continues.

A **continuing token** is a `PUNCT` token other than the closing delimiters `)`, `]` and `}`, one of the `FOREIGN` tokens `&&` and `||`, or one of the hard keywords `and`, `or` and `not` ([Punctuation and operators](#punctuation-and-operators)). A physical line continues in exactly two cases:

- Its last token is a continuing token. The logical line resumes at the next physical line holding a token; lines holding no token in between, blank and comment-only lines among them, are skipped.
- Its last token is a `TEXT_LINE` and the next physical line starts with `\\` after its indentation.

A plain comment after the last token does not affect continuation.

```nan
pub fn is_heading_mark(byte: u8) -> bool {
    return byte == b'#' or byte == b'=' or
        byte == b'-' or byte == b'*'
} is_heading_mark

pub fn clamp_to(value: u32, low: u32, high: u32) -> u32 {
    if value < low {
        return low
    }
    if value > high {
        return high
    }
    return value
} clamp_to

pub fn clamp_percent(value: u32) -> u32 {
    return clamp_to(
        value,
        low: 0,
        high: 100,
    )
} clamp_percent
```

## Virtual tokens

The lexer adds three tokens that hold no characters:

- `NL` follows the last token of each logical line. A physical line holding no token, such as a blank or comment-only line, produces no `NL`.
- `COL0` precedes each token whose first character is at column 0, also when that token continues a logical line. [Items](02-items.md#item-shape) define where `COL0` is legal.
- `EOF` is the last token of every file. When the last logical line ends in a continuing token, `NL` still follows it, before `EOF`.

```nan
/// Limits a sensor reading to the calibrated range.
pub fn clamp_reading(raw: u32) -> u32
    ensures result <= 1_000
{
    if raw > 1_000 {
        return 1_000
    }
    return raw
} clamp_reading
```

After its implicit file summary, the file of this block lexes to these tokens, one logical line per row:

```text
COL0 DOC NL
COL0 pub fn IDENT ( IDENT : IDENT ) -> IDENT NL
ensures IDENT <= INT NL
COL0 { if IDENT > INT { return INT NL
} NL
return IDENT NL
COL0 } IDENT NL
EOF
```

## Line-local lexing

Every token lies within one physical line: no comment or literal spans lines, and each line of a text run is its own token. The non-virtual tokens, the `COL0` tokens and the lexical diagnostics of a physical line depend only on that line's characters. Whether `NL` follows a line's last token depends only on that line, on the next physical line when that token is a `TEXT_LINE`, and on whether a later line holds a token. The lexer must produce the same non-virtual tokens, `COL0` tokens and lexical diagnostics for a physical line lexed alone as for that line lexed within its file.

The lexer reports at most one diagnostic per physical line, at its first offending character, and the line yields only the tokens that end before that character. The offending character of a diagnostic is:

- for `lex.indentation`, the first character of the line;
- for `lex.trailing-whitespace`, the first of the trailing spaces;
- for `lex.missing-final-newline`, the end of the last line, after its last character;
- for `lex.invalid-utf8`, the first byte of the ill-formed sequence;
- for `lex.unterminated-literal` and `lex.char-literal-size`, the literal's opening quote, or its `b`;
- for `lex.unknown-escape` and `lex.escape-out-of-range`, the escape's backslash;
- for `lex.malformed-number` and `lex.literal-suffix`, the first digit of the run;
- for `lex.not-at-line-start`, the first character of the `TEXT_LINE`, `DOC` or `FILE_DOC`;
- for every other code, the character it names.

The parser treats the position after the tokens such a line yields as a [syntax failure](02-items.md#resynchronisation) reported by the lexical diagnostic alone: it abandons the item it is reading and resynchronises from the next token. No other diagnostic is reported on that line.

```nan
pub const URL_PREFIX: Str = "https://" // the scheme ends at the second slash
```

## Tokens

A **token** is the smallest lexical unit; every token lies within one physical line.

```ebnf
SOURCE = LINE+ ;
LINE = ( INDENT LINE_BODY )? LF ;
INDENT = "    "* ;
LINE_BODY = TEXT_LINE | DOC | FILE_DOC | COMMENT | CODE ;
CODE = TOKEN ( SPACE* TOKEN )* ( SPACE* COMMENT )? ;
TOKEN = WORD | NUMBER_RUN | TEXT | BYTES | CHAR | BYTE | PUNCT | FOREIGN ;
SPACE = " " ;
LF = U+000A ;
CHARACTER = ( U+0020 .. U+D7FF | U+E000 .. U+10FFFF ) - REJECTED ;
LETTER = "a" .. "z" | "A" .. "Z" ;
DIGIT = "0" .. "9" ;
```

- The token classes are `IDENT` and the hard keywords, both from `WORD` ([Identifiers](#identifiers)); `INT` and `FLOAT`, from `NUMBER_RUN` ([Number literals](#number-literals)); `TEXT`, `BYTES`, `CHAR`, `BYTE` and `TEXT_LINE` ([Text literals](#text-literals)); `DOC` and `FILE_DOC` ([Comments](#comments)); one token per spelling of `PUNCT` and `FOREIGN` ([Punctuation and operators](#punctuation-and-operators)); and the virtual tokens. `COMMENT` produces no token. Every other lexical rule is a fragment.
- At each position after the indentation and any spaces, the characters there decide what starts, by the first row of this table that applies:

| Characters | Start |
| --- | --- |
| `//` | a `COMMENT`, `DOC` or `FILE_DOC` ([Comments](#comments)) |
| `\\` | a `TEXT_LINE` |
| `"`, `b"`, `'`, `b'` | a `TEXT`, `BYTES`, `CHAR` or `BYTE` |
| a letter or `_` | a `WORD` |
| a digit | a `NUMBER_RUN` |
| any other character | the longest `PUNCT` or `FOREIGN` spelling the text begins with |

- A `WORD` or `NUMBER_RUN` extends as far as its rule matches. A comment, `DOC`, `FILE_DOC` or `TEXT_LINE` extends to the end of its line, and a literal to its closing quote ([Text literals](#text-literals)).
- A `TEXT_LINE`, `DOC` or `FILE_DOC` is always the first token of its physical line; one following another token is an error (`lex.not-at-line-start`). For a `DOC` or `FILE_DOC`, the exact fix replaces its opener with `//`.
- A character that starts no token is an error: a non-ASCII character ([Identifiers](#identifiers)) is `lex.non-ascii`, and `@`, `#`, `$`, `` ` `` or a `\` not followed by `\` is `lex.unexpected-character`.

```nan
pub fn low_mask(width: u32) -> u64 {
    var mask: u64 = 0
    for _ in 0..<width {
        mask <<= 1
        mask |= 1
    }
    return mask
} low_mask
```

```nan
pub const RETRY_LIMIT: u8 = #7 // error: lex.unexpected-character
```

## Comments

```ebnf
COMMENT = "//" ( ( CHARACTER - ( "/" | "!" ) ) CHARACTER* )? ;
DOC = "///" CHARACTER* ;
FILE_DOC = "//!" CHARACTER* ;
```

- `//` not followed by `/` or `!` starts a plain comment, which runs to the end of the line and produces no token. It may follow tokens on its line.
- `///` starts a `DOC` token, a [doc comment](32-docs.md#doc-comments) line; `//!` starts a `FILE_DOC` token, a [file summary](32-docs.md#file-summaries) line. Each token's text is the characters after its 3-character opener.
- Source has no block comments.

```nan
/// Retries before a link counts as down.
pub const MAX_RETRIES: u32 = 3 // one dropped frame needs at most three
```

```nan
pub const MAX_RETRIES: u32 = 3 /// retries before a link counts as down // error: lex.not-at-line-start
```

## Identifiers

```ebnf
WORD = ( LETTER | "_" ) ( LETTER | DIGIT | "_" )* ;
IDENT = WORD - HARD_KEYWORD ;
```

- A `WORD` spelt as a [hard keyword](#keywords) is that keyword's token; any other `WORD` is an `IDENT` token. Context words and reserved words are `IDENT` tokens.
- Identifiers and keywords are ASCII. Spellings compare exactly, case and underscores included. Which identifiers a declaration may use is fixed by the [naming conventions](03-packages-modules.md#naming-conventions).
- A character above U+007F outside a `TEXT`, `BYTES`, `CHAR`, `BYTE`, `TEXT_LINE`, comment, `DOC` or `FILE_DOC` is an error (`lex.non-ascii`).

```nan
pub const MAX_FLOORS: u32 = 12
```

```nan
pub const MAX_ÉTAGES: u32 = 12 // error: lex.non-ascii
```

## Keywords

A **hard keyword** is a word that is never an identifier. A **context word** is an identifier with a fixed meaning at the grammar positions naming it, and an ordinary identifier elsewhere. A **reserved word** is a word without meaning in language 1, rejected at the positions reserved for it and an ordinary identifier elsewhere.

The keyword table of a [language version](37-evolution.md#language-versions) is generated from the grammar that [Grammar](41-grammar.md) publishes for that version, written in the [grammar notation](00-charter.md#grammar-notation). Among the syntactic rules that the start symbol `source_file` and the [classification rules](41-grammar.md#start-symbols) reach:

- the hard keywords are exactly the double-quoted terminals matching `WORD`;
- the context words are exactly the single-quoted terminals that these rules reach without passing through a rejected alternative;
- the reserved words are exactly the other single-quoted terminals, which these rules reach only through rejected alternatives.

No word belongs to two classes. For language 1, the hard keywords are exactly the words of `HARD_KEYWORD`:

```ebnf
HARD_KEYWORD = "_" | "and" | "assert" | "assume" | "attempt" | "await" | "break" | "const" | "continue" | "contribute"
    | "dbg" | "decreases" | "defer" | "derives" | "discard" | "else" | "ensures" | "enum" | "expect" | "fail" | "fails"
    | "false" | "first" | "fn" | "for" | "foreign" | "if" | "impl" | "in" | "interface" | "interrupt" | "invariant"
    | "landing" | "layout" | "let" | "limits" | "linear" | "loop" | "match" | "mut" | "not" | "or" | "package" | "pkg"
    | "proj" | "pub" | "registers" | "registry" | "requires" | "retired" | "return" | "self" | "struct" | "take" | "test"
    | "todo" | "true" | "trusted" | "try" | "undefer" | "uses" | "validator" | "var" | "waive" | "when" | "where" | "while" ;
```

The context words of language 1 are exactly these:

| Context words | Meaningful in |
| --- | --- |
| `result`, `old` | [ensures](18-contracts.md#ensures) clauses |
| `all`, `any` | [quantifiers](18-contracts.md#quantifiers) |
| `because` | [assume](18-contracts.md#assume) statements |
| `example` | [example](18-contracts.md#examples) clauses |
| `depth`, `length` | [waivers](06-statements-expressions.md#waivers) |
| `as` | `loop as label` ([conditionals and loops](06-statements-expressions.md#conditionals-and-loops)) |
| `from` | [error enum](11-errors-domains.md#error-enums) variants |
| `closed`, `open`, `flexible`, `view` | type [modifiers](02-items.md#modifiers) |
| `migrate` | function [modifiers](02-items.md#modifiers) |
| `wait` | `uses wait` ([waiting](10-effects-digests.md#waiting)) |
| `task` | `task fn` [function types](10-effects-digests.md#function-types) |
| `pure`, `notrap`, `terminates`, `noalloc`, `stack`, `traceless`, `recorded`, `restorable`, `irreversible`, `emit`, `actuate`, `erase`, `spend` | [limits](10-effects-digests.md#limits) clauses |
| `undo` | [undo](10-effects-digests.md#compensation) clauses |
| `convention` | [calling conventions](23-low-level.md#calling-conventions) |
| `bit`, `rest`, `bytes`, `le`, `be`, `tags`, `at` | [layouts](23-low-level.md#layouts) |
| `at`, `stride`, `read`, `write`, `write_one_clear` | [registers](23-low-level.md#registers) items |
| `keyed`, `by`, `unique` | [registries](25-registries.md#registries) |
| `protocol`, `summary` | [protocol](26-components.md#protocols) items |
| `preserves`, `changes`, `manual` | [tombstones](29-identity-versioning.md#tombstones) |
| `language`, `targets`, `entry`, `dep`, `allow`, `deny`, `device`, `priv`, `validate` | [manifest](03-packages-modules.md#manifest) directives |

The reserved words of language 1 are exactly `stable`, `machine` and `ipc`, reserved in modifier or kind position of an item head ([reserved forms](02-items.md#reserved-forms)), and `bank`, reserved at the start of a member line of a [registers](23-low-level.md#registers) item. The [reservations](37-evolution.md#reservations) register lists them with every other reservation.

```nan
pub closed struct Span {
    pub at: usize
    pub length: usize
} Span

pub fn Span.is_empty(self) -> bool = self.length == 0

pub fn Span.end(self) -> Option[usize] {
    let result = self.at.checked_add(self.length)
    return result
} Span.end
```

## Number literals

```ebnf
NUMBER_RUN = DIGIT ( LETTER | DIGIT | "_" | "." DIGIT )* ;
INT = ( DEC_INT | "0x" HEX_DIGITS | "0b" BIN_DIGITS ) INT_SUFFIX? ;
FLOAT = DEC_INT "." DEC_DIGITS FLOAT_SUFFIX? ;
DEC_INT = "0" | "1" .. "9" ( "_"? DIGIT )* ;
DEC_DIGITS = DIGIT ( "_"? DIGIT )* ;
HEX_DIGITS = HEX_DIGIT ( "_"? HEX_DIGIT )* ;
BIN_DIGITS = BIN_DIGIT ( "_"? BIN_DIGIT )* ;
HEX_DIGIT = DIGIT | "a" .. "f" | "A" .. "F" ;
BIN_DIGIT = "0" | "1" ;
INT_SUFFIX = "u8" | "u16" | "u32" | "u64" | "usize" | "i8" | "i16" | "i32" | "i64" | "isize" ;
FLOAT_SUFFIX = "f32" | "f64" ;
```

- A run matching `INT` is an `INT` token, and a run matching `FLOAT` a `FLOAT` token. Hexadecimal digits include `a` to `f`, so `0x1f32` is an `INT` without suffix.
- A run of the form `( DEC_INT | "0b" BIN_DIGITS ) FLOAT_SUFFIX` or `DEC_INT "." DEC_DIGITS INT_SUFFIX` is an error (`lex.literal-suffix`), as in `2f64` or `1.5u8`. When the digits before `f32` or `f64` form a `DEC_INT`, the exact fix inserts `.0` before the suffix.
- Every other run is an error (`lex.malformed-number`): a leading zero (`0755`), an uppercase or unknown prefix (`0X1F`, `0o17`), an unknown suffix (`255u7`), an exponent (`1e3`), an underscore not between two digits, a prefix without digits, or a second `.`.
- An `INT` denotes a non-negative integer, written in decimal, in hexadecimal after `0x` with digits of either case, or in binary after `0b`. A `FLOAT` denotes the decimal fraction it writes, with at least one digit on each side of the `.`.
- No number token has a sign: `-` is a separate token ([operators](06-statements-expressions.md#operators)).
- `_` separates digits and carries no meaning; its canonical placement is fixed by [number grouping](35-format.md#number-grouping).
- A **literal suffix** is a scalar type name ending a number literal and fixing its type: an [integer scalar](04-types.md#scalars) name on an `INT`, `f32` or `f64` on a `FLOAT`. [Expected types and literals](04-types.md#expected-types-and-literals) fixes the type and value of every number literal.

```nan
pub const PAGE_SIZE: usize = 4_096

pub const STATUS_MASK: u8 = 0b1000_0001

pub const MMIO_BASE: u64 = 0xFEB0_0000

pub const HALF: f64 = 0.5

pub fn widest_byte() -> u64 {
    let byte = 255u8
    return u64.widen(byte)
} widest_byte
```

```nan
pub const FILE_MODE: u32 = 0755 // error: lex.malformed-number
```

```nan
pub const SCALE: f64 = 2f64 // error: lex.literal-suffix
```

## Text literals

A `TEXT` is a **text literal**, a `BYTES` a **byte-string literal**, a `CHAR` a **char literal** and a `BYTE` a **byte literal**; a `TEXT_LINE` is one line of a text run.

```ebnf
TEXT = U+0022 ( TEXT_CHAR | ESCAPE | UNICODE_ESCAPE )* U+0022 ;
BYTES = "b" U+0022 ( BYTES_CHAR | ESCAPE )* U+0022 ;
CHAR = U+0027 ( CHAR_CHAR | ESCAPE | UNICODE_ESCAPE ) U+0027 ;
BYTE = "b" U+0027 ( BYTE_CHAR | ESCAPE ) U+0027 ;
TEXT_LINE = U+005C U+005C ( RUN_CHAR | ESCAPE | UNICODE_ESCAPE )* ;
ESCAPE = U+005C ( U+005C | U+0022 | U+0027 | "n" | "r" | "t" | "0" | "x" HEX_DIGIT HEX_DIGIT ) ;
UNICODE_ESCAPE = U+005C "u{" HEX_DIGIT ( HEX_DIGIT ( HEX_DIGIT ( HEX_DIGIT ( HEX_DIGIT HEX_DIGIT? )? )? )? )? "}" ;
TEXT_CHAR = CHARACTER - ( U+0022 | U+005C ) ;
CHAR_CHAR = CHARACTER - ( U+0027 | U+005C ) ;
RUN_CHAR = CHARACTER - U+005C ;
BYTES_CHAR = U+0020 .. U+007E - ( U+0022 | U+005C ) ;
BYTE_CHAR = U+0020 .. U+007E - ( U+0027 | U+005C ) ;
```

- A literal opened by `"`, `b"`, `'` or `b'` extends to the next quote of its opener's kind on the same line; scanning from the opener, a `\` and the character after it never end the literal. A literal with no such quote is an error (`lex.unterminated-literal`).
- A `CHAR` or `BYTE` holding anything other than exactly one character or one escape is an error (`lex.char-literal-size`), unless a `\` in it is `lex.unknown-escape` or `lex.escape-out-of-range`; that code is then the literal's diagnostic.
- A non-ASCII character in a `BYTES` or `BYTE` is an error (`lex.non-ascii-byte`).
- The escapes denote: `\\` U+005C, `\"` U+0022, `\'` U+0027, `\n` U+000A, `\r` U+000D, `\t` U+0009, `\0` U+0000, `\xHH` the value `HH` in hexadecimal digits of either case, and `\u{H}` the Unicode scalar value `H` of 1 to 6 hexadecimal digits of either case.
- `\xHH` above `7F` in a `TEXT`, `CHAR` or `TEXT_LINE`, whose exact fix writes `\u{HH}` with uppercase digits, and `\u{H}` naming a surrogate or a value above `10FFFF`, are errors (`lex.escape-out-of-range`).
- `\u` in a `BYTES` or `BYTE` is an error (`lex.unknown-escape`); when it begins a `UNICODE_ESCAPE` naming a value below `80`, the exact fix writes that value as `\xHH` with uppercase digits. Every other backslash sequence that the literal's rule does not accept is also an error (`lex.unknown-escape`), `\x` without 2 hexadecimal digits and `\u` without 1 to 6 hexadecimal digits between braces included.
- The value of a `TEXT` is the UTF-8 encoding of its characters, of a `CHAR` its one character, of a `BYTES` its bytes and of a `BYTE` its one byte, each escape standing for what it denotes. `""` and `b""` are empty. A `TEXT`, `BYTES` or text run has static storage, so its value is a [view](05-properties.md#views) valid for the whole program. [Expected types and literals](04-types.md#expected-types-and-literals) gives each literal's type.

A **text run** is a maximal sequence of physical lines, consecutive in the file, each starting with `\\` after its indentation.

- A `TEXT_LINE` runs from `\\` to the end of its line. Its content is the characters after `\\`, with escapes replaced.
- A text run is one literal whose value is the contents of its lines joined by U+000A, with no U+000A after the last line. An empty `TEXT_LINE` contributes an empty line.
- The lines of a run belong to one logical line, which the run ends ([Lines](#lines)).

```nan
pub const GREETING: Str = "caf\u{E9} says \"hi\"\n"

pub const MAGIC: Slice[u8] = b"NAN\x00"

pub const SEPARATOR: char = ','

pub const NEWLINE: u8 = b'\n'

pub const USAGE: Str =
    \\usage: nan <command>
    \\    check    report errors
```

```nan
pub const DATA_DIR: Str = "C:\data" // error: lex.unknown-escape
```

```nan
pub const ACCENT: Str = "caf\xE9" // error: lex.escape-out-of-range
```

```nan
pub const LATIN: Slice[u8] = b"café" // error: lex.non-ascii-byte
```

```nan
pub const MARK: char = 'ok' // error: lex.char-literal-size
```

```nan
pub const TITLE: Str = "unfinished // error: lex.unterminated-literal
```

## Punctuation and operators

```ebnf
PUNCT = "(" | ")" | "[" | "]" | "{" | "}" | "," | ":" | "::" | "." | "->" | "=>"
    | "=" | "+=" | "-=" | "*=" | "/=" | "%=" | "&=" | "|=" | "^=" | "<<=" | ">>="
    | "+" | "-" | "*" | "/" | "%" | "&" | "|" | "^" | "~" | "<<" | ">>"
    | "==" | "!=" | "<" | "<=" | ">" | ">=" | "..<" | "..=" ;
FOREIGN = "&&" | "||" | "!" | "?" | ";" | ".." | "--" ;
```

- Each spelling of `PUNCT` and `FOREIGN` is one token. The longest spelling decides: `a<<=b` holds `<<=`, and `0..<n` holds `0`, `..<` and `n`.
- `(`, `[` and `{` are open delimiters; `)`, `]` and `}` are closing delimiters. Which tokens are continuing tokens is fixed in [Lines](#lines).
- `FOREIGN` tokens appear in no grammar rule, so each is a [syntax failure](34-diagnostics.md#syntax-failures) (`syntax.foreign`), with the exact fixes of the [idiom table](34-diagnostics.md#idiom-table).
- The meanings of operators and their precedence are fixed by [Statements and expressions](06-statements-expressions.md#operators); `::` separates [path](03-packages-modules.md#paths) segments and `.` reaches members.

```nan
pub fn in_window(value: u32, low: u32, high: u32) -> bool {
    return value >= low and value <= high
} in_window
```

```nan
pub fn in_window(value: u32, low: u32, high: u32) -> bool {
    return value >= low && value <= high // error: syntax.foreign
} in_window
```

## Diagnostics

| Code | Class | Rule |
| --- | --- | --- |
| `lex.carriage-return` | error | [Source files](#source-files) |
| `lex.char-literal-size` | error | [Text literals](#text-literals) |
| `lex.escape-out-of-range` | error | [Text literals](#text-literals) |
| `lex.indentation` | error | [Indentation and whitespace](#indentation-and-whitespace) |
| `lex.invalid-utf8` | error | [Source files](#source-files) |
| `lex.literal-suffix` | error | [Number literals](#number-literals) |
| `lex.malformed-number` | error | [Number literals](#number-literals) |
| `lex.missing-final-newline` | error | [Source files](#source-files) |
| `lex.non-ascii` | error | [Identifiers](#identifiers), [Tokens](#tokens) |
| `lex.non-ascii-byte` | error | [Text literals](#text-literals) |
| `lex.not-at-line-start` | error | [Tokens](#tokens) |
| `lex.rejected-code-point` | error | [Rejected code points](#rejected-code-points) |
| `lex.tab` | error | [Indentation and whitespace](#indentation-and-whitespace) |
| `lex.trailing-whitespace` | error | [Indentation and whitespace](#indentation-and-whitespace) |
| `lex.unexpected-character` | error | [Tokens](#tokens) |
| `lex.unknown-escape` | error | [Text literals](#text-literals) |
| `lex.unterminated-literal` | error | [Text literals](#text-literals) |
