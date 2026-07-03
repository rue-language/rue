+++
title = "Grammar"
weight = 1
template = "spec/page.html"
+++

# Appendix A: Grammar

This appendix contains the complete EBNF grammar for Rue. It is maintained by
hand against the parser in `crates/rue-parser/src/chumsky_parser.rs`; when the
parser changes, this appendix must be updated in the same change.

```ebnf
(* Program structure *)
program        = { item } ;
item           = function | struct_def | enum_def | drop_fn | const_decl ;

(* Directives and intrinsics *)
directives     = { directive } ;
directive      = "@" IDENT [ "(" [ directive_args ] ")" ] ;
directive_args = IDENT { "," IDENT } [ "," ] ;
intrinsic      = "@" IDENT "(" [ intrinsic_args ] ")" ;
intrinsic_args = intrinsic_arg { "," intrinsic_arg } [ "," ] ;
intrinsic_arg  = type | expression ;

(* Functions *)
function       = directives [ "pub" ] [ "unchecked" ]
                 "fn" IDENT "(" [ params ] ")" [ "->" type ] "{" block "}" ;
params         = param { "," param } ;
param          = [ param_mode ] IDENT ":" type ;
param_mode     = "comptime" | "inout" | "borrow" ;
block          = { statement } [ expression ] ;

(* Structs: fields first (comma-separated), then inline methods *)
struct_def     = directives [ "pub" ] [ "linear" ]
                 "struct" IDENT "{" [ struct_fields ] { method } "}" ;
struct_fields  = struct_field { "," struct_field } [ "," ] ;
struct_field   = IDENT ":" type ;
method         = directives "fn" IDENT
                 "(" [ "self" [ "," params ] | params ] ")"
                 [ "->" type ] "{" block "}" ;

(* Enums *)
enum_def       = [ "pub" ] "enum" IDENT "{" [ enum_variants ] "}" ;
enum_variants  = IDENT { "," IDENT } [ "," ] ;

(* Destructors *)
drop_fn        = "drop" "fn" IDENT "(" "self" ")" "{" block "}" ;

(* Constants (also used for module re-exports) *)
const_decl     = directives [ "pub" ] "const" IDENT [ ":" type ] "=" expression ";" ;

(* Statements *)
statement      = let_stmt | assign_stmt | expr_stmt ;
let_stmt       = directives "let" [ "mut" ] let_pattern [ ":" type ] "=" expression ";" ;
let_pattern    = IDENT | "_" ;
assign_stmt    = place_expr "=" expression ";" ;
expr_stmt      = expression ";"
               | control_flow_expr ;   (* if/match/while/loop/for/break/continue/
                                          return and bare blocks need no semicolon *)

(* Place expressions: a variable with zero or more field/index projections.
   Used as assignment targets and as inout/borrow call arguments. *)
place_expr     = IDENT { "." IDENT | "[" expression "]" } ;

(* Types *)
type           = "i8" | "i16" | "i32" | "i64"
               | "u8" | "u16" | "u32" | "u64"
               | "usize" | "isize"
               | "bool" | "()" | "!"
               | "[" type ";" array_length "]"
               | "ptr" "const" type
               | "ptr" "mut" type
               | anon_struct_type
               | "Self"
               | IDENT ;
array_length   = INTEGER | IDENT ;
anon_struct_type = "struct" "{" [ anon_struct_fields ] { method } "}" ;
anon_struct_fields = struct_field { "," struct_field } [ "," ] ;

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

(* Postfix suffixes: field access, method calls, indexing, and
   module-qualified struct literals / enum paths / associated calls. *)
postfix        = primary { suffix } ;
suffix         = "." IDENT                                     (* field access *)
               | "." IDENT "(" [ call_args ] ")"               (* method call *)
               | "[" expression "]"                            (* indexing *)
               | "." IDENT "{" [ field_inits ] "}"             (* qualified struct literal *)
               | "." IDENT "::" IDENT                          (* qualified enum variant *)
               | "." IDENT "::" IDENT "(" [ call_args ] ")" ;  (* qualified assoc fn call *)

(* Call arguments: inout/borrow arguments must be place expressions
   (a variable, optionally with field/index projections); plain
   arguments are arbitrary expressions. *)
call_args      = call_arg { "," call_arg } ;
call_arg       = ( "inout" | "borrow" ) place_expr
               | expression ;

primary        = INTEGER | STRING | BOOL | "()"
               | "self"
               | ident_expr
               | self_struct_literal
               | intrinsic
               | array_literal
               | anon_struct_type        (* type used as a value, e.g. comptime *)
               | primitive_type_literal  (* e.g. `i32` as a comptime value *)
               | "(" expression ")"
               | comptime_expr
               | checked_expr
               | block_expr
               | control_flow_expr ;

(* An identifier optionally followed by call arguments, a struct literal
   body, or a path. *)
ident_expr     = IDENT "(" [ call_args ] ")"           (* function call *)
               | IDENT "{" [ field_inits ] "}"         (* struct literal *)
               | IDENT "::" IDENT "(" [ call_args ] ")" (* associated fn call *)
               | IDENT "::" IDENT                      (* enum variant *)
               | IDENT ;
self_struct_literal = "Self" "{" [ field_inits ] "}" ;
primitive_type_literal = "i8" | "i16" | "i32" | "i64"
                       | "u8" | "u16" | "u32" | "u64" | "bool" ;

(* Compound expressions *)
block_expr     = "{" block "}" ;
comptime_expr  = "comptime" "{" block "}" ;
checked_expr   = "checked" "{" block "}" ;
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
               | IDENT "::" IDENT                      (* Enum::Variant *)
               | IDENT { "." IDENT } "::" IDENT ;      (* module.Enum::Variant *)
while_expr     = "while" expression "{" block "}" ;
loop_expr      = "loop" "{" block "}" ;
for_expr       = "for" ( IDENT | "_" ) "in" expression "{" block "}" ;
                 (* preview: --preview for_loops *)
break_expr     = "break" [ expression ] ;   (* an operand parses but is always
                                               rejected in semantic analysis *)
return_expr    = "return" [ expression ] ;
array_literal  = "[" ( [ expression { "," expression } ]
                     | expression ";" array_length ) "]" ;  (* repeat form: array_length is a compile-time constant *)
field_inits    = field_init { "," field_init } [ "," ] ;
field_init     = IDENT ":" expression ;

(* Lexical elements *)
IDENT          = ( letter | "_" ) { letter | digit | "_" } ;
INTEGER        = dec_literal | hex_literal | oct_literal | bin_literal ;
dec_literal    = digit { digit | "_" } ;
hex_literal    = "0x" { hex_digit | "_" } ;   (* at least one hex_digit *)
oct_literal    = "0o" { oct_digit | "_" } ;   (* at least one oct_digit *)
bin_literal    = "0b" { bin_digit | "_" } ;   (* at least one bin_digit *)
hex_digit      = digit | "a" | ... | "f" | "A" | ... | "F" ;
oct_digit      = "0" | "1" | "2" | "3" | "4" | "5" | "6" | "7" ;
bin_digit      = "0" | "1" ;
STRING         = '"' { string_char | escape } '"' ;
escape         = "\\" ( "\\" | '"' | "n" | "t" | "r" | "0" ) ;
BOOL           = "true" | "false" ;
letter         = "a" | ... | "z" | "A" | ... | "Z" ;
digit          = "0" | "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" ;

(* Whitespace and comments are ignored between tokens *)
whitespace     = " " | "\t" | "\n" | "\r" ;
line_comment   = "//" { any_char_except_newline } newline ;
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
- **`inout`/`borrow` call arguments** must be place expressions: a variable
  optionally followed by field and index projections (e.g. `inout s.arr[i]`),
  not arbitrary expressions.
- **A parameter takes at most one mode** (`comptime`, `inout`, or `borrow`);
  duplicate or conflicting modes are a parse error.
- **Statement termination**: `let`, assignment, and ordinary expression
  statements require `;`. Control-flow expressions (`if`, `match`, `while`,
  `loop`, `for`, `break`, `continue`, `return`) and bare blocks may appear as
  statements without a trailing semicolon.
- There are no `impl` blocks: methods are declared inline inside the
  `struct` body, after the fields.
