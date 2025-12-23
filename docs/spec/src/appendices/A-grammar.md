+++
title = "Grammar"
weight = 1
template = "spec/page.html"
+++

# Appendix A: Grammar

This appendix contains the complete EBNF grammar for Rue.

```ebnf
(* Program structure *)
program        = { item } ;
item           = function | struct_def ;

(* Functions *)
function       = "fn" IDENT "(" [ params ] ")" [ "->" type ] "{" block "}" ;
params         = param { "," param } ;
param          = IDENT ":" type ;
block          = { statement } [ expression ] ;

(* Structs *)
struct_def     = "struct" IDENT "{" [ struct_fields ] "}" ;
struct_fields  = struct_field { "," struct_field } [ "," ] ;
struct_field   = IDENT ":" type ;

(* Statements *)
statement      = let_stmt | assign_stmt | expr_stmt ;
let_stmt       = "let" [ "mut" ] IDENT [ ":" type ] "=" expression ";" ;
assign_stmt    = IDENT "=" expression ";"
               | IDENT "[" expression "]" "=" expression ";"
               | expression "." IDENT "=" expression ";" ;
expr_stmt      = expression ";" ;

(* Types *)
type           = "i8" | "i16" | "i32" | "i64"
               | "u8" | "u16" | "u32" | "u64"
               | "bool" | "()"
               | "[" type ";" INTEGER "]"
               | IDENT ;

(* Expressions *)
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
unary          = "-" unary | "!" unary | "~" unary | postfix ;
postfix        = primary { "[" expression "]" | "(" [ args ] ")" | "." IDENT } ;
intrinsic      = "@" IDENT "(" [ args ] ")" ;
args           = expression { "," expression } ;
primary        = INTEGER | BOOL | IDENT
               | "(" expression ")"
               | block_expr
               | if_expr
               | match_expr
               | while_expr
               | loop_expr
               | "break" | "continue"
               | return_expr
               | array_literal
               | struct_literal
               | intrinsic ;

(* Compound expressions *)
block_expr     = "{" block "}" ;
if_expr        = "if" expression "{" block "}" [ else_clause ] ;
else_clause    = "else" ( "{" block "}" | if_expr ) ;
match_expr     = "match" expression "{" { match_arm "," } [ match_arm ] "}" ;
match_arm      = pattern "=>" expression ;
pattern        = "_" | INTEGER | BOOL ;
while_expr     = "while" expression "{" block "}" ;
loop_expr      = "loop" "{" block "}" ;
return_expr    = "return" expression ;
array_literal  = "[" [ expression { "," expression } ] "]" ;
struct_literal = IDENT "{" [ field_inits ] "}" ;
field_inits    = field_init { "," field_init } [ "," ] ;
field_init     = IDENT ":" expression ;

(* Lexical elements *)
IDENT          = ( letter | "_" ) { letter | digit | "_" } ;
INTEGER        = digit { digit } ;
BOOL           = "true" | "false" ;
letter         = "a" | ... | "z" | "A" | ... | "Z" ;
digit          = "0" | "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" ;

(* Whitespace and comments are ignored between tokens *)
whitespace     = " " | "\t" | "\n" | "\r" ;
line_comment   = "//" { any_char_except_newline } newline ;
```
