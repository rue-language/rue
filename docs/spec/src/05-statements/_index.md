+++
title = "Statements"
weight = 5
sort_by = "weight"
template = "spec/section.html"
page_template = "spec/page.html"
+++

# Statements

This chapter describes statements in Rue.

> **Grammar note.** The EBNF fragments in this chapter are illustrative
> excerpts scoped to the construct under discussion. [Appendix A](../appendices/a-grammar/)
> is the normative grammar; where a fragment here differs from it, Appendix A
> governs.

{{ rule(id="5.0:1", cat="normative") }}

A statement is a construct evaluated for its effect rather than for a value delivered to its context; every statement has type `()` (unit). The three statement forms — `let` bindings, assignments, and expression statements — elaborate in the core calculus (`docs/formal/01-core-calculus.md`) to a binding sequence `let x = e1 ; e2`, an `assign p = e`, and a discarding sequence `e1 ; e2` respectively, and none of these delivers a value to the block that contains it (core §6.7, §6.8).
