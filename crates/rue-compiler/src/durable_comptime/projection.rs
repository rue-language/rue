//! Durable anonymous, type, and value projection plus fit policy.
//!
//! Every operation here consumes already-decoded semantic facts. The module is
//! usable without constructing a comptime host and owns no query authority.

use super::diagnostics::*;
use super::lifecycle::*;
use super::*;

/// Engine-shaped semantic input for one anonymous nominal.
///
/// The AIR engine decodes RIR into this descriptor. Keeping the descriptor
/// independent of RIR lets the durable
/// AIR host reuse the exact identity, shape, mode, capture, and effect policy
/// without acquiring a second instruction dispatcher.
#[derive(Debug, Clone)]
pub(crate) struct DurableAnonymousNominalDescriptor {
    /// The producer canonicalizes this key before crossing the boundary. The
    /// kernel validates and preserves it verbatim.
    pub(crate) identity: crate::AnonymousNominalKey,
    pub(crate) shape: DurableAnonymousNominalDescriptorShape,
    pub(crate) type_captures: Arc<[(Arc<str>, DurableType)]>,
    pub(crate) value_captures: Arc<[(Arc<str>, DurableConstValue)]>,
}

#[derive(Debug, Clone)]
pub(crate) enum DurableAnonymousNominalDescriptorShape {
    Struct {
        fields: Arc<[rue_air::ComptimeField<Arc<str>, DurableType>]>,
        methods: Arc<[rue_air::ComptimeMethodDescriptor<Arc<str>, DurableType>]>,
    },
    Enum {
        variants: Arc<[(Arc<str>, Arc<[DurableType]>)]>,
    },
}

/// Construct and publish one anonymous nominal through the durable session's
/// effect authority.  The returned type is the same nominal identity whose
/// complete shape and captures are observed by the session.
pub(crate) fn project_durable_anonymous_nominal(
    session: &mut DurableComptimeSession,
    descriptor: DurableAnonymousNominalDescriptor,
) -> Result<DurableType, DurableComptimeFailure> {
    let expected_kind = match &descriptor.shape {
        DurableAnonymousNominalDescriptorShape::Struct { .. } => {
            rue_air::AnonymousNominalKind::Struct
        }
        DurableAnonymousNominalDescriptorShape::Enum { .. } => rue_air::AnonymousNominalKind::Enum,
    };
    if descriptor.identity.kind != expected_kind {
        return Err(DurableComptimeFailure::resolution(format!(
            "anonymous nominal identity kind {:?} does not match {:?} descriptor shape",
            descriptor.identity.kind, expected_kind
        )));
    }
    let type_captures = canonicalize_captures(descriptor.type_captures, "type")?;
    let value_captures = canonicalize_captures(descriptor.value_captures, "value")?;
    let shape = match descriptor.shape {
        DurableAnonymousNominalDescriptorShape::Struct { fields, methods } => {
            let method_type = |ty: rue_air::ComptimeMethodType<DurableType>| match ty {
                rue_air::ComptimeMethodType::SelfType => {
                    Ok(crate::durable_semantics::DurableAnonymousMethodType::SelfType)
                }
                rue_air::ComptimeMethodType::Concrete(ty) => {
                    Ok(crate::durable_semantics::DurableAnonymousMethodType::Concrete(ty))
                }
                rue_air::ComptimeMethodType::Unsupported(shape) => {
                    Err(DurableComptimeFailure::resolution(format!(
                        "unsupported anonymous method type: {shape}"
                    )))
                }
            };
            let methods = methods
                .iter()
                .map(|method| {
                    let parameters = method
                        .parameters
                        .iter()
                        .map(|parameter| {
                            Ok((
                                method_type(parameter.ty.clone())?,
                                durable_parameter_mode(parameter.mode),
                                parameter.is_comptime,
                            ))
                        })
                        .collect::<Result<Vec<_>, DurableComptimeFailure>>()?;
                    Ok(crate::durable_semantics::DurableAnonymousMethodSignature {
                        name: method.name.clone(),
                        has_self: method.has_self,
                        self_mode: durable_parameter_mode(method.self_mode),
                        returns_borrow: method.returns_borrow,
                        returns_inout: method.returns_inout,
                        parameters: parameters.into(),
                        result: method_type(method.result.clone())?,
                        has_body: true,
                    })
                })
                .collect::<Result<Vec<_>, DurableComptimeFailure>>()?;
            crate::durable_semantics::DurableAnonymousNominalShape::Struct {
                fields: fields
                    .iter()
                    .map(|field| (field.name.clone(), field.ty.clone()))
                    .collect(),
                methods: methods.into(),
            }
        }
        DurableAnonymousNominalDescriptorShape::Enum { variants } => {
            crate::durable_semantics::DurableAnonymousNominalShape::Enum { variants }
        }
    };
    session.observe_anonymous_nominal(DurableAnonymousNominal::new(
        descriptor.identity.clone(),
        shape,
        type_captures,
        value_captures,
    ));
    Ok(DurableType::AnonymousNominal(descriptor.identity))
}

fn canonicalize_captures<T: Clone>(
    captures: Arc<[(Arc<str>, T)]>,
    kind: &str,
) -> Result<Arc<[(Arc<str>, T)]>, DurableComptimeFailure> {
    let mut captures = captures.iter().cloned().collect::<Vec<_>>();
    captures.sort_by(|left, right| left.0.cmp(&right.0));
    for pair in captures.windows(2) {
        if pair[0].0 == pair[1].0 {
            return Err(DurableComptimeFailure::resolution(format!(
                "duplicate {kind} capture `{}` in anonymous nominal",
                pair[0].0
            )));
        }
    }
    Ok(captures.into())
}

fn durable_parameter_mode(
    mode: rue_rir::RirParamMode,
) -> crate::durable_semantics::DurableParameterMode {
    match mode {
        rue_rir::RirParamMode::Normal => crate::durable_semantics::DurableParameterMode::Value,
        rue_rir::RirParamMode::Borrow => crate::durable_semantics::DurableParameterMode::Borrow,
        rue_rir::RirParamMode::Inout => crate::durable_semantics::DurableParameterMode::Inout,
    }
}

/// Convert the canonical type-instance representation into the durable type
/// domain used by call binding. This is kept beside the binding policy so
/// diagnostics and substitution never acquire a second local conversion.
pub(crate) fn durable_type_from_instance_key(
    value: &crate::TypeInstanceKey,
) -> Option<DurableType> {
    use crate::TypeInstanceKey as T;
    use crate::durable_semantics::DurableType as D;
    Some(match value {
        T::I8 => D::I8,
        T::I16 => D::I16,
        T::I32 => D::I32,
        T::I64 => D::I64,
        T::U8 => D::U8,
        T::U16 => D::U16,
        T::U32 => D::U32,
        T::U64 => D::U64,
        T::Bool => D::Bool,
        T::Unit => D::Unit,
        T::Never => D::Never,
        T::ComptimeType => D::ComptimeType,
        T::BuiltinNominal { kind, name } => D::BuiltinNominal {
            name: name.clone(),
            kind: match kind {
                crate::AnonymousNominalKind::Struct => rue_air::SemanticImportNominalKind::Struct,
                crate::AnonymousNominalKind::Enum => rue_air::SemanticImportNominalKind::Enum,
            },
        },
        T::Nominal(crate::NominalInstanceKey::Builtin { kind, name }) => D::BuiltinNominal {
            name: name.clone(),
            kind: match kind {
                crate::AnonymousNominalKind::Struct => rue_air::SemanticImportNominalKind::Struct,
                crate::AnonymousNominalKind::Enum => rue_air::SemanticImportNominalKind::Enum,
            },
        },
        T::Nominal(crate::NominalInstanceKey::Named(key)) => D::Nominal(key.clone()),
        T::Nominal(crate::NominalInstanceKey::Anonymous(key)) => {
            D::AnonymousNominal((**key).clone())
        }
        T::Array { element, len } => D::Array {
            element: Arc::new(durable_type_from_instance_key(element)?),
            len: *len,
        },
        T::Slice { element, name } => D::Slice {
            element: Arc::new(durable_type_from_instance_key(element)?),
            name: name.clone(),
        },
        T::PtrConst(value) => D::PtrConst(Arc::new(durable_type_from_instance_key(value)?)),
        T::PtrMut(value) => D::PtrMut(Arc::new(durable_type_from_instance_key(value)?)),
        T::Module(value) => D::Module(value.clone()),
        T::GenericParameter(index) => D::GenericParameter(*index),
    })
}

fn durable_type_diagnostic_name_kernel(ty: &DurableType) -> String {
    use crate::durable_semantics::DurableType as T;

    fn function_name(function: &crate::FunctionInstanceKey) -> Option<&str> {
        match function {
            crate::FunctionInstanceKey::Definition(key) => Some(key.name()),
            crate::FunctionInstanceKey::Specialization { base, .. } => function_name(base),
            crate::FunctionInstanceKey::AnonymousMember { .. }
            | crate::FunctionInstanceKey::DropGlue(_)
            | crate::FunctionInstanceKey::TestDispatcher => None,
        }
    }

    match ty {
        T::I8 => "i8".to_owned(),
        T::I16 => "i16".to_owned(),
        T::I32 => "i32".to_owned(),
        T::I64 => "i64".to_owned(),
        T::U8 => "u8".to_owned(),
        T::U16 => "u16".to_owned(),
        T::U32 => "u32".to_owned(),
        T::U64 => "u64".to_owned(),
        T::Bool => "bool".to_owned(),
        T::Unit => "()".to_owned(),
        T::Never => "!".to_owned(),
        T::ComptimeType => "type".to_owned(),
        T::BuiltinNominal { name, .. } => name.to_string(),
        T::Nominal(key) => key.name().to_owned(),
        T::AnonymousNominal(key) => match &key.producer {
            crate::StableProducerId::Definition(key) => key.name().to_owned(),
            crate::StableProducerId::Function(function) => {
                let name = function_name(function).unwrap_or("anonymous");
                let applied = key.producer_arguments();
                let mut arguments = applied
                    .map(|applied| {
                        applied
                            .types
                            .iter()
                            .filter_map(durable_type_from_instance_key)
                            .map(|ty| durable_type_diagnostic_name(&ty))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                arguments.extend(
                    applied
                        .into_iter()
                        .flat_map(|applied| applied.values.iter())
                        .map(|value| match value {
                            crate::CanonicalArgumentValue::Integer(value) => value.to_string(),
                            crate::CanonicalArgumentValue::Bool(value) => value.to_string(),
                            crate::CanonicalArgumentValue::Type(value) => {
                                durable_type_from_instance_key(value.as_ref()).map_or_else(
                                    || "type".to_owned(),
                                    |ty| durable_type_diagnostic_name(&ty),
                                )
                            }
                            crate::CanonicalArgumentValue::Function(_) => "function".to_owned(),
                            crate::CanonicalArgumentValue::Unit => "()".to_owned(),
                            crate::CanonicalArgumentValue::String(value) => format!("\"{value}\""),
                        }),
                );
                if arguments.is_empty() {
                    name.to_owned()
                } else {
                    format!("{name}({})", arguments.join(", "))
                }
            }
        },
        T::Array { element, len } => {
            format!("[{}; {len}]", durable_type_diagnostic_name(element))
        }
        T::Slice { name, .. } => name.to_string(),
        T::PtrConst(pointee) => {
            format!("ptr const {}", durable_type_diagnostic_name(pointee))
        }
        T::PtrMut(pointee) => format!("ptr mut {}", durable_type_diagnostic_name(pointee)),
        T::Module(module) => module.to_string(),
        T::GenericParameter(index) => format!("T{index}"),
    }
}

pub(crate) fn durable_type_diagnostic_name(ty: &DurableType) -> String {
    DurableComptimeScalarPolicy::type_name(ty)
}

/// Render an anonymous producer with the declaration-relative comptime
/// parameter schema. `CanonicalArguments` intentionally stores type and value
/// streams separately for identity; the callable schema is the only authority
/// that can safely interleave those streams for presentation.
pub(crate) fn durable_type_diagnostic_name_with_parameters(
    ty: &DurableType,
    parameters: &[crate::durable_semantics::DurableSemanticParameter],
) -> String {
    let DurableType::AnonymousNominal(identity) = ty else {
        return durable_type_diagnostic_name(ty);
    };
    let crate::StableProducerId::Function(function) = &identity.producer else {
        return durable_type_diagnostic_name(ty);
    };
    let definition = match function.as_ref() {
        crate::FunctionInstanceKey::Definition(definition) => Some(definition),
        crate::FunctionInstanceKey::Specialization { base, .. } => {
            fn base_definition(
                function: &crate::FunctionInstanceKey,
            ) -> Option<&crate::StableDefinitionKey> {
                match function {
                    crate::FunctionInstanceKey::Definition(definition) => Some(definition),
                    crate::FunctionInstanceKey::Specialization { base, .. } => {
                        base_definition(base)
                    }
                    crate::FunctionInstanceKey::AnonymousMember { .. }
                    | crate::FunctionInstanceKey::DropGlue(_)
                    | crate::FunctionInstanceKey::TestDispatcher => None,
                }
            }
            base_definition(base)
        }
        crate::FunctionInstanceKey::AnonymousMember { .. }
        | crate::FunctionInstanceKey::DropGlue(_)
        | crate::FunctionInstanceKey::TestDispatcher => None,
    };
    let Some(definition) = definition else {
        return durable_type_diagnostic_name(ty);
    };
    let parameters = parameters
        .iter()
        .map(|parameter| rue_air::CanonicalDisplayParameter {
            is_comptime: parameter.is_comptime,
            is_type: matches!(
                parameter.ty,
                crate::durable_semantics::DurableType::ComptimeType
            ),
        });
    rue_air::format_canonical_application(
        definition.name(),
        parameters,
        identity.producer_arguments(),
        |argument| {
            durable_type_from_instance_key(argument)
                .map(|argument| durable_type_diagnostic_name(&argument))
        },
    )
    .unwrap_or_else(|| durable_type_diagnostic_name(ty))
}

pub(crate) fn inferred_durable_const_type_name(value: &DurableConstValue) -> &'static str {
    match value {
        DurableConstValue::Integer(value) if i32::try_from(*value).is_ok() => "i32",
        DurableConstValue::Integer(value) if i64::try_from(*value).is_ok() => "i64",
        DurableConstValue::Integer(_) => "u64",
        DurableConstValue::Bool(_) => "bool",
        DurableConstValue::Unit => "()",
        DurableConstValue::String(_) => "str",
        DurableConstValue::Type(_) | DurableConstValue::Function(_) => "type",
    }
}

pub(crate) fn substitute_durable_generics(
    ty: &DurableType,
    type_arguments: &[DurableType],
) -> DurableType {
    use crate::durable_semantics::DurableType as T;
    match ty {
        T::GenericParameter(index) => type_arguments
            .get(*index as usize)
            .cloned()
            .unwrap_or_else(|| ty.clone()),
        T::Array { element, len } => T::Array {
            element: Arc::new(substitute_durable_generics(element, type_arguments)),
            len: *len,
        },
        T::Slice { element, name } => T::Slice {
            element: Arc::new(substitute_durable_generics(element, type_arguments)),
            name: name.clone(),
        },
        T::PtrConst(pointee) => T::PtrConst(Arc::new(substitute_durable_generics(
            pointee,
            type_arguments,
        ))),
        T::PtrMut(pointee) => T::PtrMut(Arc::new(substitute_durable_generics(
            pointee,
            type_arguments,
        ))),
        _ => ty.clone(),
    }
}

pub(crate) fn durable_const_fits_type(value: &DurableConstValue, ty: &DurableType) -> bool {
    use crate::durable_semantics::{DurableConstValue as V, DurableType as T};
    match (ty, value) {
        (_, V::Integer(value)) => {
            durable_int_width(ty).is_some_and(|integer| integer.fits_i128(*value))
        }
        (T::Bool, V::Bool(_)) | (T::Unit, V::Unit) => true,
        (T::ComptimeType, V::Type(_)) => true,
        _ => false,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DurableComptimeValueFitFailure {
    CallableAlias,
    IntegerOutOfRange { value: i128, type_name: String },
    TypeMismatch { expected: String, found: String },
}

/// Return the canonical durable value-fit classification, if a reduced value
/// cannot satisfy its declared comptime parameter type. Both expression
/// binding and structured type-constructor reduction consume this policy so
/// callable aliases, integer ranges, and mismatch diagnostics cannot drift.
pub(crate) fn durable_value_fit_failure(
    value: &DurableConstValue,
    expected: &DurableType,
) -> Option<DurableComptimeValueFitFailure> {
    if durable_const_fits_type(value, expected) {
        return None;
    }
    if matches!(value, DurableConstValue::Function(_)) {
        return Some(DurableComptimeValueFitFailure::CallableAlias);
    }
    if let DurableConstValue::Integer(value) = value
        && durable_int_width(expected).is_some()
    {
        return Some(DurableComptimeValueFitFailure::IntegerOutOfRange {
            value: *value,
            type_name: durable_type_diagnostic_name(expected),
        });
    }
    Some(DurableComptimeValueFitFailure::TypeMismatch {
        expected: durable_type_diagnostic_name(expected),
        found: inferred_durable_const_type_name(value).to_owned(),
    })
}

/// Map the shared value-fit classification to the exact semantic channel used
/// by structured durable calls.  Consumers may add presentation-specific
/// wrappers, but they must not reimplement this mapping.
pub(crate) fn durable_structured_value_fit_failure(
    value: &DurableConstValue,
    expected: &DurableType,
) -> Option<SemanticNucleusFailure> {
    durable_value_fit_failure(value, expected).map(|failure| match failure {
        DurableComptimeValueFitFailure::CallableAlias => SemanticNucleusFailure::Resolution(
            Arc::from("a callable alias cannot be passed as a comptime value argument"),
        ),
        DurableComptimeValueFitFailure::IntegerOutOfRange { value, type_name } => {
            SemanticNucleusFailure::Resolution(Arc::from(format!(
                "value {value} is outside the range of type {type_name}"
            )))
        }
        DurableComptimeValueFitFailure::TypeMismatch { expected, found } => {
            SemanticNucleusFailure::Diagnostic(rue_error::ErrorKind::TypeMismatch {
                expected,
                found,
            })
        }
    })
}

pub(crate) fn durable_int_width(
    ty: &DurableType,
) -> Option<rue_air::integer_semantics::IntegerType> {
    use rue_air::integer_semantics::IntegerType;
    let (bits, signed) = match ty {
        DurableType::I8 => (8, true),
        DurableType::I16 => (16, true),
        DurableType::I32 => (32, true),
        DurableType::I64 => (64, true),
        DurableType::U8 => (8, false),
        DurableType::U16 => (16, false),
        DurableType::U32 => (32, false),
        DurableType::U64 => (64, false),
        _ => return None,
    };
    IntegerType::new(bits, signed)
}

/// Stateless scalar policy shared by declaration-time evaluation and the
/// AIR durable host.  It owns no query or RIR state; all inputs are
/// already-reduced durable values and types.
pub(crate) struct DurableComptimeScalarPolicy;

impl DurableComptimeScalarPolicy {
    pub(crate) fn type_name(ty: &DurableType) -> String {
        durable_type_diagnostic_name_kernel(ty)
    }

    #[allow(dead_code)] // activated by the canonical durable AIR host
    pub(crate) fn type_is_unsigned(ty: &DurableType) -> bool {
        Self::type_integer_semantics(ty).is_some_and(|integer| integer.is_unsigned())
    }

    pub(crate) fn type_integer_semantics(
        ty: &DurableType,
    ) -> Option<rue_air::integer_semantics::IntegerType> {
        durable_int_width(ty)
    }

    pub(crate) fn integer_operation_type(
        expected: Option<&DurableType>,
        left: Option<&DurableType>,
        right: Option<&DurableType>,
    ) -> Result<DurableType, DurableComptimeFailure> {
        let fallback = expected
            .filter(|ty| durable_int_width(ty).is_some())
            .cloned()
            .unwrap_or(DurableType::I32);
        match (left, right) {
            (Some(left), Some(right)) if left != right => Err(DurableComptimeFailure::failure(
                SemanticNucleusFailure::Diagnostic(rue_error::ErrorKind::TypeMismatch {
                    expected: durable_type_diagnostic_name(left),
                    found: durable_type_diagnostic_name(right),
                }),
            )),
            (Some(ty), _) | (_, Some(ty)) => Ok(ty.clone()),
            (None, None) => Ok(fallback),
        }
    }

    pub(crate) fn unary_integer_type(
        expected: Option<&DurableType>,
        operand: Option<&DurableType>,
    ) -> Result<DurableType, DurableComptimeFailure> {
        Self::integer_operation_type(expected, operand, None)
    }

    pub(crate) fn require_integer_fits(
        ty: &DurableType,
        value: i128,
    ) -> Result<(), DurableComptimeFailure> {
        let integer = DurableConstValue::Integer(value);
        if durable_const_fits_type(&integer, ty) {
            return Ok(());
        }
        Err(DurableComptimeFailure::integer_literal_overflow(
            &durable_type_diagnostic_name(ty),
            value,
        ))
    }

    pub(crate) fn checked_integer_result(
        ty: &DurableType,
        result: rue_air::integer_semantics::CheckedIntegerResult,
        operation: &str,
    ) -> Result<i128, DurableComptimeFailure> {
        let Some(value) = result.checked() else {
            let type_name = durable_type_diagnostic_name(ty);
            let detail = result.raw().map_or_else(
                || format!("the result does not fit in {type_name}"),
                |value| {
                    format!(
                        "value {value} is out of range for type {type_name}; {value} does not fit in {type_name}"
                    )
                },
            );
            return Err(DurableComptimeFailure::arithmetic_overflow(
                &type_name, operation, &detail,
            ));
        };
        Ok(value)
    }
}

/// Integer-bound policy consumed after AIR has classified the intrinsic. It
/// owns durable diagnostics and integer semantics but no spelling table or
/// instruction/RIR authority.
pub(crate) struct DurableComptimeTypeIntrinsicPolicy;

impl DurableComptimeTypeIntrinsicPolicy {
    pub(crate) fn integer_bound(
        bound: rue_air::ComptimeIntegerBound,
        ty: &DurableType,
    ) -> Result<i128, DurableComptimeFailure> {
        let Some(integer) = DurableComptimeScalarPolicy::type_integer_semantics(ty) else {
            return Err(DurableComptimeFailure::failure(
                SemanticNucleusFailure::Diagnostic(rue_error::ErrorKind::IntrinsicTypeMismatch(
                    Box::new(rue_error::IntrinsicTypeMismatchError {
                        name: bound.as_str().to_owned(),
                        expected: "an integer type".to_owned(),
                        found: ty.kind().display_name().to_owned(),
                    }),
                )),
            ));
        };
        Ok(match bound {
            rue_air::ComptimeIntegerBound::Max => integer.max_i128(),
            rue_air::ComptimeIntegerBound::Min => integer.min_i128(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EvaluatedSemanticConst {
    Value(Arc<TypedSemanticConst>),
    Module(ModuleId),
    TargetEnum(TargetEnumValue),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TargetEnumValue {
    pub(crate) type_name: &'static str,
    pub(crate) variant: &'static str,
}

/// The semantic state an array-repeat count can have before global lookup.
/// `Unbound` is the only state that may proceed to the provider; a shadowed
/// value or type never falls through to a same-named global constant.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)] // runtime-dependent bindings arrive with the durable AIR host
pub(crate) enum DurableComptimeArrayLengthBinding {
    LocalValue(EvaluatedSemanticConst),
    /// A type substitution shadows the name but has no value representation.
    Shadowed,
    RuntimeDependent,
    Unbound,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // consumed by the canonical durable AIR array-length hook
pub(crate) enum DurableComptimeArrayLengthDecision {
    Concrete(u64),
    Shadowed,
    RuntimeDependent,
    ResolveGlobal,
}

/// Diagnostic-free semantic failures from named array-length conversion. Each
/// caller owns the wording/channel adapter appropriate to its query surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DurableComptimeArrayLengthError {
    Module,
    TargetEnum,
    NonInteger,
    Negative(i128),
    TooLarge(i128),
}

/// Convert the AIR-owned lexical fact without dropping any value variant.
/// This is intentionally exhaustive so the AIR host cannot accidentally
/// reinterpret a shadow as an unbound global lookup.
#[allow(dead_code)] // consumed by the canonical durable AIR host
pub(crate) fn durable_array_length_binding_from_air(
    binding: rue_air::ComptimeArrayLengthBinding<EvaluatedSemanticConst>,
) -> DurableComptimeArrayLengthBinding {
    match binding {
        rue_air::ComptimeArrayLengthBinding::LocalValue(value) => {
            DurableComptimeArrayLengthBinding::LocalValue(value)
        }
        rue_air::ComptimeArrayLengthBinding::Shadowed => {
            DurableComptimeArrayLengthBinding::Shadowed
        }
        rue_air::ComptimeArrayLengthBinding::RuntimeDependent => {
            DurableComptimeArrayLengthBinding::RuntimeDependent
        }
        rue_air::ComptimeArrayLengthBinding::Unbound => DurableComptimeArrayLengthBinding::Unbound,
    }
}

/// Apply the canonical named array-length policy to a lexical semantic fact.
/// Concrete conversion and its semantic failures are shared by the durable
/// host paths; global lookup remains the caller's
/// responsibility so dependency observation happens exactly at the existing
/// provider point.
pub(crate) fn classify_durable_named_array_length(
    _name: &str,
    binding: DurableComptimeArrayLengthBinding,
) -> Result<DurableComptimeArrayLengthDecision, DurableComptimeArrayLengthError> {
    match binding {
        DurableComptimeArrayLengthBinding::RuntimeDependent => {
            Ok(DurableComptimeArrayLengthDecision::RuntimeDependent)
        }
        DurableComptimeArrayLengthBinding::Shadowed => {
            Ok(DurableComptimeArrayLengthDecision::Shadowed)
        }
        DurableComptimeArrayLengthBinding::Unbound => {
            Ok(DurableComptimeArrayLengthDecision::ResolveGlobal)
        }
        DurableComptimeArrayLengthBinding::LocalValue(value) => Ok(
            DurableComptimeArrayLengthDecision::Concrete(durable_named_array_length_value(&value)?),
        ),
    }
}

pub(crate) fn durable_named_array_length_failure(
    name: &str,
    error: DurableComptimeArrayLengthError,
) -> SemanticNucleusFailure {
    use DurableComptimeArrayLengthError as E;
    match error {
        E::Module => {
            SemanticNucleusFailure::Resolution(Arc::from("module used where a value is required"))
        }
        E::TargetEnum => SemanticNucleusFailure::Resolution(Arc::from(
            "target descriptor used where a durable const value is required",
        )),
        E::NonInteger | E::Negative(_) | E::TooLarge(_) => {
            SemanticNucleusFailure::Diagnostic(rue_error::ErrorKind::InvalidArrayLength {
                reason: if matches!(error, E::NonInteger) {
                    format!("array length expression '{name}' is not an integer")
                } else {
                    format!("array length expression '{name}' is negative or too large")
                },
            })
        }
    }
}

pub(crate) fn durable_named_array_length_value(
    value: &EvaluatedSemanticConst,
) -> Result<u64, DurableComptimeArrayLengthError> {
    let EvaluatedSemanticConst::Value(value) = value else {
        return Err(match value {
            EvaluatedSemanticConst::Module(_) => DurableComptimeArrayLengthError::Module,
            EvaluatedSemanticConst::TargetEnum(_) => DurableComptimeArrayLengthError::TargetEnum,
            EvaluatedSemanticConst::Value(_) => unreachable!(),
        });
    };
    durable_named_array_length_const(&value.value)
}

pub(crate) fn durable_named_array_length_const(
    value: &DurableConstValue,
) -> Result<u64, DurableComptimeArrayLengthError> {
    let DurableConstValue::Integer(value) = value else {
        return Err(DurableComptimeArrayLengthError::NonInteger);
    };
    durable_named_array_length_integer(*value)
}

pub(crate) fn durable_named_array_length_integer(
    value: i128,
) -> Result<u64, DurableComptimeArrayLengthError> {
    u64::try_from(value).map_err(|_| {
        if value < 0 {
            DurableComptimeArrayLengthError::Negative(value)
        } else {
            DurableComptimeArrayLengthError::TooLarge(value)
        }
    })
}

/// Match semantics shared by the durable evaluator and the AIR host.
/// Durable values deliberately remain narrower than the language's runtime
/// enum algebra: only an exact, unqualified, binding-free target descriptor
/// path is decidable here.
pub(crate) fn durable_match_pattern_matches<N: AsRef<str>>(
    pattern: &ComptimeMatchPattern<N>,
    value: &EvaluatedSemanticConst,
) -> bool {
    match pattern {
        ComptimeMatchPattern::Wildcard => true,
        ComptimeMatchPattern::Integer(pattern) => matches!(
            value,
            EvaluatedSemanticConst::Value(value)
                if matches!(value.value, DurableConstValue::Integer(actual) if actual == *pattern)
        ),
        ComptimeMatchPattern::Bool(pattern) => matches!(
            value,
            EvaluatedSemanticConst::Value(value)
                if matches!(value.value, DurableConstValue::Bool(actual) if actual == *pattern)
        ),
        ComptimeMatchPattern::Path {
            module_qualified: false,
            ctor_qualified: false,
            type_name,
            variant,
            binding_count: 0,
        } => matches!(
            value,
            EvaluatedSemanticConst::TargetEnum(target)
                if type_name.as_ref() == target.type_name && variant.as_ref() == target.variant
        ),
        ComptimeMatchPattern::Path { .. } => false,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TypedSemanticConst {
    pub(crate) value: DurableConstValue,
    /// `None` is reserved for an unconstrained integer literal. Every named
    /// value, local derived from one, and completed operation carries its
    /// canonical semantic type; consumers must never reconstruct it from the
    /// value's magnitude.
    pub(crate) ty: Option<DurableType>,
}

impl TypedSemanticConst {
    pub(crate) fn typed(value: DurableConstValue, ty: DurableType) -> Arc<Self> {
        Arc::new(Self {
            value,
            ty: Some(ty),
        })
    }

    pub(crate) fn integer_literal(value: i128) -> Arc<Self> {
        Arc::new(Self {
            value: DurableConstValue::Integer(value),
            ty: None,
        })
    }
}

/// AIR's type marker for a durable value.
///
/// Keeping the wrapper local avoids implementing an AIR trait for the generic
/// semantic-import type alias, which would violate Rust's orphan rules. The
/// conversion is lossless and intentionally carries no behavior of its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DurableComptimeType(pub(crate) DurableType);

/// Compiler-owned name domain for AIR frames. `Arc<str>` itself is foreign to
/// both crates, so the wrapper is the lossless orphan-rule boundary.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct DurableComptimeName(pub(crate) Arc<str>);

impl DurableComptimeName {
    #[allow(dead_code)]
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<Arc<str>> for DurableComptimeName {
    fn from(value: Arc<str>) -> Self {
        Self(value)
    }
}

impl From<&str> for DurableComptimeName {
    fn from(value: &str) -> Self {
        Self(Arc::from(value))
    }
}

impl AsRef<str> for DurableComptimeName {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl ComptimeName for DurableComptimeName {}

/// The AIR file domain is keyed by the complete owning program identity.
/// A raw span file id or ambient module is insufficient when foreign programs
/// reuse dense file/instruction ids, so frames receive this only after their
/// program has been validated by the session registry.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct DurableComptimeFile(crate::body_query::DurableComptimeProgramKey);

impl DurableComptimeFile {
    pub(super) fn new(program: crate::body_query::DurableComptimeProgramKey) -> Self {
        Self(program)
    }

    #[allow(dead_code)] // consumed by the canonical durable query-root host
    pub(crate) fn program(&self) -> &crate::body_query::DurableComptimeProgramKey {
        &self.0
    }
}

impl ComptimeFile for DurableComptimeFile {}

/// Lossless compiler-owned AIR identity domain. The wrapped producer retains
/// definition and specialized-function identity; the newtype exists only to
/// satisfy the cross-crate trait boundary without violating orphan rules.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct DurableComptimeIdentity(pub(crate) crate::StableProducerId);

impl From<crate::StableProducerId> for DurableComptimeIdentity {
    fn from(value: crate::StableProducerId) -> Self {
        Self(value)
    }
}

impl AsRef<crate::StableProducerId> for DurableComptimeIdentity {
    fn as_ref(&self) -> &crate::StableProducerId {
        &self.0
    }
}

impl ComptimeIdentity for DurableComptimeIdentity {}

/// Anonymous nominal identities are issued by AIR from the active producer
/// and structural anchor.  This wrapper keeps the canonical compiler key
/// opaque while satisfying AIR's identity marker at the host boundary.
#[allow(dead_code)] // consumed by the canonical durable AIR host
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct DurableComptimeAnonymousIdentity(crate::AnonymousNominalKey);

impl ComptimeIdentity for DurableComptimeAnonymousIdentity {}

impl DurableComptimeAnonymousIdentity {
    pub(super) fn new(key: crate::AnonymousNominalKey) -> Self {
        Self(key)
    }

    pub(super) fn key(&self) -> &crate::AnonymousNominalKey {
        &self.0
    }
}

impl From<DurableType> for DurableComptimeType {
    fn from(value: DurableType) -> Self {
        Self(value)
    }
}

impl From<DurableComptimeType> for DurableType {
    fn from(value: DurableComptimeType) -> Self {
        value.0
    }
}

impl AsRef<DurableType> for DurableComptimeType {
    fn as_ref(&self) -> &DurableType {
        &self.0
    }
}

impl ComptimeType for DurableComptimeType {}

impl ComptimeValue for EvaluatedSemanticConst {
    type Type = DurableComptimeType;

    fn integer(value: i128) -> Self {
        Self::Value(TypedSemanticConst::integer_literal(value))
    }

    fn boolean(value: bool) -> Self {
        Self::Value(TypedSemanticConst::typed(
            DurableConstValue::Bool(value),
            DurableType::Bool,
        ))
    }

    fn unit() -> Self {
        Self::Value(TypedSemanticConst::typed(
            DurableConstValue::Unit,
            DurableType::Unit,
        ))
    }

    fn type_value(value: Self::Type) -> Self {
        Self::Value(TypedSemanticConst::typed(
            DurableConstValue::Type(value.0),
            DurableType::ComptimeType,
        ))
    }

    fn as_integer(&self) -> Option<i128> {
        let Self::Value(value) = self else {
            return None;
        };
        match value.value {
            DurableConstValue::Integer(value) => Some(value),
            DurableConstValue::Bool(_)
            | DurableConstValue::Type(_)
            | DurableConstValue::Function(_)
            | DurableConstValue::Unit
            | DurableConstValue::String(_) => None,
        }
    }

    fn as_boolean(&self) -> Option<bool> {
        let Self::Value(value) = self else {
            return None;
        };
        match value.value {
            DurableConstValue::Bool(value) => Some(value),
            DurableConstValue::Integer(_)
            | DurableConstValue::Type(_)
            | DurableConstValue::Function(_)
            | DurableConstValue::Unit
            | DurableConstValue::String(_) => None,
        }
    }

    fn as_type(&self) -> Option<Self::Type> {
        let Self::Value(value) = self else {
            return None;
        };
        match &value.value {
            DurableConstValue::Type(value) => Some(DurableComptimeType(value.clone())),
            DurableConstValue::Integer(_)
            | DurableConstValue::Bool(_)
            | DurableConstValue::Function(_)
            | DurableConstValue::Unit
            | DurableConstValue::String(_) => None,
        }
    }

    fn eligible_for_comptime_capture(&self) -> bool {
        matches!(self, Self::Value(_))
    }

    fn as_integer_type(&self) -> Option<Self::Type> {
        let Self::Value(value) = self else {
            return None;
        };
        if !matches!(value.value, DurableConstValue::Integer(_)) {
            return None;
        }
        value.ty.clone().map(DurableComptimeType)
    }

    fn integer_typed(value: i128, ty: Option<Self::Type>) -> Self {
        match ty {
            Some(ty) => Self::Value(TypedSemanticConst::typed(
                DurableConstValue::Integer(value),
                ty.0,
            )),
            None => Self::integer(value),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    fn value(value: DurableConstValue, ty: Option<DurableType>) -> EvaluatedSemanticConst {
        EvaluatedSemanticConst::Value(Arc::new(TypedSemanticConst { value, ty }))
    }

    #[test]
    fn structured_value_fit_mapping_preserves_each_exact_failure_channel() {
        let alias_key = crate::StableDefinitionKey::from_stable_parts(
            ModuleId::from_logical_path("structured-fit.rue").unwrap(),
            crate::StableDefinitionNamespace::Value,
            crate::StableDefinitionKind::Function,
            "alias",
            None,
        );
        assert_eq!(
            durable_structured_value_fit_failure(
                &DurableConstValue::Function(alias_key),
                &DurableType::I32,
            ),
            Some(SemanticNucleusFailure::Resolution(Arc::from(
                "a callable alias cannot be passed as a comptime value argument",
            )))
        );
        assert_eq!(
            durable_structured_value_fit_failure(
                &DurableConstValue::Integer(i128::MAX),
                &DurableType::I32,
            ),
            Some(SemanticNucleusFailure::Resolution(Arc::from(
                "value 170141183460469231731687303715884105727 is outside the range of type i32",
            )))
        );
        assert_eq!(
            durable_structured_value_fit_failure(&DurableConstValue::Bool(true), &DurableType::I32,),
            Some(SemanticNucleusFailure::Diagnostic(
                rue_error::ErrorKind::TypeMismatch {
                    expected: "i32".into(),
                    found: "bool".into(),
                },
            ))
        );
    }

    #[test]
    fn scalar_policy_preserves_integer_precedence_and_fallbacks() {
        use crate::durable_semantics::DurableType as T;

        assert_eq!(DurableComptimeScalarPolicy::type_name(&T::U16), "u16");
        for ty in [T::U8, T::U16, T::U32, T::U64] {
            assert!(DurableComptimeScalarPolicy::type_is_unsigned(&ty));
        }
        for ty in [T::I8, T::I16, T::I32, T::I64, T::Bool] {
            assert!(!DurableComptimeScalarPolicy::type_is_unsigned(&ty));
        }
        assert_eq!(
            DurableComptimeScalarPolicy::type_integer_semantics(&T::I32)
                .expect("i32 has integer semantics")
                .bits(),
            32
        );

        assert_eq!(
            DurableComptimeScalarPolicy::integer_operation_type(Some(&T::U16), Some(&T::I8), None,)
                .unwrap(),
            T::I8
        );
        assert_eq!(
            DurableComptimeScalarPolicy::unary_integer_type(Some(&T::U16), Some(&T::I8)).unwrap(),
            T::I8
        );
        assert_eq!(
            DurableComptimeScalarPolicy::integer_operation_type(Some(&T::U8), None, None).unwrap(),
            T::U8
        );
        assert_eq!(
            DurableComptimeScalarPolicy::unary_integer_type(None, None).unwrap(),
            T::I32
        );
        assert!(matches!(
            DurableComptimeScalarPolicy::integer_operation_type(
                None,
                Some(&T::I8),
                Some(&T::U8),
            ),
            Err(DurableComptimeFailure::Failure(value))
                if matches!(*value, SemanticNucleusFailure::Diagnostic(
                    rue_error::ErrorKind::TypeMismatch { .. }
                ))
        ));
    }

    #[test]
    fn scalar_policy_preserves_fit_and_arithmetic_diagnostics() {
        use crate::durable_semantics::DurableType as T;

        DurableComptimeScalarPolicy::require_integer_fits(&T::U8, 255).unwrap();
        assert!(matches!(
            DurableComptimeScalarPolicy::require_integer_fits(&T::U8, 256),
            Err(DurableComptimeFailure::Failure(value))
                if matches!(*value, SemanticNucleusFailure::Diagnostic(
                    rue_error::ErrorKind::ComptimeEvaluationFailed { ref reason }
                ) if reason.contains("does not fit in u8"))
        ));
        let integer = rue_air::integer_semantics::IntegerType::new(8, true).unwrap();
        assert_eq!(
            DurableComptimeScalarPolicy::checked_integer_result(
                &T::I8,
                integer.checked_add_report_i128(1, 2),
                "addition",
            )
            .unwrap(),
            3
        );
        assert!(matches!(
            DurableComptimeScalarPolicy::checked_integer_result(
                &T::I8,
                integer.checked_add_report_i128(127, 1),
                "addition",
            ),
            Err(DurableComptimeFailure::Failure(value))
                if matches!(*value, SemanticNucleusFailure::Diagnostic(
                    rue_error::ErrorKind::ComptimeEvaluationFailed { ref reason }
                ) if reason.contains("integer overflow evaluating addition"))
        ));
        assert!(matches!(
            DurableComptimeScalarPolicy::checked_integer_result(
                &T::I8,
                integer.checked_neg_literal_report_i128(129),
                "negation",
            ),
            Err(DurableComptimeFailure::Failure(value))
                if matches!(*value, SemanticNucleusFailure::Diagnostic(
                    rue_error::ErrorKind::ComptimeEvaluationFailed { ref reason }
                ) if reason.contains("integer overflow evaluating negation"))
        ));
    }

    #[test]
    fn type_intrinsic_policy_preserves_all_bounds_gates_and_mismatch() {
        use crate::durable_semantics::DurableType as T;
        assert_eq!(
            rue_air::ComptimeTypeIntrinsic::from_name("require_droppable"),
            Some(rue_air::ComptimeTypeIntrinsic::RequireDroppable)
        );
        assert_eq!(
            rue_air::ComptimeTypeIntrinsic::from_name("require_trivially_droppable"),
            Some(rue_air::ComptimeTypeIntrinsic::RequireTriviallyDroppable)
        );
        assert_eq!(rue_air::ComptimeTypeIntrinsic::from_name("size_of"), None);

        for (ty, min, max) in [
            (T::I8, -128, 127),
            (T::I16, -32_768, 32_767),
            (T::I32, i32::MIN as i128, i32::MAX as i128),
            (T::I64, i64::MIN as i128, i64::MAX as i128),
            (T::U8, 0, 255),
            (T::U16, 0, 65_535),
            (T::U32, 0, u32::MAX as i128),
            (T::U64, 0, u64::MAX as i128),
        ] {
            assert_eq!(
                DurableComptimeTypeIntrinsicPolicy::integer_bound(
                    rue_air::ComptimeIntegerBound::Min,
                    &ty,
                )
                .unwrap(),
                min
            );
            assert_eq!(
                DurableComptimeTypeIntrinsicPolicy::integer_bound(
                    rue_air::ComptimeIntegerBound::Max,
                    &ty,
                )
                .unwrap(),
                max
            );
        }

        let Err(DurableComptimeFailure::Failure(failure)) =
            DurableComptimeTypeIntrinsicPolicy::integer_bound(
                rue_air::ComptimeIntegerBound::Min,
                &T::Bool,
            )
        else {
            panic!("non-integer bound must be a semantic failure");
        };
        assert!(matches!(
            *failure,
            SemanticNucleusFailure::Diagnostic(
                rue_error::ErrorKind::IntrinsicTypeMismatch(ref mismatch)
            ) if mismatch.name == "int_min"
                && mismatch.expected == "an integer type"
                && mismatch.found == "bool"
        ));
    }

    #[test]
    fn scalar_constructors_preserve_the_existing_durable_forms() {
        assert_eq!(
            EvaluatedSemanticConst::integer(7),
            value(DurableConstValue::Integer(7), None)
        );
        assert_eq!(
            EvaluatedSemanticConst::boolean(true),
            value(DurableConstValue::Bool(true), Some(DurableType::Bool))
        );
        assert_eq!(
            EvaluatedSemanticConst::unit(),
            value(DurableConstValue::Unit, Some(DurableType::Unit))
        );
    }

    #[test]
    fn named_array_length_policy_preserves_lexical_and_conversion_channels() {
        let integer = EvaluatedSemanticConst::Value(TypedSemanticConst::typed(
            DurableConstValue::Integer(4),
            DurableType::I32,
        ));
        assert_eq!(
            classify_durable_named_array_length(
                "N",
                DurableComptimeArrayLengthBinding::LocalValue(integer),
            )
            .unwrap(),
            DurableComptimeArrayLengthDecision::Concrete(4)
        );
        assert_eq!(
            classify_durable_named_array_length("N", DurableComptimeArrayLengthBinding::Unbound,)
                .unwrap(),
            DurableComptimeArrayLengthDecision::ResolveGlobal
        );
        assert_eq!(
            classify_durable_named_array_length(
                "N",
                DurableComptimeArrayLengthBinding::RuntimeDependent,
            )
            .unwrap(),
            DurableComptimeArrayLengthDecision::RuntimeDependent
        );

        let non_integer = EvaluatedSemanticConst::Value(TypedSemanticConst::typed(
            DurableConstValue::Bool(true),
            DurableType::Bool,
        ));
        let error = classify_durable_named_array_length(
            "N",
            DurableComptimeArrayLengthBinding::LocalValue(non_integer),
        )
        .unwrap_err();
        assert!(matches!(error, DurableComptimeArrayLengthError::NonInteger));
        assert_eq!(
            classify_durable_named_array_length("N", DurableComptimeArrayLengthBinding::Shadowed,)
                .unwrap(),
            DurableComptimeArrayLengthDecision::Shadowed
        );
        assert!(matches!(
            classify_durable_named_array_length(
                "N",
                DurableComptimeArrayLengthBinding::LocalValue(EvaluatedSemanticConst::Module(
                    ModuleId::from_validated_canonical("root")
                ),),
            )
            .unwrap_err(),
            DurableComptimeArrayLengthError::Module
        ));
        assert!(matches!(
            classify_durable_named_array_length(
                "N",
                DurableComptimeArrayLengthBinding::LocalValue(EvaluatedSemanticConst::TargetEnum(
                    TargetEnumValue {
                        type_name: "Target",
                        variant: "x86_64",
                    }
                ),),
            )
            .unwrap_err(),
            DurableComptimeArrayLengthError::TargetEnum
        ));

        for (value, expected) in [(-1, "negative"), (i128::from(u64::MAX) + 1, "too_large")] {
            let error = durable_named_array_length_value(&EvaluatedSemanticConst::Value(
                TypedSemanticConst::typed(DurableConstValue::Integer(value), DurableType::I64),
            ))
            .unwrap_err();
            match (expected, error) {
                ("negative", DurableComptimeArrayLengthError::Negative(actual)) => {
                    assert_eq!(actual, value)
                }
                ("too_large", DurableComptimeArrayLengthError::TooLarge(actual)) => {
                    assert_eq!(actual, value)
                }
                _ => panic!("unexpected array-length error"),
            }
        }
    }

    #[test]
    fn air_array_length_binding_conversion_is_exhaustive_and_lossless() {
        let value = EvaluatedSemanticConst::integer(7);
        let cases = [
            (
                rue_air::ComptimeArrayLengthBinding::LocalValue(value.clone()),
                DurableComptimeArrayLengthBinding::LocalValue(value),
            ),
            (
                rue_air::ComptimeArrayLengthBinding::Shadowed,
                DurableComptimeArrayLengthBinding::Shadowed,
            ),
            (
                rue_air::ComptimeArrayLengthBinding::RuntimeDependent,
                DurableComptimeArrayLengthBinding::RuntimeDependent,
            ),
            (
                rue_air::ComptimeArrayLengthBinding::Unbound,
                DurableComptimeArrayLengthBinding::Unbound,
            ),
        ];
        for (air, expected) in cases {
            assert_eq!(durable_array_length_binding_from_air(air), expected);
        }
    }

    #[test]
    fn integer_metadata_is_optional_and_lossless() {
        let plain = EvaluatedSemanticConst::integer(9);
        assert_eq!(plain.as_integer(), Some(9));
        assert_eq!(plain.as_integer_type(), None);

        let typed =
            EvaluatedSemanticConst::integer_typed(9, Some(DurableComptimeType(DurableType::I16)));
        assert_eq!(typed.as_integer(), Some(9));
        assert_eq!(
            typed.as_integer_type(),
            Some(DurableComptimeType(DurableType::I16))
        );
        assert_eq!(
            typed,
            value(DurableConstValue::Integer(9), Some(DurableType::I16))
        );
    }

    #[test]
    fn type_values_round_trip_without_reinterpreting_other_variants() {
        let ty = DurableComptimeType(DurableType::Array {
            element: Arc::new(DurableType::U32),
            len: 3,
        });
        let type_value = EvaluatedSemanticConst::type_value(ty.clone());
        assert_eq!(type_value.as_type(), Some(ty.clone()));
        assert_eq!(
            type_value,
            value(
                DurableConstValue::Type(ty.0),
                Some(DurableType::ComptimeType)
            )
        );
        assert_eq!(type_value.as_integer(), None);
        assert_eq!(type_value.as_boolean(), None);
        assert_eq!(type_value.as_integer_type(), None);
    }

    #[test]
    fn module_and_target_enum_values_are_not_scalar_values() {
        let module = EvaluatedSemanticConst::Module(ModuleId::from_logical_path("m").unwrap());
        assert_eq!(module.as_integer(), None);
        assert_eq!(module.as_boolean(), None);
        assert_eq!(module.as_type(), None);
        assert_eq!(module.as_integer_type(), None);

        let target = EvaluatedSemanticConst::TargetEnum(TargetEnumValue {
            type_name: "Os",
            variant: "Macos",
        });
        assert_eq!(target.as_integer(), None);
        assert_eq!(target.as_boolean(), None);
        assert_eq!(target.as_type(), None);
        assert_eq!(target.as_integer_type(), None);
    }

    #[test]
    fn clone_and_conversions_preserve_representation() {
        let ty = DurableType::PtrMut(Arc::new(DurableType::I64));
        let wrapped = DurableComptimeType(ty.clone());
        let unwrapped: DurableType = wrapped.clone().into();
        assert_eq!(unwrapped, ty.clone());
        assert_eq!(DurableComptimeType::from(ty.clone()).as_ref(), &ty);

        let original =
            EvaluatedSemanticConst::integer_typed(-12, Some(DurableComptimeType(ty.clone())));
        assert_eq!(original.clone(), original);
        assert_eq!(original.as_integer_type(), Some(DurableComptimeType(ty)));
    }

    #[test]
    fn durable_match_kernel_preserves_scalar_and_target_pattern_policy() {
        let integer = value(DurableConstValue::Integer(-7), Some(DurableType::I16));
        let boolean = value(DurableConstValue::Bool(true), Some(DurableType::Bool));
        let target = EvaluatedSemanticConst::TargetEnum(TargetEnumValue {
            type_name: "Os",
            variant: "Macos",
        });
        let path = |module_qualified, ctor_qualified, type_name, variant, binding_count| {
            ComptimeMatchPattern::Path {
                module_qualified,
                ctor_qualified,
                type_name: Arc::from(type_name),
                variant: Arc::from(variant),
                binding_count,
            }
        };

        assert!(durable_match_pattern_matches(
            &ComptimeMatchPattern::<Arc<str>>::Wildcard,
            &EvaluatedSemanticConst::Module(ModuleId::from_logical_path("m").unwrap()),
        ));
        assert!(durable_match_pattern_matches(
            &ComptimeMatchPattern::<Arc<str>>::Integer(-7),
            &integer,
        ));
        assert!(!durable_match_pattern_matches(
            &ComptimeMatchPattern::<Arc<str>>::Integer(7),
            &integer,
        ));
        assert!(durable_match_pattern_matches(
            &ComptimeMatchPattern::<Arc<str>>::Bool(true),
            &boolean,
        ));
        assert!(!durable_match_pattern_matches(
            &ComptimeMatchPattern::<Arc<str>>::Bool(false),
            &integer,
        ));
        assert!(!durable_match_pattern_matches(
            &ComptimeMatchPattern::<Arc<str>>::Integer(-7),
            &boolean,
        ));
        assert!(durable_match_pattern_matches(
            &path(false, false, "Os", "Macos", 0),
            &target,
        ));
        for pattern in [
            path(false, false, "Os", "Linux", 0),
            path(false, false, "Arch", "Macos", 0),
            path(true, false, "Os", "Macos", 0),
            path(false, true, "Os", "Macos", 0),
            path(false, false, "Os", "Macos", 1),
        ] {
            assert!(!durable_match_pattern_matches(&pattern, &target));
        }
    }
}
