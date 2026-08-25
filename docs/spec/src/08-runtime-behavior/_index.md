+++
title = "Runtime Behavior"
weight = 8
sort_by = "weight"
template = "spec/section.html"
page_template = "spec/page.html"
+++

# Runtime Behavior

This chapter describes runtime behavior in Rue, including error conditions and panics.

{{ rule(id="8.0:1", cat="dynamic-semantics") }}

Certain operations **trap** at runtime. A trap is not a value, and no surrounding
expression can consume it; it abandons the rest of the evaluation and halts the
program with exit code 101 (the panic exit code of Appendix B). The core calculus
fixes five trap categories — `overflow` (arithmetic, negation, and `min_T / -1`),
`div-zero`, `rem-zero`, `bounds` (a negative or out-of-range array index), and
`user` (an explicit `@panic`) — together with their propagation and exit code
(core calculus `docs/formal/01-core-calculus.md` §6.12, and the `(Panic-Lift)`
rule of §6.2 by which a trap in any subexpression aborts the whole evaluation).

Two further operations trap under the same discipline but are not yet carried by
the core's taxonomy: decoding a byte sequence that is not well-formed UTF-8
(4.8:28) and an out-of-range `@intCast` (4.13:28). The core records this gap
itself. The set above is therefore the core's, not an exhaustive enumeration of
every operation in this language that can trap; a rule needing the complete set
must state it rather than cite this paragraph as closed.
Each is total, deterministic, and observable, so a conforming compiler reproduces
the same trap on the same input. The following sections state each category
normatively.
