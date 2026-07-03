+++
title = "Unchecked Code"
weight = 9
sort_by = "weight"
template = "spec/section.html"
page_template = "spec/page.html"
+++

# Unchecked Code

This chapter describes Rue's mechanism for low-level operations that bypass normal safety checks.

{{ rule(id="9.0:1") }}

Rue provides `checked` blocks and `unchecked` functions to enable low-level memory operations while keeping such code visibly separate from normal safe code.

{{ rule(id="9.0:2", cat="informative") }}

The operations in this chapter are the **only** source of undefined behavior in
Rue: raw-pointer and heap intrinsics whose validity the compiler cannot check
without changing a value's representation (ADR-0036). Appendix B (B.3) catalogues
the specific conditions that are undefined, and B.1 places them within Rue's
conformance taxonomy. Outside a `checked` block none of these operations is
reachable, so the safe subset of Rue has no undefined behavior.
