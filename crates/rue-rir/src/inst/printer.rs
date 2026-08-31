//! Read-only textual presentation of RIR.

use super::*;

impl fmt::Display for InstRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "%{}", self.as_u32())
    }
}

struct DisplayedInstRef(u32);

impl fmt::Display for DisplayedInstRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "%{}", self.0)
    }
}

/// Printer for RIR that resolves symbols to their string values.
pub struct RirPrinter<'a, 'b> {
    rir: &'a Rir,
    interner: &'b lasso::ThreadedRodeo,
    instruction_order: Option<Vec<InstRef>>,
    displayed_refs: Option<Vec<u32>>,
    displayed_extra: Option<Vec<u32>>,
}

impl<'a, 'b> RirPrinter<'a, 'b> {
    fn format_type(&self, reference: RirTypeSyntaxRef) -> String {
        self.rir
            .type_syntax()
            .render_type_with(reference, |symbol| self.interner.resolve(symbol))
            .unwrap_or_else(|| "<invalid-type>".to_owned())
    }
    /// Create a new RIR printer.
    pub fn new(rir: &'a Rir, interner: &'b lasso::ThreadedRodeo) -> Self {
        Self {
            rir,
            interner,
            instruction_order: None,
            displayed_refs: None,
            displayed_extra: None,
        }
    }

    /// Create a read-only presentation of `rir` in a different instruction order.
    ///
    /// The supplied order must be a permutation of every instruction in the RIR.
    /// References are displayed in that order without cloning or rewriting the RIR.
    pub fn with_instruction_order(
        rir: &'a Rir,
        interner: &'b lasso::ThreadedRodeo,
        instruction_order: Vec<InstRef>,
    ) -> Self {
        assert_eq!(instruction_order.len(), rir.len());
        let mut displayed_refs = vec![u32::MAX; rir.len()];
        for (displayed, canonical) in instruction_order.iter().enumerate() {
            let slot = &mut displayed_refs[canonical.as_u32() as usize];
            assert_eq!(
                *slot,
                u32::MAX,
                "RIR presentation order contains a duplicate"
            );
            *slot = displayed as u32;
        }
        assert!(
            displayed_refs
                .iter()
                .all(|displayed| *displayed != u32::MAX)
        );
        Self {
            rir,
            interner,
            instruction_order: Some(instruction_order),
            displayed_refs: Some(displayed_refs),
            displayed_extra: None,
        }
    }

    /// Create a presentation that remaps both instruction and payload ordering.
    pub fn with_presentation_order(
        rir: &'a Rir,
        interner: &'b lasso::ThreadedRodeo,
        instruction_order: Vec<InstRef>,
        extra_order: Vec<u32>,
    ) -> Self {
        let mut printer = Self::with_instruction_order(rir, interner, instruction_order);
        assert_eq!(extra_order.len(), rir.extra_len());
        let mut displayed_extra = vec![u32::MAX; rir.extra_len()];
        for (displayed, canonical) in extra_order.into_iter().enumerate() {
            let slot = &mut displayed_extra[canonical as usize];
            assert_eq!(
                *slot,
                u32::MAX,
                "RIR payload presentation contains a duplicate"
            );
            *slot = displayed as u32;
        }
        assert!(
            displayed_extra
                .iter()
                .all(|displayed| *displayed != u32::MAX)
        );
        printer.displayed_extra = Some(displayed_extra);
        printer
    }

    fn display_ref(&self, inst: InstRef) -> DisplayedInstRef {
        DisplayedInstRef(
            self.displayed_refs
                .as_ref()
                .map_or(inst.as_u32(), |refs| refs[inst.as_u32() as usize]),
        )
    }

    /// Format a call argument with its mode prefix.
    fn format_call_arg(&self, arg: &RirCallArg) -> String {
        match arg.mode {
            RirArgMode::Inout => format!("inout {}", self.display_ref(arg.value)),
            RirArgMode::Borrow => format!("borrow {}", self.display_ref(arg.value)),
            RirArgMode::Normal => format!("{}", self.display_ref(arg.value)),
        }
    }

    /// Format a list of call arguments.
    fn format_call_args(&self, args: impl IntoIterator<Item = RirCallArg>) -> String {
        args.into_iter()
            .map(|arg| self.format_call_arg(&arg))
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// Format an item's directives as a `"@copy @allow(..) "` prefix
    /// (empty string when there are none).
    fn format_directives(&self, range: &RirDirectivesRange) -> String {
        let directives = self.rir.directives(range);
        if directives.len() == 0 {
            return String::new();
        }
        let dir_names: Vec<String> = directives
            .iter()
            .map(|d| format!("@{}", self.interner.resolve(&d.name)))
            .collect();
        format!("{} ", dir_names.join(" "))
    }

    /// Format a pattern for printing.
    fn format_pattern(&self, pat: &RirPatternView<'_>) -> String {
        match pat {
            RirPatternView::Wildcard(_) => "_".to_string(),
            RirPatternView::Int {
                value, negative, ..
            } => {
                if *negative {
                    format!("-{}", value)
                } else {
                    value.to_string()
                }
            }
            RirPatternView::Bool(b, _) => b.to_string(),
            RirPatternView::Path {
                module,
                type_name,
                variant,
                bindings,
                ..
            } => {
                let prefix = if let Some(module_ref) = module {
                    format!("{}..", self.display_ref(*module_ref))
                } else {
                    String::new()
                };
                let base = format!(
                    "{}{}::{}",
                    prefix,
                    self.interner.resolve(&*type_name),
                    self.interner.resolve(&*variant)
                );
                if bindings.is_empty() {
                    base
                } else {
                    let names: Vec<&str> =
                        bindings.iter().map(|b| self.interner.resolve(&b)).collect();
                    format!("{}({})", base, names.join(", "))
                }
            }
        }
    }

    /// Format the RIR as a string.
    pub fn to_string(&self) -> String {
        use std::fmt::Write;

        let mut out = String::new();
        let instruction_order: Box<dyn Iterator<Item = InstRef> + '_> =
            match &self.instruction_order {
                Some(order) => Box::new(order.iter().copied()),
                None => Box::new(self.rir.iter().map(|(inst_ref, _)| inst_ref)),
            };
        for inst_ref in instruction_order {
            let inst = self.rir.get(inst_ref);
            write!(out, "{} = ", self.display_ref(inst_ref)).unwrap();
            match &inst.data {
                // Constants
                InstData::IntConst(v) => writeln!(out, "const {}", v).unwrap(),
                // Printed with the `float` tag so a float literal is visibly
                // distinct from an integer one in a RIR dump: `1e9` and
                // `1000000000` are different nodes with the same value.
                InstData::FloatConst { text } => {
                    writeln!(out, "const float {}", self.interner.resolve(&*text)).unwrap()
                }
                InstData::BoolConst(v) => writeln!(out, "const {}", v).unwrap(),
                InstData::StringConst { content, .. } => {
                    writeln!(out, "const {:?}", self.interner.resolve(&*content)).unwrap()
                }
                InstData::UnitConst => writeln!(out, "const ()").unwrap(),

                // Binary operations
                InstData::Add { lhs, rhs } => writeln!(
                    out,
                    "add {}, {}",
                    self.display_ref(*lhs),
                    self.display_ref(*rhs)
                )
                .unwrap(),
                InstData::Sub { lhs, rhs } => writeln!(
                    out,
                    "sub {}, {}",
                    self.display_ref(*lhs),
                    self.display_ref(*rhs)
                )
                .unwrap(),
                InstData::Mul { lhs, rhs } => writeln!(
                    out,
                    "mul {}, {}",
                    self.display_ref(*lhs),
                    self.display_ref(*rhs)
                )
                .unwrap(),
                InstData::Div { lhs, rhs } => writeln!(
                    out,
                    "div {}, {}",
                    self.display_ref(*lhs),
                    self.display_ref(*rhs)
                )
                .unwrap(),
                InstData::Mod { lhs, rhs } => writeln!(
                    out,
                    "mod {}, {}",
                    self.display_ref(*lhs),
                    self.display_ref(*rhs)
                )
                .unwrap(),
                InstData::Eq { lhs, rhs } => writeln!(
                    out,
                    "eq {}, {}",
                    self.display_ref(*lhs),
                    self.display_ref(*rhs)
                )
                .unwrap(),
                InstData::Ne { lhs, rhs } => writeln!(
                    out,
                    "ne {}, {}",
                    self.display_ref(*lhs),
                    self.display_ref(*rhs)
                )
                .unwrap(),
                InstData::Lt { lhs, rhs } => writeln!(
                    out,
                    "lt {}, {}",
                    self.display_ref(*lhs),
                    self.display_ref(*rhs)
                )
                .unwrap(),
                InstData::Gt { lhs, rhs } => writeln!(
                    out,
                    "gt {}, {}",
                    self.display_ref(*lhs),
                    self.display_ref(*rhs)
                )
                .unwrap(),
                InstData::Le { lhs, rhs } => writeln!(
                    out,
                    "le {}, {}",
                    self.display_ref(*lhs),
                    self.display_ref(*rhs)
                )
                .unwrap(),
                InstData::Ge { lhs, rhs } => writeln!(
                    out,
                    "ge {}, {}",
                    self.display_ref(*lhs),
                    self.display_ref(*rhs)
                )
                .unwrap(),
                InstData::And { lhs, rhs } => writeln!(
                    out,
                    "and {}, {}",
                    self.display_ref(*lhs),
                    self.display_ref(*rhs)
                )
                .unwrap(),
                InstData::Or { lhs, rhs } => writeln!(
                    out,
                    "or {}, {}",
                    self.display_ref(*lhs),
                    self.display_ref(*rhs)
                )
                .unwrap(),
                InstData::BitAnd { lhs, rhs } => writeln!(
                    out,
                    "bit_and {}, {}",
                    self.display_ref(*lhs),
                    self.display_ref(*rhs)
                )
                .unwrap(),
                InstData::BitOr { lhs, rhs } => writeln!(
                    out,
                    "bit_or {}, {}",
                    self.display_ref(*lhs),
                    self.display_ref(*rhs)
                )
                .unwrap(),
                InstData::BitXor { lhs, rhs } => writeln!(
                    out,
                    "bit_xor {}, {}",
                    self.display_ref(*lhs),
                    self.display_ref(*rhs)
                )
                .unwrap(),
                InstData::Shl { lhs, rhs } => writeln!(
                    out,
                    "shl {}, {}",
                    self.display_ref(*lhs),
                    self.display_ref(*rhs)
                )
                .unwrap(),
                InstData::Shr { lhs, rhs } => writeln!(
                    out,
                    "shr {}, {}",
                    self.display_ref(*lhs),
                    self.display_ref(*rhs)
                )
                .unwrap(),

                // Unary operations
                InstData::Neg { operand } => {
                    writeln!(out, "neg {}", self.display_ref(*operand)).unwrap()
                }
                InstData::Not { operand } => {
                    writeln!(out, "not {}", self.display_ref(*operand)).unwrap()
                }
                InstData::BitNot { operand } => {
                    writeln!(out, "bit_not {}", self.display_ref(*operand)).unwrap()
                }
                InstData::Try { operand } => {
                    writeln!(out, "try {}", self.display_ref(*operand)).unwrap()
                }

                // Control flow
                InstData::Branch {
                    cond,
                    then_block,
                    else_block,
                } => {
                    if let Some(else_b) = else_block {
                        writeln!(
                            out,
                            "branch {}, {}, {}",
                            self.display_ref(*cond),
                            self.display_ref(*then_block),
                            self.display_ref(*else_b)
                        )
                        .unwrap();
                    } else {
                        writeln!(
                            out,
                            "branch {}, {}",
                            self.display_ref(*cond),
                            self.display_ref(*then_block)
                        )
                        .unwrap();
                    }
                }
                InstData::Loop { cond, body } => writeln!(
                    out,
                    "loop {}, {}",
                    self.display_ref(*cond),
                    self.display_ref(*body)
                )
                .unwrap(),
                InstData::InfiniteLoop { body, iter_borrow } => {
                    let borrow_str = iter_borrow
                        .map(|c| format!(" borrows {}", self.interner.resolve(&c)))
                        .unwrap_or_default();
                    writeln!(
                        out,
                        "infinite_loop {}{}",
                        self.display_ref(*body),
                        borrow_str
                    )
                    .unwrap()
                }
                InstData::Match { scrutinee, arms } => {
                    let arms = self.rir.match_arms(arms);
                    let arms_str: Vec<String> = arms
                        .iter()
                        .map(|(pat, body)| {
                            format!(
                                "{} => {}",
                                self.format_pattern(&pat),
                                self.display_ref(body)
                            )
                        })
                        .collect();
                    writeln!(
                        out,
                        "match {} {{ {} }}",
                        self.display_ref(*scrutinee),
                        arms_str.join(", ")
                    )
                    .unwrap();
                }
                InstData::Break { value } => match value {
                    Some(v) => writeln!(out, "break {}", self.display_ref(*v)).unwrap(),
                    None => writeln!(out, "break").unwrap(),
                },
                InstData::Continue => writeln!(out, "continue").unwrap(),

                // Functions
                InstData::FnDecl {
                    directives,
                    is_pub,
                    is_unchecked,
                    is_extern,
                    is_c_export,
                    name,
                    params,
                    return_type,
                    body,
                    has_self,
                    self_mode,
                    self_is_mut,
                    returns_borrow,
                    returns_inout,
                } => {
                    let pub_str = if *is_c_export {
                        "pub extern \"C\" "
                    } else if *is_pub {
                        "pub "
                    } else {
                        ""
                    };
                    let unchecked_str = if *is_unchecked {
                        "unchecked "
                    } else if *is_extern {
                        "extern "
                    } else {
                        ""
                    };
                    let name_str = self.interner.resolve(&*name);
                    let ret_str = self.format_type(*return_type);
                    let self_str = if *has_self {
                        match self_mode {
                            RirParamMode::Inout => "inout self, ",
                            RirParamMode::Borrow => "borrow self, ",
                            RirParamMode::Normal if *self_is_mut => "mut self, ",
                            RirParamMode::Normal => "self, ",
                        }
                    } else {
                        ""
                    };
                    let params = self.rir.params(params);
                    let params_str: Vec<String> = params
                        .values()
                        .map(|p| {
                            let comptime_prefix = if p.is_comptime { "comptime " } else { "" };
                            let mode_prefix = match p.mode {
                                RirParamMode::Inout => "inout ",
                                RirParamMode::Borrow => "borrow ",
                                RirParamMode::Normal => "",
                            };
                            format!(
                                "{}{}{}: {}",
                                comptime_prefix,
                                mode_prefix,
                                self.interner.resolve(&p.name),
                                self.format_type(p.ty)
                            )
                        })
                        .collect();
                    let directives_str = self.format_directives(directives);
                    let borrow_str = if *returns_borrow {
                        "borrow "
                    } else if *returns_inout {
                        "inout "
                    } else {
                        ""
                    };
                    writeln!(
                        out,
                        "{}{}{}fn {}({}{}) -> {}{} {{",
                        directives_str,
                        pub_str,
                        unchecked_str,
                        name_str,
                        self_str,
                        params_str.join(", "),
                        borrow_str,
                        ret_str
                    )
                    .unwrap();
                    writeln!(out, "    {}", self.display_ref(*body)).unwrap();
                    writeln!(out, "}}").unwrap();
                }
                InstData::ConstDecl {
                    directives,
                    is_pub,
                    name,
                    ty,
                    init,
                } => {
                    let directives_str = self.format_directives(directives);
                    let pub_str = if *is_pub { "pub " } else { "" };
                    let name_str = self.interner.resolve(&*name);
                    let ty_str = ty
                        .map(|t| format!(": {}", self.format_type(t)))
                        .unwrap_or_default();
                    writeln!(
                        out,
                        "{}{}const {}{} = {}",
                        directives_str,
                        pub_str,
                        name_str,
                        ty_str,
                        self.display_ref(*init)
                    )
                    .unwrap();
                }
                InstData::Ret(inner) => {
                    if let Some(inner) = inner {
                        writeln!(out, "ret {}", self.display_ref(*inner)).unwrap();
                    } else {
                        writeln!(out, "ret").unwrap();
                    }
                }
                InstData::Yield(inner) => {
                    writeln!(out, "yield {}", self.display_ref(*inner)).unwrap();
                }
                InstData::Call { name, args } => {
                    let name_str = self.interner.resolve(&*name);
                    let args = self.rir.call_args(args);
                    writeln!(out, "call {}({})", name_str, self.format_call_args(args)).unwrap();
                }
                InstData::Intrinsic { name, args } => {
                    let name_str = self.interner.resolve(&*name);
                    let args = self.rir.intrinsic_args(args);
                    let args_str: Vec<String> = args
                        .values()
                        .map(|a| self.display_ref(a).to_string())
                        .collect();
                    writeln!(out, "intrinsic @{}({})", name_str, args_str.join(", ")).unwrap();
                }
                InstData::InternalIntrinsic { intrinsic, args } => {
                    let args = self.rir.internal_intrinsic_args(args);
                    let args_str: Vec<String> = args
                        .values()
                        .map(|a| self.display_ref(a).to_string())
                        .collect();
                    writeln!(
                        out,
                        "internal_intrinsic @{}({})",
                        intrinsic.as_str(),
                        args_str.join(", ")
                    )
                    .unwrap();
                }
                InstData::TypeIntrinsic { name, type_arg } => {
                    let name_str = self.interner.resolve(&*name);
                    let type_str = self.format_type(*type_arg);
                    writeln!(out, "type_intrinsic @{}({})", name_str, type_str).unwrap();
                }
                InstData::OffsetOf { type_arg, field } => {
                    let type_str = self.format_type(*type_arg);
                    let field_str = self.interner.resolve(&*field);
                    writeln!(out, "offset_of @offset_of({}, {})", type_str, field_str).unwrap();
                }
                InstData::Block { instructions } => {
                    writeln!(out, "block({instructions:?})").unwrap();
                }

                // Variables
                InstData::Alloc {
                    directives,
                    name,
                    is_mut,
                    ty,
                    init,
                    iter_elem,
                } => {
                    let directives_str = self.format_directives(directives);
                    let name_str = name
                        .map(|n| self.interner.resolve(&n).to_string())
                        .unwrap_or_else(|| "_".to_string());
                    let mut_str = if *is_mut { "mut " } else { "" };
                    let ty_str = ty
                        .map(|t| format!(": {}", self.format_type(t)))
                        .unwrap_or_default();
                    let iter_str = if *iter_elem { " [iter_elem]" } else { "" };
                    writeln!(
                        out,
                        "{}alloc {}{}{}= {}{}",
                        directives_str,
                        mut_str,
                        name_str,
                        ty_str,
                        self.display_ref(*init),
                        iter_str
                    )
                    .unwrap();
                }
                InstData::VarRef { name, .. } => {
                    writeln!(out, "var_ref {}", self.interner.resolve(&*name)).unwrap();
                }
                InstData::Assign { name, value } => {
                    writeln!(
                        out,
                        "assign {} = {}",
                        self.interner.resolve(&*name),
                        self.display_ref(*value)
                    )
                    .unwrap();
                }
                InstData::PlaceSet { place, value } => {
                    writeln!(
                        out,
                        "place_set {} = {}",
                        self.display_ref(*place),
                        self.display_ref(*value)
                    )
                    .unwrap();
                }

                // Structs
                InstData::StructDecl {
                    directives,
                    is_pub,
                    is_linear,
                    name,
                    fields,
                    methods,
                } => {
                    let pub_str = if *is_pub { "pub " } else { "" };
                    let name_str = self.interner.resolve(&*name);
                    let fields = self.rir.struct_fields(fields);
                    let fields_str: Vec<String> = fields
                        .values()
                        .map(|(fname, ftype)| {
                            format!(
                                "{}: {}",
                                self.interner.resolve(&fname),
                                self.format_type(ftype)
                            )
                        })
                        .collect();
                    let linear_str = if *is_linear { "linear " } else { "" };
                    let directives_str = self.format_directives(directives);
                    let methods = self.rir.struct_methods(methods);
                    let methods_str = if methods.len() == 0 {
                        String::new()
                    } else {
                        let method_refs: Vec<String> = methods
                            .values()
                            .map(|m| self.display_ref(m).to_string())
                            .collect();
                        format!(" methods: [{}]", method_refs.join(", "))
                    };
                    writeln!(
                        out,
                        "{}{}{}struct {} {{ {} }}{}",
                        directives_str,
                        pub_str,
                        linear_str,
                        name_str,
                        fields_str.join(", "),
                        methods_str
                    )
                    .unwrap();
                }
                InstData::StructInit {
                    module,
                    ctor_head,
                    type_name,
                    fields,
                    shorthand_span: _,
                } => {
                    let module_str = match ctor_head {
                        Some(head) => format!("<{}>.", self.display_ref(*head)),
                        None => module
                            .map(|m| format!("{}.", self.display_ref(m)))
                            .unwrap_or_default(),
                    };
                    let type_str = self.interner.resolve(&*type_name);
                    let fields = self.rir.field_inits(fields);
                    let fields_str: Vec<String> = fields
                        .values()
                        .map(|(fname, value)| {
                            format!(
                                "{}: {}",
                                self.interner.resolve(&fname),
                                self.display_ref(value)
                            )
                        })
                        .collect();
                    writeln!(
                        out,
                        "struct_init {}{} {{ {} }}",
                        module_str,
                        type_str,
                        fields_str.join(", ")
                    )
                    .unwrap();
                }
                InstData::FieldGet { base, field } => {
                    writeln!(
                        out,
                        "field_get {}.{}",
                        self.display_ref(*base),
                        self.interner.resolve(&*field)
                    )
                    .unwrap();
                }
                InstData::FieldSet { base, field, value } => {
                    writeln!(
                        out,
                        "field_set {}.{} = {}",
                        self.display_ref(*base),
                        self.interner.resolve(&*field),
                        self.display_ref(*value)
                    )
                    .unwrap();
                }

                // Enums
                InstData::EnumDecl {
                    is_pub,
                    is_non_exhaustive,
                    name,
                    variants,
                    payloads,
                } => {
                    let pub_str = if *is_pub { "pub " } else { "" };
                    let marker = if *is_non_exhaustive {
                        "@non_exhaustive "
                    } else {
                        ""
                    };
                    let name_str = self.interner.resolve(&*name);
                    let payload_arities: Vec<usize> = self
                        .rir
                        .enum_payloads(payloads, variants)
                        .map(|payload| payload.len())
                        .collect();
                    let variants = self.rir.enum_variants(variants);
                    let variants_str: Vec<String> = variants
                        .iter()
                        .enumerate()
                        .map(|(i, v)| {
                            let base = self.interner.resolve(&v).to_string();
                            match payload_arities.get(i) {
                                Some(k) if *k > 0 => format!("{}/{}", base, k),
                                _ => base,
                            }
                        })
                        .collect();
                    writeln!(
                        out,
                        "{}{}enum {} {{ {} }}",
                        marker,
                        pub_str,
                        name_str,
                        variants_str.join(", ")
                    )
                    .unwrap();
                }
                InstData::EnumVariant {
                    module,
                    type_name,
                    variant,
                } => {
                    let module_str = module
                        .map(|m| format!("{}.", self.display_ref(m)))
                        .unwrap_or_default();
                    writeln!(
                        out,
                        "enum_variant {}{}::{}",
                        module_str,
                        self.interner.resolve(&*type_name),
                        self.interner.resolve(&*variant)
                    )
                    .unwrap();
                }

                // Arrays
                InstData::ArrayInit { elements } => {
                    let elements = self.rir.array_elements(elements);
                    let elems_str: Vec<String> = elements
                        .values()
                        .map(|e| self.display_ref(e).to_string())
                        .collect();
                    writeln!(out, "array_init [{}]", elems_str.join(", ")).unwrap();
                }
                InstData::ArrayRepeat { value, count } => {
                    let count_str = match count {
                        RepeatCount::Literal(n) => n.to_string(),
                        RepeatCount::Named(sym) => {
                            format!("sym:{}", sym.into_usize())
                        }
                    };
                    writeln!(
                        out,
                        "array_repeat [{}; {}]",
                        self.display_ref(*value),
                        count_str
                    )
                    .unwrap();
                }
                InstData::IndexGet { base, index } => {
                    writeln!(
                        out,
                        "index_get {}[{}]",
                        self.display_ref(*base),
                        self.display_ref(*index)
                    )
                    .unwrap();
                }
                InstData::IndexSet { base, index, value } => {
                    writeln!(
                        out,
                        "index_set {}[{}] = {}",
                        self.display_ref(*base),
                        self.display_ref(*index),
                        self.display_ref(*value)
                    )
                    .unwrap();
                }

                // Methods
                InstData::MethodCall {
                    receiver,
                    method,
                    args,
                } => {
                    let args = self.rir.call_args(args);
                    writeln!(
                        out,
                        "method_call {}.{}({})",
                        self.display_ref(*receiver),
                        self.interner.resolve(&*method),
                        self.format_call_args(args)
                    )
                    .unwrap();
                }

                // Drop
                InstData::DropFnDecl { type_name, body } => {
                    writeln!(
                        out,
                        "drop fn {}(self) {{",
                        self.interner.resolve(&*type_name)
                    )
                    .unwrap();
                    writeln!(out, "    {}", self.display_ref(*body)).unwrap();
                    writeln!(out, "}}").unwrap();
                }

                // Comptime block
                InstData::Comptime { expr } => {
                    writeln!(out, "comptime {{ {} }}", self.display_ref(*expr)).unwrap();
                }

                // Checked block
                InstData::Checked { expr } => {
                    writeln!(out, "checked {{ {} }}", self.display_ref(*expr)).unwrap();
                }

                // Type constant
                InstData::TypeConst { type_name } => {
                    let name = self.format_type(*type_name);
                    writeln!(out, "type {}", name).unwrap();
                }

                // Anonymous struct type
                InstData::AnonStructType {
                    fields, methods, ..
                } => {
                    write!(out, "struct {{ ").unwrap();
                    let fields = self.rir.anon_struct_fields(fields);
                    for (i, (name, ty)) in fields.values().enumerate() {
                        if i > 0 {
                            write!(out, ", ").unwrap();
                        }
                        let name_str = self.interner.resolve(&name);
                        let ty_str = self.format_type(ty);
                        write!(out, "{}: {}", name_str, ty_str).unwrap();
                    }
                    // Print methods if any
                    let methods = self.rir.anon_struct_methods(methods);
                    if !methods.is_empty() {
                        let methods_str: Vec<String> = methods
                            .values()
                            .map(|m| self.display_ref(m).to_string())
                            .collect();
                        if fields.len() != 0 {
                            write!(out, ", ").unwrap();
                        }
                        write!(out, "methods: [{}]", methods_str.join(", ")).unwrap();
                    }
                    writeln!(out, " }}").unwrap();
                }

                // Anonymous enum type
                InstData::AnonEnumType {
                    variants, payloads, ..
                } => {
                    let payload_arities: Vec<usize> = self
                        .rir
                        .anon_enum_payloads(payloads, variants)
                        .map(|payload| payload.len())
                        .collect();
                    let variants = self.rir.anon_enum_variants(variants);
                    let variants_str: Vec<String> = variants
                        .iter()
                        .enumerate()
                        .map(|(i, v)| {
                            let base = self.interner.resolve(&v).to_string();
                            match payload_arities.get(i) {
                                Some(k) if *k > 0 => format!("{}/{}", base, k),
                                _ => base,
                            }
                        })
                        .collect();
                    writeln!(out, "enum {{ {} }}", variants_str.join(", ")).unwrap();
                }
            }
        }
        out
    }
}

impl fmt::Display for RirPrinter<'_, '_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_string())
    }
}
