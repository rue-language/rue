//! Semantic analysis - RIR to AIR conversion.
//!
//! Sema performs type checking and converts untyped RIR to typed AIR.
//! This is analogous to Zig's Sema phase.

use std::collections::{HashMap, HashSet};

use crate::inference::{
    Constraint, ConstraintContext, ConstraintGenerator, FunctionSig, InferType, MethodSig,
    ParamVarInfo, Unifier, UnifyResult,
};
use crate::inst::{Air, AirArgMode, AirCallArg, AirInst, AirInstData, AirPattern, AirRef};
use crate::types::{
    ArrayTypeDef, ArrayTypeId, EnumDef, EnumId, StructDef, StructField, StructId, Type,
    parse_array_type_syntax,
};
use rue_error::{
    CompileError, CompileErrors, CompileResult, CompileWarning, CopyStructNonCopyFieldError,
    ErrorKind, IntrinsicTypeMismatchError, MissingFieldsError, MultiErrorResult, OptionExt,
    PreviewFeature, PreviewFeatures, WarningKind,
};
use rue_intern::{Interner, Symbol};
use rue_rir::{
    InstData, InstRef, Rir, RirArgMode, RirCallArg, RirDirective, RirParamMode, RirPattern,
};
use rue_span::Span;

/// A value that can be computed at compile time.
///
/// This is used for constant expression evaluation, primarily for compile-time
/// bounds checking. It can be extended for future `comptime` features.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConstValue {
    /// Integer value (signed to handle arithmetic correctly)
    Integer(i64),
    /// Boolean value
    Bool(bool),
}

impl ConstValue {
    /// Try to extract an integer value.
    fn as_integer(self) -> Option<i64> {
        match self {
            ConstValue::Integer(n) => Some(n),
            ConstValue::Bool(_) => None,
        }
    }

    /// Try to extract a boolean value.
    fn as_bool(self) -> Option<bool> {
        match self {
            ConstValue::Bool(b) => Some(b),
            ConstValue::Integer(_) => None,
        }
    }
}

/// Result of analyzing a function.
#[derive(Debug)]
pub struct AnalyzedFunction {
    pub name: String,
    pub air: Air,
    /// Number of local variable slots needed
    pub num_locals: u32,
    /// Number of ABI slots used by parameters.
    /// For scalar types (i32, bool), each parameter uses 1 slot.
    /// For struct types, each field uses 1 slot (flattened ABI).
    pub num_param_slots: u32,
    /// Whether each parameter slot is passed as inout (by reference).
    /// Length matches num_param_slots - for struct params, all slots share
    /// the same mode as the original parameter.
    pub param_modes: Vec<bool>,
}

/// Output from semantic analysis.
///
/// Contains all analyzed functions, struct definitions, enum definitions, and any warnings
/// generated during analysis.
#[derive(Debug)]
pub struct SemaOutput {
    /// Analyzed functions with typed IR.
    pub functions: Vec<AnalyzedFunction>,
    /// Struct definitions.
    pub struct_defs: Vec<StructDef>,
    /// Enum definitions.
    pub enum_defs: Vec<EnumDef>,
    /// Array type definitions.
    pub array_types: Vec<ArrayTypeDef>,
    /// String literals indexed by their AIR string_const index.
    pub strings: Vec<String>,
    /// Warnings collected during analysis.
    pub warnings: Vec<CompileWarning>,
}

/// Information about a local variable.
#[derive(Debug, Clone)]
struct LocalVar {
    /// Slot index for this variable
    slot: u32,
    /// Type of the variable
    ty: Type,
    /// Whether the variable is mutable
    is_mut: bool,
    /// Span of the variable declaration (for unused variable warnings)
    span: Span,
    /// Whether @allow(unused_variable) was applied to this binding
    allow_unused: bool,
}

/// Information about a variable that has been moved.
#[derive(Debug, Clone)]
struct MoveInfo {
    /// Span where the move occurred
    moved_at: Span,
}

/// Information about a function parameter.
#[derive(Debug, Clone)]
struct ParamInfo {
    /// Starting ABI slot for this parameter (0-based).
    /// For scalar types, this is the single slot.
    /// For struct types, this is the first field's slot.
    abi_slot: u32,
    /// Parameter type
    ty: Type,
    /// Parameter passing mode
    mode: RirParamMode,
}

/// Context for analyzing instructions within a function.
///
/// Bundles together the mutable state that needs to be threaded through
/// recursive `analyze_inst` calls.
struct AnalysisContext<'a> {
    /// Local variables in scope
    locals: HashMap<Symbol, LocalVar>,
    /// Function parameters (immutable reference, shared across the function)
    params: &'a HashMap<Symbol, ParamInfo>,
    /// Next available slot for local variables
    next_slot: u32,
    /// How many loops we're nested inside (for break/continue validation)
    loop_depth: u32,
    /// Local variables that have been read (for unused variable detection)
    used_locals: HashSet<Symbol>,
    /// Return type of the current function (for explicit return validation)
    return_type: Type,
    /// Scope stack for efficient scope management.
    /// Each entry is a list of (symbol, old_value) pairs for variables added/shadowed in that scope.
    /// When a scope is popped, we restore old values (for shadowed vars) or remove new vars.
    scope_stack: Vec<Vec<(Symbol, Option<LocalVar>)>>,
    /// Resolved types from HM inference.
    /// Maps RIR instruction refs to their resolved concrete types.
    /// This is populated by running constraint generation and unification
    /// before AIR emission.
    resolved_types: &'a HashMap<InstRef, Type>,
    /// Variables that have been moved (for affine type checking).
    /// Maps variable symbol to move information.
    moved_vars: HashMap<Symbol, MoveInfo>,
}

impl AnalysisContext<'_> {
    /// Push a new scope onto the stack.
    fn push_scope(&mut self) {
        // Preallocate for a small number of variables. Most scopes (loop bodies,
        // if/match arms) have 0-2 variables; function bodies have more but are
        // less frequent. 2 is a conservative choice until we have real metrics.
        self.scope_stack.push(Vec::with_capacity(2));
    }

    /// Pop the current scope, restoring any shadowed variables and removing new ones.
    fn pop_scope(&mut self) {
        if let Some(scope_entries) = self.scope_stack.pop() {
            for (symbol, old_value) in scope_entries {
                match old_value {
                    Some(old_var) => {
                        // Restore the shadowed variable
                        self.locals.insert(symbol, old_var);
                    }
                    None => {
                        // Remove the variable that was added in this scope
                        self.locals.remove(&symbol);
                    }
                }
            }
        }
    }

    /// Insert a local variable, tracking it in the current scope for later cleanup.
    fn insert_local(&mut self, symbol: Symbol, var: LocalVar) {
        let old_value = self.locals.insert(symbol, var);
        // Track in the current scope (if any) for cleanup on pop
        if let Some(current_scope) = self.scope_stack.last_mut() {
            current_scope.push((symbol, old_value));
        }
        // When a variable is (re)declared, clear any moved state for it.
        // This handles shadowing: `let x = moved_val; let x = new_val;`
        // The new `x` is a fresh binding and shouldn't carry the old moved state.
        self.moved_vars.remove(&symbol);
    }
}

/// Information about a function.
#[derive(Debug, Clone)]
struct FunctionInfo {
    /// Parameter types (in order)
    param_types: Vec<Type>,
    /// Parameter modes (in order)
    param_modes: Vec<RirParamMode>,
    /// Return type
    return_type: Type,
}

/// Information about a method in an impl block.
#[derive(Debug, Clone)]
struct MethodInfo {
    /// The struct type this method belongs to
    struct_type: Type,
    /// Whether this is a method (has self) or associated function (no self)
    has_self: bool,
    /// Parameter names (excluding self if present)
    param_names: Vec<Symbol>,
    /// Parameter types (excluding self if present)
    param_types: Vec<Type>,
    /// Return type
    return_type: Type,
    /// The RIR instruction ref for the method body
    body: InstRef,
    /// Span of the method declaration
    span: Span,
}

/// Result of analyzing an instruction: the AIR reference and its synthesized type.
#[derive(Debug, Clone, Copy)]
struct AnalysisResult {
    /// Reference to the generated AIR instruction
    air_ref: AirRef,
    /// The synthesized type of this expression
    ty: Type,
}

impl AnalysisResult {
    #[must_use]
    fn new(air_ref: AirRef, ty: Type) -> Self {
        Self { air_ref, ty }
    }
}

/// Semantic analyzer that converts RIR to AIR.
pub struct Sema<'a> {
    rir: &'a Rir,
    interner: &'a Interner,
    /// Function table: maps function name symbols to their info
    functions: HashMap<Symbol, FunctionInfo>,
    /// Struct table: maps struct name symbols to their StructId
    structs: HashMap<Symbol, StructId>,
    /// Struct definitions indexed by StructId
    struct_defs: Vec<StructDef>,
    /// Enum table: maps enum name symbols to their EnumId
    enums: HashMap<Symbol, EnumId>,
    /// Enum definitions indexed by EnumId
    enum_defs: Vec<EnumDef>,
    /// Array type table: maps (element_type, length) to ArrayTypeId
    array_types: HashMap<(Type, u64), ArrayTypeId>,
    /// Array type definitions indexed by ArrayTypeId
    array_type_defs: Vec<ArrayTypeDef>,
    /// String table: maps string content to index (for deduplication)
    string_table: HashMap<String, u32>,
    /// String data indexed by string table index
    strings: Vec<String>,
    /// Warnings collected during analysis
    warnings: Vec<CompileWarning>,
    /// Method table: maps (struct_name, method_name) to method info
    /// Used for resolving method calls (receiver.method()) and associated
    /// function calls (Type::function())
    methods: HashMap<(Symbol, Symbol), MethodInfo>,
    /// Enabled preview features
    preview_features: PreviewFeatures,
}

/// Storage location for a String receiver in mutation methods.
///
/// This is used by `analyze_string_mutation_method` to store the updated
/// String back to the original variable after calling the runtime function.
enum StringReceiverStorage {
    /// The receiver is a local variable with the given slot.
    Local { slot: u32 },
    /// The receiver is a parameter with the given ABI slot.
    Param { abi_slot: u32 },
}

impl<'a> Sema<'a> {
    /// Create a new semantic analyzer.
    pub fn new(rir: &'a Rir, interner: &'a Interner, preview_features: PreviewFeatures) -> Self {
        Self {
            rir,
            interner,
            functions: HashMap::new(),
            structs: HashMap::new(),
            struct_defs: Vec::new(),
            enums: HashMap::new(),
            enum_defs: Vec::new(),
            array_types: HashMap::new(),
            array_type_defs: Vec::new(),
            string_table: HashMap::new(),
            strings: Vec::new(),
            warnings: Vec::new(),
            methods: HashMap::new(),
            preview_features,
        }
    }

    /// Check if a preview feature is enabled, returning an error if not.
    ///
    /// This is the gating mechanism for preview features. Call this method
    /// when semantic analysis encounters a feature that requires a preview flag.
    ///
    /// # Parameters
    /// - `feature`: The preview feature that is required
    /// - `what`: A description of what requires the feature (e.g., "string concatenation")
    /// - `span`: The source location where the feature is used
    ///
    /// # Returns
    /// - `Ok(())` if the feature is enabled
    /// - `Err(CompileError)` with a helpful message if not enabled
    fn require_preview(
        &self,
        feature: PreviewFeature,
        what: &str,
        span: Span,
    ) -> CompileResult<()> {
        if self.preview_features.contains(&feature) {
            Ok(())
        } else {
            Err(CompileError::new(
                ErrorKind::PreviewFeatureRequired {
                    feature,
                    what: what.to_string(),
                },
                span,
            )
            .with_help(format!(
                "use `--preview {}` to enable this feature ({})",
                feature.name(),
                feature.adr()
            )))
        }
    }

    /// Add a string to the string table, returning its index.
    /// Deduplicates identical strings.
    fn add_string(&mut self, content: String) -> u32 {
        use std::collections::hash_map::Entry;
        match self.string_table.entry(content) {
            Entry::Occupied(e) => *e.get(),
            Entry::Vacant(e) => {
                let id = self.strings.len() as u32;
                self.strings.push(e.key().clone());
                e.insert(id);
                id
            }
        }
    }

    /// Check if directives contain @allow for a specific warning name.
    fn has_allow_directive(&self, directives: &[RirDirective], warning_name: &str) -> bool {
        let allow_sym = self.interner.get_symbol("allow");
        let warning_sym = self.interner.get_symbol(warning_name);

        for directive in directives {
            if Some(directive.name) == allow_sym {
                for arg in &directive.args {
                    if Some(*arg) == warning_sym {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Check for unused local variables in the current scope (before popping it).
    /// Uses the scope stack to determine which variables were added in the current scope.
    fn check_unused_locals_in_current_scope(&mut self, ctx: &AnalysisContext) {
        // Get the current scope entries (variables added in this scope)
        let Some(current_scope) = ctx.scope_stack.last() else {
            return;
        };

        for (symbol, _old_value) in current_scope {
            // Skip if variable was used
            if ctx.used_locals.contains(symbol) {
                continue;
            }

            // Get the local var info (it should still be in ctx.locals before pop)
            let Some(local) = ctx.locals.get(symbol) else {
                continue;
            };

            // Get variable name
            let name = self.interner.get(*symbol);

            // Skip variables starting with underscore (convention for intentionally unused)
            if name.starts_with('_') {
                continue;
            }

            // Skip if @allow(unused_variable) was applied
            if local.allow_unused {
                continue;
            }

            // Emit warning with help suggestion
            self.warnings.push(
                CompileWarning::new(WarningKind::UnusedVariable(name.to_string()), local.span)
                    .with_help(format!(
                        "if this is intentional, prefix it with an underscore: `_{}`",
                        name
                    )),
            );
        }
    }

    /// Extract the root variable symbol from an expression, if it refers to a variable.
    ///
    /// For inout arguments, we need to track which variable is being passed to detect
    /// when the same variable is passed to multiple inout parameters.
    ///
    /// Returns Some(symbol) for:
    /// - VarRef { name } -> the variable symbol
    /// - ParamRef { name, .. } -> the parameter symbol
    /// - FieldGet { base, .. } -> recursively extract from base
    /// - IndexGet { base, .. } -> recursively extract from base
    ///
    /// Returns None for expressions that don't refer to a variable (literals, calls, etc.)
    fn extract_root_variable(&self, inst_ref: InstRef) -> Option<Symbol> {
        let inst = self.rir.get(inst_ref);
        match &inst.data {
            InstData::VarRef { name } => Some(*name),
            InstData::ParamRef { name, .. } => Some(*name),
            InstData::FieldGet { base, .. } => self.extract_root_variable(*base),
            InstData::IndexGet { base, .. } => self.extract_root_variable(*base),
            _ => None,
        }
    }

    /// Check exclusivity rules for inout and borrow parameters in a call.
    ///
    /// This enforces two rules:
    /// 1. Same variable cannot be passed to multiple inout parameters (prevents aliasing)
    /// 2. Same variable cannot be passed to both inout and borrow (law of exclusivity)
    ///
    /// The law of exclusivity: either one mutable (inout) access OR any number of
    /// immutable (borrow) accesses, never both simultaneously.
    fn check_exclusive_access(&self, args: &[RirCallArg], call_span: Span) -> CompileResult<()> {
        use std::collections::HashSet;
        let mut inout_vars: HashSet<Symbol> = HashSet::new();
        let mut borrow_vars: HashSet<Symbol> = HashSet::new();

        for arg in args {
            let maybe_var_symbol = self.extract_root_variable(arg.value);

            // Check that inout/borrow arguments are lvalues
            if arg.is_inout() && maybe_var_symbol.is_none() {
                return Err(CompileError::new(
                    ErrorKind::InoutNonLvalue,
                    self.rir.get(arg.value).span,
                ));
            }
            if arg.is_borrow() && maybe_var_symbol.is_none() {
                return Err(CompileError::new(
                    ErrorKind::BorrowNonLvalue,
                    self.rir.get(arg.value).span,
                ));
            }

            if let Some(var_symbol) = maybe_var_symbol {
                if arg.is_inout() {
                    // Check for duplicate inout access
                    if !inout_vars.insert(var_symbol) {
                        let var_name = self.interner.get(var_symbol).to_string();
                        return Err(CompileError::new(
                            ErrorKind::InoutExclusiveAccess { variable: var_name },
                            call_span,
                        ));
                    }
                    // Check for borrow/inout conflict
                    if borrow_vars.contains(&var_symbol) {
                        let var_name = self.interner.get(var_symbol).to_string();
                        return Err(CompileError::new(
                            ErrorKind::BorrowInoutConflict { variable: var_name },
                            call_span,
                        ));
                    }
                } else if arg.is_borrow() {
                    borrow_vars.insert(var_symbol);
                    // Check for borrow/inout conflict
                    if inout_vars.contains(&var_symbol) {
                        let var_name = self.interner.get(var_symbol).to_string();
                        return Err(CompileError::new(
                            ErrorKind::BorrowInoutConflict { variable: var_name },
                            call_span,
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    /// Analyze a list of call arguments, handling inout unmove logic.
    ///
    /// For inout arguments, the variable is "unmoving" after analysis - this is because
    /// inout is a mutable borrow, not a move. The value stays valid after the call.
    fn analyze_call_args(
        &mut self,
        air: &mut Air,
        args: &[RirCallArg],
        ctx: &mut AnalysisContext,
    ) -> CompileResult<Vec<AirCallArg>> {
        let mut air_args = Vec::new();
        for arg in args.iter() {
            // For inout/borrow arguments, extract the variable name before analysis
            // so we can "unmove" it after - these are borrows, not moves
            let borrowed_var = if arg.is_inout() || arg.is_borrow() {
                self.extract_root_variable(arg.value)
            } else {
                None
            };

            let arg_result = self.analyze_inst(air, arg.value, ctx)?;

            // If this was an inout/borrow argument, the variable shouldn't be marked as moved
            // because these are borrows - the value stays valid after the call
            if let Some(var_symbol) = borrowed_var {
                ctx.moved_vars.remove(&var_symbol);
            }

            air_args.push(AirCallArg {
                value: arg_result.air_ref,
                mode: Self::convert_arg_mode(arg.mode),
            });
        }
        Ok(air_args)
    }

    /// Analyze all functions in the RIR.
    ///
    /// Consumes the Sema and returns a [`SemaOutput`] containing all analyzed
    /// functions, struct definitions, enum definitions, and any warnings generated during analysis.
    ///
    /// This function collects errors from multiple functions instead of stopping at the
    /// first error, allowing users to see all issues at once. Errors within type/struct
    /// definitions still cause early termination since they affect all subsequent analysis.
    pub fn analyze_all(mut self) -> MultiErrorResult<SemaOutput> {
        // First pass: collect type definitions (enums then structs)
        // Enums must be collected first so struct fields can reference enum types
        // These are critical and must succeed before we can analyze functions
        self.collect_enum_definitions()
            .map_err(CompileErrors::from)?;
        self.collect_struct_definitions()
            .map_err(CompileErrors::from)?;

        // Validate @copy struct fields are all Copy types
        self.validate_copy_structs().map_err(CompileErrors::from)?;

        // Collect user-defined destructors
        self.collect_destructor_definitions()
            .map_err(CompileErrors::from)?;

        // Second pass: collect function and method signatures
        // These are also critical since they affect type checking of calls
        self.collect_function_signatures()
            .map_err(CompileErrors::from)?;
        self.collect_method_definitions()
            .map_err(CompileErrors::from)?;

        // Now analyze function bodies - these can be analyzed independently
        // so we collect errors from all of them instead of stopping at the first
        let mut functions = Vec::new();
        let mut errors = CompileErrors::new();

        // Collect method refs from impl blocks so we can skip them in the first pass
        let mut method_refs: HashSet<InstRef> = HashSet::new();
        for (_, inst) in self.rir.iter() {
            if let InstData::ImplDecl { methods, .. } = &inst.data {
                for method_ref in methods {
                    method_refs.insert(*method_ref);
                }
            }
        }

        // Analyze regular functions (not methods in impl blocks)
        for (inst_ref, inst) in self.rir.iter() {
            if let InstData::FnDecl {
                directives: _,
                name,
                params,
                return_type,
                body,
                has_self: _,
            } = &inst.data
            {
                // Skip methods - they'll be analyzed separately with impl block context
                if method_refs.contains(&inst_ref) {
                    continue;
                }

                let fn_name = self.interner.get(*name).to_string();

                // Try to analyze this function - on error, record it and continue
                match self.analyze_single_function(&fn_name, *return_type, params, *body, inst.span)
                {
                    Ok(analyzed) => functions.push(analyzed),
                    Err(e) => errors.push(e),
                }
            }
        }

        // Fourth pass: analyze method bodies from impl blocks
        for (_, inst) in self.rir.iter() {
            if let InstData::ImplDecl { type_name, methods } = &inst.data {
                let type_name_str = self.interner.get(*type_name).to_string();
                let struct_id = *self.structs.get(type_name).unwrap();
                let struct_type = Type::Struct(struct_id);

                for method_ref in methods {
                    let method_inst = self.rir.get(*method_ref);
                    if let InstData::FnDecl {
                        name: method_name,
                        params,
                        return_type,
                        body,
                        has_self,
                        ..
                    } = &method_inst.data
                    {
                        let method_name_str = self.interner.get(*method_name).to_string();

                        // Generate method name with struct prefix: "Type.method" or "Type::function"
                        let full_name = if *has_self {
                            format!("{}.{}", type_name_str, method_name_str)
                        } else {
                            format!("{}::{}", type_name_str, method_name_str)
                        };

                        // Try to analyze this method - on error, record it and continue
                        match self.analyze_method_function(
                            &full_name,
                            *return_type,
                            params,
                            *body,
                            method_inst.span,
                            struct_type,
                            *has_self,
                        ) {
                            Ok(analyzed) => functions.push(analyzed),
                            Err(e) => errors.push(e),
                        }
                    }
                }
            }
        }

        // Fifth pass: analyze destructor bodies
        for (_, inst) in self.rir.iter() {
            if let InstData::DropFnDecl { type_name, body } = &inst.data {
                let type_name_str = self.interner.get(*type_name).to_string();
                let struct_id = *self.structs.get(type_name).unwrap();
                let struct_type = Type::Struct(struct_id);

                // Generate destructor name: "TypeName.__drop"
                let full_name = format!("{}.__drop", type_name_str);

                // Try to analyze destructor - on error, record it and continue
                match self.analyze_destructor_function(&full_name, *body, inst.span, struct_type) {
                    Ok(analyzed) => functions.push(analyzed),
                    Err(e) => errors.push(e),
                }
            }
        }

        // Return errors if any were collected
        errors.into_result_with(SemaOutput {
            functions,
            struct_defs: self.struct_defs,
            enum_defs: self.enum_defs,
            array_types: self.array_type_defs,
            strings: self.strings,
            warnings: self.warnings,
        })
    }

    /// Analyze a single regular function.
    ///
    /// This helper factors out the function analysis logic to make error
    /// collection cleaner in analyze_all.
    fn analyze_single_function(
        &mut self,
        fn_name: &str,
        return_type: Symbol,
        params: &[rue_rir::RirParam],
        body: InstRef,
        span: Span,
    ) -> CompileResult<AnalyzedFunction> {
        let ret_type = self.resolve_type(return_type, span)?;

        // Resolve parameter types and modes
        let param_info: Vec<(Symbol, Type, RirParamMode)> = params
            .iter()
            .map(|p| {
                let ty = self.resolve_type(p.ty, span)?;
                Ok((p.name, ty, p.mode))
            })
            .collect::<CompileResult<Vec<_>>>()?;

        let (air, num_locals, num_param_slots, param_modes) =
            self.analyze_function(ret_type, &param_info, body)?;

        Ok(AnalyzedFunction {
            name: fn_name.to_string(),
            air,
            num_locals,
            num_param_slots,
            param_modes,
        })
    }

    /// Analyze a method function from an impl block.
    fn analyze_method_function(
        &mut self,
        full_name: &str,
        return_type: Symbol,
        params: &[rue_rir::RirParam],
        body: InstRef,
        span: Span,
        struct_type: Type,
        has_self: bool,
    ) -> CompileResult<AnalyzedFunction> {
        let ret_type = self.resolve_type(return_type, span)?;

        // Build parameter list, adding self as first parameter for methods
        let mut param_info: Vec<(Symbol, Type, RirParamMode)> = Vec::new();

        if has_self {
            // Add self parameter (Normal mode - passed by value)
            let self_sym = self.interner.intern("self");
            param_info.push((self_sym, struct_type, RirParamMode::Normal));
        }

        // Add regular parameters with their modes
        for p in params.iter() {
            let ty = self.resolve_type(p.ty, span)?;
            param_info.push((p.name, ty, p.mode));
        }

        let (air, num_locals, num_param_slots, param_modes) =
            self.analyze_function(ret_type, &param_info, body)?;

        Ok(AnalyzedFunction {
            name: full_name.to_string(),
            air,
            num_locals,
            num_param_slots,
            param_modes,
        })
    }

    /// Analyze a destructor function.
    fn analyze_destructor_function(
        &mut self,
        full_name: &str,
        body: InstRef,
        _span: Span,
        struct_type: Type,
    ) -> CompileResult<AnalyzedFunction> {
        // Destructors take self parameter and return unit
        let self_sym = self.interner.intern("self");
        let param_info: Vec<(Symbol, Type, RirParamMode)> =
            vec![(self_sym, struct_type, RirParamMode::Normal)];

        let (air, num_locals, num_param_slots, param_modes) =
            self.analyze_function(Type::Unit, &param_info, body)?;

        Ok(AnalyzedFunction {
            name: full_name.to_string(),
            air,
            num_locals,
            num_param_slots,
            param_modes,
        })
    }

    /// Check if a directive list contains the @copy directive
    fn has_copy_directive(&self, directives: &[RirDirective]) -> bool {
        let copy_sym = self.interner.get_symbol("copy");
        for directive in directives {
            if Some(directive.name) == copy_sym {
                return true;
            }
        }
        false
    }

    /// Collect all struct definitions from the RIR.
    fn collect_struct_definitions(&mut self) -> CompileResult<()> {
        for (_, inst) in self.rir.iter() {
            if let InstData::StructDecl {
                directives,
                name,
                fields,
            } = &inst.data
            {
                let struct_id = StructId(self.struct_defs.len() as u32);
                let struct_name = self.interner.get(*name).to_string();
                let is_copy = self.has_copy_directive(directives);

                // Check for duplicate field names
                let mut seen_fields: HashSet<Symbol> = HashSet::new();
                for (field_name, _) in fields {
                    if !seen_fields.insert(*field_name) {
                        let field_name_str = self.interner.get(*field_name).to_string();
                        return Err(CompileError::new(
                            ErrorKind::DuplicateField {
                                struct_name: struct_name.clone(),
                                field_name: field_name_str,
                            },
                            inst.span,
                        ));
                    }
                }

                // Resolve field types (can only be primitive types for now, or other structs)
                let mut resolved_fields = Vec::new();
                for (field_name, field_type) in fields {
                    let field_ty = self.resolve_type(*field_type, inst.span)?;
                    resolved_fields.push(StructField {
                        name: self.interner.get(*field_name).to_string(),
                        ty: field_ty,
                    });
                }

                self.struct_defs.push(StructDef {
                    name: struct_name,
                    fields: resolved_fields,
                    is_copy,
                    destructor: None, // Filled in during collect_destructor_definitions
                });
                self.structs.insert(*name, struct_id);
            }
        }
        Ok(())
    }

    /// Collect all enum definitions from the RIR.
    fn collect_enum_definitions(&mut self) -> CompileResult<()> {
        for (_, inst) in self.rir.iter() {
            if let InstData::EnumDecl { name, variants } = &inst.data {
                let enum_id = EnumId(self.enum_defs.len() as u32);
                let enum_name = self.interner.get(*name).to_string();

                // Check for duplicate variant names
                let mut seen_variants: HashSet<Symbol> = HashSet::new();
                for variant_name in variants {
                    if !seen_variants.insert(*variant_name) {
                        let variant_name_str = self.interner.get(*variant_name).to_string();
                        return Err(CompileError::new(
                            ErrorKind::DuplicateVariant {
                                enum_name: enum_name.clone(),
                                variant_name: variant_name_str,
                            },
                            inst.span,
                        ));
                    }
                }

                // Convert variant symbols to strings
                let variant_names: Vec<String> = variants
                    .iter()
                    .map(|v| self.interner.get(*v).to_string())
                    .collect();

                self.enum_defs.push(EnumDef {
                    name: enum_name,
                    variants: variant_names,
                });
                self.enums.insert(*name, enum_id);
            }
        }
        Ok(())
    }

    /// Get a human-readable name for a type.
    fn format_type_name(&self, ty: Type) -> String {
        match ty {
            Type::I8 => "i8".to_string(),
            Type::I16 => "i16".to_string(),
            Type::I32 => "i32".to_string(),
            Type::I64 => "i64".to_string(),
            Type::U8 => "u8".to_string(),
            Type::U16 => "u16".to_string(),
            Type::U32 => "u32".to_string(),
            Type::U64 => "u64".to_string(),
            Type::Bool => "bool".to_string(),
            Type::Unit => "()".to_string(),
            Type::Never => "!".to_string(),
            Type::Error => "<error>".to_string(),
            Type::String => "String".to_string(),
            Type::Struct(struct_id) => self.struct_defs[struct_id.0 as usize].name.clone(),
            Type::Enum(enum_id) => self.enum_defs[enum_id.0 as usize].name.clone(),
            Type::Array(array_id) => {
                let array_def = &self.array_type_defs[array_id.0 as usize];
                format!(
                    "[{}; {}]",
                    self.format_type_name(array_def.element_type),
                    array_def.length
                )
            }
        }
    }

    /// Check if a type is a Copy type.
    /// This differs from Type::is_copy() because it can look up struct definitions
    /// to check if a struct is marked with @copy.
    fn is_type_copy(&self, ty: Type) -> bool {
        match ty {
            // Primitive Copy types
            Type::I8
            | Type::I16
            | Type::I32
            | Type::I64
            | Type::U8
            | Type::U16
            | Type::U32
            | Type::U64
            | Type::Bool
            | Type::Unit => true,
            // Enum types are Copy (they're small discriminant values)
            Type::Enum(_) => true,
            // Never and Error are Copy for convenience
            Type::Never | Type::Error => true,
            // Struct types: check if marked with @copy
            Type::Struct(struct_id) => {
                let struct_def = &self.struct_defs[struct_id.0 as usize];
                struct_def.is_copy
            }
            // String is a move type (owns heap data)
            Type::String => false,
            // Arrays are move types for now
            // TODO: Arrays of Copy types could be Copy
            Type::Array(_) => false,
        }
    }

    /// Validate that @copy structs only contain Copy type fields.
    /// Must be called after collect_struct_definitions.
    fn validate_copy_structs(&self) -> CompileResult<()> {
        for (_, inst) in self.rir.iter() {
            if let InstData::StructDecl {
                directives,
                name,
                fields: _,
            } = &inst.data
            {
                let is_copy = self.has_copy_directive(directives);
                if !is_copy {
                    continue;
                }

                let struct_name = self.interner.get(*name).to_string();
                let struct_id = *self.structs.get(name).unwrap();
                let struct_def = &self.struct_defs[struct_id.0 as usize];

                for field in &struct_def.fields {
                    if !self.is_type_copy(field.ty) {
                        let field_type_name = self.format_type_name(field.ty);
                        return Err(CompileError::new(
                            ErrorKind::CopyStructNonCopyField(Box::new(
                                CopyStructNonCopyFieldError {
                                    struct_name,
                                    field_name: field.name.clone(),
                                    field_type: field_type_name,
                                },
                            )),
                            inst.span,
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    /// Collect all user-defined destructor definitions.
    /// Must be called after collect_struct_definitions.
    fn collect_destructor_definitions(&mut self) -> CompileResult<()> {
        for (_, inst) in self.rir.iter() {
            if let InstData::DropFnDecl { type_name, body: _ } = &inst.data {
                let type_name_str = self.interner.get(*type_name).to_string();

                // Check that the type exists and is a struct
                let struct_id = match self.structs.get(type_name) {
                    Some(id) => *id,
                    None => {
                        return Err(CompileError::new(
                            ErrorKind::DestructorUnknownType {
                                type_name: type_name_str,
                            },
                            inst.span,
                        ));
                    }
                };

                // Check for duplicate destructor
                let struct_def = &self.struct_defs[struct_id.0 as usize];
                if struct_def.destructor.is_some() {
                    return Err(CompileError::new(
                        ErrorKind::DuplicateDestructor {
                            type_name: type_name_str,
                        },
                        inst.span,
                    ));
                }

                // Register the destructor with the struct
                // The destructor function will be named "TypeName.__drop"
                let destructor_name = format!("{}.__drop", type_name_str);
                self.struct_defs[struct_id.0 as usize].destructor = Some(destructor_name);
            }
        }
        Ok(())
    }

    /// Collect all function signatures for forward reference
    fn collect_function_signatures(&mut self) -> CompileResult<()> {
        for (_, inst) in self.rir.iter() {
            if let InstData::FnDecl {
                name,
                params,
                return_type,
                ..
            } = &inst.data
            {
                let ret_type = self.resolve_type(*return_type, inst.span)?;
                let param_types: Vec<Type> = params
                    .iter()
                    .map(|p| self.resolve_type(p.ty, inst.span))
                    .collect::<CompileResult<Vec<_>>>()?;
                let param_modes: Vec<RirParamMode> = params.iter().map(|p| p.mode).collect();

                self.functions.insert(
                    *name,
                    FunctionInfo {
                        param_types,
                        param_modes,
                        return_type: ret_type,
                    },
                );
            }
        }
        Ok(())
    }

    /// Collect all method definitions from impl blocks.
    ///
    /// This processes all ImplDecl instructions and builds the method table
    /// that maps (struct_name, method_name) to MethodInfo.
    fn collect_method_definitions(&mut self) -> CompileResult<()> {
        for (_, inst) in self.rir.iter() {
            if let InstData::ImplDecl { type_name, methods } = &inst.data {
                // Check that the type exists
                let struct_id = match self.structs.get(type_name) {
                    Some(id) => *id,
                    None => {
                        let type_name_str = self.interner.get(*type_name).to_string();
                        return Err(CompileError::new(
                            ErrorKind::UnknownType(type_name_str),
                            inst.span,
                        ));
                    }
                };
                let struct_type = Type::Struct(struct_id);

                // Process each method in the impl block
                for method_ref in methods {
                    let method_inst = self.rir.get(*method_ref);
                    if let InstData::FnDecl {
                        name: method_name,
                        params,
                        return_type,
                        body,
                        has_self,
                        ..
                    } = &method_inst.data
                    {
                        // Check for duplicate method names
                        let key = (*type_name, *method_name);
                        if self.methods.contains_key(&key) {
                            let type_name_str = self.interner.get(*type_name).to_string();
                            let method_name_str = self.interner.get(*method_name).to_string();
                            return Err(CompileError::new(
                                ErrorKind::DuplicateMethod {
                                    type_name: type_name_str,
                                    method_name: method_name_str,
                                },
                                method_inst.span,
                            ));
                        }

                        // Resolve parameter types
                        let param_names: Vec<Symbol> = params.iter().map(|p| p.name).collect();
                        let param_types: Vec<Type> = params
                            .iter()
                            .map(|p| self.resolve_type(p.ty, method_inst.span))
                            .collect::<CompileResult<Vec<_>>>()?;
                        let ret_type = self.resolve_type(*return_type, method_inst.span)?;

                        self.methods.insert(
                            key,
                            MethodInfo {
                                struct_type,
                                has_self: *has_self,
                                param_names,
                                param_types,
                                return_type: ret_type,
                                body: *body,
                                span: method_inst.span,
                            },
                        );
                    }
                }
            }
        }
        Ok(())
    }

    /// Analyze a single function, producing AIR.
    /// Returns (air, num_locals, num_param_slots, param_modes).
    fn analyze_function(
        &mut self,
        return_type: Type,
        params: &[(Symbol, Type, RirParamMode)], // (name, type, mode)
        body: InstRef,
    ) -> CompileResult<(Air, u32, u32, Vec<bool>)> {
        let mut air = Air::new(return_type);
        let mut param_map: HashMap<Symbol, ParamInfo> = HashMap::new();
        let mut param_modes: Vec<bool> = Vec::new();

        // Add parameters to the param map, tracking ABI slot offsets.
        // Each parameter starts at the next available ABI slot.
        // For struct parameters, the slot count is the number of fields.
        let mut next_abi_slot: u32 = 0;
        for (pname, ptype, mode) in params.iter() {
            param_map.insert(
                *pname,
                ParamInfo {
                    abi_slot: next_abi_slot,
                    ty: *ptype,
                    mode: *mode,
                },
            );
            // Both inout and borrow are passed by reference (as a pointer = 1 slot)
            let is_by_ref = *mode != RirParamMode::Normal;
            let slot_count = if is_by_ref {
                // By-ref parameters are always 1 slot (pointer)
                1
            } else {
                self.abi_slot_count(*ptype)
            };
            for _ in 0..slot_count {
                param_modes.push(is_by_ref);
            }
            next_abi_slot += slot_count;
        }
        let num_param_slots = next_abi_slot;

        // ======================================================================
        // Phase 1-2: Hindley-Milner Type Inference
        // ======================================================================
        // Run constraint generation and unification to determine types
        // for all expressions BEFORE emitting AIR.
        let resolved_types = self.run_type_inference(return_type, params, body)?;

        // Create analysis context with resolved types
        let mut ctx = AnalysisContext {
            locals: HashMap::new(),
            params: &param_map,
            next_slot: 0,
            loop_depth: 0,
            used_locals: HashSet::new(),
            return_type,
            scope_stack: Vec::new(),
            resolved_types: &resolved_types,
            moved_vars: HashMap::new(),
        };

        // ======================================================================
        // Phase 3: AIR Emission
        // ======================================================================
        // Analyze the body expression, emitting AIR with resolved types
        let body_result = self.analyze_inst(&mut air, body, &mut ctx)?;

        // Add implicit return only if body doesn't already diverge (e.g., explicit return)
        if body_result.ty != Type::Never {
            air.add_inst(AirInst {
                data: AirInstData::Ret(Some(body_result.air_ref)),
                ty: return_type,
                span: self.rir.get(body).span,
            });
        }

        Ok((air, ctx.next_slot, num_param_slots, param_modes))
    }

    /// Run Hindley-Milner type inference on a function body.
    ///
    /// This is Phases 1-2 of the HM algorithm:
    /// 1. Generate constraints by walking the RIR
    /// 2. Solve constraints via unification
    ///
    /// Returns a map from RIR instruction refs to their resolved concrete types.
    fn run_type_inference(
        &mut self,
        return_type: Type,
        params: &[(Symbol, Type, RirParamMode)],
        body: InstRef,
    ) -> CompileResult<HashMap<InstRef, Type>> {
        // Build function signatures map for the constraint generator.
        // Convert Type to InferType so arrays are represented structurally.
        let func_sigs: HashMap<Symbol, FunctionSig> = self
            .functions
            .iter()
            .map(|(name, info)| {
                (
                    *name,
                    FunctionSig {
                        param_types: info
                            .param_types
                            .iter()
                            .map(|t| self.type_to_infer_type(*t))
                            .collect(),
                        return_type: self.type_to_infer_type(info.return_type),
                    },
                )
            })
            .collect();

        // Build struct types map (name -> Type::Struct(id))
        let struct_types: HashMap<Symbol, Type> = self
            .structs
            .iter()
            .map(|(name, id)| (*name, Type::Struct(*id)))
            .collect();

        // Build enum types map (name -> Type::Enum(id))
        let enum_types: HashMap<Symbol, Type> = self
            .enums
            .iter()
            .map(|(name, id)| (*name, Type::Enum(*id)))
            .collect();

        // Build method signatures map for the constraint generator
        let method_sigs: HashMap<(Symbol, Symbol), MethodSig> = self
            .methods
            .iter()
            .map(|((type_name, method_name), info)| {
                (
                    (*type_name, *method_name),
                    MethodSig {
                        struct_type: info.struct_type,
                        has_self: info.has_self,
                        param_types: info
                            .param_types
                            .iter()
                            .map(|t| self.type_to_infer_type(*t))
                            .collect(),
                        return_type: self.type_to_infer_type(info.return_type),
                    },
                )
            })
            .collect();

        // Create constraint generator
        let mut cgen = ConstraintGenerator::new(
            self.rir,
            self.interner,
            &func_sigs,
            &struct_types,
            &enum_types,
            &method_sigs,
        );

        // Build parameter map for constraint context.
        // Convert Type to InferType so arrays are represented structurally.
        let param_vars: HashMap<Symbol, ParamVarInfo> = params
            .iter()
            .map(|(name, ty, _mode)| {
                (
                    *name,
                    ParamVarInfo {
                        ty: self.type_to_infer_type(*ty),
                    },
                )
            })
            .collect();

        // Create constraint context
        let mut cgen_ctx = ConstraintContext::new(&param_vars, return_type);

        // Phase 1: Generate constraints
        let body_info = cgen.generate(body, &mut cgen_ctx);

        // The function body's type must match the return type.
        // This handles implicit returns like `fn foo() -> i8 { 42 }`.
        cgen.add_constraint(Constraint::equal(
            body_info.ty,
            InferType::Concrete(return_type),
            body_info.span,
        ));

        // Consume the constraint generator to release borrows
        let (constraints, int_literal_vars, expr_types) = cgen.into_parts();

        // Phase 2: Solve constraints via unification
        let mut unifier = Unifier::new();
        let errors = unifier.solve_constraints(&constraints);

        // Convert unification errors to compile errors
        // For now, we collect the first error. In the future, we could
        // report multiple errors for better diagnostics.
        if let Some(err) = errors.first() {
            // Map each UnifyResult variant to the appropriate ErrorKind
            let error_kind = match &err.kind {
                UnifyResult::Ok => unreachable!("UnificationError should never contain Ok"),
                UnifyResult::TypeMismatch { expected, found } => ErrorKind::TypeMismatch {
                    expected: expected.to_string(),
                    found: found.to_string(),
                },
                UnifyResult::IntLiteralNonInteger { found } => ErrorKind::TypeMismatch {
                    expected: "integer type".to_string(),
                    found: found.name().to_string(),
                },
                UnifyResult::OccursCheck { var, ty } => ErrorKind::TypeMismatch {
                    expected: "non-recursive type".to_string(),
                    found: format!("{var} = {ty} (infinite type)"),
                },
                UnifyResult::NotSigned { ty } => {
                    ErrorKind::CannotNegateUnsigned(ty.name().to_string())
                }
                UnifyResult::NotInteger { ty } => ErrorKind::TypeMismatch {
                    expected: "integer type".to_string(),
                    found: ty.name().to_string(),
                },
                UnifyResult::NotUnsigned { ty } => ErrorKind::TypeMismatch {
                    expected: "unsigned integer type".to_string(),
                    found: ty.name().to_string(),
                },
                UnifyResult::ArrayLengthMismatch { expected, found } => {
                    ErrorKind::ArrayLengthMismatch {
                        expected: *expected,
                        found: *found,
                    }
                }
            };

            let mut compile_error = CompileError::new(error_kind, err.span);

            // Add note for unsigned negation errors
            if matches!(err.kind, UnifyResult::NotSigned { .. }) {
                compile_error = compile_error.with_note("unsigned values cannot be negated");
            }

            return Err(compile_error);
        }

        // Default any unconstrained integer literals to i32
        unifier.default_int_literal_vars(&int_literal_vars);

        // Build the resolved types map, converting InferType to Type
        let mut resolved_types = HashMap::new();
        for (inst_ref, infer_ty) in &expr_types {
            let resolved = unifier.resolve_infer_type(infer_ty);
            let concrete_ty = self.infer_type_to_type(&resolved);
            resolved_types.insert(*inst_ref, concrete_ty);
        }

        Ok(resolved_types)
    }

    /// Convert a fully-resolved InferType to a concrete Type.
    ///
    /// This handles the conversion of InferType::Array to Type::Array(id)
    /// by using the array type registry.
    fn infer_type_to_type(&mut self, ty: &InferType) -> Type {
        match ty {
            InferType::Concrete(t) => *t,
            InferType::Var(_) => Type::Error,   // Unbound variable
            InferType::IntLiteral => Type::I32, // Default (shouldn't happen after resolution)
            InferType::Array { element, length } => {
                // Recursively convert element type
                let elem_ty = self.infer_type_to_type(element);
                if elem_ty == Type::Error {
                    return Type::Error;
                }
                // Get or create the array type ID
                let array_type_id = self.get_or_create_array_type(elem_ty, *length);
                Type::Array(array_type_id)
            }
        }
    }

    /// Convert a concrete Type to InferType for use in constraint generation.
    ///
    /// This handles the conversion of Type::Array(id) to InferType::Array
    /// by looking up the array definition to get element type and length.
    fn type_to_infer_type(&self, ty: Type) -> InferType {
        match ty {
            Type::Array(array_id) => {
                let array_def = &self.array_type_defs[array_id.0 as usize];
                let element_infer = self.type_to_infer_type(array_def.element_type);
                InferType::Array {
                    element: Box::new(element_infer),
                    length: array_def.length,
                }
            }
            // All other types wrap directly
            _ => InferType::Concrete(ty),
        }
    }

    /// Analyze an RIR instruction for projection (field access).
    ///
    /// This is like `analyze_inst` but does NOT mark non-Copy values as moved.
    /// Used for field access where we're reading from a struct without consuming it.
    /// We still check that the variable hasn't already been moved.
    fn analyze_inst_for_projection(
        &mut self,
        air: &mut Air,
        inst_ref: InstRef,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let inst = self.rir.get(inst_ref);

        // For VarRef, we handle it specially: check for moves but don't mark as moved
        if let InstData::VarRef { name } = &inst.data {
            // First check if it's a parameter
            if let Some(param_info) = ctx.params.get(name) {
                let ty = param_info.ty;

                // Check if this parameter has been moved
                if let Some(move_info) = ctx.moved_vars.get(name) {
                    let name_str = self.interner.get(*name);
                    return Err(CompileError::new(
                        ErrorKind::UseAfterMove(name_str.to_string()),
                        inst.span,
                    )
                    .with_label("value moved here", move_info.moved_at));
                }

                // NOTE: We do NOT mark as moved here - this is a projection

                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::Param {
                        index: param_info.abi_slot,
                    },
                    ty,
                    span: inst.span,
                });
                return Ok(AnalysisResult::new(air_ref, ty));
            }

            // Look up the variable in locals
            let name_str = self.interner.get(*name);
            let local = ctx.locals.get(name).ok_or_compile_error(
                ErrorKind::UndefinedVariable(name_str.to_string()),
                inst.span,
            )?;

            let ty = local.ty;
            let slot = local.slot;

            // Check if this variable has been moved
            if let Some(move_info) = ctx.moved_vars.get(name) {
                return Err(CompileError::new(
                    ErrorKind::UseAfterMove(name_str.to_string()),
                    inst.span,
                )
                .with_label("value moved here", move_info.moved_at));
            }

            // NOTE: We do NOT mark as moved here - this is a projection

            // Mark variable as used
            ctx.used_locals.insert(*name);

            // Load the variable
            let air_ref = air.add_inst(AirInst {
                data: AirInstData::Load { slot },
                ty,
                span: inst.span,
            });
            return Ok(AnalysisResult::new(air_ref, ty));
        }

        // For nested field access (e.g., a.b.c), recursively use projection mode
        if let InstData::FieldGet { base, field } = &inst.data {
            let base_result = self.analyze_inst_for_projection(air, *base, ctx)?;
            let base_type = base_result.ty;

            let struct_id = match base_type {
                Type::Struct(id) => id,
                _ => {
                    return Err(CompileError::new(
                        ErrorKind::FieldAccessOnNonStruct {
                            found: base_type.name().to_string(),
                        },
                        inst.span,
                    ));
                }
            };

            let struct_def = &self.struct_defs[struct_id.0 as usize];
            let field_name_str = self.interner.get(*field).to_string();

            let (field_index, struct_field) =
                struct_def.find_field(&field_name_str).ok_or_compile_error(
                    ErrorKind::UnknownField {
                        struct_name: struct_def.name.clone(),
                        field_name: field_name_str.clone(),
                    },
                    inst.span,
                )?;

            let field_type = struct_field.ty;

            let air_ref = air.add_inst(AirInst {
                data: AirInstData::FieldGet {
                    base: base_result.air_ref,
                    struct_id,
                    field_index: field_index as u32,
                },
                ty: field_type,
                span: inst.span,
            });
            return Ok(AnalysisResult::new(air_ref, field_type));
        }

        // For other expressions, use the normal analyze_inst
        // (they will trigger move semantics as expected)
        self.analyze_inst(air, inst_ref, ctx)
    }

    /// Analyze an RIR instruction, producing AIR instructions.
    ///
    /// Types are determined by Hindley-Milner inference (stored in `resolved_types`).
    /// Returns both the AIR reference and the synthesized type.
    fn analyze_inst(
        &mut self,
        air: &mut Air,
        inst_ref: InstRef,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let inst = self.rir.get(inst_ref);

        match &inst.data {
            InstData::IntConst(value) => {
                // Get the type from HM inference
                let ty = ctx
                    .resolved_types
                    .get(&inst_ref)
                    .copied()
                    .expect("HM inference should provide type for all expressions");

                // Check if the literal value fits in the target type's range
                if !ty.literal_fits(*value) {
                    return Err(CompileError::new(
                        ErrorKind::LiteralOutOfRange {
                            value: *value,
                            ty: ty.name().to_string(),
                        },
                        inst.span,
                    ));
                }

                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::Const(*value),
                    ty,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, ty))
            }

            InstData::BoolConst(value) => {
                let ty = Type::Bool;
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::BoolConst(*value),
                    ty,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, ty))
            }

            InstData::StringConst(symbol) => {
                let ty = Type::String;
                // Add string to the string table
                let string_content = self.interner.get(*symbol).to_string();
                let string_id = self.add_string(string_content);

                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::StringConst(string_id),
                    ty,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, ty))
            }

            InstData::UnitConst => {
                let ty = Type::Unit;
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::UnitConst,
                    ty,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, ty))
            }

            InstData::Add { lhs, rhs } => {
                self.analyze_binary_arith(air, *lhs, *rhs, AirInstData::Add, inst.span, ctx)
            }

            InstData::Sub { lhs, rhs } => {
                self.analyze_binary_arith(air, *lhs, *rhs, AirInstData::Sub, inst.span, ctx)
            }

            InstData::Mul { lhs, rhs } => {
                self.analyze_binary_arith(air, *lhs, *rhs, AirInstData::Mul, inst.span, ctx)
            }

            InstData::Div { lhs, rhs } => {
                self.analyze_binary_arith(air, *lhs, *rhs, AirInstData::Div, inst.span, ctx)
            }

            InstData::Mod { lhs, rhs } => {
                self.analyze_binary_arith(air, *lhs, *rhs, AirInstData::Mod, inst.span, ctx)
            }

            // Comparison operators: operands must be the same type, result is bool.
            // We synthesize the type from the left operand and check the right against it.
            // Never and Error types are propagated without additional errors.
            // Equality operators (==, !=) also allow bool operands.
            InstData::Eq { lhs, rhs } => {
                self.analyze_comparison(air, *lhs, *rhs, true, AirInstData::Eq, inst.span, ctx)
            }

            InstData::Ne { lhs, rhs } => {
                self.analyze_comparison(air, *lhs, *rhs, true, AirInstData::Ne, inst.span, ctx)
            }

            InstData::Lt { lhs, rhs } => {
                self.analyze_comparison(air, *lhs, *rhs, false, AirInstData::Lt, inst.span, ctx)
            }

            InstData::Gt { lhs, rhs } => {
                self.analyze_comparison(air, *lhs, *rhs, false, AirInstData::Gt, inst.span, ctx)
            }

            InstData::Le { lhs, rhs } => {
                self.analyze_comparison(air, *lhs, *rhs, false, AirInstData::Le, inst.span, ctx)
            }

            InstData::Ge { lhs, rhs } => {
                self.analyze_comparison(air, *lhs, *rhs, false, AirInstData::Ge, inst.span, ctx)
            }

            // Logical operators: operands and result are all bool
            InstData::And { lhs, rhs } => {
                let lhs_result = self.analyze_inst(air, *lhs, ctx)?;
                let rhs_result = self.analyze_inst(air, *rhs, ctx)?;

                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::And(lhs_result.air_ref, rhs_result.air_ref),
                    ty: Type::Bool,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, Type::Bool))
            }

            InstData::Or { lhs, rhs } => {
                let lhs_result = self.analyze_inst(air, *lhs, ctx)?;
                let rhs_result = self.analyze_inst(air, *rhs, ctx)?;

                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::Or(lhs_result.air_ref, rhs_result.air_ref),
                    ty: Type::Bool,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, Type::Bool))
            }

            // Bitwise operations: operands must be same integer type, result is that type
            InstData::BitAnd { lhs, rhs } => {
                self.analyze_binary_arith(air, *lhs, *rhs, AirInstData::BitAnd, inst.span, ctx)
            }

            InstData::BitOr { lhs, rhs } => {
                self.analyze_binary_arith(air, *lhs, *rhs, AirInstData::BitOr, inst.span, ctx)
            }

            InstData::BitXor { lhs, rhs } => {
                self.analyze_binary_arith(air, *lhs, *rhs, AirInstData::BitXor, inst.span, ctx)
            }

            InstData::Shl { lhs, rhs } => {
                self.analyze_binary_arith(air, *lhs, *rhs, AirInstData::Shl, inst.span, ctx)
            }

            InstData::Shr { lhs, rhs } => {
                self.analyze_binary_arith(air, *lhs, *rhs, AirInstData::Shr, inst.span, ctx)
            }

            InstData::Neg { operand } => {
                // Get the resolved type from HM inference
                let ty = ctx
                    .resolved_types
                    .get(&inst_ref)
                    .copied()
                    .expect("HM inference should provide type for Neg");

                // Check if trying to negate an unsigned type.
                // Note: HM inference also checks this via IsSigned constraint, but that
                // check happens before type variables are fully resolved. For cases like
                // `let x: u32 = -5`, the literal's type variable isn't bound to u32 until
                // after the IsSigned check runs, so this sema check catches those cases.
                if ty.is_unsigned() {
                    return Err(CompileError::new(
                        ErrorKind::CannotNegateUnsigned(ty.name().to_string()),
                        inst.span,
                    )
                    .with_note("unsigned values cannot be negated"));
                }

                // Special case: negating a literal that equals |MIN| for signed types.
                // For example, -128 for i8, -32768 for i16, -2147483648 for i32, etc.
                // The positive literal exceeds the signed MAX, but the negated value is valid.
                let operand_inst = self.rir.get(*operand);
                if let InstData::IntConst(value) = &operand_inst.data {
                    // Check if this value, when negated, fits in the target signed type
                    if ty.negated_literal_fits(*value) && !ty.literal_fits(*value) {
                        // This is the MIN value case - the positive literal is out of range
                        // but the negated value is exactly the MIN of this type.
                        // Store the MIN value directly.
                        let neg_value = match ty {
                            Type::I8 => (i8::MIN as i64) as u64,
                            Type::I16 => (i16::MIN as i64) as u64,
                            Type::I32 => (i32::MIN as i64) as u64,
                            Type::I64 => i64::MIN as u64,
                            _ => unreachable!(),
                        };
                        let air_ref = air.add_inst(AirInst {
                            data: AirInstData::Const(neg_value),
                            ty,
                            span: inst.span,
                        });
                        return Ok(AnalysisResult::new(air_ref, ty));
                    }
                }

                let operand_result = self.analyze_inst(air, *operand, ctx)?;

                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::Neg(operand_result.air_ref),
                    ty,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, ty))
            }

            InstData::Not { operand } => {
                let operand_result = self.analyze_inst(air, *operand, ctx)?;

                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::Not(operand_result.air_ref),
                    ty: Type::Bool,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, Type::Bool))
            }

            InstData::BitNot { operand } => {
                // Get the resolved type from HM inference
                let ty = ctx
                    .resolved_types
                    .get(&inst_ref)
                    .copied()
                    .expect("HM inference should provide type for BitNot");

                // Bitwise NOT operates on integer types only
                if !ty.is_integer() && !ty.is_error() && !ty.is_never() {
                    return Err(CompileError::new(
                        ErrorKind::TypeMismatch {
                            expected: "integer type".to_string(),
                            found: ty.name().to_string(),
                        },
                        inst.span,
                    ));
                }

                let operand_result = self.analyze_inst(air, *operand, ctx)?;

                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::BitNot(operand_result.air_ref),
                    ty,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, ty))
            }

            InstData::Branch {
                cond,
                then_block,
                else_block,
            } => {
                // Condition must be bool
                let cond_result = self.analyze_inst(air, *cond, ctx)?;

                // Determine the result type:
                // - If else is present, both branches must have compatible types
                //   (Never type can coerce to any type)
                // - If else is absent, the result is Unit
                if let Some(else_b) = else_block {
                    // Analyze then branch with its own scope
                    ctx.push_scope();
                    let then_result = self.analyze_inst(air, *then_block, ctx)?;
                    let then_type = then_result.ty;
                    let then_span = self.rir.get(*then_block).span;
                    ctx.pop_scope();

                    // Analyze else branch with its own scope
                    ctx.push_scope();
                    let else_result = self.analyze_inst(air, *else_b, ctx)?;
                    let else_type = else_result.ty;
                    let else_span = self.rir.get(*else_b).span;
                    ctx.pop_scope();

                    // Compute the unified result type using never type coercion:
                    // - If both branches are Never, result is Never
                    // - If one branch is Never, result is the other branch's type
                    // - Otherwise, types must match exactly
                    let result_type = match (then_type.is_never(), else_type.is_never()) {
                        (true, true) => Type::Never,
                        (true, false) => else_type,
                        (false, true) => then_type,
                        (false, false) => {
                            // Neither diverges - types must match exactly
                            if then_type != else_type
                                && !then_type.is_error()
                                && !else_type.is_error()
                            {
                                return Err(CompileError::new(
                                    ErrorKind::TypeMismatch {
                                        expected: then_type.name().to_string(),
                                        found: else_type.name().to_string(),
                                    },
                                    else_span,
                                )
                                .with_label(
                                    format!("this is of type `{}`", then_type.name()),
                                    then_span,
                                )
                                .with_note("if and else branches must have compatible types"));
                            }
                            then_type
                        }
                    };

                    let air_ref = air.add_inst(AirInst {
                        data: AirInstData::Branch {
                            cond: cond_result.air_ref,
                            then_value: then_result.air_ref,
                            else_value: Some(else_result.air_ref),
                        },
                        ty: result_type,
                        span: inst.span,
                    });
                    Ok(AnalysisResult::new(air_ref, result_type))
                } else {
                    // No else branch - result is Unit
                    // The then branch must have unit type (spec 4.6:5)
                    ctx.push_scope();
                    let then_result = self.analyze_inst(air, *then_block, ctx)?;
                    ctx.pop_scope();

                    // Check that the then branch has unit type (or Never/Error)
                    let then_type = then_result.ty;
                    if then_type != Type::Unit && !then_type.is_never() && !then_type.is_error() {
                        return Err(CompileError::new(
                            ErrorKind::TypeMismatch {
                                expected: "()".to_string(),
                                found: then_type.name().to_string(),
                            },
                            self.rir.get(*then_block).span,
                        )
                        .with_help(
                            "if expressions without else must have unit type; \
                             consider adding an else branch or making the body return ()",
                        ));
                    }

                    let air_ref = air.add_inst(AirInst {
                        data: AirInstData::Branch {
                            cond: cond_result.air_ref,
                            then_value: then_result.air_ref,
                            else_value: None,
                        },
                        ty: Type::Unit,
                        span: inst.span,
                    });
                    Ok(AnalysisResult::new(air_ref, Type::Unit))
                }
            }

            InstData::Loop { cond, body } => {
                // While loop: condition must be bool, result is Unit
                let cond_result = self.analyze_inst(air, *cond, ctx)?;

                // Analyze body with its own scope - while body is Unit type
                // Increment loop_depth so break/continue inside the body are valid
                ctx.push_scope();
                ctx.loop_depth += 1;
                let body_result = self.analyze_inst(air, *body, ctx)?;
                ctx.loop_depth -= 1;
                ctx.pop_scope();

                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::Loop {
                        cond: cond_result.air_ref,
                        body: body_result.air_ref,
                    },
                    ty: Type::Unit,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, Type::Unit))
            }

            InstData::InfiniteLoop { body } => {
                // Infinite loop: `loop { body }` - always produces Never type
                // The loop never terminates normally (only via break, which is handled separately)

                // Analyze body with its own scope - loop body is Unit type
                // Increment loop_depth so break/continue inside the body are valid
                ctx.push_scope();
                ctx.loop_depth += 1;
                let body_result = self.analyze_inst(air, *body, ctx)?;
                ctx.loop_depth -= 1;
                ctx.pop_scope();

                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::InfiniteLoop {
                        body: body_result.air_ref,
                    },
                    ty: Type::Never,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, Type::Never))
            }

            InstData::Match { scrutinee, arms } => {
                // Analyze the scrutinee to determine its type
                let scrutinee_result = self.analyze_inst(air, *scrutinee, ctx)?;
                let scrutinee_type = scrutinee_result.ty;

                // Validate that we can match on this type (integers, booleans, and enums)
                if !scrutinee_type.is_integer()
                    && scrutinee_type != Type::Bool
                    && !scrutinee_type.is_enum()
                {
                    return Err(CompileError::new(
                        ErrorKind::InvalidMatchType(scrutinee_type.name().to_string()),
                        inst.span,
                    ));
                }

                // Check for empty match
                if arms.is_empty() {
                    return Err(CompileError::new(ErrorKind::EmptyMatch, inst.span));
                }

                // Track patterns for exhaustiveness checking and duplicate detection
                let mut wildcard_span: Option<Span> = None;
                let mut bool_true_span: Option<Span> = None;
                let mut bool_false_span: Option<Span> = None;
                let mut seen_ints: HashMap<i64, Span> = HashMap::new();
                // Track covered enum variants (variant_index -> true if covered)
                let mut covered_variants: HashSet<u32> = HashSet::new();
                // Track span of first occurrence of each variant for duplicate detection
                let mut seen_variants: HashMap<u32, Span> = HashMap::new();
                // For enum exhaustiveness, store the enum_id if we find path patterns
                let mut pattern_enum_id: Option<EnumId> = None;

                // Analyze each arm (each arm gets its own scope)
                let mut air_arms = Vec::new();
                let mut result_type: Option<Type> = None;

                for (pattern, body) in arms.iter() {
                    // Check for unreachable patterns (duplicates or patterns after wildcard)
                    let pattern_span = pattern.span();

                    // If we've seen a wildcard, everything after is unreachable
                    if let Some(first_wildcard_span) = wildcard_span {
                        let pat_str = match pattern {
                            RirPattern::Wildcard(_) => "_".to_string(),
                            RirPattern::Int(n, _) => n.to_string(),
                            RirPattern::Bool(b, _) => b.to_string(),
                            RirPattern::Path {
                                type_name, variant, ..
                            } => {
                                format!(
                                    "{}::{}",
                                    self.interner.get(*type_name),
                                    self.interner.get(*variant)
                                )
                            }
                        };
                        self.warnings.push(
                            CompileWarning::new(
                                WarningKind::UnreachablePattern(pat_str),
                                pattern_span,
                            )
                            .with_label("previous wildcard pattern here", first_wildcard_span)
                            .with_note(
                                "this pattern will never be matched because the wildcard pattern above matches everything",
                            ),
                        );
                    }

                    // Validate pattern against scrutinee type and check for duplicates
                    match pattern {
                        RirPattern::Wildcard(_) => {
                            if wildcard_span.is_none() {
                                wildcard_span = Some(pattern_span);
                            }
                            // Note: duplicate wildcards are already caught by the "pattern after wildcard" check above
                        }
                        RirPattern::Int(n, _) => {
                            if !scrutinee_type.is_integer() {
                                return Err(CompileError::new(
                                    ErrorKind::TypeMismatch {
                                        expected: scrutinee_type.name().to_string(),
                                        found: "integer".to_string(),
                                    },
                                    pattern_span,
                                ));
                            }
                            // Check for duplicate integer pattern
                            if let Some(first_span) = seen_ints.get(n) {
                                if wildcard_span.is_none() {
                                    // Only emit if not already covered by wildcard warning
                                    self.warnings.push(
                                        CompileWarning::new(
                                            WarningKind::UnreachablePattern(n.to_string()),
                                            pattern_span,
                                        )
                                        .with_label("first occurrence of this pattern", *first_span)
                                        .with_note(
                                            "this pattern will never be matched because an earlier arm already matches the same value",
                                        ),
                                    );
                                }
                            } else {
                                seen_ints.insert(*n, pattern_span);
                            }
                        }
                        RirPattern::Bool(b, _) => {
                            if scrutinee_type != Type::Bool {
                                return Err(CompileError::new(
                                    ErrorKind::TypeMismatch {
                                        expected: scrutinee_type.name().to_string(),
                                        found: "bool".to_string(),
                                    },
                                    pattern_span,
                                ));
                            }
                            // Check for duplicate boolean pattern
                            let (first_span_opt, is_true) = if *b {
                                (&mut bool_true_span, true)
                            } else {
                                (&mut bool_false_span, false)
                            };
                            if let Some(first_span) = *first_span_opt {
                                if wildcard_span.is_none() {
                                    // Only emit if not already covered by wildcard warning
                                    self.warnings.push(
                                        CompileWarning::new(
                                            WarningKind::UnreachablePattern(is_true.to_string()),
                                            pattern_span,
                                        )
                                        .with_label("first occurrence of this pattern", first_span)
                                        .with_note(
                                            "this pattern will never be matched because an earlier arm already matches the same value",
                                        ),
                                    );
                                }
                            } else {
                                *first_span_opt = Some(pattern_span);
                            }
                        }
                        RirPattern::Path {
                            type_name, variant, ..
                        } => {
                            // Look up the enum type
                            let enum_id = self.enums.get(type_name).ok_or_compile_error(
                                ErrorKind::UnknownEnumType(
                                    self.interner.get(*type_name).to_string(),
                                ),
                                pattern_span,
                            )?;
                            let enum_def = &self.enum_defs[enum_id.0 as usize];

                            // Check that scrutinee type matches the pattern's enum type
                            if scrutinee_type != Type::Enum(*enum_id) {
                                return Err(CompileError::new(
                                    ErrorKind::TypeMismatch {
                                        expected: scrutinee_type.name().to_string(),
                                        found: enum_def.name.clone(),
                                    },
                                    pattern_span,
                                ));
                            }

                            // Find the variant index
                            let variant_name = self.interner.get(*variant);
                            let variant_index =
                                enum_def.find_variant(variant_name).ok_or_compile_error(
                                    ErrorKind::UnknownVariant {
                                        enum_name: enum_def.name.clone(),
                                        variant_name: variant_name.to_string(),
                                    },
                                    pattern_span,
                                )?;

                            covered_variants.insert(variant_index as u32);
                            pattern_enum_id = Some(*enum_id);
                        }
                    }

                    // Each arm gets its own scope
                    ctx.push_scope();

                    // Analyze arm body
                    let body_result = self.analyze_inst(air, *body, ctx)?;
                    let body_type = body_result.ty;

                    ctx.pop_scope();

                    // Update result type (handle Never type coercion)
                    result_type = Some(match result_type {
                        None => body_type,
                        Some(prev) => {
                            if prev.is_never() {
                                body_type
                            } else if body_type.is_never() {
                                prev
                            } else if prev != body_type && !prev.is_error() && !body_type.is_error()
                            {
                                return Err(CompileError::new(
                                    ErrorKind::TypeMismatch {
                                        expected: prev.name().to_string(),
                                        found: body_type.name().to_string(),
                                    },
                                    self.rir.get(*body).span,
                                ));
                            } else {
                                prev
                            }
                        }
                    });

                    // Convert pattern to AIR pattern
                    let air_pattern = match pattern {
                        RirPattern::Wildcard(_) => AirPattern::Wildcard,
                        RirPattern::Int(n, _) => AirPattern::Int(*n),
                        RirPattern::Bool(b, _) => AirPattern::Bool(*b),
                        RirPattern::Path {
                            type_name, variant, ..
                        } => {
                            // We already validated this above, so unwrap is safe
                            let enum_id = *self.enums.get(type_name).unwrap();
                            let enum_def = &self.enum_defs[enum_id.0 as usize];
                            let variant_name = self.interner.get(*variant);
                            let variant_index = enum_def.find_variant(variant_name).unwrap();
                            AirPattern::EnumVariant {
                                enum_id,
                                variant_index: variant_index as u32,
                            }
                        }
                    };

                    air_arms.push((air_pattern, body_result.air_ref));
                }

                // Exhaustiveness checking
                let has_wildcard = wildcard_span.is_some();
                let bool_true_covered = bool_true_span.is_some();
                let bool_false_covered = bool_false_span.is_some();
                let is_exhaustive = if scrutinee_type == Type::Bool {
                    has_wildcard || (bool_true_covered && bool_false_covered)
                } else if let Some(enum_id) = pattern_enum_id {
                    // For enums, check all variants are covered or there's a wildcard
                    let enum_def = &self.enum_defs[enum_id.0 as usize];
                    has_wildcard || covered_variants.len() == enum_def.variant_count()
                } else {
                    // For integers, must have wildcard
                    has_wildcard
                };

                if !is_exhaustive {
                    return Err(CompileError::new(ErrorKind::NonExhaustiveMatch, inst.span));
                }

                let final_type = result_type.unwrap_or(Type::Unit);

                // Encode match arms into extra array
                let arms_len = air_arms.len() as u32;
                let mut extra_data = Vec::new();
                for (pattern, body) in &air_arms {
                    pattern.encode(*body, &mut extra_data);
                }
                let arms_start = air.add_extra(&extra_data);

                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::Match {
                        scrutinee: scrutinee_result.air_ref,
                        arms_start,
                        arms_len,
                    },
                    ty: final_type,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, final_type))
            }

            InstData::Alloc {
                directives,
                name,
                is_mut,
                ty: _,
                init,
            } => {
                // Analyze the initializer (move checking happens in analyze_inst for VarRef)
                let init_result = self.analyze_inst(air, *init, ctx)?;

                // The variable type is determined by HM inference (considering any annotation)
                // If there's a type annotation, HM will have constrained the init to match it.
                // If no annotation, HM infers from the initializer.
                let var_type = init_result.ty;

                // If name is None, this is a wildcard pattern `_` that discards the value
                // We still evaluate the initializer for side effects, but don't allocate a slot
                let Some(name) = name else {
                    // Just return the initializer result - we evaluated it, but discard it
                    // The result type is Unit since let statements produce unit
                    return Ok(AnalysisResult::new(init_result.air_ref, Type::Unit));
                };

                // Check if @allow(unused_variable) directive is present
                let allow_unused = self.has_allow_directive(directives, "unused_variable");

                // Allocate slots - structs and arrays need multiple slots
                // Use abi_slot_count which recursively computes total slots for nested types
                let slot = ctx.next_slot;
                let num_slots = self.abi_slot_count(var_type);
                ctx.next_slot += num_slots;

                // Register the variable (shadowing is allowed by just overwriting)
                ctx.insert_local(
                    *name,
                    LocalVar {
                        slot,
                        ty: var_type,
                        is_mut: *is_mut,
                        span: inst.span,
                        allow_unused,
                    },
                );

                // Emit StorageLive to mark the slot as live (for drop elaboration)
                let storage_live_ref = air.add_inst(AirInst {
                    data: AirInstData::StorageLive { slot },
                    ty: var_type,
                    span: inst.span,
                });

                // Emit the alloc instruction
                let alloc_ref = air.add_inst(AirInst {
                    data: AirInstData::Alloc {
                        slot,
                        init: init_result.air_ref,
                    },
                    ty: Type::Unit,
                    span: inst.span,
                });

                // Return a block containing both StorageLive and Alloc
                let stmts_start = air.add_extra(&[storage_live_ref.as_u32()]);
                let block_ref = air.add_inst(AirInst {
                    data: AirInstData::Block {
                        stmts_start,
                        stmts_len: 1,
                        value: alloc_ref,
                    },
                    ty: Type::Unit,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(block_ref, Type::Unit))
            }

            InstData::VarRef { name } => {
                // First check if it's a parameter
                if let Some(param_info) = ctx.params.get(name) {
                    let ty = param_info.ty;

                    // Check if this parameter has been moved
                    if let Some(move_info) = ctx.moved_vars.get(name) {
                        let name_str = self.interner.get(*name);
                        return Err(CompileError::new(
                            ErrorKind::UseAfterMove(name_str.to_string()),
                            inst.span,
                        )
                        .with_label("value moved here", move_info.moved_at));
                    }

                    // Handle move semantics based on parameter mode
                    if !self.is_type_copy(ty) {
                        match param_info.mode {
                            RirParamMode::Normal => {
                                // Normal (owned) parameters can be moved
                                ctx.moved_vars.insert(
                                    *name,
                                    MoveInfo {
                                        moved_at: inst.span,
                                    },
                                );
                            }
                            RirParamMode::Inout => {
                                // Inout parameters cannot be moved out of - they're returned to caller
                                // For now, we treat them like normal owned for move tracking
                                // (The caller still owns it, we just have mutable access)
                                ctx.moved_vars.insert(
                                    *name,
                                    MoveInfo {
                                        moved_at: inst.span,
                                    },
                                );
                            }
                            RirParamMode::Borrow => {
                                // Cannot move out of a borrowed parameter!
                                let name_str = self.interner.get(*name);
                                return Err(CompileError::new(
                                    ErrorKind::MoveOutOfBorrow {
                                        variable: name_str.to_string(),
                                    },
                                    inst.span,
                                ));
                            }
                        }
                    }

                    // Emit Param with the ABI slot (not the parameter index).
                    // For struct parameters, this is the starting slot of the first field.
                    let air_ref = air.add_inst(AirInst {
                        data: AirInstData::Param {
                            index: param_info.abi_slot,
                        },
                        ty,
                        span: inst.span,
                    });
                    return Ok(AnalysisResult::new(air_ref, ty));
                }

                // Look up the variable in locals
                let name_str = self.interner.get(*name);
                let local = ctx.locals.get(name).ok_or_compile_error(
                    ErrorKind::UndefinedVariable(name_str.to_string()),
                    inst.span,
                )?;

                let ty = local.ty;
                let slot = local.slot;

                // Check if this variable has been moved
                if let Some(move_info) = ctx.moved_vars.get(name) {
                    return Err(CompileError::new(
                        ErrorKind::UseAfterMove(name_str.to_string()),
                        inst.span,
                    )
                    .with_label("value moved here", move_info.moved_at));
                }

                // If type is not Copy, mark as moved
                if !self.is_type_copy(ty) {
                    ctx.moved_vars.insert(
                        *name,
                        MoveInfo {
                            moved_at: inst.span,
                        },
                    );
                }

                // Mark variable as used
                ctx.used_locals.insert(*name);

                // Load the variable
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::Load { slot },
                    ty,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, ty))
            }

            InstData::Assign { name, value } => {
                let name_str = self.interner.get(*name);

                // First check if it's a parameter (for inout params)
                if let Some(param_info) = ctx.params.get(name) {
                    // Check parameter mode - only inout can be assigned to
                    match param_info.mode {
                        RirParamMode::Normal => {
                            // Non-inout parameters are immutable
                            return Err(CompileError::new(
                                ErrorKind::AssignToImmutable(name_str.to_string()),
                                inst.span,
                            )
                            .with_help(format!(
                                "consider making parameter `{}` inout: `inout {}: {}`",
                                name_str,
                                name_str,
                                param_info.ty.name()
                            )));
                        }
                        RirParamMode::Inout => {
                            // Inout parameters can be assigned to - that's their purpose
                        }
                        RirParamMode::Borrow => {
                            // Borrow parameters CANNOT be assigned to
                            return Err(CompileError::new(
                                ErrorKind::MutateBorrowedValue {
                                    variable: name_str.to_string(),
                                },
                                inst.span,
                            ));
                        }
                    }

                    let abi_slot = param_info.abi_slot;

                    // Analyze the value
                    let value_result = self.analyze_inst(air, *value, ctx)?;

                    // Assignment to a parameter resets its move state
                    ctx.moved_vars.remove(name);

                    // Emit store to param slot (codegen will use the inout pointer)
                    let air_ref = air.add_inst(AirInst {
                        data: AirInstData::ParamStore {
                            param_slot: abi_slot,
                            value: value_result.air_ref,
                        },
                        ty: Type::Unit,
                        span: inst.span,
                    });
                    return Ok(AnalysisResult::new(air_ref, Type::Unit));
                }

                // Look up local variable
                let local = ctx.locals.get(name).ok_or_compile_error(
                    ErrorKind::UndefinedVariable(name_str.to_string()),
                    inst.span,
                )?;

                // Check mutability
                if !local.is_mut {
                    return Err(CompileError::new(
                        ErrorKind::AssignToImmutable(name_str.to_string()),
                        inst.span,
                    )
                    .with_label("variable declared as immutable here", local.span)
                    .with_help(format!(
                        "consider making `{}` mutable: `let mut {}`",
                        name_str, name_str
                    )));
                }

                let slot = local.slot;
                let ty = local.ty;

                // Analyze the value
                let value_result = self.analyze_inst(air, *value, ctx)?;

                // Assignment to a mutable variable resets its move state.
                // The variable is now valid again with a new value.
                ctx.moved_vars.remove(name);

                // Emit store instruction
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::Store {
                        slot,
                        value: value_result.air_ref,
                    },
                    ty: Type::Unit,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, Type::Unit))
            }

            InstData::Break => {
                // Validate that we're inside a loop
                if ctx.loop_depth == 0 {
                    return Err(CompileError::new(ErrorKind::BreakOutsideLoop, inst.span));
                }

                // Break has the never type - it diverges (doesn't produce a value)
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::Break,
                    ty: Type::Never,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, Type::Never))
            }

            InstData::Continue => {
                // Validate that we're inside a loop
                if ctx.loop_depth == 0 {
                    return Err(CompileError::new(ErrorKind::ContinueOutsideLoop, inst.span));
                }

                // Continue has the never type - it diverges (doesn't produce a value)
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::Continue,
                    ty: Type::Never,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, Type::Never))
            }

            InstData::FnDecl { .. } => {
                // Function declarations are handled at the top level
                Err(CompileError::new(
                    ErrorKind::InternalError(
                        "FnDecl should not appear in expression context".to_string(),
                    ),
                    inst.span,
                ))
            }

            InstData::Ret(inner) => {
                // Handle `return;` without expression (only valid for unit-returning functions)
                let inner_air_ref = if let Some(inner) = inner {
                    // Explicit return with value: type is already determined by HM inference
                    let inner_result = self.analyze_inst(air, *inner, ctx)?;
                    let inner_ty = inner_result.ty;

                    // Type check: returned value must match function's return type.
                    // We check for error types first to avoid cascading errors - if either
                    // type is already an error, we skip the mismatch check since there's
                    // already an error reported. Note: can_coerce_to handles inner_ty being
                    // Error (returns true), but we also need to handle return_type being Error.
                    if !ctx.return_type.is_error()
                        && !inner_ty.is_error()
                        && !inner_ty.can_coerce_to(&ctx.return_type)
                    {
                        return Err(CompileError::new(
                            ErrorKind::TypeMismatch {
                                expected: ctx.return_type.name().to_string(),
                                found: inner_ty.name().to_string(),
                            },
                            inst.span,
                        ));
                    }
                    Some(inner_result.air_ref)
                } else {
                    // `return;` without expression - only valid for unit-returning functions
                    if ctx.return_type != Type::Unit && !ctx.return_type.is_error() {
                        return Err(CompileError::new(
                            ErrorKind::TypeMismatch {
                                expected: ctx.return_type.name().to_string(),
                                found: "()".to_string(),
                            },
                            inst.span,
                        ));
                    }
                    None
                };

                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::Ret(inner_air_ref),
                    ty: Type::Never, // Return expressions have Never type (they diverge)
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, Type::Never))
            }

            InstData::Block { extra_start, len } => {
                // Get the instruction refs from extra data
                let inst_refs = self.rir.get_extra(*extra_start, *len);

                // Push a new scope for this block.
                // Variables declared in this block will be removed when the block ends.
                ctx.push_scope();

                // Process all instructions in the block
                // The last one is the final expression (the block's value)
                let mut statements = Vec::new();
                let mut last_result: Option<AnalysisResult> = None;
                let num_insts = inst_refs.len();
                for (i, &raw_ref) in inst_refs.iter().enumerate() {
                    let inst_ref = InstRef::from_raw(raw_ref);
                    let is_last = i == num_insts - 1;
                    let result = self.analyze_inst(air, inst_ref, ctx)?;

                    if is_last {
                        last_result = Some(result);
                    } else {
                        statements.push(result.air_ref);
                    }
                }

                // Check for unused variables before popping scope
                self.check_unused_locals_in_current_scope(ctx);

                // Pop scope to remove block-scoped variables.
                // Note: We don't restore next_slot, so slots are not reused.
                // This is a future optimization opportunity.
                ctx.pop_scope();

                // Handle empty blocks - they evaluate to Unit
                let last = match last_result {
                    Some(result) => result,
                    None => {
                        // Empty block: create a UnitConst
                        let air_ref = air.add_inst(AirInst {
                            data: AirInstData::UnitConst,
                            ty: Type::Unit,
                            span: inst.span,
                        });
                        AnalysisResult::new(air_ref, Type::Unit)
                    }
                };

                // Only create a Block instruction if there are statements;
                // otherwise just return the value directly (optimization)
                if statements.is_empty() {
                    Ok(last)
                } else {
                    // Block type comes from HM inference
                    let ty = last.ty;
                    // Encode statements into extra array
                    let stmt_u32s: Vec<u32> = statements.iter().map(|r| r.as_u32()).collect();
                    let stmts_start = air.add_extra(&stmt_u32s);
                    let stmts_len = statements.len() as u32;
                    let air_ref = air.add_inst(AirInst {
                        data: AirInstData::Block {
                            stmts_start,
                            stmts_len,
                            value: last.air_ref,
                        },
                        ty,
                        span: inst.span,
                    });
                    Ok(AnalysisResult::new(air_ref, ty))
                }
            }

            InstData::Call { name, args } => {
                // Look up the function
                let fn_name_str = self.interner.get(*name).to_string();
                let fn_info = self.functions.get(name).ok_or_compile_error(
                    ErrorKind::UndefinedFunction(fn_name_str.clone()),
                    inst.span,
                )?;

                // Check argument count
                if args.len() != fn_info.param_types.len() {
                    let expected = fn_info.param_types.len();
                    let found = args.len();
                    return Err(CompileError::new(
                        ErrorKind::WrongArgumentCount { expected, found },
                        inst.span,
                    ));
                }

                // Check for exclusive access violation: same variable passed to multiple inout params
                self.check_exclusive_access(args, inst.span)?;

                // Clone the data we need before mutable borrow
                let param_types = fn_info.param_types.clone();
                let param_modes = fn_info.param_modes.clone();
                let return_type = fn_info.return_type;

                // Check that call-site argument modes match function parameter modes
                for (i, (arg, expected_mode)) in args.iter().zip(param_modes.iter()).enumerate() {
                    match expected_mode {
                        RirParamMode::Inout => {
                            if arg.mode != RirArgMode::Inout {
                                return Err(CompileError::new(
                                    ErrorKind::InoutKeywordMissing,
                                    self.rir.get(args[i].value).span,
                                ));
                            }
                        }
                        RirParamMode::Borrow => {
                            if arg.mode != RirArgMode::Borrow {
                                return Err(CompileError::new(
                                    ErrorKind::BorrowKeywordMissing,
                                    self.rir.get(args[i].value).span,
                                ));
                            }
                        }
                        RirParamMode::Normal => {
                            // Normal params accept any mode (for now)
                        }
                    }
                }

                // Analyze arguments (move checking happens in analyze_inst for VarRef)
                let air_args = self.analyze_call_args(air, args, ctx)?;

                // Encode call args into extra array: each arg is (air_ref, mode)
                let args_len = air_args.len() as u32;
                let mut extra_data = Vec::with_capacity(air_args.len() * 2);
                for arg in &air_args {
                    extra_data.push(arg.value.as_u32());
                    extra_data.push(arg.mode.as_u32());
                }
                let args_start = air.add_extra(&extra_data);

                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::Call {
                        name: *name,
                        args_start,
                        args_len,
                    },
                    ty: return_type,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, return_type))
            }

            InstData::ParamRef { index: _, name } => {
                // Look up the parameter type and ABI slot from the params map
                let name_str = self.interner.get(*name);
                let param_info = ctx.params.get(name).ok_or_compile_error(
                    ErrorKind::UndefinedVariable(name_str.to_string()),
                    inst.span,
                )?;

                let ty = param_info.ty;

                // Use the ABI slot (not the RIR index) for proper struct parameter handling
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::Param {
                        index: param_info.abi_slot,
                    },
                    ty,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, ty))
            }

            InstData::StructDecl { .. } => {
                // Struct declarations are handled at the top level during collect_struct_definitions
                Err(CompileError::new(
                    ErrorKind::InternalError(
                        "StructDecl should not appear in expression context".to_string(),
                    ),
                    inst.span,
                ))
            }

            InstData::StructInit {
                type_name,
                fields: field_inits,
            } => {
                // Look up the struct type
                let type_name_str = self.interner.get(*type_name);
                let struct_id = *self.structs.get(type_name).ok_or_compile_error(
                    ErrorKind::UnknownType(type_name_str.to_string()),
                    inst.span,
                )?;

                // Clone struct def data before mutable borrow
                let struct_def = self.struct_defs[struct_id.0 as usize].clone();
                let struct_type = Type::Struct(struct_id);

                // Build a map from field name to struct field index for efficient lookup
                let field_index_map: std::collections::HashMap<&str, usize> = struct_def
                    .fields
                    .iter()
                    .enumerate()
                    .map(|(i, f)| (f.name.as_str(), i))
                    .collect();

                // Check for unknown or duplicate fields
                let mut seen_fields = std::collections::HashSet::new();
                for (init_field_name, _) in field_inits.iter() {
                    let init_name = self.interner.get(*init_field_name);

                    // Check if field exists in struct
                    if !field_index_map.contains_key(init_name) {
                        return Err(CompileError::new(
                            ErrorKind::UnknownField {
                                struct_name: struct_def.name.clone(),
                                field_name: init_name.to_string(),
                            },
                            inst.span,
                        ));
                    }

                    // Check for duplicate field
                    if !seen_fields.insert(init_name) {
                        return Err(CompileError::new(
                            ErrorKind::DuplicateField {
                                struct_name: struct_def.name.clone(),
                                field_name: init_name.to_string(),
                            },
                            inst.span,
                        ));
                    }
                }

                // Check that all fields are provided
                if field_inits.len() != struct_def.fields.len() {
                    // Find which fields are missing
                    let missing_fields: Vec<String> = struct_def
                        .fields
                        .iter()
                        .filter(|f| !seen_fields.contains(f.name.as_str()))
                        .map(|f| f.name.clone())
                        .collect();
                    return Err(CompileError::new(
                        ErrorKind::MissingFields(Box::new(MissingFieldsError {
                            struct_name: struct_def.name.clone(),
                            missing_fields,
                        })),
                        inst.span,
                    ));
                }

                // Analyze field values in SOURCE ORDER (left-to-right as written)
                // This is important for evaluation order semantics (spec 4.0:8)
                let mut analyzed_fields: Vec<Option<AirRef>> = vec![None; struct_def.fields.len()];
                // Track source order: which declaration index is evaluated at each position
                let mut source_order: Vec<usize> = Vec::with_capacity(field_inits.len());

                for (init_field_name, field_value) in field_inits.iter() {
                    let init_name = self.interner.get(*init_field_name);
                    let field_idx = field_index_map[init_name];

                    let field_result = self.analyze_inst(air, *field_value, ctx)?;
                    analyzed_fields[field_idx] = Some(field_result.air_ref);
                    source_order.push(field_idx);
                }

                // Collect field refs in DECLARATION ORDER for the AIR instruction
                // (storage layout matches declaration order)
                let field_refs: Vec<AirRef> = analyzed_fields
                    .into_iter()
                    .map(|opt| opt.expect("all fields should be initialized"))
                    .collect();

                // Encode into extra array: first field refs, then source order
                let fields_len = field_refs.len() as u32;
                let field_u32s: Vec<u32> = field_refs.iter().map(|r| r.as_u32()).collect();
                let fields_start = air.add_extra(&field_u32s);
                let source_order_u32s: Vec<u32> = source_order.iter().map(|&i| i as u32).collect();
                let source_order_start = air.add_extra(&source_order_u32s);

                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::StructInit {
                        struct_id,
                        fields_start,
                        fields_len,
                        source_order_start,
                    },
                    ty: struct_type,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, struct_type))
            }

            InstData::FieldGet { base, field } => {
                // Field access is a projection - it reads from the struct without consuming it.
                // We analyze the base in "projection mode" which checks for moves but doesn't
                // mark the variable as moved.
                let base_result = self.analyze_inst_for_projection(air, *base, ctx)?;
                let base_type = base_result.ty;

                let struct_id = match base_type {
                    Type::Struct(id) => id,
                    _ => {
                        return Err(CompileError::new(
                            ErrorKind::FieldAccessOnNonStruct {
                                found: base_type.name().to_string(),
                            },
                            inst.span,
                        ));
                    }
                };

                let struct_def = &self.struct_defs[struct_id.0 as usize];
                let field_name_str = self.interner.get(*field).to_string();

                let (field_index, struct_field) =
                    struct_def.find_field(&field_name_str).ok_or_compile_error(
                        ErrorKind::UnknownField {
                            struct_name: struct_def.name.clone(),
                            field_name: field_name_str.clone(),
                        },
                        inst.span,
                    )?;

                let field_type = struct_field.ty;

                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::FieldGet {
                        base: base_result.air_ref,
                        struct_id,
                        field_index: field_index as u32,
                    },
                    ty: field_type,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, field_type))
            }

            InstData::FieldSet { base, field, value } => {
                // For field assignment, we need to walk up the chain of field accesses
                // to find the root variable. We accumulate the slot offset as we go.
                //
                // For example, with `o.inner.value = 42`:
                // - base points to FieldGet { base: VarRef(o), field: inner }
                // - field is `value`
                //
                // We walk up to find VarRef(o), then compute:
                // - slot offset of `inner` within Outer
                // - slot offset of `value` within Inner
                // - total_slot = o.slot + offset(inner) + offset(value)

                // Walk up to find the root variable, collecting field symbols
                let mut current_base = *base;
                let mut field_symbols: Vec<Symbol> = Vec::new();

                // Result is either (Local, slot, type, is_mut, name) or (Param, abi_slot, type, mode, name)
                enum RootKind {
                    Local { slot: u32, is_mut: bool },
                    Param { abi_slot: u32, mode: RirParamMode },
                }

                let (var_name, root_kind, root_type, root_symbol) = loop {
                    let current_inst = self.rir.get(current_base);
                    match &current_inst.data {
                        InstData::VarRef { name } => {
                            let name_str = self.interner.get(*name);

                            // Check if this variable has been moved
                            if let Some(move_info) = ctx.moved_vars.get(name) {
                                return Err(CompileError::new(
                                    ErrorKind::UseAfterMove(name_str.to_string()),
                                    inst.span,
                                )
                                .with_label("value moved here", move_info.moved_at));
                            }

                            // First check if it's a parameter
                            if let Some(param_info) = ctx.params.get(name) {
                                break (
                                    name_str.to_string(),
                                    RootKind::Param {
                                        abi_slot: param_info.abi_slot,
                                        mode: param_info.mode,
                                    },
                                    param_info.ty,
                                    *name,
                                );
                            }

                            // Then check locals
                            let local = ctx.locals.get(name).ok_or_compile_error(
                                ErrorKind::UndefinedVariable(name_str.to_string()),
                                inst.span,
                            )?;

                            break (
                                name_str.to_string(),
                                RootKind::Local {
                                    slot: local.slot,
                                    is_mut: local.is_mut,
                                },
                                local.ty,
                                *name,
                            );
                        }
                        InstData::ParamRef { name, .. } => {
                            let name_str = self.interner.get(*name);
                            let param_info = ctx.params.get(name).ok_or_compile_error(
                                ErrorKind::UndefinedVariable(name_str.to_string()),
                                inst.span,
                            )?;

                            // Check if this parameter has been moved
                            if let Some(move_info) = ctx.moved_vars.get(name) {
                                return Err(CompileError::new(
                                    ErrorKind::UseAfterMove(name_str.to_string()),
                                    inst.span,
                                )
                                .with_label("value moved here", move_info.moved_at));
                            }

                            break (
                                name_str.to_string(),
                                RootKind::Param {
                                    abi_slot: param_info.abi_slot,
                                    mode: param_info.mode,
                                },
                                param_info.ty,
                                *name,
                            );
                        }
                        InstData::FieldGet {
                            base: inner_base,
                            field: inner_field,
                        } => {
                            field_symbols.push(*inner_field);
                            current_base = *inner_base;
                        }
                        _ => {
                            return Err(CompileError::new(
                                ErrorKind::InvalidAssignmentTarget,
                                inst.span,
                            ));
                        }
                    }
                };

                // Check mutability based on root kind
                let root_slot = match root_kind {
                    RootKind::Local { slot, is_mut } => {
                        if !is_mut {
                            return Err(CompileError::new(
                                ErrorKind::AssignToImmutable(var_name),
                                inst.span,
                            ));
                        }
                        slot
                    }
                    RootKind::Param { abi_slot, mode } => {
                        match mode {
                            RirParamMode::Normal => {
                                // Non-inout parameters are immutable - cannot modify their fields
                                return Err(CompileError::new(
                                    ErrorKind::AssignToImmutable(var_name.clone()),
                                    inst.span,
                                )
                                .with_help(format!(
                                    "consider making parameter `{}` inout: `inout {}: {}`",
                                    var_name,
                                    var_name,
                                    root_type.name()
                                )));
                            }
                            RirParamMode::Inout => {
                                // Inout parameters can be mutated - that's their purpose
                            }
                            RirParamMode::Borrow => {
                                // Borrow parameters CANNOT be mutated
                                return Err(CompileError::new(
                                    ErrorKind::MutateBorrowedValue { variable: var_name },
                                    inst.span,
                                ));
                            }
                        }
                        abi_slot
                    }
                };

                // Suppress unused variable warning
                let _ = root_symbol;

                // Now resolve the field chain from root to the immediate base.
                // field_symbols is in reverse order (innermost first), so iterate in reverse
                // to process from root to leaf without allocating a reversed copy.

                // Walk through the field chain to compute the slot offset and find the base struct
                let mut current_type = root_type;
                let mut slot_offset: u32 = 0;

                for field_sym in field_symbols.iter().rev() {
                    let struct_id = match current_type {
                        Type::Struct(id) => id,
                        _ => {
                            return Err(CompileError::new(
                                ErrorKind::FieldAccessOnNonStruct {
                                    found: current_type.name().to_string(),
                                },
                                inst.span,
                            ));
                        }
                    };

                    let struct_def = &self.struct_defs[struct_id.0 as usize];
                    let field_name_str = self.interner.get(*field_sym).to_string();

                    let (field_index, struct_field) =
                        struct_def.find_field(&field_name_str).ok_or_compile_error(
                            ErrorKind::UnknownField {
                                struct_name: struct_def.name.clone(),
                                field_name: field_name_str.clone(),
                            },
                            inst.span,
                        )?;

                    slot_offset += self.field_slot_offset(struct_id, field_index);
                    current_type = struct_field.ty;
                }

                // Now handle the final field being assigned
                let struct_id = match current_type {
                    Type::Struct(id) => id,
                    _ => {
                        return Err(CompileError::new(
                            ErrorKind::FieldAccessOnNonStruct {
                                found: current_type.name().to_string(),
                            },
                            inst.span,
                        ));
                    }
                };

                let struct_def = &self.struct_defs[struct_id.0 as usize];
                let field_name_str = self.interner.get(*field).to_string();

                let (field_index, _struct_field) =
                    struct_def.find_field(&field_name_str).ok_or_compile_error(
                        ErrorKind::UnknownField {
                            struct_name: struct_def.name.clone(),
                            field_name: field_name_str.clone(),
                        },
                        inst.span,
                    )?;

                // Analyze the value with the expected field type
                let value_result = self.analyze_inst(air, *value, ctx)?;

                // Emit the appropriate instruction based on whether root is a local or param
                let air_ref = match root_kind {
                    RootKind::Local { slot, .. } => {
                        // Compute the slot of the containing struct (the immediate base).
                        // Codegen will add field_index to get the actual field slot.
                        let base_slot = slot + slot_offset;
                        air.add_inst(AirInst {
                            data: AirInstData::FieldSet {
                                slot: base_slot,
                                struct_id,
                                field_index: field_index as u32,
                                value: value_result.air_ref,
                            },
                            ty: Type::Unit,
                            span: inst.span,
                        })
                    }
                    RootKind::Param { abi_slot, .. } => {
                        // For inout parameters, emit ParamFieldSet.
                        // We've already verified is_inout is true above.
                        air.add_inst(AirInst {
                            data: AirInstData::ParamFieldSet {
                                param_slot: abi_slot,
                                inner_offset: slot_offset,
                                struct_id,
                                field_index: field_index as u32,
                                value: value_result.air_ref,
                            },
                            ty: Type::Unit,
                            span: inst.span,
                        })
                    }
                };
                Ok(AnalysisResult::new(air_ref, Type::Unit))
            }

            InstData::Intrinsic { name, args } => {
                let intrinsic_name = self.interner.get(*name).to_string();

                match intrinsic_name.as_str() {
                    "dbg" => {
                        // @dbg expects exactly one argument
                        if args.len() != 1 {
                            return Err(CompileError::new(
                                ErrorKind::IntrinsicWrongArgCount {
                                    name: intrinsic_name,
                                    expected: 1,
                                    found: args.len(),
                                },
                                inst.span,
                            ));
                        }

                        // Synthesize the argument type in a single traversal
                        let arg_result = self.analyze_inst(air, args[0], ctx)?;
                        let arg_type = arg_result.ty;

                        // Check that argument is a supported type (integer, bool, or string)
                        let is_supported = arg_type.is_integer()
                            || arg_type == Type::Bool
                            || arg_type == Type::String;
                        if !is_supported {
                            return Err(CompileError::new(
                                ErrorKind::IntrinsicTypeMismatch(Box::new(
                                    IntrinsicTypeMismatchError {
                                        name: intrinsic_name,
                                        expected: "integer, bool, or string".to_string(),
                                        found: arg_type.name().to_string(),
                                    },
                                )),
                                inst.span,
                            ));
                        }

                        // Encode args into extra array
                        let args_start = air.add_extra(&[arg_result.air_ref.as_u32()]);
                        let air_ref = air.add_inst(AirInst {
                            data: AirInstData::Intrinsic {
                                name: intrinsic_name,
                                args_start,
                                args_len: 1,
                            },
                            ty: Type::Unit,
                            span: inst.span,
                        });
                        Ok(AnalysisResult::new(air_ref, Type::Unit))
                    }
                    "intCast" => {
                        // @intCast expects exactly one argument
                        if args.len() != 1 {
                            return Err(CompileError::new(
                                ErrorKind::IntrinsicWrongArgCount {
                                    name: intrinsic_name,
                                    expected: 1,
                                    found: args.len(),
                                },
                                inst.span,
                            ));
                        }

                        // Analyze the argument
                        let arg_result = self.analyze_inst(air, args[0], ctx)?;
                        let from_ty = arg_result.ty;

                        // Argument must be an integer type
                        if !from_ty.is_integer() {
                            return Err(CompileError::new(
                                ErrorKind::IntrinsicTypeMismatch(Box::new(
                                    IntrinsicTypeMismatchError {
                                        name: intrinsic_name,
                                        expected: "integer".to_string(),
                                        found: from_ty.name().to_string(),
                                    },
                                )),
                                inst.span,
                            ));
                        }

                        // Get the target type from HM inference
                        let target_ty = match ctx.resolved_types.get(&inst_ref).copied() {
                            Some(ty) if ty.is_integer() => ty,
                            Some(Type::Error) => {
                                // Error already reported during type inference
                                return Err(CompileError::new(
                                    ErrorKind::TypeAnnotationRequired,
                                    inst.span,
                                ));
                            }
                            Some(ty) => {
                                return Err(CompileError::new(
                                    ErrorKind::IntrinsicTypeMismatch(Box::new(
                                        IntrinsicTypeMismatchError {
                                            name: intrinsic_name,
                                            expected: "integer".to_string(),
                                            found: ty.name().to_string(),
                                        },
                                    )),
                                    inst.span,
                                ));
                            }
                            None => {
                                // Type inference couldn't determine the target type
                                return Err(CompileError::new(
                                    ErrorKind::TypeAnnotationRequired,
                                    inst.span,
                                ));
                            }
                        };

                        let air_ref = air.add_inst(AirInst {
                            data: AirInstData::IntCast {
                                value: arg_result.air_ref,
                                from_ty,
                            },
                            ty: target_ty,
                            span: inst.span,
                        });
                        Ok(AnalysisResult::new(air_ref, target_ty))
                    }
                    "test_preview_gate" => {
                        // @test_preview_gate() - no-op intrinsic gated by test_infra preview feature.
                        // Used to test that the preview feature gating mechanism works correctly.
                        self.require_preview(
                            PreviewFeature::TestInfra,
                            "@test_preview_gate() intrinsic",
                            inst.span,
                        )?;

                        // Takes no arguments
                        if !args.is_empty() {
                            return Err(CompileError::new(
                                ErrorKind::IntrinsicWrongArgCount {
                                    name: intrinsic_name,
                                    expected: 0,
                                    found: args.len(),
                                },
                                inst.span,
                            ));
                        }

                        // No-op: just return a unit constant
                        let air_ref = air.add_inst(AirInst {
                            data: AirInstData::UnitConst,
                            ty: Type::Unit,
                            span: inst.span,
                        });
                        Ok(AnalysisResult::new(air_ref, Type::Unit))
                    }
                    _ => Err(CompileError::new(
                        ErrorKind::UnknownIntrinsic(intrinsic_name),
                        inst.span,
                    )),
                }
            }

            InstData::TypeIntrinsic { name, type_arg } => {
                let intrinsic_name = self.interner.get(*name).to_string();
                let ty = self.resolve_type(*type_arg, inst.span)?;

                let value: u64 = match intrinsic_name.as_str() {
                    "size_of" => {
                        // Calculate size in bytes (slot count * 8)
                        let slot_count = self.abi_slot_count(ty);
                        (slot_count * 8) as u64
                    }
                    "align_of" => {
                        // Zero-sized types have 1-byte alignment, others have 8-byte
                        let slot_count = self.abi_slot_count(ty);
                        if slot_count == 0 { 1u64 } else { 8u64 }
                    }
                    _ => {
                        return Err(CompileError::new(
                            ErrorKind::UnknownIntrinsic(intrinsic_name),
                            inst.span,
                        ));
                    }
                };

                // Emit a constant with the computed value
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::Const(value),
                    ty: Type::I32,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, Type::I32))
            }

            InstData::ArrayInit { elements } => {
                // Get the array type from HM inference
                let array_type_id = match ctx.resolved_types.get(&inst_ref).copied() {
                    Some(Type::Array(id)) => id,
                    Some(Type::Error) => {
                        // Error already reported during type inference
                        return Err(CompileError::new(
                            ErrorKind::TypeAnnotationRequired,
                            inst.span,
                        ));
                    }
                    None => {
                        // HM didn't resolve the type - this is an internal error
                        return Err(CompileError::new(
                            ErrorKind::InternalError(
                                "array type inference failed: type not resolved".to_string(),
                            ),
                            inst.span,
                        ));
                    }
                    Some(other) => {
                        // HM resolved to an unexpected type - this is an internal error
                        return Err(CompileError::new(
                            ErrorKind::InternalError(format!(
                                "array type inference failed: expected array type, got {:?}",
                                other
                            )),
                            inst.span,
                        ));
                    }
                };

                // Analyze all elements
                let mut element_refs = Vec::with_capacity(elements.len());
                for elem in elements.iter() {
                    let elem_result = self.analyze_inst(air, *elem, ctx)?;
                    element_refs.push(elem_result.air_ref);
                }

                // Encode elements into extra array
                let elems_len = element_refs.len() as u32;
                let elem_u32s: Vec<u32> = element_refs.iter().map(|r| r.as_u32()).collect();
                let elems_start = air.add_extra(&elem_u32s);

                let array_type = Type::Array(array_type_id);
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::ArrayInit {
                        array_type_id,
                        elems_start,
                        elems_len,
                    },
                    ty: array_type,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, array_type))
            }

            InstData::IndexGet { base, index } => {
                // Array indexing is a projection - it reads from the array without consuming it.
                // Like field access, we analyze the base in projection mode.
                let base_result = self.analyze_inst_for_projection(air, *base, ctx)?;
                let base_type = base_result.ty;

                let array_type_id = match base_type {
                    Type::Array(id) => id,
                    _ => {
                        return Err(CompileError::new(
                            ErrorKind::IndexOnNonArray {
                                found: base_type.name().to_string(),
                            },
                            inst.span,
                        ));
                    }
                };

                // Index must be an unsigned integer
                let index_result = self.analyze_inst(air, *index, ctx)?;
                if !index_result.ty.is_unsigned() && !index_result.ty.is_error() {
                    return Err(CompileError::new(
                        ErrorKind::TypeMismatch {
                            expected: "unsigned integer type".to_string(),
                            found: index_result.ty.name().to_string(),
                        },
                        self.rir.get(*index).span,
                    ));
                }

                let array_def = &self.array_type_defs[array_type_id.0 as usize];
                let element_type = array_def.element_type;
                let array_length = array_def.length;

                // Compile-time bounds check for constant indices
                if let Some(const_index) = self.try_get_const_index(*index) {
                    if const_index < 0 || const_index as u64 >= array_length {
                        return Err(CompileError::new(
                            ErrorKind::IndexOutOfBounds {
                                index: const_index,
                                length: array_length,
                            },
                            self.rir.get(*index).span,
                        ));
                    }
                }

                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::IndexGet {
                        base: base_result.air_ref,
                        array_type_id,
                        index: index_result.air_ref,
                    },
                    ty: element_type,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, element_type))
            }

            InstData::IndexSet { base, index, value } => {
                // For index assignment, we need the base to be a local variable or parameter
                let base_inst = self.rir.get(*base);

                // Root kind to distinguish locals from params
                enum IndexSetRootKind {
                    Local { slot: u32, is_mut: bool },
                    Param { abi_slot: u32, mode: RirParamMode },
                }

                let (var_name, root_kind, base_type) = match &base_inst.data {
                    InstData::VarRef { name } => {
                        let name_str = self.interner.get(*name);

                        // Check if this variable has been moved
                        if let Some(move_info) = ctx.moved_vars.get(name) {
                            return Err(CompileError::new(
                                ErrorKind::UseAfterMove(name_str.to_string()),
                                inst.span,
                            )
                            .with_label("value moved here", move_info.moved_at));
                        }

                        // First check if it's a parameter (like FieldSet does)
                        if let Some(param_info) = ctx.params.get(name) {
                            (
                                name_str.to_string(),
                                IndexSetRootKind::Param {
                                    abi_slot: param_info.abi_slot,
                                    mode: param_info.mode,
                                },
                                param_info.ty,
                            )
                        } else {
                            // Then check locals
                            let local = ctx.locals.get(name).ok_or_compile_error(
                                ErrorKind::UndefinedVariable(name_str.to_string()),
                                inst.span,
                            )?;

                            (
                                name_str.to_string(),
                                IndexSetRootKind::Local {
                                    slot: local.slot,
                                    is_mut: local.is_mut,
                                },
                                local.ty,
                            )
                        }
                    }
                    InstData::ParamRef { name, .. } => {
                        let name_str = self.interner.get(*name);
                        let param_info = ctx.params.get(name).ok_or_compile_error(
                            ErrorKind::UndefinedVariable(name_str.to_string()),
                            inst.span,
                        )?;

                        // Check if this parameter has been moved
                        if let Some(move_info) = ctx.moved_vars.get(name) {
                            return Err(CompileError::new(
                                ErrorKind::UseAfterMove(name_str.to_string()),
                                inst.span,
                            )
                            .with_label("value moved here", move_info.moved_at));
                        }

                        (
                            name_str.to_string(),
                            IndexSetRootKind::Param {
                                abi_slot: param_info.abi_slot,
                                mode: param_info.mode,
                            },
                            param_info.ty,
                        )
                    }
                    _ => {
                        return Err(CompileError::new(
                            ErrorKind::InvalidAssignmentTarget,
                            inst.span,
                        ));
                    }
                };

                // Check mutability based on root kind
                let (is_inout_param, slot) = match root_kind {
                    IndexSetRootKind::Local { slot, is_mut } => {
                        if !is_mut {
                            return Err(CompileError::new(
                                ErrorKind::AssignToImmutable(var_name),
                                inst.span,
                            ));
                        }
                        (false, slot)
                    }
                    IndexSetRootKind::Param { abi_slot, mode } => {
                        let is_inout = match mode {
                            RirParamMode::Normal => {
                                // Normal (owned) parameters can be mutated
                                false
                            }
                            RirParamMode::Inout => {
                                // Inout parameters can be mutated
                                true
                            }
                            RirParamMode::Borrow => {
                                // Borrow parameters CANNOT be mutated
                                return Err(CompileError::new(
                                    ErrorKind::MutateBorrowedValue { variable: var_name },
                                    inst.span,
                                ));
                            }
                        };
                        (is_inout, abi_slot)
                    }
                };

                let array_type_id = match base_type {
                    Type::Array(id) => id,
                    _ => {
                        return Err(CompileError::new(
                            ErrorKind::IndexOnNonArray {
                                found: base_type.name().to_string(),
                            },
                            inst.span,
                        ));
                    }
                };

                // Index must be an unsigned integer
                let index_result = self.analyze_inst(air, *index, ctx)?;
                if !index_result.ty.is_unsigned() && !index_result.ty.is_error() {
                    return Err(CompileError::new(
                        ErrorKind::TypeMismatch {
                            expected: "unsigned integer type".to_string(),
                            found: index_result.ty.name().to_string(),
                        },
                        self.rir.get(*index).span,
                    ));
                }

                let array_def = &self.array_type_defs[array_type_id.0 as usize];
                let element_type = array_def.element_type;
                let array_length = array_def.length;

                // Compile-time bounds check for constant indices
                if let Some(const_index) = self.try_get_const_index(*index) {
                    if const_index < 0 || const_index as u64 >= array_length {
                        return Err(CompileError::new(
                            ErrorKind::IndexOutOfBounds {
                                index: const_index,
                                length: array_length,
                            },
                            self.rir.get(*index).span,
                        ));
                    }
                }

                // Analyze the value with the expected element type
                let value_result = self.analyze_inst(air, *value, ctx)?;

                // Emit appropriate instruction based on whether this is an inout parameter
                let air_ref = if is_inout_param {
                    air.add_inst(AirInst {
                        data: AirInstData::ParamIndexSet {
                            param_slot: slot,
                            array_type_id,
                            index: index_result.air_ref,
                            value: value_result.air_ref,
                        },
                        ty: Type::Unit,
                        span: inst.span,
                    })
                } else {
                    air.add_inst(AirInst {
                        data: AirInstData::IndexSet {
                            slot,
                            array_type_id,
                            index: index_result.air_ref,
                            value: value_result.air_ref,
                        },
                        ty: Type::Unit,
                        span: inst.span,
                    })
                };
                Ok(AnalysisResult::new(air_ref, Type::Unit))
            }

            // Enum declarations are processed during collection phase, skip here
            InstData::EnumDecl { .. } => {
                // Return Unit - enum declarations don't produce a value
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::UnitConst,
                    ty: Type::Unit,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, Type::Unit))
            }

            // Enum variant expression (e.g., Color::Red)
            InstData::EnumVariant { type_name, variant } => {
                // Look up the enum type
                let enum_id = self.enums.get(type_name).ok_or_compile_error(
                    ErrorKind::UnknownEnumType(self.interner.get(*type_name).to_string()),
                    inst.span,
                )?;
                let enum_def = &self.enum_defs[enum_id.0 as usize];

                // Find the variant index
                let variant_name = self.interner.get(*variant);
                let variant_index = enum_def.find_variant(variant_name).ok_or_compile_error(
                    ErrorKind::UnknownVariant {
                        enum_name: enum_def.name.clone(),
                        variant_name: variant_name.to_string(),
                    },
                    inst.span,
                )?;

                let ty = Type::Enum(*enum_id);

                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::EnumVariant {
                        enum_id: *enum_id,
                        variant_index: variant_index as u32,
                    },
                    ty,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, ty))
            }

            // Impl block declarations are processed during collection phase, skip here
            InstData::ImplDecl { .. } => {
                // Return Unit - impl blocks don't produce a value
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::UnitConst,
                    ty: Type::Unit,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, Type::Unit))
            }

            // Drop fn declarations are processed during collection phase, skip here
            InstData::DropFnDecl { .. } => {
                // Return Unit - drop fn declarations don't produce a value
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::UnitConst,
                    ty: Type::Unit,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, Type::Unit))
            }

            // Method call: receiver.method(args)
            InstData::MethodCall {
                receiver,
                method,
                args,
            } => {
                // For String borrow methods, we need to extract the root variable before
                // analyzing the receiver so we can "unmove" it afterwards. String query
                // methods (len, capacity, is_empty) use `borrow self` semantics - they
                // don't consume the String.
                let receiver_var = self.extract_root_variable(*receiver);

                // Get the method name as a string before analyzing receiver
                let method_name_str = self.interner.get(*method).to_string();

                // Check if this is a String mutation method that needs storage location
                let is_string_mutation_method = matches!(
                    method_name_str.as_str(),
                    "push_str" | "push" | "clear" | "reserve"
                );

                // For String mutation methods, we need to get the storage location
                // BEFORE analyzing the receiver (which may mark it as moved)
                let receiver_storage = if is_string_mutation_method {
                    self.get_string_receiver_storage(*receiver, ctx, inst.span)?
                } else {
                    None
                };

                // Analyze the receiver expression
                let receiver_result = self.analyze_inst(air, *receiver, ctx)?;
                let receiver_type = receiver_result.ty;

                // Handle String methods specially
                if receiver_type == Type::String {
                    // String query methods (len, capacity, is_empty) use borrow semantics.
                    // The receiver was marked as moved during analysis, but we need to
                    // "unmove" it since it's actually being borrowed, not consumed.
                    let is_borrow_method = matches!(
                        method_name_str.as_str(),
                        "len" | "capacity" | "is_empty" | "clone"
                    );
                    if is_borrow_method {
                        if let Some(var_symbol) = receiver_var {
                            ctx.moved_vars.remove(&var_symbol);
                        }
                    }

                    // String mutation methods need special handling
                    if is_string_mutation_method {
                        // Mutation methods use inout semantics - variable remains valid after
                        if let Some(var_symbol) = receiver_var {
                            ctx.moved_vars.remove(&var_symbol);
                        }
                        return self.analyze_string_mutation_method(
                            air,
                            ctx,
                            &method_name_str,
                            receiver_result,
                            receiver_storage,
                            args,
                            inst.span,
                        );
                    }

                    return self.analyze_string_method(
                        air,
                        ctx,
                        &method_name_str,
                        receiver_result,
                        args,
                        inst.span,
                    );
                }

                // Check that receiver is a struct type
                let struct_id = match receiver_type {
                    Type::Struct(id) => id,
                    _ => {
                        return Err(CompileError::new(
                            ErrorKind::MethodCallOnNonStruct {
                                found: receiver_type.name().to_string(),
                                method_name: method_name_str,
                            },
                            inst.span,
                        ));
                    }
                };

                // Look up the struct name by its ID
                let struct_def = &self.struct_defs[struct_id.0 as usize];
                let struct_name_str = struct_def.name.clone();

                // Find the struct name symbol for method lookup
                let struct_name_sym = self.interner.intern(&struct_name_str);

                // Look up the method
                let method_key = (struct_name_sym, *method);
                let method_info = self.methods.get(&method_key).ok_or_compile_error(
                    ErrorKind::UndefinedMethod {
                        type_name: struct_name_str.clone(),
                        method_name: method_name_str.clone(),
                    },
                    inst.span,
                )?;

                // Check that this is a method (has self), not an associated function
                if !method_info.has_self {
                    return Err(CompileError::new(
                        ErrorKind::AssocFnCalledAsMethod {
                            type_name: struct_name_str,
                            function_name: method_name_str,
                        },
                        inst.span,
                    ));
                }

                // Check argument count (method_info.param_types excludes self)
                if args.len() != method_info.param_types.len() {
                    return Err(CompileError::new(
                        ErrorKind::WrongArgumentCount {
                            expected: method_info.param_types.len(),
                            found: args.len(),
                        },
                        inst.span,
                    ));
                }

                // Check for exclusive access violation in method args
                self.check_exclusive_access(args, inst.span)?;

                // Clone data needed before mutable borrow
                let return_type = method_info.return_type;

                // Analyze arguments - receiver first, then remaining args
                let mut air_args = vec![AirCallArg {
                    value: receiver_result.air_ref,
                    mode: AirArgMode::Normal, // receiver is not inout
                }];
                air_args.extend(self.analyze_call_args(air, args, ctx)?);

                // Generate a method call name: Type.method (intern for AIR)
                let call_name = format!("{}.{}", struct_name_str, method_name_str);
                let call_name_sym = self.interner.intern(&call_name);

                // Encode call args into extra array
                let args_len = air_args.len() as u32;
                let mut extra_data = Vec::with_capacity(air_args.len() * 2);
                for arg in &air_args {
                    extra_data.push(arg.value.as_u32());
                    extra_data.push(arg.mode.as_u32());
                }
                let args_start = air.add_extra(&extra_data);

                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::Call {
                        name: call_name_sym,
                        args_start,
                        args_len,
                    },
                    ty: return_type,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, return_type))
            }

            // Associated function call: Type::function(args)
            InstData::AssocFnCall {
                type_name,
                function,
                args,
            } => {
                // Get the type and function names for error messages
                let type_name_str = self.interner.get(*type_name).to_string();
                let function_name_str = self.interner.get(*function).to_string();

                // Check if this is a built-in type with associated functions
                let well_known = self.interner.well_known();

                // Handle String::new() and String::with_capacity()
                if *type_name == well_known.string {
                    return self.analyze_string_assoc_fn(
                        air,
                        ctx,
                        &function_name_str,
                        args,
                        inst.span,
                    );
                }

                // Check that the type exists and is a struct
                let _struct_id = self.structs.get(type_name).ok_or_compile_error(
                    ErrorKind::UnknownType(type_name_str.clone()),
                    inst.span,
                )?;

                // Look up the function
                let method_key = (*type_name, *function);
                let method_info = self.methods.get(&method_key).ok_or_compile_error(
                    ErrorKind::UndefinedAssocFn {
                        type_name: type_name_str.clone(),
                        function_name: function_name_str.clone(),
                    },
                    inst.span,
                )?;

                // Check that this is an associated function (no self), not a method
                if method_info.has_self {
                    return Err(CompileError::new(
                        ErrorKind::MethodCalledAsAssocFn {
                            type_name: type_name_str,
                            method_name: function_name_str,
                        },
                        inst.span,
                    ));
                }

                // Check argument count
                if args.len() != method_info.param_types.len() {
                    return Err(CompileError::new(
                        ErrorKind::WrongArgumentCount {
                            expected: method_info.param_types.len(),
                            found: args.len(),
                        },
                        inst.span,
                    ));
                }

                // Check for exclusive access violation in assoc fn args
                self.check_exclusive_access(args, inst.span)?;

                // Clone data needed before mutable borrow
                let return_type = method_info.return_type;

                // Analyze arguments
                let air_args = self.analyze_call_args(air, args, ctx)?;

                // Generate a function call name: Type::function (intern for AIR)
                let call_name = format!("{}::{}", type_name_str, function_name_str);
                let call_name_sym = self.interner.intern(&call_name);

                // Encode call args into extra array
                let args_len = air_args.len() as u32;
                let mut extra_data = Vec::with_capacity(air_args.len() * 2);
                for arg in &air_args {
                    extra_data.push(arg.value.as_u32());
                    extra_data.push(arg.mode.as_u32());
                }
                let args_start = air.add_extra(&extra_data);

                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::Call {
                        name: call_name_sym,
                        args_start,
                        args_len,
                    },
                    ty: return_type,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, return_type))
            }
        }
    }

    /// Convert RIR argument mode to AIR argument mode.
    fn convert_arg_mode(mode: RirArgMode) -> AirArgMode {
        match mode {
            RirArgMode::Normal => AirArgMode::Normal,
            RirArgMode::Inout => AirArgMode::Inout,
            RirArgMode::Borrow => AirArgMode::Borrow,
        }
    }

    /// Resolve a type symbol to a Type.
    ///
    /// Uses symbol comparison instead of string comparison for efficiency.
    /// Handles array types with the syntax "[T; N]".
    fn resolve_type(&mut self, type_sym: Symbol, span: Span) -> CompileResult<Type> {
        let well_known = self.interner.well_known();

        if type_sym == well_known.i8 {
            Ok(Type::I8)
        } else if type_sym == well_known.i16 {
            Ok(Type::I16)
        } else if type_sym == well_known.i32 {
            Ok(Type::I32)
        } else if type_sym == well_known.i64 {
            Ok(Type::I64)
        } else if type_sym == well_known.u8 {
            Ok(Type::U8)
        } else if type_sym == well_known.u16 {
            Ok(Type::U16)
        } else if type_sym == well_known.u32 {
            Ok(Type::U32)
        } else if type_sym == well_known.u64 {
            Ok(Type::U64)
        } else if type_sym == well_known.bool {
            Ok(Type::Bool)
        } else if type_sym == well_known.unit {
            Ok(Type::Unit)
        } else if type_sym == well_known.never {
            Ok(Type::Never)
        } else if type_sym == well_known.string {
            Ok(Type::String)
        } else if let Some(&struct_id) = self.structs.get(&type_sym) {
            Ok(Type::Struct(struct_id))
        } else if let Some(&enum_id) = self.enums.get(&type_sym) {
            Ok(Type::Enum(enum_id))
        } else {
            // Check for array type syntax: [T; N]
            let type_name = self.interner.get(type_sym);
            if let Some((element_type, length)) = parse_array_type_syntax(type_name) {
                // Resolve the element type first
                let element_sym = self.interner.intern(&element_type);
                let element_ty = self.resolve_type(element_sym, span)?;
                // Get or create the array type
                let array_type_id = self.get_or_create_array_type(element_ty, length);
                Ok(Type::Array(array_type_id))
            } else {
                Err(CompileError::new(
                    ErrorKind::UnknownType(type_name.to_string()),
                    span,
                ))
            }
        }
    }

    /// Get or create an array type for the given element type and length.
    fn get_or_create_array_type(&mut self, element_type: Type, length: u64) -> ArrayTypeId {
        let key = (element_type, length);
        if let Some(&id) = self.array_types.get(&key) {
            return id;
        }

        let id = ArrayTypeId(self.array_type_defs.len() as u32);
        self.array_type_defs.push(ArrayTypeDef {
            element_type,
            length,
        });
        self.array_types.insert(key, id);
        id
    }

    /// Get the number of ABI slots required for a type.
    /// Scalar types (i8, i16, i32, i64, u8, u16, u32, u64, bool) use 1 slot,
    /// structs use 1 slot per field, arrays use 1 slot per element.
    /// Zero-sized types (unit, never, empty structs, zero-length arrays) use 0 slots.
    fn abi_slot_count(&self, ty: Type) -> u32 {
        match ty {
            Type::I8
            | Type::I16
            | Type::I32
            | Type::I64
            | Type::U8
            | Type::U16
            | Type::U32
            | Type::U64
            | Type::Bool
            | Type::Error => 1,
            // Zero-sized types use 0 slots
            Type::Unit | Type::Never => 0,
            // Enums are represented as their discriminant type (a scalar), so 1 slot
            Type::Enum(_) => 1,
            // Strings are (ptr + len + cap), so 3 slots
            Type::String => 3,
            Type::Struct(struct_id) => {
                // Sum the slot counts of all fields (handles arrays and nested structs)
                // Empty structs naturally get 0 slots here
                let struct_def = &self.struct_defs[struct_id.0 as usize];
                struct_def
                    .fields
                    .iter()
                    .map(|f| self.abi_slot_count(f.ty))
                    .sum()
            }
            Type::Array(array_type_id) => {
                // Zero-length arrays naturally get 0 slots (0 * element_slots)
                let array_def = &self.array_type_defs[array_type_id.0 as usize];
                let element_slots = self.abi_slot_count(array_def.element_type);
                element_slots * array_def.length as u32
            }
        }
    }

    /// Get the slot offset of a field within a struct.
    /// Returns the number of slots before the field starts.
    fn field_slot_offset(&self, struct_id: StructId, field_index: usize) -> u32 {
        let struct_def = &self.struct_defs[struct_id.0 as usize];
        struct_def.fields[..field_index]
            .iter()
            .map(|f| self.abi_slot_count(f.ty))
            .sum()
    }

    /// Analyze a binary arithmetic operator (+, -, *, /, %).
    ///
    /// Follows Rust's type inference rules:
    /// Types are determined by HM inference. Both operands must have the same type.
    fn analyze_binary_arith<F>(
        &mut self,
        air: &mut Air,
        lhs: InstRef,
        rhs: InstRef,
        make_data: F,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult>
    where
        F: FnOnce(AirRef, AirRef) -> AirInstData,
    {
        let lhs_result = self.analyze_inst(air, lhs, ctx)?;
        let rhs_result = self.analyze_inst(air, rhs, ctx)?;

        // Verify the type is integer (HM should have enforced this, but check anyway)
        if !lhs_result.ty.is_integer() && !lhs_result.ty.is_error() && !lhs_result.ty.is_never() {
            return Err(CompileError::new(
                ErrorKind::TypeMismatch {
                    expected: "integer type".to_string(),
                    found: lhs_result.ty.name().to_string(),
                },
                span,
            ));
        }

        let air_ref = air.add_inst(AirInst {
            data: make_data(lhs_result.air_ref, rhs_result.air_ref),
            ty: lhs_result.ty,
            span,
        });
        Ok(AnalysisResult::new(air_ref, lhs_result.ty))
    }

    /// Analyze a comparison operator.
    ///
    /// Types are determined by HM inference. Both operands must have the same type.
    ///
    /// For equality operators (`==`, `!=`), both integers and booleans are allowed.
    /// For ordering operators (`<`, `>`, `<=`, `>=`), only integers are allowed.
    fn analyze_comparison<F>(
        &mut self,
        air: &mut Air,
        lhs: InstRef,
        rhs: InstRef,
        allow_bool: bool,
        make_data: F,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult>
    where
        F: FnOnce(AirRef, AirRef) -> AirInstData,
    {
        // Check for chained comparisons (e.g., `a < b < c`)
        // Since the parser is left-associative, `a < b < c` parses as `(a < b) < c`,
        // so we only need to check if the LHS is a comparison.
        if self.is_comparison(lhs) {
            return Err(CompileError::new(ErrorKind::ChainedComparison, span)
                .with_help("use `&&` to combine comparisons: `a < b && b < c`"));
        }

        // Comparisons read values without consuming them (like projections).
        // This matches Rust's PartialEq trait which takes references.
        let lhs_result = self.analyze_inst_for_projection(air, lhs, ctx)?;
        let rhs_result = self.analyze_inst_for_projection(air, rhs, ctx)?;
        let lhs_type = lhs_result.ty;

        // Propagate Never/Error without additional type errors
        if lhs_type.is_never() || lhs_type.is_error() {
            let air_ref = air.add_inst(AirInst {
                data: make_data(lhs_result.air_ref, rhs_result.air_ref),
                ty: Type::Bool,
                span,
            });
            return Ok(AnalysisResult::new(air_ref, Type::Bool));
        }

        // Validate the type is appropriate for this comparison
        if allow_bool {
            // Equality operators (==, !=) work on integers, booleans, strings, unit, and structs
            if !lhs_type.is_integer()
                && lhs_type != Type::Bool
                && lhs_type != Type::String
                && lhs_type != Type::Unit
                && !lhs_type.is_struct()
            {
                return Err(CompileError::new(
                    ErrorKind::TypeMismatch {
                        expected: "integer, bool, string, unit, or struct".to_string(),
                        found: lhs_type.name().to_string(),
                    },
                    self.rir.get(lhs).span,
                ));
            }
        } else if !lhs_type.is_integer() {
            return Err(CompileError::new(
                ErrorKind::TypeMismatch {
                    expected: "integer".to_string(),
                    found: lhs_type.name().to_string(),
                },
                self.rir.get(lhs).span,
            ));
        }

        let air_ref = air.add_inst(AirInst {
            data: make_data(lhs_result.air_ref, rhs_result.air_ref),
            ty: Type::Bool,
            span,
        });
        Ok(AnalysisResult::new(air_ref, Type::Bool))
    }

    /// Try to evaluate an RIR expression as a compile-time constant.
    ///
    /// Returns `Some(value)` if the expression can be fully evaluated at compile time,
    /// or `None` if evaluation requires runtime information (e.g., variable values,
    /// function calls) or would cause overflow/panic.
    ///
    /// This is the foundation for compile-time bounds checking and can be extended
    /// for future `comptime` features.
    fn try_evaluate_const(&self, inst_ref: InstRef) -> Option<ConstValue> {
        let inst = self.rir.get(inst_ref);
        match &inst.data {
            // Integer literals
            InstData::IntConst(value) => i64::try_from(*value).ok().map(ConstValue::Integer),

            // Boolean literals
            InstData::BoolConst(value) => Some(ConstValue::Bool(*value)),

            // Unary negation: -expr
            InstData::Neg { operand } => {
                match self.try_evaluate_const(*operand)? {
                    ConstValue::Integer(n) => n.checked_neg().map(ConstValue::Integer),
                    ConstValue::Bool(_) => None, // Can't negate a boolean
                }
            }

            // Logical NOT: !expr
            InstData::Not { operand } => {
                match self.try_evaluate_const(*operand)? {
                    ConstValue::Bool(b) => Some(ConstValue::Bool(!b)),
                    ConstValue::Integer(_) => None, // Can't logical-NOT an integer
                }
            }

            // Binary arithmetic operations
            InstData::Add { lhs, rhs } => {
                let l = self.try_evaluate_const(*lhs)?.as_integer()?;
                let r = self.try_evaluate_const(*rhs)?.as_integer()?;
                l.checked_add(r).map(ConstValue::Integer)
            }
            InstData::Sub { lhs, rhs } => {
                let l = self.try_evaluate_const(*lhs)?.as_integer()?;
                let r = self.try_evaluate_const(*rhs)?.as_integer()?;
                l.checked_sub(r).map(ConstValue::Integer)
            }
            InstData::Mul { lhs, rhs } => {
                let l = self.try_evaluate_const(*lhs)?.as_integer()?;
                let r = self.try_evaluate_const(*rhs)?.as_integer()?;
                l.checked_mul(r).map(ConstValue::Integer)
            }
            InstData::Div { lhs, rhs } => {
                let l = self.try_evaluate_const(*lhs)?.as_integer()?;
                let r = self.try_evaluate_const(*rhs)?.as_integer()?;
                if r == 0 {
                    None // Division by zero - defer to runtime
                } else {
                    l.checked_div(r).map(ConstValue::Integer)
                }
            }
            InstData::Mod { lhs, rhs } => {
                let l = self.try_evaluate_const(*lhs)?.as_integer()?;
                let r = self.try_evaluate_const(*rhs)?.as_integer()?;
                if r == 0 {
                    None // Modulo by zero - defer to runtime
                } else {
                    l.checked_rem(r).map(ConstValue::Integer)
                }
            }

            // Comparison operations
            InstData::Eq { lhs, rhs } => {
                let l = self.try_evaluate_const(*lhs)?;
                let r = self.try_evaluate_const(*rhs)?;
                match (l, r) {
                    (ConstValue::Integer(a), ConstValue::Integer(b)) => {
                        Some(ConstValue::Bool(a == b))
                    }
                    (ConstValue::Bool(a), ConstValue::Bool(b)) => Some(ConstValue::Bool(a == b)),
                    _ => None, // Mixed types
                }
            }
            InstData::Ne { lhs, rhs } => {
                let l = self.try_evaluate_const(*lhs)?;
                let r = self.try_evaluate_const(*rhs)?;
                match (l, r) {
                    (ConstValue::Integer(a), ConstValue::Integer(b)) => {
                        Some(ConstValue::Bool(a != b))
                    }
                    (ConstValue::Bool(a), ConstValue::Bool(b)) => Some(ConstValue::Bool(a != b)),
                    _ => None,
                }
            }
            InstData::Lt { lhs, rhs } => {
                let l = self.try_evaluate_const(*lhs)?.as_integer()?;
                let r = self.try_evaluate_const(*rhs)?.as_integer()?;
                Some(ConstValue::Bool(l < r))
            }
            InstData::Gt { lhs, rhs } => {
                let l = self.try_evaluate_const(*lhs)?.as_integer()?;
                let r = self.try_evaluate_const(*rhs)?.as_integer()?;
                Some(ConstValue::Bool(l > r))
            }
            InstData::Le { lhs, rhs } => {
                let l = self.try_evaluate_const(*lhs)?.as_integer()?;
                let r = self.try_evaluate_const(*rhs)?.as_integer()?;
                Some(ConstValue::Bool(l <= r))
            }
            InstData::Ge { lhs, rhs } => {
                let l = self.try_evaluate_const(*lhs)?.as_integer()?;
                let r = self.try_evaluate_const(*rhs)?.as_integer()?;
                Some(ConstValue::Bool(l >= r))
            }

            // Logical operations
            InstData::And { lhs, rhs } => {
                let l = self.try_evaluate_const(*lhs)?.as_bool()?;
                let r = self.try_evaluate_const(*rhs)?.as_bool()?;
                Some(ConstValue::Bool(l && r))
            }
            InstData::Or { lhs, rhs } => {
                let l = self.try_evaluate_const(*lhs)?.as_bool()?;
                let r = self.try_evaluate_const(*rhs)?.as_bool()?;
                Some(ConstValue::Bool(l || r))
            }

            // Bitwise operations
            InstData::BitAnd { lhs, rhs } => {
                let l = self.try_evaluate_const(*lhs)?.as_integer()?;
                let r = self.try_evaluate_const(*rhs)?.as_integer()?;
                Some(ConstValue::Integer(l & r))
            }
            InstData::BitOr { lhs, rhs } => {
                let l = self.try_evaluate_const(*lhs)?.as_integer()?;
                let r = self.try_evaluate_const(*rhs)?.as_integer()?;
                Some(ConstValue::Integer(l | r))
            }
            InstData::BitXor { lhs, rhs } => {
                let l = self.try_evaluate_const(*lhs)?.as_integer()?;
                let r = self.try_evaluate_const(*rhs)?.as_integer()?;
                Some(ConstValue::Integer(l ^ r))
            }
            InstData::Shl { lhs, rhs } => {
                let l = self.try_evaluate_const(*lhs)?.as_integer()?;
                let r = self.try_evaluate_const(*rhs)?.as_integer()?;
                // Only constant-fold small shift amounts to avoid type-width issues.
                // For shifts >= 8, defer to runtime where hardware handles masking correctly.
                // This is conservative but safe - we don't know the operand type here.
                if r < 0 || r >= 8 {
                    return None;
                }
                Some(ConstValue::Integer(l << r))
            }
            InstData::Shr { lhs, rhs } => {
                let l = self.try_evaluate_const(*lhs)?.as_integer()?;
                let r = self.try_evaluate_const(*rhs)?.as_integer()?;
                // Only constant-fold small shift amounts to avoid type-width issues.
                // For shifts >= 8, defer to runtime where hardware handles masking correctly.
                if r < 0 || r >= 8 {
                    return None;
                }
                Some(ConstValue::Integer(l >> r))
            }
            InstData::BitNot { operand } => {
                let n = self.try_evaluate_const(*operand)?.as_integer()?;
                Some(ConstValue::Integer(!n))
            }

            // Everything else requires runtime evaluation
            _ => None,
        }
    }

    /// Try to extract a constant integer value from an RIR index expression.
    ///
    /// This is used for compile-time bounds checking. Returns `Some(value)` if
    /// the index can be evaluated to an integer constant at compile time.
    fn try_get_const_index(&self, inst_ref: InstRef) -> Option<i64> {
        self.try_evaluate_const(inst_ref)?.as_integer()
    }

    /// Check if an RIR instruction is an integer literal.
    ///
    /// This is used for bidirectional type inference to detect when the LHS
    /// of a binary operator is a literal that can adopt its type from the RHS.
    fn is_integer_literal(&self, inst_ref: InstRef) -> bool {
        matches!(self.rir.get(inst_ref).data, InstData::IntConst(_))
    }

    /// Check if an RIR instruction is a comparison operation.
    ///
    /// This is used to detect chained comparisons (e.g., `a < b < c`) which are
    /// not allowed in Rue.
    fn is_comparison(&self, inst_ref: InstRef) -> bool {
        matches!(
            self.rir.get(inst_ref).data,
            InstData::Lt { .. }
                | InstData::Gt { .. }
                | InstData::Le { .. }
                | InstData::Ge { .. }
                | InstData::Eq { .. }
                | InstData::Ne { .. }
        )
    }

    /// Analyze a String associated function call (String::new, String::with_capacity).
    ///
    /// These are built-in functions for the String type that construct new String values.
    fn analyze_string_assoc_fn(
        &mut self,
        air: &mut Air,
        ctx: &mut AnalysisContext,
        function_name: &str,
        args: &[RirCallArg],
        span: Span,
    ) -> CompileResult<AnalysisResult> {
        match function_name {
            "new" => {
                // String::new() takes no arguments and returns an empty String
                if !args.is_empty() {
                    return Err(CompileError::new(
                        ErrorKind::WrongArgumentCount {
                            expected: 0,
                            found: args.len(),
                        },
                        span,
                    ));
                }

                // Generate a call to String__new (runtime function)
                // We use double underscore because :: is not valid in C symbol names
                let call_name = self.interner.intern("String__new");
                // No args to encode
                let args_start = air.add_extra(&[]);
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::Call {
                        name: call_name,
                        args_start,
                        args_len: 0,
                    },
                    ty: Type::String,
                    span,
                });
                Ok(AnalysisResult::new(air_ref, Type::String))
            }

            "with_capacity" => {
                // String::with_capacity(cap: u64) takes one u64 argument
                if args.len() != 1 {
                    return Err(CompileError::new(
                        ErrorKind::WrongArgumentCount {
                            expected: 1,
                            found: args.len(),
                        },
                        span,
                    ));
                }

                // Analyze the capacity argument
                let cap_result = self.analyze_inst(air, args[0].value, ctx)?;

                // Verify the type is u64
                if cap_result.ty != Type::U64 && !cap_result.ty.is_error() {
                    return Err(CompileError::new(
                        ErrorKind::TypeMismatch {
                            expected: "u64".to_string(),
                            found: cap_result.ty.name().to_string(),
                        },
                        span,
                    ));
                }

                // Generate a call to String__with_capacity (runtime function)
                let call_name = self.interner.intern("String__with_capacity");
                // Encode args into extra array
                let args_start =
                    air.add_extra(&[cap_result.air_ref.as_u32(), AirArgMode::Normal.as_u32()]);
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::Call {
                        name: call_name,
                        args_start,
                        args_len: 1,
                    },
                    ty: Type::String,
                    span,
                });
                Ok(AnalysisResult::new(air_ref, Type::String))
            }

            _ => {
                // Unknown associated function for String
                Err(CompileError::new(
                    ErrorKind::UndefinedAssocFn {
                        type_name: "String".to_string(),
                        function_name: function_name.to_string(),
                    },
                    span,
                ))
            }
        }
    }

    /// Get the storage location for a String receiver in a mutation method call.
    ///
    /// For mutation methods like `push_str`, `push`, `clear`, `reserve`, we need
    /// to know where to store the updated String after the runtime function returns.
    ///
    /// Returns `Some(storage)` if the receiver is a mutable local or inout parameter.
    /// Returns an error if the receiver is:
    /// - An immutable binding (`let` instead of `var`)
    /// - A borrow parameter (can't mutate borrowed values)
    /// - Not an lvalue (e.g., a function call result)
    fn get_string_receiver_storage(
        &self,
        receiver_ref: InstRef,
        ctx: &AnalysisContext,
        span: Span,
    ) -> CompileResult<Option<StringReceiverStorage>> {
        let receiver_inst = self.rir.get(receiver_ref);

        match &receiver_inst.data {
            InstData::VarRef { name } => {
                // Check if this is a parameter
                if let Some(param_info) = ctx.params.get(name) {
                    // Check parameter mode
                    match param_info.mode {
                        RirParamMode::Inout => {
                            return Ok(Some(StringReceiverStorage::Param {
                                abi_slot: param_info.abi_slot,
                            }));
                        }
                        RirParamMode::Borrow => {
                            let name_str = self.interner.get(*name);
                            return Err(CompileError::new(
                                ErrorKind::MutateBorrowedValue {
                                    variable: name_str.to_string(),
                                },
                                span,
                            ));
                        }
                        RirParamMode::Normal => {
                            // Normal parameters can be mutated if declared as `var`
                            // For now, we don't allow mutation of normal params
                            let name_str = self.interner.get(*name);
                            return Err(CompileError::new(
                                ErrorKind::AssignToImmutable(name_str.to_string()),
                                span,
                            ));
                        }
                    }
                }

                // Check if it's a local variable
                if let Some(local) = ctx.locals.get(name) {
                    if !local.is_mut {
                        let name_str = self.interner.get(*name);
                        return Err(CompileError::new(
                            ErrorKind::AssignToImmutable(name_str.to_string()),
                            span,
                        ));
                    }
                    return Ok(Some(StringReceiverStorage::Local { slot: local.slot }));
                }

                // Variable not found
                let name_str = self.interner.get(*name);
                Err(CompileError::new(
                    ErrorKind::UndefinedVariable(name_str.to_string()),
                    span,
                ))
            }

            // For other receiver types (field access, function calls, etc.),
            // we don't support mutation for now
            _ => Err(CompileError::new(ErrorKind::InvalidAssignmentTarget, span)),
        }
    }

    /// Analyze a String mutation method call (push_str, push, clear, reserve).
    ///
    /// These methods use `inout self` semantics - they modify the string in place.
    /// The runtime function returns an updated String (ptr, len, cap) which we
    /// store back to the original variable.
    fn analyze_string_mutation_method(
        &mut self,
        air: &mut Air,
        ctx: &mut AnalysisContext,
        method_name: &str,
        receiver: AnalysisResult,
        receiver_storage: Option<StringReceiverStorage>,
        args: &[RirCallArg],
        span: Span,
    ) -> CompileResult<AnalysisResult> {
        // Storage is required for mutation methods
        let storage = receiver_storage
            .ok_or_else(|| CompileError::new(ErrorKind::InvalidAssignmentTarget, span))?;

        match method_name {
            "push_str" => {
                // fn push_str(inout self, other: String)
                if args.len() != 1 {
                    return Err(CompileError::new(
                        ErrorKind::WrongArgumentCount {
                            expected: 1,
                            found: args.len(),
                        },
                        span,
                    ));
                }

                // Analyze the other string argument
                let other_result = self.analyze_inst(air, args[0].value, ctx)?;

                // Type check: other must be String
                if other_result.ty != Type::String && !other_result.ty.is_error() {
                    return Err(CompileError::new(
                        ErrorKind::TypeMismatch {
                            expected: "String".to_string(),
                            found: other_result.ty.name().to_string(),
                        },
                        span,
                    ));
                }

                // Call String__push_str(self, other) -> String
                let call_name = self.interner.intern("String__push_str");
                let args_start = air.add_extra(&[
                    receiver.air_ref.as_u32(),
                    AirArgMode::Normal.as_u32(),
                    other_result.air_ref.as_u32(),
                    AirArgMode::Normal.as_u32(),
                ]);
                let call_ref = air.add_inst(AirInst {
                    data: AirInstData::Call {
                        name: call_name,
                        args_start,
                        args_len: 2,
                    },
                    ty: Type::String,
                    span,
                });

                // Store the result back to the receiver
                self.store_string_result(air, call_ref, storage, span)
            }

            "push" => {
                // fn push(inout self, byte: u8)
                if args.len() != 1 {
                    return Err(CompileError::new(
                        ErrorKind::WrongArgumentCount {
                            expected: 1,
                            found: args.len(),
                        },
                        span,
                    ));
                }

                // Analyze the byte argument
                let byte_result = self.analyze_inst(air, args[0].value, ctx)?;

                // Type check: byte must be u8
                if byte_result.ty != Type::U8 && !byte_result.ty.is_error() {
                    return Err(CompileError::new(
                        ErrorKind::TypeMismatch {
                            expected: "u8".to_string(),
                            found: byte_result.ty.name().to_string(),
                        },
                        span,
                    ));
                }

                // Call String__push(self, byte) -> String
                let call_name = self.interner.intern("String__push");
                let args_start = air.add_extra(&[
                    receiver.air_ref.as_u32(),
                    AirArgMode::Normal.as_u32(),
                    byte_result.air_ref.as_u32(),
                    AirArgMode::Normal.as_u32(),
                ]);
                let call_ref = air.add_inst(AirInst {
                    data: AirInstData::Call {
                        name: call_name,
                        args_start,
                        args_len: 2,
                    },
                    ty: Type::String,
                    span,
                });

                // Store the result back to the receiver
                self.store_string_result(air, call_ref, storage, span)
            }

            "clear" => {
                // fn clear(inout self)
                if !args.is_empty() {
                    return Err(CompileError::new(
                        ErrorKind::WrongArgumentCount {
                            expected: 0,
                            found: args.len(),
                        },
                        span,
                    ));
                }

                // Call String__clear(self) -> String
                let call_name = self.interner.intern("String__clear");
                let args_start =
                    air.add_extra(&[receiver.air_ref.as_u32(), AirArgMode::Normal.as_u32()]);
                let call_ref = air.add_inst(AirInst {
                    data: AirInstData::Call {
                        name: call_name,
                        args_start,
                        args_len: 1,
                    },
                    ty: Type::String,
                    span,
                });

                // Store the result back to the receiver
                self.store_string_result(air, call_ref, storage, span)
            }

            "reserve" => {
                // fn reserve(inout self, additional: u64)
                if args.len() != 1 {
                    return Err(CompileError::new(
                        ErrorKind::WrongArgumentCount {
                            expected: 1,
                            found: args.len(),
                        },
                        span,
                    ));
                }

                // Analyze the additional argument
                let additional_result = self.analyze_inst(air, args[0].value, ctx)?;

                // Type check: additional must be u64
                if additional_result.ty != Type::U64 && !additional_result.ty.is_error() {
                    return Err(CompileError::new(
                        ErrorKind::TypeMismatch {
                            expected: "u64".to_string(),
                            found: additional_result.ty.name().to_string(),
                        },
                        span,
                    ));
                }

                // Call String__reserve(self, additional) -> String
                let call_name = self.interner.intern("String__reserve");
                let args_start = air.add_extra(&[
                    receiver.air_ref.as_u32(),
                    AirArgMode::Normal.as_u32(),
                    additional_result.air_ref.as_u32(),
                    AirArgMode::Normal.as_u32(),
                ]);
                let call_ref = air.add_inst(AirInst {
                    data: AirInstData::Call {
                        name: call_name,
                        args_start,
                        args_len: 2,
                    },
                    ty: Type::String,
                    span,
                });

                // Store the result back to the receiver
                self.store_string_result(air, call_ref, storage, span)
            }

            _ => {
                // This should never happen - caller should only call this for mutation methods
                Err(CompileError::new(
                    ErrorKind::UndefinedMethod {
                        type_name: "String".to_string(),
                        method_name: method_name.to_string(),
                    },
                    span,
                ))
            }
        }
    }

    /// Store the result of a String mutation method back to the receiver's storage.
    ///
    /// Returns a Unit-typed result since mutation methods don't return a value.
    fn store_string_result(
        &self,
        air: &mut Air,
        call_ref: AirRef,
        storage: StringReceiverStorage,
        span: Span,
    ) -> CompileResult<AnalysisResult> {
        let store_ref = match storage {
            StringReceiverStorage::Local { slot } => air.add_inst(AirInst {
                data: AirInstData::Store {
                    slot,
                    value: call_ref,
                },
                ty: Type::Unit,
                span,
            }),
            StringReceiverStorage::Param { abi_slot } => air.add_inst(AirInst {
                data: AirInstData::ParamStore {
                    param_slot: abi_slot,
                    value: call_ref,
                },
                ty: Type::Unit,
                span,
            }),
        };

        Ok(AnalysisResult::new(store_ref, Type::Unit))
    }

    /// Analyze a String method call (len, capacity, is_empty, etc.).
    ///
    /// These are built-in methods for the String type that query the string's state.
    /// Query methods use `borrow self` semantics - they don't consume the string.
    fn analyze_string_method(
        &mut self,
        air: &mut Air,
        _ctx: &mut AnalysisContext,
        method_name: &str,
        receiver: AnalysisResult,
        args: &[RirCallArg],
        span: Span,
    ) -> CompileResult<AnalysisResult> {
        match method_name {
            "len" => {
                // fn len(borrow self) -> u64
                // Returns the length of the string in bytes
                if !args.is_empty() {
                    return Err(CompileError::new(
                        ErrorKind::WrongArgumentCount {
                            expected: 0,
                            found: args.len(),
                        },
                        span,
                    ));
                }

                // Generate a call to String__len (runtime function)
                // We pass the String by value (its 3 fields) but don't consume it semantically.
                // The caller still owns the String after this call.
                // Using Normal mode instead of Borrow because String's 3 fields should be
                // passed directly, not by reference.
                let call_name = self.interner.intern("String__len");
                let args_start =
                    air.add_extra(&[receiver.air_ref.as_u32(), AirArgMode::Normal.as_u32()]);
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::Call {
                        name: call_name,
                        args_start,
                        args_len: 1,
                    },
                    ty: Type::U64,
                    span,
                });
                Ok(AnalysisResult::new(air_ref, Type::U64))
            }

            "capacity" => {
                // fn capacity(borrow self) -> u64
                // Returns the allocated capacity (0 for string literals)
                if !args.is_empty() {
                    return Err(CompileError::new(
                        ErrorKind::WrongArgumentCount {
                            expected: 0,
                            found: args.len(),
                        },
                        span,
                    ));
                }

                // Generate a call to String__capacity (runtime function)
                // Same pattern as len() - pass by value, don't consume.
                let call_name = self.interner.intern("String__capacity");
                let args_start =
                    air.add_extra(&[receiver.air_ref.as_u32(), AirArgMode::Normal.as_u32()]);
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::Call {
                        name: call_name,
                        args_start,
                        args_len: 1,
                    },
                    ty: Type::U64,
                    span,
                });
                Ok(AnalysisResult::new(air_ref, Type::U64))
            }

            "is_empty" => {
                // fn is_empty(borrow self) -> bool
                // Returns true if the string length is zero
                if !args.is_empty() {
                    return Err(CompileError::new(
                        ErrorKind::WrongArgumentCount {
                            expected: 0,
                            found: args.len(),
                        },
                        span,
                    ));
                }

                // Generate a call to String__is_empty (runtime function)
                // Same pattern as len() - pass by value, don't consume.
                let call_name = self.interner.intern("String__is_empty");
                let args_start =
                    air.add_extra(&[receiver.air_ref.as_u32(), AirArgMode::Normal.as_u32()]);
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::Call {
                        name: call_name,
                        args_start,
                        args_len: 1,
                    },
                    ty: Type::Bool,
                    span,
                });
                Ok(AnalysisResult::new(air_ref, Type::Bool))
            }

            "clone" => {
                // fn clone(borrow self) -> String
                // Creates a deep copy of the string
                if !args.is_empty() {
                    return Err(CompileError::new(
                        ErrorKind::WrongArgumentCount {
                            expected: 0,
                            found: args.len(),
                        },
                        span,
                    ));
                }

                // Generate a call to String__clone (runtime function)
                // Takes the String (ptr, len, cap) and returns a new String (ptr, len, cap)
                // where the new ptr points to freshly allocated memory with copied content.
                let call_name = self.interner.intern("String__clone");
                let args_start =
                    air.add_extra(&[receiver.air_ref.as_u32(), AirArgMode::Normal.as_u32()]);
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::Call {
                        name: call_name,
                        args_start,
                        args_len: 1,
                    },
                    ty: Type::String,
                    span,
                });
                Ok(AnalysisResult::new(air_ref, Type::String))
            }

            _ => {
                // Unknown method for String
                Err(CompileError::new(
                    ErrorKind::UndefinedMethod {
                        type_name: "String".to_string(),
                        method_name: method_name.to_string(),
                    },
                    span,
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rue_lexer::Lexer;
    use rue_parser::Parser;
    use rue_rir::AstGen;

    fn compile_to_air(source: &str) -> MultiErrorResult<SemaOutput> {
        let lexer = Lexer::new(source);
        let (tokens, interner) = lexer.tokenize().map_err(CompileErrors::from_error)?;
        let parser = Parser::new(tokens, interner);
        let (ast, mut interner) = parser.parse().map_err(CompileErrors::from_error)?;

        let astgen = AstGen::new(&ast, &mut interner);
        let rir = astgen.generate();

        let sema = Sema::new(&rir, &mut interner, PreviewFeatures::new());
        sema.analyze_all()
    }

    #[test]
    fn test_analyze_simple_function() {
        let output = compile_to_air("fn main() -> i32 { 42 }").unwrap();
        let functions = &output.functions;

        assert_eq!(functions.len(), 1);
        assert_eq!(functions[0].name, "main");

        let air = &functions[0].air;
        assert_eq!(air.return_type(), Type::I32);
        assert_eq!(air.len(), 2); // Const + Ret
    }

    #[test]
    fn test_analyze_addition() {
        let output = compile_to_air("fn main() -> i32 { 1 + 2 }").unwrap();

        let air = &output.functions[0].air;
        assert_eq!(air.return_type(), Type::I32);
        // Const(1) + Const(2) + Add + Ret = 4 instructions
        assert_eq!(air.len(), 4);

        // Check that add instruction exists with correct type
        let add_inst = air.get(AirRef::from_raw(2));
        assert!(matches!(add_inst.data, AirInstData::Add(_, _)));
        assert_eq!(add_inst.ty, Type::I32);
    }

    #[test]
    fn test_analyze_all_binary_ops() {
        // Test that all binary operators compile correctly
        assert!(compile_to_air("fn main() -> i32 { 1 + 2 }").is_ok());
        assert!(compile_to_air("fn main() -> i32 { 1 - 2 }").is_ok());
        assert!(compile_to_air("fn main() -> i32 { 1 * 2 }").is_ok());
        assert!(compile_to_air("fn main() -> i32 { 1 / 2 }").is_ok());
        assert!(compile_to_air("fn main() -> i32 { 1 % 2 }").is_ok());
    }

    #[test]
    fn test_analyze_negation() {
        let output = compile_to_air("fn main() -> i32 { -42 }").unwrap();

        let air = &output.functions[0].air;
        // Const(42) + Neg + Ret = 3 instructions
        assert_eq!(air.len(), 3);

        let neg_inst = air.get(AirRef::from_raw(1));
        assert!(matches!(neg_inst.data, AirInstData::Neg(_)));
        assert_eq!(neg_inst.ty, Type::I32);
    }

    #[test]
    fn test_analyze_complex_expr() {
        let output = compile_to_air("fn main() -> i32 { (1 + 2) * 3 }").unwrap();

        let air = &output.functions[0].air;
        // Const(1) + Const(2) + Add + Const(3) + Mul + Ret = 6 instructions
        assert_eq!(air.len(), 6);

        // Check that result is multiplication
        let mul_inst = air.get(AirRef::from_raw(4));
        assert!(matches!(mul_inst.data, AirInstData::Mul(_, _)));
    }

    #[test]
    fn test_analyze_let_binding() {
        let output = compile_to_air("fn main() -> i32 { let x = 42; x }").unwrap();

        assert_eq!(output.functions.len(), 1);
        assert_eq!(output.functions[0].num_locals, 1);

        let air = &output.functions[0].air;
        // Const(42) + StorageLive + Alloc + Block([StorageLive], Alloc) + Load + Block([alloc block], Load) + Ret = 7 instructions
        assert_eq!(air.len(), 7);

        // Check storage_live instruction
        let storage_live_inst = air.get(AirRef::from_raw(1));
        assert!(matches!(
            storage_live_inst.data,
            AirInstData::StorageLive { slot: 0 }
        ));

        // Check alloc instruction
        let alloc_inst = air.get(AirRef::from_raw(2));
        assert!(matches!(
            alloc_inst.data,
            AirInstData::Alloc { slot: 0, .. }
        ));

        // Check load instruction
        let load_inst = air.get(AirRef::from_raw(4));
        assert!(matches!(load_inst.data, AirInstData::Load { slot: 0 }));

        // Check block instruction groups the alloc with the load
        let block_inst = air.get(AirRef::from_raw(5));
        assert!(matches!(block_inst.data, AirInstData::Block { .. }));
    }

    #[test]
    fn test_analyze_let_mut_assignment() {
        let output = compile_to_air("fn main() -> i32 { let mut x = 10; x = 20; x }").unwrap();

        let air = &output.functions[0].air;
        // Const(10) + StorageLive + Alloc + Block([StorageLive], Alloc) + Const(20) + Store + Load + Block([alloc block, Store], Load) + Ret = 9 instructions
        assert_eq!(air.len(), 9);

        // Check store instruction
        let store_inst = air.get(AirRef::from_raw(5));
        assert!(matches!(
            store_inst.data,
            AirInstData::Store { slot: 0, .. }
        ));

        // Check block instruction groups statements
        let block_inst = air.get(AirRef::from_raw(7));
        assert!(matches!(block_inst.data, AirInstData::Block { .. }));
    }

    #[test]
    fn test_undefined_variable() {
        let result = compile_to_air("fn main() -> i32 { x }");
        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert_eq!(errors.len(), 1);
        assert!(matches!(
            errors.iter().next().unwrap().kind,
            ErrorKind::UndefinedVariable(_)
        ));
    }

    #[test]
    fn test_assign_to_immutable() {
        let result = compile_to_air("fn main() -> i32 { let x = 10; x = 20; x }");
        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert_eq!(errors.len(), 1);
        assert!(matches!(
            errors.iter().next().unwrap().kind,
            ErrorKind::AssignToImmutable(_)
        ));
    }

    #[test]
    fn test_multiple_variables() {
        let output = compile_to_air("fn main() -> i32 { let x = 10; let y = 20; x + y }").unwrap();

        assert_eq!(output.functions[0].num_locals, 2);
    }

    #[test]
    fn test_empty_block_evaluates_to_unit() {
        // Empty block should evaluate to () and not panic
        let output = compile_to_air("fn main() { let _x: () = {}; }").unwrap();

        let air = &output.functions[0].air;
        // Should have a UnitConst instruction for the empty block
        let has_unit_const = air
            .iter()
            .any(|(_, inst)| matches!(inst.data, AirInstData::UnitConst));
        assert!(has_unit_const, "Empty block should produce UnitConst");
    }
}
