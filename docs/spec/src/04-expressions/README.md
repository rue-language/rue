# Expressions

This chapter describes expressions in Rue.

r[4.0:1]
An expression is a syntactic construct that evaluates to a value. Every expression has a type.

## Evaluation

r[4.0:2#normative]
When an expression contains subexpressions, those subexpressions are evaluated in a defined order as specified in this section.

r[4.0:3#normative]
For binary operators, the left operand is evaluated before the right operand.

r[4.0:4#normative]
For function call expressions, the callee expression is evaluated first, then arguments are evaluated left-to-right.

r[4.0:5#normative]
For index expressions of the form `base[index]`, the base expression is evaluated before the index expression.

r[4.0:6#normative]
For field access expressions of the form `base.field`, the base expression is evaluated before the field is accessed.

r[4.0:7#normative]
Logical operators `&&` and `||` are an exception to the normal left-to-right evaluation. They use short-circuit evaluation as specified in section 4.4: the right operand may not be evaluated depending on the value of the left operand.
