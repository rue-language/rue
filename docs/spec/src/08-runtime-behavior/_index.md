+++
title = "Runtime Behavior"
weight = 8
sort_by = "weight"
template = "spec/section.html"
page_template = "spec/page.html"
+++

# Runtime Behavior

This chapter describes runtime behavior in Rue, including error conditions and panics.

{{ rule(id="8.0:1") }}

A small, fixed set of primitive operations can **trap** at runtime: integer
overflow, division or remainder by zero, and an out-of-range array index. A trap
is not a value, and no surrounding expression can consume it; it abandons the rest
of the evaluation and halts the program with exit code 101 (the panic exit code of
Appendix B). The core calculus fixes exactly these trap categories — `overflow`,
`div-zero`, `rem-zero`, and `bounds` — together with their propagation and exit
code (core calculus `docs/formal/01-core-calculus.md` §6.12, and the `(Panic-Lift)`
rule of §6.2 by which a trap in any subexpression aborts the whole evaluation).
Each is total, deterministic, and observable, so a conforming compiler reproduces
the same trap on the same input. The following sections state each category
normatively.
