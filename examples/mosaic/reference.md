---
title: Command Reference
description: Mosaic commands, validation, and generated artifacts.
template: reference
nav_order: 20
---
# Command Reference

## Commands

| Command | Side effects | Result |
| --- | --- | --- |
| `build` | writes files | HTML and shared assets |
| `check` | none | graph validation |
| `dump` | none | arena inventory |
| `stats` | none | deterministic counts |
| `stress` | none | scaling invariants |

## Validation {#validation}

Mosaic rejects duplicate page paths, duplicate anchors, missing titles,
malformed front matter, broken page or fragment links, unreadable inputs, and
unreachable published pages. External `https://` and `mailto:` links do not add
edges to the local graph.

## Outputs {#outputs}

Each published page becomes HTML. A build also writes `mosaic.css`,
`sitemap.xml`, and `search-index.json`. Content is escaped according to its
HTML text, attribute, URL, XML, or JSON context.

Read the [pipeline details](internals.html#pipeline) or return [home](index.html).
