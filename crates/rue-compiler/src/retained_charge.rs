//! Allocator-independent retained-artifact accounting.
//!
//! Inline storage is counted by its owning terminal or container exactly once.
//! These implementations count only recursively reachable owned allocations by
//! logical length. Shared `Arc`/`Rc` allocations are charged in full on every
//! path that reaches them; no allocation-identity registry or allocator capacity
//! enters the policy.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::rc::Rc;
use std::sync::Arc;

use rue_parser::ast;

pub(crate) trait RetainedCharge {
    fn retained_charge(&self) -> u64;
}

fn owned_slice_charge<T>(values: &[T], nested: impl Fn(&T) -> u64) -> u64 {
    values
        .iter()
        .fold((std::mem::size_of_val(values)) as u64, |charge, value| {
            charge.saturating_add(nested(value))
        })
}

fn boxed_charge<T>(value: &Box<T>, nested: impl Fn(&T) -> u64) -> u64 {
    (std::mem::size_of::<T>() as u64).saturating_add(nested(value))
}

fn directives_charge(directives: &ast::Directives) -> u64 {
    let inline_or_heap = if directives.spilled() {
        std::mem::size_of_val(directives.as_slice()) as u64
    } else {
        0
    };
    directives.iter().fold(inline_or_heap, |charge, directive| {
        charge.saturating_add(owned_slice_charge(&directive.args, |_| 0))
    })
}

fn type_charge(ty: &ast::TypeExpr) -> u64 {
    use ast::TypeExpr;
    match ty {
        TypeExpr::Named(_) | TypeExpr::Unit(_) | TypeExpr::Never(_) => 0,
        TypeExpr::Qualified { segments, .. } => owned_slice_charge(segments, |_| 0),
        TypeExpr::Array {
            element, length, ..
        } => boxed_charge(element, type_charge).saturating_add(array_length_charge(length)),
        TypeExpr::Slice { element, .. }
        | TypeExpr::PointerConst {
            pointee: element, ..
        }
        | TypeExpr::PointerMut {
            pointee: element, ..
        } => boxed_charge(element, type_charge),
        TypeExpr::AnonymousStruct {
            fields, methods, ..
        } => owned_slice_charge(fields, |field| type_charge(&field.ty))
            .saturating_add(owned_slice_charge(methods, method_charge)),
        TypeExpr::AnonymousEnum { variants, .. } => {
            owned_slice_charge(variants, enum_variant_charge)
        }
        TypeExpr::TypeCall { args, .. } => owned_slice_charge(args, type_charge),
        TypeExpr::QualifiedTypeCall { segments, args, .. } => owned_slice_charge(segments, |_| 0)
            .saturating_add(owned_slice_charge(args, type_charge)),
        TypeExpr::StrFixed { .. } | TypeExpr::IntArg { .. } => 0,
    }
}

fn array_length_charge(length: &ast::ArrayLength) -> u64 {
    match length {
        ast::ArrayLength::Call { args, .. } => owned_slice_charge(args, array_length_charge),
        ast::ArrayLength::Literal(_) | ast::ArrayLength::Named(_) => 0,
    }
}

fn param_charge(param: &ast::Param) -> u64 {
    type_charge(&param.ty)
}

fn enum_variant_charge(variant: &ast::EnumVariant) -> u64 {
    owned_slice_charge(&variant.payload, type_charge)
}

fn method_charge(method: &ast::Method) -> u64 {
    directives_charge(&method.directives)
        .saturating_add(owned_slice_charge(&method.params, param_charge))
        .saturating_add(method.return_type.as_ref().map_or(0, type_charge))
        .saturating_add(expr_charge(&method.body))
}

fn call_arg_charge(arg: &ast::CallArg) -> u64 {
    expr_charge(&arg.expr)
}

fn block_charge(block: &ast::BlockExpr) -> u64 {
    owned_slice_charge(&block.statements, statement_charge)
        .saturating_add(boxed_charge(&block.expr, expr_charge))
}

fn pattern_charge(pattern: &ast::Pattern) -> u64 {
    match pattern {
        ast::Pattern::Path(path) => path
            .base
            .as_ref()
            .map_or(0, |base| boxed_charge(base, expr_charge))
            .saturating_add(
                path.ctor_args
                    .as_ref()
                    .map_or(0, |args| owned_slice_charge(args, call_arg_charge)),
            )
            .saturating_add(owned_slice_charge(&path.bindings, |_| 0)),
        ast::Pattern::Wildcard(_)
        | ast::Pattern::Int(_)
        | ast::Pattern::NegInt(_)
        | ast::Pattern::Bool(_) => 0,
    }
}

fn statement_charge(statement: &ast::Statement) -> u64 {
    match statement {
        ast::Statement::Let(statement) => directives_charge(&statement.directives)
            .saturating_add(statement.ty.as_ref().map_or(0, type_charge))
            .saturating_add(boxed_charge(&statement.init, expr_charge)),
        ast::Statement::Assign(statement) => {
            let target = match &statement.target {
                ast::AssignTarget::Var(_) => 0,
                ast::AssignTarget::Field(field) => boxed_charge(&field.base, expr_charge),
                ast::AssignTarget::Index(index) => boxed_charge(&index.base, expr_charge)
                    .saturating_add(boxed_charge(&index.index, expr_charge)),
            };
            target.saturating_add(boxed_charge(&statement.value, expr_charge))
        }
        ast::Statement::Expr(expr) => expr_charge(expr),
    }
}

fn expr_charge(expr: &ast::Expr) -> u64 {
    use ast::Expr;
    match expr {
        Expr::Int(_)
        | Expr::Float(_)
        | Expr::String(_)
        | Expr::Bool(_)
        | Expr::Unit(_)
        | Expr::Ident(_)
        | Expr::Continue(_)
        | Expr::SelfExpr(_)
        | Expr::Error(_) => 0,
        Expr::Binary(binary) => boxed_charge(&binary.left, expr_charge)
            .saturating_add(boxed_charge(&binary.right, expr_charge)),
        Expr::Unary(unary) => boxed_charge(&unary.operand, expr_charge),
        Expr::Paren(paren) => boxed_charge(&paren.inner, expr_charge),
        Expr::Block(block) => block_charge(block),
        Expr::If(value) => boxed_charge(&value.cond, expr_charge)
            .saturating_add(block_charge(&value.then_block))
            .saturating_add(value.else_block.as_ref().map_or(0, block_charge)),
        Expr::Match(value) => boxed_charge(&value.scrutinee, expr_charge).saturating_add(
            owned_slice_charge(&value.arms, |arm| {
                pattern_charge(&arm.pattern).saturating_add(boxed_charge(&arm.body, expr_charge))
            }),
        ),
        Expr::While(value) => {
            boxed_charge(&value.cond, expr_charge).saturating_add(block_charge(&value.body))
        }
        Expr::Loop(value) => block_charge(&value.body),
        Expr::For(value) => {
            boxed_charge(&value.iterable, expr_charge).saturating_add(block_charge(&value.body))
        }
        Expr::Call(value) => owned_slice_charge(&value.args, call_arg_charge),
        Expr::Break(value) => value
            .value
            .as_ref()
            .map_or(0, |value| boxed_charge(value, expr_charge)),
        Expr::Return(value) => value
            .value
            .as_ref()
            .map_or(0, |value| boxed_charge(value, expr_charge)),
        Expr::Yield(value) => boxed_charge(&value.value, expr_charge),
        Expr::StructLit(value) => value
            .base
            .as_ref()
            .map_or(0, |base| boxed_charge(base, expr_charge))
            .saturating_add(
                value
                    .ctor_args
                    .as_ref()
                    .map_or(0, |args| owned_slice_charge(args, call_arg_charge)),
            )
            .saturating_add(owned_slice_charge(&value.fields, |field| {
                boxed_charge(&field.value, expr_charge)
            })),
        Expr::Field(value) => boxed_charge(&value.base, expr_charge),
        Expr::MethodCall(value) => boxed_charge(&value.receiver, expr_charge)
            .saturating_add(owned_slice_charge(&value.args, call_arg_charge)),
        Expr::Try(value) => boxed_charge(&value.operand, expr_charge),
        Expr::IntrinsicCall(value) => owned_slice_charge(&value.args, |arg| match arg {
            ast::IntrinsicArg::Expr(expr) => expr_charge(expr),
            ast::IntrinsicArg::Type(ty) => type_charge(ty),
        }),
        Expr::ArrayLit(value) => owned_slice_charge(&value.elements, expr_charge)
            .saturating_add(value.repeat.as_ref().map_or(0, array_length_charge)),
        Expr::Index(value) => boxed_charge(&value.base, expr_charge)
            .saturating_add(boxed_charge(&value.index, expr_charge)),
        Expr::Path(value) => value
            .base
            .as_ref()
            .map_or(0, |base| boxed_charge(base, expr_charge)),
        Expr::Comptime(value) => boxed_charge(&value.expr, expr_charge),
        Expr::Checked(value) => boxed_charge(&value.expr, expr_charge),
        Expr::TypeLit(value) => type_charge(&value.type_expr),
    }
}

fn item_charge(item: &ast::Item) -> u64 {
    match item {
        ast::Item::Function(function) => directives_charge(&function.directives)
            .saturating_add(owned_slice_charge(&function.params, param_charge))
            .saturating_add(function.return_type.as_ref().map_or(0, type_charge))
            .saturating_add(expr_charge(&function.body))
            .saturating_add(function.export_abi.retained_charge()),
        ast::Item::Struct(value) => directives_charge(&value.directives)
            .saturating_add(owned_slice_charge(&value.fields, |field| {
                type_charge(&field.ty)
            }))
            .saturating_add(owned_slice_charge(&value.methods, method_charge)),
        ast::Item::Enum(value) => owned_slice_charge(&value.variants, enum_variant_charge),
        ast::Item::DropFn(value) => expr_charge(&value.body),
        ast::Item::Extern(value) => value
            .abi
            .retained_charge()
            .saturating_add(owned_slice_charge(&value.fns, |function| {
                owned_slice_charge(&function.params, param_charge)
                    .saturating_add(function.return_type.as_ref().map_or(0, type_charge))
            })),
        ast::Item::Const(value) => directives_charge(&value.directives)
            .saturating_add(value.ty.as_ref().map_or(0, type_charge))
            .saturating_add(boxed_charge(&value.init, expr_charge)),
        ast::Item::Error(_) => 0,
    }
}

impl RetainedCharge for ast::Ast {
    fn retained_charge(&self) -> u64 {
        owned_slice_charge(&self.items, item_charge)
    }
}

macro_rules! zero_charge {
    ($($ty:ty),* $(,)?) => {
        $(impl RetainedCharge for $ty {
            fn retained_charge(&self) -> u64 { 0 }
        })*
    };
}

zero_charge!(
    (),
    bool,
    char,
    u8,
    u16,
    u32,
    u64,
    u128,
    usize,
    i8,
    i16,
    i32,
    i64,
    i128,
    isize,
    f32,
    f64
);

zero_charge!(
    crate::DefinitionNamespace,
    rue_span::FileId,
    rue_span::Span,
    rue_query::Revision,
    rue_cfg::BlockId
);

impl<'a> RetainedCharge for Cow<'a, str> {
    fn retained_charge(&self) -> u64 {
        match self {
            Cow::Owned(value) => value.retained_charge(),
            Cow::Borrowed(_) => 0,
        }
    }
}

impl RetainedCharge for str {
    fn retained_charge(&self) -> u64 {
        0
    }
}

impl RetainedCharge for String {
    fn retained_charge(&self) -> u64 {
        self.len() as u64
    }
}

impl<T: RetainedCharge + ?Sized> RetainedCharge for Arc<T> {
    fn retained_charge(&self) -> u64 {
        (std::mem::size_of_val(self.as_ref()) as u64)
            .saturating_add(self.as_ref().retained_charge())
    }
}

impl<T: RetainedCharge + ?Sized> RetainedCharge for Rc<T> {
    fn retained_charge(&self) -> u64 {
        (std::mem::size_of_val(self.as_ref()) as u64)
            .saturating_add(self.as_ref().retained_charge())
    }
}

impl<T: RetainedCharge + ?Sized> RetainedCharge for Box<T> {
    fn retained_charge(&self) -> u64 {
        (std::mem::size_of_val(self.as_ref()) as u64)
            .saturating_add(self.as_ref().retained_charge())
    }
}

impl<T: RetainedCharge> RetainedCharge for [T] {
    fn retained_charge(&self) -> u64 {
        self.iter().fold(0_u64, |charge, value| {
            charge.saturating_add(value.retained_charge())
        })
    }
}

impl<T: RetainedCharge> RetainedCharge for Vec<T> {
    fn retained_charge(&self) -> u64 {
        (self.len().saturating_mul(std::mem::size_of::<T>()) as u64)
            .saturating_add(self.as_slice().retained_charge())
    }
}

impl<T: RetainedCharge> RetainedCharge for VecDeque<T> {
    fn retained_charge(&self) -> u64 {
        (self.len().saturating_mul(std::mem::size_of::<T>()) as u64).saturating_add(
            self.iter().fold(0_u64, |charge, value| {
                charge.saturating_add(value.retained_charge())
            }),
        )
    }
}

impl<T: RetainedCharge> RetainedCharge for Option<T> {
    fn retained_charge(&self) -> u64 {
        self.as_ref().map_or(0, RetainedCharge::retained_charge)
    }
}

impl<T: RetainedCharge, E: RetainedCharge> RetainedCharge for Result<T, E> {
    fn retained_charge(&self) -> u64 {
        match self {
            Ok(value) => value.retained_charge(),
            Err(error) => error.retained_charge(),
        }
    }
}

impl<K: RetainedCharge, V: RetainedCharge> RetainedCharge for BTreeMap<K, V> {
    fn retained_charge(&self) -> u64 {
        let entries = self.len().saturating_mul(std::mem::size_of::<(K, V)>()) as u64;
        self.iter().fold(entries, |charge, (key, value)| {
            charge
                .saturating_add(key.retained_charge())
                .saturating_add(value.retained_charge())
        })
    }
}

impl<T: RetainedCharge> RetainedCharge for BTreeSet<T> {
    fn retained_charge(&self) -> u64 {
        let entries = self.len().saturating_mul(std::mem::size_of::<T>()) as u64;
        self.iter().fold(entries, |charge, value| {
            charge.saturating_add(value.retained_charge())
        })
    }
}

impl<K: RetainedCharge, V: RetainedCharge> RetainedCharge for HashMap<K, V> {
    fn retained_charge(&self) -> u64 {
        let entries = self.len().saturating_mul(std::mem::size_of::<(K, V)>()) as u64;
        self.iter().fold(entries, |charge, (key, value)| {
            charge
                .saturating_add(key.retained_charge())
                .saturating_add(value.retained_charge())
        })
    }
}

impl<T: RetainedCharge> RetainedCharge for HashSet<T> {
    fn retained_charge(&self) -> u64 {
        let entries = self.len().saturating_mul(std::mem::size_of::<T>()) as u64;
        self.iter().fold(entries, |charge, value| {
            charge.saturating_add(value.retained_charge())
        })
    }
}

impl<T: RetainedCharge> RetainedCharge for crate::shared_segments::SharedSegments<T> {
    fn retained_charge(&self) -> u64 {
        let inline = self.len().saturating_mul(std::mem::size_of::<T>()) as u64;
        self.iter().fold(inline, |charge, value| {
            charge.saturating_add(value.retained_charge())
        })
    }
}

impl<T: RetainedCharge> RetainedCharge for crate::shared_segments::SharedList<T> {
    fn retained_charge(&self) -> u64 {
        let inline = self.len().saturating_mul(std::mem::size_of::<T>()) as u64;
        self.iter().fold(inline, |charge, value| {
            charge.saturating_add(value.retained_charge())
        })
    }
}

impl<A: RetainedCharge, B: RetainedCharge> RetainedCharge for (A, B) {
    fn retained_charge(&self) -> u64 {
        self.0
            .retained_charge()
            .saturating_add(self.1.retained_charge())
    }
}

impl<A: RetainedCharge, B: RetainedCharge, C: RetainedCharge> RetainedCharge for (A, B, C) {
    fn retained_charge(&self) -> u64 {
        self.0
            .retained_charge()
            .saturating_add(self.1.retained_charge())
            .saturating_add(self.2.retained_charge())
    }
}

impl RetainedCharge for rue_query::InputIdentity {
    fn retained_charge(&self) -> u64 {
        (self.family().len() + self.key().len()) as u64
    }
}

impl RetainedCharge for rue_query::InputObservation {
    fn retained_charge(&self) -> u64 {
        self.input.retained_charge()
    }
}

impl RetainedCharge for rue_query::NodeIdentity {
    fn retained_charge(&self) -> u64 {
        (self.family().len() + self.key().len()) as u64
    }
}

impl RetainedCharge for rue_query::Observation {
    fn retained_charge(&self) -> u64 {
        self.node.retained_charge()
    }
}

impl RetainedCharge for rue_query::QueryFailure {
    fn retained_charge(&self) -> u64 {
        self.code
            .retained_charge()
            .saturating_add(self.payload.retained_charge())
    }
}

impl RetainedCharge for rue_query::PresentationPosition {
    fn retained_charge(&self) -> u64 {
        self.source.retained_charge()
    }
}

impl RetainedCharge for rue_query::QueryDiagnostic {
    fn retained_charge(&self) -> u64 {
        self.identity
            .retained_charge()
            .saturating_add(self.payload.retained_charge())
            .saturating_add(self.presentation.retained_charge())
    }
}

impl<V: RetainedCharge> RetainedCharge for rue_query::QueryOutcome<V> {
    fn retained_charge(&self) -> u64 {
        match self {
            Self::Success(value) => value.retained_charge(),
            Self::Failure(failure) => failure.retained_charge(),
        }
    }
}

impl<V: RetainedCharge> RetainedCharge for rue_query::QueryTerminal<V> {
    fn retained_charge(&self) -> u64 {
        // Publication already computed the terminal's complete deterministic
        // charge. Reuse that summary when another artifact retains this
        // terminal instead of recursively walking its value and observations.
        // The blanket `Arc<T>` implementation adds the pointee's inline size,
        // which is already in the summary, so exclude it here.
        self.retained_charge()
            .saturating_sub(std::mem::size_of::<Self>() as u64)
    }
}

impl RetainedCharge for rue_rir::RirStructuralAnchor {
    fn retained_charge(&self) -> u64 {
        std::mem::size_of_val(self.segments()) as u64
    }
}

impl RetainedCharge for rue_air::AnonymousMemberKey {
    fn retained_charge(&self) -> u64 {
        self.name.retained_charge()
    }
}

impl<D: RetainedCharge, M: RetainedCharge> RetainedCharge
    for rue_air::CanonicalArgumentValue<D, M>
{
    fn retained_charge(&self) -> u64 {
        match self {
            Self::Type(value) => value.retained_charge(),
            Self::Function(value) => value.retained_charge(),
            Self::String(value) => value.retained_charge(),
            Self::Integer(_) | Self::Bool(_) | Self::Unit => 0,
        }
    }
}

impl<D: RetainedCharge, M: RetainedCharge> RetainedCharge for rue_air::CanonicalArguments<D, M> {
    fn retained_charge(&self) -> u64 {
        self.types
            .retained_charge()
            .saturating_add(self.values.retained_charge())
    }
}

impl<D: RetainedCharge, M: RetainedCharge> RetainedCharge for rue_air::StableProducerId<D, M> {
    fn retained_charge(&self) -> u64 {
        match self {
            Self::Definition(value) => value.retained_charge(),
            Self::Function(value) => value.retained_charge(),
        }
    }
}

impl<D: RetainedCharge, M: RetainedCharge> RetainedCharge for rue_air::AnonymousNominalKey<D, M> {
    fn retained_charge(&self) -> u64 {
        self.producer
            .retained_charge()
            .saturating_add(self.anchor.retained_charge())
            .saturating_add(self.arguments.retained_charge())
    }
}

impl<D: RetainedCharge, M: RetainedCharge> RetainedCharge for rue_air::NominalInstanceKey<D, M> {
    fn retained_charge(&self) -> u64 {
        match self {
            Self::Builtin { name, .. } => name.retained_charge(),
            Self::Named(value) => value.retained_charge(),
            Self::Anonymous(value) => value.retained_charge(),
        }
    }
}

impl<D: RetainedCharge, M: RetainedCharge> RetainedCharge for rue_air::TypeInstanceKey<D, M> {
    fn retained_charge(&self) -> u64 {
        match self {
            Self::BuiltinNominal { name, .. } => name.retained_charge(),
            Self::Nominal(value) => value.retained_charge(),
            Self::Array { element, .. } | Self::PtrConst(element) | Self::PtrMut(element) => {
                element.retained_charge()
            }
            Self::Slice { element, name } => element
                .retained_charge()
                .saturating_add(name.retained_charge()),
            Self::Module(value) => value.retained_charge(),
            Self::I8
            | Self::I16
            | Self::I32
            | Self::I64
            | Self::U8
            | Self::U16
            | Self::U32
            | Self::U64
            | Self::Bool
            | Self::Unit
            | Self::Never
            | Self::ComptimeType
            | Self::GenericParameter(_) => 0,
        }
    }
}

impl<D: RetainedCharge, M: RetainedCharge> RetainedCharge for rue_air::FunctionInstanceKey<D, M> {
    fn retained_charge(&self) -> u64 {
        match self {
            Self::Definition(value) => value.retained_charge(),
            Self::Specialization { base, arguments } => base
                .retained_charge()
                .saturating_add(arguments.retained_charge()),
            Self::AnonymousMember { owner, member } => owner
                .retained_charge()
                .saturating_add(member.retained_charge()),
            Self::DropGlue(value) => value.retained_charge(),
        }
    }
}

impl<K: RetainedCharge, M: RetainedCharge> RetainedCharge for rue_air::SemanticImportType<K, M> {
    fn retained_charge(&self) -> u64 {
        match self {
            Self::BuiltinNominal { name, .. } => name.retained_charge(),
            Self::Nominal(value) => value.retained_charge(),
            Self::AnonymousNominal(value) => value.retained_charge(),
            Self::Array { element, .. } | Self::PtrConst(element) | Self::PtrMut(element) => {
                element.retained_charge()
            }
            Self::Slice { element, name } => element
                .retained_charge()
                .saturating_add(name.retained_charge()),
            Self::Module(value) => value.retained_charge(),
            Self::I8
            | Self::I16
            | Self::I32
            | Self::I64
            | Self::U8
            | Self::U16
            | Self::U32
            | Self::U64
            | Self::Bool
            | Self::Unit
            | Self::Never
            | Self::ComptimeType
            | Self::GenericParameter(_) => 0,
        }
    }
}

impl<K: RetainedCharge, M: RetainedCharge> RetainedCharge
    for rue_air::SemanticImportConstValue<K, M>
{
    fn retained_charge(&self) -> u64 {
        match self {
            Self::Type(value) => value.retained_charge(),
            Self::Function(value) => value.retained_charge(),
            Self::String(value) => value.retained_charge(),
            Self::Integer(_) | Self::Bool(_) | Self::Unit => 0,
        }
    }
}

impl<K: RetainedCharge, M: RetainedCharge> RetainedCharge
    for rue_air::SemanticSpecializationIdentity<K, M>
{
    fn retained_charge(&self) -> u64 {
        self.base
            .retained_charge()
            .saturating_add(self.type_arguments.retained_charge())
            .saturating_add(self.value_arguments.retained_charge())
    }
}

impl<D: RetainedCharge, M: RetainedCharge> RetainedCharge for rue_air::LocalAtomId<D, M> {
    fn retained_charge(&self) -> u64 {
        self.producer
            .retained_charge()
            .saturating_add(self.anchor.retained_charge())
    }
}

impl<D: RetainedCharge, M: RetainedCharge> RetainedCharge for rue_air::LocalAtomRecord<D, M> {
    fn retained_charge(&self) -> u64 {
        self.identity
            .retained_charge()
            .saturating_add(self.content.retained_charge())
    }
}

impl<D: RetainedCharge, M: RetainedCharge> RetainedCharge for rue_air::SemanticBodyLocalAtom<D, M> {
    fn retained_charge(&self) -> u64 {
        self.identity
            .retained_charge()
            .saturating_add(self.content.retained_charge())
    }
}

impl RetainedCharge for rue_air::PaddingRange {
    fn retained_charge(&self) -> u64 {
        0
    }
}

impl RetainedCharge for rue_air::DurableBodySourceLocator {
    fn retained_charge(&self) -> u64 {
        self.physical_path.retained_charge()
    }
}

impl<K: RetainedCharge, M: RetainedCharge> RetainedCharge for rue_air::SemanticBodyPattern<K, M> {
    fn retained_charge(&self) -> u64 {
        match self {
            Self::EnumVariant { enum_key, .. } => enum_key.retained_charge(),
            Self::Wildcard | Self::Int(_) | Self::Bool(_) => 0,
        }
    }
}

impl<K: RetainedCharge, M: RetainedCharge> RetainedCharge for rue_air::SemanticBodyMatchArm<K, M> {
    fn retained_charge(&self) -> u64 {
        self.pattern.retained_charge()
    }
}

impl RetainedCharge for rue_air::SemanticBodyCallArg {
    fn retained_charge(&self) -> u64 {
        0
    }
}

impl<K: RetainedCharge, M: RetainedCharge> RetainedCharge
    for rue_air::SemanticBodyProjection<K, M>
{
    fn retained_charge(&self) -> u64 {
        match self {
            Self::Field { struct_key, .. } => struct_key.retained_charge(),
            Self::Index { array_type, .. } => array_type.retained_charge(),
        }
    }
}

impl<K: RetainedCharge, M: RetainedCharge> RetainedCharge for rue_air::SemanticBodyPlace<K, M> {
    fn retained_charge(&self) -> u64 {
        self.base_type
            .retained_charge()
            .saturating_add(self.projections.retained_charge())
    }
}

impl<K: RetainedCharge, M: RetainedCharge> RetainedCharge for rue_air::SemanticBodyInst<K, M> {
    fn retained_charge(&self) -> u64 {
        self.data
            .retained_charge()
            .saturating_add(self.ty.retained_charge())
    }
}

impl<K: RetainedCharge, M: RetainedCharge> RetainedCharge for rue_air::SemanticBodyInstData<K, M> {
    fn retained_charge(&self) -> u64 {
        match self {
            Self::TypeConst(ty) => ty.retained_charge(),
            Self::Match { arms, .. } => arms.retained_charge(),
            Self::Call { function, args } => function
                .retained_charge()
                .saturating_add(args.retained_charge()),
            Self::AccessorCall { function, args } => function
                .retained_charge()
                .saturating_add(args.retained_charge()),
            Self::RuntimeCall { args, .. } => args.retained_charge(),
            Self::CallSpecialized { identity, args } => identity
                .retained_charge()
                .saturating_add(args.retained_charge()),
            Self::Intrinsic { name, args, .. } => name
                .retained_charge()
                .saturating_add(args.retained_charge()),
            Self::Block { statements, .. } => statements.retained_charge(),
            Self::StructInit {
                struct_key,
                fields,
                source_order,
            } => struct_key
                .retained_charge()
                .saturating_add(fields.retained_charge())
                .saturating_add(source_order.retained_charge()),
            Self::ArrayInit { elements } => elements.retained_charge(),
            Self::EnumVariant {
                enum_key, payload, ..
            } => enum_key
                .retained_charge()
                .saturating_add(payload.retained_charge()),
            Self::EnumPayloadGet { enum_key, .. } => enum_key.retained_charge(),
            Self::IntCast { from_ty, .. } => from_ty.retained_charge(),
            Self::Const(_)
            | Self::BoolConst(_)
            | Self::StringConst(_)
            | Self::UnitConst
            | Self::Add(_, _)
            | Self::Sub(_, _)
            | Self::Mul(_, _)
            | Self::WrappingAdd(_, _)
            | Self::WrappingSub(_, _)
            | Self::WrappingMul(_, _)
            | Self::Div(_, _)
            | Self::Mod(_, _)
            | Self::Eq(_, _)
            | Self::Ne(_, _)
            | Self::Lt(_, _)
            | Self::Gt(_, _)
            | Self::Le(_, _)
            | Self::Ge(_, _)
            | Self::And(_, _)
            | Self::Or(_, _)
            | Self::BitAnd(_, _)
            | Self::BitOr(_, _)
            | Self::BitXor(_, _)
            | Self::Shl(_, _)
            | Self::Shr(_, _)
            | Self::Neg(_)
            | Self::Not(_)
            | Self::BitNot(_)
            | Self::Branch { .. }
            | Self::Loop { .. }
            | Self::InfiniteLoop { .. }
            | Self::Break
            | Self::Continue
            | Self::Alloc { .. }
            | Self::Load { .. }
            | Self::Store { .. }
            | Self::ParamStore { .. }
            | Self::Ret(_)
            | Self::CallGeneric
            | Self::Param { .. }
            | Self::PlaceRead { .. }
            | Self::PlaceWrite { .. }
            | Self::Drop { .. }
            | Self::StorageLive { .. }
            | Self::StorageDead { .. }
            | Self::MarkMoved { .. } => 0,
        }
    }
}

impl RetainedCharge for rue_air::SemanticBodyWarningLabel {
    fn retained_charge(&self) -> u64 {
        self.message.retained_charge()
    }
}

impl RetainedCharge for rue_air::SemanticBodyWarningSuggestion {
    fn retained_charge(&self) -> u64 {
        self.message
            .retained_charge()
            .saturating_add(self.replacement.retained_charge())
    }
}

impl RetainedCharge for rue_air::SemanticBodyWarning {
    fn retained_charge(&self) -> u64 {
        self.kind
            .retained_charge()
            .saturating_add(self.labels.retained_charge())
            .saturating_add(self.notes.retained_charge())
            .saturating_add(self.helps.retained_charge())
            .saturating_add(self.suggestions.retained_charge())
    }
}

impl<K: RetainedCharge, M: RetainedCharge> RetainedCharge
    for rue_air::SemanticBodyMethodReference<K, M>
{
    fn retained_charge(&self) -> u64 {
        self.receiver
            .retained_charge()
            .saturating_add(self.method.retained_charge())
    }
}

impl<K: RetainedCharge, M: RetainedCharge> RetainedCharge for rue_air::SemanticBody<K, M> {
    fn retained_charge(&self) -> u64 {
        self.return_type
            .retained_charge()
            .saturating_add(self.instructions.retained_charge())
            .saturating_add(self.places.retained_charge())
            .saturating_add(self.strings.retained_charge())
            .saturating_add(self.local_atoms.retained_charge())
            .saturating_add(self.param_drops.retained_charge())
            .saturating_add(self.borrow_slots.retained_charge())
            .saturating_add(self.param_by_ref.retained_charge())
            .saturating_add(self.param_writable.retained_charge())
            .saturating_add(self.warnings.retained_charge())
            .saturating_add(self.method_references.retained_charge())
    }
}

impl RetainedCharge for crate::ModuleId {
    fn retained_charge(&self) -> u64 {
        self.as_str().len() as u64
    }
}

impl RetainedCharge for crate::SourceId {
    fn retained_charge(&self) -> u64 {
        ((std::mem::size_of::<crate::SourceIdVersion>() + std::mem::size_of::<[u8; 32]>()) as u64)
            .saturating_add(self.shared_text().retained_charge())
    }
}

impl RetainedCharge for crate::ModuleRevision {
    fn retained_charge(&self) -> u64 {
        self.module
            .retained_charge()
            .saturating_add(self.source.retained_charge())
    }
}

impl RetainedCharge for crate::SourceRevision {
    fn retained_charge(&self) -> u64 {
        self.root()
            .retained_charge()
            .saturating_add(self.module_segments().retained_charge())
    }
}

impl RetainedCharge for crate::bound_definitions::StableNamedTypeKey {
    fn retained_charge(&self) -> u64 {
        self.module()
            .retained_charge()
            .saturating_add(self.name().len() as u64)
    }
}

impl RetainedCharge for crate::StableDefinitionKey {
    fn retained_charge(&self) -> u64 {
        self.module()
            .retained_charge()
            .saturating_add(self.name().len() as u64)
            .saturating_add(self.owner().map_or(0, RetainedCharge::retained_charge))
    }
}

impl RetainedCharge for crate::declaration_candidate::DeclarationCandidateOwner {
    fn retained_charge(&self) -> u64 {
        self.name.retained_charge()
    }
}

impl RetainedCharge for crate::declaration_candidate::DeclarationCandidateKey {
    fn retained_charge(&self) -> u64 {
        self.module
            .retained_charge()
            .saturating_add(self.name.retained_charge())
            .saturating_add(self.owner.retained_charge())
    }
}

impl RetainedCharge for crate::declaration_candidate::DeclarationOccurrenceCapability {
    fn retained_charge(&self) -> u64 {
        self.key().retained_charge()
    }
}

impl RetainedCharge for crate::declaration_candidate::DeclarationOccurrenceFailure {
    fn retained_charge(&self) -> u64 {
        match self {
            Self::ParseRejected { module } => module.retained_charge(),
        }
    }
}

impl RetainedCharge for crate::declaration_candidate::DeclarationShellFailure {
    fn retained_charge(&self) -> u64 {
        match self {
            Self::OccurrencesUnavailable(value) => value.retained_charge(),
            Self::Absent(key) | Self::Ambiguous(key) | Self::ParserCapabilityMismatch(key) => {
                key.retained_charge()
            }
        }
    }
}

impl RetainedCharge for crate::declaration_candidate::RawAnonymousSite {
    fn retained_charge(&self) -> u64 {
        0
    }
}

impl RetainedCharge for crate::declaration_candidate::RawConstSyntax {
    fn retained_charge(&self) -> u64 {
        self.declared_type
            .retained_charge()
            .saturating_add(self.initializer.retained_charge())
            .saturating_add(self.anonymous_sites.retained_charge())
    }
}

macro_rules! candidate_failure_charge {
    ($ty:ty) => {
        impl RetainedCharge for $ty {
            fn retained_charge(&self) -> u64 {
                match self {
                    Self::OccurrencesUnavailable(value) => value.retained_charge(),
                    Self::Absent(key)
                    | Self::Ambiguous(key)
                    | Self::CategoryMismatch(key)
                    | Self::ParserCapabilityMismatch(key) => key.retained_charge(),
                }
            }
        }
    };
}

candidate_failure_charge!(crate::declaration_candidate::RawConstSyntaxFailure);
candidate_failure_charge!(crate::declaration_candidate::RawDeclarationBodyFailure);

impl RetainedCharge for crate::semantic_query_nucleus::ParsedSemanticParameter {
    fn retained_charge(&self) -> u64 {
        0
    }
}

zero_charge!(
    crate::semantic_query_nucleus::ParsedSemanticText,
    crate::semantic_query_nucleus::ParsedSemanticField,
    crate::semantic_query_nucleus::ParsedSemanticVariant,
);

impl RetainedCharge for crate::semantic_query_nucleus::ParsedSemanticSignature {
    fn retained_charge(&self) -> u64 {
        match self {
            Self::Callable {
                text,
                parameters,
                accessor_cycle,
                ..
            } => text
                .retained_charge()
                .saturating_add(parameters.retained_charge())
                .saturating_add(accessor_cycle.retained_charge()),
            Self::Struct { text, fields, .. } => text
                .retained_charge()
                .saturating_add(fields.retained_charge()),
            Self::Enum {
                text,
                variants,
                payloads,
            } => text
                .retained_charge()
                .saturating_add(variants.retained_charge())
                .saturating_add(payloads.retained_charge()),
            Self::Destructor => 0,
        }
    }
}

impl RetainedCharge for rue_air::declaration_validation::AccessorOwnerMethod {
    fn retained_charge(&self) -> u64 {
        self.name
            .retained_charge()
            .saturating_add(self.is_accessor.retained_charge())
            .saturating_add(self.self_call_targets.retained_charge())
    }
}

impl RetainedCharge for crate::declaration_candidate::RawDeclarationBodySyntax {
    fn retained_charge(&self) -> u64 {
        self.body
            .retained_charge()
            .saturating_add(self.anonymous_sites.retained_charge())
    }
}

impl RetainedCharge for crate::declaration_candidate::DeclarationImportSiteKey {
    fn retained_charge(&self) -> u64 {
        self.declaration
            .retained_charge()
            .saturating_add(self.specifier.retained_charge())
    }
}

impl RetainedCharge for crate::declaration_candidate::DeclarationImportFailure {
    fn retained_charge(&self) -> u64 {
        match self {
            Self::OccurrencesUnavailable(value) => value.retained_charge(),
            Self::AbsentDeclaration(key)
            | Self::AmbiguousDeclaration(key)
            | Self::CategoryMismatch(key)
            | Self::ParserCapabilityMismatch(key)
            | Self::ResolutionUnavailable(key)
            | Self::SiteOutOfRange { key, .. } => key.retained_charge(),
            Self::SpecifierMismatch { key, actual } => key
                .retained_charge()
                .saturating_add(actual.retained_charge()),
        }
    }
}

impl RetainedCharge for crate::declaration_candidate::DeclarationParameterHeader {
    fn retained_charge(&self) -> u64 {
        self.name.retained_charge()
    }
}

impl RetainedCharge for crate::declaration_candidate::DeclarationShellFact {
    fn retained_charge(&self) -> u64 {
        self.key
            .retained_charge()
            .saturating_add(self.parameters.retained_charge())
    }
}

impl RetainedCharge for crate::ImportDiscoveryContext {
    fn retained_charge(&self) -> u64 {
        (self.project_root().len()
            + self.std_root().map_or(0, str::len)
            + self.read_policy_revision().len()) as u64
    }
}

impl RetainedCharge for crate::ImportOccurrenceKey {
    fn retained_charge(&self) -> u64 {
        self.importer()
            .retained_charge()
            .saturating_add(self.specifier().len() as u64)
    }
}

impl RetainedCharge for crate::ImportDiscoveryRequest {
    fn retained_charge(&self) -> u64 {
        self.context()
            .retained_charge()
            .saturating_add(self.occurrence().retained_charge())
            .saturating_add(self.exact_specifier().len() as u64)
            .saturating_add(self.normalized_specifier().len() as u64)
            .saturating_add(self.importer_anchor().len() as u64)
            .saturating_add(self.root_anchor().len() as u64)
            .saturating_add(self.requested_path().len() as u64)
    }
}

impl RetainedCharge for crate::CanonicalImportResolution {
    fn retained_charge(&self) -> u64 {
        match self {
            Self::Resolved(module) => module.retained_charge(),
            Self::Ambiguous {
                file_module,
                directory_module,
            } => file_module
                .retained_charge()
                .saturating_add(directory_module.retained_charge()),
            Self::Missing => 0,
        }
    }
}

impl RetainedCharge for crate::ImportDirective {
    fn retained_charge(&self) -> u64 {
        self.importer()
            .retained_charge()
            .saturating_add(self.specifier().len() as u64)
    }
}

impl RetainedCharge for crate::SourceMetadata {
    fn retained_charge(&self) -> u64 {
        self.file_ids().fold(0_u64, |charge, file_id| {
            // The descriptor owns two logical path maps and their normalized
            // identity sets. Charge each retained string occurrence in full.
            let physical = self.physical_path(file_id).map_or(0, str::len) as u64;
            let logical = self.logical_path(file_id).map_or(0, str::len) as u64;
            charge
                .saturating_add((2 * std::mem::size_of::<(rue_span::FileId, String)>()) as u64)
                .saturating_add(physical.saturating_mul(2))
                .saturating_add(logical.saturating_mul(2))
        })
    }
}

impl RetainedCharge for crate::SourceSnapshot {
    fn retained_charge(&self) -> u64 {
        let records = self.metadata().file_ids().fold(0_u64, |charge, file_id| {
            charge
                .saturating_add(std::mem::size_of::<(
                    rue_span::FileId,
                    crate::ModuleId,
                    crate::SourceId,
                    Arc<String>,
                )>() as u64)
                .saturating_add(
                    self.module_id(file_id)
                        .map_or(0, RetainedCharge::retained_charge),
                )
                .saturating_add(
                    self.shared_source_text(file_id)
                        .map_or(0, |text| text.retained_charge()),
                )
        });
        (std::mem::size_of::<crate::SourceMetadata>() as u64)
            .saturating_add(self.metadata().retained_charge())
            .saturating_add(self.source_revision().retained_charge())
            .saturating_add(records)
    }
}

impl RetainedCharge for crate::parsed_modules::ParsedModule {
    fn retained_charge(&self) -> u64 {
        self.retained_allocation_charge()
    }
}

impl RetainedCharge for crate::parsed_modules::ParsedProgram {
    fn retained_charge(&self) -> u64 {
        let modules = self.modules_iter().fold(
            (self.modules_len() * std::mem::size_of::<Arc<crate::parsed_modules::ParsedModule>>())
                as u64,
            |charge, module| charge.saturating_add(module.retained_charge()),
        );
        self.source_revision()
            .retained_charge()
            .saturating_add(modules)
            .saturating_add(
                (self.import_directives().len() * std::mem::size_of::<crate::ImportDirective>())
                    as u64,
            )
            .saturating_add(
                self.import_directives()
                    .iter()
                    .fold(0_u64, |charge, value| {
                        charge.saturating_add(value.retained_charge())
                    }),
            )
            .saturating_add(std::mem::size_of_val(self.invalid_imports()) as u64)
            .saturating_add(
                self.invalid_imports()
                    .iter()
                    .fold(0_u64, |charge, invalid| {
                        charge.saturating_add(invalid.retained_allocation_charge())
                    }),
            )
    }
}

impl RetainedCharge for crate::SemanticSymbolUniverse {
    fn retained_charge(&self) -> u64 {
        let modules = self.admitted_modules().iter().fold(
            std::mem::size_of_val(self.admitted_modules()) as u64,
            |charge, module| charge.saturating_add(module.retained_charge()),
        );
        let interner = (std::mem::size_of::<lasso::ThreadedRodeo>() as u64)
            .saturating_add((self.interner().len() * std::mem::size_of::<lasso::Spur>()) as u64)
            .saturating_add(self.interner().iter().fold(0_u64, |charge, (_, value)| {
                charge.saturating_add(value.len() as u64)
            }));
        modules.saturating_add(interner)
    }
}

impl RetainedCharge for crate::ParseInvalidationSummary {
    fn retained_charge(&self) -> u64 {
        self.exact_reused
            .retained_charge()
            .saturating_add(self.payload_rebound.retained_charge())
            .saturating_add(self.reparsed.retained_charge())
            .saturating_add(self.added.retained_charge())
            .saturating_add(self.removed.retained_charge())
    }
}

impl RetainedCharge for crate::diagnostic_attempt_store::DiagnosticAttemptProvenance {
    fn retained_charge(&self) -> u64 {
        match self {
            Self::Canonical => 0,
            Self::Presentation(modules) => modules.retained_charge(),
        }
    }
}

impl RetainedCharge for crate::TrustedToolchainModuleDemand {
    fn retained_charge(&self) -> u64 {
        self.logical_path().len() as u64
    }
}

impl RetainedCharge for crate::ParkedToolchainModules {
    fn retained_charge(&self) -> u64 {
        let demands = std::mem::size_of_val(self.demands()) as u64;
        let requesters = std::mem::size_of_val(self.requesters()) as u64;
        demands
            .saturating_add(self.demands().retained_charge())
            .saturating_add(requesters)
            .saturating_add(self.requesters().retained_charge())
    }
}

impl RetainedCharge for rue_error::Label {
    fn retained_charge(&self) -> u64 {
        self.message.retained_charge()
    }
}

impl RetainedCharge for rue_error::Note {
    fn retained_charge(&self) -> u64 {
        self.0.retained_charge()
    }
}

impl RetainedCharge for rue_error::Help {
    fn retained_charge(&self) -> u64 {
        self.0.retained_charge()
    }
}

impl RetainedCharge for rue_error::Suggestion {
    fn retained_charge(&self) -> u64 {
        self.message
            .retained_charge()
            .saturating_add(self.replacement.retained_charge())
    }
}

impl RetainedCharge for rue_error::WarningKind {
    fn retained_charge(&self) -> u64 {
        match self {
            Self::UnusedVariable(value)
            | Self::UnusedFunction(value)
            | Self::UnreachablePattern(value) => value.retained_charge(),
            Self::UnreachableCode | Self::SentinelLookup => 0,
        }
    }
}

impl RetainedCharge for rue_error::ErrorKind {
    fn retained_charge(&self) -> u64 {
        use rue_error::ErrorKind as E;
        match self {
            E::MalformedByteLiteral(value)
            | E::MalformedFloatLiteral(value)
            | E::ParseError(value)
            | E::UndefinedVariable(value)
            | E::UndefinedFunction(value)
            | E::AssignToImmutable(value)
            | E::UnknownType(value)
            | E::UseAfterMove(value)
            | E::LinearValueNotConsumed(value)
            | E::LinearValueNotConsumedOnAllPaths(value)
            | E::LinearStructCopy(value)
            | E::UnknownEnumType(value)
            | E::InvalidMatchType(value)
            | E::UnknownIntrinsic(value)
            | E::CannotInferCastTarget(value)
            | E::CannotInferPointeeType(value)
            | E::CannotNegate(value)
            | E::LinkError(value)
            | E::UnsupportedTarget(value)
            | E::InvalidCompilerInput(value)
            | E::CompilerResourceLimit(value)
            | E::CompilerResourceExhaustion(value)
            | E::OutputPublication(value)
            | E::UnsatisfiedTrustedToolchainInput(value)
            | E::CompilerProducerInvariant(value)
            | E::InternalError(value)
            | E::InternalCodegenError(value) => value.retained_charge(),

            E::InvalidArrayLength { reason: value }
            | E::DuplicatePatternBinding { name: value }
            | E::ReservedTypeName { type_name: value }
            | E::ReservedFunctionName {
                function_name: value,
            }
            | E::DuplicateTypeDefinition { type_name: value }
            | E::DuplicateFunctionDefinition {
                function_name: value,
            }
            | E::LinearValueDiscarded { type_name: value }
            | E::LinearValueOverwritten { type_name: value }
            | E::LinearValueOverwrittenThroughInout { type_name: value }
            | E::DuplicateParameter { name: value }
            | E::DuplicateDestructor { type_name: value }
            | E::DestructorUnknownType { type_name: value }
            | E::ConstExprNotSupported { expr_kind: value }
            | E::ConstInitializerCycle { cycle: value }
            | E::ConstMissingTypeAnnotation { name: value }
            | E::FieldAccessOnNonStruct { found: value }
            | E::StrViewNotFirstClass { site: value }
            | E::ContainerElementIsLinear { ty: value }
            | E::ContainerElementNotTriviallyDroppable { ty: value }
            | E::InoutExclusiveAccess { variable: value }
            | E::MutateBorrowedValue { variable: value }
            | E::MoveOutOfBorrow { variable: value }
            | E::BorrowInoutConflict { variable: value }
            | E::MoveOutOfInout { variable: value }
            | E::AccessorYieldNotReceiverRooted { found: value }
            | E::AccessorBodyOtherExit { found: value }
            | E::AccessorRequiresBorrowSelf { found: value }
            | E::AccessorResultMoved { ty: value }
            | E::AccessorLoanConflict {
                variable: value, ..
            }
            | E::AccessorParamModeUnsupported { mode: value }
            | E::AccessorRecursion { method: value }
            | E::MoveSelfOutOfDestructor { type_name: value }
            | E::CopyStructWithDestructor { type_name: value }
            | E::QuestionOutsideOptionFn { return_type: value }
            | E::QuestionOnNonOption { found: value }
            | E::QuestionOutsideResultFn { return_type: value }
            | E::IntrinsicWrongArgCount { name: value, .. }
            | E::IntrinsicAlignNotPowerOfTwo { name: value, .. }
            | E::LiteralOutOfRange { ty: value, .. }
            | E::IndexOnNonArray { found: value }
            | E::MoveOutOfIndex {
                element_type: value,
            }
            | E::ArrayRepeatNonCopy {
                element_type: value,
            }
            | E::TypeTooLarge {
                type_name: value, ..
            }
            | E::AssignToPartiallyMovedArray { array: value }
            | E::PreviewFeatureRequired { what: value, .. }
            | E::ExternSignatureTypeUnsupported { ty: value }
            | E::ExternAggregateNotReprC { ty: value }
            | E::ExternArrayByValue { ty: value }
            | E::ComptimeEvaluationFailed { reason: value }
            | E::ComptimeArgNotConst { param_name: value }
            | E::UncheckedOpRequiresChecked { what: value } => value.retained_charge(),

            E::TypeMismatch {
                expected: left,
                found: right,
            }
            | E::UnknownField {
                struct_name: left,
                field_name: right,
            }
            | E::DuplicateField {
                struct_name: left,
                field_name: right,
            }
            | E::DuplicateMethod {
                type_name: left,
                method_name: right,
            }
            | E::UndefinedMethod {
                type_name: left,
                method_name: right,
            }
            | E::UndefinedAssocFn {
                type_name: left,
                function_name: right,
            }
            | E::MethodCallOnNonStruct {
                found: left,
                method_name: right,
            }
            | E::MethodCalledAsAssocFn {
                type_name: left,
                method_name: right,
            }
            | E::AssocFnCalledAsMethod {
                type_name: left,
                function_name: right,
            }
            | E::DuplicateConstant {
                name: left,
                kind: right,
            }
            | E::RecursiveTypeInfiniteSize {
                name: left,
                cycle: right,
            }
            | E::DuplicateVariant {
                enum_name: left,
                variant_name: right,
            }
            | E::UnknownVariant {
                enum_name: left,
                variant_name: right,
            }
            | E::BufferNotFirstClassStr {
                found: left,
                site: right,
            }
            | E::MoveWhileCallLoaned {
                variable: left,
                loan_mode: right,
            }
            | E::AccessorResultReturned {
                method: left,
                root: right,
            }
            | E::AccessorResultStored {
                method: left,
                root: right,
            }
            | E::AccessorResultBound {
                method: left,
                root: right,
            }
            | E::AccessorResultCaptured {
                method: left,
                root: right,
            }
            | E::MoveFieldOutOfDestructorType {
                struct_name: left,
                field_name: right,
            }
            | E::QuestionErrTypeMismatch {
                operand_err: left,
                fn_err: right,
            }
            | E::BitCastWidthMismatch {
                from: left,
                to: right,
                ..
            }
            | E::PrivateMemberAccess {
                item_kind: left,
                name: right,
            }
            | E::UnknownModuleMember {
                module_name: left,
                member_name: right,
            }
            | E::ExportSignatureUnsupported {
                name: left,
                reason: right,
            } => left
                .retained_charge()
                .saturating_add(right.retained_charge()),

            E::UnexpectedToken { expected, found } => expected
                .retained_charge()
                .saturating_add(found.retained_charge()),
            E::ModuleNotFound { path, candidates } => path
                .retained_charge()
                .saturating_add(candidates.retained_charge()),
            E::MissingFields(value) => (std::mem::size_of_val(value.as_ref()) as u64)
                .saturating_add(value.struct_name.retained_charge())
                .saturating_add(value.missing_fields.retained_charge()),
            E::CopyStructNonCopyField(value) => (std::mem::size_of_val(value.as_ref()) as u64)
                .saturating_add(value.struct_name.retained_charge())
                .saturating_add(value.field_name.retained_charge())
                .saturating_add(value.field_type.retained_charge()),
            E::LinearFieldDroppedByDestructure(value) => (std::mem::size_of_val(value.as_ref())
                as u64)
                .saturating_add(value.struct_name.retained_charge())
                .saturating_add(value.accessed.retained_charge())
                .saturating_add(value.dropped.retained_charge()),
            E::IntrinsicTypeMismatch(value) => (std::mem::size_of_val(value.as_ref()) as u64)
                .saturating_add(value.name.retained_charge())
                .saturating_add(value.expected.retained_charge())
                .saturating_add(value.found.retained_charge()),
            E::AmbiguousModule(value) => (std::mem::size_of_val(value.as_ref()) as u64)
                .saturating_add(value.path.retained_charge())
                .saturating_add(value.file_module.retained_charge())
                .saturating_add(value.dir_module.retained_charge()),
            E::PrivateUnqualifiedAccess(value) => (std::mem::size_of_val(value.as_ref()) as u64)
                .saturating_add(value.item_kind.retained_charge())
                .saturating_add(value.name.retained_charge())
                .saturating_add(value.defining_file.retained_charge()),
            E::ForeignSignatureConflict(value) => (std::mem::size_of_val(value.as_ref()) as u64)
                .saturating_add(value.symbol.retained_charge())
                .saturating_add(value.declared.retained_charge())
                .saturating_add(value.previously_declared.retained_charge()),
            E::ReprCStructIneligible(value) => (std::mem::size_of_val(value.as_ref()) as u64)
                .saturating_add(value.struct_name.retained_charge())
                .saturating_add(value.field_path.retained_charge())
                .saturating_add(value.failing_type.retained_charge())
                .saturating_add(value.reason.retained_charge()),
            E::UnexpectedCharacter(_)
            | E::InvalidStringEscape(_)
            | E::UppercaseBasePrefix(_)
            | E::EmptyBasedLiteral { .. }
            | E::InvalidDigitForBase { .. }
            | E::LexerDiagnosticsOmitted { .. }
            | E::ParserDiagnosticsOmitted { .. }
            | E::NestingLimitExceeded { .. }
            | E::InvalidMainSignature { .. }
            | E::WrongArgumentCount { .. }
            | E::StrFixedCapacityExceeded { .. }
            | E::UnexpectedCallArgumentMode { .. }
            | E::ArrayLengthMismatch { .. }
            | E::IndexOutOfBounds { .. }
            | E::FunctionFrameTooLarge { .. }
            | E::InvalidInteger
            | E::UnterminatedString
            | E::NoMainFunction
            | E::EmptyStruct
            | E::InvalidAssignmentTarget
            | E::RawRequiresPlace
            | E::FieldPtrRequiresField
            | E::InoutStrRequiresLocalBuffer
            | E::StrViewReassignment
            | E::InoutNonLvalue
            | E::BorrowNonLvalue
            | E::InoutKeywordMissing
            | E::BorrowKeywordMissing
            | E::AccessorBodyMissingYield
            | E::YieldOutsideAccessor
            | E::BreakOutsideLoop
            | E::ContinueOutsideLoop
            | E::BreakWithValue
            | E::NonExhaustiveMatch
            | E::EmptyMatch
            | E::ImportRequiresStringLiteral
            | E::StdLibNotFound
            | E::ChainedComparison
            | E::TypeAnnotationRequired
            | E::ExternVariadicUnsupported
            | E::ForeignEntryPointDeclaration
            | E::SliceNotYetImplemented
            | E::FloatNotYetImplemented
            | E::SliceReturnNotAllowed
            | E::SliceInAggregateField
            | E::SliceEscapesScope => 0,
        }
    }
}

impl<K: RetainedCharge> RetainedCharge for rue_error::DiagnosticWrapper<K> {
    fn retained_charge(&self) -> u64 {
        let diagnostic = self.diagnostic();
        self.kind
            .retained_charge()
            .saturating_add(diagnostic.labels.retained_charge())
            .saturating_add(diagnostic.notes.retained_charge())
            .saturating_add(diagnostic.helps.retained_charge())
            .saturating_add(diagnostic.suggestions.retained_charge())
    }
}

impl RetainedCharge for rue_error::CompileErrors {
    fn retained_charge(&self) -> u64 {
        ((self.len() * std::mem::size_of::<rue_error::CompileError>()) as u64).saturating_add(
            self.iter().fold(0_u64, |charge, value| {
                charge.saturating_add(value.retained_charge())
            }),
        )
    }
}

impl RetainedCharge for crate::FrontendDiagnosticSnapshot {
    fn retained_charge(&self) -> u64 {
        self.source
            .retained_charge()
            .saturating_add(self.provenance.retained_charge())
            .saturating_add(self.errors.retained_charge())
            .saturating_add(self.warnings.retained_charge())
    }
}

#[cfg(test)]
mod tests {
    use super::RetainedCharge;
    use std::sync::Arc;

    use crate::parsed_modules::parse_source_snapshot_modules;

    #[test]
    fn vector_charge_depends_on_logical_length_not_capacity() {
        let compact = vec![String::from("alpha"), String::from("beta")];
        let mut reserved = Vec::with_capacity(4096);
        reserved.extend(compact.iter().cloned());

        assert_eq!(compact.retained_charge(), reserved.retained_charge());
        assert_eq!(
            compact.retained_charge(),
            (2 * std::mem::size_of::<String>() + "alpha".len() + "beta".len()) as u64
        );
    }

    #[test]
    fn nested_owned_strings_are_charged_recursively() {
        let values = vec![vec![String::from("one"), String::from("three")]];
        let expected = std::mem::size_of::<Vec<String>>()
            + 2 * std::mem::size_of::<String>()
            + "one".len()
            + "three".len();
        assert_eq!(values.retained_charge(), expected as u64);
    }

    #[test]
    fn every_shared_arc_path_is_charged_in_full() {
        let shared = Arc::new(String::from("shared payload"));
        let one_path = shared.retained_charge();
        let two_paths = (shared.clone(), shared).retained_charge();

        assert_eq!(two_paths, one_path.saturating_mul(2));
        assert_eq!(
            one_path,
            (std::mem::size_of::<String>() + "shared payload".len()) as u64
        );
    }

    #[test]
    fn parsed_module_charge_includes_recursive_ast_index_and_invalid_import_paths() {
        let snapshot = crate::SourceSnapshot::single(
            "/project/main.rue",
            "struct Pair { left: i32, right: i32, fn sum(borrow self) -> i32 { self.left + self.right } } fn main() { @import(1); }",
        )
        .unwrap();
        let parsed = parse_source_snapshot_modules(&snapshot).unwrap();
        let module = &parsed.modules()[0];

        let ast = module.ast().retained_charge();
        assert!(
            ast > (module.ast().items.len() * std::mem::size_of::<rue_parser::ast::Item>()) as u64,
            "nested method/body allocations must exceed the root item vector"
        );
        let definitions = module.definitions().retained_allocation_charge();
        assert!(
            definitions > 0,
            "definition-index owned collections are charged"
        );
        let invalid = module.invalid_imports()[0].retained_allocation_charge();
        assert!(
            invalid > 0,
            "the invalid import's owned module path is charged"
        );

        let direct_paths = (std::mem::size_of::<rue_parser::ast::Ast>() as u64)
            .saturating_add(ast)
            .saturating_add(definitions)
            .saturating_add(std::mem::size_of_val(module.invalid_imports()) as u64)
            .saturating_add(invalid);
        assert!(module.retained_charge() >= direct_paths);
    }

    #[test]
    fn semantic_universe_charges_every_admitted_parsed_module_path() {
        let snapshot = crate::SourceSnapshot::single(
            "/project/main.rue",
            "fn helper(value: i32) -> i32 { value + 1 } fn main() -> i32 { helper(1) }",
        )
        .unwrap();
        let parsed = parse_source_snapshot_modules(&snapshot).unwrap();
        let universe = crate::SemanticSymbolUniverse::new(&parsed);
        let admitted = parsed.modules().iter().fold(
            std::mem::size_of_val(parsed.modules()) as u64,
            |charge, module| charge.saturating_add(module.retained_charge()),
        );

        let logical_interner = (std::mem::size_of::<lasso::ThreadedRodeo>() as u64)
            .saturating_add((universe.interner().len() * std::mem::size_of::<lasso::Spur>()) as u64)
            .saturating_add(
                universe
                    .interner()
                    .iter()
                    .fold(0_u64, |charge, (_, value)| {
                        charge.saturating_add(value.len() as u64)
                    }),
            );
        assert_eq!(
            universe.retained_charge(),
            admitted.saturating_add(logical_interner)
        );
    }
}
