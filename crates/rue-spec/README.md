# rue-spec: Specification Test Runner

Spec tests provide traceability between the language specification and tests.
This README holds the full format reference; the day-to-day commands live in
CLAUDE.md's Testing section.

### Specification Tests

The specification test system provides traceability between the language specification and tests.

#### Test Directory Structure

Tests are organized in `crates/rue-spec/cases/` by language feature:

```
cases/
├── lexical/          # Tokens, comments, whitespace
├── types/            # Integer, boolean, unit, never types
├── expressions/      # Literals, operators, control flow
├── statements/       # Let, assignment, expression statements
├── items/            # Functions, structs
├── arrays/           # Fixed-size arrays
├── runtime/          # Intrinsics, runtime behavior
├── golden/           # IR dump tests
└── errors/           # Compile-time error tests
```

#### Test Format

```toml
[section]
id = "expressions.arithmetic"
spec_chapter = "4.2"           # Links to spec chapter
name = "Arithmetic Operators"

# Run-pass test with spec traceability
[[case]]
name = "addition_basic"
spec = ["4.2:1", "4.2:2"]      # Spec paragraphs this test covers
source = "fn main() -> i32 { 1 + 2 }"
exit_code = 3

# Compile-fail test
[[case]]
name = "type_mismatch"
spec = ["4.2:5"]
source = "fn main() -> i32 { 1 + true }"
compile_fail = true
error_contains = "type mismatch"

# Golden test (exact IR output)
[[case]]
name = "simple_add_air"
spec = ["4.2:1"]
source = "fn main() -> i32 { 42 }"
expected_air = """
function main:
air (return_type: i32) {
    %0 : i32 = const 42
    %1 : i32 = ret %0
}
"""

# Target-specific backend golden tests require an explicit target:
# expected_mir, expected_lowering, expected_liveness, expected_regalloc,
# expected_asm, and expected_stackframe.

# Preview feature test (expected to fail)
[[case]]
name = "some_preview_feature"
spec = ["X.Y:Z"]
preview = "test_infra"           # Requires --preview test_infra
source = "..."
exit_code = 0

# Preview feature test (must pass)
[[case]]
name = "some_preview_feature_basic"
spec = ["X.Y:Z"]
preview = "test_infra"
preview_should_pass = true       # Fails CI if this test fails
source = "..."
exit_code = 0
```

#### Preview Feature Tests

Tests for preview features use two fields:
- `preview = "feature_name"` - Marks the test as requiring a preview feature. An ordinary assertion failure is expected and shows as "ignored". Fatal subprocess failures still fail the suite, and a passing assertion is an XPASS failure until its metadata is updated.
- `preview_should_pass = true` - When combined with `preview`, makes the test required to pass. Use this for portions of preview features that are already implemented.

**Workflow for preview features:**
1. Initially, add failing tests with just `preview = "feature_name"` (xfail)
2. As you implement parts of the feature, add `preview_should_pass = true` to tests that should now pass
3. When stabilizing the feature, remove both `preview` and `preview_should_pass` fields

The `preview` field must match a valid `PreviewFeature` variant name. The test runner validates all preview feature names on startup and will fail with a clear error if an unknown feature name is used.

#### Spec Paragraph References

The `spec` field links tests to specification paragraphs using the format `{chapter}.{section}:{paragraph}`:
- `3.1:1` - Chapter 3, Section 1, Paragraph 1
- `4.2:5` - Chapter 4, Section 2, Paragraph 5

#### Filtering by Specification Paragraph

An argument shaped like a specification ID selects the cases citing it, instead
of being matched against test names (`section.id::case_name`):

```bash
scripts/rue spec 4.2       # every case citing a paragraph in section 4.2
scripts/rue spec 4.2:5     # only the cases citing paragraph 4.2:5
scripts/rue spec --spec 4.2:5   # explicit form
scripts/rue spec arithmetic     # ordinary libtest name filter
```

The shared libtest layer rejects an ordinary name filter that selects no cases.
Spec paragraph selectors that select no cases are rejected by the spec runner's
own validation as well. An unfiltered spec run that leaves no cases after its
platform selection remains an error from the spec runner; this shared layer
does not alter that policy.

### Platform Responsibility

Each case declares, structurally, which lane is responsible for executing it:

| Responsibility | What the case asserts | Who runs it |
| --- | --- | --- |
| Semantic | diagnostics, semantics, and target-independent golden IR (tokens/AST/RIR/AIR/CFG) | the Linux-complete lane |
| Native | compiles and runs a real program for the host's target | the Linux-complete lane, plus the native lane of every `only_on` host |
| Backend | architecture-specific golden output for a declared `target` | the Linux-complete lane when it only emits; the matching native lane when it also executes |

The classification is derived from the assertions a case makes, so it cannot
drift from what the case does. Loading the corpus **rejects** a case whose
platform responsibility is ambiguous:

- backend-specific golden output (`expected_mir`, `expected_lowering`,
  `expected_liveness`, `expected_regalloc`, `expected_asm`,
  `expected_stackframe`) with no declared `target` — the expectation belongs to
  one architecture, but nothing says which;
- a `target` whose architecture differs from an `only_on` host;
- a `target` combined with execution assertions and no `only_on` scope, which
  would ask whichever host runs the suite to execute a foreign-architecture
  program.

Because `//:spec-traceability` loads the whole corpus, that gate is where an
ambiguous case surfaces first — a cheap, standalone required check.

#### `only_on` and CI reachability

`only_on` scopes a case to specific hosts. Required CI executes
`x86-64-linux` (complete lane), `aarch64-linux`, and `aarch64-macos`
(native lanes) — the list in `rue_test_runner::CI_EXECUTED_TARGETS`, which
`scripts/validate-ci-gate.py` keeps in lockstep with `.github/workflows/ci.yml`.

`x86-64-macos` is a legal host name so the suite runs on an Intel Mac, but no
required lane is one. A case scoped only to platforms outside the matrix
therefore **does not count as specification coverage**: it still runs for a
developer on that host, but nothing in CI executes it, so it cannot stand as
evidence that a rule holds. The traceability report lists every such case.

#### Focused coverage

A normative rule's coverage must include at least one *focused* case — at most
`FOCUSED_CASE_MAX_SOURCE_LINES` (40) lines of source. A rule whose only evidence
is a large multi-feature program is covered on paper only: when it regresses,
the failure names the program rather than the rule. Large programs remain
valuable as integration and slow-tier coverage; they just cannot be a rule's
sole evidence.


### Language Specification

The formal language specification is in `docs/spec/src/`. It is integrated into the website via Zola.

#### Building the Spec

The spec is built as part of the website:

```bash
./website/build.sh
# Output in website/public/spec/
```

#### Spec Structure

```
docs/spec/src/
├── _index.md               # Spec root (Zola section)
├── 01-introduction.md      # Conformance, definitions
├── 02-lexical-structure/   # Tokens, comments, keywords
├── 03-types/               # Type system
├── 04-expressions/         # All expression forms
├── 05-statements/          # Statement forms
├── 06-items/               # Functions, structs
├── 07-arrays/              # Array types
├── 08-runtime-behavior/    # Overflow, bounds checking
└── appendices/             # Grammar, UB summary
```

#### Spec Paragraph Format

Each paragraph has an ID using the Zola shortcode format `{{ rule(id="X.Y:Z", cat="category") }}`:

```markdown
{{ rule(id="3.1:1", cat="normative") }}
A signed integer type is one of: `i8`, `i16`, `i32`, or `i64`.

{{ rule(id="3.1:2", cat="normative") }}
Signed integer arithmetic that overflows causes a runtime panic.

{{ rule(id="3.1:3", cat="example") }}
```rue
let x: i32 = 42;
```
```

The format is `{{ rule(id="X.Y:Z") }}` or `{{ rule(id="X.Y:Z", cat="category") }}` where:
- `X.Y` is the chapter and section (e.g., `3.1` for Chapter 3, Section 1)
- `Z` is the paragraph number within that section
- The colon (`:`) separates the structural location from the paragraph number
- `cat` is optional (defaults to `informative` if omitted)

**Paragraph categories:**
- `normative` - General normative rule (requires test coverage)
- `legality-rule` - Compile-time requirements (normative)
- `dynamic-semantics` - Runtime behavior (normative)
- `syntax` - Grammar rules (normative)
- `undefined-behavior` - UB conditions (normative)
- `example` - Code examples (informative)
- `informative` - Explanatory text (informative, default)

#### Traceability Report

Generate a report showing test coverage of spec paragraphs:

```bash
# Summary report
./buck2 run //crates/rue-spec:rue-spec -- --traceability
./buck2 test //:spec-traceability

# Detailed matrix (shows all paragraphs and their covering tests)
./buck2 run //crates/rue-spec:rue-spec -- --traceability --detailed
```

The traceability check is run as the `//:spec-traceability` Buck target and is
included in `./test.sh` and `./buck2 test //...`. It fails if:
- Any spec paragraph has no covering test (coverage < 100%)
- Any test references a non-existent spec paragraph ID
- Any normative rule is covered only through a large program (see
  "Focused coverage" above)
- Any behavior-asserting case cites only informative/example paragraphs. Such a case
  must cite the matching normative rule.
- Any case declares an ambiguous platform responsibility (see
  "Platform Responsibility" above)

A case only contributes coverage when it actually runs: skipped cases,
preview cases allowed to fail, and cases scoped to platforms no required CI lane
executes are all reported but not credited.


## Fuzz testing

Fuzzing lives in `crates/rue-fuzz`; see `crates/rue-fuzz/README.md` for
targets, corpus management, and CI integration.
