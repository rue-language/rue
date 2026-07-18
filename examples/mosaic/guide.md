---
title: Authoring Guide
description: How to configure and write a Mosaic site.
template: article
nav_order: 10
---
# Authoring Guide

## Getting started {#getting-started}

Create a manifest and at least one Markdown-like document. A page declaration
maps its source to a flat output path:

```text
page = guide.md | guide.html | Guide | 10
```

Run `mosaic check site.mosaic` before `mosaic build site.mosaic`. Both commands
use the same parser and graph validator, so a successful check predicts the
semantic result of the build without writing files.

## Inline markup

Use *emphasis*, **strong emphasis**, `inline code`, and
[cross-page links](reference.html#validation). Escape \* punctuation when it
should remain literal.

## Lists and quotes

1. Put the root page first in the manifest.
2. Give headings stable explicit anchors when they are public API.
3. Keep every published page reachable through an internal link.

> Draft pages are parsed but excluded from navigation and generated outputs.

Return to the [handbook](index.html#welcome).
