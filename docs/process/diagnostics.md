# Machine-Readable Diagnostics (`--error-format`)

`rue --error-format <fmt>` selects how compiler diagnostics are presented.

| Format | Meaning |
|--------|---------|
| `text` (default) | Human-readable diagnostics with source snippets, carets, and colour. |
| `json` | Machine-readable structured diagnostics for editors, LSP bridges, and CI. |

```bash
rue --error-format json main.rue -o prog
rue --error-format=json main.rue -o prog   # both spellings work
```

The flag is orthogonal to `--log-format` (see [logging.md](logging.md)):
`--log-format` controls the *tracing* stream (`--log-level`, `--time-passes`),
`--error-format` controls *program diagnostics*. Setting one does not change
the other.

## Stream and framing

Under `--error-format json`:

- **All diagnostics go to stderr.** Nothing else does. The success banner
  (`Compiled main.rue -> prog (...)`) and every `--emit` artifact go to
  **stdout**, so a consumer can treat the entire stderr stream as structured
  output without filtering.
- **Every non-empty stderr line is one JSON array of diagnostic objects.** The
  compiler emits diagnostics in batches (the warnings of a successful compile;
  the errors of a failed one), one batch per line. A batch with nothing to
  report prints nothing rather than `[]`.
- A batch is never a bare object. A single diagnostic — an output-publication
  failure, a compiler panic — is still wrapped in a one-element array, so a
  consumer parses one shape (RUE-436).
- Exit status is unchanged by the flag: `0` on success, `1` on a rejected
  program or a failed publication.

```console
$ rue --error-format json main.rue -o prog
[{"code":"E0206","helps":[],"message":"type mismatch: expected i32, found bool","notes":[],"severity":"error","spans":[{"column":20,"end":23,"file":"main.rue","label":null,"line":1,"primary":true,"start":19}],"suggestions":[]}]
```

## Diagnostic schema

Each element of a batch is an object with exactly these seven keys. Object keys
are serialized in alphabetical order; consumers must not depend on key order.

| Key | Type | Meaning |
|-----|------|---------|
| `severity` | `"error"` \| `"warning"` | Diagnostic class. |
| `code` | string | Error code, e.g. `"E0206"`. **Warnings are not yet coded and carry `""`.** |
| `message` | string | The primary, human-readable message. Never empty. |
| `spans` | array of span objects | Source locations. May be empty (a diagnostic with no location in the user's program). |
| `suggestions` | array of suggestion objects | Machine-applicable fixes. May be empty. |
| `notes` | array of strings | Contextual footnotes. |
| `helps` | array of strings | Actionable advice footnotes. |

### Span object

| Key | Type | Meaning |
|-----|------|---------|
| `file` | string | Source path as the compiler observed it. Never empty. |
| `start` | integer | Start byte offset into that file, 0-indexed. |
| `end` | integer | End byte offset, exclusive. Never less than `start`. |
| `line` | integer | Line of `start`, **1-indexed**. |
| `column` | integer | Column of `start`, **1-indexed**, counting Unicode scalar values (not bytes or UTF-16 code units). |
| `label` | string \| `null` | Label text for a secondary span; `null` on the primary. |
| `primary` | boolean | Whether this is the diagnostic's primary span. |

When `spans` is non-empty, exactly one span has `"primary": true` and it is
**`spans[0]`**; every later entry is a secondary label span. A diagnostic that
has no location at all reports `"spans": []`.

### Suggestion object

| Key | Type | Meaning |
|-----|------|---------|
| `message` | string | What the fix does. |
| `file` | string | File the replacement applies to. |
| `start` / `end` | integer | Byte range to replace. |
| `replacement` | string | Text to substitute for that range. |
| `applicability` | string | How safe the fix is to apply automatically. |

## Internal compiler errors

Both halves of the ICE surface are structured under `--error-format json`:

- A **graceful ICE** (`ice_error!` → `ErrorKind::InternalError`) is an ordinary
  `CompileError` and travels the normal diagnostic path, with code `E9000`.
- A **compiler panic** is caught by the driver's panic hook and published as a
  `E9000` diagnostic with `"spans": []` — a panic has no location in the user's
  program, and inventing one would be a lie. The panic payload is appended to
  `message`; the panic site, compiler version, and (when `RUST_BACKTRACE` is
  set) the backtrace are carried in `notes`. The default Rust
  `thread 'main' panicked at ...` banner is suppressed in this mode precisely
  because it is not JSON.

Either way the string `internal compiler error` appears in `message`, so the
existing harness ICE detector (`rue_test_runner::ice_message`) still catches
crashes hiding inside a JSON stream.

## Ordering

Two pressures decide diagnostic order, and only one of them picks a specific
order. Reproducibility is why an order is defined at all: two runs over the same
inputs owe a consumer byte-identical output, whatever `-j`/`--jobs` was. But
reproducibility is satisfied by *any* fixed order, so it cannot choose one.
Usefulness chooses: a person reads a program top to bottom and wants its first
complaint first.

So the CLI sorts a batch into **source order** — by the path of the file each
diagnostic lands in, then by byte offset within that file — before handing it to
a formatter, and the JSON formatter preserves that order exactly (it never
re-sorts or groups by file, unlike the text formatter's per-file snippet
rendering). The sort is stable, so diagnostics sharing a location keep the
publication order upstream has already fixed. Diagnostics with no span belong to
the compilation rather than to any one line, and come first.

This is ADR-0063's rule made real: "execution order never determines
presentation order", with batches sorted by "stable source identity, current
source positions, and producer order" — the path, the anchor, and (via the
stable sort) the order upstream published in.

The sort lives at the render boundary (`DiagnosticOutput` in
`crates/rue/src/main.rs`), not upstream in the query engine. The engine's own
canonical ordering answers a different question — which diagnostics are the same
diagnostic, for red/green identity — and ADR-0063 deliberately keeps current
source locations out of that comparison: they are "a separately stamped
presentation projection and do not participate in semantic terminal equality".
Ordering by position there would couple a cursor's position back into the
equality that decides stamp reuse, so editing a line above a diagnostic would
invalidate the world. Downstream of every stamp, position costs nothing.

CLI cases pin this with `json_diagnostic_order`, which asserts the exact
`"<severity> <code> <file>:<line>:<column>"` sequence across the whole stderr
stream (an absent field renders as `-`) — see
`crates/rue-cli-tests/cases/diagnostic_json.toml`.

## Known gap

Driver-level failures raised *before* a compilation snapshot exists — a missing
root file, a broken toolchain, a hermetic-build denial — are still printed as
plain text (`Error reading main.rue: No such file or directory`) even under
`--error-format json`. They carry no error code today, so they have no schema
to be rendered into. Consumers should treat a non-JSON stderr line as such a
driver failure rather than a parse bug.

## Testing the surface

`crates/rue-cli-tests` validates the schema rather than substrings. A case with

```toml
json_diagnostics = true
json_diagnostic_order = ["error E0206 main.rue:1:17"]
```

parses **every** stderr line, rejects any object whose key set is not exactly
the documented one, checks span/suggestion field types and the 1-indexed
line/column invariant, and then compares the full ordered digest. Malformed
JSON, a dropped field, a renamed key, a demoted primary span, or a reordered
batch each fail the case.
