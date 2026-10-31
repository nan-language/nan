# Grammar

This chapter publishes the grammar of language 1: its lexical and syntactic rules, start symbols, line structure, keyword table, lookahead resolutions, prefix viability and place in a language version.

## Lexical grammar

These rules of [Lexical structure](01-lexical.md) form the tokens of source files and machine files; `SOURCE` derives a file's text.

```ebnf
REJECTED = REJECTED_CONTROL | REJECTED_SPACE | BIDI_CONTROL | INVISIBLE | TAG ;
REJECTED_CONTROL = U+0000 .. U+0009 | U+000B .. U+001F | U+007F .. U+009F ;
REJECTED_SPACE = U+00A0 | U+1680 | U+2000 .. U+200A | U+2028 | U+2029 | U+202F | U+205F | U+3000 ;
BIDI_CONTROL = U+061C | U+200E | U+200F | U+202A .. U+202E | U+2066 .. U+2069 ;
INVISIBLE = U+00AD | U+034F | U+115F | U+1160 | U+17B4 | U+17B5 | U+180B .. U+180F | U+200B .. U+200D | U+2060 .. U+2065
    | U+206A .. U+206F | U+3164 | U+FE00 .. U+FE0F | U+FEFF | U+FFA0 | U+FFF0 .. U+FFFB | U+1BCA0 .. U+1BCA3 | U+1D173 .. U+1D17A ;
TAG = U+E0000 .. U+E0FFF ;
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
COMMENT = "//" ( ( CHARACTER - ( "/" | "!" ) ) CHARACTER* )? ;
DOC = "///" CHARACTER* ;
FILE_DOC = "//!" CHARACTER* ;
WORD = ( LETTER | "_" ) ( LETTER | DIGIT | "_" )* ;
IDENT = WORD - HARD_KEYWORD ;
HARD_KEYWORD = "_" | "and" | "assert" | "assume" | "attempt" | "await" | "break" | "const" | "continue" | "contribute"
    | "dbg" | "decreases" | "defer" | "derives" | "discard" | "else" | "ensures" | "enum" | "expect" | "fail" | "fails"
    | "false" | "first" | "fn" | "for" | "foreign" | "if" | "impl" | "in" | "interface" | "interrupt" | "invariant"
    | "landing" | "layout" | "let" | "limits" | "linear" | "loop" | "match" | "mut" | "not" | "or" | "package" | "pkg"
    | "proj" | "pub" | "registers" | "registry" | "requires" | "retired" | "return" | "self" | "struct" | "take" | "test"
    | "todo" | "true" | "trusted" | "try" | "undefer" | "uses" | "validator" | "var" | "waive" | "when" | "where" | "while" ;
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
PUNCT = "(" | ")" | "[" | "]" | "{" | "}" | "," | ":" | "::" | "." | "->" | "=>"
    | "=" | "+=" | "-=" | "*=" | "/=" | "%=" | "&=" | "|=" | "^=" | "<<=" | ">>="
    | "+" | "-" | "*" | "/" | "%" | "&" | "|" | "^" | "~" | "<<" | ">>"
    | "==" | "!=" | "<" | "<=" | ">" | ">=" | "..<" | "..=" ;
FOREIGN = "&&" | "||" | "!" | "?" | ";" | ".." | "--" ;
```

## Syntactic grammar

Each subsection holds the rules of the chapter it links, in that chapter's order, so a nonterminal's subsection names its owner.

### Items

From [Items](02-items.md).

```ebnf
item_label = IDENT ( "." IDENT | "::" IDENT ( "::" IDENT )? "." IDENT )? ;
source_file = ( COL0 ( FILE_DOC NL | item_start ) item* )? EOF ;
item = COL0 item_start ;
item_start = ( DOC NL COL0 )* visibility? item_modifier* item_kind ;
item_modifier = type_modifier | fn_modifier | reserved_head_word ;
item_kind = fn_item
    | struct_item
    | enum_item
    | interface_item
    | impl_item
    | layout_item
    | registers_item
    | "const" ( const_item | const_assert_item )
    | registry_item
    | contribute_item
    | protocol_item
    | test_item
    | retired_item
    | package_item ;
type_modifier = 'closed' | 'open' | 'flexible' | "linear" | 'view' ;
fn_modifier = "proj" | "interrupt" | "landing" | "foreign" | "validator" | 'migrate' ;
fn_item = "fn" fn_head fn_end ;
fn_head = item_label generic_params? "(" param_list? ")" fn_result? fails_clause? uses_clause? limits_clause? ;
fn_result = "->" type_expr ;
param_list = param ( "," param_list? )? ;
param = mode? ( IDENT ":" type_expr | "self" self_field_set? ) ;
fn_end = convention_clause NL
    | "=" expr NL
    | "{" fn_body
    | NL clause_region COL0 "{" fn_body ;
fn_body = stmt* COL0 "}" item_label NL ;
clause_region = clause_line+ ;
clause_line = requires_clause
    | ensures_clause
    | example_clause
    | undo_clause
    | decreases_clause
    | todo_clause
    | waive_clause ;
const_item = IDENT ( ":" type_expr "=" expr NL | "=" expr NL {!item.const-untyped} ) ;
const_assert_item = "assert" expr NL ;
self_field_set = "." "{" IDENT ( "," IDENT )* "}" {!item.self-field-set} ;
reserved_head_word = ( 'stable' | 'machine' | 'ipc' ) {!item.reserved-word} ;
```

### Packages and names

From [Packages, modules and names](03-packages-modules.md).

```ebnf
package_item = "package" IDENT "{" directive* COL0 "}" item_label NL ;
directive = language_directive | targets_directive | entry_directive
    | dep_directive | allow_directive | deny_directive ;
language_directive = 'language' INT NL ;
targets_directive = 'targets' target_list NL ;
target_list = target ( "," target )* ;
target = IDENT ( "-" IDENT )? ;
entry_directive = 'entry' IDENT NL ;
dep_directive = 'dep' IDENT TEXT NL ;
allow_directive = 'allow' authority "in" name_list NL ;
authority = 'device' | 'priv' | "trusted" | 'validate' | 'capability' | "contribute" path ;
name_list = IDENT ( "," IDENT )* ;
deny_directive = 'deny' IDENT "->" IDENT NL ;
path = IDENT ( "::" IDENT )* ;
visibility = "pub" | "pkg" ;
```

### Data types

From [Data types](04-types.md).

```ebnf
struct_item = "struct" IDENT generic_params? derives_clause? "{" struct_member* COL0 "}" item_label NL ;
struct_member = ( DOC NL )* ( field | invariant_line ) ;
field = visibility? IDENT ":" type_expr where_clause? NL ;
type_expr = fn_type | path ( generic_args | "." IDENT generic_args? {!name.dot-for-path} )? ;
enum_item = "enum" IDENT generic_params? derives_clause? "{" enum_member+ COL0 "}" item_label NL ;
enum_member = ( DOC NL )* variant ;
variant = IDENT variant_payload? 'from'? NL ;
variant_payload = "(" payload_list ")" ;
payload_list = payload_field ( "," payload_list? )? ;
payload_field = type_expr ( ":" type_expr )? ;
construction_expr = path generic_args? ( "." IDENT )? "(" arg_list? ")" ;
where_clause = "where" expr ;
invariant_line = "invariant" expr NL ;
pattern = "_" | literal_pattern | named_pattern ;
literal_pattern = pattern_literal range_tail? ;
pattern_literal = "-"? ( INT | FLOAT ) | CHAR | BYTE | TEXT | BYTES | "true" | "false" ;
range_tail = ( "..<" | "..=" ) range_bound ;
range_bound = pattern_literal | path ;
named_pattern = path ( "." IDENT )? ( "(" field_pattern_list? ")" | range_tail )? ;
field_pattern_list = field_pattern ( "," field_pattern_list? )? ;
field_pattern = pattern ( ":" pattern )? ;
```

### Statements and expressions

From [Statements and expressions](06-statements-expressions.md).

```ebnf
block = "{" stmt* "}" ;
loop_block = "{" loop_invariant* stmt* "}" ;
stmt = let_stmt | var_stmt | discard_stmt | while_stmt | for_stmt | loop_stmt
    | defer_stmt | undefer_stmt | expect_stmt | assume_stmt | expr_stmt ;
let_stmt = "let" pattern ( ":" type_expr )? "=" expr ( "else" block )? NL ;
var_stmt = "var" IDENT ( ":" type_expr )? "=" expr NL ;
expr_stmt = expr ( assign_operator expr )? NL ;
assign_operator = "=" | "+=" | "-=" | "*=" | "/=" | "%=" | "&=" | "|=" | "^=" | "<<=" | ">>=" ;
discard_stmt = "discard" expr NL ;
if_expr = "if" expr block ( "else" ( if_expr | block ) )? ;
while_stmt = "while" expr loop_block NL ;
for_stmt = "for" pattern "in" expr loop_block NL ;
loop_stmt = "loop" ( 'as' IDENT )? loop_block NL ;
jump_expr = "return" expr? | "break" IDENT? | "continue" IDENT? ;
match_expr = "match" "first"? expr "{" match_arm+ "}" ;
match_arm = ( pattern ( "or" pattern )* ( "if" expr )? | "else" ) "=>" arm_body NL ;
arm_body = block | expr ;
expr = jump_expr | fail_expr | quantifier_expr | operator_expr ;
operator_expr = negation logic_tail?
    | prefix_expr operator_tail? ;
negation = "not" operand ( value_operator {!expr.parentheses} )? ;
operator_tail = relation_tail logic_tail?
    | logic_tail
    | bitwise_tail
    | shift_operator operand ( binary_operator {!expr.parentheses} )?
    | range_operator operand ( binary_operator {!expr.parentheses} )? ;
relation_tail = arithmetic_tail ( compare_tail | mixed_operator )?
    | compare_tail ;
arithmetic_tail = ( multiplicative_operator operand )+ ( additive_operator term )*
    | ( additive_operator term )+ ;
term = operand ( multiplicative_operator operand )* ;
compare_tail = compare_operator comparand ( compare_operator {!expr.chained-comparison} | mixed_operator )? ;
comparand = operand arithmetic_tail? ;
logic_tail = ( "and" logic_operand )+ ( "or" {!expr.parentheses} )?
    | ( "or" logic_operand )+ ( "and" {!expr.parentheses} )? ;
logic_operand = negation | prefix_expr ( relation_tail | mixed_operator )? ;
bitwise_tail = ( "&" operand )+ ( ( "|" | "^" | other_operator ) {!expr.parentheses} )?
    | ( "|" operand )+ ( ( "&" | "^" | other_operator ) {!expr.parentheses} )?
    | ( "^" operand )+ ( ( "&" | "|" | other_operator ) {!expr.parentheses} )? ;
mixed_operator = ( bitwise_operator | shift_operator | range_operator ) {!expr.parentheses} ;
operand = prefix_expr | "not" {!expr.parentheses} ;
prefix_expr = ( "-" | "~" ) prefix_expr | marked_expr | postfix_expr ;
value_operator = compare_operator | multiplicative_operator | additive_operator | bitwise_operator | shift_operator | range_operator ;
binary_operator = value_operator | "and" | "or" ;
other_operator = compare_operator | multiplicative_operator | additive_operator | shift_operator | range_operator | "and" | "or" ;
compare_operator = "==" | "!=" | "<" | "<=" | ">" | ">=" ;
multiplicative_operator = "*" | "/" | "%" ;
additive_operator = "+" | "-" ;
bitwise_operator = "&" | "|" | "^" ;
shift_operator = "<<" | ">>" ;
range_operator = "..<" | "..=" ;
arg_list = arg ( "," arg_list? )? ;
arg = expr ( ":" expr )? ;
call_expr = marked_expr | postfix_expr ;
marked_expr = failure_marker after_failure_marker
    | "await" after_await
    | mode after_mode ;
failure_marker = "try" | "attempt" ;
after_failure_marker = "await" after_await | mode after_mode | postfix_expr | failure_marker {!expr.marker-order} ;
after_await = when_expr | mode after_mode | postfix_expr | ( failure_marker | "await" ) {!expr.marker-order} ;
after_mode = postfix_expr | marker {!expr.marker-order} ;
marker = failure_marker | "await" | mode ;
postfix_expr = primary_expr postfix_suffix* ;
postfix_suffix = "." IDENT | "(" arg_list? ")" | "[" expr "]" ;
primary_expr = literal | path_expr | "self" | "(" ( operator_expr | quantifier_expr ) ")" | if_expr | match_expr
    | closure_expr | trusted_expr | todo_expr | dbg_expr | quantifier_expr {!expr.parentheses} ;
path_expr = path generic_args? ;
literal = INT | FLOAT | TEXT | BYTES | CHAR | BYTE | TEXT_LINE+ | "true" | "false" ;
waive_clause = "waive" ( 'depth' | 'length' ) TEXT NL ;
```

### Ownership

From [Ownership and access](07-ownership.md).

```ebnf
mode = "mut" | "take" ;
place = place_root place_step* ;
place_root = IDENT | "self" ;
place_step = "." IDENT | "[" expr "]" ;
closure_expr = "fn" capture_list? "(" fn_type_param_list? ")" closure_body ;
closure_body = "=>" expr | fn_result? fails_clause? uses_clause? limits_clause? block ;
capture_list = "[" capture ( "," capture )* "]" ;
capture = mode place_root ;
```

### Generics and interfaces

From [Generics and interfaces](08-generics-interfaces.md).

```ebnf
interface_item = "interface" IDENT generic_params? "{" interface_member* COL0 "}" item_label NL ;
interface_member = ( DOC NL )* "proj"? "fn" IDENT generic_params? "(" param_list? ")" fn_result? fails_clause? uses_clause? limits_clause? method_end ;
method_end = method_body | NL ( clause_region method_body? )? ;
method_body = "{" stmt* "}" IDENT NL ;
impl_item = "impl" path impl_params? ":" bound NL ;
impl_params = "[" impl_param ( "," impl_param )* "]" ;
impl_param = IDENT ( ":" bound ( "and" bound )* )? ;
generic_params = "[" generic_param ( "," generic_param )* "]" ;
generic_param = IDENT ( ":" bound ( "and" bound )* where_clause? )? ;
bound = path generic_args? ;
generic_args = "[" generic_arg ( "," generic_arg )* "]" ;
generic_arg = type_expr | "-"? INT | "true" | "false" ;
derives_clause = "derives" IDENT ( "," IDENT )* ;
```

### Effects and limits

From [Effects, digests and limits](10-effects-digests.md).

```ebnf
uses_clause = "uses" ( 'wait' | IDENT {!effect.uses-unknown} ) ;
limits_clause = "limits" limit ( "," limit )* ;
limit = 'pure' | 'notrap' | 'terminates' | 'noalloc' | 'stack' INT | reversibility ;
reversibility = 'traceless' | 'recorded' | 'restorable' | 'irreversible' "(" irreversible_kind ( "," irreversible_kind )* ")" ;
irreversible_kind = 'emit' | 'actuate' | 'erase' | 'spend' ;
undo_clause = 'undo' path ( "." IDENT )? TEXT NL ;
fn_type = fn_kind "fn" fn_type_head | "fn" fn_type_head convention_clause? ;
fn_kind = 'task' | "interrupt" ;
fn_type_head = "(" fn_type_param_list? ")" fn_result? fails_clause? uses_clause? limits_clause? ;
fn_type_param_list = fn_type_param ( "," fn_type_param_list? )? ;
fn_type_param = mode? IDENT ":" type_expr ;
```

### Failures

From [Errors, traps and domains](11-errors-domains.md).

```ebnf
fails_clause = "fails" type_expr ;
fail_expr = "fail" operator_expr ;
```

### Cowns

From [Structured concurrency](16-concurrency.md).

```ebnf
when_expr = "when" when_access ( "," when_access )* when_body ;
when_access = "mut" when_place | "take" when_place {!task.when-take} | when_place ;
when_place = place_root ( "." IDENT )* ;
when_body = block | "=>" expr ;
```

### Deferred calls

From [Suspension, I/O and cleanup](17-suspension-io.md).

```ebnf
defer_stmt = "defer" call_expr NL ;
undefer_stmt = "undefer" place NL ;
```

### Contracts

From [Contracts](18-contracts.md).

```ebnf
requires_clause = "requires" expr NL ;
ensures_clause = "ensures" expr NL ;
result_expr = 'result' ;
old_expr = 'old' "(" expr ")" ;
example_clause = 'example' expr NL ;
decreases_clause = "decreases" expr NL ;
loop_invariant = "invariant" expr NL ;
quantifier_expr = ( 'all' | 'any' ) IDENT "in" expr ":" expr ;
expect_stmt = "expect" expr NL ;
assume_stmt = "assume" expr 'because' TEXT NL ;
todo_expr = "todo" TEXT ;
todo_clause = "todo" TEXT ( NL | requires_clause | ensures_clause | example_clause ) ;
```

### Tests

From [Testing and evidence](22-testing.md).

```ebnf
test_item = ( "todo" TEXT )? "test" IDENT "for" test_target test_params? "{" fn_body ;
test_target = path ( "." IDENT )? ;
test_params = "(" param_list ")" ;
dbg_expr = "dbg" "(" expr ")" ;
```

### Low-level programming

From [Low-level programming](23-low-level.md).

```ebnf
trusted_expr = "trusted" TEXT "{" trusted_body "}" ;
trusted_body = expr ( ( assign_operator expr )? NL stmt* )? | trusted_lead_stmt stmt* ;
trusted_lead_stmt = let_stmt | var_stmt | discard_stmt | while_stmt | for_stmt | loop_stmt
    | defer_stmt | undefer_stmt | expect_stmt | assume_stmt ;
layout_item = "layout" IDENT ":" layout_repr ( 'tags' "{" tag_line+ | "{" layout_member+ ) COL0 "}" item_label NL ;
layout_repr = 'bytes' INT layout_order? | IDENT layout_order? ;
layout_order = 'le' | 'be' ;
tag_line = ( DOC NL )* IDENT "=" INT NL ;
layout_member = ( DOC NL )* IDENT ":" layout_field NL ;
layout_field = 'bit' INT ( "..=" INT )? | 'rest' | type_expr 'at' INT ;
registers_item = "registers" IDENT "{" register_member+ COL0 "}" item_label NL ;
register_member = ( DOC NL )* register_line ;
register_line = 'bank' {!register.bank-reserved} | IDENT register_tail ;
register_tail = ":" type_expr 'at' INT ( 'stride' INT )? register_access NL ;
register_access = 'read' 'write'? | 'write' | 'write_one_clear' ;
convention_clause = 'convention' TEXT ;
```

### Registries

From [Registries](25-registries.md).

```ebnf
registry_item = "registry" IDENT ":" type_expr 'keyed' 'by' IDENT unique_list? NL ;
unique_list = 'unique' IDENT ( "," IDENT )* ;
contribute_item = "contribute" path IDENT "=" expr NL ;
```

### Components

From [Components](26-components.md).

```ebnf
protocol_item = 'protocol' IDENT ( 'summary' type_expr )? "{" protocol_operation+ COL0 "}" item_label NL ;
protocol_operation = ( DOC NL )* visibility? "fn" IDENT "(" operation_params? ")" fn_result? fails_clause? limits_clause? NL clause_region? ;
operation_params = operation_param ( "," operation_params? )? ;
operation_param = IDENT ":" type_expr ;
```

### Tombstones

From [Identity and versioning](29-identity-versioning.md).

```ebnf
retired_item = "retired" retired_old "=>" retired_new NL ;
retired_old = path ( "." IDENT )? retired_head? ;
retired_head = generic_params? "(" param_list? ")" fn_result? fails_clause? uses_clause? ;
retired_new = 'manual' TEXT | expr retired_class ;
retired_class = 'preserves' | 'changes' TEXT ;
```

### Doc references

From [Documentation](32-docs.md).

```ebnf
doc_reference = path ( "." IDENT )? ;
```

### Machine files

From [Canonical format and machine files](35-format.md).

```ebnf
check_record = 'check' TEXT ;
lock_file = COL0 'nan' 'lock' INT NL COL0 lock_tail ;
lock_tail = lock_record NL COL0 lock_tail | check_record NL EOF ;
lock_record = 'snapshot' INT
    | 'dep' IDENT ( 'alias' IDENT )? TEXT TEXT ( 'signed' TEXT )? TEXT
    | 'approved' IDENT summary_entry
    | 'epoch' TEXT INT
    | "registry" ( IDENT | INT | TEXT )* {!format.reserved-field}
    | 'floor' ( IDENT | INT | TEXT )* {!format.reserved-field} ;
summary_entry = 'capability' TEXT
    | 'alloc'
    | 'device'
    | 'priv'
    | "validator" TEXT
    | 'escape' escape_kind TEXT ;
escape_kind = "trusted" | "assume" | "foreign" | "landing" | "validator" | 'decoder' | 'grant' | 'class' | 'undo' ;
history_file = COL0 'nan' 'history' INT NL COL0 history_tail ;
history_tail = history_record NL COL0 history_tail
    | retired_item COL0 history_tail
    | check_record NL EOF ;
history_record = 'name' TEXT | 'head' TEXT | 'module' IDENT | 'schema' IDENT TEXT ;
api_file = COL0 'nan' 'api' INT NL COL0 api_tail ;
api_tail = api_record NL COL0 api_tail | check_record NL EOF ;
api_record = 'item' TEXT
    | 'text' TEXT
    | 'promises' promise ( "," promise )*
    | "interface" TEXT
    | 'digest' target TEXT
    | 'doc' ( IDENT | INT )? TEXT TEXT
    | 'method' IDENT
    | 'head' TEXT
    | 'accepted' TEXT
    | "test" TEXT TEXT TEXT*
    | 'pending' TEXT TEXT
    | 'seal' TEXT TEXT TEXT
    | 'closure' ( "interface" | 'body' ) TEXT TEXT
    | 'signature' TEXT ;
promise = IDENT ( "[" IDENT ( "," IDENT )* "]" )? ;
principals_file = COL0 'nan' 'principals' INT NL COL0 principals_tail ;
principals_tail = principal_record NL COL0 principals_tail | check_record NL EOF ;
principal_record = 'principal' TEXT TEXT ;
```

## Start symbols

A **start symbol** is a rule deriving a whole token sequence, using exactly the rules it reaches.

- `source_file` derives a source file, `package.nan` included, without the `DOC` and `FILE_DOC` lines that [Items](02-items.md#item-kinds) reads as plain comments.
- `lock_file`, `history_file`, `api_file` and `principals_file` derive the [machine files](35-format.md#machine-files) `.nan/lock.nan`, `.nan/history.nan`, `.nan/api/<module>.nan` and `.nan/principals.nan`; their single-quoted words match by spelling and are not context words.
- `doc_reference` derives a [doc reference](32-docs.md#references).
- `construction_expr`, `result_expr` and `old_expr` are **classification rules**: the form of a `postfix_expr` or `path_expr` that [construction](04-types.md#construction) or [ensures](18-contracts.md#ensures) classifies as one. No start symbol reaches them; the keyword table counts them as reached from `source_file`.

## Line structure

- In source, the rules accept [`COL0`](01-lexical.md#virtual-tokens) exactly before a file summary, an item's first token or doc comment line, the `}` of a closing line, and the `{` opening a function body after a clause region.
- `NL` ends each statement, loop invariant, match arm, member line, clause line, directive, doc comment or file summary line, single-line item, closing line and head line before a clause region; no rule accepts it right after a [continuing token](01-lexical.md#lines).

## Keyword table

Generated as [Keywords](01-lexical.md#keywords) states:

| Class | Words |
| --- | --- |
| hard keywords | the 67 words of `HARD_KEYWORD` |
| context words | `actuate`, `all`, `allow`, `any`, `as`, `at`, `be`, `because`, `bit`, `by`, `bytes`, `capability`, `changes`, `closed`, `convention`, `deny`, `dep`, `depth`, `device`, `emit`, `entry`, `erase`, `example`, `flexible`, `from`, `irreversible`, `keyed`, `language`, `le`, `length`, `manual`, `migrate`, `noalloc`, `notrap`, `old`, `open`, `preserves`, `priv`, `protocol`, `pure`, `read`, `recorded`, `restorable`, `rest`, `result`, `spend`, `stack`, `stride`, `summary`, `tags`, `targets`, `task`, `terminates`, `traceless`, `undo`, `unique`, `validate`, `view`, `wait`, `write`, `write_one_clear` |
| reserved words | `bank`, `ipc`, `machine`, `stable` |

## One-token lookahead

The syntactic grammar is LL(1) under the resolutions of this section: the next token decides each alternative and whether each optional or repeated part continues, and only the three context-word exceptions and the limit resolution read the token after it.

- Items and statements are keyword-led: after an item's `COL0`, each token decides its part of `item_start` and the kind keyword the item's rule; every statement but `expr_stmt` starts with a hard keyword starting no expression; and the first token of each clause line, directive and member line decides its rule.
- Where a context word and `IDENT` both match a token, the token is the context word, except that `task` is one only before `fn`, `all` and `any` only before an `IDENT`, and `manual` only before a `TEXT`.
- After a `path_expr`, `[` opens `generic_args` exactly when the path's last identifier is PascalCase ([field access and method calls](06-statements-expressions.md#field-access-and-method-calls)).
- After a limit, `,` continues the `limits_clause` exactly when a limit word follows it ([limits](10-effects-digests.md#limits)).
- Everywhere else, an optional or repeated part takes the next token whenever that token can start it. So an `else` after the last block of an `if` belongs to that `if` ([conditionals and loops](06-statements-expressions.md#conditionals-and-loops)); `fails`, `uses`, `limits` and `convention` after a result that is a function type belong to that type, in every head ([function types](10-effects-digests.md#function-types)); and the `expr` ending a short closure or a `when` body after `=>`, or a quantifier after `:`, extends as far as an expression continues.

```nan
pub closed struct Pair[T: Copy] derives Copy {
    pub left: T
    pub right: T
} Pair

pub fn Pair.same(value: T) -> Pair[T] = Pair(left: value, right: value)

pub fn count_where(values: Slice[u32], keep: fn(x: u32) -> bool) -> usize {
    var count: usize = 0
    for value in values {
        if keep(value) {
            count += 1
        }
    }
    return count
} count_where

pub fn above_pair(values: Slice[u32], floor: u32) -> Pair[usize] {
    let matches = count_where(values, keep: fn(x: u32) => x > floor)
    return Pair[usize].same(matches)
} above_pair
```

## Prefix viability

A **prefix** is the text of a source file up to any byte. It lexes as a file, except that it reports no `lex.missing-final-newline` and its tokens have no `EOF`. When it does not end with LF, its last line ends at the cut, reports no `lex.trailing-whitespace` and drops the token, an unterminated literal included, reaching the cut. No `NL` follows its last token when it does not end with LF or that token is a continuing token or a `TEXT_LINE`.

A token is **accepted** when the parse of the tokens before it continues with it under the rules and resolutions of this chapter, outside every rejected alternative; in a [single-line item](02-items.md#item-shape), `match` and an `NL` inside a `{` the item opens never are. The **first dead token** of a file or prefix is its first token not accepted, or, when earlier, the token after those yielded by a line holding a lexical diagnostic ([line-local lexing](01-lexical.md#line-local-lexing)).

**Closing** a prefix, or its part before its first dead token, appends the fewest tokens after which `source_file` derives the whole, counting virtual tokens and not typed holes. Among equally few, at the first choice where they differ, closing skips an optional or repeated part, or else takes the alternative written first.

- A **typed hole** is an appended `todo_expr`, `todo "hole"`, with the expected type of its position ([holes](18-contracts.md#holes)).
- An appended closing label repeats the name it closes; any other appended `IDENT` is `hole`, `INT` is `0` and `TEXT` is `"hole"`.
- A **closer** is a maximal run of appended tokens other than typed holes on one line.

A **pending clause** is a `fails` clause, `uses wait` clause or from variant that the exact fix of an `item.fails-missing`, `item.wait-missing`, `error.cancel-unmapped` or `error.unconvertible` diagnostic adds. In a prefix, such a diagnostic with an exact fix is not reported, and closing adds its pending clauses as the fix does. Pending clauses are lower bounds: text after the prefix never removes one.

The prefix up to line 3 of this file closes with two closers:

```nan
pub fn clamp_level(level: u32, ceiling: u32) -> u32 {
    if level > ceiling {
        return ceiling
    }
    return level
} clamp_level
```

```text
$ nan check --prefix example/example.nan:3
ok check --prefix example/example.nan:3: dead 0, pending 0, holes 0
close }
close } clamp_level
ok check --prefix example/example.nan:3: dead 0, pending 0, holes 0
```

## Grammar publication

These rules and resolutions are the grammar that language 1 publishes ([contents of a version](37-evolution.md#contents-of-a-version)); every implementation of language 1 lexes and parses exactly by them.
