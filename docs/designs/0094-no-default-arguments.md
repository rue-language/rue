---
id: 0094
title: "No default arguments: every call spells every argument"
status: accepted
tags: [language, syntax, semantics, principle]
feature-flag: null
created: 2026-09-11
accepted: 2026-09-10
implemented:
spec-sections: ["6.1:45", "4.10:3"]
superseded-by:
relates: ["RUE-1904", "RUE-1886", "RUE-1894", "RUE-981", "ADR-0087"]
---

# ADR-0094: No default arguments: every call spells every argument

## Status

Accepted on 2026-09-10 by Steve (the RUE-1904 ruling). This record formalizes
the existing parameter grammar as a permanent language principle, adds a
targeted diagnostic for the refused spelling, and introduces no preview
feature.

## Decision

A parameter declaration never carries a default value, and a call supplies an
argument for every parameter. `fn connect(host: str, timeout: i32 = 30)` is
rejected at the `=` with a diagnostic that names the rule and the
alternatives; `connect(host)` against a two-parameter signature stays the
arity error it is today, and nothing is filled in from the declaration. The
values a call passes are therefore always visible at the call site, the same
locality Rue requires of mutation through `inout`; there is no rule to learn
about when a default is evaluated, and no partial overload resolution over
which parameters were supplied (ADR-0087). An API with a common configuration
spells it as a distinct function name per arity, a named constructor, or a
configuration value the caller builds explicitly. Argument labels (RUE-1886)
remain a separate decision that this one does not depend on. Struct-literal
defaults (RUE-981) are decided on their own merits and are not a back door for
call defaults.

## References

- [Specification rule 6.1:45](../spec/src/06-items/01-functions.md)
- [Specification rule 4.10:3](../spec/src/04-expressions/10-call-expressions.md)
- [ADR-0087: No function overloading: one name, one signature per scope](0087-no-function-overloading-one-name-one-signature.md)
