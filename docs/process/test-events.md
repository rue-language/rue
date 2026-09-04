# The `rue test` Event Stream

`rue test <root.rue>` builds one test image for the root module's `@import`
closure and runs one process per selected test. Its **primary output is a
versioned NDJSON event stream on stdout**; the human rendering is a consumer of
that same stream in the same process, never a separate computation.

This document is the schema's normative description (ADR-0083 §2). It is the
companion of [diagnostics.md](diagnostics.md), which owns the *other* machine
surface: `--error-format json` compiler diagnostics on stderr. The two are
orthogonal and stay on their own streams.

```bash
rue test app/main.rue
rue test app/main.rue --format json
rue test app/main.rue --list --format json
```

Test declarations are stable (RUE-1955), so no preview flag is needed here or
for an ordinary build of the same closure.

## Streams

- **stdout is the runner's surface.** Every line is one event, so a consumer
  can read the whole of stdout as the stream. Under `--format human` the same
  events are rendered as text on stdout instead.
- **stderr is the compiler's surface**, byte-for-byte as
  [diagnostics.md](diagnostics.md) pins it, plus the runner's own warnings
  (unimported test files) and notices (the missing-inventory note), and its
  one-line reason for a nonzero exit. Those are stderr's alone, once: the human
  renderer does not repeat them on stdout, where a terminal joining the streams
  would show two copies of one line. The warnings are written in both formats;
  the missing-inventory note is presentation and so is written in the human
  format only, because `--format json` publishes the same fact as
  `run_finished.test_candidates`.
- **No event is emitted before the test image exists.** A compile failure is
  diagnostics on stderr, an empty event stream, and exit `2` — never a
  `run_started` for a run that never began.

## Exit codes

| Code | Meaning |
|------|---------|
| `0` | Every selected test passed. |
| `1` | At least one selected test failed, timed out, or crashed. |
| `2` | The run could not be performed: a compile failure, a failing image link, an ICE, a bad flag combination, an unreadable root or candidate inventory, or a runner error. |
| `3` | The selection was empty. |

`3` is a distinct outcome rather than a vacuous success because an empty
selection is how a typo becomes false evidence. A run whose selection is empty
still emits `run_started` and `run_finished` (with zero counts) so a machine
consumer sees a complete stream, and names on stderr which emptiness it was —
`error: no tests matched the selection` when the closure has tests that nothing
selected, or `error: the compiled closure declares no tests; a test-only file
must be reached by an @import` when it has none at all. Those are different
mistakes with different fixes. `--list` reports the same two on stderr and emits
no records.

## The exec contract

The image's interface is public by commitment (ADR-0083 §5.4): an alternative
runner may enumerate with `--list --format json` and execute through exactly
this contract. See [runtime-abi.md](../runtime-abi.md) for the runtime helpers
that implement its channel half.

| Aspect | Value |
|--------|-------|
| `argv` | `["rue-test", "<ordinal as 16 lowercase hex digits>"]` |
| `envp` | exactly `["RUE_TEST=1"]` |
| working directory | a fresh private scratch directory |
| stdin | `/dev/null` (immediate EOF) |
| descriptor 3 | the write end of the structured failure channel |
| process group | its own; the runner kills the group on expiry and again after exit |

These are pinned values, not conveniences. The loader lays the real `argv` and
`envp` strings on the initial process stack before any Rue code runs, so their
sizes are stack consumption no later pointer swap can undo; pinning them is what
will make a keyed configuration's stack consumption deterministic for the
deferred verdict cache (ADR-0083 §6). The dispatcher consumes the selector and
then narrows the visible argument count to one, so a test observes
`argv[0] == "rue-test"` and never the selector it was dispatched by.

A malformed selector is the image's own error: it writes
`rue-test: expected one 16-hex-digit test selector` to stderr and exits `2`.

### The failure channel

Descriptor 3 carries newline-delimited JSON, drained by the runner under its own
budget. Two record kinds exist in this version, and `failure` has two shapes:

```json
{"record":"complete","schema":"1.0"}
{"record":"failure","schema":"1.0","kind":"...","message":"...","location":{"file":"...","line":0,"column":0},"payload":"..."}
{"record":"failure","schema":"1.0","kind":"...","message":"...","location":{"file":"...","line":0,"column":0},"left":"...","right":"..."}
{"record":"failure","schema":"1.0","kind":"assert","message":"...","location":{"file":"...","line":0,"column":0}}
```

The `complete` record is written by the dispatcher's epilogue alone, after the
selected test body returns normally. `failure` records are written by assertion
sugar and by the test-body `?` failure arm before aborting; `kind` is an open
field, so a user assertion library's own kind is published verbatim rather than
flattened (ADR-0083 §5.1).

A `failure` record carries **at most one of** `payload` and the pair
`left`/`right`, and a bare `@assert` carries neither: its record ends at the
location object, because it has no operands to report and nothing structured to
put in the open field. `payload` is the open, versioned extension point §5.1
reserves for
an assertion library with something structured to say in one string.
`left`/`right` is the shape a **comparison** assertion writes: they are the two
operands in the order the source spelled them and are never reordered or
labelled by role, so `@assert_eq(got, want)` reads the way it is written — each
rendered by the compiler-synthesized structural printer under the same rules and
the same 4 KiB bound the test-body `?` payload uses. The runner has no notion of
which operand is expected and which observed. The conventional spelling puts the
observed value first, as Go's `got, want` idiom and this repository's own tests
do; the human rendering prints `left:` above `right:` with the caret under
`right`, so that spelling shows the wanted value beneath the observed one at the
first differing column. An empty rendering is a value and stays present as an
empty string; a comparison is recognized by the fields' presence, not by parsing
`kind`.

`@assert_eq` and `@assert_ne` are the producers of that pair in this version,
through the `__rue_test_fail_comparison` helper
([runtime-abi.md](../runtime-abi.md)). They are ordinary intrinsics, usable
anywhere `@assert` is, and they lower the same way in a test image and in an
ordinary executable — the only difference is that an ordinary process has no
descriptor 3, so the frame write fails with `EBADF` as designed and the pinned
stderr message is the whole report.

`@assert` reports through the same channel, with `__rue_test_fail_assert`, and
under the same build-independent rule. Its record's `message` is the pinned
`assertion failed` for `@assert(cond)` and the programmer's text for
`@assert(cond, msg)`, so both spellings publish one kind and a consumer never
has to know which was written. Both stderr forms are unchanged: `assertion
failed` and `panic: {msg}` respectively, with status 101 (spec 4.13:5d).

Writes are best-effort by design: an image run by hand has no descriptor 3, and
`EBADF` there is expected rather than exceptional. The channel is **not a
security boundary** — it prevents accidental collision with a test's own stdout,
which is all its consumers are promised.

**Producers in this version.** Every record on this channel is
compiler-synthesized: `@assert`, `@assert_eq`, `@assert_ne`, the test-body `?`
failure arm, and the dispatcher's `complete` epilogue. Nothing in the language
can call `__rue_test_failure_site` or `__rue_test_fail`, so the user assertion
library of ADR-0083 §5.1 is what the open `kind` and the reserved payload shapes
are *for* rather than something that exists yet — a line naming an unknown
`record` is recorded as malformed, which fails the test. A user-callable
intrinsic is tracked as RUE-2027.

Reserved by ADR-0083 §5.1 and §5.2, and produced by nothing in this version:
`promotion` payloads (a machine-applicable suggested fix, for a future
`rue test --accept`) and `sub_result` records (named child results attributed as
`<test-id>/<sub-name>`).

## Stable test identity

A test's stable ID is `<module>::<name>`.

- `<module>` is the module's published identity: project-root-relative for a
  user module. A standard-library module reports its requested path rather than
  the internal trusted spelling.
- `<name>` is the test's string literal verbatim. It may contain spaces and
  punctuation and is never mangled.

The inventory is sorted by byte order over the whole ID — deliberately not
declaration order, module order, or symbol order, because only a property of the
source text keeps a filtered run's ordinals equal to a full run's. A test's
`ordinal` is its index in that order, and equivalently the selector the image
dispatches it on.

## Events

Every event is one JSON object on one line. **Object keys are serialized in
alphabetical order**; consumers must not depend on key order, but the ordering
is fixed so two runs over the same inputs produce byte-identical output — the
same determinism stance [diagnostics.md](diagnostics.md) takes.

Under `--jobs N` with `N > 1`, whole lines from concurrent tests interleave. The
schema promises the content of each line, not an order across concurrent tests;
`test_started` always precedes its own `test_finished`.

### `run_started`

The head event, and the only one carrying the schema version for a run.

| Key | Type | Meaning |
|-----|------|---------|
| `event` | `"run_started"` | |
| `schema` | string | Schema version, `"1.0"`. |
| `root` | string | The root source exactly as the command line spelled it. |
| `target` | string | The Rue target the image was built for. |
| `opt_level` | string | The optimization level's digit: `"0"`–`"3"`. |
| `seed` | integer | The run's shuffle seed (see `--seed`). |
| `jobs` | integer | Concurrent test processes, which is all `--jobs` bounds in test mode; compilation uses auto-detected parallelism. |
| `shard` | string | `"K/N"`. **Absent** when `--shard` was not given. |
| `plan` | object | `{"selected": integer, "total": integer}` — tests selected, and tests in the closure. |

### `test_started`

| Key | Type | Meaning |
|-----|------|---------|
| `event` | `"test_started"` | |
| `id` | string | The stable test ID. |

### `test_finished`

| Key | Type | Meaning |
|-----|------|---------|
| `event` | `"test_finished"` | |
| `id` | string | The stable test ID. |
| `verdict` | string | `"pass"`, `"fail"`, `"timeout"`, or `"crash"`. |
| `duration_ms` | integer | Wall time from spawn to reap. |
| `capability_summary` | object | `{"status":"unavailable"}` — see below. |
| `failure` | object | The failure record. **Absent** on a pass. |
| `stdout` | object | Capture record. |
| `stderr` | object | Capture record. |
| `scratch_dir` | string | The retained scratch directory. **Absent** on a pass, whose directory is deleted. |
| `repro` | array of strings | The argv that reproduces this one test. Always present. |

`capability_summary` is present from v1.0 with an explicit `unavailable` status
rather than omitted. The MVP verifies no hermeticity claim for any test and says
so in-band, so the deferred capability ADR (ADR-0083 §6) can populate a field
consumers already handle instead of adding one.

#### The failure record

| Key | Type | Meaning |
|-----|------|---------|
| `kind` | string | See the taxonomy below. |
| `message` | string | The pinned runtime message, the frame's message, or the runner's description. |
| `exit_code` | integer | The process's exit status. **Absent** when it did not exit normally. |
| `signal` | integer | The signal that killed it. **Absent** otherwise. |
| `location` | object | `{"file","line","column"}` — the test declaration's header, unless a failure frame carried its own site. |
| `payload` | string | The channel's open payload. **Absent** when empty, and a bare `@assert` writes none. |
| `left` | string | A comparison assertion's left operand, rendered. **Absent** on every other failure. |
| `right` | string | Its right operand, rendered. Present exactly when `left` is. |
| `diff` | array | The runner's diff from `left` to `right`. Present exactly when `left` is. |
| `runner_note` | string | The runner's own explanation. **Absent** unless the runner could not trust what it read. |

`line` and `column` are both 1-based, and `column` counts Unicode scalars rather
than bytes — the same coordinate the compiler's own diagnostics print for the
same position, so a report and a diagnostic never disagree about where something
is. An `unhandled_error` record is the one that routinely carries a site of its
own: the failure arm of a test body's `?` stages the position of the `?`
operator's operand, so `location` names the failing expression rather than the
`test` line the runner would otherwise fall back to (spec 6.7:14).

`timeout` and `crash` verdicts also carry a failure record, with kind `timeout`
and `signal` respectively.

##### The comparison diff

`diff` is computed by the runner, not carried on the channel, so the event
stream and the human rendering are one computation and can never disagree about
where two values differ. It is an array of hunks in order:

```json
"diff":[{"op":"equal","text":"4"},{"op":"delete","text":"1"},{"op":"insert","text":"2"}]
```

`op` is `equal` (present in both), `delete` (present in `left`, absent from
`right`), or `insert` (the reverse). Two invariants make the encoding lossless
rather than a rendering: concatenating every `equal` and `delete` hunk's `text`
yields `left` exactly, and concatenating every `equal` and `insert` hunk's
yields `right`. No two adjacent hunks share an `op`, and no hunk is empty, so
two identical values are one `equal` hunk and two empty values are an empty
array.

Granularity follows the values: if either contains a newline the diff is
line-by-line, and each line unit carries its own terminator; otherwise it is
character-by-character, over Unicode scalar values. A common prefix and suffix
are removed before any alignment, and a remaining middle large enough to make an
exact longest-common-subsequence expensive is reported as one wholesale
`delete` followed by one `insert` rather than refined — the invariants above
hold either way.

A rendering that is not valid UTF-8 never reaches the diff: `left` and
`right` are JSON string fields, and a channel line that is not valid UTF-8 is
rejected as malformed before it becomes a frame at all, yielding `fail` with
kind `exit` and a `runner_note` (see Precedence below). There is no second
encoding tag on these fields, unlike a capture record's.

Under `--format human` the same values are printed as `left:` and `right:`
lines, in source order — the labels name the operand positions, not roles. A
single-line pair gets a caret under the first differing character; a multi-line
pair gets a `-`/`+` hunk listing instead, because a caret into a wall of text
locates nothing.

#### Capture records

| Key | Type | Meaning |
|-----|------|---------|
| `encoding` | `"utf8"` \| `"base64"` | How `data` is encoded. |
| `bytes_total` | integer | Every byte the process wrote to this stream — not the size of what was kept. |
| `data` | string | The retained prefix. Present on a **non-pass**. |
| `digest` | string | `sha256:<hex>` over the retained bytes. Present on a **pass**. |

Rue strings are arbitrary byte sequences written raw, so capture is lossless
within its retained window rather than lossy-UTF-8: `encoding` is `utf8` when
the retained bytes are valid UTF-8 and `base64` (standard alphabet, padded)
otherwise.

The asymmetry is deliberate (ADR-0083 §2): a failing test's output is what a
reader needs, and inlining every passing test's output is the wall of green the
design rejects. A pass can never have overflowed its budget — an overflow is a
failure verdict — so a pass's digest always covers the whole stream. This
version has no flag to inline a passing test's output; the opt-in is deferred.

**Budgets.** 1 MiB retained per stream for stdout and stderr; 256 KiB for the
failure channel, deliberately separate so a test that floods its streams cannot
truncate its own failure record. Exceeding a *stream* budget kills the process
group and yields `fail` with kind `output_overflow`, retained prefix attached.
Reading continues past the budget so `bytes_total` is the true count.

That window is sized for a machine, so the human renderer bounds what it prints
of it: at most 64 lines or 8 KiB per stream, as a 48-line head and a 16-line
tail with one line naming the lines and bytes skipped between them. Lines are
never cut through, the `bytes_total` header is unchanged, and `--format json`
still carries the retained window whole — the bound is a display bound, not a
second retention limit.

### `run_finished`

| Key | Type | Meaning |
|-----|------|---------|
| `event` | `"run_finished"` | |
| `passed` / `failed` / `timeout` / `crash` | integer | Counts by verdict. |
| `wall_ms` | integer | Wall time for the whole run. |
| `unimported_test_files` | array | Declared test files outside the closure. **Absent** without `--test-candidates`. |
| `test_candidates` | `"declared"` \| `"none"` | Whether an inventory was supplied. |

Each `unimported_test_files` entry is `{"path": string, "tests": integer,
"parse_failed": boolean}`. `parse_failed` means the candidate could not be read
or parsed, so `tests` counts nothing and the honest answer is that the count is
unknown. Without `--test-candidates`, a human-format run writes
`note: no --test-candidates inventory; unimported test files are not detected`
to stderr after the summary line — silence would be read as "none found" — but
only when the compiled closure holds more than one user module (the standard
library does not count). A closure of one has no second module that could have
failed to import a test file, so the note would answer a question that run
cannot raise; it was noise under every filtered rerun pasted from a `repro:`
line. Both conditions are presentation only: `test_candidates` still answers
`"none"` for such a run, no event carries the closure size, and stdout carries
the summary alone (RUE-2021).

The inventory file is one path per line, each relative to the ROOT MODULE'S
DIRECTORY — the compiler's project root — and its build-side producer is the
`rue_test` rule (`rue_rules.bzl`, ADR-0083's boundary), which writes it from
the target's declared `srcs` and fails the target when this array is non-empty,
since `rue test` itself reports the orphan and still exits `0`.

### `test` (listing records)

`--list --format json` emits one of these per inventory entry **and nothing
else**: no `run_started`, no `run_finished`. Because there is no head event to
carry it, each record names the schema version itself.

| Key | Type | Meaning |
|-----|------|---------|
| `event` | `"test"` | |
| `schema` | string | `"1.0"`. |
| `id` | string | The stable test ID. |
| `module` | string | The module half of the ID. |
| `name` | string | The test's name, verbatim. |
| `file` | string | The declaring file as the compiler observed it. |
| `line` / `column` | integer | 1-indexed position of the `test "name"` header, or `0` when the declaration could not be located in its module's syntax tree. |

`--list --format human` prints one ID per line.

`--list` performs semantic analysis of the test closure and stops: no CFG, no
codegen, no linking, no execution. Two additive surfaces are reserved rather
than shipped (ADR-0083 §2): `--list --cache-status`, which arrives with the
deferred verdict cache and would have to materialize closure terminal
artifacts the default listing must never pay for, and `--list --reaches <item>`.

## The verdict taxonomy

One classifier decides every verdict from one observation of a finished process:
the runner's own supervision outcome, the exit status, the last non-empty line
of stderr, and the frames read from the failure channel.

| Verdict | Failure kind | Produced when |
|---------|--------------|---------------|
| `pass` | — | Exit `0` **and** a `complete` frame was read. |
| `fail` | `incomplete` | Exit `0` with no `complete` frame. |
| `fail` | `assert` | A `failure` frame from a failed `@assert`, carrying the intrinsic's own site and its message. Falls back to the last stderr line being exactly `assertion failed` — which is what a comptime-decidable `@assert_eq` still reports as, and what an older image writes. |
| `fail` | `assert_eq` | A `failure` frame from a failed `@assert_eq`, carrying `left` and `right`. |
| `fail` | `assert_ne` | The same, from a failed `@assert_ne`. |
| `fail` | `trap:<class>` | The last stderr line is another pinned runtime message. |
| `fail` | `unhandled_error` | A `failure` frame with that kind — the test-body `?` failure arm. |
| `fail` | *(verbatim)* | A `failure` frame with a kind the runner does not know (ADR-0083 §5.1). |
| `fail` | `exit` | Any other nonzero exit. |
| `fail` | `output_overflow` | A stream budget was exhausted; the group was killed. |
| `timeout` | `timeout` | The per-test budget expired; the group was killed. |
| `crash` | `signal` | Killed by a signal, SIGPIPE included. The `signal` field carries the number. |

`trap:<class>` classes are `panic`, `div_by_zero`, `overflow`,
`intcast_overflow`, `bounds_check`, `invalid_utf8`, and `stack_overflow`. The
messages they match are the ones `crates/rue-runtime/src/error.rs` and `entry.rs`
write before `exit(101)`; `crates/rue-cli-tests/cases/rue_test.toml` runs real
programs down those paths, so a reworded runtime message fails a case rather than
silently reclassifying a trap as a bare `exit`.

Precedence, in order:

1. **The runner's own supervision** — a timeout or an output overflow — because
   the runner's kill is what produced the signal death that would otherwise read
   as a crash.
2. **A malformed channel line.** An unreadable failure report is never silently
   ignored: the verdict is `fail` with kind `exit` and a `runner_note` saying so,
   even when the process otherwise looks like a pass.
3. **A well-formed `failure` frame**, which carries structure the exit status
   cannot.
4. **The exit status**, and for the runtime's abort status `101`, the pinned
   message on the last non-empty line of stderr. The last line rather than the
   whole stream, so a test that printed diagnostics of its own before tripping an
   assertion is still classified by the trap it took; the comparison against that
   line is exact.

### Reserved values

Published in this taxonomy, produced by nothing in this version, and named here
so a consumer can handle them when they arrive:

- **`skipped`** is reserved **out** of the v1 verdict set. It has no producing
  mechanism: `--filter` removes tests from the selection rather than reporting
  them, and `@skip` is deferred with directive-argument grammar. An unproducible
  verdict in a published enum is a consumer trap, so it is documented rather
  than emitted (this settles ADR-0083's open question).
- **`compile_error`** — a per-test verdict, deferred with ADR-0083 §6. The MVP's
  whole-run compile failure is exit `2`.
- **`cached_pass`** — deferred with the hermetic verdict cache.
- **`ice`** — a reserved failure kind.

## Selection

Three stages, always in this order: **filter, then shard, then shuffle**.
Sharding after filtering is what makes the N shards of a filtered run
reconstitute exactly that run; shuffling last is what keeps the shuffle from
changing which tests a shard owns.

Filtering narrows the **run set, never the analysis root set** (ruled,
ADR-0083 §2): the request still roots every test in the closure, so a filtered
run's verdicts are identical to the same tests' verdicts in a full run.

### `--filter`

**Matching rule:** a test is selected when its stable ID *contains* the pattern
as a substring. `--filter` is repeatable and repeated filters **union**. Matching
is over the whole ID, so `--filter app/lexer_tests.rue` selects one file's tests
and `--filter "parse_port"` selects by name fragment.

### `--shard K/N`

`K` is 1-based. A test belongs to shard `K` when
`fnv1a64(id) % N == K - 1`, where `fnv1a64` is 64-bit **FNV-1a** over the ID's
UTF-8 bytes:

```text
hash = 0xcbf29ce484222325
for byte in id: hash = (hash XOR byte) * 0x100000001b3   (mod 2^64)
```

FNV-1a rather than the standard library's `DefaultHasher`, whose values are
explicitly not stable across Rust releases — a shard assignment that moved when
the compiler was rebuilt would silently drop tests from a sharded CI run. It is
specified here so an external scheduler computes the same partition.

Duration-aware bin-packing is deferred with the scheduling ADR; it needs
recorded-duration history the MVP does not keep.

### `--seed`

The run order is a Fisher-Yates shuffle over a **SplitMix64** stream seeded by
`--seed`:

```text
state += 0x9e3779b97f4a7c15
z = state
z = (z XOR (z >> 30)) * 0xbf58476d1ce4e5b9
z = (z XOR (z >> 27)) * 0x94d049bb133111eb
return z XOR (z >> 31)
```

Indices are drawn by rejection sampling so the modulo bias that would quietly
favour low indices never enters the shuffle. With no `--seed`, the runner derives
one from a fresh OS-seeded random source and reports it in `run_started`, so a
shuffle that surfaced a bug is re-runnable.

`--list` applies `--filter` and `--shard` but **not** the shuffle: a listing is
an inventory, and stable-ID order is what makes two listings comparable.

## Reproduction as data

Every `test_finished` carries the exact argv that reproduces that one test:

```json
["rue","test","app/main.rue","--filter","app/t.rue::parses a port","--seed","417",
 "--target","x86-64-linux","-O0","--timeout-ms","10000"]
```

It selects by the **full stable ID, never the bare name** — two modules may
declare tests with the same name, and a repro that re-runs both is not a repro.
The seed, target, optimization level, any enabled preview features, and the
per-test budget travel with it so the same image is rebuilt, and `--source-manifest` and
`--link-archive` are repeated when they were given. The target and optimization
level are emitted even when they were defaulted: a repro is run later, possibly
elsewhere, and "whatever the host was" is not a reproduction.

The argv array is the authoritative form. The human renderer's `repro:` line is
the same argv shell-quoted for pasting; a consumer should re-execute the array
rather than parse the line.

## Scratch directories and isolation

Each test starts in a fresh private scratch directory, which is **deleted on a
pass and retained on anything else**, with its path in the event. The abort-only
runtime means destructors do not run on a failing path, so the retained
directory plus process death is what teardown-on-failure amounts to
(ADR-0083 §5.4).

Directories live under a per-run directory named for the seed and the runner's
process id, and are themselves named `rue-test-<seed>-<ordinal>`. The run
directory is what keeps two runs that share an explicit `--seed` — a repro next
to the run that produced it, or two suites in parallel — from deleting each
other's live working directories.

**What isolation does and does not promise.** Each test observes fresh process
state, has an independent lifecycle the runner enforces, gets its output
captured and attributed exactly, and receives best-effort process-tree cleanup.
Noninterference is *not* promised: a test doing raw syscalls or FFI can signal
arbitrary processes or contend on shared OS state, and a process group is not
containment. ADR-0083 §3 scopes that clause to verified-hermetic tests, which
the deferred capability ADR introduces; the MVP verifies nothing and claims it
for no test.

## Versioning

This schema follows ADR-0061 §6. The version is `1.0`, published in the head
event of a run and in each `--list --format json` record.

- **Additive changes are minors.** A new event kind, a new optional field, a new
  reserved value becoming producible, or a richer `payload` inside the failure
  record. Consumers must ignore unknown fields and unknown event kinds. The
  failure record's `left`, `right`, and `diff` and the `assert_eq` /
  `assert_ne` kinds arrived this way (ADR-0083 Phase 2.5): the version stays
  `1.0` because nothing a `1.0` consumer already read changed.
- The comparison operands were briefly named `expected`/`actual` and were
  renamed to `left`/`right` in place, still at `1.0` (RUE-1954): the rename
  landed before any consumer outside this repository existed, so it is the one
  exception to the major-version rule below rather than a break in it.
- **Removing or repurposing a field, renaming an event, or changing a field's
  type is a major.** So is producing a verdict this document reserves *out* of
  the taxonomy.
- Changing a field here is a consumer-visible break: update this document and
  the `cli.rue_test` cases in `crates/rue-cli-tests/cases/rue_test.toml` in the
  same change, the way [diagnostics.md](diagnostics.md) and its
  `json_diagnostics` cases move together.

## Testing this surface

`crates/rue-cli-tests/cases/rue_test.toml` runs the real binary end to end: each
verdict against the real runtime, the exit-code contract, the dispatch rule, the
orphan warning, and exact object text for the events — alphabetical keys
included, so a field rename or a dropped field fails a case. A case asserting a
driver subcommand's status uses the harness's `driver_exit_code` field, which
pins an exact status where `compile_fail` can only say "nonzero" and so cannot
tell `1` from `3`.
