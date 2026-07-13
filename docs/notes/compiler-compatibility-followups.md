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
2. **Remove raw-AST semantic re-lowering (completed by RUE-734).** The public
   raw `Ast` semantic entry points were removed rather than shimmed: they had no
   production consumer and could not carry complete source identity. Embedders
   now construct `SourceSnapshot` plus `CompileOptions` and query
   `CanonicalFrontendSession`. Syntax-only AST parsing and presentation remain
   available. This also removes anonymous metadata synthesis, minimum-`FileId`
   root inference, and the positional-plus-semantic double RIR walk.
