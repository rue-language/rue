---
title: Implementation Internals
description: Arena representation and deterministic compiler workload.
template: reference
nav_order: 30
---
# Implementation Internals

## Pipeline {#pipeline}

The manifest parser creates a pooled-string site model. Front matter and block
parsing add POD records to arenas. Inline parsing adds links, graph validation
resolves pages and anchors, and a queue plus bitset proves reachability.

```text
manifest -> documents -> arenas -> graph -> renderer -> files
```

An independent source scanner counts headings, links, fences, lists, quotes,
rules, and tables. Its inventory must agree with the canonical parser before
rendering. An arena verifier then checks every range and reference.

## Determinism

Navigation keys are sorted before rendering. Search records follow manifest
order. A build renders each page twice and refuses to write if the byte streams
differ. Stress mode repeats the fingerprint across a 200-page cycle.

## Standard library exercise

- `StrBuf` owns source, pooled strings, HTML, JSON, XML, and CSS.
- `ArrayBuf` stores every model arena.
- `StrMap` resolves source and output paths.
- `Queue` and `BitSet` implement graph traversal.
- `sort` orders navigation and diagnostics.
- `env` and `fs` drive the real command-line build.

See the [validation reference](reference.html#validation) and the
[authoring guide](guide.html#getting-started).
