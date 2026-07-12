# Compiler compatibility cleanup follow-ups

The canonical snapshot and `CompilationUnit` pipelines no longer repeat parsing,
RIR lowering, or semantic analysis. The remaining compatibility paths are
separate API/driver concerns and should be removed in this order:

1. **Migrate `--emit` to canonical artifacts.** `crates/rue/src/main.rs` still
   calls the public shared-interner `parse_all_files_with_source_snapshot`,
   `merge_symbols`, and caller-positional `AstGen` path. Preserve the public
   parse/merge APIs, but make emit consume canonical parsed, merged, and RIR
   artifacts and add byte/diagnostic parity tests for every emit mode.
2. **Retire duplicate RIR import extraction from production concepts.**
   `extract_import_directives` remains a public compatibility query and has
   direct tests, but production snapshot/unit compilation now retains import
   directives from canonical parsed modules. Deprecate rather than delete the
   API, with parity tests covering nested and type-position imports.
3. **Remove raw-AST semantic re-lowering.** The public raw `Ast` frontend in
   `lib.rs` creates positional RIR and then lowers a semantic-order RIR again.
   Eliminating that second walk requires an explicit provenance/ordering
   contract for caller-created ASTs; it must not be folded into snapshot work.
4. **Keep the legacy AST/interner projection demand-driven.** `ast()`,
   pre-lower `interner()`, and pre-lower `take_interner()` are deliberately
   public compatibility surfaces. They should remain lazy and counted until a
   versioned API change can replace the shared-interner `MergedAst` contract.

