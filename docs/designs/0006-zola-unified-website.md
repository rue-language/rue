---
id: 0006
title: Unified Zola Website
status: implemented
tags: [tooling, documentation]
feature-flag:
created: 2025-12-23
accepted: 2025-12-23
implemented: 2025-12-23
spec-sections: []
superseded-by:
---

# ADR-0006: Unified Zola Website

## Status

Implemented

## Summary

Consolidate the website from a Zola + mdbook hybrid to a unified Zola-only solution. The language specification currently built with mdbook will be migrated to Zola using custom templates that integrate seamlessly with the existing website styling.

## Context

The current website architecture uses two separate static site generators:

1. **Zola** - Main website (homepage, getting started, blog)
2. **mdbook** - Language specification (`docs/spec/`)

This creates several pain points:

- **Two build systems**: The `website/build.sh` script must orchestrate both builds, copy mdbook output to `static/spec/`, and maintain synchronization
- **Styling inconsistency**: mdbook has its own theme system separate from the Tailwind-based Zola site
- **Navigation fragmentation**: The spec is served as a static subdirectory with its own navigation, requiring a "back to site" JavaScript hack
- **Two templating systems**: mdbook uses Handlebars; Zola uses Tera
- **Custom preprocessor**: The `mdbook-spec` preprocessor handles rule ID rendering, but could be a Zola shortcode or filter

### Current mdbook Features Used

The specification uses these mdbook features:
- **SUMMARY.md** for sidebar navigation structure
- **Search** (built-in mdbook feature)
- **Custom preprocessor** (`mdbook-spec`) for `r[X.Y:Z]` rule rendering
- **Theme customization** (CSS for 3-column grid layout)

### Test Framework Integration

The `rue-spec` crate's traceability system parses spec markdown files to extract `r[X.Y:Z]` rule definitions. This system is path-agnostic - it finds all `.md` files under the spec directory. Moving to Zola won't break this as long as:
1. The markdown files remain accessible
2. The `r[X.Y:Z]` syntax is preserved in the source files

## Decision

Migrate the specification to Zola using a custom "book" template inspired by Zola's [book theme](https://github.com/getzola/book) and mdbook's layout, but integrated with the existing Rue website styling.

### Architecture

```
docs/spec/src/                 # Spec content (unchanged location)
├── _index.md                  # Spec index (new, for Zola)
├── 01-introduction.md
├── 02-lexical-structure/
│   ├── _index.md
│   ├── 01-tokens.md
│   └── ...
└── ...

website/
├── content/
│   ├── _index.md              # Homepage
│   ├── getting-started.md     # Getting started
│   ├── blog/                  # Blog
│   └── spec -> ../../docs/spec/src/  # Symlink to spec
├── templates/
│   ├── base.html              # Existing
│   ├── spec/
│   │   ├── base.html          # Spec layout with sidebar
│   │   ├── section.html       # Chapter index pages
│   │   └── page.html          # Individual spec pages
│   └── shortcodes/
│       └── rule.html          # Rule ID shortcode
├── static/
│   └── css/
│       └── spec.css           # Spec-specific styling
└── config.toml
```

### Rule ID Handling

Replace the mdbook preprocessor with a Zola shortcode:

**Before (mdbook preprocessor)**:
```markdown
r[3.1:1#normative]
A signed integer type is one of: `i8`, `i16`, `i32`, or `i64`.
```

**After (Zola shortcode)**:
```markdown
{{ rule(id="3.1:1", cat="normative") }}
A signed integer type is one of: `i8`, `i16`, `i32`, or `i64`.
```

The shortcode generates the same HTML structure:
```html
<div class="rule" id="r-3.1:1">
  <a class="rule-link" href="#r-3.1:1">[3.1:1]</a>
</div>
```

**Migration script**: A one-time script will convert all `r[X.Y:Z]` patterns to shortcode syntax.

### Traceability System Update

The `rue-spec` traceability parser must be updated to recognize both:
1. The new shortcode syntax: `{{ rule(id="3.1:1", cat="normative") }}`
2. The original pattern (for backwards compatibility during migration)

This is a simple regex change in `crates/rue-spec/src/traceability.rs`.

### Sidebar Navigation

Zola's section system with `weight` front matter provides ordered navigation:

```markdown
+++
title = "Integer Types"
weight = 1
template = "spec/page.html"
+++
```

The spec template will generate a sidebar from the section hierarchy, similar to mdbook's SUMMARY.md-driven navigation.

### Search

Zola has built-in search index generation. Enable with:
```toml
build_search_index = true
```

The spec template will include a search box styled to match the site.

### Dark Mode

The spec pages will inherit the existing dark mode toggle from `base.html`, eliminating the need for mdbook's separate theme system.

## Implementation Phases

- [x] **Phase 1: Create spec templates**
  - Create `templates/spec/base.html` with sidebar layout
  - Create `templates/spec/section.html` and `page.html`
  - Create `templates/shortcodes/rule.html`
  - Add spec CSS to `css/input.css` with 3-column grid layout

- [x] **Phase 2: Migrate content**
  - Convert `r[X.Y:Z]` to shortcodes via migration script
  - Symlink spec content from `website/content/spec` to `docs/spec/src/`
  - Add front matter to all spec pages
  - Convert README.md to `_index.md` for Zola sections

- [x] **Phase 3: Update test framework**
  - Update traceability parser to handle shortcode syntax
  - Verify 100% coverage maintained

- [x] **Phase 4: Clean up**
  - Remove mdbook build from `website/build.sh`
  - Remove `docs/spec/tools/mdbook-spec/`
  - Remove `docs/spec/theme/`
  - Update CLAUDE.md documentation
  - Delete `docs/spec/book.toml`

## Consequences

### Positive

- **Single build system**: One `zola build` command, no orchestration needed
- **Consistent styling**: Spec uses same Tailwind classes as rest of site
- **Unified navigation**: Spec pages have site header/footer, native navigation
- **Simpler maintenance**: One templating language (Tera), one CSS system
- **Faster builds**: No mdbook preprocessing step
- **Better dark mode**: Inherits site-wide theme toggle

### Negative

- **Migration effort**: One-time conversion of ~50 markdown files
- **Shortcode verbosity**: `{{ rule(id="3.1:1", cat="normative") }}` is more verbose than `r[3.1:1#normative]`
- **Learning curve**: Contributors must learn Zola's shortcode syntax

### Neutral

- **Search**: Zola's search is comparable to mdbook's
- **Rule link references**: The `[text]: rule.id` syntax won't work; authors must use explicit links

## Open Questions

1. **Should we keep `docs/spec/src/` as the source of truth and have Zola reference it?**
   - Pro: Keeps spec near compiler code
   - Con: Complicates Zola content structure
   - **Decision**: Keep in `docs/spec/src/` - Zola will reference via symlink or config

2. **What about the shortcode verbosity?**
   - Alternative: Create a Zola filter that processes raw markdown
   - This would require writing a Rust Zola plugin
   - **Decision**: Use shortcodes initially; revisit if verbosity is problematic

## Future Work

- Consider a Zola plugin to process `r[X.Y:Z]` natively (like the mdbook preprocessor)
- Add PDF export of specification
- Add version selector for multiple spec versions

## References

- [Zola book theme](https://github.com/getzola/book) - Inspiration for sidebar layout
- [Rust Reference mdbook-spec](https://github.com/rust-lang/reference/tree/master/tools/mdbook-spec) - Original preprocessor inspiration
- Current build: `website/build.sh`
