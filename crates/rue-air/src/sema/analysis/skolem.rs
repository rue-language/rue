//! Skolem types for the definition-site check of bounded generic bodies
//! (spec 6.7:19–6.7:22, preview `interfaces`).
//!
//! A bounded function is checked once by analyzing its body as an ordinary
//! specialization whose type arguments are *skolems*: fieldless move types,
//! distinct from every program type, whose inherent members are exactly the
//! requirements of the bound (spec 6.7:20). The compiler mints the skolem
//! nominals; this module synthesizes their member tables from the interface
//! facts the host exposes — the union of every requirement of every bound
//! and every interface it refines, each signature instantiated with the
//! skolem for `Self` and the skolem's own opaque associated types for the
//! interface's associated-constant names — and rejects a bound set whose
//! interfaces disagree about one member (spec 6.7:21).
//!
//! Member lookup on a skolem then goes through the host's ordinary named
//! method path, so inference, ownership, exclusivity, and drop analysis run
//! unchanged over the body.

use super::super::ordinary_engine::{OrdinaryBodyAnalysisHost, OrdinaryBodyEngine};
use super::*;
use crate::sema::info::RequirementSignature;
use std::sync::Arc;

/// One synthesized inherent member of a skolem: the requirement it comes
/// from and its signature under the skolem substitution.
#[derive(Debug, Clone)]
pub(crate) struct SkolemMember {
    pub name: Arc<str>,
    /// The interface whose requirement this member is; its stub is the
    /// callee identity a call to the member records.
    pub interface: StructId,
    pub signature: RequirementSignature,
}

impl<H: OrdinaryBodyAnalysisHost> OrdinaryBodyEngine<'_, H> {
    /// Synthesize the members of every skolem among a specialization's type
    /// arguments before its body is analyzed (spec 6.7:20). Non-skolem
    /// arguments are left alone, so an ordinary specialization pays one
    /// host lookup per type argument and nothing else.
    pub(crate) fn prepare_skolems(&mut self, type_args: &[Type]) -> CompileResult<()> {
        for ty in type_args {
            self.prepare_skolem(*ty)?;
        }
        Ok(())
    }

    fn prepare_skolem(&mut self, ty: Type) -> CompileResult<()> {
        let Some(skolem) = ty.as_struct() else {
            return Ok(());
        };
        let Some(display) = self.skolem_display_name(skolem) else {
            return Ok(());
        };
        if self.skolem_prepared(skolem) {
            return Ok(());
        }
        let span = self.skolem_parameter_span(&display);
        // The skolem's durable header asserts exactly its bound set, so the
        // assertions are the bound (spec 6.7:20) and satisfy every nested
        // call's bound check on the same path a program type takes.
        let bounds = self
            .conformance_assertions(ty)
            .into_iter()
            .map(|assertion| assertion.interface)
            .collect::<Vec<_>>();
        let (members, assoc_types) = self.synthesize_skolem_members(ty, &bounds, span)?;
        self.install_skolem_members(skolem, members);
        // An associated type of the skolem is itself a skolem (opaque today:
        // an associated-type requirement carries no bound), prepared so its
        // own lookups answer through the same table.
        for (_, assoc) in assoc_types {
            self.prepare_skolem(assoc)?;
        }
        Ok(())
    }

    /// The union of the requirements of `bounds` and of every interface they
    /// refine, instantiated for the skolem `ty` (spec 6.7:20), with the
    /// skolem's associated types. Two interfaces may declare the same
    /// member only with the same signature (spec 6.7:21).
    pub(crate) fn synthesize_skolem_members(
        &mut self,
        ty: Type,
        bounds: &[StructId],
        span: Span,
    ) -> CompileResult<(Vec<SkolemMember>, Vec<(Arc<str>, Type)>)> {
        let mut interfaces = Vec::new();
        for bound in bounds {
            for interface in self.refinement_closure(*bound) {
                if !interfaces.contains(&interface) {
                    interfaces.push(interface);
                }
            }
        }
        let mut assoc_types: Vec<(Arc<str>, Type)> = Vec::new();
        for interface in &interfaces {
            let Some(facts) = self.interface_facts(*interface) else {
                continue;
            };
            for name in facts.assoc_requirements {
                if assoc_types.iter().any(|(known, _)| *known == name) {
                    continue;
                }
                if let Some(assoc) = self.assoc_type(ty, &name) {
                    assoc_types.push((name, assoc));
                }
            }
        }
        let mut members: Vec<SkolemMember> = Vec::new();
        for interface in &interfaces {
            let Some(facts) = self.interface_facts(*interface) else {
                continue;
            };
            for name in facts.method_requirements {
                let Some(signature) = self.interface_requirement_signature(
                    *interface,
                    &name,
                    ty,
                    &assoc_types,
                    span,
                )?
                else {
                    continue;
                };
                match members.iter().find(|member| member.name == name) {
                    Some(existing) if same_signature(&existing.signature, &signature) => {}
                    Some(_) => {
                        let bound = bounds
                            .iter()
                            .map(|bound| self.interface_display_name(*bound))
                            .collect::<Vec<_>>()
                            .join(" + ");
                        return Err(CompileError::new(
                            ErrorKind::ConflictingBoundRequirements {
                                member: name.to_string(),
                                bound,
                            },
                            span,
                        ));
                    }
                    None => members.push(SkolemMember {
                        name,
                        interface: *interface,
                        signature,
                    }),
                }
            }
        }
        Ok((members, assoc_types))
    }
}

/// Whether two requirement signatures agree on receiver presence and mode,
/// parameter count, each parameter's mode and type, and the result type —
/// the same comparison conformance verification makes (spec 6.7:10).
fn same_signature(left: &RequirementSignature, right: &RequirementSignature) -> bool {
    left.has_self == right.has_self
        && left.self_mode == right.self_mode
        && left.result == right.result
        && left.params.len() == right.params.len()
        && left.params.iter().zip(right.params.iter()).all(
            |((_, left_mode, left_ty), (_, right_mode, right_ty))| {
                left_mode == right_mode && left_ty == right_ty
            },
        )
}

#[cfg(test)]
mod tests {
    use super::super::interfaces::interface_fixtures::{
        T, bounded_type_param, declare_borrow_method, declare_interface,
    };
    use crate::SemanticParameterMode;
    use crate::sema::provider_fixture::{FixtureKey, ProviderFixture, mode_param};
    use rue_error::{ErrorKind, PreviewFeature};

    /// `interface Show { fn show(borrow self) -> i64; }` and
    /// `interface Twice { fn twice(borrow self) -> i64; }` under the preview.
    fn show_and_twice(fixture: &mut ProviderFixture) -> (FixtureKey, FixtureKey) {
        fixture.enable_preview(PreviewFeature::Interfaces);
        let show = declare_interface(fixture, "Show", &[], &[], &["show"]);
        declare_borrow_method(fixture, &show, "show", Vec::new(), T::I64, None);
        let twice = declare_interface(fixture, "Twice", &[], &[], &["twice"]);
        declare_borrow_method(fixture, &twice, "twice", Vec::new(), T::I64, None);
        (show, twice)
    }

    /// Run the skolem check of `fn render(comptime T: <bounds>, borrow x: T) -> i64 { body }`
    /// against the skolem of `bounds`: the production specialization entry
    /// with the skolem as the type argument (spec 6.7:19).
    fn check(
        fixture: &mut ProviderFixture,
        bounds: &[FixtureKey],
        body: &str,
    ) -> rue_error::CompileResult<()> {
        fixture.declare_function(
            "render",
            vec![
                bounded_type_param("T", bounds),
                mode_param("x", T::GenericParameter(0), SemanticParameterMode::Borrow),
            ],
            T::I64,
        );
        let skolem = fixture.declare_skolem("T", bounds, &[]);
        let source = format!("fn render(comptime T: type, borrow x: T) -> i64 {{ {body} }}");
        fixture
            .analyze_specialized_with_types(&source, "render", &[T::Nominal(skolem)], &[])
            .map(|_| ())
    }

    #[test]
    fn a_composed_bound_unions_the_requirements() {
        let mut fixture = ProviderFixture::new();
        let (show, twice) = show_and_twice(&mut fixture);
        check(&mut fixture, &[show, twice], "x.show() + x.twice()")
            .expect("the skolem of `Show + Twice` has both members (spec 6.7:20)");
    }

    #[test]
    fn a_member_outside_the_bound_is_reported_against_the_parameter_name() {
        let mut fixture = ProviderFixture::new();
        let (show, _) = show_and_twice(&mut fixture);
        let error = check(&mut fixture, &[show], "x.show() + x.twice()")
            .err()
            .expect("`twice` is not a requirement of `Show`");
        let ErrorKind::UndefinedMethod {
            type_name,
            method_name,
        } = &error.kind
        else {
            panic!("{error:?}");
        };
        assert_eq!((method_name.as_str(), type_name.as_str()), ("twice", "T"));
    }

    #[test]
    fn refinement_is_walked_when_synthesizing_members() {
        let mut fixture = ProviderFixture::new();
        let (show, _) = show_and_twice(&mut fixture);
        // `Louder: Loud`, `Loud: Show`: the skolem of `Louder` has `show`.
        let loud = declare_interface(&mut fixture, "Loud", &[show.clone()], &[], &["loud"]);
        declare_borrow_method(&mut fixture, &loud, "loud", Vec::new(), T::I64, None);
        let louder = declare_interface(&mut fixture, "Louder", &[loud], &[], &["louder"]);
        declare_borrow_method(&mut fixture, &louder, "louder", Vec::new(), T::I64, None);
        check(&mut fixture, &[louder], "x.show() + x.loud() + x.louder()")
            .expect("every refined interface's requirements are members (spec 6.7:20)");
    }

    #[test]
    fn conflicting_requirements_are_reported_at_the_parameter() {
        let mut fixture = ProviderFixture::new();
        fixture.enable_preview(PreviewFeature::Interfaces);
        let sized = declare_interface(&mut fixture, "Sized", &[], &[], &["len"]);
        declare_borrow_method(&mut fixture, &sized, "len", Vec::new(), T::U64, None);
        let counted = declare_interface(&mut fixture, "Counted", &[], &[], &["len"]);
        declare_borrow_method(&mut fixture, &counted, "len", Vec::new(), T::I64, None);
        let error = check(&mut fixture, &[sized, counted], "0")
            .err()
            .expect("`len` differs between the two interfaces (spec 6.7:21)");
        let ErrorKind::ConflictingBoundRequirements { member, bound } = &error.kind else {
            panic!("{error:?}");
        };
        assert_eq!(
            (member.as_str(), bound.as_str()),
            ("len", "Sized + Counted")
        );
        assert_eq!(
            crate::sema::provider_fixture::error_source_slice(
                "fn render(comptime T: type, borrow x: T) -> i64 { 0 }",
                &error
            ),
            "T",
            "the conflict is anchored at the parameter"
        );
    }

    #[test]
    fn agreeing_requirements_contribute_one_member() {
        let mut fixture = ProviderFixture::new();
        fixture.enable_preview(PreviewFeature::Interfaces);
        let sized = declare_interface(&mut fixture, "Sized", &[], &[], &["len"]);
        declare_borrow_method(&mut fixture, &sized, "len", Vec::new(), T::U64, None);
        let counted = declare_interface(&mut fixture, "Counted", &[], &[], &["len"]);
        declare_borrow_method(&mut fixture, &counted, "len", Vec::new(), T::U64, None);
        check(&mut fixture, &[sized, counted], "@intCast(x.len())")
            .expect("one signature declared twice is one member (spec 6.7:21)");
    }
}
