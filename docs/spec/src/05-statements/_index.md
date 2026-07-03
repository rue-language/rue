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

{{ rule(id="5.0:1") }}

A statement is a syntactic construct that performs an action but does not produce a value. Statements have type `()`.
