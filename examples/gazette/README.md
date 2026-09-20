# Gazette

Gazette is a static site generator written in Rue. It builds the production
website and is also the application measured by ADR-0072's runtime benchmark.
Both use the same program and rendering pipeline.

Run `website/build.sh` to build the complete website into `website/public`, or
`website/build.sh serve` to preview on port 1111 (`--port PORT` selects another
port). Preview watches inputs and reloads the browser after each successful
build. Content and template edits reuse prepared repository data; changes to
its source inputs refresh it. Failed builds keep the last good preview and
show diagnostics in the browser. The script prepares the specification, compiler
error pages, performance data, homepage status, source excerpts and Tailwind CSS,
then compiles and runs `//examples:gazette`. The Website workflow checks the
generated artifacts before uploading them to GitHub Pages.

For another site, build `//examples:gazette` and invoke its executable with
`build SITE_DIR -o OUTPUT_DIR [--base-url URL] [--list] [--check]`. The site directory holds
`config.toml`, `content/`, `templates/`, and optionally `static/`. Use a fresh
output directory; the website script stages and replaces its output tree.
`--check` validates rendered `href`, `src`, `poster`, and `action` references
against the output inventory, including local fragments and duplicate IDs.
It checks HTML and SVG, reports the source page and output path, and skips
external destinations. The website build always enables it.

The production feature set includes:

- TOML front matter and configuration, including multiline arrays of inline
  tables; page bundles and explicit case-preserving routes.
- Markdown, shortcodes, template inheritance, recursive macros, array
  expressions, integer addition, `is starting_with`, and the `get` filter.
- `load_data(path="…", format="json")`, with nested JSON values and numeric
  comparisons that retain fractional values without Rue floating-point types.
- Paginated section listings, redirects, root and section RSS feeds, search
  documents, and sitemap/robots/default 404 files with `generate_metadata = true`.
- `generate_content_metadata = true` adds shared rendered-body metadata to
  `page` and `section`: `plain_text` and a flat `toc` whose entries carry the
  emitted heading `id`, visible `title`, `level`, and canonical `permalink`.
  The same scan powers the optional search index. Search output uses
  `format_version: 2`; each page document has `page_ref`, an empty `heading`,
  and plain-text `body`, while each anchored heading document carries its
  fragment `ref`, parent `page_ref`, section title, and section body.
- Page and site metadata may provide `extra.social_image` (a site-relative
  asset path) and `extra.social_image_alt`; templates use these for social
  preview tags and retain their existing defaults when absent.
- Templates receive `current_url` for the actual rendered route, including
  paginated listings. The website uses it for canonical and social URLs.
- Optional HTML whitespace minification that preserves code, raw text and
  quoted attributes.
- Optional syntax highlighting and generated light/dark theme stylesheets.

Highlighting is configured under `[markdown]`: `highlight_code = true`,
`highlight_theme = "css"`, `syntax_definitions = "syntaxes/gazette.json"`, and
`highlight_themes_css = [{ theme = "gruvbox-dark", filename = "syntax-dark.css" }]`.
The JSON definitions describe each language's vocabulary and lexical delimiters;
the scanner contains no site-specific language names. See the production
definitions for Rue, EBNF and Bash. The first word of a fence's info string selects
the language; unknown languages remain escaped plain text. Gazette does not read
Sublime syntax definitions or claim their full grammar support.

`build_search_index = true` with `[search] index_format = "gazette_json"` emits
`search_index.en.json`, containing a `documents` array with `ref`, `title`,
`description`, `path`, `page_ref`, `heading`, and plain-text `body` fields. Page
documents cover the introduction (or the whole body when there are no anchored
headings); heading documents cover one anchored section through the next. The
website's search client indexes these documents with Elasticlunr, retaining its
tokenization, stemming, field boosts and result snippets.

The benchmark uses `examples/gazette/config.toml` and its template port. It keeps
content metadata, highlighting, search and minification disabled and pagination removed during
fixture preparation so the existing cross-tool work comparison remains valid.
The production features are not claimed as measured benchmark work. Corpus,
template and configuration identities continue to delimit comparable runs.

Validation includes `//examples:gazette-tests`, the `examples_gazette` CLI cases,
`scripts/gazette-corpus-diff.py`'s body/site/golden/peers checks, the compiler
keyword-to-highlighter vocabulary check, and `scripts/check-website.py` over the
production output. These cover the supported website language rather than
claiming complete TOML, Tera, CommonMark or Zola compatibility.
