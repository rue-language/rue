+++
title = "Expressions"
weight = 4
sort_by = "weight"
template = "spec/section.html"
page_template = "spec/page.html"
+++

# Expressions

This chapter describes expressions in Rue.

{{ rule(id="4.0:1") }}

An expression is a syntactic construct that evaluates to a value.

{{ rule(id="4.0:2", cat="normative") }}

Every expression has a type.

## Evaluation

{{ rule(id="4.0:3", cat="normative") }}

When an expression contains subexpressions, those subexpressions are evaluated in a defined order as specified in this section.

{{ rule(id="4.0:4", cat="normative") }}

For binary operators, the left operand is evaluated before the right operand.

{{ rule(id="4.0:5", cat="normative") }}

For function call expressions, the callee expression is evaluated first, then arguments are evaluated left-to-right.

{{ rule(id="4.0:6", cat="normative") }}

For index expressions of the form `base[index]`, the base expression is evaluated before the index expression.

{{ rule(id="4.0:7", cat="normative") }}

For field access expressions of the form `base.field`, the base expression is evaluated before the field is accessed.

{{ rule(id="4.0:8", cat="normative") }}

Logical operators `&&` and `||` are an exception to the normal left-to-right evaluation. They use short-circuit evaluation as specified in section 4.4: the right operand may not be evaluated depending on the value of the left operand.

{{ rule(id="4.0:9", cat="normative") }}

For struct literal expressions of the form `Type { field1: expr1, field2: expr2, ... }`, the field initializer expressions are evaluated in source order (left-to-right as written).
