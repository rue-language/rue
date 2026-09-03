//! Structural printers for the error a test body's `?` traps on (ADR-0083 §1).
//!
//! When `?` fails inside a test body, the failure arm reports the error value
//! itself, not just the site. Rendering it is the job of a compiler-synthesized
//! function, one per error type, identified by
//! [`crate::FunctionInstanceKey::ErrorPrinter`] — keyed like drop glue, so every
//! `?` site on the same error type shares one instance instead of inlining a
//! rendering at each site.
//!
//! # Shape
//!
//! ```text
//! fn __rue_error_printer__<digest>(error: E) -> str
//! ```
//!
//! The body allocates one bounded buffer through the runtime allocator, appends
//! the rendering into it, and returns a `{ptr, len}` view of what it wrote. It
//! never frees the buffer, and it never drops its parameter: the only caller is
//! a failure arm that traps in the next instruction, so process teardown is the
//! reclamation, exactly as it is for every other trapping path (ADR-0083 §1
//! records this consequence).
//!
//! # Why it imports nothing
//!
//! Like the test dispatcher, this body reaches no standard-library declaration.
//! That is a hard requirement rather than a preference: the printer is demanded
//! by a body's `?`, not by a call the closure walked, so anything it named would
//! have to be scheduled into the request's closure after the fact. Integers are
//! therefore rendered by dividing them here rather than by `@to_string`, whose
//! `StrBuf` result is a source type; literal runs are `str` constants of this
//! body; and the buffer comes from the byte-shaped `@alloc` helper.
//!
//! The one shape it must know about is a byte string's, since the payload rules
//! render one verbatim. A core `str` is a compiler-owned builtin whose `{ptr,
//! len}` fields this module may read directly. `StrBuf` is a source type, so its
//! byte view is located *structurally* while the plan is built — the unique
//! `u64` field, and the unique `ptr u8` reachable through its aggregate fields —
//! rather than by field index or field name, and a `StrBuf` whose shape no
//! longer matches renders as its type name instead of by a stale projection.
//!
//! # Bounds
//!
//! Rendering is bounded to [`PAYLOAD_BUDGET`] bytes. Every append clamps to what
//! is left and records that it clamped; a run that overflowed the budget ends
//! with [`TRUNCATION_MARKER`], written into headroom the allocation reserves for
//! exactly that purpose, so truncation never has to rewind.

use std::sync::Arc;

use rue_air::{
    AirArgMode, AirPlaceBase, IntrinsicOperation, LangItem, Node, RuntimeCallKind,
    SemanticBodyAnchor, SemanticBodyCallArg, SemanticBodyInst, SemanticBodyInstData,
    SemanticBodyMatchArm, SemanticBodyPattern, SemanticBodyPlace, SemanticBodyProjection,
};

use crate::drop_glue::semantic_type_from_instance;

type Ty = rue_air::SemanticImportType<crate::StableDefinitionKey, crate::ModuleId>;
type Body = rue_air::SemanticBody<crate::StableDefinitionKey, crate::ModuleId>;
type Inst = SemanticBodyInst<crate::StableDefinitionKey, crate::ModuleId>;
type Data = SemanticBodyInstData<crate::StableDefinitionKey, crate::ModuleId>;
type Place = SemanticBodyPlace<crate::StableDefinitionKey, crate::ModuleId>;
type Projection = SemanticBodyProjection<crate::StableDefinitionKey, crate::ModuleId>;

/// Rendered-payload budget in bytes (ADR-0083 §2 capture bounds).
pub(crate) const PAYLOAD_BUDGET: u64 = 4096;

/// Appended when a rendering exceeded [`PAYLOAD_BUDGET`].
pub(crate) const TRUNCATION_MARKER: &str = " …[truncated]";

/// How one value renders once the walk has stopped descending.
///
/// Every one of these is a leaf: the payload rules render aggregates one level
/// deep, so a field that is itself an aggregate renders as its type name rather
/// than recursively.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum LeafRender {
    /// Fixed text — a type name, or the rendering of a value with no readable
    /// content (a raw pointer, a module, a unit).
    Literal(Arc<str>),
    /// Decimal, with a leading `-` for a negative value.
    Signed,
    /// Decimal.
    Unsigned,
    /// `true` or `false`.
    Bool,
    /// The value's own bytes, reached by these projections.
    Bytes(ByteView),
}

/// Where a byte string keeps its pointer and its length, as field projections
/// from the value itself.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct ByteView {
    pub(crate) pointer: Arc<[PrinterProjection]>,
    pub(crate) length: Arc<[PrinterProjection]>,
    /// Whether the pointer field is `ptr mut u8` rather than `ptr const u8`. A
    /// place read is typed by the field it lands on, so the two spellings are
    /// not interchangeable here.
    pub(crate) pointer_is_mut: bool,
}

/// One field step of a [`ByteView`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct PrinterProjection {
    pub(crate) nominal: crate::NominalInstanceKey,
    pub(crate) field_index: u32,
}

/// One rendered component: a struct field, or one payload of an enum variant.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct PrinterField {
    /// Empty for an enum payload, which renders positionally.
    pub(crate) name: Arc<str>,
    pub(crate) ty: crate::TypeInstanceKey,
    pub(crate) render: LeafRender,
}

/// One variant of an enum error type.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct PrinterVariant {
    pub(crate) name: Arc<str>,
    pub(crate) fields: Arc<[PrinterField]>,
}

/// The rendering plan for one error type.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum ErrorPrinterPlan {
    /// The error type renders as one value.
    Leaf(LeafRender),
    /// `{ field: value, … }`.
    Struct { fields: Arc<[PrinterField]> },
    /// `Variant` or `Variant(payload, …)`.
    Enum { variants: Arc<[PrinterVariant]> },
}

/// The exact plan an error printer is synthesized from.
///
/// This is to the printer what [`crate::type_queries::DropGlueFacts`] is to drop
/// glue: everything about the error type the body depends on, resolved once so
/// the synthesizer is a pure function of it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct ErrorPrinterFacts {
    pub(crate) plan: ErrorPrinterPlan,
}

impl crate::retained_charge::RetainedCharge for ErrorPrinterFacts {
    fn retained_charge(&self) -> u64 {
        match &self.plan {
            ErrorPrinterPlan::Leaf(render) => render.retained_charge(),
            ErrorPrinterPlan::Struct { fields } => fields.retained_charge(),
            ErrorPrinterPlan::Enum { variants } => variants.retained_charge(),
        }
    }
}

impl crate::retained_charge::RetainedCharge for LeafRender {
    fn retained_charge(&self) -> u64 {
        match self {
            Self::Literal(text) => text.retained_charge(),
            Self::Signed | Self::Unsigned | Self::Bool => 0,
            Self::Bytes(view) => {
                view.pointer
                    .len()
                    .saturating_add(view.length.len())
                    .saturating_mul(std::mem::size_of::<PrinterProjection>()) as u64
            }
        }
    }
}

impl crate::retained_charge::RetainedCharge for PrinterField {
    fn retained_charge(&self) -> u64 {
        self.name
            .retained_charge()
            .saturating_add(self.ty.retained_charge())
            .saturating_add(self.render.retained_charge())
    }
}

impl crate::retained_charge::RetainedCharge for PrinterVariant {
    fn retained_charge(&self) -> u64 {
        self.name
            .retained_charge()
            .saturating_add(self.fields.retained_charge())
    }
}

/// The declaration facts the planner reads.
///
/// The rooted request already owns an index over exactly these facts, so the
/// planner borrows it rather than querying: an error printer's plan depends on
/// nothing the selected declarations do not already carry.
pub(crate) trait ErrorPrinterTypes {
    /// Fields of a named or anonymous struct, in declaration order.
    fn struct_fields(&self, ty: &crate::TypeInstanceKey) -> Option<Vec<(Arc<str>, Ty)>>;
    /// Variants of a named or anonymous enum, in declaration order.
    fn enum_variants(&self, ty: &crate::TypeInstanceKey) -> Option<Vec<(Arc<str>, Vec<Ty>)>>;
    /// The language item a nominal is, when it is one.
    fn lang_item(&self, ty: &crate::TypeInstanceKey) -> Option<LangItem>;
    /// The source-level spelling used when a value renders as its type name.
    fn type_name(&self, ty: &crate::TypeInstanceKey) -> Arc<str>;
}

/// Build the rendering plan for one error type.
///
/// Total: an error type the payload rules cannot render is not a failure, it is
/// a value that renders as its own type name.
pub(crate) fn plan_error_printer(
    owner: &crate::TypeInstanceKey,
    types: &impl ErrorPrinterTypes,
) -> ErrorPrinterFacts {
    if let Some(variants) = types.enum_variants(owner) {
        let variants = variants
            .into_iter()
            .map(|(name, payloads)| PrinterVariant {
                name,
                fields: payloads
                    .iter()
                    .map(|ty| {
                        let ty = crate::drop_glue::type_instance_from_semantic(ty);
                        PrinterField {
                            name: Arc::from(""),
                            render: leaf_render(&ty, types),
                            ty,
                        }
                    })
                    .collect::<Vec<_>>()
                    .into(),
            })
            .collect::<Vec<_>>();
        return ErrorPrinterFacts {
            plan: ErrorPrinterPlan::Enum {
                variants: variants.into(),
            },
        };
    }
    // A byte string is a struct, and rendering its bytes beats rendering its
    // fields, so the leaf classification is consulted before the struct walk.
    if let Some(view) = byte_view(owner, types) {
        return ErrorPrinterFacts {
            plan: ErrorPrinterPlan::Leaf(LeafRender::Bytes(view)),
        };
    }
    if let Some(fields) = types.struct_fields(owner) {
        let fields = fields
            .into_iter()
            .map(|(name, ty)| {
                let ty = crate::drop_glue::type_instance_from_semantic(&ty);
                PrinterField {
                    name,
                    render: leaf_render(&ty, types),
                    ty,
                }
            })
            .collect::<Vec<_>>();
        return ErrorPrinterFacts {
            plan: ErrorPrinterPlan::Struct {
                fields: fields.into(),
            },
        };
    }
    ErrorPrinterFacts {
        plan: ErrorPrinterPlan::Leaf(leaf_render(owner, types)),
    }
}

/// Classify one value that the walk will not descend into.
fn leaf_render(ty: &crate::TypeInstanceKey, types: &impl ErrorPrinterTypes) -> LeafRender {
    use crate::TypeInstanceKey as T;
    match ty {
        T::I8 | T::I16 | T::I32 | T::I64 => LeafRender::Signed,
        T::U8 | T::U16 | T::U32 | T::U64 => LeafRender::Unsigned,
        T::Bool => LeafRender::Bool,
        T::Unit => LeafRender::Literal(Arc::from("()")),
        _ => match byte_view(ty, types) {
            Some(view) => LeafRender::Bytes(view),
            // One level deep, and no deeper: an aggregate reached here is a
            // field of the error type, and rendering its own fields would be
            // the second level the payload rules stop at.
            None => LeafRender::Literal(types.type_name(ty)),
        },
    }
}

/// Locate the `{pointer, length}` of a byte string, if `ty` is one.
///
/// Two shapes qualify. The core `str` view is a compiler-owned builtin: its
/// fields are fixed at `ptr` then `len`, and this module may read them by index
/// because it owns them. `StrBuf` is a source type whose internals belong to
/// std, so it is located structurally: the unique `u64` field is the length,
/// and the unique `ptr u8` reachable through the struct's aggregate fields is
/// the pointer. A `StrBuf` that ever stops matching that description renders as
/// its type name rather than through a projection that has gone stale.
fn byte_view(ty: &crate::TypeInstanceKey, types: &impl ErrorPrinterTypes) -> Option<ByteView> {
    if let crate::TypeInstanceKey::BuiltinNominal { kind, name } = ty
        && *kind == rue_air::AnonymousNominalKind::Struct
        && (name.as_ref() == "str" || rue_air::fixed_string_capacity(name).is_some())
    {
        let nominal = crate::NominalInstanceKey::Builtin {
            kind: rue_air::AnonymousNominalKind::Struct,
            name: name.clone(),
        };
        return Some(ByteView {
            pointer: vec![PrinterProjection {
                nominal: nominal.clone(),
                field_index: 0,
            }]
            .into(),
            length: vec![PrinterProjection {
                nominal,
                field_index: 1,
            }]
            .into(),
            pointer_is_mut: false,
        });
    }
    if types.lang_item(ty) != Some(LangItem::StrBuf) {
        return None;
    }
    let nominal = type_nominal(ty)?;
    let fields = types.struct_fields(ty)?;
    let length =
        unique_field(&fields, |ty| matches!(ty, Ty::U64)).map(|index| PrinterProjection {
            nominal: nominal.clone(),
            field_index: index,
        })?;
    let (pointer, pointer_is_mut) = byte_pointer_projections(ty, &nominal, &fields, types)?;
    Some(ByteView {
        pointer: pointer.into(),
        length: vec![length].into(),
        pointer_is_mut,
    })
}

/// The unique `ptr u8` field of `ty`, or of the unique aggregate field it has.
///
/// One level of descent is enough for every byte string the compiler knows:
/// `StrBuf` keeps its allocation in a shared growable-buffer core, so its
/// pointer is one field inside one field.
fn byte_pointer_projections(
    ty: &crate::TypeInstanceKey,
    nominal: &crate::NominalInstanceKey,
    fields: &[(Arc<str>, Ty)],
    types: &impl ErrorPrinterTypes,
) -> Option<(Vec<PrinterProjection>, bool)> {
    if let Some(index) = unique_field(fields, is_byte_pointer) {
        return Some((
            vec![PrinterProjection {
                nominal: nominal.clone(),
                field_index: index,
            }],
            is_mut_byte_pointer(&fields[index as usize].1),
        ));
    }
    let inner_index = unique_field(fields, |field| {
        matches!(field, Ty::Nominal(_) | Ty::AnonymousNominal(_))
    })?;
    let inner_ty = crate::drop_glue::type_instance_from_semantic(&fields[inner_index as usize].1);
    // Guard against a self-referential shape rather than recursing forever.
    if inner_ty == *ty {
        return None;
    }
    let inner_nominal = type_nominal(&inner_ty)?;
    let inner_fields = types.struct_fields(&inner_ty)?;
    let index = unique_field(&inner_fields, is_byte_pointer)?;
    Some((
        vec![
            PrinterProjection {
                nominal: nominal.clone(),
                field_index: inner_index,
            },
            PrinterProjection {
                nominal: inner_nominal,
                field_index: index,
            },
        ],
        is_mut_byte_pointer(&inner_fields[index as usize].1),
    ))
}

fn is_mut_byte_pointer(ty: &Ty) -> bool {
    matches!(ty, Ty::PtrMut(pointee) if matches!(**pointee, Ty::U8))
}

fn is_byte_pointer(ty: &Ty) -> bool {
    matches!(ty, Ty::PtrConst(pointee) | Ty::PtrMut(pointee) if matches!(**pointee, Ty::U8))
}

/// The index of the one field matching `predicate`, or `None` when zero or
/// several do. "Exactly one" is what makes a structural match unambiguous.
fn unique_field(fields: &[(Arc<str>, Ty)], predicate: impl Fn(&Ty) -> bool) -> Option<u32> {
    let mut found = None;
    for (index, (_, ty)) in fields.iter().enumerate() {
        if predicate(ty) {
            if found.is_some() {
                return None;
            }
            found = Some(u32::try_from(index).ok()?);
        }
    }
    found
}

fn type_nominal(ty: &crate::TypeInstanceKey) -> Option<crate::NominalInstanceKey> {
    match ty {
        crate::TypeInstanceKey::Nominal(nominal) => Some(nominal.clone()),
        crate::TypeInstanceKey::BuiltinNominal { kind, name } => {
            Some(crate::NominalInstanceKey::Builtin {
                kind: *kind,
                name: name.clone(),
            })
        }
        _ => None,
    }
}

/// Every type whose ABI slot width the synthesizer needs.
///
/// The parameter list is the error value flattened, so laying out a field means
/// knowing the width of every field before it — the same prerequisite drop glue
/// collects for the same reason.
pub(crate) fn collect_printer_plan_types(
    owner: &crate::TypeInstanceKey,
    facts: &ErrorPrinterFacts,
) -> std::collections::BTreeSet<crate::TypeInstanceKey> {
    let mut types = std::collections::BTreeSet::new();
    types.insert(owner.clone());
    match &facts.plan {
        ErrorPrinterPlan::Leaf(_) => {}
        ErrorPrinterPlan::Struct { fields } => {
            types.extend(fields.iter().map(|field| field.ty.clone()));
        }
        ErrorPrinterPlan::Enum { variants } => {
            for variant in variants.iter() {
                types.extend(variant.fields.iter().map(|field| field.ty.clone()));
            }
        }
    }
    types
}

/// Local slots the printer body declares.
mod slot {
    /// The rendering buffer.
    pub(super) const BUF: u32 = 0;
    /// Bytes written so far, never above the budget.
    pub(super) const LEN: u32 = 1;
    /// Non-zero once an append clamped.
    pub(super) const OVERFLOWED: u32 = 2;
    /// Bytes an append was asked for.
    pub(super) const WANTED: u32 = 3;
    /// Bytes the budget still has room for.
    pub(super) const AVAILABLE: u32 = 4;
    /// Bytes an append actually copies.
    pub(super) const COPIED: u32 = 5;
    /// The magnitude being rendered in decimal.
    pub(super) const VALUE: u32 = 6;
    /// Digits that magnitude needs.
    pub(super) const DIGITS: u32 = 7;
    /// Running power of ten used to count them.
    pub(super) const POWER: u32 = 8;
    /// Remaining magnitude while digits are emitted.
    pub(super) const REMAINDER: u32 = 9;
    /// Position of the digit being emitted, counted from the end.
    pub(super) const CURSOR: u32 = 10;
    /// The literal run currently being appended. A local occupies one slot per
    /// ABI word, and a `str` is two — `{ptr, len}` — so this one is last of the
    /// fixed slots and the base reserves both.
    pub(super) const TEXT: u32 = 11;
    /// First slot of the payload binding: an enum payload is a value, and a
    /// byte view needs a place, so a byte-string payload is bound here before
    /// it is projected. Its width comes from the payload's own layout.
    pub(super) const PAYLOAD: u32 = TEXT + 2;
}

/// The largest number of decimal digits a `u64` needs.
const MAX_DIGITS: u64 = 20;

/// Build the canonical printer body for one error type.
///
/// `slots` reports the ABI slot width of a type. Fact selection supplies a
/// width of one for everything, because the facts a body depends on are its
/// types, strings, and callees — never its parameter offsets; CFG evaluation
/// supplies the exact widths its layout prerequisites published.
pub(crate) fn synthesize_error_printer(
    owner: &crate::TypeInstanceKey,
    facts: &ErrorPrinterFacts,
    slots: &dyn Fn(&crate::TypeInstanceKey) -> Option<u32>,
) -> Result<Body, Arc<str>> {
    let width = |ty: &crate::TypeInstanceKey| {
        slots(ty).ok_or_else(|| {
            Arc::<str>::from(format!("missing layout for error-printer type {ty:?}"))
        })
    };
    let num_param_slots = width(owner)?;
    let owner_ty = semantic_type_from_instance(owner);
    let mut builder = Builder::default();
    let mut statements = builder.prologue();
    // The error value is one parameter, not one per slot. Naming it whole here
    // is what makes the CFG's parameter-area descriptor cover the value's full
    // width; every read below then goes through a place rooted at that one
    // parameter, so the address arithmetic is the compiler's own rather than a
    // second copy of it. Reading raw parameter slots would need this body to
    // restate the parameter area's slot order, which is exactly the knowledge
    // place lowering already owns.
    //
    // Declared, deliberately unreferenced: the descriptor is derived from the
    // AIR's `Param` instructions, and this body's real reads are place reads.
    // Evaluating a whole-value `Param` here would additionally *consume* an
    // error type that owns anything, and the reads that follow would then be
    // reads after a move. The parameter's drop schedule cannot supply the
    // descriptor instead, because this body deliberately has none (6.7:16).
    builder.add(Data::Param { index: 0 }, owner_ty.clone());

    // One payload binding serves every byte-string payload, one variant at a
    // time; it is as wide as the widest such payload and absent when no variant
    // carries one.
    let mut payload_slots = 0;
    if let ErrorPrinterPlan::Enum { variants } = &facts.plan {
        for variant in variants.iter() {
            for field in variant.fields.iter() {
                if matches!(field.render, LeafRender::Bytes(_)) {
                    payload_slots = payload_slots.max(width(&field.ty)?);
                }
            }
        }
    }

    match &facts.plan {
        ErrorPrinterPlan::Leaf(render) => {
            let source = LeafSource::Projected(Vec::new());
            builder.render_leaf(&mut statements, render, &source, &owner_ty, &owner_ty);
        }
        ErrorPrinterPlan::Struct { fields } => {
            let nominal = type_nominal(owner)
                .ok_or_else(|| Arc::<str>::from("a struct error type names a nominal"))?;
            builder.append_literal(&mut statements, "{");
            for (index, field) in fields.iter().enumerate() {
                builder.append_literal(&mut statements, if index == 0 { " " } else { ", " });
                builder.append_literal(&mut statements, &format!("{}: ", field.name));
                let source = LeafSource::Projected(vec![PrinterProjection {
                    nominal: nominal.clone(),
                    field_index: u32::try_from(index).unwrap_or(u32::MAX),
                }]);
                let field_ty = semantic_type_from_instance(&field.ty);
                builder.render_leaf(
                    &mut statements,
                    &field.render,
                    &source,
                    &owner_ty,
                    &field_ty,
                );
            }
            builder.append_literal(&mut statements, if fields.is_empty() { "}" } else { " }" });
        }
        ErrorPrinterPlan::Enum { variants } => {
            let nominal = type_nominal(owner)
                .ok_or_else(|| Arc::<str>::from("an enum error type names a nominal"))?;
            // Matching the value itself, rather than an integer read out of a
            // parameter slot, leaves the discriminant's position and width to
            // the same lowering an ordinary `match` uses.
            let scrutinee = builder.subject(&owner_ty);
            let mut arms = Vec::with_capacity(variants.len());
            for (index, variant) in variants.iter().enumerate() {
                let variant_index = u32::try_from(index).unwrap_or(u32::MAX);
                let mut arm = Vec::new();
                builder.append_literal(&mut arm, &variant.name);
                if !variant.fields.is_empty() {
                    builder.append_literal(&mut arm, "(");
                    for (position, field) in variant.fields.iter().enumerate() {
                        if position > 0 {
                            builder.append_literal(&mut arm, ", ");
                        }
                        let source = LeafSource::Payload {
                            base: scrutinee,
                            enum_key: nominal.clone(),
                            variant_index,
                            field_index: u32::try_from(position).unwrap_or(u32::MAX),
                        };
                        let field_ty = semantic_type_from_instance(&field.ty);
                        builder.render_leaf(&mut arm, &field.render, &source, &owner_ty, &field_ty);
                    }
                    builder.append_literal(&mut arm, ")");
                }
                let unit = builder.add(Data::UnitConst, Ty::Unit);
                let body = builder.add(
                    Data::Block {
                        statements: arm.into(),
                        value: unit,
                    },
                    Ty::Unit,
                );
                arms.push(SemanticBodyMatchArm {
                    pattern: SemanticBodyPattern::EnumVariant {
                        enum_key: nominal.clone(),
                        variant_index,
                    },
                    body,
                });
            }
            statements.push(builder.add(
                Data::Match {
                    scrutinee,
                    arms: arms.into(),
                },
                Ty::Unit,
            ));
        }
    }

    builder.epilogue(&mut statements);
    let view = builder.finish_view();
    let ret = builder.add(Data::Ret(Some(view)), Ty::Never);
    builder.add(
        Data::Block {
            statements: statements.into(),
            value: ret,
        },
        Ty::Never,
    );

    // Every literal run this body writes is `.rodata` the linker needs a symbol
    // for, and a symbol needs a stable identity: the printer is that identity's
    // producer, and the run's position in the body's string table is what
    // distinguishes one run from another. Runs are interned by content, so the
    // table has no duplicates and the anchor is unique per atom.
    let local_atoms = builder
        .strings
        .iter()
        .enumerate()
        .map(|(index, content)| rue_air::SemanticBodyLocalAtom {
            identity: rue_air::LocalAtomId {
                producer: error_printer_identity(owner),
                kind: rue_air::LocalAtomKind::ReadOnlyData,
                anchor: rue_rir::RirStructuralAnchor::new(vec![
                    rue_rir::RirStructuralPathSegment::Body,
                    rue_rir::RirStructuralPathSegment::ReadOnlyData(
                        u32::try_from(index).unwrap_or(u32::MAX),
                    ),
                ]),
            },
            content: content.clone(),
        })
        .collect::<Vec<_>>();

    Ok(Body {
        is_accessor: false,
        return_type: str_type(),
        instructions: builder.instructions.into(),
        places: builder.places.into(),
        strings: builder.strings.into(),
        local_atoms: local_atoms.into(),
        // The error value is not dropped. The only caller traps in its next
        // instruction, so there is nothing after this body for a destructor to
        // protect (ADR-0083 §1 accepts exactly this).
        param_drops: Arc::new([]),
        // The payload binding aliases the parameter's own storage rather than
        // taking ownership of it; see the byte-view rendering above.
        borrow_slots: if payload_slots > 0 {
            Arc::new([slot::PAYLOAD])
        } else {
            Arc::new([])
        },
        num_locals: slot::PAYLOAD.saturating_add(payload_slots),
        num_param_slots,
        param_by_ref: vec![false; num_param_slots as usize].into(),
        param_writable: vec![false; num_param_slots as usize].into(),
        allow_unreachable_code: false,
        warnings: Arc::new([]),
        method_references: Arc::new([]),
    })
}

fn str_type() -> Ty {
    Ty::BuiltinNominal {
        kind: rue_air::SemanticImportNominalKind::Struct,
        name: Arc::from("str"),
    }
}

fn str_nominal() -> crate::NominalInstanceKey {
    crate::NominalInstanceKey::Builtin {
        kind: rue_air::AnonymousNominalKind::Struct,
        name: Arc::from("str"),
    }
}

fn byte_pointer_type() -> Ty {
    Ty::PtrMut(Arc::new(Ty::U8))
}

/// The base a printer place is rooted at.
enum PlaceRoot {
    /// The one parameter, optionally reached through a field prefix.
    Param(Vec<PrinterProjection>),
    /// A local the printer bound a payload to.
    Local(u32),
}

/// Where one rendered leaf lives inside the printer's single parameter.
enum LeafSource {
    /// Reached from the parameter by field projections: the parameter itself
    /// when the list is empty, one of its fields otherwise.
    Projected(Vec<PrinterProjection>),
    /// One payload of the variant an arm matched, projected out of the enum
    /// value `base`. A payload is a value rather than a place, so a byte view
    /// binds it to [`slot::PAYLOAD`] first.
    ///
    /// `base` is the *one* read of the parameter this body performs. An error
    /// type that owns anything is moved by that read, so reading the parameter
    /// again per payload would be a use after move; every arm and every payload
    /// therefore projects out of the same materialized value, exactly as the
    /// `?` desugaring itself does with its scrutinee.
    Payload {
        base: u32,
        enum_key: crate::NominalInstanceKey,
        variant_index: u32,
        field_index: u32,
    },
}

/// Instruction accumulator for the generated body.
///
/// Like the dispatcher's, this only ever appends, and a value read more than
/// once is bound to a local rather than shared across a branch boundary.
#[derive(Default)]
struct Builder {
    instructions: Vec<Inst>,
    places: Vec<Place>,
    strings: Vec<Arc<str>>,
}

impl Builder {
    fn add(&mut self, data: Data, ty: Ty) -> u32 {
        let index = u32::try_from(self.instructions.len()).expect("printer body fits u32");
        self.instructions.push(SemanticBodyInst {
            data,
            ty,
            anchor: SemanticBodyAnchor { start: 0, end: 0 },
        });
        index
    }

    fn constant(&mut self, value: u64, ty: Ty) -> u32 {
        self.add(Data::Const(value), ty)
    }

    fn load(&mut self, slot: u32, ty: Ty) -> u32 {
        self.add(Data::Load { slot }, ty)
    }

    fn store(&mut self, slot: u32, value: u32) -> u32 {
        self.add(Data::Store { slot, value }, Ty::Unit)
    }

    fn block(&mut self, statements: Vec<u32>) -> u32 {
        let unit = self.add(Data::UnitConst, Ty::Unit);
        self.add(
            Data::Block {
                statements: statements.into(),
                value: unit,
            },
            Ty::Unit,
        )
    }

    /// `if <condition> { <taken> }` as one statement.
    fn guard(&mut self, condition: u32, taken: Vec<u32>) -> u32 {
        let taken = self.block(taken);
        self.add(
            Data::Branch {
                cond: condition,
                then_value: taken,
                else_value: None,
            },
            Ty::Unit,
        )
    }

    /// `if <condition> { <taken> } else { <otherwise> }` as one statement.
    fn choose(&mut self, condition: u32, taken: Vec<u32>, otherwise: Vec<u32>) -> u32 {
        let taken = self.block(taken);
        let otherwise = self.block(otherwise);
        self.add(
            Data::Branch {
                cond: condition,
                then_value: taken,
                else_value: Some(otherwise),
            },
            Ty::Unit,
        )
    }

    fn place(&mut self, place: Place) -> u32 {
        let index = u32::try_from(self.places.len()).expect("printer place count fits u32");
        self.places.push(place);
        index
    }

    fn string(&mut self, text: &str) -> u32 {
        if let Some(index) = self.strings.iter().position(|held| held.as_ref() == text) {
            return u32::try_from(index).expect("printer string count fits u32");
        }
        let index = u32::try_from(self.strings.len()).expect("printer string count fits u32");
        self.strings.push(Arc::from(text));
        index
    }

    /// Declare every local and take the buffer.
    ///
    /// The allocation reserves the truncation marker's bytes past the budget, so
    /// a truncated rendering appends the marker rather than rewinding to make
    /// room for it.
    fn prologue(&mut self) -> Vec<u32> {
        let mut statements = Vec::new();
        let size = self.constant(
            PAYLOAD_BUDGET.saturating_add(TRUNCATION_MARKER.len() as u64),
            Ty::U64,
        );
        let align = self.constant(1, Ty::U64);
        let buffer = self.add(
            Data::RuntimeCall {
                runtime: RuntimeCallKind::Alloc,
                args: Arc::new([argument(size), argument(align)]),
            },
            byte_pointer_type(),
        );
        statements.push(self.bind(slot::BUF, buffer, byte_pointer_type()));
        for slot in [
            slot::LEN,
            slot::OVERFLOWED,
            slot::WANTED,
            slot::AVAILABLE,
            slot::COPIED,
            slot::VALUE,
            slot::DIGITS,
            slot::POWER,
            slot::REMAINDER,
            slot::CURSOR,
        ] {
            let zero = self.constant(0, Ty::U64);
            statements.push(self.bind(slot, zero, Ty::U64));
        }
        let index = self.string("");
        let empty = self.add(Data::StringConst(index), str_type());
        statements.push(self.bind(slot::TEXT, empty, str_type()));
        statements
    }

    /// Introduce a local slot holding `value`, as one statement.
    fn bind(&mut self, slot: u32, value: u32, ty: Ty) -> u32 {
        let live = self.add(Data::StorageLive { slot }, ty);
        let allocation = self.add(Data::Alloc { slot, init: value }, Ty::Unit);
        self.add(
            Data::Block {
                statements: Arc::new([live]),
                value: allocation,
            },
            Ty::Unit,
        )
    }

    /// Append the truncation marker when anything clamped.
    fn epilogue(&mut self, statements: &mut Vec<u32>) {
        let overflowed = self.load(slot::OVERFLOWED, Ty::U64);
        let zero = self.constant(0, Ty::U64);
        let clamped = self.add(Data::Ne(overflowed, zero), Ty::Bool);
        let mut taken = Vec::new();
        // The marker goes into the reserved headroom, so it is written without
        // the clamping every other append does: `len` is exactly the budget
        // here, and the allocation is the budget plus the marker.
        let budget = self.constant(PAYLOAD_BUDGET, Ty::U64);
        taken.push(self.store(slot::LEN, budget));
        let index = self.string(TRUNCATION_MARKER);
        let marker = self.add(Data::StringConst(index), str_type());
        taken.push(self.store(slot::TEXT, marker));
        let base = self.load(slot::BUF, byte_pointer_type());
        let offset = self.load(slot::LEN, Ty::U64);
        let destination = self.add(
            Data::Intrinsic {
                operation: IntrinsicOperation::PtrOffset,
                name: Arc::from(IntrinsicOperation::PtrOffset.expected_spelling()),
                args: Arc::new([argument(base), argument(offset)]),
            },
            byte_pointer_type(),
        );
        let source = self.text_pointer();
        let length = self.text_length();
        taken.push(self.add(
            Data::RuntimeCall {
                runtime: RuntimeCallKind::ByteCopy,
                args: Arc::new([argument(destination), argument(source), argument(length)]),
            },
            Ty::Unit,
        ));
        let written = self.load(slot::LEN, Ty::U64);
        let marker_len = self.constant(TRUNCATION_MARKER.len() as u64, Ty::U64);
        let total = self.add(Data::Add(written, marker_len), Ty::U64);
        taken.push(self.store(slot::LEN, total));
        let guard = self.guard(clamped, taken);
        statements.push(guard);
    }

    /// `str { ptr: buffer, len: written }`.
    fn finish_view(&mut self) -> u32 {
        let pointer = self.load(slot::BUF, byte_pointer_type());
        let length = self.load(slot::LEN, Ty::U64);
        self.add(
            Data::StructInit {
                struct_key: str_nominal(),
                fields: Arc::new([pointer, length]),
                source_order: Arc::new([0, 1]),
            },
            str_type(),
        )
    }

    /// The `ptr` field of the literal run held in [`slot::TEXT`].
    fn text_pointer(&mut self) -> u32 {
        let place = self.place(Place {
            base: AirPlaceBase::Local(slot::TEXT),
            base_type: str_type(),
            projections: Arc::new([Projection::Field {
                struct_key: str_nominal(),
                field_index: 0,
            }]),
        });
        self.add(Data::PlaceRead { place }, Ty::PtrConst(Arc::new(Ty::U8)))
    }

    /// The `len` field of the literal run held in [`slot::TEXT`].
    fn text_length(&mut self) -> u32 {
        let place = self.place(Place {
            base: AirPlaceBase::Local(slot::TEXT),
            base_type: str_type(),
            projections: Arc::new([Projection::Field {
                struct_key: str_nominal(),
                field_index: 1,
            }]),
        });
        self.add(Data::PlaceRead { place }, Ty::U64)
    }

    /// Append one fixed run of bytes.
    fn append_literal(&mut self, statements: &mut Vec<u32>, text: &str) {
        if text.is_empty() {
            return;
        }
        let index = self.string(text);
        let constant = self.add(Data::StringConst(index), str_type());
        statements.push(self.store(slot::TEXT, constant));
        let length = self.text_length();
        self.append_bytes(statements, length, |builder| builder.text_pointer());
    }

    /// Append `length` bytes read from the pointer `pointer` builds, clamped to
    /// what the budget still allows.
    ///
    /// The pointer is built inside the copying branch rather than bound to a
    /// local: it is a pure read, and the branch is the only consumer, so
    /// re-emitting it there is cheaper than a slot whose type would have to
    /// cover both a `ptr const u8` and a `ptr mut u8` source.
    fn append_bytes(
        &mut self,
        statements: &mut Vec<u32>,
        length: u32,
        pointer: impl FnOnce(&mut Self) -> u32,
    ) {
        statements.push(self.store(slot::WANTED, length));
        let budget = self.constant(PAYLOAD_BUDGET, Ty::U64);
        let written = self.load(slot::LEN, Ty::U64);
        // `len` never exceeds the budget, so this cannot borrow.
        let available = self.add(Data::WrappingSub(budget, written), Ty::U64);
        statements.push(self.store(slot::AVAILABLE, available));
        let wanted = self.load(slot::WANTED, Ty::U64);
        let room = self.load(slot::AVAILABLE, Ty::U64);
        let fits = self.add(Data::Le(wanted, room), Ty::Bool);
        let all = self.load(slot::WANTED, Ty::U64);
        let some = self.load(slot::AVAILABLE, Ty::U64);
        let copied = self.add(
            Data::Branch {
                cond: fits,
                then_value: all,
                else_value: Some(some),
            },
            Ty::U64,
        );
        statements.push(self.store(slot::COPIED, copied));

        let count = self.load(slot::COPIED, Ty::U64);
        let zero = self.constant(0, Ty::U64);
        let nonempty = self.add(Data::Ne(count, zero), Ty::Bool);
        let base = self.load(slot::BUF, byte_pointer_type());
        let offset = self.load(slot::LEN, Ty::U64);
        let destination = self.add(
            Data::Intrinsic {
                operation: IntrinsicOperation::PtrOffset,
                name: Arc::from(IntrinsicOperation::PtrOffset.expected_spelling()),
                args: Arc::new([argument(base), argument(offset)]),
            },
            byte_pointer_type(),
        );
        let source = pointer(self);
        let amount = self.load(slot::COPIED, Ty::U64);
        let copy = self.add(
            Data::RuntimeCall {
                runtime: RuntimeCallKind::ByteCopy,
                args: Arc::new([argument(destination), argument(source), argument(amount)]),
            },
            Ty::Unit,
        );
        let guard = self.guard(nonempty, vec![copy]);
        statements.push(guard);

        let actual = self.load(slot::COPIED, Ty::U64);
        let requested = self.load(slot::WANTED, Ty::U64);
        let clamped = self.add(Data::Ne(actual, requested), Ty::Bool);
        let one = self.constant(1, Ty::U64);
        let mark = self.store(slot::OVERFLOWED, one);
        let guard = self.guard(clamped, vec![mark]);
        statements.push(guard);

        let written = self.load(slot::LEN, Ty::U64);
        let amount = self.load(slot::COPIED, Ty::U64);
        let total = self.add(Data::Add(written, amount), Ty::U64);
        statements.push(self.store(slot::LEN, total));
    }

    /// Emit the rendering of one leaf.
    fn render_leaf(
        &mut self,
        statements: &mut Vec<u32>,
        render: &LeafRender,
        source: &LeafSource,
        owner_ty: &Ty,
        leaf_ty: &Ty,
    ) {
        match render {
            LeafRender::Literal(text) => self.append_literal(statements, text),
            LeafRender::Bool => {
                let read = self.read_leaf(source, owner_ty, leaf_ty);
                let mut taken = Vec::new();
                self.append_literal(&mut taken, "true");
                let mut otherwise = Vec::new();
                self.append_literal(&mut otherwise, "false");
                let branch = self.choose(read, taken, otherwise);
                statements.push(branch);
            }
            LeafRender::Unsigned => {
                let read = self.read_leaf(source, owner_ty, leaf_ty);
                let widened = self.widen(read, leaf_ty, Ty::U64);
                statements.push(self.store(slot::VALUE, widened));
                self.append_decimal(statements);
            }
            LeafRender::Signed => {
                let read = self.read_leaf(source, owner_ty, leaf_ty);
                let widened = self.widen(read, leaf_ty, Ty::I64);
                let zero = self.constant(0, Ty::I64);
                let negative = self.add(Data::Lt(widened, zero), Ty::Bool);
                // The magnitude is computed by wrapping negation so the most
                // negative value keeps its magnitude rather than trapping: its
                // wrapped bit pattern read as unsigned is exactly `2^63`.
                let read = self.read_leaf(source, owner_ty, leaf_ty);
                let widened = self.widen(read, leaf_ty, Ty::I64);
                let origin = self.constant(0, Ty::I64);
                let magnitude = self.add(Data::WrappingSub(origin, widened), Ty::I64);
                let magnitude = self.add(
                    Data::IntCast {
                        value: magnitude,
                        from_ty: Ty::I64,
                    },
                    Ty::U64,
                );
                let mut taken = Vec::new();
                self.append_literal(&mut taken, "-");
                taken.push(self.store(slot::VALUE, magnitude));
                let read = self.read_leaf(source, owner_ty, leaf_ty);
                let widened = self.widen(read, leaf_ty, Ty::I64);
                let positive = self.add(
                    Data::IntCast {
                        value: widened,
                        from_ty: Ty::I64,
                    },
                    Ty::U64,
                );
                let otherwise = vec![self.store(slot::VALUE, positive)];
                let branch = self.choose(negative, taken, otherwise);
                statements.push(branch);
                self.append_decimal(statements);
            }
            LeafRender::Bytes(view) => {
                // A byte view is two field reads from the same value, which
                // needs a place. A projected leaf already is one; a payload is a
                // value, so it is bound to the payload slot first. That binding
                // is declared non-owning (`borrow_slots` below): the error value
                // the payload came from is the parameter, which this body never
                // destroys, so a second owner here would only add a destructor
                // call to a path that traps before it could matter — and one
                // this request never rooted.
                let (base, base_type) = match source {
                    LeafSource::Projected(projections) => {
                        (PlaceRoot::Param(projections.clone()), owner_ty.clone())
                    }
                    LeafSource::Payload { .. } => {
                        let value = self.read_leaf(source, owner_ty, leaf_ty);
                        statements.push(self.bind(slot::PAYLOAD, value, leaf_ty.clone()));
                        (PlaceRoot::Local(slot::PAYLOAD), leaf_ty.clone())
                    }
                };
                let pointer_ty = if view.pointer_is_mut {
                    byte_pointer_type()
                } else {
                    Ty::PtrConst(Arc::new(Ty::U8))
                };
                let length = self.rooted_read(&base, &base_type, &view.length, Ty::U64);
                let pointer_projections = view.pointer.clone();
                self.append_bytes(statements, length, move |builder| {
                    builder.rooted_read(&base, &base_type, &pointer_projections, pointer_ty)
                });
            }
        }
    }

    /// The whole error value, read through its own parameter place.
    fn subject(&mut self, owner_ty: &Ty) -> u32 {
        self.rooted_read(
            &PlaceRoot::Param(Vec::new()),
            owner_ty,
            &[],
            owner_ty.clone(),
        )
    }

    /// Read one leaf's value.
    ///
    /// Re-emitted per use rather than shared: a value is lowered where it is
    /// first named, and the decimal renderer names its subject inside two
    /// different branch arms.
    fn read_leaf(&mut self, source: &LeafSource, owner_ty: &Ty, leaf_ty: &Ty) -> u32 {
        match source {
            LeafSource::Projected(projections) => self.rooted_read(
                &PlaceRoot::Param(Vec::new()),
                owner_ty,
                projections,
                leaf_ty.clone(),
            ),
            LeafSource::Payload {
                base,
                enum_key,
                variant_index,
                field_index,
            } => self.add(
                Data::EnumPayloadGet {
                    base: *base,
                    enum_key: enum_key.clone(),
                    variant_index: *variant_index,
                    field_index: *field_index,
                },
                leaf_ty.clone(),
            ),
        }
    }

    /// Read `projections` from a place rooted at the parameter or a local.
    fn rooted_read(
        &mut self,
        root: &PlaceRoot,
        base_type: &Ty,
        projections: &[PrinterProjection],
        ty: Ty,
    ) -> u32 {
        let (base, prefix) = match root {
            PlaceRoot::Param(prefix) => (AirPlaceBase::Param(0), prefix.as_slice()),
            PlaceRoot::Local(slot) => (AirPlaceBase::Local(*slot), [].as_slice()),
        };
        let place = self.place(Place {
            base,
            base_type: base_type.clone(),
            projections: prefix
                .iter()
                .chain(projections)
                .map(|step| Projection::Field {
                    struct_key: step.nominal.clone(),
                    field_index: step.field_index,
                })
                .collect::<Vec<_>>()
                .into(),
        });
        self.add(Data::PlaceRead { place }, ty)
    }

    /// Widen a narrow integer to the width the decimal renderer works in.
    fn widen(&mut self, value: u32, from: &Ty, to: Ty) -> u32 {
        if *from == to {
            return value;
        }
        self.add(
            Data::IntCast {
                value,
                from_ty: from.clone(),
            },
            to,
        )
    }

    /// Append the decimal digits of the magnitude in [`slot::VALUE`].
    ///
    /// Counting the digits first and then filling them in from the end is what
    /// lets the digits be written in place: the alternative — emitting them
    /// least-significant first and reversing — would need a second buffer.
    fn append_decimal(&mut self, statements: &mut Vec<u32>) {
        let one = self.constant(1, Ty::U64);
        statements.push(self.store(slot::DIGITS, one));
        let ten = self.constant(10, Ty::U64);
        statements.push(self.store(slot::POWER, ten));

        let digits = self.load(slot::DIGITS, Ty::U64);
        let limit = self.constant(MAX_DIGITS, Ty::U64);
        let more_room = self.add(Data::Lt(digits, limit), Ty::Bool);
        let value = self.load(slot::VALUE, Ty::U64);
        let power = self.load(slot::POWER, Ty::U64);
        let larger = self.add(Data::Ge(value, power), Ty::Bool);
        let condition = self.add(Data::And(more_room, larger), Ty::Bool);
        let digits = self.load(slot::DIGITS, Ty::U64);
        let one = self.constant(1, Ty::U64);
        let next = self.add(Data::Add(digits, one), Ty::U64);
        let advance = self.store(slot::DIGITS, next);
        let power = self.load(slot::POWER, Ty::U64);
        let ten = self.constant(10, Ty::U64);
        // Twenty digits fill a `u64` exactly, so the last multiply here is a
        // legitimate overflow rather than an error; the digit count stops the
        // loop before the wrapped value can be compared against.
        let scaled = self.add(Data::WrappingMul(power, ten), Ty::U64);
        let scale = self.store(slot::POWER, scaled);
        let body = self.block(vec![advance, scale]);
        statements.push(self.add(
            Data::Loop {
                cond: condition,
                body,
            },
            Ty::Unit,
        ));

        let written = self.load(slot::LEN, Ty::U64);
        let digits = self.load(slot::DIGITS, Ty::U64);
        let end = self.add(Data::Add(written, digits), Ty::U64);
        let budget = self.constant(PAYLOAD_BUDGET, Ty::U64);
        let fits = self.add(Data::Le(end, budget), Ty::Bool);

        let mut taken = Vec::new();
        let digits = self.load(slot::DIGITS, Ty::U64);
        taken.push(self.store(slot::CURSOR, digits));
        let value = self.load(slot::VALUE, Ty::U64);
        taken.push(self.store(slot::REMAINDER, value));
        let cursor = self.load(slot::CURSOR, Ty::U64);
        let zero = self.constant(0, Ty::U64);
        let remaining = self.add(Data::Gt(cursor, zero), Ty::Bool);
        let mut step = Vec::new();
        let cursor = self.load(slot::CURSOR, Ty::U64);
        let one = self.constant(1, Ty::U64);
        let back = self.add(Data::Sub(cursor, one), Ty::U64);
        step.push(self.store(slot::CURSOR, back));
        let base = self.load(slot::BUF, byte_pointer_type());
        let written = self.load(slot::LEN, Ty::U64);
        let cursor = self.load(slot::CURSOR, Ty::U64);
        let offset = self.add(Data::Add(written, cursor), Ty::U64);
        let destination = self.add(
            Data::Intrinsic {
                operation: IntrinsicOperation::PtrOffset,
                name: Arc::from(IntrinsicOperation::PtrOffset.expected_spelling()),
                args: Arc::new([argument(base), argument(offset)]),
            },
            byte_pointer_type(),
        );
        let remainder = self.load(slot::REMAINDER, Ty::U64);
        let ten = self.constant(10, Ty::U64);
        let digit = self.add(Data::Mod(remainder, ten), Ty::U64);
        let zero_byte = self.constant(u64::from(b'0'), Ty::U64);
        let byte = self.add(Data::Add(zero_byte, digit), Ty::U64);
        let byte = self.add(
            Data::IntCast {
                value: byte,
                from_ty: Ty::U64,
            },
            Ty::U8,
        );
        step.push(self.add(
            Data::Intrinsic {
                operation: IntrinsicOperation::PtrWrite,
                name: Arc::from(IntrinsicOperation::PtrWrite.expected_spelling()),
                args: Arc::new([argument(destination), argument(byte)]),
            },
            Ty::Unit,
        ));
        let remainder = self.load(slot::REMAINDER, Ty::U64);
        let ten = self.constant(10, Ty::U64);
        let shifted = self.add(Data::Div(remainder, ten), Ty::U64);
        step.push(self.store(slot::REMAINDER, shifted));
        let step = self.block(step);
        taken.push(self.add(
            Data::Loop {
                cond: remaining,
                body: step,
            },
            Ty::Unit,
        ));
        let written = self.load(slot::LEN, Ty::U64);
        let digits = self.load(slot::DIGITS, Ty::U64);
        let total = self.add(Data::Add(written, digits), Ty::U64);
        taken.push(self.store(slot::LEN, total));

        let one = self.constant(1, Ty::U64);
        let otherwise = vec![self.store(slot::OVERFLOWED, one)];
        let branch = self.choose(fits, taken, otherwise);
        statements.push(branch);
    }
}

fn argument(value: u32) -> SemanticBodyCallArg {
    SemanticBodyCallArg {
        value,
        mode: AirArgMode::Normal,
    }
}

/// The identity of the printer for `owner`.
pub(crate) fn error_printer_identity(owner: &crate::TypeInstanceKey) -> crate::FunctionInstanceKey {
    crate::FunctionInstanceKey::ErrorPrinter(Node::new(owner.clone()))
}
