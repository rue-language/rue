+++
title = "Allocation Failure"
weight = 6
template = "spec/page.html"
+++

# Allocation Failure

{{ rule(id="8.6:1", cat="dynamic-semantics") }}

Any safe, infallible operation that allocates or grows owned storage traps on
allocation failure rather than returning a null pointer or publishing an
invalid owner. This includes constructors; unit-returning `StrBuf` operations
such as `push`, `push_str`, and `reserve`; `@read_line`; and safe standard
library owners such as `ArrayBuf` and collections built on it. Capacity
arithmetic that cannot be represented is treated as allocation failure.

{{ rule(id="8.6:2", cat="dynamic-semantics") }}

The allocation-failure trap writes exactly `panic: out of memory` followed by
a newline to standard error and terminates the program with exit code 101. The
trap path itself performs no heap allocation.

{{ rule(id="8.6:3", cat="dynamic-semantics") }}

When a safe owner attempts to grow using reallocation, it does not replace its
stored pointer or capacity until reallocation succeeds. On failure, the
original allocation and its initialized contents remain valid until the
allocation-failure trap terminates the program.

{{ rule(id="8.6:4", cat="normative") }}

The raw `@alloc`, `@alloc_zeroed`, and `@realloc` intrinsics are not safe,
infallible allocation APIs. Allocator failure returns null as specified by
§9.2, and does not itself produce the safe allocation-failure trap; checked
code using these raw intrinsics is responsible for handling a null result.
`@resize` likewise never traps: it reports refusal as `false`.
