//! Visibility checking for module system.
//!
//! This module implements the visibility rules defined in ADR-0026:
//! - `pub` items are always accessible
//! - Private items are accessible if the files are in the same directory module

use rue_error::{CompileError, CompileResult, ErrorKind};

use super::aggregate_resolution::select_module_type_member;
use super::context::AnalysisContext;
use super::ordinary_engine::{OrdinaryBodyAnalysisHost, OrdinaryBodyEngine};
use crate::types::EnumId;

impl<H: OrdinaryBodyAnalysisHost> OrdinaryBodyEngine<'_, H> {
    /// Resolve an enum type through a module reference.
    ///
    /// Used for qualified enum paths like `module.EnumName.Variant` in match
    /// patterns and variant construction. The member is selected by the one
    /// canonical [`select_module_type_member`], so a `const` type alias
    /// (`pub const R = Result(u64, E);`) names its enum here exactly as a
    /// declaration does (RUE-1956). Visibility is E0706, checked against the
    /// alias binding when the name arrived through one.
    pub(crate) fn resolve_enum_through_module(
        &mut self,
        module_ref: rue_rir::InstRef,
        type_name: lasso::Spur,
        span: rue_span::Span,
        ctx: &AnalysisContext,
    ) -> CompileResult<EnumId> {
        let type_name_str = self.body_interner().resolve(&type_name).to_string();

        // Resolve the receiver's full module spine through the one canonical
        // walker, shared with expression position (`Self::try_module_id_of`):
        // the root binding belongs to the source file containing the pattern,
        // a runtime binding of the same name shadows it, and every subsequent
        // field is a module binding in the preceding module's defining file
        // whose own visibility is checked as it is crossed (RUE-1964).
        let module_id = self.try_module_id_of(module_ref, span, ctx)?;

        // A qualified member is resolved only in the referenced module's
        // defining file. If the receiver has no module identity, it is not a
        // valid qualified type path.
        let unknown_enum =
            || CompileError::new(ErrorKind::UnknownEnumType(type_name_str.to_string()), span);
        let module_file = module_id
            .map(|module_id| self.aggregate_facts().aggregate_module(module_id).file)
            .ok_or_else(&unknown_enum)?;
        let member = {
            let facts = self.aggregate_facts();
            select_module_type_member(facts, module_file, type_name)
        };
        let nominal = member.as_enum().ok_or_else(&unknown_enum)?;

        let enum_def = self.body_type_pool().enum_def(nominal.id);
        self.check_module_qualified_visibility(
            nominal.alias,
            module_file,
            (enum_def.file_id, enum_def.is_pub),
            "enum",
            &type_name_str,
            span,
        )?;

        Ok(nominal.id)
    }
}
