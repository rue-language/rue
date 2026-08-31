use super::*;

/// The sole recursive RIR dispatcher for compile-time evaluation.
///
/// Query registries and completed-call memoization remain host capabilities;
/// the core owns only its active frame stack and never stores durable facts.
macro_rules! outcome_value {
    ($value:expr) => {
        match $value {
            ComptimeOutcome::Known(value) => value,
            ComptimeOutcome::RuntimeDependent => return ComptimeOutcome::RuntimeDependent,
            ComptimeOutcome::NotReady => return ComptimeOutcome::NotReady,
            ComptimeOutcome::UnsupportedContext => return ComptimeOutcome::UnsupportedContext,
            ComptimeOutcome::Trap(trap) => return ComptimeOutcome::Trap(trap),
            ComptimeOutcome::HostFailure(error) => return ComptimeOutcome::HostFailure(error),
            ComptimeOutcome::Abort(error) => return ComptimeOutcome::Abort(error),
        }
    };
}

macro_rules! host_value {
    ($value:expr) => {
        match $value {
            Ok(value) => value,
            Err(ComptimeHostError::HostFailure(error)) => {
                return ComptimeOutcome::HostFailure(error)
            }
            Err(ComptimeHostError::Abort(error)) => return ComptimeOutcome::Abort(error),
        }
    };
}

pub struct ComptimeEngine<'e, H: ComptimeHost> {
    pub(super) host: &'e mut H,
    frames: Vec<
        ComptimeFrame<H::Value, H::Type, H::Name, H::File, H::ProgramKey, H::CanonicalIdentity>,
    >,
    #[cfg(test)]
    provenance_classifications: usize,
}

impl<'e, H: ComptimeHost> ComptimeEngine<'e, H> {
    pub fn new(host: &'e mut H) -> Self {
        Self {
            host,
            frames: Vec::new(),
            #[cfg(test)]
            provenance_classifications: 0,
        }
    }

    fn decode_match_pattern(
        &self,
        program: &H::ProgramKey,
        pattern: &rue_rir::RirPatternView<'_>,
    ) -> ComptimeMatchPattern<H::Name> {
        decode_comptime_match_pattern(pattern, |symbol| {
            self.host.name_from_symbol(program, symbol)
        })
    }

    pub(super) fn classify_array_length_binding(
        env: &ComptimeEnv<'_, H::Value, H::Type, H::Name, H::File, H::CanonicalIdentity>,
        name: &H::Name,
    ) -> ComptimeArrayLengthBinding<H::Value> {
        if let Some(value) = env.locals.get(name) {
            return ComptimeArrayLengthBinding::LocalValue(value.clone());
        }
        if env.is_runtime_local_name(name) {
            return ComptimeArrayLengthBinding::RuntimeDependent;
        }
        if env.type_subst.contains_key(name) {
            return ComptimeArrayLengthBinding::Shadowed;
        }
        if let Some(value) = env.value_subst.get(name) {
            return ComptimeArrayLengthBinding::LocalValue(value.clone());
        }
        if env.runtime_binding_names.contains(name) {
            return ComptimeArrayLengthBinding::RuntimeDependent;
        }
        ComptimeArrayLengthBinding::Unbound
    }

    #[cfg(test)]
    pub(super) fn provenance_classification_count(&self) -> usize {
        self.provenance_classifications
    }

    /// Drive one opaque structured-type suspension on this engine's existing
    /// frame stack. Only this method interprets `Memoized` versus `Enter` for
    /// structured-type reductions; hosts merely prepare and resume typed
    /// continuations.
    fn drive_structured_type(
        &mut self,
        suspension: H::StructuredTypeSuspension,
        span: Span,
    ) -> ComptimeOutcome<H::Type, H::Failure> {
        let mut suspension = suspension;
        loop {
            let preparation = self.host.prepare_structured_type_call(&suspension, span);
            let reduced = match preparation {
                ComptimeOutcome::Known(Some(preparation)) => match preparation {
                    ComptimeCallPreparation::Memoized(outcome) => outcome,
                    ComptimeCallPreparation::Enter { frame, ticket } => {
                        let span = frame.span;
                        self.enter_prepared_call(frame, ticket, span)
                    }
                },
                ComptimeOutcome::Known(None) => ComptimeOutcome::RuntimeDependent,
                ComptimeOutcome::RuntimeDependent => ComptimeOutcome::RuntimeDependent,
                ComptimeOutcome::NotReady => ComptimeOutcome::NotReady,
                ComptimeOutcome::UnsupportedContext => ComptimeOutcome::UnsupportedContext,
                ComptimeOutcome::Trap(trap) => ComptimeOutcome::Trap(trap),
                ComptimeOutcome::HostFailure(error) => ComptimeOutcome::HostFailure(error),
                ComptimeOutcome::Abort(error) => ComptimeOutcome::Abort(error),
            };
            match self.host.resume_structured_type_call(suspension, reduced) {
                ComptimeOutcome::Known(ComptimeStructuredTypeResolution::Ready(value)) => {
                    return ComptimeOutcome::Known(value);
                }
                ComptimeOutcome::Known(ComptimeStructuredTypeResolution::Suspended(next)) => {
                    suspension = next;
                }
                ComptimeOutcome::RuntimeDependent => return ComptimeOutcome::RuntimeDependent,
                ComptimeOutcome::NotReady => return ComptimeOutcome::NotReady,
                ComptimeOutcome::UnsupportedContext => {
                    return ComptimeOutcome::UnsupportedContext;
                }
                ComptimeOutcome::Trap(trap) => return ComptimeOutcome::Trap(trap),
                ComptimeOutcome::HostFailure(error) => {
                    return ComptimeOutcome::HostFailure(error);
                }
                ComptimeOutcome::Abort(error) => return ComptimeOutcome::Abort(error),
            }
        }
    }

    /// Route a type-bearing instruction through the same engine-owned
    /// structured loop as every other typed reduction. A synchronous host
    /// returns `Ready`; a keyed host may return a canonical job suspension.
    fn evaluate_comptime_type_syntax(
        &mut self,
        program: &H::ProgramKey,
        syntax: rue_rir::RirTypeSyntaxRef,
        types: &AHashMap<H::Name, H::Type>,
        values: &AHashMap<H::Name, H::Value>,
        span: Span,
    ) -> ComptimeOutcome<H::Type, H::Failure> {
        match self
            .host
            .begin_comptime_type_syntax(program, syntax, types, values, span)
        {
            ComptimeOutcome::Known(ComptimeStructuredTypeResolution::Ready(value)) => {
                ComptimeOutcome::Known(value)
            }
            ComptimeOutcome::Known(ComptimeStructuredTypeResolution::Suspended(suspension)) => {
                self.drive_structured_type(suspension, span)
            }
            ComptimeOutcome::RuntimeDependent => ComptimeOutcome::RuntimeDependent,
            ComptimeOutcome::NotReady => ComptimeOutcome::NotReady,
            ComptimeOutcome::UnsupportedContext => ComptimeOutcome::UnsupportedContext,
            ComptimeOutcome::Trap(trap) => ComptimeOutcome::Trap(trap),
            ComptimeOutcome::HostFailure(error) => ComptimeOutcome::HostFailure(error),
            ComptimeOutcome::Abort(error) => ComptimeOutcome::Abort(error),
        }
    }

    /// Decode anonymous method signatures from RIR exactly once, at the AIR
    /// boundary. Hosts receive the resolved descriptor below and therefore do
    /// not need to interpret `FnDecl`, parameter ranges, or type syntax.
    pub(crate) fn decode_anon_method_descriptors(
        &mut self,
        program: &H::ProgramKey,
        methods: &rue_rir::RirAnonStructMethodsRange,
        types: &AHashMap<H::Name, H::Type>,
        values: &AHashMap<H::Name, H::Value>,
    ) -> ComptimeOutcome<Vec<ComptimeMethodDescriptor<H::Name, H::Type>>, H::Failure> {
        let method_refs = self
            .host
            .program_rir(program)
            .anon_struct_methods(methods)
            .to_vec();
        let mut descriptors = Vec::with_capacity(method_refs.len());
        for method_ref in method_refs {
            let (instruction_data, method_span) = {
                let instruction = self.host.program_rir(program).get(method_ref);
                (instruction.data.clone(), instruction.span)
            };
            let InstData::FnDecl {
                name,
                params,
                return_type,
                has_self,
                self_mode,
                returns_borrow,
                returns_inout,
                ..
            } = instruction_data
            else {
                let site = ComptimeDiagnosticSite::new(program.clone(), method_span);
                return ComptimeOutcome::HostFailure(self.host.non_function_anon_method(&site));
            };
            let method_name = self.host.name_from_symbol(program, name.into());
            let parameter_data = self.host.program_rir(program).params(&params).to_vec();
            let parameter_names = parameter_data
                .iter()
                .map(|parameter| self.host.name_from_symbol(program, parameter.name.into()))
                .collect();
            // Preserve declaration-level diagnostic priority: reject an own
            // comptime type parameter before any other parameter or result
            // syntax can suspend or fail.
            if parameter_data.iter().any(|parameter| {
                parameter.is_comptime
                    && self
                        .host
                        .rir_type_named_symbol(program, parameter.ty)
                        .is_some_and(|name| self.host.display_name(&name) == "type")
            }) {
                let site = ComptimeDiagnosticSite::new(program.clone(), method_span);
                return ComptimeOutcome::HostFailure(self.host.unsupported_anon_method_type_param(
                    &self.host.display_name(&method_name),
                    &site,
                ));
            }
            let mut parameters = Vec::with_capacity(parameter_data.len());
            for parameter in parameter_data {
                let is_self = self
                    .host
                    .rir_type_named_symbol(program, parameter.ty)
                    .is_some_and(|name| self.host.display_name(&name) == "Self");
                let is_comptime_type = parameter.is_comptime
                    && self
                        .host
                        .rir_type_named_symbol(program, parameter.ty)
                        .is_some_and(|name| self.host.display_name(&name) == "type");
                let ty = if is_self {
                    ComptimeMethodType::SelfType
                } else {
                    match self.evaluate_comptime_type_syntax(
                        program,
                        parameter.ty,
                        types,
                        values,
                        method_span,
                    ) {
                        ComptimeOutcome::Known(ty) => ComptimeMethodType::Concrete(ty),
                        ComptimeOutcome::RuntimeDependent | ComptimeOutcome::UnsupportedContext => {
                            ComptimeMethodType::Unsupported(
                                self.host
                                    .rir_type_named_symbol(program, parameter.ty)
                                    .map_or_else(
                                        || self.host.render_rir_type(program, parameter.ty),
                                        |name| self.host.display_name(&name),
                                    ),
                            )
                        }
                        ComptimeOutcome::NotReady => return ComptimeOutcome::NotReady,
                        ComptimeOutcome::Trap(trap) => return ComptimeOutcome::Trap(trap),
                        ComptimeOutcome::HostFailure(error) => {
                            return ComptimeOutcome::HostFailure(error);
                        }
                        ComptimeOutcome::Abort(error) => return ComptimeOutcome::Abort(error),
                    }
                };
                parameters.push(ComptimeMethodParameter {
                    ty,
                    mode: parameter.mode,
                    is_comptime: parameter.is_comptime,
                    is_comptime_type,
                });
            }
            let result = if self
                .host
                .rir_type_named_symbol(program, return_type)
                .is_some_and(|name| self.host.display_name(&name) == "Self")
            {
                ComptimeMethodType::SelfType
            } else {
                match self.evaluate_comptime_type_syntax(
                    program,
                    return_type,
                    types,
                    values,
                    method_span,
                ) {
                    ComptimeOutcome::Known(ty) => ComptimeMethodType::Concrete(ty),
                    ComptimeOutcome::RuntimeDependent | ComptimeOutcome::UnsupportedContext => {
                        ComptimeMethodType::Unsupported(
                            self.host
                                .rir_type_named_symbol(program, return_type)
                                .map_or_else(
                                    || self.host.render_rir_type(program, return_type),
                                    |name| self.host.display_name(&name),
                                ),
                        )
                    }
                    ComptimeOutcome::NotReady => return ComptimeOutcome::NotReady,
                    ComptimeOutcome::Trap(trap) => return ComptimeOutcome::Trap(trap),
                    ComptimeOutcome::HostFailure(error) => {
                        return ComptimeOutcome::HostFailure(error);
                    }
                    ComptimeOutcome::Abort(error) => return ComptimeOutcome::Abort(error),
                }
            };
            descriptors.push(ComptimeMethodDescriptor {
                name: method_name,
                has_self,
                self_mode,
                returns_borrow,
                returns_inout,
                parameters,
                parameter_names,
                result,
                declaration_span: method_span,
            });
        }
        ComptimeOutcome::Known(descriptors)
    }

    pub(super) fn program_rir(&self) -> &Rir {
        let frame = self
            .frames
            .last()
            .expect("comptime evaluation requires an active frame");
        self.host.program_rir(&frame.program)
    }

    pub(super) fn program_key(&self) -> H::ProgramKey {
        self.frames
            .last()
            .expect("comptime evaluation requires an active frame")
            .program
            .clone()
    }

    fn diagnostic_site(&self, span: Span) -> ComptimeDiagnosticSite<H::ProgramKey> {
        ComptimeDiagnosticSite::new(self.program_key(), span)
    }

    fn semantic_site(
        &self,
        inst_ref: InstRef,
        kind: ComptimeSiteKind,
        span: Span,
    ) -> ComptimeSite<H::ProgramKey> {
        let program = self.program_key();
        let rir = self.host.program_rir(&program);
        let mut sites = Vec::new();
        for (candidate, instruction) in rir.iter() {
            let candidate_kind = match &instruction.data {
                InstData::Intrinsic { name, args } => self
                    .decode_expression_intrinsic(
                        self.host.name_from_symbol(&program, (*name).into()),
                        args,
                    )
                    .ok()
                    .map(|decoded| decoded.site_kind),
                InstData::EnumVariant { .. } if kind == ComptimeSiteKind::EnumVariant => {
                    Some(ComptimeSiteKind::EnumVariant)
                }
                InstData::FieldGet { .. } if kind == ComptimeSiteKind::Member => {
                    Some(ComptimeSiteKind::Member)
                }
                _ => None,
            };
            if candidate_kind == Some(kind) {
                sites.push((instruction.span.start, instruction.span.end, candidate));
            }
        }
        sites.sort_by_key(|(start, end, candidate)| (*start, *end, candidate.as_u32()));
        let occurrence = sites
            .iter()
            .position(|(_, _, candidate)| *candidate == inst_ref)
            .expect("classified comptime site must be present in its owning RIR");
        let occurrence =
            u32::try_from(occurrence).expect("comptime site occurrence must fit in u32");
        ComptimeSite::new(program, kind, occurrence, span)
    }

    fn name_from_rir(&self, symbol: SymbolHandle) -> H::Name {
        let frame = self
            .frames
            .last()
            .expect("comptime evaluation requires an active frame");
        let name = self.host.name_from_symbol(&frame.program, symbol);
        frame.name_bindings.get(&name).cloned().unwrap_or(name)
    }

    pub fn evaluate(
        &mut self,
        frame: ComptimeFrame<
            H::Value,
            H::Type,
            H::Name,
            H::File,
            H::ProgramKey,
            H::CanonicalIdentity,
        >,
        env: &mut ComptimeEnv<'_, H::Value, H::Type, H::Name, H::File, H::CanonicalIdentity>,
    ) -> ComptimeOutcome<H::Value, H::Failure> {
        // Public expression evaluation is intentionally ticket-free. Named
        // frames may only enter through the admitted-call path, after depth
        // and canonical-producer checks have issued their mandatory ticket.
        if frame.name.is_some() {
            return ComptimeOutcome::UnsupportedContext;
        }
        let body = frame.body;
        let previous_expected = env.expected_result.clone();
        env.expected_result = frame.expected_result.clone();
        self.frames.push(frame);
        let result = self.eval(body, env);
        self.frames.pop();
        env.expected_result = previous_expected;
        result
    }

    /// Evaluate only an if selector and return the canonical selection fact.
    /// This is used by staged inference so selector evaluation has exactly the
    /// same typed arithmetic and diagnostic behavior as semantic analysis.
    pub fn select_branch(
        &mut self,
        program: H::ProgramKey,
        condition: InstRef,
        env: &mut ComptimeEnv<'_, H::Value, H::Type, H::Name, H::File, H::CanonicalIdentity>,
    ) -> ComptimeOutcome<ComptimeSelection, H::Failure> {
        match self.evaluate(ComptimeFrame::expression(program, condition), env) {
            ComptimeOutcome::Known(value) => match value.as_boolean() {
                Some(taken) => ComptimeOutcome::Known(ComptimeSelection::Branch { taken }),
                None => ComptimeOutcome::RuntimeDependent,
            },
            ComptimeOutcome::RuntimeDependent => ComptimeOutcome::RuntimeDependent,
            ComptimeOutcome::NotReady => ComptimeOutcome::NotReady,
            ComptimeOutcome::UnsupportedContext => ComptimeOutcome::UnsupportedContext,
            ComptimeOutcome::Trap(trap) => ComptimeOutcome::Trap(trap),
            ComptimeOutcome::HostFailure(error) => ComptimeOutcome::HostFailure(error),
            ComptimeOutcome::Abort(error) => ComptimeOutcome::Abort(error),
        }
    }

    /// Evaluate only a match scrutinee and return the first matching source
    /// arm. Pattern decoding and matching remain owned by this engine/host
    /// boundary; callers never recreate the selector policy.
    pub fn select_match(
        &mut self,
        program: H::ProgramKey,
        scrutinee: InstRef,
        arms: &rue_rir::RirMatchArmsRange,
        env: &mut ComptimeEnv<'_, H::Value, H::Type, H::Name, H::File, H::CanonicalIdentity>,
    ) -> ComptimeOutcome<ComptimeSelection, H::Failure> {
        let frame = ComptimeFrame::expression(program, scrutinee);
        let body = frame.body;
        let previous_expected = env.expected_result.clone();
        env.expected_result = frame.expected_result.clone();
        self.frames.push(frame);
        let value = self.eval(body, env);
        let selected = match value {
            ComptimeOutcome::Known(value) => {
                let patterns = self.program_rir().match_arms(arms).to_vec();
                patterns
                    .into_iter()
                    .enumerate()
                    .find_map(|(index, (pattern, _))| {
                        let pattern = self.decode_match_pattern(&self.program_key(), &pattern);
                        match self.host.match_pattern(&pattern, &value) {
                            Some(true) => Some(ComptimeOutcome::Known(ComptimeSelection::Match {
                                arm: index,
                            })),
                            Some(false) => None,
                            None => Some(ComptimeOutcome::RuntimeDependent),
                        }
                    })
                    .unwrap_or(ComptimeOutcome::RuntimeDependent)
            }
            ComptimeOutcome::RuntimeDependent => ComptimeOutcome::RuntimeDependent,
            ComptimeOutcome::NotReady => ComptimeOutcome::NotReady,
            ComptimeOutcome::UnsupportedContext => ComptimeOutcome::UnsupportedContext,
            ComptimeOutcome::Trap(trap) => ComptimeOutcome::Trap(trap),
            ComptimeOutcome::HostFailure(error) => ComptimeOutcome::HostFailure(error),
            ComptimeOutcome::Abort(error) => ComptimeOutcome::Abort(error),
        };
        self.frames.pop();
        env.expected_result = previous_expected;
        selected
    }

    /// Evaluate a named call through a child call. The body host receives
    /// only the semantically named call operation; recursive expression edges
    /// stay in this engine.
    #[inline(never)]
    fn evaluate_call(
        &mut self,
        name: H::Name,
        args: &rue_rir::RirCallArgsRange,
        env: &mut ComptimeEnv<'_, H::Value, H::Type, H::Name, H::File, H::CanonicalIdentity>,
        span: Span,
    ) -> ComptimeOutcome<H::Value, H::Failure> {
        let args = self.program_rir().call_args(args).to_vec();
        let arg_modes: Vec<ComptimeArgMode> = args
            .iter()
            .map(|arg| (arg.mode, self.program_rir().get(arg.value).span))
            .collect();
        let admission =
            host_value!(
                self.host
                    .admit_comptime_call(name, args.len(), &arg_modes, env, false)
            );
        let Some(admission) = admission else {
            return ComptimeOutcome::RuntimeDependent;
        };
        let mut binding = host_value!(self.host.begin_comptime_call_binding(
            &admission,
            args.len(),
            span,
        ));
        outcome_value!(self.evaluate_call_arguments(&args, env, &mut binding, span));
        let bound = host_value!(self.host.finish_comptime_call_binding(binding, span));
        let Some(bound) = bound else {
            return ComptimeOutcome::RuntimeDependent;
        };
        let preparation = host_value!(self.host.prepare_comptime_call(admission, bound, span));
        let Some(preparation) = preparation else {
            return ComptimeOutcome::RuntimeDependent;
        };
        match preparation {
            ComptimeCallPreparation::Memoized(outcome) => outcome,
            ComptimeCallPreparation::Enter { frame, ticket } => {
                self.enter_prepared_call(frame, ticket, span)
            }
        }
    }

    /// Reduce call arguments in source order while retaining only the
    /// engine-derived provenance needed by semantic binding. Each child is
    /// reduced before its source node is inspected, while the owning program
    /// key is retained across foreign-frame evaluation.
    #[inline(never)]
    fn evaluate_call_arguments(
        &mut self,
        args: &[rue_rir::RirCallArg],
        env: &mut ComptimeEnv<'_, H::Value, H::Type, H::Name, H::File, H::CanonicalIdentity>,
        binding: &mut H::CallBinding,
        span: Span,
    ) -> ComptimeOutcome<(), H::Failure> {
        for (index, arg) in args.iter().enumerate() {
            let program = self.program_key();
            let value = outcome_value!(self.eval(arg.value, env));
            let direct_unit_literal = matches!(
                &self.host.program_rir(&program).get(arg.value).data,
                InstData::UnitConst
            );
            #[cfg(test)]
            {
                self.provenance_classifications += 1;
            }
            let accepted = host_value!(self.host.bind_comptime_call_argument(
                binding,
                ComptimeCallArgument::new(value, direct_unit_literal),
                index,
                span,
            ));
            if !accepted {
                return ComptimeOutcome::RuntimeDependent;
            }
        }
        ComptimeOutcome::Known(())
    }

    #[inline(never)]
    fn enter_prepared_call(
        &mut self,
        frame: ComptimeFrame<
            H::Value,
            H::Type,
            H::Name,
            H::File,
            H::ProgramKey,
            H::CanonicalIdentity,
        >,
        ticket: H::CompletionTicket,
        call_span: Span,
    ) -> ComptimeOutcome<H::Value, H::Failure> {
        // Root expression frames are intentionally ticket-free. A host must
        // not be able to smuggle one through Enter and silently bypass the
        // enter/finish lifecycle.
        if frame.name.is_none() {
            return ComptimeOutcome::UnsupportedContext;
        }
        self.enter_call(frame, ticket, call_span)
    }

    #[inline(never)]
    fn enter_call(
        &mut self,
        frame: ComptimeFrame<
            H::Value,
            H::Type,
            H::Name,
            H::File,
            H::ProgramKey,
            H::CanonicalIdentity,
        >,
        ticket: H::CompletionTicket,
        call_span: Span,
    ) -> ComptimeOutcome<H::Value, H::Failure> {
        self.run_frame(frame, ticket, call_span)
    }

    pub fn evaluate_frame(
        &mut self,
        frame: ComptimeFrame<
            H::Value,
            H::Type,
            H::Name,
            H::File,
            H::ProgramKey,
            H::CanonicalIdentity,
        >,
    ) -> ComptimeOutcome<H::Value, H::Failure> {
        let mut env = ComptimeEnv::with_subst(&frame.type_bindings, &frame.value_bindings);
        self.evaluate(frame, &mut env)
    }

    /// Evaluate an owned frame admitted by this engine's host on the current
    /// engine stack. The caller must pass the exact non-replayable completion
    /// ticket returned with the frame by `prepare_comptime_call`; this entry
    /// point never creates a child engine or dispatches a peer RIR walker.
    pub(crate) fn evaluate_entered_frame(
        &mut self,
        frame: ComptimeFrame<
            H::Value,
            H::Type,
            H::Name,
            H::File,
            H::ProgramKey,
            H::CanonicalIdentity,
        >,
        ticket: H::CompletionTicket,
    ) -> ComptimeOutcome<H::Value, H::Failure> {
        let span = frame.span;
        self.run_frame(frame, ticket, span)
    }

    #[inline(never)]
    fn run_frame(
        &mut self,
        mut frame: ComptimeFrame<
            H::Value,
            H::Type,
            H::Name,
            H::File,
            H::ProgramKey,
            H::CanonicalIdentity,
        >,
        ticket: H::CompletionTicket,
        call_span: Span,
    ) -> ComptimeOutcome<H::Value, H::Failure> {
        let entered_depth = self
            .frames
            .iter()
            .filter(|frame| frame.name.is_some() || frame.call_identity.is_some())
            .count();
        if frame.name.is_some() && comptime_depth_over_limit(entered_depth) {
            let site = ComptimeDiagnosticSite::new(frame.program.clone(), frame.function_span);
            return ComptimeOutcome::HostFailure(self.host.depth_exceeded(
                frame.name.as_ref().expect("named frame"),
                MAX_COMPTIME_CALL_DEPTH,
                &site,
            ));
        }
        if let Some(name) = frame.name.clone() {
            let canonical_identity = if let Some(identity) = frame.call_identity.clone() {
                identity
            } else {
                host_value!(self.host.canonical_function_producer(
                    &frame.program,
                    &ticket,
                    name,
                    &frame.type_bindings,
                    &frame.value_bindings,
                    frame.span,
                ))
            };
            frame.call_identity = Some(canonical_identity);
            if let Some(value) = host_value!(self.host.lookup_completed_comptime_call(&frame)) {
                return ComptimeOutcome::Known(value);
            }
            // Admission and canonical producer issuance are complete. Only
            // now may a host activate the opaque completion ticket carried by
            // this frame; depth/producer failures above never activate it.
            host_value!(self.host.enter_comptime_call(&frame, &ticket));
        }
        let mut child_env = ComptimeEnv::with_subst(&frame.type_bindings, &frame.value_bindings);
        child_env.canonical_identity = frame.call_identity.clone();
        child_env.defining_file = frame.context.clone();
        child_env.expected_result = frame.expected_result.clone();
        let body = frame.body;
        let is_call = frame.name.is_some();
        self.frames.push(frame);
        let result = self.eval(body, &mut child_env);
        let frame = self.frames.pop().expect("comptime frame stack underflow");
        if is_call {
            let result = match result {
                ComptimeOutcome::HostFailure(error) => {
                    ComptimeOutcome::HostFailure(H::label_ctor_instantiation_site(error, call_span))
                }
                ComptimeOutcome::Abort(error) => ComptimeOutcome::Abort(error),
                other => other,
            };
            self.host.finish_comptime_call(&frame, ticket, result)
        } else {
            result
        }
    }

    fn evaluate_method_call(
        &mut self,
        receiver: InstRef,
        method: H::Name,
        args: &rue_rir::RirCallArgsRange,
        env: &mut ComptimeEnv<'_, H::Value, H::Type, H::Name, H::File, H::CanonicalIdentity>,
        span: Span,
    ) -> ComptimeOutcome<H::Value, H::Failure> {
        let args = self.program_rir().call_args(args).to_vec();
        if matches!(
            self.host.comptime_method_receiver_policy(),
            ComptimeMethodReceiverPolicy::EvaluateReceiver
        ) {
            let receiver = outcome_value!(self.eval(receiver, env));
            let arg_modes: Vec<ComptimeArgMode> = args
                .iter()
                .map(|arg| (arg.mode, self.program_rir().get(arg.value).span))
                .collect();
            let admission = outcome_value!(self.host.admit_evaluated_comptime_method(
                receiver,
                method,
                args.len(),
                &arg_modes,
                env,
                &self.diagnostic_site(span),
                span,
            ));
            let Some(admission) = admission else {
                return ComptimeOutcome::RuntimeDependent;
            };
            return self.evaluate_admitted_call(admission, &args, env, span);
        }

        let decoded = self.decode_module_path(receiver, env);
        let Some((file_id, segments)) = decoded else {
            return ComptimeOutcome::RuntimeDependent;
        };
        let name = host_value!(
            self.host
                .resolve_module_comptime_callable(file_id, &segments, method, span)
        );
        let Some(name) = name else {
            return ComptimeOutcome::RuntimeDependent;
        };
        let arg_modes: Vec<ComptimeArgMode> = args
            .iter()
            .map(|arg| (arg.mode, self.program_rir().get(arg.value).span))
            .collect();
        let admission =
            host_value!(
                self.host
                    .admit_comptime_call(name, args.len(), &arg_modes, env, true)
            );
        let Some(admission) = admission else {
            return ComptimeOutcome::RuntimeDependent;
        };
        self.evaluate_admitted_call(admission, &args, env, span)
    }

    fn evaluate_admitted_call(
        &mut self,
        admission: ComptimeCallAdmission<H::CallAdmission, H::Name>,
        args: &[rue_rir::RirCallArg],
        env: &mut ComptimeEnv<'_, H::Value, H::Type, H::Name, H::File, H::CanonicalIdentity>,
        span: Span,
    ) -> ComptimeOutcome<H::Value, H::Failure> {
        let mut binding = host_value!(self.host.begin_comptime_call_binding(
            &admission,
            args.len(),
            span,
        ));
        outcome_value!(self.evaluate_call_arguments(args, env, &mut binding, span));
        let bound = host_value!(self.host.finish_comptime_call_binding(binding, span));
        let Some(bound) = bound else {
            return ComptimeOutcome::RuntimeDependent;
        };
        let preparation = host_value!(self.host.prepare_comptime_call(admission, bound, span));
        let Some(preparation) = preparation else {
            return ComptimeOutcome::RuntimeDependent;
        };
        match preparation {
            ComptimeCallPreparation::Memoized(outcome) => outcome,
            ComptimeCallPreparation::Enter { frame, ticket } => {
                self.enter_prepared_call(frame, ticket, span)
            }
        }
    }

    /// Decode only the syntactic module path for a method call. Resolution of
    /// the path's declarations and visibility stays in the semantic host; the
    /// engine owns this RIR edge so hosts never need to inspect child
    /// instructions to discover a callable.
    fn decode_module_path(
        &self,
        receiver: InstRef,
        env: &ComptimeEnv<'_, H::Value, H::Type, H::Name, H::File, H::CanonicalIdentity>,
    ) -> Option<(H::File, Vec<H::Name>)> {
        let mut chain_rev = Vec::new();
        let mut cursor = receiver;
        let root = loop {
            match self.program_rir().get(cursor).data {
                InstData::VarRef { name, .. } => break self.name_from_rir(name.into()),
                InstData::FieldGet { base, field } => {
                    chain_rev.push(self.name_from_rir(field.into()));
                    cursor = base;
                }
                _ => return None,
            }
        };
        if env.locals.contains_key(&root)
            || env.is_runtime_local_name(&root)
            || env.runtime_binding_names.contains(&root)
            || env.type_subst.contains_key(&root)
            || env.value_subst.contains_key(&root)
        {
            return None;
        }
        let file_id = env.defining_file.clone()?;
        chain_rev.reverse();
        let mut segments = Vec::with_capacity(chain_rev.len() + 1);
        segments.push(root);
        segments.extend(chain_rev);
        Some((file_id, segments))
    }

    /// Decode a dotted type path before crossing the host boundary. The host
    /// receives only copied semantic path facts; it never needs to inspect the
    /// RIR spine or an evaluation environment to decide whether this is a
    /// module/type path.
    fn decode_type_path(
        &self,
        inst_ref: InstRef,
        env: &ComptimeEnv<'_, H::Value, H::Type, H::Name, H::File, H::CanonicalIdentity>,
    ) -> Option<(H::File, Vec<H::Name>)> {
        let mut chain_rev = Vec::new();
        let mut cursor = inst_ref;
        let root = loop {
            match self.program_rir().get(cursor).data {
                InstData::VarRef { name, .. } => break self.name_from_rir(name.into()),
                InstData::FieldGet { base, field } => {
                    chain_rev.push(self.name_from_rir(field.into()));
                    cursor = base;
                }
                _ => return None,
            }
        };
        if env.locals.contains_key(&root)
            || env.is_runtime_local_name(&root)
            || env.runtime_binding_names.contains(&root)
            || env.type_subst.contains_key(&root)
            || env.value_subst.contains_key(&root)
        {
            return None;
        }
        let file_id = env.defining_file.clone()?;
        chain_rev.reverse();
        let mut segments = Vec::with_capacity(chain_rev.len() + 1);
        segments.push(root);
        segments.extend(chain_rev);
        Some((file_id, segments))
    }

    fn eval_int_operands(
        &mut self,
        operation: ComptimeIntegerOperation,
        lhs: InstRef,
        rhs: InstRef,
        env: &mut ComptimeEnv<'_, H::Value, H::Type, H::Name, H::File, H::CanonicalIdentity>,
        span: Span,
    ) -> ComptimeOutcome<(H::Value, H::Value), H::Failure> {
        let l = match self.eval(lhs, env) {
            ComptimeOutcome::Known(value) => value,
            other => return Self::discard_rejection(other),
        };
        if l.as_integer().is_none() && !self.host.evaluate_binary_rhs_after_rejection() {
            return Self::discard_rejection(self.host.reject_comptime_expression(
                ComptimeSemanticRejection::ArithmeticOperandNotInteger {
                    operation,
                    lhs: l,
                    rhs: None,
                },
                &self.diagnostic_site(span),
            ));
        }
        let r = match self.eval(rhs, env) {
            ComptimeOutcome::Known(value) => value,
            other => return Self::discard_rejection(other),
        };
        if l.as_integer().is_none() || r.as_integer().is_none() {
            return Self::discard_rejection(self.host.reject_comptime_expression(
                ComptimeSemanticRejection::ArithmeticOperandNotInteger {
                    operation,
                    lhs: l,
                    rhs: Some(r),
                },
                &self.diagnostic_site(span),
            ));
        }
        ComptimeOutcome::Known((l, r))
    }

    fn integer_pair(values: &(H::Value, H::Value)) -> Option<(i128, i128)> {
        Some((values.0.as_integer()?, values.1.as_integer()?))
    }

    fn discard_rejection<T>(
        outcome: ComptimeOutcome<H::Value, H::Failure>,
    ) -> ComptimeOutcome<T, H::Failure> {
        match outcome {
            ComptimeOutcome::Known(_) => ComptimeOutcome::RuntimeDependent,
            ComptimeOutcome::RuntimeDependent => ComptimeOutcome::RuntimeDependent,
            ComptimeOutcome::NotReady => ComptimeOutcome::NotReady,
            ComptimeOutcome::UnsupportedContext => ComptimeOutcome::UnsupportedContext,
            ComptimeOutcome::Trap(trap) => ComptimeOutcome::Trap(trap),
            ComptimeOutcome::HostFailure(error) => ComptimeOutcome::HostFailure(error),
            ComptimeOutcome::Abort(error) => ComptimeOutcome::Abort(error),
        }
    }

    fn integer_type_for(
        &mut self,
        env: &ComptimeEnv<'_, H::Value, H::Type, H::Name, H::File, H::CanonicalIdentity>,
        inst_ref: InstRef,
        lhs: &H::Value,
        rhs: &H::Value,
        span: Span,
    ) -> ComptimeOutcome<Option<H::Type>, H::Failure> {
        // A declared/substituted parameter type is an explicit contract for
        // this evaluation.  It must win over the probe's provisional i32
        // expression type (notably for `use(Id(i8), 1 << 8)`).
        let hint = env
            .expected_result
            .as_ref()
            .filter(|ty| self.host.type_integer_semantics(ty).is_some())
            .cloned()
            .or_else(|| {
                self.host
                    .const_expr_type(&self.program_key(), env, inst_ref)
            });
        let site = self.diagnostic_site(span);
        ComptimeOutcome::Known(host_value!(self.host.integer_operation_type(
            hint.as_ref(),
            lhs,
            rhs,
            &site,
        )))
    }

    fn unary_integer_type_for(
        &mut self,
        env: &ComptimeEnv<'_, H::Value, H::Type, H::Name, H::File, H::CanonicalIdentity>,
        inst_ref: InstRef,
        operand: &H::Value,
        span: Span,
    ) -> ComptimeOutcome<Option<H::Type>, H::Failure> {
        let hint = env
            .expected_result
            .as_ref()
            .filter(|ty| self.host.type_integer_semantics(ty).is_some())
            .cloned()
            .or_else(|| {
                self.host
                    .const_expr_type(&self.program_key(), env, inst_ref)
            });
        let site = self.diagnostic_site(span);
        ComptimeOutcome::Known(host_value!(self.host.unary_integer_type(
            hint.as_ref(),
            operand,
            &site,
        )))
    }

    fn finish_arith_value(
        &mut self,
        result: CheckedIntegerResult,
        ty: Option<H::Type>,
        op: &str,
        span: Span,
    ) -> ComptimeOutcome<H::Value, H::Failure> {
        let site = self.diagnostic_site(span);
        let Some(value) = host_value!(self.host.finish_arith(result, ty, op, &site)) else {
            return ComptimeOutcome::RuntimeDependent;
        };
        ComptimeOutcome::Known(value)
    }

    /// Keep recursive control-flow and call edges out of the large instruction
    /// dispatcher stack frame. This small trampoline is important for the
    /// shared depth boundary: a deeply recursive comptime call must reach the
    /// engine's 48-frame check before the dispatcher itself exhausts the host
    /// thread stack.
    #[inline(never)]
    fn eval(
        &mut self,
        inst_ref: InstRef,
        env: &mut ComptimeEnv<'_, H::Value, H::Type, H::Name, H::File, H::CanonicalIdentity>,
    ) -> ComptimeOutcome<H::Value, H::Failure> {
        host_value!(self.host.check_canceled());
        let (data, span) = {
            let source = self.program_rir().get(inst_ref);
            (source.data.clone(), source.span)
        };
        match data {
            InstData::Call { name, args } => {
                let name = self.name_from_rir(name.into());
                self.evaluate_call(name, &args, env, span)
            }
            InstData::Comptime { expr } => self.eval(expr, env),
            InstData::Block { instructions } => self.eval_block(instructions, env, span),
            InstData::Branch {
                cond,
                then_block,
                else_block,
            } => self.eval_branch(cond, then_block, else_block, env),
            _ => self.eval_dispatch(inst_ref, env),
        }
    }

    #[inline(never)]
    fn eval_block(
        &mut self,
        instructions: rue_rir::RirBlockInstsRange,
        env: &mut ComptimeEnv<'_, H::Value, H::Type, H::Name, H::File, H::CanonicalIdentity>,
        span: Span,
    ) -> ComptimeOutcome<H::Value, H::Failure> {
        let stmt_refs = self.program_rir().block_insts(&instructions).to_vec();
        if stmt_refs.is_empty() {
            return self.host.reject_comptime_expression(
                ComptimeSemanticRejection::EmptyBlock,
                &self.diagnostic_site(span),
            );
        }
        let saved_locals = env.locals.clone();
        let mut result = H::Value::unit();
        for (i, stmt_ref) in stmt_refs.iter().copied().enumerate() {
            let is_tail = i + 1 == stmt_refs.len();
            if !is_tail
                && matches!(
                    self.program_rir().get(stmt_ref).data,
                    InstData::Assign { .. }
                )
            {
                env.locals = saved_locals;
                return self.host.reject_comptime_expression(
                    ComptimeSemanticRejection::Assignment,
                    &self.diagnostic_site(self.program_rir().get(stmt_ref).span),
                );
            }
            let value = if let InstData::Alloc { name, init, .. } =
                &self.program_rir().get(stmt_ref).data
            {
                let name = name.map(|name| self.name_from_rir(name.into()));
                let init = *init;
                let value = match self.eval(init, env) {
                    ComptimeOutcome::Known(value) => value,
                    other => {
                        env.locals = saved_locals;
                        return other;
                    }
                };
                if let Some(name) = name {
                    env.locals.insert(name, value);
                }
                H::Value::unit()
            } else {
                match self.eval(stmt_ref, env) {
                    ComptimeOutcome::Known(value) => value,
                    other => {
                        env.locals = saved_locals;
                        return other;
                    }
                }
            };
            if is_tail {
                result = value;
            }
        }
        env.locals = saved_locals;
        ComptimeOutcome::Known(result)
    }

    #[inline(never)]
    fn eval_branch(
        &mut self,
        cond: InstRef,
        then_block: InstRef,
        else_block: Option<InstRef>,
        env: &mut ComptimeEnv<'_, H::Value, H::Type, H::Name, H::File, H::CanonicalIdentity>,
    ) -> ComptimeOutcome<H::Value, H::Failure> {
        match self.eval(cond, env) {
            ComptimeOutcome::Known(value) if value.as_boolean() == Some(true) => {
                self.eval(then_block, env)
            }
            ComptimeOutcome::Known(value) if value.as_boolean() == Some(false) => {
                match else_block {
                    Some(else_block) => self.eval(else_block, env),
                    None => ComptimeOutcome::Known(H::Value::unit()),
                }
            }
            ComptimeOutcome::Known(value) => self.host.reject_comptime_expression(
                ComptimeSemanticRejection::ConditionNotBoolean(value),
                &self.diagnostic_site(self.program_rir().get(cond).span),
            ),
            other => other,
        }
    }

    /// The single compile-time evaluation engine. See the module docs for the
    /// result encoding is a typed `ComptimeOutcome`; no recursive edge is
    /// collapsed into a legacy optional result inside the engine.
    #[inline(never)]
    fn eval_dispatch(
        &mut self,
        inst_ref: InstRef,
        env: &mut ComptimeEnv<'_, H::Value, H::Type, H::Name, H::File, H::CanonicalIdentity>,
    ) -> ComptimeOutcome<H::Value, H::Failure> {
        let inst = {
            let source = self.program_rir().get(inst_ref);
            rue_rir::Inst {
                data: source.data.clone(),
                span: source.span,
            }
        };
        let span = inst.span;
        match &inst.data {
            // Integer literals. The literal itself must fit its resolved type
            // (the inner expression of a comptime block never goes through
            // `analyze_literal`, so this is where `300` at type u8 is caught).
            InstData::IntConst(value) => {
                let v = *value as i128;
                let ty = self
                    .host
                    .const_expr_type(&self.program_key(), env, inst_ref);
                if let Some(ty) = &ty {
                    if !self
                        .host
                        .type_integer_semantics(ty)
                        .is_some_and(|integer| integer.fits_i128(v))
                    {
                        return ComptimeOutcome::HostFailure(self.host.literal_out_of_range(
                            *value,
                            ty,
                            &self.diagnostic_site(span),
                        ));
                    }
                }
                ComptimeOutcome::Known(H::Value::integer_typed(v, ty))
            }

            // Float literals stop here for the same reason they stop in
            // `analyze_inst_dispatch` (ADR-0065, RUE-1069): there is no
            // `comptime_float` value in the host's value domain yet. Naming the real
            // reason matters more here than elsewhere — falling through to
            // the generic "not knowable at compile time" would be actively
            // wrong about a literal, which is the most compile-time-knowable
            // thing there is. Delete this arm when Phase 4 lands.
            InstData::FloatConst { .. } => {
                host_value!(self.host.require_preview(
                    rue_error::PreviewFeature::Floats,
                    "a floating-point literal",
                    &self.diagnostic_site(span),
                ));
                ComptimeOutcome::HostFailure(
                    self.host.float_not_implemented(&self.diagnostic_site(span)),
                )
            }

            // String constants are intentionally routed through the host:
            // they are not part of the ordinary four-value comptime algebra,
            // but durable declaration evaluation needs their semantic spelling
            // for controls such as `@import`. The host sees only the name; the
            // engine still owns this instruction dispatch.
            InstData::StringConst { content, .. } => self
                .host
                .resolve_string_const(self.name_from_rir((*content).into()), span),

            // Boolean literals
            InstData::BoolConst(value) => ComptimeOutcome::Known(H::Value::boolean(*value)),

            // Unit literal
            InstData::UnitConst => ComptimeOutcome::Known(H::Value::unit()),

            // Unary negation: -expr
            InstData::Neg { operand } => {
                if let InstData::IntConst(magnitude) = self.program_rir().get(*operand).data {
                    let literal = H::Value::integer(magnitude as i128);
                    let ty =
                        outcome_value!(self.unary_integer_type_for(env, inst_ref, &literal, span,));
                    if let Some(ref ty) = ty {
                        if self.host.type_is_unsigned(ty) {
                            if let Some(failure) = self
                                .host
                                .reject_unsigned_negation(ty, &self.diagnostic_site(span))
                            {
                                return ComptimeOutcome::HostFailure(failure);
                            }
                        }
                    }
                    // The literal path uses mathematical magnitude semantics:
                    // unlike an ordinary runtime value, `128` must not first
                    // canonicalize to -128 before becoming `-128`.
                    let result = ty
                        .as_ref()
                        .and_then(|ty| self.host.type_integer_semantics(ty))
                        .map_or_else(
                            || CheckedIntegerResult::from_raw((magnitude as i128).checked_neg()),
                            |integer| integer.checked_neg_literal_report_i128(magnitude as i128),
                        );
                    self.finish_arith_value(result, ty, "negation", span)
                } else {
                    match self.eval(*operand, env) {
                        ComptimeOutcome::Known(value) => {
                            let Some(n) = value.as_integer() else {
                                return self.host.reject_comptime_expression(
                                    ComptimeSemanticRejection::UnaryOperandNotInteger(value),
                                    &self.diagnostic_site(span),
                                );
                            };
                            let ty = outcome_value!(
                                self.unary_integer_type_for(env, inst_ref, &value, span,)
                            );
                            if let Some(ref ty) = ty {
                                if self.host.type_is_unsigned(ty) {
                                    if let Some(failure) = self
                                        .host
                                        .reject_unsigned_negation(ty, &self.diagnostic_site(span))
                                    {
                                        return ComptimeOutcome::HostFailure(failure);
                                    }
                                }
                            }
                            let result = match ty
                                .as_ref()
                                .and_then(|ty| self.host.type_integer_semantics(ty))
                            {
                                Some(integer) => integer.checked_neg_report_i128(n),
                                None if ty.is_some() => {
                                    return self.host.reject_comptime_expression(
                                        ComptimeSemanticRejection::UnaryTypeNotInteger {
                                            operation: ComptimeUnaryOperation::Neg,
                                            value,
                                        },
                                        &self.diagnostic_site(span),
                                    );
                                }
                                None => CheckedIntegerResult::from_raw(n.checked_neg()),
                            };
                            self.finish_arith_value(result, ty, "negation", span)
                        }
                        other => other,
                    }
                }
            }

            // Logical NOT: !expr
            InstData::Not { operand } => {
                match self.eval(*operand, env) {
                    ComptimeOutcome::Known(value) => match value.as_boolean() {
                        Some(b) => ComptimeOutcome::Known(H::Value::boolean(!b)),
                        None => self.host.reject_comptime_expression(
                            ComptimeSemanticRejection::ConditionNotBoolean(value),
                            &self.diagnostic_site(span),
                        ),
                    },
                    // Can't logical-NOT an integer, type, or unit
                    other => other,
                }
            }

            // Binary arithmetic operations, checked at the operand type
            InstData::Add { lhs, rhs } => {
                let operands = outcome_value!(self.eval_int_operands(
                    ComptimeIntegerOperation::Add,
                    *lhs,
                    *rhs,
                    env,
                    span
                ));
                let (l, r) = Self::integer_pair(&operands).expect("integer operands");
                let ty = outcome_value!(self.integer_type_for(
                    env,
                    inst_ref,
                    &operands.0,
                    &operands.1,
                    span,
                ));
                let result = ty
                    .as_ref()
                    .and_then(|ty| self.host.type_integer_semantics(ty))
                    .map_or_else(
                        || CheckedIntegerResult::from_raw(l.checked_add(r)),
                        |integer| integer.checked_add_report_i128(l, r),
                    );
                self.finish_arith_value(result, ty, "+", span)
            }
            InstData::Sub { lhs, rhs } => {
                let operands = outcome_value!(self.eval_int_operands(
                    ComptimeIntegerOperation::Sub,
                    *lhs,
                    *rhs,
                    env,
                    span
                ));
                let (l, r) = Self::integer_pair(&operands).expect("integer operands");
                let ty = outcome_value!(self.integer_type_for(
                    env,
                    inst_ref,
                    &operands.0,
                    &operands.1,
                    span,
                ));
                let result = ty
                    .as_ref()
                    .and_then(|ty| self.host.type_integer_semantics(ty))
                    .map_or_else(
                        || CheckedIntegerResult::from_raw(l.checked_sub(r)),
                        |integer| integer.checked_sub_report_i128(l, r),
                    );
                self.finish_arith_value(result, ty, "-", span)
            }
            InstData::Mul { lhs, rhs } => {
                let operands = outcome_value!(self.eval_int_operands(
                    ComptimeIntegerOperation::Mul,
                    *lhs,
                    *rhs,
                    env,
                    span
                ));
                let (l, r) = Self::integer_pair(&operands).expect("integer operands");
                let ty = outcome_value!(self.integer_type_for(
                    env,
                    inst_ref,
                    &operands.0,
                    &operands.1,
                    span,
                ));
                let result = ty
                    .as_ref()
                    .and_then(|ty| self.host.type_integer_semantics(ty))
                    .map_or_else(
                        || CheckedIntegerResult::from_raw(l.checked_mul(r)),
                        |integer| integer.checked_mul_report_i128(l, r),
                    );
                self.finish_arith_value(result, ty, "*", span)
            }
            InstData::Div { lhs, rhs } | InstData::Mod { lhs, rhs } => {
                let is_div = matches!(&inst.data, InstData::Div { .. });
                let op = if is_div { "/" } else { "%" };
                let operands = outcome_value!(self.eval_int_operands(
                    if is_div {
                        ComptimeIntegerOperation::Div
                    } else {
                        ComptimeIntegerOperation::Mod
                    },
                    *lhs,
                    *rhs,
                    env,
                    span
                ));
                let (l, r) = Self::integer_pair(&operands).expect("integer operands");
                let ty = outcome_value!(self.integer_type_for(
                    env,
                    inst_ref,
                    &operands.0,
                    &operands.1,
                    span,
                ));
                if r == 0 {
                    return match ty {
                        Some(_) => ComptimeOutcome::Trap(ComptimeTrap {
                            operation: if is_div {
                                "division by zero"
                            } else {
                                "remainder by zero"
                            },
                            span,
                        }),
                        // Untyped fallback: defer to the runtime check.
                        None => ComptimeOutcome::RuntimeDependent,
                    };
                }
                // Untyped evaluation retains its historical i64 fallback;
                // typed MIN / -1 trapping is owned by the kernel report.
                if r == -1 && ty.is_none() && l == i128::from(i64::MIN) {
                    return ComptimeOutcome::RuntimeDependent;
                }
                let result = ty
                    .as_ref()
                    .and_then(|ty| self.host.type_integer_semantics(ty))
                    .map_or_else(
                        || {
                            CheckedIntegerResult::from_raw(if is_div {
                                l.checked_div(r)
                            } else {
                                l.checked_rem(r)
                            })
                        },
                        |integer| {
                            if is_div {
                                integer.checked_div_report_i128(l, r)
                            } else {
                                integer.checked_rem_report_i128(l, r)
                            }
                        },
                    );
                self.finish_arith_value(result, ty, op, span)
            }

            // Comparison operations
            InstData::Eq { lhs, rhs } => {
                let lhs = match self.eval(*lhs, env) {
                    ComptimeOutcome::Known(value) => value,
                    ComptimeOutcome::RuntimeDependent => {
                        return match self.eval(*rhs, env) {
                            ComptimeOutcome::Known(_) | ComptimeOutcome::RuntimeDependent => {
                                ComptimeOutcome::RuntimeDependent
                            }
                            other => other,
                        };
                    }
                    other => return other,
                };
                match self.eval(*rhs, env) {
                    ComptimeOutcome::Known(rhs) => {
                        if lhs.as_integer().is_some() && rhs.as_integer().is_some() {
                            let _ = outcome_value!(
                                self.integer_type_for(env, inst_ref, &lhs, &rhs, span,)
                            );
                        }
                        match (
                            lhs.as_integer(),
                            rhs.as_integer(),
                            lhs.as_boolean(),
                            rhs.as_boolean(),
                        ) {
                            (Some(lhs), Some(rhs), _, _) => {
                                ComptimeOutcome::Known(H::Value::boolean(lhs == rhs))
                            }
                            (_, _, Some(lhs), Some(rhs)) => {
                                ComptimeOutcome::Known(H::Value::boolean(lhs == rhs))
                            }
                            _ => {
                                let site = self.diagnostic_site(span);
                                self.host.compare_comptime_values(&lhs, &rhs, true, &site)
                            }
                        }
                    }
                    other => other,
                }
            }
            InstData::Ne { lhs, rhs } => {
                let lhs = match self.eval(*lhs, env) {
                    ComptimeOutcome::Known(value) => value,
                    ComptimeOutcome::RuntimeDependent => {
                        return match self.eval(*rhs, env) {
                            ComptimeOutcome::Known(_) | ComptimeOutcome::RuntimeDependent => {
                                ComptimeOutcome::RuntimeDependent
                            }
                            other => other,
                        };
                    }
                    other => return other,
                };
                match self.eval(*rhs, env) {
                    ComptimeOutcome::Known(rhs) => {
                        if lhs.as_integer().is_some() && rhs.as_integer().is_some() {
                            let _ = outcome_value!(
                                self.integer_type_for(env, inst_ref, &lhs, &rhs, span,)
                            );
                        }
                        match (
                            lhs.as_integer(),
                            rhs.as_integer(),
                            lhs.as_boolean(),
                            rhs.as_boolean(),
                        ) {
                            (Some(lhs), Some(rhs), _, _) => {
                                ComptimeOutcome::Known(H::Value::boolean(lhs != rhs))
                            }
                            (_, _, Some(lhs), Some(rhs)) => {
                                ComptimeOutcome::Known(H::Value::boolean(lhs != rhs))
                            }
                            _ => {
                                let site = self.diagnostic_site(span);
                                self.host.compare_comptime_values(&lhs, &rhs, false, &site)
                            }
                        }
                    }
                    other => other,
                }
            }
            InstData::Lt { lhs, rhs } => {
                let operands = outcome_value!(self.eval_int_operands(
                    ComptimeIntegerOperation::Lt,
                    *lhs,
                    *rhs,
                    env,
                    span
                ));
                let (l, r) = Self::integer_pair(&operands).expect("integer operands");
                let _ = outcome_value!(self.integer_type_for(
                    env,
                    inst_ref,
                    &operands.0,
                    &operands.1,
                    span,
                ));
                ComptimeOutcome::Known(H::Value::boolean(l < r))
            }
            InstData::Gt { lhs, rhs } => {
                let operands = outcome_value!(self.eval_int_operands(
                    ComptimeIntegerOperation::Gt,
                    *lhs,
                    *rhs,
                    env,
                    span
                ));
                let (l, r) = Self::integer_pair(&operands).expect("integer operands");
                let _ = outcome_value!(self.integer_type_for(
                    env,
                    inst_ref,
                    &operands.0,
                    &operands.1,
                    span,
                ));
                ComptimeOutcome::Known(H::Value::boolean(l > r))
            }
            InstData::Le { lhs, rhs } => {
                let operands = outcome_value!(self.eval_int_operands(
                    ComptimeIntegerOperation::Le,
                    *lhs,
                    *rhs,
                    env,
                    span
                ));
                let (l, r) = Self::integer_pair(&operands).expect("integer operands");
                let _ = outcome_value!(self.integer_type_for(
                    env,
                    inst_ref,
                    &operands.0,
                    &operands.1,
                    span,
                ));
                ComptimeOutcome::Known(H::Value::boolean(l <= r))
            }
            InstData::Ge { lhs, rhs } => {
                let operands = outcome_value!(self.eval_int_operands(
                    ComptimeIntegerOperation::Ge,
                    *lhs,
                    *rhs,
                    env,
                    span
                ));
                let (l, r) = Self::integer_pair(&operands).expect("integer operands");
                let _ = outcome_value!(self.integer_type_for(
                    env,
                    inst_ref,
                    &operands.0,
                    &operands.1,
                    span,
                ));
                ComptimeOutcome::Known(H::Value::boolean(l >= r))
            }

            // Logical operations: short-circuit like the runtime, so a
            // non-constant (or would-panic) RHS is irrelevant when the LHS
            // already decides the result.
            InstData::And { lhs, rhs } => match self.eval(*lhs, env) {
                ComptimeOutcome::Known(value) if value.as_boolean() == Some(false) => {
                    ComptimeOutcome::Known(H::Value::boolean(false))
                }
                ComptimeOutcome::Known(value) if value.as_boolean() == Some(true) => {
                    match self.eval(*rhs, env) {
                        ComptimeOutcome::Known(value) if value.as_boolean().is_some() => {
                            ComptimeOutcome::Known(value)
                        }
                        ComptimeOutcome::Known(value) => self.host.reject_comptime_expression(
                            ComptimeSemanticRejection::ConditionNotBoolean(value),
                            &self.diagnostic_site(span),
                        ),
                        other => other,
                    }
                }
                ComptimeOutcome::Known(value) => self.host.reject_comptime_expression(
                    ComptimeSemanticRejection::ConditionNotBoolean(value),
                    &self.diagnostic_site(span),
                ),
                other => other,
            },
            InstData::Or { lhs, rhs } => match self.eval(*lhs, env) {
                ComptimeOutcome::Known(value) if value.as_boolean() == Some(true) => {
                    ComptimeOutcome::Known(H::Value::boolean(true))
                }
                ComptimeOutcome::Known(value) if value.as_boolean() == Some(false) => {
                    match self.eval(*rhs, env) {
                        ComptimeOutcome::Known(value) if value.as_boolean().is_some() => {
                            ComptimeOutcome::Known(value)
                        }
                        ComptimeOutcome::Known(value) => self.host.reject_comptime_expression(
                            ComptimeSemanticRejection::ConditionNotBoolean(value),
                            &self.diagnostic_site(span),
                        ),
                        other => other,
                    }
                }
                ComptimeOutcome::Known(value) => self.host.reject_comptime_expression(
                    ComptimeSemanticRejection::ConditionNotBoolean(value),
                    &self.diagnostic_site(span),
                ),
                other => other,
            },

            // Bitwise operations. For values in range of their type these are
            // closed (no overflow possible), so no range check is needed.
            InstData::BitAnd { lhs, rhs } => {
                let operands = outcome_value!(self.eval_int_operands(
                    ComptimeIntegerOperation::BitAnd,
                    *lhs,
                    *rhs,
                    env,
                    span
                ));
                let (l, r) = Self::integer_pair(&operands).expect("integer operands");
                let ty = outcome_value!(self.integer_type_for(
                    env,
                    inst_ref,
                    &operands.0,
                    &operands.1,
                    span,
                ));
                ComptimeOutcome::Known(H::Value::integer_typed(l & r, ty))
            }
            InstData::BitOr { lhs, rhs } => {
                let operands = outcome_value!(self.eval_int_operands(
                    ComptimeIntegerOperation::BitOr,
                    *lhs,
                    *rhs,
                    env,
                    span
                ));
                let (l, r) = Self::integer_pair(&operands).expect("integer operands");
                let ty = outcome_value!(self.integer_type_for(
                    env,
                    inst_ref,
                    &operands.0,
                    &operands.1,
                    span,
                ));
                ComptimeOutcome::Known(H::Value::integer_typed(l | r, ty))
            }
            InstData::BitXor { lhs, rhs } => {
                let operands = outcome_value!(self.eval_int_operands(
                    ComptimeIntegerOperation::BitXor,
                    *lhs,
                    *rhs,
                    env,
                    span
                ));
                let (l, r) = Self::integer_pair(&operands).expect("integer operands");
                let ty = outcome_value!(self.integer_type_for(
                    env,
                    inst_ref,
                    &operands.0,
                    &operands.1,
                    span,
                ));
                ComptimeOutcome::Known(H::Value::integer_typed(l ^ r, ty))
            }

            // Shifts: the amount is masked modulo the bit width and the
            // result truncated to the operand width (spec 4.3a:10), exactly
            // matching the runtime semantics (RUE-29).
            InstData::Shl { lhs, rhs } | InstData::Shr { lhs, rhs } => {
                let is_shl = matches!(&inst.data, InstData::Shl { .. });
                let operands = outcome_value!(self.eval_int_operands(
                    if is_shl {
                        ComptimeIntegerOperation::Shl
                    } else {
                        ComptimeIntegerOperation::Shr
                    },
                    *lhs,
                    *rhs,
                    env,
                    span
                ));
                let (l, r) = Self::integer_pair(&operands).expect("integer operands");
                let ty = outcome_value!(self.integer_type_for(
                    env,
                    inst_ref,
                    &operands.0,
                    &operands.1,
                    span,
                ));
                match ty.as_ref() {
                    Some(ty) => {
                        let Some(integer) = self.host.type_integer_semantics(ty) else {
                            return ComptimeOutcome::RuntimeDependent;
                        };
                        // Two's-complement AND masks negative amounts the same
                        // way the hardware masks the count register.
                        let v = integer.shift_i128(l, r, is_shl);
                        ComptimeOutcome::Known(H::Value::integer_typed(v, Some(ty.clone())))
                    }
                    None => {
                        // Without the operand type the width is unknown, so
                        // only fold amounts < 8 (safe for every width) and
                        // defer the rest to runtime.
                        if !(0..8).contains(&r) {
                            return ComptimeOutcome::RuntimeDependent;
                        }
                        ComptimeOutcome::Known(H::Value::integer_typed(
                            if is_shl { l << r } else { l >> r },
                            None,
                        ))
                    }
                }
            }

            // Bitwise NOT: truncated to the operand width (`~0` as u8 = 255).
            InstData::BitNot { operand } => {
                let n = outcome_value!(self.eval(*operand, env));
                let Some(raw) = n.as_integer() else {
                    return self.host.reject_comptime_expression(
                        ComptimeSemanticRejection::UnaryOperandNotInteger(n),
                        &self.diagnostic_site(span),
                    );
                };
                let ty = outcome_value!(self.unary_integer_type_for(env, inst_ref, &n, span,));
                let v = match ty
                    .as_ref()
                    .and_then(|ty| self.host.type_integer_semantics(ty))
                {
                    Some(integer) => integer.bitnot_i128(raw),
                    None if ty.is_some() => {
                        return self.host.reject_comptime_expression(
                            ComptimeSemanticRejection::UnaryTypeNotInteger {
                                operation: ComptimeUnaryOperation::BitNot,
                                value: n,
                            },
                            &self.diagnostic_site(span),
                        );
                    }
                    None => !raw,
                };
                ComptimeOutcome::Known(H::Value::integer_typed(v, ty))
            }

            // These control-flow and call forms are handled by `eval`'s small
            // trampoline so recursive calls do not retain this large frame.
            InstData::Comptime { .. }
            | InstData::Block { .. }
            | InstData::Branch { .. }
            | InstData::Call { .. } => unreachable!("routed by comptime eval trampoline"),

            // Comptime-known `match`: evaluate the scrutinee, select the first
            // arm whose pattern matches, and reduce to that arm's body value
            // (spec 4.14:19, RUE-262). An enum-variant (`Path`) pattern isn't
            // representable in the host's value domain, and a non-constant scrutinee is
            // not decidable here — both make the `match` non-evaluable.
            InstData::Match { scrutinee, arms } => {
                let scrutinee = *scrutinee;
                let scrut = outcome_value!(self.eval(scrutinee, env));
                let arms = self.program_rir().match_arms(arms).to_vec();
                for (pattern, body) in arms.iter() {
                    let semantic_pattern = self.decode_match_pattern(&self.program_key(), pattern);
                    match self.host.match_pattern(&semantic_pattern, &scrut) {
                        Some(true) => return self.eval(*body, env),
                        Some(false) => continue,
                        // Undecidable pattern (e.g. an enum-variant `Path`
                        // against a non-representable scrutinee): bail out.
                        None => return ComptimeOutcome::RuntimeDependent,
                    }
                }
                self.host.match_no_selected_arm(&self.diagnostic_site(span))
            }

            // Anonymous struct type: evaluate to a comptime type value,
            // resolving field types through the type substitution.
            InstData::AnonStructType {
                fields,
                methods,
                anchor,
            } => {
                let field_decls = self.program_rir().anon_struct_fields(fields).to_vec();

                // Comptime `let` locals in scope participate in field-type
                // resolution (`let Inner = Mk(T); struct { x: Inner }`,
                // RUE-575), alongside the enclosing parameters.
                let (local_type_subst, local_value_subst) = env.substs_with_locals();

                let mut struct_fields = Vec::with_capacity(field_decls.len());
                for (name_sym, type_sym) in field_decls {
                    let field_name = self.name_from_rir(name_sym.into());
                    // Field types resolve through both the type substitution
                    // (`comptime T: type`) and the value substitution
                    // (`comptime N: i32`, so an `[i32; N]` field gets a concrete
                    // length at each specialization; RUE-16).
                    let field_ty = outcome_value!(self.evaluate_comptime_type_syntax(
                        &self.program_key(),
                        type_sym,
                        &local_type_subst,
                        &local_value_subst,
                        span,
                    ));
                    struct_fields.push(ComptimeField {
                        name: field_name,
                        ty: field_ty,
                    });
                }

                // Decode method signatures in the canonical engine. The host
                // receives only resolved semantic descriptors below.
                let method_sigs = outcome_value!(self.decode_anon_method_descriptors(
                    &self.program_key(),
                    methods,
                    &local_type_subst,
                    &local_value_subst,
                ));

                let Some(producer) = env.canonical_identity.clone() else {
                    return ComptimeOutcome::RuntimeDependent;
                };
                let identity = self.host.issue_anonymous_identity(
                    &self.program_key(),
                    ComptimeAnonymousKind::Struct,
                    &producer,
                    anchor,
                );
                let (struct_ty, _is_new) = host_value!(self.host.find_or_create_anon_struct(
                    identity,
                    &struct_fields,
                    &method_sigs,
                    &local_type_subst,
                    &local_value_subst,
                ));

                // Method body registration is an ordinary analysis concern.
                // The generic comptime host receives only structural method
                // descriptors and never a child-RIR token.
                ComptimeOutcome::Known(H::Value::type_value(struct_ty))
            }

            // Anonymous enum type: evaluate to a comptime type value, resolving
            // each variant's payload types through the type/value substitution.
            // The enum analog of the AnonStructType arm above — this is what
            // makes `fn Option(comptime T: type) -> type { enum { Some(T), None } }`
            // monomorphize per instantiation (ADR-0038, RUE-6 phase 2).
            InstData::AnonEnumType {
                variants,
                payloads,
                anchor,
            } => {
                let variant_syms = self.program_rir().anon_enum_variants(variants).to_vec();
                let payload_symbols: Vec<Vec<rue_rir::RirTypeSyntaxRef>> = self
                    .program_rir()
                    .anon_enum_payloads(payloads, variants)
                    .map(|payload| payload.to_vec())
                    .collect();

                // Decode the self-describing payload region into per-variant
                // type-symbol lists (parallel to `variant_syms`), then resolve
                // each payload type through the substitutions.
                // Comptime `let` locals participate in payload-type
                // resolution, matching the struct arm (RUE-575).
                let (enum_type_subst, enum_value_subst) = env.substs_with_locals();

                let mut variant_names: Vec<String> = Vec::with_capacity(variant_syms.len());
                let mut variant_payloads: Vec<Vec<H::Type>> =
                    Vec::with_capacity(variant_syms.len());
                for (&vsym, symbols) in variant_syms.iter().zip(payload_symbols) {
                    variant_names.push(self.host.display_name(&self.name_from_rir(vsym.into())));
                    let mut tys: Vec<H::Type> = Vec::with_capacity(symbols.len());
                    for ty_sym in symbols {
                        let ty = outcome_value!(self.evaluate_comptime_type_syntax(
                            &self.program_key(),
                            ty_sym,
                            &enum_type_subst,
                            &enum_value_subst,
                            span,
                        ));
                        tys.push(ty);
                    }
                    variant_payloads.push(tys);
                }

                let Some(producer) = env.canonical_identity.clone() else {
                    return ComptimeOutcome::RuntimeDependent;
                };
                let identity = self.host.issue_anonymous_identity(
                    &self.program_key(),
                    ComptimeAnonymousKind::Enum,
                    &producer,
                    anchor,
                );
                let enum_ty = host_value!(self.host.find_or_create_anon_enum(
                    identity,
                    &variant_names,
                    &variant_payloads,
                    &enum_type_subst,
                    &enum_value_subst,
                ));
                ComptimeOutcome::Known(H::Value::type_value(enum_ty))
            }

            // TypeConst: a type used as a value (e.g., `i32` in `identity(i32, 42)`)
            InstData::TypeConst { type_name } => {
                let type_name = *type_name;
                // Type parameters in scope substitute first.
                if let Some(type_symbol) = self
                    .host
                    .rir_type_named_symbol(&self.program_key(), type_name)
                {
                    // A runtime local shadows every outer type substitution
                    // and global type name. Only a local carrying a type value
                    // is eligible for comptime use here.
                    if let Some(local) = env.locals.get(&type_symbol) {
                        if let Some(ty) = local.as_type() {
                            return ComptimeOutcome::Known(H::Value::type_value(ty));
                        }
                        return ComptimeOutcome::RuntimeDependent;
                    }
                    if let Some(ty) = env.type_subst.get(&type_symbol) {
                        return ComptimeOutcome::Known(H::Value::type_value(ty.clone()));
                    }
                    // A named type (primitive / struct / enum) resolves directly.
                    if let Some(ty) = host_value!(self.host.resolve_named_type_value(
                        &self.program_key(),
                        type_symbol,
                        span,
                    )) {
                        return ComptimeOutcome::Known(H::Value::type_value(ty));
                    }
                }
                // A *composite* or *unit* type value — `[i32; 2]`, `()`,
                // `ptr const T` — is an equally-valid type argument (Appendix A
                // treats them as unambiguous type spellings; RUE-565). Its
                // TypeConst carries the composite spelling as the interned
                // `type_name`, so decode it through the full comptime type
                // resolver under the current substitutions (an inner element /
                // pointee naming an enclosing `comptime T` still resolves). An
                // unresolvable spelling stays non-evaluable (`None`).
                let ty = outcome_value!(self.evaluate_comptime_type_syntax(
                    &self.program_key(),
                    type_name,
                    &env.type_subst,
                    &env.value_subst,
                    span,
                ));
                ComptimeOutcome::Known(H::Value::type_value(ty))
            }

            // An array-repeat expression `[T; N]` used as a comptime *type* value
            // (RUE-565). The surface form `[i32; 2]` in expression position parses
            // as an array-repeat literal whose element is a type value; when that
            // element reduces to a type-valued comptime value, the whole expression is the
            // array TYPE `[T; N]` — a legal type-constructor argument
            // (`Option([i32; 2])`). A repeat over a *runtime* element is a genuine
            // array value literal and is not comptime-foldable here (`None`).
            InstData::ArrayRepeat { value, count } => {
                let (value, count) = (*value, count.clone());
                let value = outcome_value!(self.eval(value, env));
                let Some(elem_ty) = value.as_type() else {
                    let site = self.diagnostic_site(span);
                    return self.host.reject_non_type_array_repeat(value, &site);
                };
                let len = match count {
                    RepeatCount::Literal(n) => n,
                    RepeatCount::Named(sym) => {
                        let name = self.name_from_rir(sym.into());
                        let site = self.diagnostic_site(span);
                        let binding = Self::classify_array_length_binding(env, &name);
                        outcome_value!(self.host.resolve_named_array_length(
                            &name,
                            &site,
                            Some(&env.value_subst),
                            binding,
                        ))
                    }
                };
                let array_ty = self.host.get_or_create_array_type(elem_ty, len);
                ComptimeOutcome::Known(H::Value::type_value(array_ty))
            }

            // VarRef: comptime let-bindings, comptime parameters, file-level
            // constants, then type names.
            InstData::VarRef { name, .. } => {
                let name = self.name_from_rir((*name).into());
                // 1. `let` bindings inside the comptime expression
                if let Some(v) = env.locals.get(&name) {
                    return ComptimeOutcome::Known(v.clone());
                }
                // 2. Runtime locals shadow comptime parameters and file-level
                //    constants: a reference that resolves to one is not
                //    compile-time evaluable (spec 4.14:6).
                if env.is_runtime_local_name(&name) {
                    return ComptimeOutcome::RuntimeDependent;
                }
                // 3. Comptime type parameters in scope
                if let Some(ty) = env.type_subst.get(&name) {
                    return ComptimeOutcome::Known(H::Value::type_value(ty.clone()));
                }
                // 4. Comptime value parameters in scope
                if let Some(v) = env.value_subst.get(&name) {
                    return ComptimeOutcome::Known(v.clone());
                }
                // 5. Runtime parameters shadow file-level constants and type
                //    names. A comptime parameter with a concrete value was
                //    already handled by the substitution maps above.
                if env.runtime_binding_names.contains(&name) {
                    return ComptimeOutcome::RuntimeDependent;
                }
                // 6. File-level constants and named types are one atomic
                //    semantic lookup. The host owns direct dependency
                //    observation and visibility so durable adapters cannot
                //    split those effects across side channels.
                let program = self.program_key();
                let file = self.host.file_for_program_span(&program, &span);
                match host_value!(self.host.resolve_comptime_named_value(file, name, span)) {
                    ComptimeNamedValueResolution::Known(value) => ComptimeOutcome::Known(value),
                    ComptimeNamedValueResolution::RuntimeDependent
                    | ComptimeNamedValueResolution::Missing => ComptimeOutcome::RuntimeDependent,
                }
            }

            // Module-member access (`m.CONST`) as an operand of a larger const
            // initializer. The value was pre-resolved from the module's file
            // (with privacy checks) before evaluation — see the
            // `const_module_members` field — since the engine has no file or
            // constant-collector context to resolve it here. A member absent
            // from the map may still be a member-access *type* path used as a
            // comptime type-constructor argument (`std.strbuf.StrBuf` in
            // `Result(std.strbuf.StrBuf, i32)`, RUE-948): resolve that chain to
            // its nominal type through the same walker the qualified
            // type-annotation position uses. A base that is neither a
            // pre-resolved member value nor a module type path (a runtime
            // value's field) stays non-evaluable, so the caller reports it
            // (RUE-267).
            InstData::FieldGet { base, field } => {
                if let Some(value) = env.const_module_members.get(&inst_ref) {
                    return ComptimeOutcome::Known(value.clone());
                }
                if let Some((file, segments)) = self.decode_type_path(inst_ref, env) {
                    if let Some(value) =
                        host_value!(self.host.resolve_comptime_type_path(file, &segments, span))
                    {
                        return ComptimeOutcome::Known(value);
                    }
                }
                let field = self.name_from_rir((*field).into());
                let site = self.semantic_site(inst_ref, ComptimeSiteKind::Member, span);
                if !host_value!(self.host.admit_comptime_member(field.clone(), &site)) {
                    return ComptimeOutcome::RuntimeDependent;
                }
                let base = outcome_value!(self.eval(*base, env));
                self.host.resolve_comptime_member(base, field, &site, span)
            }

            // `checked { expr }` does not change the value produced by a
            // comptime expression. Keep the child traversal in this engine,
            // then let a semantic host observe or refine the completed value.
            InstData::Checked { expr } => {
                if !self.host.allow_checked_comptime() {
                    return ComptimeOutcome::RuntimeDependent;
                }
                match self.eval(*expr, env) {
                    ComptimeOutcome::Known(value) => self.host.finish_checked(value, span),
                    other => other,
                }
            }

            // Expression intrinsics are classified and structurally decoded
            // before child evaluation. This finite family has no comptime
            // child arguments: a durable host receives the import literal or
            // target arity as semantic facts, while malformed controls retain
            // their intrinsic-site diagnostics.
            InstData::Intrinsic { name, args } => {
                let name = self.name_from_rir((*name).into());
                let decoded = match self.decode_expression_intrinsic(name, args) {
                    Ok(decoded) => decoded,
                    Err(display_name) => {
                        return self.host.reject_comptime_expression(
                            ComptimeSemanticRejection::UnsupportedIntrinsic(display_name),
                            &self.diagnostic_site(span),
                        );
                    }
                };
                let site = self.semantic_site(inst_ref, decoded.site_kind, span);
                self.host
                    .resolve_comptime_expression_intrinsic(decoded.request, &site)
            }

            // Enum variants are runtime values in the ordinary body domain.
            // Reduce a qualified module expression first, then hand only the
            // resulting semantic value and names to the host.
            InstData::EnumVariant {
                module,
                type_name,
                variant,
            } => {
                let site = self.semantic_site(inst_ref, ComptimeSiteKind::EnumVariant, span);
                let type_name = self.name_from_rir((*type_name).into());
                let variant = self.name_from_rir((*variant).into());
                if !host_value!(self.host.admit_comptime_enum_variant(
                    type_name.clone(),
                    variant.clone(),
                    module.is_some(),
                    &site,
                )) {
                    return ComptimeOutcome::RuntimeDependent;
                }
                let module = match module {
                    Some(module) => Some(outcome_value!(self.eval(*module, env))),
                    None => None,
                };
                self.host
                    .resolve_comptime_enum_variant(module, type_name, variant, &site, span)
            }

            // Type intrinsic in comptime position. `@require_droppable(T)` is the
            // owning-container well-formedness gate (RUE-388/RUE-646): std's
            // `ArrayBuf(T)` calls it in its `-> type` constructor body so that
            // instantiating the container with an element type it cannot yet
            // correctly own — one that is `linear` — is rejected at instantiation
            // time (E0499). Droppable-but-non-linear elements are accepted: the
            // container runs each live element's drop glue before freeing its
            // buffer (RUE-646). It reduces to unit so the surrounding block
            // body still yields the `struct { .. }` tail. `@size_of`/`@align_of`
            // are not comptime-foldable here and stay non-evaluable (spec
            // 4.14:29); `@int_max`/`@int_min` depend only on the type identity,
            // not layout, so they evaluate to their integer bound (RUE-694).
            InstData::TypeIntrinsic { name, type_arg } => {
                let (name, type_arg) = (*name, *type_arg);
                let gate_name = self.name_from_rir(name.into());
                let gate = self.host.display_name(&gate_name);
                let Some(intrinsic) = ComptimeTypeIntrinsic::from_name(&gate) else {
                    return self.host.reject_comptime_expression(
                        ComptimeSemanticRejection::UnsupportedIntrinsic(gate),
                        &self.diagnostic_site(span),
                    );
                };
                // Both well-formedness gates reduce to unit at comptime:
                // `@require_droppable` (instantiation-time, rejects `linear`) and
                // `@require_trivially_droppable` (read-time, rejects drop glue —
                // RUE-651). Any other type intrinsic (`@size_of`/`@align_of`) is
                // not comptime-foldable here.
                // Resolve the element type through the enclosing comptime
                // substitutions (`T -> Inner` for `ArrayBuf(Inner)`); a
                // still-unresolved type parameter makes the gate non-evaluable
                // (it will be re-checked at a concrete instantiation).
                let intrinsic_ty = outcome_value!(self.evaluate_comptime_type_syntax(
                    &self.program_key(),
                    type_arg,
                    &env.type_subst,
                    &env.value_subst,
                    span,
                ));
                match host_value!(self.host.resolve_comptime_type_intrinsic(
                    intrinsic,
                    intrinsic_ty,
                    &self.diagnostic_site(span),
                )) {
                    Some(value) => ComptimeOutcome::Known(value),
                    None => ComptimeOutcome::RuntimeDependent,
                }
            }

            // Module-qualified comptime type-constructor call in value position,
            // e.g. `let O = b.Mk(T)` inside a `-> type` constructor body that is
            // being reduced (RUE-511). The receiver must be an unshadowed
            // `VarRef` naming a module binding of the *defining* file; membership
            // and visibility are validated before the call is reduced through the
            // same path unqualified calls take. Any other receiver (a runtime
            // value's method, a shadowed name) is a genuine runtime call and
            // stays non-evaluable.
            InstData::MethodCall {
                receiver,
                method,
                args,
            } => {
                let receiver = *receiver;
                let method = self.name_from_rir((*method).into());
                self.evaluate_method_call(receiver, method, args, env, span)
            }

            InstData::StructInit { .. } | InstData::ArrayInit { .. } => {
                self.host.reject_comptime_expression(
                    ComptimeSemanticRejection::AggregateExpression,
                    &self.diagnostic_site(span),
                )
            }

            // Everything else requires runtime evaluation. The semantic
            // rejection hook lets durable hosts preserve the exact
            // declaration-time reason while ordinary evaluation remains
            // runtime-dependent.
            _ => self.host.reject_comptime_expression(
                ComptimeSemanticRejection::UnsupportedExpression,
                &self.diagnostic_site(span),
            ),
        }
    }
}
