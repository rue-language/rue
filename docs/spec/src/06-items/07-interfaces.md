+++
title = "Interfaces"
weight = 7
template = "spec/page.html"
+++

# Interfaces

{{ rule(id="6.7:1", cat="informative") }}

An *interface* names a set of member requirements: methods, associated
functions, and type-valued associated constants. A type *conforms* to an
interface only when a *conformance assertion* says so, and an assertion is
verified against the type's inherent members (6.4, 6.5); an assertion carries
no bodies of its own. A comptime type parameter (4.14) may name interfaces as
its *bound*, in which case every type argument bound to it must conform to
each of them. Interfaces are a preview feature: every construct in this
section requires `--preview interfaces` (6.7:3).

## Preview gate

{{ rule(id="6.7:2", cat="syntax") }}

```ebnf
interface_def   = [ "pub" ] "interface" IDENT [ ":" interface_list ]
                  "{" { interface_member } "}" ;
interface_list  = interface_ref { "+" interface_ref } ;
interface_ref   = IDENT { "." IDENT } ;
interface_member = interface_const | interface_fn ;
interface_const = "const" IDENT ":" "type" ";" ;
interface_fn    = "fn" IDENT "(" [ interface_params ] ")" [ result ] ";" ;
interface_params = [ "inout" | "borrow" ] "self" [ "," params ] | params ;
conformance_decl = type "is" interface_list ";" ;
struct_conformance = "is" interface_list ;   (* between the struct name and "{" *)
struct_assoc_type = [ "pub" ] "const" IDENT "=" type ";" ;   (* in a struct body, after fields *)
interface_bound = "comptime" IDENT ":" interface_list ;
```

{{ rule(id="6.7:3", cat="legality-rule") }}

An `interface` item, a conformance assertion (freestanding or in a struct
header), or an interface bound on a comptime parameter **MUST NOT** appear in a
program compiled without the `interfaces` preview feature. Each such use
produces the preview-feature diagnostic (E1100) naming `interfaces`.

## Interface declarations

{{ rule(id="6.7:4", cat="normative") }}

An interface declaration introduces a named interface at module scope. Its
name occupies the module's type namespace: it **MUST** be unique among the
module's user-defined type and interface names (6.0:2), and it is visible to
other modules only when declared `pub` (10.3).

{{ rule(id="6.7:5", cat="normative") }}

An interface body lists *requirements*. A method requirement is a bodiless
function signature whose first parameter is `self` with an access mode of
`borrow`, `inout`, or by-value (no mode). An associated-function requirement
is a bodiless signature without `self`. A type-valued associated constant
requirement `const Name: type;` names a type the conforming type must supply.
Within an interface body, `Self` denotes the conforming type, and the name of
an associated constant requirement denotes the conforming type's value for it.

{{ rule(id="6.7:6", cat="legality-rule") }}

Requirement names within one interface **MUST** be distinct. A requirement
**MUST NOT** carry a body, a `pub` modifier, or directives. An interface
**MUST NOT** be empty: it declares at least one requirement, so that
conformance is always a claim about members rather than a marker.

{{ rule(id="6.7:7", cat="normative") }}

An interface **MAY** refine one or more other interfaces with
`interface Name: Parent + Other { ... }`. A type that conforms to a refining
interface must also conform to each refined interface (6.7:12). Refinement
**MUST** be acyclic.

{{ rule(id="6.7:8", cat="example") }}

```rue
interface Equatable {
    fn equals(borrow self, borrow other: Self) -> bool;
}

interface Sequence {
    const Element: type;
    fn next(inout self) -> Option(Element);
}

interface Collection: Sequence {
    fn len(borrow self) -> u64;
}
```

## Conformance assertions

{{ rule(id="6.7:9", cat="normative") }}

A conformance assertion states that a type conforms to one or more
interfaces. It is written either freestanding at module scope,
`Type is Interface + Other;`, or in a struct header,
`struct Name is Interface + Other { ... }`. Both forms have the same meaning.
The freestanding form **MAY** name a type declared in another module,
including a primitive type and a standard-library type, so conformance can be
asserted retroactively.

{{ rule(id="6.7:10", cat="legality-rule") }}

An assertion is verified against the asserted type's inherent members. For
each requirement of the interface (and, transitively, of every interface it
refines):

- a method requirement is satisfied only by an inherent method of the same
  name whose receiver mode, parameter count, parameter modes, parameter types,
  and result type equal the requirement's after substituting the asserted type
  for `Self` and the type's associated-constant values for the interface's
  associated-constant names;
- an associated-function requirement is satisfied only by an inherent
  associated function of the same name under the same comparison;
- a type-valued associated constant requirement is satisfied only by an
  associated type declaration `pub const Name = Type;` of the same name in the
  type's body (6.7:2), whose right-hand side is a type.

An assertion is verified whenever it is relied on to satisfy a bound
(6.7:15); an implementation **MAY** also verify assertions eagerly. If any
requirement is unsatisfied the assertion is a compile-time error, reported at
the assertion, that
names the type, the interface, and every unsatisfied requirement, and — for
a member that exists with the wrong signature — the requirement's expected
signature and the member's actual signature.

{{ rule(id="6.7:11", cat="normative") }}

An assertion carries no implementation. It cannot supply, rename, or adapt a
member; a type whose member has the wrong name or signature does not conform
until the type itself changes, or a wrapper type is introduced. Consequently
two assertions of the same fact are identical: repeating an assertion is
permitted and has no additional effect, and there is no notion of two
conflicting conformances of one type to one interface.

{{ rule(id="6.7:12", cat="legality-rule") }}

An assertion that a type conforms to a refining interface also asserts
conformance to each interface it refines, and every such conformance is
verified by 6.7:10 as part of the same assertion.

{{ rule(id="6.7:13", cat="example") }}

```rue
struct Range is Sequence {
    cur: i64,
    end: i64,
    pub const Element = i64;
    fn next(inout self) -> Option(i64) {
        if self.cur >= self.end { return Option(i64).None; }
        let v = self.cur;
        self.cur += 1;
        Option(i64).Some(v)
    }
}

i64 is Equatable;   // error when relied on: `i64` has no inherent method `equals`
```

## Interface bounds

{{ rule(id="6.7:14", cat="normative") }}

A comptime type parameter (4.14:5) **MAY** declare an interface bound in place
of `type`: `comptime T: Interface + Other`. The parameter still takes a type
argument (4.14:6). The bound is part of the function's signature and is
displayed with it.

{{ rule(id="6.7:15", cat="legality-rule") }}

At every call that binds a type argument to a bounded comptime parameter,
the argument type **MUST** conform to every interface in the bound; that is,
a conformance assertion for the argument type and each interface (or an
interface refining it, 6.7:12) must exist in the program. Otherwise the call
is a compile-time error at the call site that names the argument type, the
parameter, and each interface it does not conform to. Verification of the
assertion itself (6.7:10) is reported at the assertion, not at the call.

{{ rule(id="6.7:16", cat="normative") }}

A bound does not change how the callee's body is analyzed: as for every
generic function (4.14:25), the body is analyzed per specialization, with the
type argument substituted for `T`. Within the body, `T` denotes the argument
type, and the bound does not restrict which of the argument type's members the
body may use. (A future revision may check the body once against the bound;
see the design record.)

{{ rule(id="6.7:17", cat="example") }}

```rue
fn contains(comptime T: Equatable, borrow xs: ArrayBuf(T), borrow x: T) -> bool {
    let mut i: u64 = 0;
    while i < xs.len() {
        if xs.get_or(i, x).equals(borrow x) { return true; }
        i += 1;
    }
    false
}

struct Id is Equatable {
    n: i64,
    fn equals(borrow self, borrow other: Id) -> bool { self.n == other.n }
}

// contains(Id, borrow ids, borrow needle)   // OK: Id is Equatable
// contains(i64, borrow nums, borrow 3)      // error: i64 does not conform to Equatable
```

## Name resolution

{{ rule(id="6.7:18", cat="normative") }}

An interface is named like a type: by its identifier in the declaring module,
or through a module binding (`m.Interface`, 10.4) when it is `pub`. An
interface name is not a type: it **MUST NOT** appear where a type is required
(a binding annotation, a field type, a parameter type other than the
`comptime` bound position, or a return type). An interface **MAY** be
re-exported through a public constant like any other declaration (6.5).
