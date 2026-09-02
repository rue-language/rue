# Compiler error and specification index

Rue publishes a deterministic JSON index from the compiler and specification
source authorities:

```console
./buck2 run //crates/rue-spec:machine-index
```

The top-level `schema_version` is currently `1`. Consumers must reject versions
newer than they support. Fields may be added in a future version; removing a
field or changing its meaning requires incrementing the version.

The `errors` array contains every public `rue-error::ErrorCode`, ordered by
numeric code. `code` and `name` come directly from the compiler declarations;
`title` is their deterministic human-readable spelling. `source_path` records
the repository authority. This metadata does not change compiler diagnostic
text or the separate `--error-format json` schema.

The `spec_rules` array contains every normative specification paragraph,
ordered by paragraph ID. Each entry records its rule ID, normative category,
nearest enclosing Markdown heading as its title, repository source path,
rendered anchor, and canonical `rue-lang.dev` URL. Informative and example
paragraphs remain present in the traceability report but are not language rules
in this index.

Canonical URLs consume the website's base URL, rule shortcode, and shared
`website/spec-route-root.txt` mount authority as declared Buck inputs. Route
projection matches the repository's checked Zola subset: directory components
must already use lower-case ASCII letters, digits, and hyphens, while a page
file stem may fold ASCII case. Underscores, dots, non-ASCII components, and
upper-case directories fail the index gate rather than being approximately
slugified.

The `error_spec_relationships` array contains only relationships proven by an
existing spec case that cites the rule through its structured `spec` field and
either declares a validated `expected_error_code`, asserts an exact `E`-code
token through `error_contains`, or supplies that code in a diagnostic header in
`expected_error`. Typed declarations are checked after parameter expansion
against the compiler-owned inventory, and the ordinary spec run requires the
real compiler to emit exactly one error with that code through
`--error-format json`. Each relationship carries the proving case and its
source path. Mentions in descriptions, source comments, rendered source
excerpts, diagnostic prose, and issue text are deliberately ignored; absence
from this array means “not mechanically proven by current metadata,” not
“unrelated.”

`//:compiler-spec-machine-index` is the drift gate. It validates source IDs,
code references, uniqueness, ordering, and byte-for-byte reproducibility. The
`rue-error` unit tests separately prove that declared code values are unique
and that all non-driver codes are covered by the exhaustive `ErrorKind::code`
mapping.
