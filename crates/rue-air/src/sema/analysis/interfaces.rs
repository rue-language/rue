//! Interface bounds and conformance verification (spec 6.7, preview
//! `interfaces`).
//!
//! Interfaces are erased before CFG construction: nothing here changes the
//! AIR a call produces. The engine only decides, at each call that binds a
//! type argument to a bounded comptime parameter, whether the argument type
//! conforms (spec 6.7:15), and verifies the conformance assertions that
//! decision relied on against the type's inherent members (spec 6.7:10).
//!
//! The declaration facts — which shells are interfaces, what a struct
//! asserts, which requirement signatures an interface declares — come from
//! the host through [`super::super::ordinary_engine::DeclarationFacts`]; this
//! module owns the comparison and the diagnostics.

use super::super::ordinary_engine::{OrdinaryBodyAnalysisHost, OrdinaryBodyEngine};
use super::*;
use crate::sema::info::{
    ConformanceAssertion, FunctionCallInfo, MethodCallInfo, RequirementSignature,
};
use rue_error::{
    InterfaceRequirementUnsatisfiedError, InterfaceSignatureMismatchError, PreviewFeature,
};
use rue_rir::RirParamMode;
use std::sync::Arc;

/// One unsatisfied requirement of a conformance assertion (spec 6.7:10),
/// carried until every requirement of the assertion has been checked so
/// the diagnostic names all of them.
struct RequirementFailure {
    kind: ErrorKind,
    /// The requirement's short description for the secondary labels of a
    /// multi-failure diagnostic.
    summary: String,
}

impl<H: OrdinaryBodyAnalysisHost> OrdinaryBodyEngine<'_, H> {
    /// Enforce the interface bound of comptime parameter `index` at a call
    /// that binds it to `argument` (spec 6.7:15). A parameter without a bound
    /// is accepted without consulting the host any further.
    pub(crate) fn check_interface_bounds(
        &mut self,
        function: &FunctionCallInfo,
        index: usize,
        parameter: Spur,
        argument: Type,
        argument_span: Span,
        call_span: Span,
    ) -> CompileResult<()> {
        let bounds = self.comptime_parameter_bounds(function, index);
        if bounds.is_empty() {
            return Ok(());
        }
        // A bound declared by a trusted standard-library function is usable
        // without the preview (spec 6.7:25); every other bound is gated at
        // the call as at its declaration (spec 6.7:3).
        if !self.file_module_is_trusted_standard_library(function.file_id) {
            self.require_preview(
                PreviewFeature::Interfaces,
                "an interface bound on a comptime parameter",
                call_span,
            )?;
        }
        let parameter = self.body_interner().resolve(&parameter).to_string();
        for bound in bounds {
            let Some(assertion) = self.assertion_satisfying(argument, bound) else {
                let ty = self.format_type_name(argument);
                let interface = self.interface_display_name(bound);
                // Under a skolem check (spec 6.7:19) the argument is the
                // caller's own bounded parameter: nothing can be asserted
                // about it, only its bound can grow.
                let help = match argument
                    .as_struct()
                    .and_then(|skolem| self.skolem_display_name(skolem))
                {
                    Some(skolem) => format!(
                        "add `{interface}` to the bound of parameter `{skolem}`: `comptime {skolem}: ... + {interface}`"
                    ),
                    None => format!(
                        "add `{ty} is {interface};` to assert the conformance, or a `struct {ty} is {interface}` header"
                    ),
                };
                return Err(CompileError::new(
                    ErrorKind::InterfaceBoundNotSatisfied { ty, interface },
                    argument_span,
                )
                .with_label(
                    format!("required by the bound on parameter `{parameter}`"),
                    call_span,
                )
                .with_help(help));
            };
            self.verify_conformance(argument, assertion, call_span)?;
        }
        Ok(())
    }

    /// An assertion that `subject` conforms to `interface` or to an interface
    /// refining it (spec 6.7:12), if the body can see one.
    fn assertion_satisfying(
        &mut self,
        subject: Type,
        interface: StructId,
    ) -> Option<ConformanceAssertion> {
        let assertions = self.conformance_assertions(subject);
        assertions.into_iter().find(|assertion| {
            self.refinement_closure(assertion.interface)
                .contains(&interface)
        })
    }

    /// `interface` and every interface it transitively refines (spec 6.7:7),
    /// in discovery order. Refinement is acyclic by rule; the visited set
    /// keeps an ill-formed cycle from looping.
    pub(crate) fn refinement_closure(&mut self, interface: StructId) -> Vec<StructId> {
        let mut closure = vec![interface];
        let mut next = 0;
        while next < closure.len() {
            let current = closure[next];
            next += 1;
            let Some(facts) = self.interface_facts(current) else {
                continue;
            };
            for parent in facts.parents {
                if !closure.contains(&parent) {
                    closure.push(parent);
                }
            }
        }
        closure
    }

    /// Verify `assertion` for `subject` against the subject's inherent
    /// members (spec 6.7:10), covering every interface the asserted one
    /// refines (spec 6.7:12). Each (subject, interface) pair is verified once
    /// per body: repeated assertions are one fact (spec 6.7:11), and a later
    /// call relying on the same fact reuses the answer.
    pub(crate) fn verify_conformance(
        &mut self,
        subject: Type,
        assertion: ConformanceAssertion,
        relied_on: Span,
    ) -> CompileResult<()> {
        let assertion_span = assertion.span.unwrap_or(relied_on);
        for interface in self.refinement_closure(assertion.interface) {
            if self.verified_conformances.contains(&(subject, interface)) {
                continue;
            }
            let failures = self.requirement_failures(subject, interface, assertion_span)?;
            let mut failures = failures.into_iter();
            if let Some(first) = failures.next() {
                let mut error = CompileError::new(first.kind, assertion_span);
                for failure in failures {
                    error = error.with_label(failure.summary, assertion_span);
                }
                if assertion.span.is_some() {
                    error = error.with_label("conformance relied on here", relied_on);
                }
                return Err(error);
            }
            self.verified_conformances.insert((subject, interface));
        }
        Ok(())
    }

    /// Every requirement of `interface` that `subject` does not satisfy.
    fn requirement_failures(
        &mut self,
        subject: Type,
        interface: StructId,
        span: Span,
    ) -> CompileResult<Vec<RequirementFailure>> {
        let Some(facts) = self.interface_facts(interface) else {
            return Ok(Vec::new());
        };
        let ty = self.format_type_name(subject);
        let interface_name = facts.name.to_string();
        let mut failures = Vec::new();
        let unsatisfied = |member: &str| {
            Box::new(InterfaceRequirementUnsatisfiedError {
                ty: ty.clone(),
                interface: interface_name.clone(),
                member: member.to_owned(),
            })
        };
        let mut assoc_types: Vec<(Arc<str>, Type)> = Vec::new();
        for name in &facts.assoc_requirements {
            match self.assoc_type(subject, name) {
                Some(value) => assoc_types.push((name.clone(), value)),
                None => failures.push(RequirementFailure {
                    kind: ErrorKind::MissingAssociatedType(unsatisfied(name)),
                    summary: format!("missing associated type `{name}`"),
                }),
            }
        }
        // A method requirement's signature can name the missing associated
        // types, so it is only comparable once every associated type is
        // present; the missing ones are the whole finding until then.
        if !failures.is_empty() {
            return Ok(failures);
        }
        for name in &facts.method_requirements {
            let Some(required) =
                self.interface_requirement_signature(interface, name, subject, &assoc_types, span)?
            else {
                continue;
            };
            let member = subject.as_struct().and_then(|owner| {
                self.method_info((owner, self.body_interner().get_or_intern(name)))
            });
            let Some(member) = member else {
                failures.push(RequirementFailure {
                    kind: ErrorKind::InterfaceMemberMissing(unsatisfied(name)),
                    summary: format!("missing member `{name}`"),
                });
                continue;
            };
            let found = self.render_member_signature(name, &member);
            let expected = self.render_requirement_signature(name, &required);
            if found != expected {
                failures.push(RequirementFailure {
                    kind: ErrorKind::InterfaceSignatureMismatch(Box::new(
                        InterfaceSignatureMismatchError {
                            ty: ty.clone(),
                            interface: interface_name.clone(),
                            member: name.to_string(),
                            expected: expected.clone(),
                            found,
                        },
                    )),
                    summary: format!("member `{name}` does not have the signature `{expected}`"),
                });
            }
        }
        Ok(failures)
    }

    /// Render a requirement as `fn name(borrow self, x: T) -> R`, the form
    /// both sides of an E0303 are compared and displayed in. Comparing the
    /// rendering compares receiver presence and mode, parameter count, each
    /// parameter's mode and type, and the result type (spec 6.7:10);
    /// parameter names are deliberately not rendered so they cannot
    /// distinguish otherwise equal signatures.
    fn render_requirement_signature(&self, name: &str, signature: &RequirementSignature) -> String {
        let params = signature
            .params
            .iter()
            .map(|(_, mode, ty)| (*mode, *ty))
            .collect::<Vec<_>>();
        self.render_signature(
            name,
            signature.has_self,
            signature.self_mode,
            &params,
            signature.result,
        )
    }

    fn render_member_signature(&self, name: &str, member: &MethodCallInfo) -> String {
        let data = self.body_param_data(member.params);
        let params = data
            .modes()
            .iter()
            .zip(data.types())
            .map(|(mode, ty)| (*mode, *ty))
            .collect::<Vec<_>>();
        self.render_signature(
            name,
            member.has_self,
            member.self_mode,
            &params,
            member.return_type,
        )
    }

    fn render_signature(
        &self,
        name: &str,
        has_self: bool,
        self_mode: RirParamMode,
        params: &[(RirParamMode, Type)],
        result: Type,
    ) -> String {
        let mode_prefix = |mode: RirParamMode| match mode {
            RirParamMode::Normal => "",
            RirParamMode::Borrow => "borrow ",
            RirParamMode::Inout => "inout ",
        };
        let mut parts = Vec::new();
        if has_self {
            parts.push(format!("{}self", mode_prefix(self_mode)));
        }
        for (mode, ty) in params {
            parts.push(format!(
                "{}_: {}",
                mode_prefix(*mode),
                self.format_type_name(*ty)
            ));
        }
        let mut rendered = format!("fn {name}({})", parts.join(", "));
        if result != Type::UNIT {
            rendered.push_str(" -> ");
            rendered.push_str(&self.format_type_name(result));
        }
        rendered
    }

    /// The interface's source name for diagnostics.
    pub(crate) fn interface_display_name(&mut self, interface: StructId) -> String {
        match self.interface_facts(interface) {
            Some(facts) => facts.name.to_string(),
            None => self.format_type_name(Type::new_struct(interface)),
        }
    }
}

/// Fixture helpers shared by the interface and skolem tests: the durable
/// shapes the nucleus produces for interface shells, bounded parameters, and
/// requirement signatures.
#[cfg(test)]
pub(super) mod interface_fixtures {
    use crate::sema::provider_fixture::{
        FixtureKey, FixtureModule, MethodShape, ProviderFixture, StructShape,
    };
    use crate::{
        DurableCallableTypeSyntax, DurableConformance, DurableConformanceFacts,
        DurableSignatureParameter, SemanticImportType, SemanticParameterMode,
    };
    use std::sync::Arc;

    pub(in crate::sema) type T = SemanticImportType<FixtureKey, FixtureModule>;

    /// `comptime T: <bounds>` — the durable shape the nucleus produces for a
    /// bounded comptime type parameter (spec 6.7:14).
    pub(in crate::sema) fn bounded_type_param(
        name: &str,
        bounds: &[FixtureKey],
    ) -> DurableSignatureParameter<FixtureKey, FixtureModule> {
        DurableSignatureParameter {
            name: Arc::from(name),
            ty: T::ComptimeType,
            mode: SemanticParameterMode::Value,
            is_comptime: true,
            bounds: bounds.to_vec().into(),
        }
    }

    pub(in crate::sema) fn assertion(interface: &FixtureKey) -> DurableConformance<FixtureKey> {
        DurableConformance {
            interface: interface.clone(),
            start: 0,
            end: 0,
        }
    }

    /// Declare an interface shell with the given refinement list, type-valued
    /// requirements, and method-requirement names.
    pub(in crate::sema) fn declare_interface(
        fixture: &mut ProviderFixture,
        name: &str,
        parents: &[FixtureKey],
        assoc: &[&str],
        requirements: &[&str],
    ) -> FixtureKey {
        fixture.declare_struct_with(
            name,
            Vec::new(),
            false,
            StructShape {
                conformance: DurableConformanceFacts {
                    is_interface: true,
                    conformances: parents.iter().map(assertion).collect(),
                    assoc_types: assoc
                        .iter()
                        .map(|name| (Arc::from(*name), T::ComptimeType))
                        .collect(),
                    requirements: requirements.iter().map(|name| Arc::from(*name)).collect(),
                },
                ..StructShape::default()
            },
        )
    }

    /// A `borrow self` method with concrete parameter and result types.
    pub(in crate::sema) fn declare_borrow_method(
        fixture: &mut ProviderFixture,
        owner: &FixtureKey,
        name: &str,
        params: Vec<DurableSignatureParameter<FixtureKey, FixtureModule>>,
        result: T,
        type_syntax: Option<DurableCallableTypeSyntax>,
    ) -> FixtureKey {
        fixture.declare_method_with(
            owner,
            name,
            params,
            result,
            MethodShape {
                self_mode: SemanticParameterMode::Borrow,
                type_syntax,
                ..MethodShape::default()
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::interface_fixtures::{
        T, assertion, bounded_type_param, declare_borrow_method, declare_interface,
    };
    use crate::sema::provider_fixture::{
        FixtureKey, MethodShape, ProviderFixture, StructShape, mode_param,
    };
    use crate::{DurableCallableTypeSyntax, DurableConformanceFacts, SemanticParameterMode};
    use rue_error::{ErrorKind, PreviewFeature};
    use std::sync::Arc;

    /// The retained syntax of `fn equals(borrow self, borrow other: Self) -> bool`
    /// as an interface requirement: `Self` is the owner placeholder
    /// (`GenericParameter(0)`) in the durable signature.
    fn equals_requirement_syntax() -> DurableCallableTypeSyntax {
        let mut builder = rue_rir::RirTypeSyntaxBuilder::<Arc<str>>::default();
        let other = builder.push_named_type(Arc::from("Self")).unwrap();
        let result = builder.push_named_type(Arc::from("bool")).unwrap();
        DurableCallableTypeSyntax {
            syntax: builder.finish(),
            parameters: Arc::from([other]),
            result,
        }
    }

    /// `interface Show { fn show(borrow self) -> i64; }` plus a `render`
    /// function bounded on it, under the interfaces preview.
    fn show_fixture(fixture: &mut ProviderFixture) -> FixtureKey {
        fixture.enable_preview(PreviewFeature::Interfaces);
        show_fixture_ungated(fixture)
    }

    fn show_fixture_ungated(fixture: &mut ProviderFixture) -> FixtureKey {
        fixture.declare_function("main", Vec::new(), T::I32);
        let show = declare_interface(fixture, "Show", &[], &[], &["show"]);
        declare_borrow_method(fixture, &show, "show", Vec::new(), T::I64, None);
        fixture.declare_function(
            "render",
            vec![
                bounded_type_param("T", &[show.clone()]),
                mode_param("x", T::GenericParameter(0), SemanticParameterMode::Borrow),
            ],
            T::I64,
        );
        show
    }

    const RENDER_MAIN: &str =
        "fn main() -> i32 { let v = Val { n: 3 }; @intCast(render(Val, borrow v)) }";

    #[test]
    fn header_assertion_satisfies_the_bound_and_verifies() {
        let mut fixture = ProviderFixture::new();
        let show = show_fixture(&mut fixture);
        let val = fixture.declare_struct_with(
            "Val",
            vec![("n", T::I64)],
            false,
            StructShape {
                conformance: DurableConformanceFacts {
                    conformances: Arc::from([assertion(&show)]),
                    ..DurableConformanceFacts::default()
                },
                ..StructShape::default()
            },
        );
        declare_borrow_method(&mut fixture, &val, "show", Vec::new(), T::I64, None);
        fixture
            .analyze(RENDER_MAIN, "main")
            .expect("a conforming type argument satisfies the bound");
    }

    #[test]
    fn missing_assertion_is_reported_at_the_argument() {
        let mut fixture = ProviderFixture::new();
        show_fixture(&mut fixture);
        let val = fixture.declare_struct("Val", vec![("n", T::I64)], false);
        declare_borrow_method(&mut fixture, &val, "show", Vec::new(), T::I64, None);
        let error = fixture
            .analyze(RENDER_MAIN, "main")
            .err()
            .expect("no assertion means no conformance (spec 6.7:15)");
        assert!(
            matches!(
                &error.kind,
                ErrorKind::InterfaceBoundNotSatisfied { ty, interface }
                    if ty == "Val" && interface == "Show"
            ),
            "{error:?}"
        );
        assert!(
            error
                .diagnostic()
                .labels
                .iter()
                .any(|label| label.message.contains("parameter `T`")),
            "{error:?}"
        );
    }

    #[test]
    fn bound_requires_the_interfaces_preview() {
        let mut fixture = ProviderFixture::new();
        show_fixture_ungated(&mut fixture);
        let val = fixture.declare_struct("Val", vec![("n", T::I64)], false);
        declare_borrow_method(&mut fixture, &val, "show", Vec::new(), T::I64, None);
        let error = fixture
            .analyze(RENDER_MAIN, "main")
            .err()
            .expect("a bound is gated (spec 6.7:3)");
        assert!(
            matches!(
                &error.kind,
                ErrorKind::PreviewFeatureRequired {
                    feature: PreviewFeature::Interfaces,
                    ..
                }
            ),
            "{error:?}"
        );
    }

    #[test]
    fn wrong_receiver_mode_renders_both_signatures() {
        let mut fixture = ProviderFixture::new();
        let show = show_fixture(&mut fixture);
        let val = fixture.declare_struct_with(
            "Val",
            vec![("n", T::I64)],
            false,
            StructShape {
                conformance: DurableConformanceFacts {
                    conformances: Arc::from([assertion(&show)]),
                    ..DurableConformanceFacts::default()
                },
                ..StructShape::default()
            },
        );
        fixture.declare_method_with(
            &val,
            "show",
            Vec::new(),
            T::I64,
            MethodShape {
                self_mode: SemanticParameterMode::Inout,
                ..MethodShape::default()
            },
        );
        let error = fixture
            .analyze(RENDER_MAIN, "main")
            .err()
            .expect("an inout receiver does not satisfy a borrow requirement");
        let ErrorKind::InterfaceSignatureMismatch(mismatch) = &error.kind else {
            panic!("{error:?}");
        };
        assert_eq!(mismatch.member, "show");
        assert_eq!(mismatch.expected, "fn show(borrow self) -> i64");
        assert_eq!(mismatch.found, "fn show(inout self) -> i64");
    }

    #[test]
    fn missing_member_names_type_interface_and_member() {
        let mut fixture = ProviderFixture::new();
        let show = show_fixture(&mut fixture);
        fixture.declare_struct_with(
            "Val",
            vec![("n", T::I64)],
            false,
            StructShape {
                conformance: DurableConformanceFacts {
                    conformances: Arc::from([assertion(&show)]),
                    ..DurableConformanceFacts::default()
                },
                ..StructShape::default()
            },
        );
        let error = fixture
            .analyze(RENDER_MAIN, "main")
            .err()
            .expect("a type without the member does not conform");
        let ErrorKind::InterfaceMemberMissing(missing) = &error.kind else {
            panic!("{error:?}");
        };
        assert_eq!(
            (
                missing.ty.as_str(),
                missing.interface.as_str(),
                missing.member.as_str()
            ),
            ("Val", "Show", "show")
        );
    }

    #[test]
    fn self_in_a_requirement_is_the_conforming_type() {
        let mut fixture = ProviderFixture::new();
        fixture.enable_preview(PreviewFeature::Interfaces);
        fixture.declare_function("main", Vec::new(), T::I32);
        let equatable = declare_interface(&mut fixture, "Equatable", &[], &[], &["equals"]);
        declare_borrow_method(
            &mut fixture,
            &equatable,
            "equals",
            vec![mode_param(
                "other",
                T::GenericParameter(0),
                SemanticParameterMode::Borrow,
            )],
            T::Bool,
            Some(equals_requirement_syntax()),
        );
        fixture.declare_function(
            "same",
            vec![
                bounded_type_param("T", &[equatable.clone()]),
                mode_param("a", T::GenericParameter(0), SemanticParameterMode::Borrow),
                mode_param("b", T::GenericParameter(0), SemanticParameterMode::Borrow),
            ],
            T::Bool,
        );
        let id = fixture.declare_struct_with(
            "Id",
            vec![("n", T::I64)],
            false,
            StructShape {
                conformance: DurableConformanceFacts {
                    conformances: Arc::from([assertion(&equatable)]),
                    ..DurableConformanceFacts::default()
                },
                ..StructShape::default()
            },
        );
        // `equals(borrow self, borrow other: i64)`: `Self` substitutes to
        // `Id`, so the `i64` parameter mismatches and both renderings show
        // the substituted type.
        declare_borrow_method(
            &mut fixture,
            &id,
            "equals",
            vec![mode_param("other", T::I64, SemanticParameterMode::Borrow)],
            T::Bool,
            None,
        );
        let source = "fn main() -> i32 { let a = Id { n: 1 }; if same(Id, borrow a, borrow a) { 0 } else { 1 } }";
        let error = fixture
            .analyze(source, "main")
            .err()
            .expect("the parameter type must equal the substituted `Self`");
        let ErrorKind::InterfaceSignatureMismatch(mismatch) = &error.kind else {
            panic!("{error:?}");
        };
        assert_eq!(
            mismatch.expected,
            "fn equals(borrow self, borrow _: Id) -> bool"
        );
        assert_eq!(
            mismatch.found,
            "fn equals(borrow self, borrow _: i64) -> bool"
        );
    }

    /// `interface Sequence { const Element: type; fn next(inout self) -> Element; }`
    /// plus a `drain` function bounded on it.
    fn sequence_fixture() -> (ProviderFixture, FixtureKey) {
        let mut fixture = ProviderFixture::new();
        fixture.enable_preview(PreviewFeature::Interfaces);
        fixture.declare_function("main", Vec::new(), T::I32);
        let sequence = declare_interface(&mut fixture, "Sequence", &[], &["Element"], &["next"]);
        // `fn next(inout self) -> Element;`: `Element` is the second owner
        // placeholder after `Self`.
        let mut builder = rue_rir::RirTypeSyntaxBuilder::<Arc<str>>::default();
        let element = builder.push_named_type(Arc::from("Element")).unwrap();
        fixture.declare_method_with(
            &sequence,
            "next",
            Vec::new(),
            T::GenericParameter(1),
            MethodShape {
                self_mode: SemanticParameterMode::Inout,
                type_syntax: Some(DurableCallableTypeSyntax {
                    syntax: builder.finish(),
                    parameters: Arc::from([]),
                    result: element,
                }),
                ..MethodShape::default()
            },
        );
        fixture.declare_function(
            "drain",
            vec![
                bounded_type_param("T", &[sequence.clone()]),
                mode_param("s", T::GenericParameter(0), SemanticParameterMode::Inout),
            ],
            T::I64,
        );
        (fixture, sequence)
    }

    const DRAIN_MAIN: &str =
        "fn main() -> i32 { let mut r = Range { cur: 0 }; @intCast(drain(Range, inout r)) }";

    #[test]
    fn missing_associated_type_is_unsatisfied() {
        let (mut missing, sequence) = sequence_fixture();
        let range = missing.declare_struct_with(
            "Range",
            vec![("cur", T::I64)],
            false,
            StructShape {
                conformance: DurableConformanceFacts {
                    conformances: Arc::from([assertion(&sequence)]),
                    ..DurableConformanceFacts::default()
                },
                ..StructShape::default()
            },
        );
        missing.declare_method_with(
            &range,
            "next",
            Vec::new(),
            T::I64,
            MethodShape {
                self_mode: SemanticParameterMode::Inout,
                ..MethodShape::default()
            },
        );
        let error = missing
            .analyze(DRAIN_MAIN, "main")
            .err()
            .expect("a missing associated type is unsatisfied (spec 6.7:10)");
        assert!(
            matches!(&error.kind, ErrorKind::MissingAssociatedType(missing) if missing.member == "Element"),
            "{error:?}"
        );
    }

    #[test]
    fn associated_type_is_substituted_into_requirements() {
        // With `pub const Element = i64;` the requirement `-> Element` is
        // compared as `-> i64` and the member satisfies it.
        let (mut fixture, sequence) = sequence_fixture();
        let range = fixture.declare_struct_with(
            "Range",
            vec![("cur", T::I64)],
            false,
            StructShape {
                conformance: DurableConformanceFacts {
                    conformances: Arc::from([assertion(&sequence)]),
                    assoc_types: Arc::from([(Arc::from("Element"), T::I64)]),
                    ..DurableConformanceFacts::default()
                },
                ..StructShape::default()
            },
        );
        fixture.declare_method_with(
            &range,
            "next",
            Vec::new(),
            T::I64,
            MethodShape {
                self_mode: SemanticParameterMode::Inout,
                ..MethodShape::default()
            },
        );
        fixture
            .analyze(DRAIN_MAIN, "main")
            .expect("the declared associated type satisfies the requirement");
    }

    #[test]
    fn refinement_is_walked_transitively_and_is_cycle_safe() {
        let mut fixture = ProviderFixture::new();
        fixture.enable_preview(PreviewFeature::Interfaces);
        fixture.declare_function("main", Vec::new(), T::I32);
        // `Loud: Show`, `Louder: Loud`; asserting `Louder` satisfies a bound
        // on `Show` and verifies every interface on the way (spec 6.7:12).
        let show = declare_interface(&mut fixture, "Show", &[], &[], &["show"]);
        declare_borrow_method(&mut fixture, &show, "show", Vec::new(), T::I64, None);
        let loud = declare_interface(&mut fixture, "Loud", &[show.clone()], &[], &["twice"]);
        declare_borrow_method(&mut fixture, &loud, "twice", Vec::new(), T::I64, None);
        let louder = declare_interface(&mut fixture, "Louder", &[loud.clone()], &[], &["thrice"]);
        declare_borrow_method(&mut fixture, &louder, "thrice", Vec::new(), T::I64, None);
        fixture.declare_function(
            "render",
            vec![
                bounded_type_param("T", &[show.clone()]),
                mode_param("x", T::GenericParameter(0), SemanticParameterMode::Borrow),
            ],
            T::I64,
        );
        let val = fixture.declare_struct_with(
            "Val",
            vec![("n", T::I64)],
            false,
            StructShape {
                conformance: DurableConformanceFacts {
                    conformances: Arc::from([assertion(&louder)]),
                    ..DurableConformanceFacts::default()
                },
                ..StructShape::default()
            },
        );
        declare_borrow_method(&mut fixture, &val, "show", Vec::new(), T::I64, None);
        declare_borrow_method(&mut fixture, &val, "thrice", Vec::new(), T::I64, None);
        // `twice`, required by the refined `Loud`, is missing: the assertion
        // of `Louder` fails on `Loud`'s requirement.
        let error = fixture
            .analyze(RENDER_MAIN, "main")
            .err()
            .expect("a refined interface's requirement is verified too");
        let ErrorKind::InterfaceMemberMissing(missing) = &error.kind else {
            panic!("{error:?}");
        };
        assert_eq!(
            (missing.interface.as_str(), missing.member.as_str()),
            ("Loud", "twice")
        );

        declare_borrow_method(&mut fixture, &val, "twice", Vec::new(), T::I64, None);
        fixture
            .analyze(RENDER_MAIN, "main")
            .expect("the transitive assertion satisfies the bound");

        // An ill-formed refinement cycle terminates.
        let mut cyclic = ProviderFixture::new();
        cyclic.enable_preview(PreviewFeature::Interfaces);
        cyclic.declare_function("main", Vec::new(), T::I32);
        let a = FixtureKey::nominal("A", crate::StableDefinitionKind::Struct);
        let b = declare_interface(&mut cyclic, "B", &[a.clone()], &[], &["b"]);
        declare_borrow_method(&mut cyclic, &b, "b", Vec::new(), T::I64, None);
        let a = declare_interface(&mut cyclic, "A", &[b.clone()], &[], &["a"]);
        declare_borrow_method(&mut cyclic, &a, "a", Vec::new(), T::I64, None);
        cyclic.declare_function(
            "render",
            vec![
                bounded_type_param("T", &[b.clone()]),
                mode_param("x", T::GenericParameter(0), SemanticParameterMode::Borrow),
            ],
            T::I64,
        );
        let val = cyclic.declare_struct_with(
            "Val",
            vec![("n", T::I64)],
            false,
            StructShape {
                conformance: DurableConformanceFacts {
                    conformances: Arc::from([assertion(&a)]),
                    ..DurableConformanceFacts::default()
                },
                ..StructShape::default()
            },
        );
        declare_borrow_method(&mut cyclic, &val, "a", Vec::new(), T::I64, None);
        declare_borrow_method(&mut cyclic, &val, "b", Vec::new(), T::I64, None);
        cyclic
            .analyze(RENDER_MAIN, "main")
            .expect("a refinement cycle is walked once and satisfies the bound");
    }

    #[test]
    fn a_verified_assertion_answers_every_later_call_in_the_body() {
        let mut fixture = ProviderFixture::new();
        let show = show_fixture(&mut fixture);
        let val = fixture.declare_struct_with(
            "Val",
            vec![("n", T::I64)],
            false,
            StructShape {
                conformance: DurableConformanceFacts {
                    conformances: Arc::from([assertion(&show), assertion(&show)]),
                    ..DurableConformanceFacts::default()
                },
                ..StructShape::default()
            },
        );
        declare_borrow_method(&mut fixture, &val, "show", Vec::new(), T::I64, None);
        // Two calls and a repeated assertion (spec 6.7:11): one verification,
        // one answer.
        let source = "fn main() -> i32 { let v = Val { n: 3 }; let w = Val { n: 4 }; @intCast(render(Val, borrow v) + render(Val, borrow w)) }";
        fixture
            .analyze(source, "main")
            .expect("repeated reliance on one assertion analyzes");
    }
}
