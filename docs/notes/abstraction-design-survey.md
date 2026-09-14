# Abstraction in Rue: a survey of the design space and a recommendation

*Prepared 2026-09-01 for Steve Klabnik and Dorian, at Steve's request, as input to the RUE-246 ruling. This is an analysis, not a decision; the family fork and the v1 scope are the maintainers' call. Where I state a recommendation I also state what would change my mind.*

## 0. Summary

Rue needs a way to abstract over behavior. Today the only behavior a generic body can rely on is the built-in operator set on a comptime type parameter, so `std/sort.rue` hard-codes `<`, the maps are Copy-value-only, `binary_heap` cannot take a comparator, and the 105K-line Caldera program encodes 416 behaviors as statically generated registries because it cannot pass a function. The corpus evidence (§2) says the pressure is concrete and comes in two distinct shapes: *pass a behavior* (comparators, predicates, visitors) and *abstract over a type's capabilities* (equality, ordering, hashing, display, iteration, formatting).

My recommendation, in one paragraph: adopt **two small features rather than one large one**. First, second-class function parameters (RUE-643): a `fn`-typed parameter that can be called but not stored, returned, or captured. It is the cheapest abstraction win Rue can buy, it mirrors the second-class loan philosophy exactly, and it removes the comparator/predicate/visitor class of duplication without touching the type system. Second, **interfaces with declared, structurally verified conformance and no implementation bodies**, checked at the definition site by skolemization. This is the design the July checkpoint on RUE-246 converged on, and after surveying Carbon, Swift, Hylo, Zig, Go, Rust, Mojo, Austral, C++ concepts, and the LLM-repair literature I think it is the right point in the space for Rue specifically, for reasons that are Rue-specific: Rue is already parametric by law (no comptime reflection, `T == i32` does not reduce), it has no lifetimes (so the hardest parts of Rust's trait system have nothing to attach to), its dispatch is already monomorphization, and its authors are mostly agents, for whom non-local errors and coherence puzzles are the most expensive failure class. I would explicitly **decline** implementation bodies in conformances, blanket impls, default methods, specialization, and impl-based inference, which is where Rust's complexity lives, and I would explicitly **reserve** existentials (`dyn`) in a second-class form, because the single most important lesson from Zig's 2025 I/O rewrite is that behavioral interfaces used at API boundaries want a concrete vtable form eventually.

I would **not** lean further into Zig-style comptime for behavioral abstraction. Rue already has the good half of Zig (types as values, one generics syntax, monomorphization). The other half, intensional reflection with duck typing, is the half Zig's own standard library just retreated from for its most important interface, the half Zig's core team refuses to make safer with constraints, and the half that produces the error class agents handle worst. Keeping "types are opaque to comptime control flow" as a spine ruling costs nothing today (zero violating code in the corpus) and is what makes definition-site checking cheap.

On compile time: the design above adds bounded, memoizable work. Bound satisfaction is one members-diff per (type, interface) pair. Definition-site checking adds at most one extra instantiation per generic definition per bound signature; std has 38 generic definitions, and the largest program in the corpus applies only 14 distinct type constructors. There is no solver, no search, and no new analysis mode. §8 gives the cost model and the experiment I would run on the existing performance harness before ratifying.

## 1. What is being decided, and what is not

RUE-246 already fixed several things that narrow this decision, and it is worth restating them because they remove most of the design space:

- **Dispatch is static.** ADR-0025 makes monomorphization the mechanism; the formal core is first-order and monomorphic. Any interface mechanism lives in the elaboration layer and leaves the six memory-safety theorems untouched. Dynamic dispatch, if it ever comes, is a separate feature.
- **Rue is parametric today.** Comptime `==` never reduces for `type` operands (spec 4.14:26-29), there is no `@typeInfo`/`@hasDecl`, `@size_of` is not comptime-evaluable, and comptime `if`/`match` branch only on Bool/Integer values. The compiler agent confirmed this in the current source: `ConstValue` is `Integer | Bool | Type | Function | String | Unit`, and `type` values are opaque (`crates/rue-air/src/sema/context.rs:838-872`, `comptime.rs:1656-1680`). The corpus has zero code that inspects a type.
- **Loans are second-class.** No lifetimes means no GATs, no variance, no lifetime bounds. RUE-219's `get(borrow self, p) -> borrow Element` is a lending iterator in Rust and a plain method with an access mode in Rue.
- **Multiplicity is a lattice, not a trait.** `Copy`/`Affine`/`Linear` are properties on the type, so the marker-trait family (`Copy`, `Send`, `Sync`) does not need an interface mechanism.
- **The acceptance tests are known.** RUE-219 layer 2 (`Sequence`/`Collection` with an associated `Element`, an `inout self` method, a `Position: Copy` lattice bound, and a projection-returning `get`), RUE-6's `From`-style error widening (a receiverless associated function), RUE-350's `Display` seam (one method), ADR-0043's allocator parameter, and spec 4.3's equality refinement.

What remains open is exactly three questions:

1. **How is a bound satisfied?** Implicitly by structure (Go, C++20 concepts, Zig duck typing), by declaration checked against structure (TypeScript `implements`, C++0x explicit concept maps without adaptation, the July sketch), or by declaration with implementation bodies (Rust, Swift, Carbon, Hylo, Mojo, Austral, Haskell).
2. **When is a generic body checked?** At instantiation (Zig, C++, D, current Rue) or at definition (Rust, Swift, Carbon, Hylo).
3. **What is the complexity budget?** Which of associated types, associated constants, conditional conformance, default methods, blanket conformance, specialization, operator overloading, existentials, and reflection are in, out, or reserved.

The July checkpoint (Steve and Fable, 2026-07-15/16; Dorian's review 2026-07-18) answered (1) as "declared, structurally checked," leaned toward definition checking on (2) via skolems, and left (3) partly open. This report re-examines all three against outside evidence and the compiler as it exists today, and proposes a v1 budget.

## 2. What the corpus is actually paying for

Before comparing mechanisms it is worth knowing what repetition exists, because the options remove different things. A census of every `.rue` file in `std/` and `examples/` (scripts in the session scratchpad; counts are of the "organic" 49K lines, excluding the 104K generated by `scripts/generate-caldera.py` and the 41K of skeleton-identical meridian/lattice analysis families, unless stated).

**Behavior that cannot be passed.** Ten comparison sorts exist, seven of them only because a key or comparator cannot be passed (`std/fs.rue:1572`, `gazette/site.rue:666` and `:777`, `lattice/query.rue:87`, `meridian/engine.rue:37`, and so on). `std/binary_heap.rue` contains `PriorityQueue` as a copy of `BinaryHeap` with one comparison swapped, 50 lines of `_sift_down` duplicated, and a header saying a comparator needs RUE-643. Forty-four function pairs differ in at most four lines and those lines are a comparison or a field access (`harbor/operator_checklist.rue:179/190/212` is a count-where with three predicates written three times). Thirty functions are the exact `while i < xs.len() { if a == b { return i } }` shape, matching on a field, because `ArrayBuf.index_of` can only match a whole element. In the templated families the same shape reaches its limit: `observe_operations` appears 112 times, 2,576 identical lines, differing only in a projection expression that would be a closure argument in any language with function values.

**Capabilities that cannot be abstracted over.** Three hash maps (`std/intmap.rue`, `std/strmap.rue`, `examples/hashmap`) whose `get` and `remove` are over-0.8-similar copies; both std maps say in their header that `HashMap(K, V)` waits on function values or traits. Seven string interners totalling 778 lines, all using the byte-pool plus parallel `offs/lens/hashes` layout because there is no `Hash`/`Eq` for a generic key and `ArrayBuf(StrBuf)` reads are gated. `File` and `TcpStream` duplicate `read`, `write`, and `write_all` (227 lines); `std/net.rue:154` says "without a trait abstraction." Hashing: 5 struct `hash` methods, 22 free hash functions, and FNV mixing inlined on 65 lines in 63 files, while `std.hash` has two call sites. Fourteen `same()`-style wrappers around `StrBuf` equality serve 237 call sites, each so a literal can be compared without binding and borrowing it.

**Interfaces that exist as conventions.** Only 71 of 368 hand-written structs have any method at all. The real interfaces are module-level free-function surfaces: `render(...) -> StrBuf` in 384 free functions, `analyze`/`verify`/`render` in 161 analysis modules, `TAG`/`MODE`/`analyze`/`verify`/`render` in 256 Caldera audits, `BEHAVIOR`/`PROFILE`/`evaluate` in 128 behaviors, `RULE`/`POLICY`/`evaluate` in 96 rules. Because a module is not a value and a function cannot be stored, every suite enumerates its members by hand: `meridian/analysis_suite.rue` is 598 lines of imports and calls, `caldera/audit_suite.rue` 1,302; about 5,100 lines of pure enumeration across the corpus. This is the Caldera evidence attached to RUE-643, and it is worth noting that a second-class function *parameter* does not solve it: a registry is a *stored* table. What solves it is a comptime-known table of function references in a `const`, which needs no closures and no runtime function values in structs.

**Dispatch by tag.** `match` is barely used for dispatch (325 of 353 matches are on `Option`/`Result`). The hand-written vtable is the else-if chain on an integer kind: 90 chains of three or more branches, 27 with eight or more, the largest 54 branches (`ruelex/ast.rue:116`). Integer kinds are chosen over payload enums because AST nodes must be flat POD in an `ArrayBuf` and owned enum payloads cannot be read by copy, so this is downstream of the element-access gap rather than a missing `dyn`.

**Iteration.** 2,374 `while i < ....len()` index loops against 2 `for` loops in the whole hand-written corpus. Inside them, 3,821 `get_or` calls, 3,058 of which pass an `empty_x()` filler built by 58 filler constructors (299 lines). `get_ref`, the borrow-returning accessor, is used three times.

**The alias tax.** 264 file-level and 158 function-local aliases for `ArrayBuf(...)`, `Option(...)`, and `Result(...)`; 321 match arms need the alias-qualified variant. `U64s = ArrayBuf(u64)` is declared 39 times.

**What each feature would remove** (organic lines, rough, with the census author's confidence):

| Feature | Removes or simplifies | Confidence |
|---|---|---|
| Iteration protocol with element-borrowing `for` | about 5,000 lines: loop ceremony, 3,058 filler arguments, 58 filler constructors, and the parallel-array pressure | high |
| Function values (parameters plus const tables) | 700 to 900 organic lines (sorts, heap copy, comparator pairs, field-key searches); roughly 18K in the meridian/lattice families and the 5,100 lines of suite enumeration | high that it is mechanical |
| Interfaces with bounded generics | about 1,300 lines: one `HashMap(K, V)`, one interner, `Read`/`Write` for `File`/`TcpStream`, one `Hash`; plus the same suite enumeration if modules can conform | high |
| Operator overloading via interfaces | few lines, but it is the only fix for the 237-site `same()` idiom and the 3,821 `get_or` sites | medium |
| Comptime reflection | an alternative route to the suite tables; overlaps interfaces | medium |
| Variant inference, `const` idiom docs, a shared example package | about 3,700 lines of alias and copy tax; not abstraction | medium |

Two conclusions. First, the two pressures are separable, and the cheaper one (function values) removes the larger share of the *hand-written* duplication. Second, by line count the biggest single win is not an abstraction mechanism at all but element-borrowing iteration, which the interface design has to be shaped to deliver (§9.3) but which does not have to wait for it.

## 3. Rue as it exists today (the substrate any design lands on)

Facts verified against trunk on 2026-09-01, with file references for the ones that constrain the design.

**Generics.** A generic is a function with `comptime T: type` parameters; a generic type is a comptime function returning an anonymous struct or enum (`std/arraybuf.rue:95`). Type arguments are always explicit and are substituted *before* runtime arguments are inferred (spec 3.11:11). std has 38 generic definitions; the examples corpus has 19 files that define one. There are no bounds of any kind. The only capability a body can assume of `T` is the built-in operator set, so `std/sort.rue` documents itself as ordering "by the natural `<` relation" and passes a literal `0` as the out-of-bounds filler, which restricts it to integer-like `T`.

**Checking.** A generic body is analyzed only per specialization (`provider_body_host.rs:6576`, "a generic free function reaches analysis only through its specializations"). At declaration the signature is built with every `T`-mentioning type replaced by a placeholder `Type::COMPTIME_TYPE` and nothing else is checked. An unreferenced generic is never analyzed. A misuse of `T` inside the body surfaces as E0413/E0411 "no method named ... on type ..." with the span in the generic body and **no instantiated-here label** (the only instantiation-site label in the compiler is for `ContainerElementIsLinear`). This is the Zig error shape.

**Instances.** Specializations are keyed by a digest-carrying `FunctionInstanceKey::Specialization { base, arguments }` (`semantic_identity.rs:481-492`), deduplicated by the reachability scheduler, bounded by depth 64 only (RUE-1098 is the open cardinality question), and durable per body across revisions. Anonymous-type methods are separate bodies keyed per type instance. In cold Lattice, free specializations are about 1.5% of instructions and anonymous-method bodies about 1.2%; the identity cluster (minting anonymous types, building signatures) is 11.4%. Breadth is small in practice: Caldera applies 14 distinct type constructors, Meridian 16, Harbor 44.

**Operators and built-in protocols.** Arithmetic and ordering are intrinsics on integers (`builtin_ops.rs:318-327`); `==` is structural on aggregates (spec 4.3:3b) and the spec defers trait-based refinement to RUE-246. `@to_string` is integer-only; hashing is byte-only and `std/hash.rue` says in capitals that there is no `Hash` trait. Drop is the one type-directed protocol (`drop fn`, drop glue keyed `FunctionInstanceKey::DropGlue`). The multiplicity predicates `@require_droppable` and `@require_trivially_droppable` are ordinary intrinsics and are, today, the closest thing to a bound: instantiation-time structural class checks.

**Function values.** None. `ConstValue::Function` is a callable alias for re-exports only; materializing one errors "function references cannot exist at runtime" (`instructions.rs:332-337`). `std/binary_heap.rue`, `std/strmap.rue`, and `std/intmap.rue` each carry a comment saying a comparator or a generic `HashMap(K, V)` is blocked on RUE-643 or RUE-246.

**Trusted std.** Three privileges gate on `is_trusted_standard_library`: borrow-returning accessors on anonymous types (`get_ref`), the `@place` bridge inside a trailing `yield checked {}`, and the exact `Option`/`Result` producers that `?` binds to. A user-written container can replicate `ArrayBuf` except for `get_ref` and `@place`. Any interface design has to say whether interface-typed code can reach these privileges; the iteration protocol needs `get` to return a projection, so the answer for std is yes and the mechanism is the existing accessor rule.

**Methods live in the struct body** (spec 6.4:1-3). There are no free-standing `impl` blocks that add methods to a type from another module. This is a quietly important fact: it means a type's method set is closed at its definition, which is the property Carbon calls "a type's API is consistent no matter what is imported" and which Rust and Swift lack.

**Comptime value domain.** Integer, Bool, Type, Unit as arguments; strings and function aliases are rejected as arguments; aggregates and enums are unrepresentable (RUE-562). Depth bound 64, no cardinality bound. `ComptimeHost` is a ~60-method host trait with three implementors (ordinary, durable, fixtures), so any new comptime-visible concept costs three implementations plus durable identity encoding.

## 4. The survey: what each design is, what it costs, what Rue can take from it

Each entry states the coordinates on the axes of §5 (conformance, checking, compilation, budget, intensionality), the known cost profile, and the transferable lesson. Sources are listed in §10.

### Rust
*Declared with bodies; definition-site; monomorphization; everything; not intensional.*
The reference point for "traits done thoroughly." Its complexity has identifiable sources: impls carry bodies and may overlap, so coherence, the orphan rule, and (unstable) specialization exist to keep selection unique; blanket impls make impl selection a search; associated types with projections and equality constraints make normalization a rewriting problem; inference is trait-directed, so method probing and `.collect()`-style APIs depend on which impls are in scope; and first-class references bring lifetime bounds, variance, and GATs. The next-generation trait solver has been in development for about four years, hit cases that were "quadratic or even exponentially slower than the old solver," and as of August 2026 is being enabled on nightly with stabilization planned within months; one benchmark went from 27 seconds to under one second in three months of tuning. Specialization has been nightly-only and documented as unsound for a decade. The orphan rules have been rebalanced twice (RFC 1023, RFC 2451), Ixrec's survey of them concludes "there are a lot of `impl`s that people want to write, but they currently cannot write," and as of August 2026 nobody has published a formal treatment proving the rules yield global coherence. The Argus study of trait-error diagnostics found that with a dedicated visualizer users localized 2.2 times as many faults 3.3 times faster, which is a measure of how far the message sits from the mistake. The lesson for Rue is not "traits are slow" but "these specific features are what make trait solving a solver." Rue can take the definition-site guarantee and the associated-constant idea and decline the rest.

### Swift
*Declared with bodies; definition-site; hybrid (witness tables, specialized when visible); large; not intensional.*
Swift protocols are the closest ergonomic model for Rue's value-semantics world, and Swift's experience with floats is directly reusable: the 2017 core-team thread on `FloatingPoint` and `Equatable` explicitly considered Rust's `PartialEq`/`Eq` split and rejected it. Xiaodi Wu's summary: with two protocols "most people still use only one, and still use it incorrectly." Swift kept one `Equatable`, kept IEEE `==`, documented the exception, and added total-order tools for generic algorithms. On cost, the definitive account is Slava Pestov's *Compiling Swift Generics* (November 2025): generic signature queries are "at least as hard as the word problem; that is, undecidable in the general case," and the compiler's decision procedure is a Knuth-Bendix rewriting system built per generic signature. That machinery exists because of associated types with same-type constraints and protocol refinement chains. Rue's v1 should stop before that boundary, and §9 does. Two more Swift decisions are instructive. Existentials were originally spelled like generics and had to be walked back with an explicit `any` keyword (SE-0335) because "the similar spelling to generic constraints has caused many programmers to confuse existential types with generics" and because existentials "require dynamic memory" and "dynamic method dispatch that cannot be optimized away"; and retroactive conformance from any module, which Swift allows, is listed by its own designers as a regret because "two ways to hash a single type" can arise. The SE-0067 floating-point proposal states the ruling Rue should copy: "Exceptional values need not take part in the strict total order."

### Carbon
*Declared with bodies; definition-site against an archetype; monomorphization for static, witness tables only for `dyn`; medium; templates as the marked escape hatch.*
Carbon is the most useful comparative document because it is a post-Rust, post-Swift design by people who wrote down every rejected alternative. Interfaces are nominal "which means that types explicitly describe how they implement interfaces"; the stated goal is that "a type's API is consistent no matter what is imported, unlike Swift and Rust." Impls are restricted to the library defining the type or the interface, with overlap, prioritization, acyclicity, and termination rules to keep selection deterministic. Checked generics are type-checked "using only the information present in the signature," against an archetype, and `template` marks a function that opts back into instantiation-time duck typing. On compilation strategy the witness-table appendix is explicit: witness tables were rejected for static generics because associated constants "allow the signature of a function to vary," blanket implementations and specialization are intractable to synthesize, and "relying on witness tables would result in different semantics for calling the same function with the same types, depending on which witness tables were available at the callsite." Everything Carbon says about archetypes applies to Rue's skolem idea; everything it says about impl rules is what Rue avoids by making assertions bodiless.

### Hylo
*Declared with bodies (`conformance T: Trait`), retroactive allowed; separately type-checked generics; monomorphization; medium with existentials; not intensional.*
The nearest relative: mutable value semantics, `let`/`inout`/`sink` receivers in requirement declarations, subscripts as requirements, and existentials `any T`. Coherence is by fiat: "a type may have at most one source of conformance to a specific trait," conditional conformance may not coexist with unconditional conformance to a refining trait, and a conformance is only exposed in a module that declares either the type or the trait. Hylo shows that Rue's access modes and projections fit naturally into requirement signatures, and its one-source rule is what §9 rule 2 adopts. The current rewrite (`hylo-new`) renames conformances to *givens* and draws a line Rue should notice: "An extension reopens the scope of a type and adds new members. A given exposes a conformance." Members introduced in a given are not found by ordinary `a.m` lookup; a given is evidence, not API. That is the same separation Carbon draws between `extend impl` and plain `impl`, and it is the separation the bodiless assertion gives Rue for free. Hylo's implementation is still early, so it offers no compile-time data.

### Zig
*Implicit; instantiation-site; monomorphization; none; fully intensional.*
Two pieces of evidence matter more than any opinion. First, issue #17198 ("replace anytype") collects the community's problems with unconstrained generics: constraints are "the first thing you want to know" about a function and are invisible in its signature; errors appear "deep in call chains"; language servers cannot help. The core team's answer was to reject constraints because "generic code is harder to read, reason about, and optimize than code using concrete types," and concrete code "should be the primary focus." Second, in Zig 0.15.1 the standard library removed `GenericReader`, `GenericWriter`, `AnyReader`, and `AnyWriter` in favor of concrete `std.Io.Reader`/`Writer` with a vtable, because "the old interface was generic, poisoning structs that contain them and forcing all functions to be generic as well with `anytype`," and because monomorphizing large functions per writer type produced binary bloat. The new design keeps performance by putting "the buffer above the vtable," so the hot path is concrete and the vtable is hit only on refill. Read together: comptime generics are excellent for type construction and parametric containers (which Rue has), and the wrong tool for behavioral interfaces at API boundaries (which is the question on the table). Rue already has Zig's good half.

### Go
*Implicit structural; constraints as type sets; GC-shape stenciling with dictionaries; minimal; not intensional.*
Go 1.18 groups types by GC shape and passes a dictionary of type descriptors, derived types, sub-dictionaries, and method tables; calls through the dictionary are indirect and inlining is limited, and programs that generate unbounded distinct types via recursion cannot compile. Measured: value-type generics ran about 24% faster than interface code in one benchmark while pointer-type generics were at parity or slower, because all pointers share one shape and fall back to dictionary calls. Go checks generic bodies at the definition site against the constraint, with the rationale "we don't want to derive the constraints from whatever `Stringify` happens to do," and it keeps that check cheap by refusing specialization, operator methods, and value parameters, three things Rue's comptime already provides. The transferable idea is *type sets*: a constraint written as a union of primitive types is exactly the right tool for "numeric generics," and it is what §9 rule 4 calls a closed tier. Implicit satisfaction is the part Steve is wary of; TypeScript's `implements` clause shows that a structural type system can still require a declaration, and that is the shape the July anchor takes.

### C++ concepts, then and now
The 2009 removal of concepts from C++0x turned on precisely axis A. The design had explicit `concept_map`s; Stroustrup argued for `auto` concepts, ranking "accidental match" as "a minor problem, not in the top 100 problems" and defending duck typing as "the key to the success of templates." The committee could not agree and removed the feature; C++20 shipped the implicit version, checked at instantiation, as a diagnostics improvement over raw templates. The lesson is that the dispute is real and old, and that its resolution depends on who writes the code: Stroustrup's ranking was for expert humans writing libraries. §7 argues the ranking flips for agents.

### Mojo
*Declared with bodies; definition-site; monomorphization; growing; partially intensional (parameters).*
Mojo is the freshest data point on axis A. Its traits were originally satisfied implicitly; implicit conformance was deprecated in release 25.4 and removed in 25.5, and the manual now says "Conformance is explicit. A struct that happens to implement `fetch_reading()` doesn't conform to `DeflectionSensing` unless it declares the trait." Since then it has added conditional conformance, trait compositions (`Copyable & Defaultable`), default methods (September 2025), custom `where (cond, "message")` diagnostics, and conformance conditions over parameter packs. It is a live demonstration of the pressure Steve describes: each addition was demanded by real library code, and the system is converging on Swift's. The design in §9 refuses the two additions (default methods, blanket conformance) that create import-sensitive APIs, and accepts the one (conditional conformance) that is needed for containers.

### Austral
*Declared with bodies (`typeclass`/`instance`); definition-site; monomorphization; deliberately minimal; not intensional.*
Austral pairs linear types with type classes and universe constraints (`T: Free`, `T: Linear`), which is the same shape as Rue's multiplicity lattice used as a bound. Its system is the smallest coherent nominal design in the survey: one type parameter per class, no associated types, instances "globally unique" under three sentences of orphan rule (local class or local type; never foreign class with foreign type), and resolution that is a match over visible instances. Borretti's design principle, "If it's not in the source code, it's not happening, and you're not paying the cost of it," is close to Rue's. It is proof that a linear language can have a very small class system, and that "Copy" belongs to the type's universe rather than to a trait.

### OCaml modules and functors
*Explicit parameterization by a module; definition-site; no inference; no coherence problem because instances are named values.*
Functors are the honest ancestor of "pass the dictionary explicitly." They have no coherence problem because there is no implicit selection; they are verbose because every use names the instance. Rue's explicit type arguments (`std.sort.quicksort(i64, inout v)`) are already functor-shaped, and modular implicits have not shipped in a decade, which is a data point on how hard adding implicit resolution to an explicit system is.

### D, Julia, Haskell, Scala (brief)
D is the beloved member of the instantiation-checked family: named constraints with good members-diff diagnostics, no definition checking. Julia is its cautionary tale at ecosystem scale, where the absence of interfaces makes "does this type support X" an unanswerable question. Haskell's dictionary passing plus specialization is the origin of the Rust design, and Well-Typed's measurements are the cleanest published price of monomorphization versus dictionaries: full specialization of a small program cost +72% code size and +8.7% compile time for a 73% runtime win, while the Cabal parser paid +24% code and +30% compile time for about 1% throughput. Scala 3's `given`/`using` shows that local instances buy flexibility at the price of resolution that depends on scope, which is the import-sensitivity Carbon and §9 rule out.

### The pattern across the survey

Every design that started structural or implicit and then shipped at scale has moved toward explicit nominal conformance: Mojo removed implicit conformance in 25.5, Zig's standard library moved its I/O interfaces to concrete vtables, Swift added `any` to make existentials explicit, and C++0x's explicit concept maps were the design the committee could not agree to drop rather than one nobody wanted. Go is the exception and pays for it by forbidding specialization, operator methods, and value parameters. The compile-time hazards cluster in three places: associated-type normalization inside a solver that must handle cycles (Rust), expression-level overload search over literal protocols (Swift), and per-instantiation body re-analysis with no separate check (Zig, C++). None of the three is inherent to "a nominal interface with methods, explicit conformance, monomorphized," which is what Austral has and what Carbon has once associated items are set aside.

### Summary matrix

| | Conformance | Checked at | Compiled by | Coherence machinery | Assoc. types | Defaults | `dyn` | Reflection |
|---|---|---|---|---|---|---|---|---|
| Rust | declared+bodies | definition | mono | orphan, overlap, solver | yes, with projections | yes | yes | no |
| Swift | declared+bodies | definition | witness + specialize | one conformance per module set | yes, with same-type | yes | yes | no |
| Carbon | declared+bodies | definition (archetype) | mono; witness for dyn | orphan, overlap, priority, acyclic | yes | yes | yes | templates opt out |
| Hylo | declared+bodies | definition | mono | one source per type | yes | yes | yes | no |
| Zig | implicit | instantiation | mono | none | n/a | n/a | hand vtables | full |
| Go | implicit | type sets | dictionaries | none | no | no | yes | runtime only |
| C++20 | implicit | instantiation | mono | none | n/a | n/a | no | C++26 |
| July sketch / §9 | declared, bodiless | definition (skolem) | mono | none needed | consts only | no | reserved, second-class | no, by ruling |

## 5. The design axes

Every mechanism in the survey is a point in a five-axis space. Naming the axes separately matters because the popular designs bundle choices that are independent, and Rue can unbundle them.

**Axis A: how conformance is established.**
- *Implicit structural.* A type satisfies an interface if it has the members. Go interfaces, C++20 concepts, TypeScript structural types, Zig duck typing, D constraints, Julia. No declaration; accidental conformance possible; no place for a rename to error except a distant use site.
- *Declared, structurally verified.* A type satisfies an interface only if a conformance is asserted, and the assertion is checked against the type's inherent members with no bodies of its own. TypeScript `implements`, C++0x explicit `concept_map` without adaptation, Go's `var _ I = T{}` idiom made mandatory, the July sketch's `T is I`. Assertions are idempotent facts, so two of them cannot disagree.
- *Declared with bodies.* The conformance carries the implementations. Rust `impl`, Swift `extension T: P`, Carbon `impl`, Hylo `conformance`, Mojo, Austral `instance`, Haskell `instance`. Adaptation and retroactive conformance for free; in exchange, a type can have several candidate implementations of one interface, so the language needs uniqueness rules (coherence, orphan rules, overlap, priority) and, once blanket impls exist, a search procedure.

**Axis B: when a generic body is checked.**
- *At instantiation.* C++ templates, Zig, D, current Rue. Errors are non-local and depend on which instantiations exist; a library author tests rather than verifies. Reflection is "free" here.
- *At definition, against the bound.* Rust, Swift, Carbon, Hylo, Haskell, OCaml functors. The signature is the whole contract. Historically this required a symbolic type-checking mode; the July insight is that in a parametric language it can be done by instantiating the body with a skolem (a synthesized nominal type carrying exactly the bound's members), because no program can distinguish the skolem from a real type. Carbon's "archetype" is the same idea expressed symbolically.

**Axis C: how generic code is compiled.**
- *Monomorphization.* Rust, C++, Zig, Carbon (for static), current Rue. Per-instance cost; best runtime; sizes and associated constants can vary per instance.
- *Dictionary or witness-table passing.* Haskell, Swift unspecialized, Go (GC-shape stenciling + dictionaries), Rust `dyn`. One body; indirect calls; blocks inlining; sizes must be runtime-known or boxed. Carbon rejected witness tables for static generics because they cannot express associated constants that change signatures, blanket implementations, or specialization, and because "relying on witness tables would result in different semantics for calling the same function with the same types, depending on which witness tables were available at the callsite."
- *Hybrid.* Swift specializes when it can see the body; Go stencils per GC shape. Both inherit the worst-case of the dictionary path.

**Axis D: the feature budget.** Associated types, associated constants, refinement (interface inheritance), conditional conformance (`ArrayBuf(T) is Eq where T is Eq`), default methods, blanket conformance (`every T with X is Y`), specialization, operator overloading, existentials, reflection. Each is a separate decision, and §9 makes each one.

**Axis E: intensionality.** Whether comptime code can inspect a type. Zig, D, C++26 reflection: yes. Rust, Swift, Carbon checked generics, Hylo, and current Rue: no. This axis is coupled to axis B: a body that branches on the shape of `T` cannot be checked once for all `T`. It is a one-way door; adding reflection later closes definition checking for any body that uses it, which Carbon handles by marking such functions `template`.

The bundles people argue about are points in this space: "Rust traits" is (declared-with-bodies, definition, monomorphization, everything, no). "Go interfaces" is (implicit, instantiation-ish via type sets, dictionaries, minimal, no). "Zig" is (implicit, instantiation, monomorphization, none, yes). The July sketch is (declared-verified, definition-by-skolem, monomorphization, small, no). Seeing them as coordinates makes the trade-offs discussable one axis at a time.

## 6. Candidate designs for Rue, head to head

Five candidates cover the space. Each is scored against the same things: the acceptance tests in §1, the corpus needs in §2, the substrate in §3, agent ergonomics (§7), and compile-time cost (§8).

**Candidate A: lean into comptime (Zig).** Keep `comptime T: type` unbounded, add `@typeInfo`/`@hasDecl`-style reflection, and build interfaces as conventions (`std.io.Reader`-style "comptime interfaces" or hand-written vtable structs). No new declaration forms.
- *Covers:* everything, in principle; `Sequence` becomes "any `T` with a `next` method," checked when instantiated.
- *Costs:* instantiation-time errors with no signature contract; every library body must be tested per instantiation; tooling cannot resolve members of `T`; closes the door on definition checking and on dictionary compilation for any body that reflects. Requires adding reflection to a comptime engine whose host trait has three implementations and whose value domain is deliberately closed (RUE-562). Zig's own standard library retreated from this for its most important interface, and Zig's core team declines to make it safer.
- *Compile time:* no new machinery, but instance breadth is unbounded (RUE-1098) and reflection-heavy bodies are the ones that fan out.
- *Agents:* worst error locality; least reviewable output.
- *Verdict:* Rue already has the good half of Zig. Do not add the other half.

**Candidate B: structural interfaces checked at instantiation (C++20 concepts, Go-style constraints).** Add `interface` as a named predicate over members; `comptime T: Sequence` is satisfied by any `T` with the members; check at the call site.
- *Covers:* the acceptance tests; gives a members-diff error at the call instead of deep in the body.
- *Costs:* accidental conformance; a rename errors at a distant call; no textual answer to "what conforms"; the body may still use members beyond the bound and fail late (Carbon's verdict on C++20: definition checking is "infeasible, not impossible"). Fable's own July 4 recommendation was this candidate; it was overtaken by the checkpoint of July 15/16 for the reasons in §7.
- *Compile time:* one members-diff per (type, interface) pair. Cheapest possible.
- *Agents:* good call-site errors for callers; nothing for library authors; grep-ability poor.
- *Verdict:* the right instantiation-site diagnostic, and it should exist inside the winning design as the call-site half. Not sufficient alone.

**Candidate C: declared conformance, structurally verified, bodiless (July anchor, TypeScript `implements`, C++0x explicit concept maps without adaptation).** `interface` declarations; `T is I` assertions checked against inherent members; definition-site checking by skolem; closed tiers for primitives.
- *Covers:* the acceptance tests (§9.3 walks each); the corpus needs in §2 except comparators (which are function values) and iteration ceremony (which is the accessor rule plus `for` desugaring).
- *Costs:* one assertion line per conformance; no rename adaptation (wrap instead); no default methods; no blanket conformance. Needs the skolem mechanism built into the compiler (estimate below).
- *Compile time:* pairs plus one instantiation per generic definition. No search.
- *Agents:* best error locality (signature is the contract), best grep-ability, no coherence error class.
- *Verdict:* recommended. It is Carbon's checked generics with the impl bodies removed, which is exactly the part that generates Carbon's orphan, overlap, priority, acyclicity, and termination rules.

**Candidate D: nominal traits with implementation bodies (Rust, Carbon, Hylo, Mojo, Austral).** `interface` plus `impl I for T { ... }` or Hylo-style `given`.
- *Covers:* everything C covers, plus retroactive adaptation of foreign types and default methods.
- *Costs:* uniqueness rules are now required (Hylo's one-source rule and exposure rule are the minimum; Carbon's five rules are the full set); with blanket impls, resolution becomes search; Mojo shows the feature list grows under library pressure. Rue's spec already says methods live in the struct body (6.4:1), so this adds a second place methods can come from and makes a type's API depend on imports unless the Hylo/Carbon "evidence is not API" rule is also adopted, at which point the bodies buy only adaptation.
- *Compile time:* fine without blanket impls; a solver with them.
- *Agents:* introduces the coherence error class, which is the worst-repaired class in the literature.
- *Verdict:* the reserve position. If dogfooding shows adaptation of foreign types is a frequent need, the smallest step from C is "adapter conformances with bodies, permitted only for types you do not own, one per (type, interface)," which is Hylo's exposure rule applied to a bodiless base.

**Candidate E: dictionary passing or witness tables as the primary compilation strategy (Go, Swift unspecialized, Haskell).** Any conformance model above, compiled once per generic with an explicit dictionary.
- *Covers:* the same surface; reduces instance count.
- *Costs:* indirect calls, blocked inlining (Go: pointer-shaped generics at parity with interfaces or slower), sizes and associated constants must be runtime-known or boxed (Carbon's reason for rejecting it), semantics that can depend on which witnesses are visible. Conflicts with Rue's per-instantiation comptime values in signatures.
- *Compile time:* fewer bodies, but Haskell's data says the trade is +30% compile time for the Cabal parser under full specialization versus a 73% runtime win on small programs; the direction of the trade depends on the workload.
- *Verdict:* not as the primary strategy. Keep it as RUE-1550's option for the day instance counts become the problem, which requires definition-checked generics (C or D) to be possible at all. Use vtables for `dyn` only, as Carbon and Zig do.

**Orthogonal to all five: function values.** Second-class `fn` parameters plus comptime-known `const` tables of function references. No candidate above removes the need for them, and none is made harder by them. Ship first.

**Estimated implementation size, from the compiler agent's reading of the pipeline.** Candidate B is 1 to 2K lines including spec and tests: a declaration kind, name resolution, and a check where `type_subst` is built (`calls.rs:600-625`) or a new intrinsic in the `@require_droppable` family. Candidate C adds skolem synthesis (a source-less nominal `StructId`, which anonymous structs and builtins already use), a `@panic`-bodied method table, the closed-tier exhaustive driver, and assertion resolution with `where` conditions; my estimate is 4 to 8K lines, well short of the 10 to 20K a symbolic abstract-type mode would cost, because no phase learns to analyze an abstract `T`. Candidate D adds uniqueness rules and a second method source on top of C. Candidate A adds reflection to a closed value domain and three comptime host implementations, plus durable identity encoding for every new comptime-visible fact.

## 7. What is good for agents

This is the question Steve flagged as unknown, so I want to be careful about what is evidence and what is my judgment.

**What the literature supports.** The LLM-repair work is almost all on Rust. RustAssistant (Microsoft) reaches roughly 74% fix accuracy on real compilation errors by iterating between the model and the compiler, which tells us the compiler-error loop is the mechanism agents actually use. SafeTrans (C to Rust, 2025) breaks repair success down by error class: borrow-checker errors are repaired at 74.2%, but "trait implementation failures" at 58.7%, with E0277 (trait bound not satisfied) and E0308 the two worst categories at roughly 59% and 55%. A 2026 study of 86,726 failing LLM samples across C, C++, Java, and Rust found Rust's largest error share was incompatible parameter types (43.4%), then ownership and lifetimes (16.7%), then trait and type-bound errors (6.1%), and attributes Rust's difficulty to "its strict type system and lower representation in training data." A separate study of hallucinated Rust crates found the rate "surprisingly consistent" across models and sampling settings, which argues for designs where the set of valid conformances is explicit and locally discoverable rather than inferred. The C-to-Rust translation papers (AdaTrans, SafeTrans, and the Amazon whole-project work) converge on one finding: models produce code that is locally fluent and globally wrong, and the failures cluster on **non-local** constraints, ownership and lifetimes above all. The Go-to-Rust project-translation work notes that signature-level type compatibility checks "narrow the scope of potential repairs." Nobody has published a controlled study of Zig comptime versus traits for agents, so anything beyond "locality of errors matters" is inference. RUE-353 is the one in-house data point: the training prior is strong enough that an agent wrote `as` casts six-plus times in one session against a language that has none. Familiarity is a real but bounded, one-time cost; per-call-site bookkeeping recurs forever.

**What follows from how agents work.** Agents read signatures and grep; they rarely read a library body unless an error sends them there. They generate lots of small types with short method names. They fix what the error message points at. They handle "add a line here" well and "restructure because of a rule you cannot see from here" badly. Given that:

1. **Definition-site checking is the agent feature.** Under instantiation checking, a library author (usually an agent) gets no feedback until some caller instantiates the body, and the caller (another agent) gets an error deep inside code it did not write, with no instantiated-here label in today's compiler. Under definition checking the signature is the complete contract, the error is at the line the author is editing, and go-to-definition, rename, and `rue doc` can actually resolve members of `T`. Dorian's review put this as local reasoning and token efficiency; I would add that it also makes generic code *reviewable by the human*, which for agent-written code is the scarce resource.

2. **Declared conformance is the grep feature.** "Which types are `Display`" is `grep 'is Display'`. Under implicit structural conformance the question has no textual answer, which is why Go programmers invented `var _ I = T{}` and why agents apply that idiom inconsistently. Accidental conformance, which Stroustrup ranked "not in the top 100 problems" for human C++ programmers, is a bigger risk for agent-written code because agents produce many near-duplicate `len`/`get`/`next`/`push` methods per session; a declared assertion is one line, and agents do not mind boilerplate.

3. **Coherence errors are the worst class.** The orphan rule, conflicting impls, and "the trait is not implemented for this type in this scope" are Rust errors whose fix is non-local (newtype wrappers, re-exports, moving code between crates). A design in which assertions are bodiless has no such class, because there is nothing to select among. This is, I think, the single strongest agent-side argument for the July anchor over Rust-shaped traits.

4. **Reflection is the least reviewable code.** Zig's core team, rejecting constraints in #17198, says plainly that "generic code is harder to read, reason about, and optimize than code using concrete types." Comptime that inspects types is the extreme case, and it is exactly the kind of clever code agents produce when allowed and humans cannot audit. Parametric generics stay boring. For a project whose stated thesis is agent-first engineering with human review, boring generic code is the goal.

5. **Familiarity favors `interface` plus a header clause.** `struct Range is Sequence { ... }` reads like TypeScript's `implements`, Swift's `struct X: P`, and Java's `implements`; methods in the struct body read like Rust inherent impls. The freestanding `i64 is Loggable;` form has no prior in the corpus, but the compiler can suggest it verbatim from a "missing conformance" error, which is the channel agents actually learn from. The keyword `trait` would pull agents toward writing `impl Trait for T { fn ... }` bodies that the grammar rejects; `interface` pulls toward the right shape.

Where this argument is weakest: I have no measurement that agents write correct bounded-generic code faster under declared conformance than under structural, and Go's two decades are evidence that implicit conformance is livable for humans. The experiment in §8 should include an agent-ergonomics arm, not just a compile-time arm.

## 8. Compile time: cost model and the experiment to run

Rue treats compile time as a feature, so the design has to come with a cost model that the harness can check.

**What each candidate costs, on the architecture as it exists.**

| Mechanism | New work per compile | Bounded by | Memoizable / durable |
|---|---|---|---|
| Instantiation-site bound check | one members-diff per (type, interface) pair reached | pairs actually used; Caldera reaches 14 type-constructor applications | yes; a fact about two declarations |
| Definition-site check by skolem | one extra instantiation of each generic body per distinct bound signature | number of generic definitions (std: 38) | yes; a fact about the definition, revision-independent |
| Closed-tier exhaustive check (`Integer`) | one instantiation per member of the tier per numeric generic | tier size (8 integer types today, 10 with floats) times numeric generics (std/math, std/cmp: about 15) | yes |
| Conditional conformance resolution | recursive walk of the type constructor tree | type nesting depth | yes |
| Blanket conformance / impl search | a search over candidate impls with backtracking | not bounded by program shape; this is Rust's solver | poorly |
| Dictionary passing | one body per generic plus indirect calls at runtime | fewer instances, slower code, blocks inlining (Go's finding) | n/a |
| Zig-style reflection | none at compile time beyond what comptime already does | instantiation count, unbounded (RUE-1098) | per instance only |

The first four are additive to what the compiler already does and are all facts about pairs of declarations, which is the shape the durable query layer already stores. The fifth is the one to refuse. The sixth is a different compilation strategy and should be evaluated under RUE-1550 on its own merits, noting that Carbon's analysis of why witness tables cannot carry associated constants applies verbatim to Rue's `const Element: type`.

**The experiment.** The harness cannot price a feature the corpus does not use, so the plan is prototype, port, measure, in four steps, each with a number the harness already reports (wall time, per-function work counters, instance counts, peak memory):

1. *Machinery on non-users.* Land `interface`, assertions, and instantiation-site bound checks behind `--preview interfaces`. Measure Caldera, Meridian, Lattice unchanged. Expected: within noise. This prices the parser, declaration, and identity plumbing.
2. *Bounded std.* Port `std/cmp`, `std/sort`, `std/binary_heap`, `std/math`, and add a generic `HashMap(K, V)` over `Hashable + Equatable`. Measure the std-heavy examples (wordfreq, tinydb, jsonfmt) and the `scale_instantiations` probe. Expected: per-instance cost flat; instance count unchanged, since bounds do not create instances.
3. *Skolem checking.* Turn on definition-site checking. Measure the same programs. Expected: plus one instantiation per generic definition; in Lattice that is on the order of 40 extra bodies against 1,053 ordinary ones, so under 5% of semantic work even if each skolem body costs as much as a real one. This is the number that decides the family fork, so it should be measured, not assumed.
4. *A corpus slice that looks like user code.* Port one Meridian analysis family and Caldera's behavior registries to interfaces and second-class function parameters. Compare wall time and line count against the index-loop originals. This measures the cost of the feature *as users will use it*, which is the comparison Steve described, and it should be run against the compile-time budget as a stated number, not a vibe.

If step 3 comes back with skolem bodies costing materially more than ordinary ones, the fallback is not Zig-style checking; it is to make skolem checking lazy (on publish, or on `rue check`) while keeping instantiation-site checks always on. If step 4 shows instance counts growing with bounded generics, that is the signal to open RUE-1550's dictionary option, and definition-checked generics are the only kind that *can* be dictionary-compiled, which is one more reason to take axis B now.

## 9. Recommendation

### 9.1 Two features, in this order

**First: second-class function parameters (RUE-643).** A parameter of type `fn(A, B) -> R` that may be called and passed on to another second-class parameter, but not stored in a struct or `ArrayBuf`, returned, or captured. Named functions are the only values today; comptime-known partial application can come later, closures are a separate design with capture semantics against the access model. Codegen is a code pointer and an indirect call; the ABI is a plain argument. This unlocks `sort_by`, `max_by`, `filter`, `for_each`, hash functions, query predicates, visitors, and Caldera's registries, and it needs no interface machinery. It also composes with interfaces later: a bound method and a function parameter are both "behavior the body may call," one by name and one by value.

Order matters: shipping this first gets the largest single reduction in corpus repetition (§2) while the interface design is being ruled, and it lets the interface v1 stay small because comparators do not need to be interfaces.

**Second: interfaces, declared and structurally verified.** The July anchor, restated with the decisions it left open now taken:

```rue
interface Equatable {
    fn equals(borrow self, borrow other: Self) -> bool;
}

interface Sequence {
    const Element: type;
    fn next(inout self) -> Option(Element);
}

interface Collection: Sequence {
    const Position: type;                       // Copy bound spelled below
    fn start(borrow self) -> Position;
    fn is_end(borrow self, p: Position) -> bool;
    fn advance(borrow self, p: Position) -> Position;
    fn get(borrow self, p: Position) -> borrow Element;   // accessor, RUE-662
}

struct Range is Sequence {                      // header form
    cur: i64, end: i64,
    pub const Element = i64;
    fn next(inout self) -> Option(i64) { ... }
}

i64 is Equatable;                               // freestanding, retroactive, idempotent
ArrayBuf(T) is Equatable where T is Equatable;  // conditional, one assertion per constructor

fn contains(comptime T: Equatable, borrow xs: ArrayBuf(T), borrow x: T) -> bool { ... }
fn gcd(comptime T: Integer, a: T, b: T) -> T { ... }   // closed tier
```

Rules, each of which is a decision:

1. **An assertion has no body.** Conformance is verified against the type's inherent members by resolved-signature comparison, including access modes and associated constants. Two assertions of the same fact are identical, so there is nothing to select among: no coherence, no orphan rule, no overlap rule. A type whose method is named `advance` cannot be asserted into an interface wanting `next`; you wrap it. This is the line that keeps the solver out, and it should be stated in the ADR as the invariant that every later extension must preserve.
2. **Exactly one assertion per (type constructor, interface).** For a generic constructor the assertion may carry `where` conditions on its parameters. Resolution is syntax-directed on the type: to decide `ArrayBuf(i64) is Equatable`, find the unique assertion for `ArrayBuf`, substitute, and check the conditions recursively. This is Hylo's "one source of conformance" rule and it is a recursive function over the type, not a search. Blanket assertions (`every T is Loggable`) are refused because they are what turn resolution into search.
3. **Bounds are checked at the definition site by skolemization**, in the compiler, as Dorian asked, not as a CI harness. For a generic `f(comptime T: I, ...)`, synthesize a nominal struct `Skolem_I` carrying exactly `I`'s members with `@panic` bodies (typed `!`, so they coerce to any return), instantiate `f` with it, and report the errors at `f`. Associated constants on the skolem are themselves skolems of their bounds. Instantiation-site checks remain on as well; they produce the "T does not satisfy I: missing `fn next(inout self) -> Option(Element)`" diagnostic at the call. A body may only use members of `T` that the bound provides; this is what makes the skolem representative, and it is the property the July audit found already holds everywhere in the corpus.
4. **Closed tiers for primitives.** `Integer` (and `Float`, `Number` when floats land) are type sets, not interfaces: a numeric generic is checked by exhaustive instantiation over the set, which is complete rather than approximate. Operators stay intrinsics. This covers all 26 corpus value functions that use operators. Operator overloading through interface methods (`a < b` on user `T` resolving to `T is Ordered`) is deferred, and the closed tier is the acknowledged hole in the local-reasoning story until it lands.
5. **Associated constants, including type-valued ones, spelled `const Element: type` in the interface and `pub const Element = i64` in the type.** One item kind, and the mechanism (`ConstValue::Type`) exists. Same-type `where` constraints between associated constants of different parameters (`T.Element == U.Element`) are out of v1; that is the boundary past which Swift needed a rewriting system, and the iteration and formatting acceptance tests do not need it.
6. **Refinement (`Collection: Sequence`) yes; default methods no; specialization no.** Defaults would make a type's API depend on which assertion is in scope, which reintroduces the import-sensitivity Carbon designed out. std provides generic free functions instead, which is already its idiom (`std.sort.quicksort(T, inout v)`). Extension methods are a separate future design.
7. **Receiverless requirements yes** (`fn from(e: E) -> Self`) because `?` widening and `Default`-style construction need them and the cost is nil.
8. **Static dispatch only in v1. Reserve `dyn` as a second-class existential.** `fn write_all(inout out: dyn Writer, ...)`: an interface-typed parameter backed by a pointer and a vtable, callable but not storable, exactly the shape of Zig's new `std.Io.Writer` and of second-class loans and function parameters. Owned boxed existentials need a heap story and can follow. Declared conformance makes the vtable trivially constructible from the assertion.
9. **Types stay opaque to comptime control flow.** Ratify this as a spine ruling alongside the family decision, since the skolem guarantee depends on it. Reflection, if it ever comes, is staged: derive-style reflection over concrete types at the definition site, and Carbon-style `template`-marked functions that opt out of the guarantee, never unmarked reflection on opaque parameters.
10. **Keyword `interface`, header clause `is`, composition `+`.** `struct X is A + B { }`; bounds `comptime T: A + B`; lattice bounds compose the same way (`comptime P: Copy + Equatable`).

### 9.2 What this declines, and why each declination is safe

| Rust feature | Pressure that produced it in Rust | In Rue |
|---|---|---|
| Lifetime bounds, variance, GATs | first-class references | absent; loans are second-class |
| `Fn`/`FnMut`/`FnOnce` traits, closure `impl Trait` | closures are trait objects | absent; function parameters are second-class values, not types with impls |
| `Send`/`Sync`/auto traits | concurrency markers | absent; no concurrency story; markers would be lattice facts |
| Coherence, orphan rule, overlap, negative reasoning | impls carry bodies and can conflict | absent; assertions are bodiless and idempotent |
| Blanket impls, specialization | reuse across all `T: X` | refused; std ships generic free functions instead |
| Trait-directed inference, method probing through impls in scope | `.collect()`-style APIs | refused; type arguments are explicit (spec 3.11:11) and methods are inherent |
| `async fn` in traits | async | absent |
| Associated types with projections and equality constraints | iterator adapters, GATs | associated constants only, no same-type constraints in v1 |
| `dyn Trait`, object safety | dynamic dispatch | reserved as second-class `dyn` |

The pressures that produced most of Rust's additions come from features Rue does not have. The residual pressures, associated constants and conditional conformance, are the ones the iteration and container acceptance tests genuinely need, and both are resolvable without search under rule 2.

### 9.3 How the other open designs fall out

- **Floats and equality.** Take Swift's ruling, which was reached after explicitly considering and rejecting Rust's `PartialEq`/`Eq` split: one `Equatable` and one `Comparable`; `f64 is Equatable` and `f64 is Comparable` with IEEE semantics and a documented non-reflexivity exception; `@total_cmp` for sorting and hashing keys, with `Hashable` for floats defined over the total-order bit pattern. Swift's forum thread records the reason: "you now have two protocols instead of one, but most people still use only one, and still use it incorrectly." This also unblocks floats phases 4 through 6 today, since nothing in them depends on the interface design.
- **Iteration (RUE-219).** `for` desugars to `Collection` methods, with iteration modes as access modes. `ArrayBuf(T) is Collection` needs `get` to be a projection accessor, which is the trusted-std `get_ref` mechanism the container already has. The built-in `for` over `[T; N]`, `StrBuf`, and `chars()` becomes the first three conformances instead of a special case.
- **Formatting (RUE-350) and `?` widening (RUE-6).** `Display` with one method; `From` with one receiverless function. Both are one-line interfaces under this design.
- **`Option`/`Result` presence (RUE-1203).** Interfaces make the trusted-producer problem worse, not better, because `Sequence.next` returns `Option(Element)` and every interface user now needs `std/option.rue` in the graph. Option 2 in that issue (guaranteed toolchain presence without a prelude) becomes the natural companion ruling.
- **RUE-1550 (restrict comptime, opt-in specialization).** Definition-checked generics are the only kind that can be compiled once with dictionaries. Taking axis B keeps that door open; leaning into reflection closes it.
- **RUE-1552 (anonymous aggregates).** A skolem is a synthesized nominal struct; producer-nominal identity is exactly what makes it distinct from every real type. The two designs are compatible, and the restricted-anonymous proposal would, if anything, simplify assertion resolution by giving every conforming type a name.

### 9.4 What would change my mind

- If step 3 of the experiment shows skolem bodies cost more than a few percent of semantic work and cannot be made lazy, I would still keep declared conformance and drop to instantiation-site checking with an instantiated-here chain in diagnostics, which is a strictly better error than today's at near-zero cost.
- If dogfooding shows frequent need to adapt third-party types (rename a method to satisfy an interface), the bodiless rule is the thing to revisit, and it should be revisited as "adapter conformances with bodies for foreign types only," which reintroduces a small amount of coherence in a controlled place.
- If a real program needs `T.Element == U.Element`, that is the moment to look at Swift's requirement machine and decide whether a bounded fragment is worth it. Nothing in the current corpus or the acceptance tests does.
- If it turns out agents prefer implicit conformance in practice (the ergonomics arm of the experiment), relaxing required assertions to optional is a non-breaking change; the reverse is not, which is why v1 starts strict.

## 10. Sources

Rue-internal: RUE-246 (issue and the July 4, July 16, and July 18 comments), RUE-643, RUE-219, RUE-1550, RUE-1552, RUE-1203, RUE-1098, RUE-562, RUE-1012, RUE-350, RUE-353, RUE-196; `docs/designs/0025-comptime.md`, `0037`, `0038`, `0043`, `0062`; `docs/spec/src/04-expressions/14-comptime.md` (4.14:25-29), `03-types/11-type-inference.md` (3.11:11), `04-expressions/03-comparison-operators.md` (4.3:3b), `06-items/04-impl-blocks.md` (6.4:1-3); `docs/formal/01-core-calculus.md`; `docs/notes/per-body-identity-closure-materialization.md`, `body-analysis-cfg-incrementality-audit.md`; `performance/manifest.toml`; the compiler paths cited inline. Corpus census scripts and outputs are in the session scratchpad (`rueparse.py`, `census.py`, `similar.py`, `similar2.py`).

External, primary where possible:

- Carbon: generics overview, goals, terminology, details, appendix-coherence, appendix-witness (docs.carbon-lang.dev and the trunk repository); Chandler Carruth, CppNow 2023 generics talks (slides); Carbon Copy No. 9, November 2025.
- Swift: SE-0067 (floating-point protocols), SE-0335 (`any`), SE-0341 (`some` parameters); `Comparable.swift` documentation; forums.swift.org "Rationalizing FloatingPoint conformance to Equatable" (2017); Slava Pestov, *Compiling Swift Generics*, November 10, 2025 (download.swift.org), Part IV; Daniel Hooper, "Why Swift is slow"; Swift OptimizationTips.
- Hylo: specification (spec.md), introduction, `hylo-new` README and standard library (`Copyable.hylo`, `Equatable.hylo`), implementation-status page; BLDL 2025 keynote abstract.
- Zig: 0.15.1 and 0.16.0 release notes (Writergate section); ziglang/zig issue #17198 "replace anytype"; PR #24329; `std/mem/Allocator.zig`; Loris Cro, "Zig's new async I/O" and "Improving your ZLS experience"; typesanitizer, "Zig generics"; matklad, "Types and Zig" and "Things Zig comptime won't do".
- Go: type parameters proposal (43651); GC-shape stenciling design; golang/go issue #77273; PlanetScale, "Generics can make your Go code slower"; DoltHub, "Fast generics" (2022).
- Rust: "Enabling the next-generation trait solver on nightly" (blog.rust-lang.org, 2026-08-21); 2026 project goal "Stabilize the next-generation trait solver"; LWN, "Rust's next-generation trait solver"; Nethercote, "How to speed up the Rust compiler in July 2026"; Matsakis, "Cyclic trait solving" (2026-08-10); rust-lang/rust #31844 (specialization); Ixrec, rust-orphan-rules; osa1, "Coherence and orphans" (2026-08-29); Argus (arXiv 2504.18704).
- C++: Stroustrup, "No 'Concepts' in C++0x" (ACCU Overload 92) and "The C++0x 'Remove Concepts' Decision" (Dr. Dobb's, 2009); Siek, "The C++0x 'Concepts' Effort" (arXiv 1201.0027); isocpp historical FAQ; [temp.constr] in the C++ draft.
- Austral: specification (type classes, instance uniqueness, instance resolution) and Borretti, "Introducing Austral".
- Mojo: manual (traits), changelog (25.4, 25.5, 25.6), v1.0.0 release notes.
- OCaml: "Implicit modules: a middle step towards modular implicits" (ML Workshop 2025). Haskell: Well-Typed, "Choreographing specialization" part 2 (2024). Scala 3: implicit resolution changes. Odin FAQ and overview; Jai primer.
- LLM and language design: RustAssistant (Microsoft Research); SafeTrans (arXiv 2505.10708); AdaTrans (arXiv 2606.31706); Amazon, "Scalable, validated code translation of entire projects" (arXiv 2412.08035); 2026 compilation-error study (arXiv 2608.00661); AutoCodeBench (arXiv 2508.09101); type-constrained decoding (arXiv 2504.09246); Rust crate hallucination study (arXiv 2606.08444); Bruin, "Go is the best language for agents" and the HN thread; Akita, 2026-02-09 essay.
