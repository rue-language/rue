# Compiler compatibility cleanup follow-ups

The RUE-734 through RUE-736 compatibility cleanup is complete. Semantic
consumers now construct a `SourceSnapshot`, publish it to `CompilerSession`,
and retain immutable query artifacts. The compiler facade has one final-output
adapter, `compile_snapshot`.

Completed cleanup:

1. Production import discovery consumes directives retained by parsed session
   modules. The extraction helper is test-only.
2. Public raw-AST semantic lowering was removed. `Ast` and
   `ParsedAstPresentation` remain syntax and presentation values only.
3. Shared-interner parse records and concatenated merge records were removed.
   Stable-module parsed values and candidate validation are the only semantic
   path.
4. The peer compilation driver and duplicate single/multi-file adapters were
   removed. CLI, fuzzing, oracle, timing tests, and benchmarks use session
   queries or the sole batch adapter.

The exact removed-symbol inventory and intentionally separate responsibilities
are recorded in
[the RUE-730 completion audit](canonical-query-completion-audit.md).
