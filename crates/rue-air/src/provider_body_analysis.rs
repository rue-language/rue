//! Body-local demand discovery for provider-driven semantic analysis.
//!
//! This is the pre-cutover entry used by the RUE-1091 differential harness.
//! Unlike a replay of an already-published body transaction, it starts from one
//! RIR body root and discovers semantic facts while walking that body.  The
//! caller owns the provider/overlay context and materializes each demand through
//! the keyed [`ProviderBodyAnalysisContext`] callbacks.

use std::collections::HashSet;

use lasso::ThreadedRodeo;
use rue_rir::{InstData, InstRef, RepeatCount, Rir, RirPatternView};
use rue_span::Span;

use crate::OperatorName;

/// Provider/overlay operations demanded by a body-local RIR walk.
///
/// The context, rather than the walker, owns the provider's durable identity
/// types. This keeps the entry in `rue-air` while allowing the compiler adapter
/// to materialize exact declaration signatures, nominals, methods, constants,
/// and imports into its task-owned overlay.
pub trait ProviderBodyAnalysisContext {
    /// A value-position name was referenced by the body.
    fn demand_value(&mut self, name: &str, span: Span);

    /// A call head and its body-local argument expressions were referenced.
    fn demand_call(&mut self, name: &str, arguments: &[InstRef], span: Span);

    /// A type-syntax spelling was referenced by the body.
    fn demand_type(&mut self, syntax: &str, span: Span);

    /// A struct literal head was referenced. `module` is the body-local RIR
    /// expression that names a qualified module, when present.
    fn demand_struct(&mut self, module: Option<InstRef>, type_name: &str, span: Span);

    /// An enum variant head was referenced.
    fn demand_enum(&mut self, module: Option<InstRef>, type_name: &str, span: Span);

    /// A method was referenced on a body-local receiver expression.
    fn demand_method(&mut self, receiver: InstRef, method: &str, span: Span);

    /// A user-overloadable operator was referenced on a body-local receiver.
    fn demand_operator(&mut self, receiver: InstRef, operator: OperatorName, span: Span);

    /// A named array repeat count was referenced.
    fn demand_const_value(&mut self, name: &str, span: Span);

    /// An anonymous nominal syntax node was reached in this body.
    fn demand_anonymous_nominal(&mut self, instruction: InstRef, span: Span);
}

/// Walk exactly one RIR body and materialize every semantic demand originating
/// in that body.
///
/// The walk follows only instruction edges reachable from `body`; it never
/// enumerates declarations, module indexes, or an in-scope symbol universe.
pub fn analyze_provider_body(
    context: &mut impl ProviderBodyAnalysisContext,
    rir: &Rir,
    interner: &ThreadedRodeo,
    body: InstRef,
) {
    struct Walker<'a, C> {
        context: &'a mut C,
        rir: &'a Rir,
        interner: &'a ThreadedRodeo,
        visited: HashSet<InstRef>,
    }

    impl<C: ProviderBodyAnalysisContext> Walker<'_, C> {
        fn symbol(&self, symbol: lasso::Spur) -> String {
            self.interner.resolve(&symbol).to_owned()
        }

        fn visit(&mut self, reference: InstRef) {
            if !self.visited.insert(reference) {
                return;
            }
            let inst = self.rir.get(reference);
            let span = inst.span;
            macro_rules! visit {
                ($($child:expr),* $(,)?) => {
                    {
                        $(self.visit($child);)*
                    }
                };
            }
            match &inst.data {
                InstData::IntConst(_)
                | InstData::BoolConst(_)
                | InstData::StringConst { .. }
                | InstData::UnitConst
                | InstData::Continue => {}
                InstData::VarRef { name, .. } => {
                    let name = self.symbol(*name);
                    self.context.demand_value(&name, span);
                }
                InstData::TypeConst { type_name } => {
                    let syntax = self.symbol(*type_name);
                    self.context.demand_type(&syntax, span);
                }
                InstData::Add { lhs, rhs } => {
                    self.context.demand_operator(*lhs, OperatorName::Add, span);
                    visit!(*lhs, *rhs);
                }
                InstData::Sub { lhs, rhs } => {
                    self.context.demand_operator(*lhs, OperatorName::Sub, span);
                    visit!(*lhs, *rhs);
                }
                InstData::Mul { lhs, rhs } => {
                    self.context.demand_operator(*lhs, OperatorName::Mul, span);
                    visit!(*lhs, *rhs);
                }
                InstData::Div { lhs, rhs } => {
                    self.context.demand_operator(*lhs, OperatorName::Div, span);
                    visit!(*lhs, *rhs);
                }
                InstData::Eq { lhs, rhs } | InstData::Ne { lhs, rhs } => {
                    self.context.demand_operator(*lhs, OperatorName::Eq, span);
                    visit!(*lhs, *rhs);
                }
                InstData::Mod { lhs, rhs }
                | InstData::Lt { lhs, rhs }
                | InstData::Gt { lhs, rhs }
                | InstData::Le { lhs, rhs }
                | InstData::Ge { lhs, rhs }
                | InstData::And { lhs, rhs }
                | InstData::Or { lhs, rhs }
                | InstData::BitAnd { lhs, rhs }
                | InstData::BitOr { lhs, rhs }
                | InstData::BitXor { lhs, rhs }
                | InstData::Shl { lhs, rhs }
                | InstData::Shr { lhs, rhs } => visit!(*lhs, *rhs),
                InstData::Neg { operand }
                | InstData::Not { operand }
                | InstData::BitNot { operand }
                | InstData::Try { operand }
                | InstData::Comptime { expr: operand }
                | InstData::Checked { expr: operand } => visit!(*operand),
                InstData::Branch {
                    cond,
                    then_block,
                    else_block,
                } => {
                    visit!(*cond, *then_block);
                    if let Some(child) = else_block {
                        self.visit(*child);
                    }
                }
                InstData::Loop { cond, body } => visit!(*cond, *body),
                InstData::InfiniteLoop { body, .. } => visit!(*body),
                InstData::Match { scrutinee, arms } => {
                    visit!(*scrutinee);
                    for (pattern, body) in self.rir.match_arms(arms).iter() {
                        if let RirPatternView::Path {
                            module,
                            ctor_head,
                            type_name,
                            ..
                        } = pattern
                        {
                            let type_name = self.symbol(type_name);
                            self.context.demand_enum(module, &type_name, span);
                            if let Some(module) = module {
                                self.visit(module);
                            }
                            if let Some(head) = ctor_head {
                                self.visit(head);
                            }
                        }
                        self.visit(body);
                    }
                }
                InstData::Break { value } | InstData::Ret(value) => {
                    if let Some(child) = value {
                        self.visit(*child);
                    }
                }
                InstData::FnDecl {
                    params,
                    return_type,
                    body,
                    ..
                } => {
                    for parameter in self.rir.params(params) {
                        let syntax = self.symbol(parameter.ty);
                        self.context.demand_type(&syntax, parameter.span);
                    }
                    let syntax = self.symbol(*return_type);
                    self.context.demand_type(&syntax, span);
                    visit!(*body);
                }
                InstData::ConstDecl { ty, init, .. } => {
                    if let Some(ty) = ty {
                        let syntax = self.symbol(*ty);
                        self.context.demand_type(&syntax, span);
                    }
                    visit!(*init);
                }
                InstData::Call { name, args } => {
                    let name = self.symbol(*name);
                    let arguments = self
                        .rir
                        .call_args(args)
                        .iter()
                        .map(|argument| argument.value)
                        .collect::<Vec<_>>();
                    self.context.demand_call(&name, &arguments, span);
                    for argument in arguments {
                        self.visit(argument);
                    }
                }
                InstData::Intrinsic { args, .. } => {
                    for child in self.rir.intrinsic_args(args) {
                        self.visit(child);
                    }
                }
                InstData::InternalIntrinsic { args, .. } => {
                    for child in self.rir.internal_intrinsic_args(args) {
                        self.visit(child);
                    }
                }
                InstData::TypeIntrinsic { type_arg, .. } | InstData::OffsetOf { type_arg, .. } => {
                    let syntax = self.symbol(*type_arg);
                    self.context.demand_type(&syntax, span);
                }
                InstData::Block { instructions } => {
                    for child in self.rir.block_insts(instructions) {
                        self.visit(child);
                    }
                }
                InstData::Alloc { ty, init, .. } => {
                    if let Some(ty) = ty {
                        let syntax = self.symbol(*ty);
                        self.context.demand_type(&syntax, span);
                    }
                    visit!(*init);
                }
                InstData::Assign { value, .. } => visit!(*value),
                InstData::StructDecl {
                    fields, methods, ..
                } => {
                    for (_, ty) in self.rir.struct_fields(fields) {
                        let syntax = self.symbol(ty);
                        self.context.demand_type(&syntax, span);
                    }
                    for method in self.rir.struct_methods(methods) {
                        self.visit(method);
                    }
                }
                InstData::StructInit {
                    module,
                    ctor_head,
                    type_name,
                    fields,
                    ..
                } => {
                    let type_name = self.symbol(*type_name);
                    self.context.demand_struct(*module, &type_name, span);
                    if let Some(module) = module {
                        self.visit(*module);
                    }
                    if let Some(head) = ctor_head {
                        self.visit(*head);
                    }
                    for (_, child) in self.rir.field_inits(fields) {
                        self.visit(child);
                    }
                }
                InstData::FieldGet { base, .. } => visit!(*base),
                InstData::FieldSet { base, value, .. } => visit!(*base, *value),
                InstData::EnumDecl {
                    variants, payloads, ..
                } => {
                    for payload in self.rir.enum_payloads(payloads, variants) {
                        for ty in payload {
                            let syntax = self.symbol(ty);
                            self.context.demand_type(&syntax, span);
                        }
                    }
                }
                InstData::EnumVariant {
                    module, type_name, ..
                } => {
                    let type_name = self.symbol(*type_name);
                    self.context.demand_enum(*module, &type_name, span);
                    if let Some(module) = module {
                        self.visit(*module);
                    }
                }
                InstData::ArrayInit { elements } => {
                    for child in self.rir.array_elements(elements) {
                        self.visit(child);
                    }
                }
                InstData::ArrayRepeat { value, count } => {
                    visit!(*value);
                    if let RepeatCount::Named(name) = count {
                        let name = self.symbol(*name);
                        self.context.demand_const_value(&name, span);
                    }
                }
                InstData::IndexGet { base, index } => {
                    self.context
                        .demand_operator(*base, OperatorName::Index, span);
                    visit!(*base, *index);
                }
                InstData::IndexSet { base, index, value } => {
                    self.context
                        .demand_operator(*base, OperatorName::Index, span);
                    visit!(*base, *index, *value);
                }
                InstData::MethodCall {
                    receiver,
                    method,
                    args,
                } => {
                    let method = self.symbol(*method);
                    self.context.demand_method(*receiver, &method, span);
                    self.visit(*receiver);
                    for argument in self.rir.call_args(args) {
                        self.visit(argument.value);
                    }
                }
                InstData::DropFnDecl { type_name, body } => {
                    let syntax = self.symbol(*type_name);
                    self.context.demand_type(&syntax, span);
                    visit!(*body);
                }
                InstData::AnonStructType {
                    fields, methods, ..
                } => {
                    self.context.demand_anonymous_nominal(reference, span);
                    for (_, ty) in self.rir.anon_struct_fields(fields) {
                        let syntax = self.symbol(ty);
                        self.context.demand_type(&syntax, span);
                    }
                    for method in self.rir.anon_struct_methods(methods) {
                        self.visit(method);
                    }
                }
                InstData::AnonEnumType {
                    variants, payloads, ..
                } => {
                    self.context.demand_anonymous_nominal(reference, span);
                    for payload in self.rir.anon_enum_payloads(payloads, variants) {
                        for ty in payload {
                            let syntax = self.symbol(ty);
                            self.context.demand_type(&syntax, span);
                        }
                    }
                }
            }
        }
    }

    Walker {
        context,
        rir,
        interner,
        visited: HashSet::new(),
    }
    .visit(body);
}
