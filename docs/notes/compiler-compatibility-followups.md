# Compiler compatibility cleanup follow-ups

Normal snapshot compilation and semantic emit modes now query a fresh
`CanonicalFrontendSession`; `CompilationUnit` is a compatibility facade rather
than a production orchestrator. The remaining compatibility paths are separate
API/driver concerns and should be removed in this order:

1. **Finish canonical `--emit rir` presentation.** AIR, CFG, lowering, MIR,
   liveness, register allocation, assembly, and stack-frame modes now consume
   canonical session artifacts. AST-only remains a deliberate
   syntax compatibility route. Exact RIR text is still isolated on the legacy
   caller-positional path because its printed `%N` instruction references are
   observable. Replace that route with a read-only presentation mapping that
   reorders records and remaps every printed `InstRef`; it must not run a
   second `AstGen` or alter canonical artifact identity/work. The mapping also
   needs lowering-time provenance for synthesized names: AstGen's global
   `for_counter` assigns `__for_*_N` names according to lowering order, so a
   record permutation alone cannot reproduce historical caller-order bytes.
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
