+++
title = "Types"
weight = 3
sort_by = "weight"
template = "spec/section.html"
page_template = "spec/page.html"
+++

# Types

This chapter describes the type system of Rue.

{{ rule(id="3.0:1") }}

Every value in Rue has a type that determines its representation in memory and the operations that can be performed on it.

## Zero-Sized Types

{{ rule(id="3.0:2", cat="normative") }}

A zero-sized type (ZST) is a type with a size of zero bytes. Zero-sized types can be instantiated and passed by value, but they occupy no storage.

{{ rule(id="3.0:3", cat="normative") }}

The following types are zero-sized:
- The unit type `()`
- The never type `!`
- Empty structs (structs with no fields)
- Zero-length arrays `[T; 0]` for any type `T`

{{ rule(id="3.0:4", cat="normative") }}

Zero-sized types have an alignment of 1 byte.
