//! Visibility checking for module system.
//!
//! This module implements the visibility rules defined in ADR-0026:
//! - `pub` items are always accessible
//! - Private items are accessible if the files are in the same directory module

use super::ordinary_engine::{OrdinaryBodyAnalysisHost, OrdinaryBodyEngine};
use rue_error::{CompileError, CompileResult, ErrorKind};
use rue_span::FileId;

use super::aggregate_resolution::{
    AggregateFacts, is_accessible, resolve_visibility_module_ref, select_qualified_enum,
};
use super::{DeclarationPhase, Sema, context::AnalysisContext};
use crate::types::EnumId;

impl<D: DeclarationPhase> Sema<'_, D> {
    /// Check if the accessing file can see a private item from the target file.
    ///
    /// Visibility rules (per ADR-0026):
    /// - `pub` items are always accessible
    /// - Private items are accessible if the files are in the same directory module
    ///
    /// Directory module membership is simply "files in the same directory" —
    /// including the facade, which lives inside its directory
    /// (`utils/_utils.rue` is in the `utils` module, RUE-137).
    ///
    /// Returns true if the item is accessible.
    pub(crate) fn is_accessible(
        &self,
        accessing_file_id: FileId,
        target_file_id: FileId,
        is_pub: bool,
    ) -> bool {
        let facts = self.aggregate_facts();
        is_accessible(&facts, accessing_file_id, target_file_id, is_pub)
    }

    /// Check that an *unqualified* reference may reach the item (RUE-37,
    /// RUE-180, RUE-183, RUE-185).
    ///
    /// Unqualified references resolve only in the reference file. This helper
    /// then applies the uniform privacy rule to the declaration found there
    /// (spec 10.3:1, 10.3:7):
    ///
    /// - If the item is accessible per [`Sema::is_accessible`] (it is
    ///   `pub`, or the reference is in the item's directory — ADR-0026
    ///   intra-directory visibility), the reference is fine.
    /// - Otherwise the reference is an error (E0460), naming the item's
    ///   defining file. Ordinary unqualified lookups are file-local, so this
    ///   fires only on the comptime resolution paths that carry a reference
    ///   into another file, e.g. applying a private comptime type constructor
    ///   in type position (RUE-283).
    ///
    /// `item_kind` names the kind in the diagnostic ("function", "struct",
    /// "enum", "constant").
    pub(crate) fn check_unqualified_visibility(
        &self,
        item_kind: &str,
        name: &str,
        defining_file_id: FileId,
        is_pub: bool,
        span: rue_span::Span,
    ) -> CompileResult<()> {
        if self.is_accessible(span.file_id, defining_file_id, is_pub) {
            return Ok(());
        }

        // `is_accessible` is permissive when either file path is unknown
        // (single-file mode, synthetic items), so the item's path is
        // always known here.
        let defining_file = self
            .aggregate_facts()
            .file_path(defining_file_id)
            .unwrap_or("<unknown>")
            .to_string();

        Err(CompileError::new(
            ErrorKind::PrivateUnqualifiedAccess(Box::new(
                rue_error::PrivateUnqualifiedAccessData {
                    item_kind: item_kind.to_string(),
                    name: name.to_string(),
                    defining_file,
                },
            )),
            span,
        )
        .with_help(format!(
            "`{name}` is not marked `pub`; private items are only visible within their defining directory"
        )))
    }
}

impl<H: OrdinaryBodyAnalysisHost> OrdinaryBodyEngine<'_, H> {
    /// Resolve an enum type through a module reference.
    ///
    /// Used for qualified enum paths like `module.EnumName::Variant` in match patterns.
    /// Checks visibility: private enums are only accessible from the same directory.
    pub(crate) fn resolve_enum_through_module(
        &self,
        module_ref: rue_rir::InstRef,
        type_name: lasso::Spur,
        span: rue_span::Span,
        ctx: &AnalysisContext,
    ) -> CompileResult<EnumId> {
        let type_name_str = self.body_interner().resolve(&type_name);

        // Resolve the receiver's full module spine.  The root binding belongs
        // to the source file containing the expression; every subsequent
        // field is a module binding in the preceding module's defining file.
        // This mirrors inference's module-member walk for paths such as
        // `std.geo.Sign.Pos`, while keeping every lookup file-qualified.
        let module_file_id = self.module_file_for_ref(module_ref, ctx);

        // A qualified member is resolved only in the referenced module's
        // defining file. If the receiver has no module identity, it is not a
        // valid qualified type path.
        let enum_id = module_file_id
            .and_then(|module_id| {
                let facts = self.aggregate_facts();
                let file = facts.module(module_id).file;
                select_qualified_enum(&facts, file, type_name)
            })
            .ok_or_else(|| {
                CompileError::new(ErrorKind::UnknownEnumType(type_name_str.to_string()), span)
            })?;

        // Check visibility
        let enum_def = self.body_type_pool().enum_def(enum_id);
        let accessing_file_id = span.file_id;
        let target_file_id = enum_def.file_id;

        if !self.is_accessible(accessing_file_id, target_file_id, enum_def.is_pub) {
            return Err(CompileError::new(
                ErrorKind::PrivateMemberAccess {
                    item_kind: "enum".to_string(),
                    name: type_name_str.to_string(),
                },
                span,
            ));
        }

        Ok(enum_id)
    }

    fn module_file_for_ref(
        &self,
        module_ref: rue_rir::InstRef,
        ctx: &AnalysisContext,
    ) -> Option<crate::types::ModuleId> {
        let facts = self.aggregate_facts();
        resolve_visibility_module_ref(&facts, self.body_rir_ref(), module_ref, &ctx.locals)
    }
}
