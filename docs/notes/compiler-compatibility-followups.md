# Compiler compatibility cleanup follow-ups

Normal snapshot compilation and semantic emit modes now query a fresh
`CanonicalFrontendSession`. The former `CompilationUnit` peer orchestrator has
been removed, and canonical RIR presentation preserves caller-ordered text
without selecting a legacy frontend. The remaining compatibility paths are
separate API/driver concerns and should be removed in this order:

1. **Retire duplicate RIR import extraction from production concepts.**
   `extract_import_directives` remains a public compatibility query and has
   direct tests, but production snapshot compilation now retains import
   directives from canonical parsed modules. Deprecate rather than delete the
   API, with parity tests covering nested and type-position imports.
2. **Remove raw-AST semantic re-lowering.** The public raw `Ast` frontend in
   `lib.rs` creates positional RIR and then lowers a semantic-order RIR again.
   Eliminating that second walk requires an explicit provenance/ordering
   contract for caller-created ASTs; it must not be folded into snapshot work.
