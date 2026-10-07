# Rue Language Brief

Terse machine-readable summary for LLM code generation. Every construct, operator, keyword,
type rule, and semantic constraint.

---

## Lexical

- `//` line comments (no block comments `/* */`)
- whitespace = space | tab | newline; ignored between tokens; multiple = single
- identifiers: `[a-zA-Z_][a-zA-Z0-9_]*`; `_` is wildcard (discards value, no binding, no storage,
  can't be read); `_prefixed` suppresses unused-warning, otherwise normal
- integer literals: decimal `42`, `1_000_000`, hex `0xFF`, octal `0o17`, binary `0b1010`;
  base prefixes lowercase; no dangling prefix; no invalid digit; underscores only after first digit
- byte literals: `b'c'` or `b'\n'`; single ASCII byte 0-255; is integer literal (defaults i32);
  escapes: `\\`, `\'`, `\n`, `\t`, `\r`, `\0`
- string literals: `"hello\n"`; escapes: `\\`, `\"`, `\n`, `\t`, `\r`, `\0`
- trailing commas allowed in all comma-separated lists (params, args, array literals, struct
  fields, enum variants, match arms, patterns)

## Keywords

- `fn`, `let`, `mut`, `if`, `else`, `while`, `loop`, `for`, `in`, `match`, `return`, `break`,
  `continue`, `true`, `false`, `struct`, `enum`, `impl`, `self`, `borrow`, `inout`, `drop`,
  `linear`, `pub`, `const`, `comptime`, `checked`, `unchecked`, `ptr`, `extern`, `yield`

## Reserved type names

- `i8`, `i16`, `i32`, `i64`, `u8`, `u16`, `u32`, `u64`, `isize`, `usize`, `bool`, `type`, `Self`
- `isize`/`usize` = pointer-width; all targets 64-bit → same as i64/u64; interchangeable

## Types

### Integer types

- `i8`(-128..127), `i16`(-32768..32767), `i32`(-2147483648..2147483647),
  `i64`(-9223372036854775808..9223372036854775807)
- `u8`(0..255), `u16`(0..65535), `u32`(0..4294967295), `u64`(0..18446744073709551615)
- overflow = runtime panic (exit 101); signed and unsigned
- literal default: `i32` if unconstrained, unifies across all uses in function body
  (Hindley-Milner); two uses requiring different types = compile error; negated literal
  for min signed value is legal (e.g. `let x: i8 = -128`)
- literal range validated at compile time; out-of-range → error
- `@intCast(expr)`: convert integer types; infers target type from context; wraps on overflow
  in unchecked only (safe: traps at runtime as of RUE-1003)

### Boolean

- `bool`: `true` | `false`; 1 byte (0/1); `==` `!=` only (no `<` `>`)

### Unit and Never

- `()`: unit type, one value `()`; zero-sized; fn without return type → `()`
- `!`: never type; diverging expressions (`return`, `break`, `continue`, infinite loop,
  `@panic`); coerces to any type (sole coercion in Rue); zero-sized
- block with `!` tail → `!`; `loop` without `break` → `!`

### Structs

- `struct Name { field: Type, ... }`; field access `.field`; init `Name { field: val, ... }`;
  fields matched by name (order-independent); field-init shorthand `{x}` = `{x: x}`
- all fields must be initialized; field names unique
- layout: implementation-defined compact layout (ADR-0052); fields in declaration order;
  natural alignment per scalar; `@size_of`/`@align_of`/`@offset_of` report actual layout;
  never hand-compute offsets
- `@copy struct`: Copy type (use duplicates, original stays valid); all fields must be Copy
- `linear struct`: must be explicitly consumed; can't be dropped implicitly; must be consumed
  on all control-flow paths; can't be `@copy`
- `@repr(c)`: guarantee C psABI layout; gated behind `--preview c_ffi`; fields must be
  C-compatible (scalars, raw pointers, `@repr(c)` nested structs, fixed arrays of eligible)
- empty struct = zero-sized type

### Enums

- `enum Name { V1, V2(T, U), V3 }`; discriminants + optional tuple payloads
- pattern matching on enums; exhaustiveness required
- discriminant-only (C-like) enum = Copy; payload-carrying enum = move type unless all
  payloads are Copy (join over variants)
- enum variants accessed as `EnumName.Variant` or `E.Variant` via `let` binding

### Strings

- `str`: read-only byte-string slice {ptr, len}; Copy; conventionally UTF-8 (not enforced at
  runtime); string literals → `str`
- `Str(N)`: fixed-capacity inline string buffer; Copy; N = compile-time constant
- `StrBuf`: growable heap string {ptr, len, capacity}; move type (affine); has destructor
  (frees heap when capacity>0); imported from `std.strbuf.StrBuf`
- `s[i]`: byte at offset i → `u8`; O(1); traps if i >= len
- `s.substring(start, len)`: new `StrBuf` copy of byte range; borrows self; traps if range
  invalid
- `+` on StrBuf: new heap `StrBuf` = `s1 ++ s2`; both borrowed; only `StrBuf + StrBuf`
- `s.contains(borrow needle) -> bool`; `s.starts_with(borrow prefix) -> bool`; byte-level
- `s.chars()`: iterate Unicode scalars (u32); strict decode, trap on invalid byte
- `s.chars_lossy()`: iterate scalars, U+FFFD replacement for invalid
- `@to_string(n)`: integer → decimal `StrBuf`; any integer type; heap-allocated
- `print(s)`: raw bytes to stdout, no newline; `println(s)`: + trailing newline; borrows
- `StrBuf.push_str(borrow s)`: append bytes in-place (grows if needed); `StrBuf.clear()`
  sets len=0; `StrBuf.clone(borrow self)`: deep copy; `s.byte_at(borrow self, i) -> Option(u8)`
- `StrBuf` from literal (`let s: StrBuf = "x"`): capacity=0, no heap; grows on first push

### Array types

- `[T; N]`: fixed-size contiguous array; N = compile-time integer (literal, const, comptime
  param, comptime-evaluable call)
- stored ascending: element i at offset `i * size_of(T)`; size = `N * size_of(T)`;
  `[T; 0]` = zero-sized
- list literal: `[a, b, c]`; elements must have same type; count must match declared size
- repeat literal: `[value; count]`; value type must be Copy; value evaluated once
- `arr[i]` value context: copies (Copy) or moves (move) element
- `arr[i]` place context: assignment target
- index must be integer; constant index → compile-time bounds check; variable index →
  runtime bounds check; out-of-range → trap exit 101

### Move semantics

- Copy types: all integers, bool, `()`, `str`, `Str(N)`, discriminant-only enums, `[T; N]`
  where T is Copy, `@copy` structs
- Move types (affine): structs (default), `StrBuf`, payload-carrying enums (unless all
  payloads Copy), `linear` structs
- Value context use → copies Copy type, consumes move type (binding invalidated)
- Place contexts (assignment target, field/index base, `borrow`/`inout` arg, `==`/`!=`):
  no consumption
- Partial move: field projection or const array index moves only that sub-place;
  parent struct invalidated until all fields reinitialized
- `linear` struct must be explicitly consumed; can't be dropped; must be consumed on every
  control-flow path (divergent paths exempt); only consumption via function call or return;
  no `drop` declaration on linear types
- Destructor: `fn drop(inout self)` in `impl` block; called when value goes out of scope;
  called after field destructors; field destructors run in declaration order (top-down)

### Type inference

- Local only: one function body at a time; no cross-function inference
- Parameters, return types, const items MUST have explicit annotations
- `let` MAY omit annotation; type inferred from initializer and later uses
- Expected types propagate bidirectionally: let annotation, call param type, array context,
  return type → flows inward
- Integer literals: unification variable; if unconstrained at function end → i32
- Only coercion: `!` → any type; no implicit integer conversions

## Operators

### Arithmetic

- `+`, `-` (binary), `*`, `/`, `%`; `-` (unary negation)
- operands must be same integer type; result same type
- overflow → panic exit 101; division/remainder by zero → panic exit 101
- operator precedence: unary `-` > `*` `/` `%` > `+` `-`

### Comparison

- `==`, `!=`, `<`, `>`, `<=`, `>=`
- `==`/`!=` allowed on bool, integer, string, array, struct, enum (structural equality);
  struct fields compared in declaration order; no short-circuit on `==`
- `<` `>` `<=` `>=` only on integers; no chaining (`a < b < c` is `(a<b) < c`)
- `==`/`!=` on strings: byte-level equality (same length, same bytes)

### Bitwise

- `&`, `|`, `^`, `~` (not), `<<`, `>>`
- operands must be same integer type; result same type
- `~` unary; `<<`/`>>` shifts modulo bit-width; signed shift is arithmetic (sign-extending)
- precedence: `~` > `<<` `>>` > `&` > `^` > `|`; all below comparison, above logical

### Logical

- `!`, `&&`, `||`; operands must be `bool`; short-circuit (`&&`/`||`)
- `!` highest precedence; `&&` > `||`

### Assignment

- `=`, `+=`, `-=`, `*=`, `/=`, `%=`, `&=`, `|=`, `^=`, `<<=`, `>>=`
- statement only (not expression); target must be mutable (`let mut` or `inout`)
- compound assignment evaluates LHS once, then RHS; `a += b` = `a = a + b`
- assignment evaluates RHS first, then writes to LHS

## Expressions

### Literals

- integer: `42`, `0xFF`, `0o17`, `0b1010`, `1_000`; defaults i32
- byte: `b'a'`, `b'\n'`; integer literal subtype
- string: `"hello"`, `"with \"quotes\""`; type `str`
- boolean: `true`, `false`
- unit: `()`

### Block expressions

- `{ stmt; stmt; expr }`; value = tail expression or `()` if absent
- each statement ends with `;`; tail expression has no `;`
- creates new scope; bindings shadow outer; drops at scope end (reverse declaration order)

### If expression

- `if cond { then } else { else }`; `else if` chains
- condition must be `bool`; struct literal as condition prohibited (disambiguated by
  requiring `{}` after `if` body)
- both branches must agree on type; one may diverge (`!` coercing)

### Match expression

- `match scrutinee { pat => body, ... }`; exhaustive matching required
- patterns: integer/boolean literals, wildcard `_`, enum variant with binding
  (`Enum.Var(x, y)`), integer range `lo..hi`, struct pattern `Name { f1, f2 }`
- arms must agree on type; unreachable arms compile error
- scrutinee evaluated once; arms evaluated in declaration order; first match wins
- enum payload binding: `Enum.Var(a, b) => ...` destructures into local bindings
- struct pattern: `Point { x, y }` binds fields; shorthand when field name matches
- value context match: moves scrutinee if move type

### Loops

- `while cond { body }`: evaluates to `()`; condition must be `bool`
- `loop { body }`: infinite loop; evaluates to `!` without `break`, `()` with `break`
- `for pat in iterable { body }`: iterates over iterable; `iterable` can be:
  array (by value or borrow), `str` bytes (`u8`), `StrBuf` bytes (`u8`),
  `chars()` view (`u32` scalars), `chars_lossy()` view, `StrBuf.split(borrow sep)`,
  `[T; N].as_iter()` (returns array iterator)
- `break` [value]: exits loop; `break` with value sets loop result type (from `()` to T);
  all `break` values must agree on type
- `continue`: skip to next iteration
- loops may be labeled for `break label` / `continue label` targeting outer loops

### Return

- `return`; `return expr`; type `!`; value coerces to function return type
- bare `return` in `-> ()` function

### Function calls

- `f(a, b, c)`; arguments evaluated left-to-right
- argument types must match parameter types (exact or via expected-type inference)
- `borrow`/`inout` parameter modes visible at call site (_not_ required at call site:
  caller passes place, compiler borrows/inouts automatically); no explicit `&`/`&mut` at
  call site
- methods: `value.method(args)`; desugars to `Type.method(value, args)` or
  `Type.method(borrow value, args)` depending on receiver mode

### Field access

- `value.field`; accesses struct field; returns field type
- field access in place context → assignment target
- field access in value context → copies (Copy) or moves (move) field

### Index

- `arr[i]`; `s[i]`; index type must be integer; return element type
- constant index → compile-time bounds check; variable → runtime trap if out of range

## Statements

### Let

- `let name = expr`; `let mut name = expr`; `let name: Type = expr`
- mutable via `mut`; without `mut`, binding is immutable (value can't be reassigned)
- `_` wildcard: discards value, no binding, can't be read
- shadowing allowed in nested blocks

### Assignment

- `target = expr`; `target += expr` etc.; statement only
- target must be mutable: `let mut` binding, `inout` parameter, or array element / field
  of mutable binding
- compound assignment: RHS evaluated, then LHS read, operation, write back

### Expression statement

- expression followed by `;`; expression evaluated for side effects; value discarded
- expression type must be `()` or its value must be droppable (not linear)
- `!`-typed expression as statement: no discarding issue (diverges)

## Items

### Functions

- `fn name(p1: T1, p2: T2) -> RetType { body }`
- parameters: by-value (moves), `borrow` (shared read-only reference), `inout` (mutable
  reference, read-write), `comptime` (compile-time known)
- `borrow` params: read-only, can't mutate, can't move out, can read Copy fields;
  implicit at call site
- `inout` params: read-write reference; can mutate, can read Copy fields; implicit at
  call site
- return type required (or defaults to `()`)
- body: block expression; tail expression = return value; `return` for early exit
- `fn main() -> i32`: program entry point; exit code = main's return value
- functions are private by default; `pub fn` exports
- mutually recursive functions allowed (no forward-declaration needed; declaration order
  independent within module)

### Structs

- `struct Name { f1: T1, f2: T2 }`; `@copy struct`, `linear struct`
- `@repr(c) struct Name { ... }`; FFI layout guarantee; gated `--preview c_ffi`
- can't be empty for `@repr(c)`
- struct init: `Name { f1: val, f2: val }`; field order independent; field-init shorthand

### Enums

- `enum Name { V1, V2(T, U), V3 }`; variants can have tuple payloads
- payload-carrying enum = tagged union; discriminant-only = C-like
- payload variant constructor: `Name.V2(a, b)` (in expression position creates instance)
- enum equality: `==`/`!=` structural (discriminant + payloads)
- variant payload binding in match: `Name.V2(x, y) => ...`
- `pub enum` for exporting

### Impl blocks

- `impl Name { fn method(...) ... fn associated() ... }`
- methods: first param is `self`, `borrow self`, `inout self` (receiver)
- `self` = by-value (consumes); `borrow self` = shared reference; `inout self` = mutable ref
- `Self` = enclosing struct type inside impl
- method call: `value.method(args)` → `Type.method(value, args)` with correct borrowing
- associated functions: no self param; called as `Type::func()` via qualified path
- destructor: `drop(inout self)` in impl; called on scope exit; field destructors run in
  declaration order after `drop` body; can't be called explicitly; must not reference
  partially-destroyed fields
- separate method namespaces per impl block

### Constants

- `const NAME: Type = expr`; file-level only; comptime-evaluable initializer
- integer, boolean, string, unit literals; arithmetic; references to other constants
  (acyclic); array/struct literals (all elements comptime); fully-comptime function calls
- evaluation order: topological sort over dependency graph
- constants are inlined at use site (no address)

### Borrow accessors

- `fn field(borrow self) -> borrow T { ... yield place; }`; ADR-0062
- body runs in calling context (inlined); `yield` returns a borrowed sub-place of self
- accessor result must be a place rooted in the receiver or a comptime immutable value
- scoping: accessor can't yield a reference to local variable (E0241); can't return a value
  that requires drop glue (E0258); must be acyclic (E0261)

## Arrays
- `[T; N]`: type syntax; N = comptime integer
- literal list: `[1, 2, 3]`; count must match N
- literal repeat: `[0; 100]`; value must be Copy
- indexing `arr[i]`; bounds checked at compile time (const index) or runtime (variable)
- mutable: `let mut arr: [i32; 3] = [1,2,3]; arr[0] = 42`
- array in `for`: `for x in arr { ... }`; by-copy iteration (Copy element type)
- arrays compare with `==`/`!=` (element-wise structural equality in order)
- nested: `[[i32; 3]; 4]` is 4×3 array
- array literal type must match declared type; `[1, 2]` where `[i64; 2]` expected:
  context uses expected-type inference for elements

## Comptime

### Comptime blocks

- `comptime { ... }`: evaluated at compile time; result replaces block
- supports: integer/boolean literals, arithmetic, comparisons, logical, bitwise,
  `let` bindings, references to const/comptime params, fully-comptime calls
- errors: runtime variables, non-comptime calls, operations that would panic at runtime

### Comptime parameters

- `fn f(comptime n: i32, x: i32) -> i32`; compiler requires compile-time-known argument
- enables monomorphization: each unique combination of comptime args → specialized function
- `comptime T: type`: T is a type; caller passes concrete type name; substituted everywhere
  in params/return
- type param can appear in arrays, pointers, nested composites: `[T; N]`, `ptr const T`,
  `[[T; 2]; 3]`
- compile-time `if`/`match` over comptime-known values: only taken branch analyzed;
  enables comptime-recursive functions
- specialization depth limit: min 64, exceeding → compile error

### Comptime-generic struct/enum returning `type`

- fn returning `type` can construct anonymous struct/enum: `fn Pair(comptime T: type) ->
  type { struct { first: T, second: T } }`
- producer-nominal: each evaluation under same specialization → same type; different
  specializations → different types
- anonymous struct fields may reference comptime parameters in scope
- anonymous enums: `enum { V1, V2(T) }` where T is comptime param
- type constructor expressions: `let P = Pair(i32); let p: P = P { first: 1, second: 2 }`

## Try operator `?`

- postfix `?` on trusted `Option` or `Result`
- operand must be `std.option.Option(T)` or `std.result.Result(T, E)` (exact
  specialization); user-defined lookalikes rejected
- enclosing function must return same trusted producer type
- `Option`: `operand?` → `T` if `Some(v)`, short-circuits to `return None` if `None`
- `Result`: `operand?` → `T` if `Ok(v)`, short-circuits to `return Err(e)`; error type
  must match exactly
- `@read_line()?` and `@parse_i64(s)?` work: intrinsics already return trusted Option
- `?` binds as tightly as other postfix operators

## Intrinsics (`@name(args)`)

### Expression intrinsics (any expression position)

- `@dbg(x)`: borrows x; prints value + newline; int (base 10, signed), bool (true/false),
  StrBuf (raw bytes); return `()`
- `@size_of(T)`: compile-time; type size in bytes → `i32`
- `@align_of(T)`: compile-time; type alignment → `i32`
- `@offset_of(T, field)`: compile-time; field byte offset in struct → `u64`
- `@intCast(expr)`: convert integer types; infers target from context
- `@wrapping_add(a, b)`, `@wrapping_sub(a, b)`, `@wrapping_mul(a, b)`: modular arithmetic,
  never panics; same integer type in/out
- `@to_string(n)`: any integer → heap-allocated `StrBuf` decimal
- `@drop(expr)`: run destructor and consume value → `()`; RUE-187
- `@read_line()`: read stdin line → `Option(StrBuf)`; requires `--preview io`
- `@parse_i32(s)`, `@parse_i64(s)`, `@parse_u32(s)`, `@parse_u64(s)`: parse text →
  `Option(i32)` etc.; requires `--preview io`
- `@parse_i32_base(s, base)`, `@parse_i64_base(s, base)`, etc.: parse in base 2-36
- `@random_u32()`, `@random_u64()`: random number; requires `--preview random`
- `@arg_count()`: number of command-line args → `i32`; requires `--preview io`
- `@arg(n)`: nth command-line arg → `Option(StrBuf)`; requires `--preview io`
- `@panic(msg?)`: abort program; msg must be comptime string or omitted; return `!`;
  exit code 101
- `@assert(cond)`: compile error if cond is comptime false; runtime trap (exit 101) if
  cond is runtime false
- `@cast(expr, T)`: bitcast; same-size types only; `--preview unchecked` required;
  placeholder for future safe transmute
- `@field_ptr(place)`: raw pointer to a field of an `inout`/`ptr` place; unchecked only
- `@ptr_offset(ptr, count)`: advance pointer by count elements; unchecked only
- `@ptr_read(ptr)`: read value from raw pointer; unchecked only
- `@ptr_write(ptr, value)`: write value to raw pointer; unchecked only
- `@byte_read(ptr, offset)`: read byte at pointer+offset → `u8`; unchecked only
- `@byte_write(ptr, offset, byte)`: write u8 at pointer+offset; unchecked only
- `@alloc_bytes(size)`: allocate heap memory → `[u8; size]` raw; unchecked only
- `@realloc_bytes(ptr, old_size, new_size)`: reallocate; unchecked only
- `@free_bytes(ptr, size)`: deallocate; unchecked only

### Directives (before item/statement)

- `@allow(warning, ...)`: suppress warnings on next item/statement;
  warnings: `unused_variable`, `unused_function`, `unreachable_code`, `unreachable_pattern`
- `@copy`: before `struct` → Copy type; no args
- `@repr(c)`: before `struct` → C layout guarantee; `--preview c_ffi`

## Modules

- `const name = @import("path")`: import module; path relative to importing file;
  `.rue` extension optional
- `@import` returns module scope as a namespace value
- `pub`: visibility modifier on fn, struct, enum, const; private by default; only pub
  items accessible from importing modules
- modules are files; no `mod` keyword; every `.rue` file is a module
- re-export: `pub const thing = @import("other.rue").thing`
- import binding: `const module = @import("module")`; `pub` makes it visible to importers
- module bindings can be `const` or `pub const`; no `mut` at module level
- program composition: compiler receives one root `.rue` file; its transitive `@import`
  graph is the program; no second positional `.rue` argument
- circular imports: compile-time error

## Unchecked code

- `unchecked fn name(...) -> T { ... }`: function body operates under relaxed rules;
  raw pointers, unsafe intrinsics allowed
- `checked { ... }` block: inside unchecked fn, restore safety guarantees for a block;
  safe code can appear inside checked
- raw pointer types: `ptr const T` (read-only), `ptr mut T` (read-write)
- pointer can be null (value `0`); dereferencing null → undefined behavior (not trapped)
- pointer arithmetic: `@ptr_offset(ptr, n)`; advance by n elements
- reading through pointer after its memory is freed → undefined behavior
- `@ptr_read`, `@ptr_write`: raw memory access; no ownership tracking
- `@alloc_bytes`, `@realloc_bytes`, `@free_bytes`: manual heap management
- `@byte_read`, `@byte_write`: byte-level access through pointer+offset
- C FFI: `extern "C" { fn name(params) -> Ret; ... }`; C calling convention;
  `--preview c_ffi` required
- `@repr(c)` struct + `extern "C"` function: FFI boundary; three FFI-safety predicates
  (layout, function, call-site)

## Runtime behavior

- integer overflow (signed and unsigned) → panic exit 101
- division/remainder by zero → panic exit 101
- array/string index out of bounds → panic exit 101
- `@panic` → abort exit 101; `@assert` false → trap exit 101
- allocation failure → panic exit 101 (`--preview alloc_panic`; future: fallible alloc)
- SIGPIPE (writing to broken pipe) → terminate with signal (not panic)
- checked arithmetic: all operators trap on overflow
- bounds checks: implementation may eliminate provably-redundant checks; must preserve
  first-fault semantics (no introduced trap, no removed trap, no reordering across
  observable effects)

## Preview features

- gated behind `--preview <name>` flag
- current gates: `c_ffi` (extern + @repr(c)), `io` (@read_line, @parse_*, @arg, @arg_count),
  `random` (@random_u32, @random_u64), `unchecked` (@cast, @ptr_*, @alloc_*), `alloc_panic`
  (allocation failure → panic), `test_infra` (test infrastructure, not language)
- new language features require preview gate until complete (ADR-0005)

## Undefined behavior (unchecked code only)

- null/misaligned pointer dereference; use-after-free; double free; incorrect
  size/alignment to alloc/realloc/free; data race; invalid bool byte (not 0/1);
  out-of-bounds pointer arithmetic (past one-past-the-end); reading uninitialized memory
  through raw pointer; accessing field through wrong-type pointer; violating borrow
  exclusivity in unchecked; @byte_read/@byte_write at non-live byte
- safe Rue: no undefined behavior; UB confined to `unchecked`

## Exit codes

- main return value → process exit code (truncated to 0-255)
- panic (overflow, div0, bounds, @panic, @assert false) → exit 101

## Implementation limits (reference compiler)

- source file: max 4,294,967,295 bytes
- files per compilation: max 4,294,967,295
- single object/array type: max 2,147,483,647 bytes (i32::MAX)
- per-function cumulative storage: max 2,147,483,632 bytes
- syntactic nesting depth: 256 (E0482)
- IR instructions/payloads/elements/fields: all bounded by u32 indexing
- specialization depth: min 64 (E0482)
