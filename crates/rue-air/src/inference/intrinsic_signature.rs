//! The one HM constraint shape per intrinsic.
//!
//! Constraint generation used to state an intrinsic's inference behaviour as a
//! branch of a long `name == "..."` chain, which meant an intrinsic missing
//! from the chain silently type-checked as "anything". The shape is stated
//! here instead, once per row of the one intrinsic table, and
//! [`intrinsic_shape`] is an exhaustive match: an intrinsic added to the table
//! without a signature does not compile.
//!
//! The vocabulary describes what constraint generation actually does today.
//! Where two intrinsics that look alike constrain differently — `@arg_len`
//! relates its index contextually while `@arg_ptr` relates its index by
//! equality — the difference is preserved as written, because it is visible in
//! which diagnostics a wrongly typed operand receives.

use rue_builtins::IntrinsicName;

/// A type constraint generation can name without solving anything first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FixedType {
    Unit,
    Never,
    Bool,
    U8,
    U32,
    U64,
    I32,
    I64,
    /// `ptr mut u8`, the byte-pointer type the allocation and process families
    /// speak in.
    MutBytePointer,
}

/// What one operand position contributes to the constraint set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ParamConstraint {
    /// The operand is generated for its own constraints and its effect on
    /// reachability, but nothing relates it to the call.
    Free,
    /// Unified with the signature's shared operand variable.
    Common,
    /// Strict equality against a fixed type.
    Equal(FixedType),
    /// Contextual (coercion-tolerant) relation to a fixed type.
    Contextual(FixedType),
    /// Strict equality against a fixed type, but only for an integer literal:
    /// a wrongly typed non-literal keeps semantic analysis' targeted
    /// diagnostic instead of a generic unification failure.
    EqualIntLiteral(FixedType),
    /// Strict equality against the canonical text type, but only for an
    /// operand that is still a string-literal candidate.
    EqualStringLiteral,
}

/// The constraints an intrinsic's operand list takes, by position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ParamShape {
    /// Operands are not visited at all. These intrinsics are nullary, and a
    /// stray operand keeps whatever type its own node produced rather than
    /// being folded into this call's constraint set.
    Ungenerated,
    /// Every position takes the same constraint, whatever the arity.
    Uniform(ParamConstraint),
    /// Leading positions take their own constraints; every later position
    /// takes `tail`. Arity itself is semantic analysis' diagnostic, so a call
    /// with too few or too many operands still constrains the ones it has.
    Positional {
        head: &'static [ParamConstraint],
        tail: ParamConstraint,
    },
}

impl ParamShape {
    /// The constraint the operand at `index` takes.
    pub(crate) fn at(self, index: usize) -> ParamConstraint {
        match self {
            Self::Ungenerated => ParamConstraint::Free,
            Self::Uniform(constraint) => constraint,
            Self::Positional { head, tail } => head.get(index).copied().unwrap_or(tail),
        }
    }

    /// Whether any position relates to the shared operand variable. Used by
    /// the guard that a signature declares one exactly when it uses one.
    #[cfg(test)]
    pub(crate) fn mentions_common(self) -> bool {
        match self {
            Self::Ungenerated => false,
            Self::Uniform(constraint) => constraint == ParamConstraint::Common,
            Self::Positional { head, tail } => {
                tail == ParamConstraint::Common || head.contains(&ParamConstraint::Common)
            }
        }
    }
}

/// The shared fresh variable an intrinsic's operands unify with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CommonVar {
    /// An ordinary fresh variable.
    Plain,
    /// A fresh variable registered as a float-literal defaulting site, so an
    /// otherwise unconstrained operand takes the `f64` default.
    FloatLiteral,
}

/// The type an intrinsic call evaluates to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResultShape {
    /// A type constraint generation names directly.
    Fixed(FixedType),
    /// The canonical standard-library text type.
    Text,
    /// A compiler-provided enum, named by its source spelling.
    BuiltinEnum(&'static str),
    /// A module value carrying the documented unresolved module id; semantic
    /// analysis resolves the real identity.
    UnresolvedModule,
    /// The signature's shared operand variable.
    Common,
    /// A fresh variable resolved from context, leaving semantic analysis
    /// authoritative for both the type and its diagnostic.
    Fresh,
    /// A fresh variable registered as an integer-literal defaulting site.
    FreshIntLiteral,
    /// A fresh variable registered as a float-literal defaulting site.
    FreshFloatLiteral,
}

/// One intrinsic's complete HM constraint shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct IntrinsicSignature {
    /// The fresh variable operands and result may share, when the intrinsic
    /// has one.
    pub(crate) common: Option<CommonVar>,
    pub(crate) params: ParamShape,
    pub(crate) result: ResultShape,
    /// Whether the result carries an `is_integer` constraint, anchored at the
    /// call's own span rather than an operand's.
    pub(crate) result_is_integer: bool,
    /// Whether the call transfers control and never returns.
    pub(crate) diverges: bool,
}

impl IntrinsicSignature {
    const fn new(params: ParamShape, result: ResultShape) -> Self {
        Self {
            common: None,
            params,
            result,
            result_is_integer: false,
            diverges: false,
        }
    }

    const fn with_common(mut self, common: CommonVar) -> Self {
        self.common = Some(common);
        self
    }

    const fn integer_result(mut self) -> Self {
        self.result_is_integer = true;
        self
    }

    const fn diverging(mut self) -> Self {
        self.diverges = true;
        self
    }
}

/// The pointer-family intrinsics, whose result is read off a concrete operand
/// rather than described by a fixed shape.
///
/// Each publishes an exact pointer or pointee type only when this pass already
/// sees a concrete, well-formed operand; otherwise it leaves a fresh variable
/// so semantic analysis keeps ownership of the pointee reconciliation and of
/// the arity and place diagnostics (RUE-244, RUE-1341, RUE-301).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PointerSignature {
    /// `@ptr_read(p)` / `@ptr_read_unaligned(p)`: the pointee type.
    Read,
    /// `@ptr_write(p, v)` / `@ptr_write_unaligned(p, v)`: unit, with the value
    /// operand related contextually to a concrete pointee.
    Write,
    /// `@ptr_offset(p, i)`: the pointer operand's own type.
    Offset,
    /// `@raw(place)` / `@raw_mut(place)`: a pointer to the operand's place.
    AddrOf { mutable: bool },
    /// `@field_ptr(place.field)`: a `ptr mut` to a field place.
    FieldPtr,
}

/// How constraint generation types one intrinsic call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IntrinsicShape {
    /// The declarative shape above drives generation completely.
    Declared(IntrinsicSignature),
    /// A pointer-family intrinsic whose result depends on operand provenance.
    Pointer(PointerSignature),
}

use CommonVar::{FloatLiteral, Plain};
use FixedType::{Bool, I32, I64, MutBytePointer, Never, U8, U32, U64, Unit};
use ParamConstraint::{Common, Contextual, Equal, EqualIntLiteral, EqualStringLiteral, Free};
use ParamShape::{Positional, Ungenerated, Uniform};
use ResultShape::{BuiltinEnum, Fixed, Fresh, FreshFloatLiteral, FreshIntLiteral, Text};

/// `@realloc`/`@resize`/`@free`: a `ptr mut u8` block followed by byte counts
/// (ADR-0059 Phase 3, RUE-961/RUE-968).
const BLOCK_THEN_BYTE_COUNTS: [ParamConstraint; 1] = [Equal(MutBytePointer)];
/// `@byte_copy`/`@byte_move(dst, src, size)`: the source pointer may be const
/// or mut `u8`, so it is left to semantic analysis' own pointer check rather
/// than pinned here (RUE-964).
const BYTE_COPY_PARAMS: [ParamConstraint; 3] = [Equal(MutBytePointer), Free, Equal(U64)];
/// `@byte_set(dst, byte, size)`.
const BYTE_SET_PARAMS: [ParamConstraint; 3] = [Equal(MutBytePointer), Equal(U8), Equal(U64)];

const fn block_and_byte_counts(result: ResultShape) -> IntrinsicSignature {
    IntrinsicSignature::new(
        Positional {
            head: &BLOCK_THEN_BYTE_COUNTS,
            tail: Equal(U64),
        },
        result,
    )
}

/// The constraint shape of one intrinsic. Exhaustive over the one intrinsic
/// table, so a new row must state how it types.
pub(crate) const fn intrinsic_shape(name: IntrinsicName) -> IntrinsicShape {
    use IntrinsicName as I;
    use IntrinsicShape::{Declared, Pointer};

    let signature = match name {
        // `@intCast`/`@bitCast` take their target from context (RUE-952);
        // `@cast` stays a fresh variable too so semantic analysis can reject it
        // with a clean "use @intCast" diagnostic rather than inference masking
        // it with a type mismatch (RUE-319).
        I::IntCast | I::BitCast | I::Cast => IntrinsicSignature::new(Uniform(Free), Fresh),
        I::IntToFloat | I::FloatCast => IntrinsicSignature::new(Uniform(Free), FreshFloatLiteral),
        I::FloatToInt => IntrinsicSignature::new(Uniform(Free), FreshIntLiteral),
        // The unary float intrinsics return their operand's type; an
        // unconstrained literal operand takes the `f64` default (ADR-0065 §7).
        I::Sqrt | I::Floor | I::Ceil | I::Trunc | I::Round => {
            IntrinsicSignature::new(Uniform(Common), ResultShape::Common).with_common(FloatLiteral)
        }
        I::TotalCmp => {
            IntrinsicSignature::new(Uniform(Common), Fixed(I32)).with_common(FloatLiteral)
        }
        // `@panic` aborts and never returns, so its expression type is `!`, a
        // control-transfer form that participates in never coercion (spec
        // 3.4:2, 4.13:5c; formal core §5.7; RUE-512). A text operand is left
        // unconstrained so a literal takes the canonical `str` default when
        // std is not imported.
        I::Panic => IntrinsicSignature::new(Uniform(Free), Fixed(Never)).diverging(),
        // `@assert` is NOT never-typed: on the success path it returns and
        // evaluates to `()`, so its static type is unit on both paths (spec
        // 4.13:5d).
        I::Assert => IntrinsicSignature::new(Uniform(Free), Fixed(Unit)),
        // `@assert_eq(l, r)` / `@assert_ne(l, r)`: the two operands share one
        // type and the call evaluates to `()` on the path that continues (spec
        // 4.13:5f). Unifying the operands is what lets `@assert_eq(port, 8080)`
        // give the literal the other side's type; semantic analysis then checks
        // that type supports `==`.
        I::AssertEq | I::AssertNe => {
            IntrinsicSignature::new(Uniform(Common), Fixed(Unit)).with_common(Plain)
        }
        // `@read_line` returns `Option(StrBuf)` (RUE-6, ADR-0038). The concrete
        // Option type comes from context, so the result is a fresh variable and
        // semantic analysis validates the resolved shape.
        I::ReadLine => IntrinsicSignature::new(Ungenerated, Fresh),
        // `@to_string(n)` takes any integer width and returns text (RUE-17
        // Phase 1, ADR-0035; RUE-314). No integer constraint is added, so a
        // bare literal defaults to `i32` like everywhere else.
        I::ToString => IntrinsicSignature::new(Uniform(Free), Text),
        // `@parse_*` takes text and returns `Option(int)` (RUE-6, ADR-0038).
        // The payload comes from context; only a still-contextual string
        // literal is pinned to the canonical text type.
        I::ParseI32 | I::ParseI64 | I::ParseU32 | I::ParseU64 => {
            IntrinsicSignature::new(Uniform(EqualStringLiteral), Fresh)
        }
        I::RandomU32 => IntrinsicSignature::new(Ungenerated, Fixed(U32)),
        I::RandomU64 => IntrinsicSignature::new(Ungenerated, Fixed(U64)),
        // `@arg_count` / `@env_count`: nullary, `u64` (RUE-935).
        I::ArgCount | I::EnvCount => IntrinsicSignature::new(Ungenerated, Fixed(U64)),
        // `@arg_len(i)` / `@env_len(i)`: a single `u64` index, `u64` result
        // (RUE-935). The index is related contextually, unlike the pointer
        // siblings below, so a coercible operand is accepted here and a
        // mismatch keeps semantic analysis' own message.
        I::ArgLen | I::EnvLen => IntrinsicSignature::new(Uniform(Contextual(U64)), Fixed(U64)),
        // `@arg_ptr(i)` / `@env_ptr(i)`: the same index, related by strict
        // equality, returning `ptr mut u8` (RUE-935).
        I::ArgPtr | I::EnvPtr => {
            IntrinsicSignature::new(Uniform(Equal(U64)), Fixed(MutBytePointer))
        }
        // `@wrapping_add/sub/mul(a, b)`: both operands and the result share one
        // integer type — the same equality-and-integer constraints as checked
        // `+`/`-`/`*`, minus the text-concat overload (RUE-647).
        I::WrappingAdd | I::WrappingSub | I::WrappingMul => {
            IntrinsicSignature::new(Uniform(Common), ResultShape::Common)
                .with_common(Plain)
                .integer_result()
        }
        // `@syscall(number, ...)`: an integer-literal operand sees the declared
        // `u64` parameter type here (RUE-954), so `@syscall(32, 1)` works
        // without pre-binding. Only literals are constrained: a wrongly typed
        // non-literal keeps semantic analysis' targeted E0702.
        I::Syscall => IntrinsicSignature::new(Uniform(EqualIntLiteral(U64)), Fixed(I64)),
        I::PtrToInt => IntrinsicSignature::new(Uniform(Free), Fixed(U64)),
        // The trusted `@place(ptr)` bridge is pointer-shaped until
        // accessor-yield analysis turns it into an indirect place.
        I::Place => IntrinsicSignature::new(Uniform(Free), Fresh),
        // `@alloc(size, align)` and its zeroing twin: both operands are
        // physical byte counts, so the result type is fixed rather than
        // context-inferred (ADR-0059 Phase 3, RUE-961/RUE-968).
        I::Alloc | I::AllocZeroed => {
            IntrinsicSignature::new(Uniform(Equal(U64)), Fixed(MutBytePointer))
        }
        I::Realloc => block_and_byte_counts(Fixed(MutBytePointer)),
        I::Resize => block_and_byte_counts(Fixed(Bool)),
        I::Free => block_and_byte_counts(Fixed(Unit)),
        I::ByteCopy | I::ByteMove => IntrinsicSignature::new(
            Positional {
                head: &BYTE_COPY_PARAMS,
                tail: Free,
            },
            Fixed(Unit),
        ),
        I::ByteSet => IntrinsicSignature::new(
            Positional {
                head: &BYTE_SET_PARAMS,
                tail: Free,
            },
            Fixed(Unit),
        ),
        // `@int_to_ptr(addr)` returns a pointer type inferred from context, but
        // its address operand is the declared `u64` of spec 9.2:6c. An
        // integer-literal operand sees that type here (RUE-2167), so
        // `@int_to_ptr(0)` works without pre-binding the address to a `u64`.
        // Only literals are constrained, as for `@syscall`: a wrongly typed
        // non-literal keeps semantic analysis' targeted E0702.
        I::IntToPtr => IntrinsicSignature::new(Uniform(EqualIntLiteral(U64)), Fresh),
        I::TargetArch => IntrinsicSignature::new(Ungenerated, BuiltinEnum("Arch")),
        I::TargetOs => IntrinsicSignature::new(Ungenerated, BuiltinEnum("Os")),
        I::TargetDataModel => IntrinsicSignature::new(Ungenerated, BuiltinEnum("DataModel")),
        // `@import("path")` is a module value. Resolving the path to a real
        // module id needs the registry, which inference does not have, so the
        // documented sentinel is used: inference only needs module-ness, and
        // semantic analysis resolves the member with the receiver's real
        // identity. Returning unit here made a member call on the binding
        // unresolvable (RUE-142).
        I::Import => IntrinsicSignature::new(Uniform(Free), ResultShape::UnresolvedModule),
        // The remaining value intrinsics evaluate to unit.
        I::Dbg | I::Drop | I::TestPreviewGate => {
            IntrinsicSignature::new(Uniform(Free), Fixed(Unit))
        }
        // The type-position intrinsics only reach value-intrinsic generation
        // when their argument shape was wrong (RUE-788). A fresh variable keeps
        // semantic analysis' unknown-intrinsic rejection unmasked, exactly as a
        // context-inferred result does.
        I::SizeOf
        | I::AlignOf
        | I::RequireDroppable
        | I::RequireTriviallyDroppable
        | I::IntMax
        | I::IntMin
        | I::OffsetOf => IntrinsicSignature::new(Uniform(Free), Fresh),

        I::PtrRead | I::PtrReadUnaligned => return Pointer(PointerSignature::Read),
        I::PtrWrite | I::PtrWriteUnaligned => return Pointer(PointerSignature::Write),
        I::PtrOffset => return Pointer(PointerSignature::Offset),
        I::Raw => return Pointer(PointerSignature::AddrOf { mutable: false }),
        I::RawMut => return Pointer(PointerSignature::AddrOf { mutable: true }),
        I::FieldPtr => return Pointer(PointerSignature::FieldPtr),
    };
    Declared(signature)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_intrinsic_row_has_a_signature() {
        for name in IntrinsicName::ALL {
            // Exhaustiveness is a compile-time property of `intrinsic_shape`;
            // this evaluates every row so a future table-driven lookup cannot
            // regress into a partial one.
            let _ = intrinsic_shape(name);
        }
    }

    #[test]
    fn a_shared_operand_variable_is_declared_wherever_it_is_used() {
        for name in IntrinsicName::ALL {
            let IntrinsicShape::Declared(signature) = intrinsic_shape(name) else {
                continue;
            };
            let uses_common =
                signature.params.mentions_common() || signature.result == ResultShape::Common;
            assert_eq!(
                uses_common,
                signature.common.is_some(),
                "{name:?} must declare a shared operand variable exactly when it uses one"
            );
        }
    }

    #[test]
    fn only_panic_diverges_and_only_wrapping_arithmetic_constrains_its_result() {
        let diverging: Vec<_> = IntrinsicName::ALL
            .into_iter()
            .filter(|name| match intrinsic_shape(*name) {
                IntrinsicShape::Declared(signature) => signature.diverges,
                IntrinsicShape::Pointer(_) => false,
            })
            .collect();
        assert_eq!(diverging, vec![IntrinsicName::Panic]);

        let integer_results: Vec<_> = IntrinsicName::ALL
            .into_iter()
            .filter(|name| match intrinsic_shape(*name) {
                IntrinsicShape::Declared(signature) => signature.result_is_integer,
                IntrinsicShape::Pointer(_) => false,
            })
            .collect();
        assert_eq!(
            integer_results,
            vec![
                IntrinsicName::WrappingAdd,
                IntrinsicName::WrappingSub,
                IntrinsicName::WrappingMul,
            ]
        );
    }

    #[test]
    fn the_process_families_keep_their_distinct_index_constraints() {
        // `@arg_len`/`@env_len` relate their index contextually while
        // `@arg_ptr`/`@env_ptr` relate theirs by strict equality. The two are
        // observably different — a coercible operand is accepted by one and
        // rejected by the other — so the difference is pinned rather than
        // quietly unified.
        for name in [IntrinsicName::ArgLen, IntrinsicName::EnvLen] {
            let IntrinsicShape::Declared(signature) = intrinsic_shape(name) else {
                panic!("{name:?} is a declared signature");
            };
            assert_eq!(signature.params.at(0), Contextual(U64));
        }
        for name in [IntrinsicName::ArgPtr, IntrinsicName::EnvPtr] {
            let IntrinsicShape::Declared(signature) = intrinsic_shape(name) else {
                panic!("{name:?} is a declared signature");
            };
            assert_eq!(signature.params.at(0), Equal(U64));
        }
    }

    #[test]
    fn positional_shapes_extend_their_tail_past_the_declared_head() {
        let IntrinsicShape::Declared(free) = intrinsic_shape(IntrinsicName::Free) else {
            panic!("@free is a declared signature");
        };
        assert_eq!(free.params.at(0), Equal(MutBytePointer));
        for index in 1..8 {
            assert_eq!(free.params.at(index), Equal(U64));
        }

        let IntrinsicShape::Declared(byte_copy) = intrinsic_shape(IntrinsicName::ByteCopy) else {
            panic!("@byte_copy is a declared signature");
        };
        assert_eq!(byte_copy.params.at(1), Free);
        assert_eq!(byte_copy.params.at(2), Equal(U64));
        assert_eq!(byte_copy.params.at(3), Free);
    }
}
