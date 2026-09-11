+++
title = "Grammar"
weight = 1
template = "spec/page.html"
+++

# Appendix A: Grammar

This appendix contains the complete EBNF grammar for Rue. It is maintained by
hand against the parser in `crates/rue-parser/src/parser.rs`; when the
parser changes, this appendix must be updated in the same change.

This grammar is **normative**: it is the authoritative syntactic definition of
Rue. The EBNF fragments that appear inline in Chapters 5 and 6 are
**illustrative excerpts** for local exposition and are deliberately narrowed to
the construct under discussion; where any of them differs from this appendix,
this appendix governs.

<!-- grammar-sync(id="9.3:1a", production="item", role="appendix", relation="contains", symbol="extern_block") -->
<!-- grammar-sync(id="9.3:1a", production="item", role="appendix", relation="contains", symbol="extern_export") -->
<!-- grammar-sync(id="9.3:1a", production="extern_block", role="appendix") -->
<!-- grammar-sync(id="9.3:1a", production="extern_fn", role="appendix") -->
<!-- grammar-sync(id="9.3:1a", production="extern_result", role="appendix") -->
<!-- grammar-sync(id="9.3:1a", production="extern_export", role="appendix") -->
<!-- grammar-sync(id="6.6:2", production="primary", role="appendix", relation="contains", symbol="yield_expr") -->
<!-- grammar-sync(id="6.6:2", production="yield_expr", role="appendix") -->
<!-- grammar-sync(id="6.7:2", production="item", role="appendix", relation="contains", symbol="test_item") -->
<!-- grammar-sync(id="6.7:2", production="test_item", role="appendix") -->
<!-- grammar-sync(id="5.1:2", production="let_pattern", role="appendix") -->
<!-- grammar-sync(id="5.1:2", production="struct_pattern", role="appendix") -->
<!-- grammar-sync(id="5.1:2", production="field_patterns", role="appendix") -->
<!-- grammar-sync(id="5.1:2", production="field_pattern", role="appendix") -->
<!-- grammar-sync(id="4.7:2", production="pattern", role="appendix") -->
<!-- grammar-sync(id="4.7:2", production="path_pattern", role="appendix") -->
<!-- grammar-sync(id="4.7:2", production="pattern_elements", role="appendix") -->
<!-- grammar-sync(id="4.7:2", production="pattern_element", role="appendix") -->

<!-- grammar-sync(id="2.1:26", production="INTEGER", role="appendix", relation="contains", symbol="byte_literal") -->
<!-- grammar-sync(id="2.1:26", production="byte_literal", role="appendix") -->
<!-- grammar-sync(id="2.1:26", production="byte_char", role="appendix") -->
<!-- grammar-sync(id="2.1:6", production="STRING", role="appendix", relation="contains", symbol="string_char") -->
<!-- grammar-sync(id="2.1:6", production="string_char", role="appendix") -->
<!-- grammar-sync(id="2.1:6", production="escape_sequence", role="appendix") -->
<!-- grammar-sync(id="2.2:1", production="any_char_except_newline", role="appendix") -->
<!-- grammar-sync(id="2.2:1", production="newline", role="appendix") -->
<!-- grammar-sync(id="2.2:1", production="line_comment", role="appendix") -->

```ebnf
(* Program structure *)
program        = { item } ;
item           = function | extern_block | extern_export | struct_def | enum_def | drop_fn | const_decl | test_item ;

(* Directives and intrinsics *)
directives     = { directive } ;
directive      = "@" IDENT [ "(" [ directive_args ] ")" ] ;
directive_args = directive_arg { "," directive_arg } [ "," ] ;
directive_arg  = IDENT | STRING ;
intrinsic      = "@" IDENT "(" [ intrinsic_args ] ")" ;
intrinsic_args = intrinsic_arg { "," intrinsic_arg } [ "," ] ;
intrinsic_arg  = type | expression ;

(* Functions *)
function       = directives [ "pub" ] [ "unchecked" ]
                 "fn" IDENT "(" [ params ] ")" [ result ] "{" block "}" ;
result         = "->" [ "borrow" | "inout" ] type ; (* marks a place-returning
                                                        accessor (ADR-0062);
                                                        result and
                                                        receiver modes pair —
                                                        a legality rule *)
params         = param { "," param } [ "," ] ;
param          = [ param_mode ] IDENT ":" type ;
param_mode     = "comptime" | "inout" | "borrow" ;
block          = { statement } [ expression ] ;

(* C foreign boundary declarations. Preview, ABI, and FFI-safety requirements
   are legality rules rather than grammar. *)
extern_block   = "extern" STRING "{" { extern_fn } "}" ;
extern_fn      = "fn" IDENT "(" [ params ] ")" [ extern_result ] ";" ;
extern_result  = "->" type ;
extern_export  = "pub" "extern" STRING [ "unchecked" ] "fn" IDENT
                 "(" [ params ] ")" [ result ] "{" block "}" ;

(* Structs: fields first (comma-separated), then inline methods *)
struct_def     = directives [ "pub" ] [ "linear" ]
                 "struct" IDENT "{" [ struct_fields ] { method } "}" ;
struct_fields  = struct_field { "," struct_field } [ "," ] ;
struct_field   = IDENT ":" type ;
method         = directives "fn" IDENT
                 "(" [ [ "inout" | "borrow" | "mut" ] "self" [ "," params ] | params ] ")"
                 [ result ] "{" block "}" ;

(* Enums *)
enum_def       = directives [ "pub" ] "enum" IDENT "{" [ enum_variants ] "}" ;
enum_variants  = enum_variant { "," enum_variant } [ "," ] ;
enum_variant   = IDENT [ "(" type { "," type } [ "," ] ")" ] ;  (* optional tuple payload; at least one type inside the parens *)

(* Destructors *)
drop_fn        = "drop" "fn" IDENT "(" "self" ")" "{" block "}" ;

(* Constants (also used for module re-exports) *)
const_decl     = directives [ "pub" ] "const" IDENT [ ":" type ] "=" expression ";" ;

(* Tests. `test` is a contextual keyword: it introduces a test only at item
   position and only when followed by a STRING. Name uniqueness within a
   module is a legality rule. *)
test_item      = directives "test" STRING "{" block "}" ;

(* Statements *)
statement      = let_stmt | assign_stmt | expr_stmt ;
let_stmt       = directives "let" [ "mut" ] let_pattern [ ":" type ] "=" expression ";" ;
let_pattern    = IDENT | "_" | struct_pattern ;
struct_pattern = type "{" [ field_patterns ] "}" ;
field_patterns = field_pattern { "," field_pattern } [ "," ] ;
field_pattern  = [ "mut" ] IDENT
               | IDENT ":" ( [ "mut" ] IDENT | "_" ) ;
assign_stmt    = place_expr ( "=" | compound_op ) expression ";" ;
compound_op    = "+=" | "-=" | "*=" | "/=" | "%="
               | "&=" | "|=" | "^=" | "<<=" | ">>=" ;
expr_stmt      = expression ";"
               | control_flow_expr
               | block_expr ;          (* block-like expressions need no semicolon *)
(* The trailing exit of an accessor body (ADR-0062); parsed as an expression
   form, valid only as the single trailing statement of a `-> borrow` or
   `-> inout` accessor body — a legality rule. *)
yield_expr     = "yield" expression ;

(* Place expressions: a variable — or `self`, inside a method — followed by
   zero or more field/index projections, or an accessor call followed by those
   projections. Ordinary method calls are rejected semantically as targets.
   Assigning to a bare `self` is legal only for a `mut self` receiver
   (a legality rule). *)
place_expr     = ( IDENT | "self" ) { place_postfix } ;
place_postfix  = "." IDENT | "[" expression "]"
               | "." IDENT "(" [ call_args ] ")" ;

(* Types *)
type           = "i8" | "i16" | "i32" | "i64"
               | "u8" | "u16" | "u32" | "u64"
               | "usize" | "isize"
               | "f32" | "f64"
               | "bool" | "type" | "()" | "!"
               | "[" type [ ";" array_length ] "]"
               | "ptr" "const" type
               | "ptr" "mut" type
               | anon_struct_type
               | anon_enum_type
               | "Self"
               | named_type ;
named_type     = qualified_ident [ "(" [ type_call_args ] ")" ] ;
qualified_ident = IDENT { "." IDENT } ;
type_call_args = type_call_arg { "," type_call_arg } [ "," ] ;
type_call_arg  = type | [ "-" ] INTEGER ;  (* a type argument for a `comptime T: type` parameter, or an integer value argument for a comptime value parameter such as `comptime N: i32` *)
array_length   = INTEGER | IDENT | length_call ;
length_call    = IDENT "(" [ array_length { "," array_length } [ "," ] ] ")" ;  (* comptime-evaluable call, e.g. fact(4) *)
anon_struct_type = "struct" "{" [ anon_struct_fields ] "}" ;
anon_struct_value = "struct" "{" [ anon_struct_fields ] { anon_struct_member } "}" ;
anon_struct_fields = struct_field { "," struct_field } [ "," ] ;
anon_struct_member = method | anon_drop_fn ;
anon_drop_fn   = "drop" "fn" "(" "self" ")" "{" block "}" ;
anon_enum_type = "enum" "{" [ enum_variants ] "}" ;

(* Expressions: the precedence ladder, loosest first. This matches Rust's
   operator precedence: unary > * / % > + - > << >> > & > ^ > | >
   comparisons > && > ||. All binary operators are left-associative. *)
expression     = or_expr ;
or_expr        = and_expr { "||" and_expr } ;
and_expr       = comparison { "&&" comparison } ;
comparison     = bitor_expr { ( "==" | "!=" | "<" | ">" | "<=" | ">=" ) bitor_expr } ;
bitor_expr     = bitxor_expr { "|" bitxor_expr } ;
bitxor_expr    = bitand_expr { "^" bitand_expr } ;
bitand_expr    = shift_expr { "&" shift_expr } ;
shift_expr     = additive { ( "<<" | ">>" ) additive } ;
additive       = multiplicative { ( "+" | "-" ) multiplicative } ;
multiplicative = unary { ( "*" | "/" | "%" ) unary } ;
unary          = ( "-" | "!" | "~" ) unary | postfix ;

(* Postfix suffixes: field access, method calls, indexing, and qualified
   struct literals. `.` is the sole member-access spelling (RUE-488): an enum
   variant `Enum.Variant`, an associated call `Type.function(args)`, and their
   module-qualified forms are all chains of `.` field/method suffixes here,
   disambiguated during semantic analysis by whether the base names a type. *)
postfix        = primary { suffix } ;
suffix         = "." IDENT                                     (* field access / Enum.Variant / assoc-fn path *)
               | "." IDENT "(" [ call_args ] ")"               (* method call / Type.function(args) *)
               | "." IDENT "(" [ call_args ] ")"
                   "{" [ field_inits ] "}"                    (* qualified generic struct literal *)
               | "[" expression "]"                            (* indexing *)
               | "?"                                           (* try / Option propagation *)
               | "." IDENT "{" [ field_inits ] "}" ;           (* qualified struct literal *)

(* Call arguments: any argument may carry an `inout`/`borrow` mode. The
   argument itself is parsed as an arbitrary expression; the requirement that
   an inout argument denote a place (a variable, optionally with field/
   index projections) is a legality rule (6.1:17), not a syntactic one. A
   `borrow` argument that denotes no place is elaborated into one (6.1:39). *)
call_args      = call_arg { "," call_arg } [ "," ] ;
call_arg       = [ "inout" | "borrow" ] expression ;

primary        = INTEGER | FLOAT | STRING | BOOL | "()"
               | "self"
               | ident_expr
               | self_struct_literal
               | intrinsic
               | array_literal
               | anon_struct_value       (* type used as a value, e.g. comptime *)
               | anon_enum_type          (* anonymous sum type used as a value *)
               | primitive_type_literal  (* e.g. `i32` as a comptime value *)
               | "(" expression ")"
               | comptime_expr
               | checked_expr
               | block_expr
               | control_flow_expr
               | yield_expr ;

(* An identifier optionally followed by call arguments, a struct literal
   body, or a path. *)
ident_expr     = IDENT "(" [ call_args ] ")"           (* function call *)
               | IDENT "(" [ call_args ] ")"
                   "{" [ field_inits ] "}"             (* generic struct literal *)
               | IDENT "{" [ field_inits ] "}"         (* struct literal *)
               | IDENT ;                               (* Enum.Variant / Type.function(args) parse via postfix `.` suffixes *)
self_struct_literal = "Self" "{" [ field_inits ] "}" ;
primitive_type_literal = "i8" | "i16" | "i32" | "i64"
                       | "u8" | "u16" | "u32" | "u64" | "bool"
                       | "f32" | "f64" ;

(* Compound expressions *)
block_expr     = "{" block "}" ;
comptime_expr  = "comptime" "{" block "}" ;
checked_expr   = "checked" [ STRING ] "{" block "}" ;   (* the STRING is the block's reason, 9.1:14; preview `checked_reasons` *)
control_flow_expr = if_expr | match_expr | while_expr | loop_expr | for_expr
                  | break_expr | "continue" | return_expr ;
if_expr        = "if" expression "{" block "}" [ else_clause ] ;
else_clause    = "else" ( "{" block "}" | if_expr ) ;
match_expr     = "match" expression "{" [ match_arms ] "}" ;
match_arms     = match_arm { "," match_arm } [ "," ] ;
match_arm      = pattern "=>" expression ;
pattern        = "_"
               | [ "-" ] INTEGER
               | BOOL
               | path_pattern
               | struct_pattern ;
path_pattern = pattern_head "." IDENT [ "(" pattern_elements ")" ] ;
pattern_head   = qualified_ident [ "(" [ call_args ] ")" ] ;
pattern_elements = pattern_element { "," pattern_element } [ "," ] ;
pattern_element = IDENT | "_" | path_pattern | struct_pattern ;
while_expr     = "while" expression "{" block "}" ;
loop_expr      = "loop" "{" block "}" ;
for_expr       = "for" ( IDENT | "_" ) "in" expression "{" block "}" ;
break_expr     = "break" [ expression ] ;   (* an operand parses but is always
                                               rejected in semantic analysis *)
return_expr    = "return" [ expression ] ;
array_literal  = "[" ( [ expression { "," expression } [ "," ] ]
                     | expression ";" repeat_count ) "]" ;
repeat_count   = INTEGER | IDENT ;  (* repeat form: a literal or named compile-time constant (no call form) *)
field_inits    = field_init { "," field_init } [ "," ] ;
field_init     = IDENT ":" expression      (* explicit *)
               | IDENT ;                    (* field-init shorthand: `x` means `x: x` (RUE-613) *)

(* Lexical elements *)
IDENT          = ( letter | "_" ) { letter | digit | "_" } ;
INTEGER        = byte_literal | dec_literal | hex_literal | oct_literal | bin_literal ;
dec_literal    = digit { digit | "_" } ;
hex_literal    = "0x" { hex_digit | "_" } ;   (* at least one hex_digit *)
oct_literal    = "0o" { oct_digit | "_" } ;   (* at least one oct_digit *)
bin_literal    = "0b" { bin_digit | "_" } ;   (* at least one bin_digit *)
FLOAT          = dec_literal ( float_fraction [ float_exponent ]
                             | float_exponent ) ;   (* §2.1:29; no suffixes *)
float_fraction = "." dec_literal ;
float_exponent = ( "e" | "E" ) [ "+" | "-" ] dec_literal ;
hex_digit      = digit | "a" | ... | "f" | "A" | ... | "F" ;
oct_digit      = "0" | "1" | "2" | "3" | "4" | "5" | "6" | "7" ;
bin_digit      = "0" | "1" ;
byte_literal   = "b'" ( byte_char | escape_sequence | "\'" ) "'" ;
byte_char      = ? any ASCII character except "'" or "\" ? ; (* one ASCII byte; value 0–255 *)
STRING         = '"' { string_char } '"' ;
string_char    = ? any character except '"', '\\', '\n', or '\r' ?
               | escape_sequence ;
escape_sequence = "\\" | "\"" | "\n" | "\t" | "\r" | "\0" ;
BOOL           = "true" | "false" ;
letter         = "a" | ... | "z" | "A" | ... | "Z" ;
digit          = "0" | "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" ;

(* Whitespace and comments are ignored between tokens *)
whitespace     = " " | "\t" | "\n" | "\r" ;
any_char_except_newline = ? any character except '\n' or '\r' ? ;
newline        = "\r\n" | "\n" | "\r" ;
line_comment   = "//" { any_char_except_newline } [ newline ] ;
```

Notes:

- **Operator precedence** is Rust's ladder (see spec rule 4.3a:13 and the
  `precedence` module in the parser): unary operators bind tightest, then
  `* / %`, `+ -`, `<< >>`, `&`, `^`, `|`, comparisons, `&&`, and `||`
  loosest. So `a << b + c` is `a << (b + c)` and `a & b == c` is
  `(a & b) == c`.
- **`usize` and `isize`** lex as ordinary identifiers (they are not keyword
  tokens) but always denote the pointer-width integer types (see spec rules
  3.1:21–3.1:22); the other primitive type names are keywords.
- **Intrinsic arguments** may be types or expressions. Syntax that is
  unambiguously a type (`()`, `[T; N]`, a primitive type keyword, or a `!`
  that is the entire argument) parses as a type; anything else — including
  `!expr`, which is logical not — parses as an expression.
- **`inout`/`borrow` call arguments** are parsed as ordinary expressions; the
  rule that an `inout` argument must denote a place — a variable optionally
  followed by field and index projections (e.g. `inout s.arr[i]`) — is a
  legality rule enforced during semantic analysis (6.1:17), not a syntactic
  restriction. A non-place `inout` argument such as `inout a + 1` parses but is
  rejected with an lvalue error (E0425). A `borrow` argument has no such
  restriction: one that denotes no place is elaborated into a promoted static
  or a compiler-materialized temporary (6.1:39).
- **Type-function application in type position** (`named_type` followed by
  arguments): a name or
  module-qualified path applied to arguments, e.g. `Pair(i32)`,
  `Result(Option(i32), i32)`, `std.option.Option(i64)`, or `Buffer(2)`,
  denotes the type produced by a comptime `-> type` constructor (RUE-241).
  Each argument is bound by the corresponding parameter's declared kind: a
  `comptime T: type` parameter takes a type argument, and a comptime value
  parameter (`comptime N: i32`) takes an integer literal, a comptime
  parameter name, or a constant name (RUE-552). Nested applications compose.
  A local or module-qualified application may head a struct literal, as in
  `Pair(i32) { first: 1, second: 2 }` or
  `std.tuple.Pair(i32) { first: 1, second: 2 }`.
- **Anonymous struct types** use the fields-only `anon_struct_type` production
  in pure type position (a type annotation). When a `struct { … }` appears as a
  *value* (e.g. the body of a comptime `-> type` function),
  `anon_struct_value` also permits inline methods and one `drop fn(self)`
  member. Unlike a top-level `drop_fn`, this form has no type name between `fn`
  and the receiver list.
- **Anonymous enum types** use the same variant grammar as named enums and may
  appear in either type or value position. The value-position form is how a
  comptime type constructor returns a sum type, as in
  `fn Option(comptime T: type) -> type { enum { Some(T), None } }`.
- **Generic enum patterns** may apply a local or module-qualified type
  constructor immediately before the final variant segment, as in
  `Result(i32, E).Ok(v)` or `std.result.Result(i32, E).Ok(v)`. The final
  parenthesized group, when present, contains payload positions rather than
  constructor arguments.
- **Nested variant patterns**: a payload position may itself be a
  `path_pattern`, recursively — `R.Err(E.A(b))` — matching against that
  position's payload type (4.7:37).
- **Struct patterns in arms**: a match arm, or a payload position, may be a
  `struct_pattern` — `Point { x, y }`, `R.Ok(Point { x, y })` — binding the
  fields of the struct value there (4.7:42). A name that continues into `{`
  (directly, through a module path, or through a type-constructor call)
  begins a struct pattern; any other name begins a `path_pattern`. A struct
  pattern's field positions are binders and do not nest.
- **A parameter takes at most one mode** (`comptime`, `inout`, or `borrow`);
  duplicate or conflicting modes are a parse error.
- **Statement termination**: `let`, assignment, and ordinary expression
  statements require `;`. Control-flow expressions (`if`, `match`, `while`,
  `loop`, `for`, `break`, `continue`, `return`) and bare blocks may appear as
  statements without a trailing semicolon. The token that decides each of
  these forms is given in the parsing requirements below (A.2:2).
- There is no `impl`-block construct (`impl` remains reserved syntax; 2.4:2):
  methods are declared inline inside the `struct` body, after the fields.
- **Method receivers** may carry a mode: `inout self` (mutating receiver) or
  `borrow self` (read-only receiver); a bare `self` is by-value. This mirrors
  the `inout`/`borrow` parameter modes; `comptime self` is not permitted.

## Parsing requirements

{{ rule(id="A.2:1", cat="normative") }}

The grammar is *committed*: a conforming implementation **MUST** parse a
syntactically valid program in one left-to-right pass whose cost is linear in
the program's token count, up to a constant factor bounded by the nesting
allowance of C.6:3. Every choice between alternatives **MUST** be settled by
the current token, by a fixed number of following tokens, or by one forward
scan to the matching close delimiter of a group that is then parsed once. A
construct that has been parsed **MUST NOT** be parsed again to settle a later
choice, and an implementation **MUST NOT** parse an expression speculatively
and discard the result. Error recovery is held to the same bound: the tokens
a recovery skips are consumed once. Appendix A is written so that this is
possible, and a change to the grammar that would require speculative parsing
is a change to this rule.

{{ rule(id="A.2:2", cat="normative") }}

The decision points of the grammar, and the token that settles each:

1. **Block item or block tail** (`block = { statement } [ expression ]`).
   After an expression in statement position, `;` makes it an expression
   statement (5.3:1), `=` or a compound assignment operator makes it the
   target of an assignment (5.2), and `}` makes it the block's tail value
   (4.6). A block-like expression (5.3:6) followed by any other token is a
   complete statement on its own; any other expression followed by any other
   token is a syntax error at that token, reported as a missing semicolon.
2. **Semicolon-free control flow or tail value.** A block-like expression in
   statement position followed by `}` is the block's tail value; followed by
   anything else it is a statement, and the next statement begins at that
   token. No token after the closing brace is re-read to make that choice.
3. **Continuation of a block-like expression.** In statement position a
   block-like expression is continued only by a postfix suffix (`.`, `(`,
   `[`, `?`); `-` begins a new statement (5.3:9), and any other infix
   operator is a syntax error at the operator (5.3:7). In operand position
   the ordinary precedence climb applies. `else` continues an `if` only when
   it is the very next token.
4. **Array list or array repeat** (`[a, b]` against `[a; n]`). The first
   element is parsed once; the token after it decides: `;` selects the
   repeat form, whose count is a single integer literal or identifier, and
   `,` or `]` selects the list form. The same rule decides an array type
   `[T; n]`, where the token after the element type is always `;`.
5. **Optional operands of `return` and `break`.** The operand is absent
   exactly when the token after the keyword is `{`, or one of the expression
   terminators `;`, `,`, `)`, `]`, `}`, `=>`, or the end of input; any other
   token begins the operand.
6. **Payload group or constructor application in a pattern**
   (`Ok(v)` against `Result(i32, E).Ok(v)`). A parenthesised group after a
   path segment is scanned once to its matching `)`: a following `.` makes
   it constructor arguments, and anything else makes it the variant's
   payload positions. The group is then parsed once in the role the scan
   chose.
7. **Recovery.** A malformed item is skipped to the next item keyword at
   brace depth zero, tracking delimiter depth across the skipped tokens so
   that an item keyword inside the failed item's own braces does not restart
   parsing. A missing semicolon or delimiter is reported at the offending
   token and the parse of the enclosing construct ends there; the tokens
   already consumed are not revisited.

{{ rule(id="A.2:3", cat="informative") }}

The forms in A.2:2 have each admitted an exponential parser in this
compiler's history. Before RUE-276 a statement-position block-like expression
was parsed once to look for a following `-` and then, failing that, parsed
again by the general expression parser, so `n` nested blocks cost 2^n
descents; a 20-level nest did not finish. Nested slice types had the same
shape (RUE-1113). The criterion in A.2:1 is what those fixes established,
recorded here so that it binds a rewrite of the parser and the Rue-hosted
frontend (`examples/ruelex`), whose token dump and AST shape are held to the
production parser by the frontend differential, alike. Both are held to the
rate itself by generated programs: `scripts/check-parser-complexity.py` runs
each frontend on a program for every decision point above at two sizes,
one four times the other, and fails when the larger costs more than a fixed
multiple of the smaller.

The bound in A.2:1 is linear "up to a constant factor bounded by the nesting
allowance" because the one forward scan an implementation may make (item 6,
and the reference implementation's peel of leading `[` tokens when deciding
whether a bracketed intrinsic argument is a type) can be repeated once per
level of a nest, and a nest is at most C.6:3 levels deep. The pre-parse
nesting guard that enforces C.6:3 is held to the same discipline: it reads
each token once and counts an `else if` link, not a plain `else`, toward the
depth, discharging a completed chain when the token after its last brace is
not `else`, so that a sequence of statement-position `if ... else ...`
statements is a sequence and not a nest (RUE-1107).

{{ rule(id="A.2:4", cat="example") }}

```rue
fn f(c: bool) -> i32 {
    if c { 1 } else { 2 }      // complete statement: `}` then `let` (A.2:2 item 2)
    let a = [1; 3];            // `;` after the first element: repeat form (item 4)
    let b = [1, 2, 3];         // `,` after the first element: list form (item 4)
    loop { if c { break } break }  // `}` after `break`: no operand (item 5)
    if c { return a[0] }       // `a` after `return`: an operand (item 5)
    b[2]                       // `}` after the expression: the tail (item 1)
}
```
