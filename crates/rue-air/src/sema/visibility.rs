//! Visibility checking for module system.
//!
//! This module implements the visibility rules defined in ADR-0026:
//! - `pub` items are always accessible
//! - Private items are accessible if the files are in the same directory module

use rue_error::{CompileError, CompileResult, ErrorKind};

use super::aggregate_resolution::{resolve_visibility_module_ref, select_qualified_enum};
use super::context::AnalysisContext;
use super::ordinary_engine::{OrdinaryBodyAnalysisHost, OrdinaryBodyEngine};
use crate::types::EnumId;

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
                let file = facts.aggregate_module(module_id).file;
                select_qualified_enum(facts, file, type_name)
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
        resolve_visibility_module_ref(facts, self.body_rir_ref(), module_ref, &ctx.locals)
    }
}
