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
//! The one exception is the float formatter: shortest-round-trip decimal is not
//! something to reimplement here, so a float leaf calls the same runtime helper
//! `@to_string` and `@dbg` share. A runtime helper is not a standard-library
//! declaration — it is an ABI export, like the allocator this body already
//! calls — but its result *would* be a `StrBuf`, which is a source type, so
//! this body receives the helper's out-pointer triple as the three machine
//! words it already is (see `float_text_type`).
//!
//! The one shape it must know about is a byte string's, since the payload rules
//! render one verbatim. A core `str` is a compiler-owned builtin whose `{ptr,
//! len}` fields this module may read directly. `StrBuf` is a source type, so its
//! byte view is located *structurally* while the plan is built — the unique
//! `u64` field, and the unique `ptr u8` reachable through its aggregate fields —
//! rather than by field index or field name, and a `StrBuf` whose shape no
//! longer matches renders as its type name instead of by a stale projection.
//!
//! # Standard containers
//!
//! One level of struct rendering says nothing about a collection: every
//! `ArrayBuf(i64)` in a program renders as `{ core: RawBuf(i64), len: 3 }`, so
//! two that differ produce the same text and the report claims an equality the
//! comparison did not find. The container rule of 6.7:15 renders those types by
//! their elements instead — `[1, 2, 3]` — and the count of a container whose
//! elements these rules cannot render, so no rendering is ever content-free.
//!
//! A container is recognized by canonical standard-library identity, never by
//! field shape: each of these types is an anonymous struct minted by a `pub fn
//! Name(comptime …) -> type` in one std module, so the identity gated on is the
//! producing type function's ([`rue_air::StdContainer`]), one level up from the
//! nominal provenance rule [`LangItem`] applies. A user struct holding a buffer
//! and a length is therefore not one.
//!
//! Once a container is recognized, its *parts* are located structurally, the
//! same way a `StrBuf`'s byte view is: `ArrayBuf(T)`'s length is its unique
//! `u64` field and its element base the unique raw pointer reachable through
//! its aggregate field. Every other sequence container wraps one `ArrayBuf` and
//! adds scalar bookkeeping, so it is "that `ArrayBuf`'s parts behind this
//! field" plus the named `u64`s its canonical shape declares. A shape that no
//! longer matches renders as its type name, exactly as a stale `StrBuf` does.
//!
//! Elements are read through `@ptr_offset` on that base — the pointee's stride
//! is codegen's own, so this module needs no layout knowledge — inside a loop
//! whose index and count are locals, one pair per nesting level.
//!
//! A container is also transparent to the one-level rule, so an element carries
//! a whole plan rather than a leaf: a struct element renders as `{ x: 1 }` and
//! an enum element as `Some(3)`, and it is *that* rendering's own aggregate
//! fields and payloads which fall to the type-name rule. A struct element is
//! copied into a local of its own first, one per nesting level, because a
//! narrow field read through the element pointer would load a whole word.
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

use crate::semantic_identity::semantic_type_from_instance;

type Ty = rue_air::SemanticImportType<crate::StableDefinitionKey, crate::ModuleId>;
type Body = rue_air::SemanticBody<crate::StableDefinitionKey, crate::ModuleId>;
type Inst = SemanticBodyInst<crate::StableDefinitionKey, crate::ModuleId>;
type Data = SemanticBodyInstData<crate::StableDefinitionKey, crate::ModuleId>;
type Place = SemanticBodyPlace<crate::StableDefinitionKey, crate::ModuleId>;
type Projection = SemanticBodyProjection<crate::StableDefinitionKey, crate::ModuleId>;

/// Rendered-payload budget in bytes (ADR-0083 §2 capture bounds).
///
/// This is [`rue_runtime_abi::RENDERING_BOUND`]: the runtime's test channel
/// bounds a failure record's `message` to the same constant, so a rendering
/// and the record carrying it agree by construction rather than by two
/// literals staying in step by hand.
pub(crate) const PAYLOAD_BUDGET: u64 = rue_runtime_abi::RENDERING_BOUND;

/// Appended when a rendering exceeded [`PAYLOAD_BUDGET`], in the spelling
/// [`rue_runtime_abi::RENDERING_TRUNCATION_MARKER`] carries for both writers.
pub(crate) const TRUNCATION_MARKER: &str = rue_runtime_abi::RENDERING_TRUNCATION_MARKER;

/// How one value renders once the walk has stopped descending.
///
/// Most of these are leaves: the payload rules render aggregates one level
/// deep, so a *user* aggregate reached below the top level renders as its type
/// name rather than recursively. A standard-library container is the exception
/// the container rule of 6.7:15 carves out — it is nominal, its length is
/// readable, and rendering it as its type name would give two unequal
/// containers the same text.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum LeafRender {
    /// Fixed text — the rendering of a value with no readable content
    /// (a raw pointer, a module, a unit).
    Literal(Arc<str>),
    /// The value's own type name, because these rules cannot look inside it.
    ///
    /// Rendered exactly like [`LeafRender::Literal`]; kept apart from it so a
    /// container can tell "an element the printer can render" from "an element
    /// it can only name", and fall back to a count for the latter.
    Opaque(Arc<str>),
    /// Decimal, with a leading `-` for a negative value.
    Signed,
    /// Decimal.
    Unsigned,
    /// `true` or `false`.
    Bool,
    /// The shortest round-trip decimal of an `f32`/`f64`, formatted by the one
    /// runtime helper `@to_string` and `@dbg` already share.
    Float,
    /// The value's own bytes, reached by these projections.
    Bytes(ByteView),
    /// The value's own bytes, double-quoted with `\` and `"` escaped.
    ///
    /// A container element takes this form: a verbatim element could contain
    /// the list's own `, ` separator and make two different lists render the
    /// same, which is exactly what a rendering must never do.
    QuotedBytes(ByteView),
    /// A standard-library container, by its elements or by its count.
    Container(Arc<ContainerRender>),
}

/// How a standard-library container renders (6.7:15).
///
/// Every path here is a chain of field projections from the container value.
/// They are located from the container's *canonical std identity* — the type
/// function that minted it — and then checked against the shape that identity
/// implies, so a std container whose internals change renders by its count
/// rather than through a projection that has gone stale.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct ContainerRender {
    /// The container's source-level spelling, for the measured form.
    pub(crate) name: Arc<str>,
    /// What one of its units is called in the measured form, singular then
    /// plural.
    pub(crate) noun: (Arc<str>, Arc<str>),
    /// Projections to the pointer addressing the backing store. Empty for a
    /// container that only ever renders its count.
    pub(crate) base: Arc<[PrinterProjection]>,
    /// Whether that pointer is `ptr mut T` rather than `ptr const T`.
    pub(crate) base_is_mut: bool,
    /// Projections to the backing store's own element count.
    pub(crate) length: Arc<[PrinterProjection]>,
    /// Projections to the index the first live element sits at.
    pub(crate) start: Option<Arc<[PrinterProjection]>>,
    /// Projections to the live element count, for a container that keeps one
    /// rather than running to the end of its backing store.
    pub(crate) count: Option<Arc<[PrinterProjection]>>,
    /// Projections to the modulus live indices wrap at.
    pub(crate) modulus: Option<Arc<[PrinterProjection]>>,
    /// Projections to the width the rendering breaks rows at.
    pub(crate) row: Option<Arc<[PrinterProjection]>>,
    /// The element type and how one element renders, or `None` when the
    /// printer renders the count instead of the elements.
    pub(crate) element: Option<ContainerElement>,
}

/// One rendered container element.
///
/// The element carries a whole plan, not a leaf: a container is transparent to
/// the one-level rule, so an element that is a struct or an enum renders as one
/// — `[{ x: 1 }]`, `[Some(3), None]` — and it is *that* rendering's fields and
/// payloads which fall to the type-name rule.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct ContainerElement {
    pub(crate) ty: crate::TypeInstanceKey,
    pub(crate) plan: Arc<ErrorPrinterPlan>,
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
        self.plan.retained_charge()
    }
}

impl crate::retained_charge::RetainedCharge for ErrorPrinterPlan {
    fn retained_charge(&self) -> u64 {
        match self {
            Self::Leaf(render) => render.retained_charge(),
            Self::Struct { fields } => fields.retained_charge(),
            Self::Enum { variants } => variants.retained_charge(),
        }
    }
}

impl crate::retained_charge::RetainedCharge for LeafRender {
    fn retained_charge(&self) -> u64 {
        match self {
            Self::Literal(text) | Self::Opaque(text) => text.retained_charge(),
            Self::Signed | Self::Unsigned | Self::Bool | Self::Float => 0,
            Self::Bytes(view) | Self::QuotedBytes(view) => {
                view.pointer
                    .len()
                    .saturating_add(view.length.len())
                    .saturating_mul(std::mem::size_of::<PrinterProjection>()) as u64
            }
            Self::Container(container) => container.retained_charge(),
        }
    }
}

impl crate::retained_charge::RetainedCharge for ContainerRender {
    fn retained_charge(&self) -> u64 {
        let path = |path: &Arc<[PrinterProjection]>| {
            (path.len() * std::mem::size_of::<PrinterProjection>()) as u64
        };
        let optional =
            |optional: &Option<Arc<[PrinterProjection]>>| optional.as_ref().map_or(0, path);
        self.name
            .retained_charge()
            .saturating_add(self.noun.0.retained_charge())
            .saturating_add(self.noun.1.retained_charge())
            .saturating_add(path(&self.base))
            .saturating_add(path(&self.length))
            .saturating_add(optional(&self.start))
            .saturating_add(optional(&self.count))
            .saturating_add(optional(&self.modulus))
            .saturating_add(optional(&self.row))
            .saturating_add(self.element.as_ref().map_or(0, |element| {
                element
                    .ty
                    .retained_charge()
                    .saturating_add(element.plan.retained_charge())
            }))
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
    /// The standard-library container a nominal is an instance of, when it is
    /// one. Keyed by the canonical std identity of the type function that
    /// minted the nominal, never by field shape, so a user struct that happens
    /// to hold a buffer and a length is not mistaken for one.
    fn std_container(&self, ty: &crate::TypeInstanceKey) -> Option<rue_air::StdContainer>;
    /// The source-level spelling used when a value renders as its type name.
    fn type_name(&self, ty: &crate::TypeInstanceKey) -> Arc<str>;
}

/// How deep the printer follows one standard container into another before it
/// renders a count instead of elements.
///
/// A container's element type is a strictly smaller type expression, so the
/// recursion terminates on its own; this is the belt-and-braces bound that
/// keeps a pathological nesting out of the rendering rather than the
/// termination argument.
const MAX_CONTAINER_DEPTH: u32 = 4;

/// Build the rendering plan for one error type.
///
/// Total: an error type the payload rules cannot render is not a failure, it is
/// a value that renders as its own type name.
pub(crate) fn plan_error_printer(
    owner: &crate::TypeInstanceKey,
    types: &impl ErrorPrinterTypes,
) -> ErrorPrinterFacts {
    // The top level always renders something: an error type these rules cannot
    // look inside is not a failure, it is a value that renders as its own type
    // name. Only an element inside a container treats that as "unrenderable"
    // and falls back to a count.
    let plan = value_plan(owner, types, 0)
        .unwrap_or_else(|| ErrorPrinterPlan::Leaf(LeafRender::Opaque(types.type_name(owner))));
    ErrorPrinterFacts { plan }
}

/// The rendering plan for one value, or `None` when these rules cannot look
/// inside it at all.
///
/// `depth` is the number of standard containers already entered, which bounds
/// the nesting and picks the loop slots the body will use.
fn value_plan(
    ty: &crate::TypeInstanceKey,
    types: &impl ErrorPrinterTypes,
    depth: u32,
) -> Option<ErrorPrinterPlan> {
    if depth > MAX_CONTAINER_DEPTH {
        return None;
    }
    if let Some(variants) = types.enum_variants(ty) {
        let variants = variants
            .into_iter()
            .map(|(name, payloads)| PrinterVariant {
                name,
                fields: payloads
                    .iter()
                    .map(|ty| {
                        let ty = crate::semantic_identity::type_instance_from_semantic(ty);
                        PrinterField {
                            name: Arc::from(""),
                            render: leaf_render_at(&ty, types, depth),
                            ty,
                        }
                    })
                    .collect::<Vec<_>>()
                    .into(),
            })
            .collect::<Vec<_>>();
        return Some(ErrorPrinterPlan::Enum {
            variants: variants.into(),
        });
    }
    // A byte string is a struct, and rendering its bytes beats rendering its
    // fields, so the leaf classification is consulted before the struct walk.
    if let Some(view) = byte_view(ty, types) {
        return Some(ErrorPrinterPlan::Leaf(LeafRender::Bytes(view)));
    }
    // A standard container is a struct too, and its `{ core: RawBuf(T), len: 3 }`
    // rendering says nothing about what it holds, so the container rule is
    // consulted ahead of the struct walk for the same reason.
    if let Some(render) = container_render(ty, types, depth) {
        return Some(ErrorPrinterPlan::Leaf(render));
    }
    if let Some(fields) = types.struct_fields(ty) {
        let fields = fields
            .into_iter()
            .map(|(name, ty)| {
                let ty = crate::semantic_identity::type_instance_from_semantic(&ty);
                PrinterField {
                    name,
                    render: leaf_render_at(&ty, types, depth),
                    ty,
                }
            })
            .collect::<Vec<_>>();
        return Some(ErrorPrinterPlan::Struct {
            fields: fields.into(),
        });
    }
    match leaf_render_at(ty, types, depth) {
        // A raw pointer, a module, an array: named, never read. Inside a
        // container that is a list of identical names, so the container
        // reports its count instead.
        LeafRender::Opaque(_) => None,
        render => Some(ErrorPrinterPlan::Leaf(render)),
    }
}

/// Classify one value that the walk will not descend into, `depth` standard
/// containers in.
fn leaf_render_at(
    ty: &crate::TypeInstanceKey,
    types: &impl ErrorPrinterTypes,
    depth: u32,
) -> LeafRender {
    use crate::TypeInstanceKey as T;
    match ty {
        T::I8 | T::I16 | T::I32 | T::I64 => LeafRender::Signed,
        T::U8 | T::U16 | T::U32 | T::U64 => LeafRender::Unsigned,
        T::Bool => LeafRender::Bool,
        T::F32 | T::F64 => LeafRender::Float,
        T::Unit => LeafRender::Literal(Arc::from("()")),
        _ => {
            if let Some(view) = byte_view(ty, types) {
                return LeafRender::Bytes(view);
            }
            if let Some(render) = container_render(ty, types, depth) {
                return render;
            }
            // One level deep, and no deeper: a user aggregate reached here is a
            // field of the error type, and rendering its own fields would be
            // the second level the payload rules stop at.
            LeafRender::Opaque(types.type_name(ty))
        }
    }
}

/// Plan the rendering of `ty` when it is a standard-library container.
fn container_render(
    ty: &crate::TypeInstanceKey,
    types: &impl ErrorPrinterTypes,
    depth: u32,
) -> Option<LeafRender> {
    let kind = types.std_container(ty)?;
    let name = types.type_name(ty);
    let (singular, plural) = kind.unit_noun();
    let noun = (Arc::<str>::from(singular), Arc::<str>::from(plural));
    let parts = container_parts(kind, ty, types)?;
    let element = match parts.element {
        // A map's entries live behind a private slot-state encoding this
        // module deliberately does not know, so it renders its entry count.
        None => None,
        // A container is transparent to the one-level rule, so its element is
        // planned as if it were the top level: a struct element renders as
        // `{ x: 1 }`, and it is that struct's own aggregate fields that fall to
        // the type-name rule. An element the rules cannot look into at all
        // leaves `None`, and the container renders its size.
        Some(element_ty) => value_plan(&element_ty, types, depth + 1)
            .map(quote_byte_elements)
            .map(|plan| ContainerElement {
                ty: element_ty,
                plan: Arc::new(plan),
            }),
    };
    Some(LeafRender::Container(Arc::new(ContainerRender {
        name,
        noun,
        base: parts.base.into(),
        base_is_mut: parts.base_is_mut,
        length: parts.length.into(),
        start: parts.start.map(Into::into),
        count: parts.count.map(Into::into),
        modulus: parts.modulus.map(Into::into),
        row: parts.row.map(Into::into),
        element,
    })))
}

/// A byte-string element renders quoted, never verbatim; see
/// [`LeafRender::QuotedBytes`]. Only a whole element takes that form — a byte
/// string reached inside an element struct is an ordinary field rendering.
fn quote_byte_elements(plan: ErrorPrinterPlan) -> ErrorPrinterPlan {
    match plan {
        ErrorPrinterPlan::Leaf(LeafRender::Bytes(view)) => {
            ErrorPrinterPlan::Leaf(LeafRender::QuotedBytes(view))
        }
        other => other,
    }
}

/// The projection paths one standard container's rendering reads.
struct ContainerParts {
    base: Vec<PrinterProjection>,
    base_is_mut: bool,
    length: Vec<PrinterProjection>,
    start: Option<Vec<PrinterProjection>>,
    count: Option<Vec<PrinterProjection>>,
    modulus: Option<Vec<PrinterProjection>>,
    row: Option<Vec<PrinterProjection>>,
    /// The element type, or `None` for a container that renders its count.
    element: Option<crate::TypeInstanceKey>,
}

/// Locate the parts of one standard container.
///
/// `ArrayBuf` is the one that owns storage: its length is its unique `u64`
/// field and its element base is the unique raw pointer reachable through its
/// aggregate field, both located structurally exactly as a `StrBuf`'s byte view
/// is. Every other sequence container wraps one `ArrayBuf` and adds scalar
/// bookkeeping, so it is located as "that `ArrayBuf`'s parts, behind this
/// field" plus the named `u64`s its own canonical shape declares. A lookup that
/// does not find the shape its identity implies yields `None`, and the value
/// falls back to the type-name rendering it has today.
fn container_parts(
    kind: rue_air::StdContainer,
    ty: &crate::TypeInstanceKey,
    types: &impl ErrorPrinterTypes,
) -> Option<ContainerParts> {
    use rue_air::StdContainer as C;
    let fields = types.struct_fields(ty)?;
    let nominal = type_nominal(ty)?;
    match kind {
        C::ArrayBuf => {
            let length = vec![PrinterProjection {
                nominal: nominal.clone(),
                field_index: unique_field(&fields, |ty| matches!(ty, Ty::U64))?,
            }];
            let (base, base_is_mut, element) =
                element_pointer_projections(ty, &nominal, &fields, types)?;
            Some(ContainerParts {
                base,
                base_is_mut,
                length,
                start: None,
                count: None,
                modulus: None,
                row: None,
                element: Some(element),
            })
        }
        // `StrMap`/`IntMap` are open-addressed tables whose live slots are
        // marked by a private state byte, so the printer reports the entry
        // count the table itself keeps rather than guessing at occupancy.
        C::StrMap | C::IntMap => Some(ContainerParts {
            base: Vec::new(),
            base_is_mut: false,
            length: vec![named_u64_field(&nominal, &fields, "count")?],
            start: None,
            count: None,
            modulus: None,
            row: None,
            element: None,
        }),
        C::Stack | C::Queue | C::BinaryHeap | C::Grid2D | C::Deque => {
            let storage = match kind {
                C::BinaryHeap => "data",
                C::Grid2D => "cells",
                _ => "buf",
            };
            let index = field_index(&fields, storage)?;
            let inner_ty =
                crate::semantic_identity::type_instance_from_semantic(&fields[index as usize].1);
            if types.std_container(&inner_ty)? != C::ArrayBuf {
                return None;
            }
            let inner = container_parts(C::ArrayBuf, &inner_ty, types)?;
            let step = PrinterProjection {
                nominal: nominal.clone(),
                field_index: index,
            };
            let behind = |path: Vec<PrinterProjection>| {
                std::iter::once(step.clone())
                    .chain(path)
                    .collect::<Vec<_>>()
            };
            let (start, count, modulus, row) = match kind {
                C::Stack | C::BinaryHeap => (None, None, None, None),
                // A queue never moves its front: it renders the suffix of its
                // backing buffer that begins at `head`.
                C::Queue => (
                    Some(vec![named_u64_field(&nominal, &fields, "head")?]),
                    None,
                    None,
                    None,
                ),
                // A deque is a ring: `count` live elements from `head`, taken
                // modulo the capacity its backing buffer was grown to.
                C::Deque => (
                    Some(vec![named_u64_field(&nominal, &fields, "head")?]),
                    Some(vec![named_u64_field(&nominal, &fields, "count")?]),
                    Some(vec![named_u64_field(&nominal, &fields, "cap")?]),
                    None,
                ),
                // A grid stores its cells row-major, so its rendering breaks
                // every `cols` elements.
                C::Grid2D => (
                    None,
                    None,
                    None,
                    Some(vec![named_u64_field(&nominal, &fields, "cols")?]),
                ),
                C::ArrayBuf | C::StrMap | C::IntMap => unreachable!("handled above"),
            };
            Some(ContainerParts {
                base: behind(inner.base),
                base_is_mut: inner.base_is_mut,
                length: behind(inner.length),
                start,
                count,
                modulus,
                row,
                element: inner.element,
            })
        }
    }
}

/// The index of the field spelled `name`.
fn field_index(fields: &[(Arc<str>, Ty)], name: &str) -> Option<u32> {
    let index = fields
        .iter()
        .position(|(field, _)| field.as_ref() == name)?;
    u32::try_from(index).ok()
}

/// A projection to the `u64` field spelled `name`.
fn named_u64_field(
    nominal: &crate::NominalInstanceKey,
    fields: &[(Arc<str>, Ty)],
    name: &str,
) -> Option<PrinterProjection> {
    let index = field_index(fields, name)?;
    matches!(fields[index as usize].1, Ty::U64).then(|| PrinterProjection {
        nominal: nominal.clone(),
        field_index: index,
    })
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
        && rue_air::is_string_view_struct_name(name)
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
    let (projections, pointer) = pointer_projections(ty, nominal, fields, types, is_byte_pointer)?;
    Some((projections, is_mut_byte_pointer(&pointer)))
}

/// The unique element pointer of a standard container, and what it addresses.
///
/// `ArrayBuf(T)` keeps its allocation in the shared growable-buffer core, so
/// this is the same one-field-inside-one-field walk a `StrBuf`'s byte view
/// takes; the difference is only which pointees qualify.
fn element_pointer_projections(
    ty: &crate::TypeInstanceKey,
    nominal: &crate::NominalInstanceKey,
    fields: &[(Arc<str>, Ty)],
    types: &impl ErrorPrinterTypes,
) -> Option<(Vec<PrinterProjection>, bool, crate::TypeInstanceKey)> {
    let (projections, pointer) = pointer_projections(ty, nominal, fields, types, is_any_pointer)?;
    let (pointee, is_mut) = match &pointer {
        Ty::PtrMut(pointee) => (pointee, true),
        Ty::PtrConst(pointee) => (pointee, false),
        _ => return None,
    };
    let element = crate::semantic_identity::type_instance_from_semantic(pointee);
    Some((projections, is_mut, element))
}

/// The one field of `ty` matching `predicate`, or the one inside its one
/// aggregate field, together with that field's type.
fn pointer_projections(
    ty: &crate::TypeInstanceKey,
    nominal: &crate::NominalInstanceKey,
    fields: &[(Arc<str>, Ty)],
    types: &impl ErrorPrinterTypes,
    predicate: fn(&Ty) -> bool,
) -> Option<(Vec<PrinterProjection>, Ty)> {
    if let Some(index) = unique_field(fields, predicate) {
        return Some((
            vec![PrinterProjection {
                nominal: nominal.clone(),
                field_index: index,
            }],
            fields[index as usize].1.clone(),
        ));
    }
    let inner_index = unique_field(fields, |field| {
        matches!(field, Ty::Nominal(_) | Ty::AnonymousNominal(_))
    })?;
    let inner_ty =
        crate::semantic_identity::type_instance_from_semantic(&fields[inner_index as usize].1);
    // Guard against a self-referential shape rather than recursing forever.
    if inner_ty == *ty {
        return None;
    }
    let inner_nominal = type_nominal(&inner_ty)?;
    let inner_fields = types.struct_fields(&inner_ty)?;
    let index = unique_field(&inner_fields, predicate)?;
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
        inner_fields[index as usize].1.clone(),
    ))
}

fn is_mut_byte_pointer(ty: &Ty) -> bool {
    matches!(ty, Ty::PtrMut(pointee) if matches!(**pointee, Ty::U8))
}

fn is_byte_pointer(ty: &Ty) -> bool {
    matches!(ty, Ty::PtrConst(pointee) | Ty::PtrMut(pointee) if matches!(**pointee, Ty::U8))
}

fn is_any_pointer(ty: &Ty) -> bool {
    matches!(ty, Ty::PtrConst(_) | Ty::PtrMut(_))
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
    fn walk(
        plan: &ErrorPrinterPlan,
        types: &mut std::collections::BTreeSet<crate::TypeInstanceKey>,
    ) {
        let leaf =
            |render: &LeafRender,
             types: &mut std::collections::BTreeSet<crate::TypeInstanceKey>| {
                if let LeafRender::Container(container) = render
                    && let Some(element) = container.element.as_ref()
                {
                    types.insert(element.ty.clone());
                    walk(&element.plan, types);
                }
            };
        match plan {
            ErrorPrinterPlan::Leaf(render) => leaf(render, types),
            ErrorPrinterPlan::Struct { fields } => {
                for field in fields.iter() {
                    types.insert(field.ty.clone());
                    leaf(&field.render, types);
                }
            }
            ErrorPrinterPlan::Enum { variants } => {
                for variant in variants.iter() {
                    for field in variant.fields.iter() {
                        types.insert(field.ty.clone());
                        leaf(&field.render, types);
                    }
                }
            }
        }
    }
    let mut types = std::collections::BTreeSet::new();
    types.insert(owner.clone());
    walk(&facts.plan, &mut types);
    types
}

/// How many standard containers this plan can be inside at once.
///
/// One rendering never enters two containers at the same nesting level, so this
/// is the number of loop index/count pairs the body needs, not the number of
/// containers it mentions.
fn container_depth(plan: &ErrorPrinterPlan) -> u32 {
    fn leaf(render: &LeafRender) -> u32 {
        match render {
            LeafRender::Container(container) => container
                .element
                .as_ref()
                .map_or(0, |element| container_depth(&element.plan))
                .saturating_add(1),
            LeafRender::Literal(_)
            | LeafRender::Opaque(_)
            | LeafRender::Signed
            | LeafRender::Unsigned
            | LeafRender::Bool
            | LeafRender::Float
            | LeafRender::Bytes(_)
            | LeafRender::QuotedBytes(_) => 0,
        }
    }
    match plan {
        ErrorPrinterPlan::Leaf(render) => Some(leaf(render)),
        ErrorPrinterPlan::Struct { fields } => fields.iter().map(|field| leaf(&field.render)).max(),
        ErrorPrinterPlan::Enum { variants } => variants
            .iter()
            .flat_map(|variant| variant.fields.iter())
            .map(|field| leaf(&field.render))
            .max(),
    }
    .unwrap_or(0)
}

/// The width of the local each nesting level stages a *struct* element in,
/// indexed by the depth that element is rendered at.
///
/// A struct element's fields are read as ordinary storage rather than through
/// the element pointer, so the element is copied into a local first; see
/// [`Builder::stage_struct_source`]. Only struct elements need one, so most
/// depths have a width of zero.
fn element_staging_widths(
    plan: &ErrorPrinterPlan,
    depths: u32,
    width: &dyn Fn(&crate::TypeInstanceKey) -> Result<u32, Arc<str>>,
) -> Result<Vec<u32>, Arc<str>> {
    fn walk(
        plan: &ErrorPrinterPlan,
        depth: u32,
        widths: &mut [u32],
        width: &dyn Fn(&crate::TypeInstanceKey) -> Result<u32, Arc<str>>,
    ) -> Result<(), Arc<str>> {
        let leaf = |render: &LeafRender, widths: &mut [u32]| -> Result<(), Arc<str>> {
            let LeafRender::Container(container) = render else {
                return Ok(());
            };
            let Some(element) = container.element.as_ref() else {
                return Ok(());
            };
            let inner = depth.saturating_add(1);
            if matches!(*element.plan, ErrorPrinterPlan::Struct { .. })
                && let Some(slot) = widths.get_mut(inner as usize)
            {
                *slot = (*slot).max(width(&element.ty)?);
            }
            walk(&element.plan, inner, widths, width)
        };
        match plan {
            ErrorPrinterPlan::Leaf(render) => leaf(render, widths)?,
            ErrorPrinterPlan::Struct { fields } => {
                for field in fields.iter() {
                    leaf(&field.render, widths)?;
                }
            }
            ErrorPrinterPlan::Enum { variants } => {
                for variant in variants.iter() {
                    for field in variant.fields.iter() {
                        leaf(&field.render, widths)?;
                    }
                }
            }
        }
        Ok(())
    }
    let mut widths = vec![0; depths.saturating_add(1) as usize];
    walk(plan, 0, &mut widths, width)?;
    Ok(widths)
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
    /// single-word fixed slots and the base reserves both.
    pub(super) const TEXT: u32 = 11;
    /// Address of the byte string a quoted element is escaping, held as an
    /// integer so one slot serves a `ptr const u8` and a `ptr mut u8` source.
    pub(super) const QUOTED: u32 = TEXT + 2;
    /// Position of the byte being escaped.
    pub(super) const QUOTED_CURSOR: u32 = QUOTED + 1;
    /// How many bytes that string has.
    pub(super) const QUOTED_LEN: u32 = QUOTED + 2;
    /// First slot of the container loops: two per nesting level, the index of
    /// the element being rendered and the number of elements to render.
    pub(super) const SEQUENCE: u32 = QUOTED + 3;
    /// Slots one container-nesting level occupies.
    pub(super) const SEQUENCE_STRIDE: u32 = 2;
    /// Slots the float formatter's result occupies: the runtime writes a
    /// `{ptr, cap, len}` triple through its out-pointer.
    pub(super) const FLOAT_STRIDE: u32 = 3;

    /// The index slot of the container loop at `depth`.
    pub(super) const fn sequence_index(depth: u32) -> u32 {
        SEQUENCE + depth * SEQUENCE_STRIDE
    }

    /// The element-count slot of the container loop at `depth`.
    pub(super) const fn sequence_count(depth: u32) -> u32 {
        sequence_index(depth) + 1
    }

    /// The text a float formatted to, as the runtime handed it back.
    pub(super) const fn float(depths: u32) -> u32 {
        SEQUENCE + depths * SEQUENCE_STRIDE
    }

    /// First slot of the region each nesting level stages a struct element in.
    pub(super) const fn elements(depths: u32) -> u32 {
        float(depths) + FLOAT_STRIDE
    }
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
    // Every container loop the plan can enter at once gets its own index and
    // count, so a nested rendering never reuses the level above it.
    let depths = container_depth(&facts.plan);
    // Each nesting level that renders a struct element copies it into a local
    // of its own; the widths are known only here, where layout is.
    let element_widths = element_staging_widths(&facts.plan, depths, &width)?;
    let mut element_slots = Vec::with_capacity(element_widths.len());
    let mut next = slot::elements(depths);
    for element_width in element_widths {
        element_slots.push((next, element_width));
        next = next.saturating_add(element_width);
    }
    let builder_payload_slot = next;
    let mut builder = Builder {
        float_slot: slot::float(depths),
        element_slots,
        payload_slot: builder_payload_slot,
        ..Builder::default()
    };
    let mut statements = builder.prologue();
    // The error value is one parameter, not one per slot, and every read
    // below goes through a place rooted at that one parameter, so the address
    // arithmetic is the compiler's own rather than a second copy of it.
    // Reading raw parameter slots would need this body to restate the
    // parameter area's slot order, which is exactly the knowledge place
    // lowering already owns.
    //
    // Those places are also what tells CFG construction the parameter's
    // shape: each carries the owner as its base type, and the parameter-area
    // descriptor is grouped from that (RUE-1943). No whole-value `Param`
    // instruction is declared for it. Evaluating one would *consume* an error
    // type that owns anything, making the reads that follow reads after a
    // move, and this body deliberately has no drop schedule to name the
    // parameter instead (6.7:16).

    // One payload binding serves every byte-string payload, one variant at a
    // time; it is as wide as the widest such payload and absent when no variant
    // carries one.
    let mut payload_slots = 0;
    if let ErrorPrinterPlan::Enum { variants } = &facts.plan {
        for variant in variants.iter() {
            for field in variant.fields.iter() {
                if matches!(
                    field.render,
                    LeafRender::Bytes(_) | LeafRender::QuotedBytes(_) | LeafRender::Container(_)
                ) {
                    payload_slots = payload_slots.max(width(&field.ty)?);
                }
            }
        }
    }

    builder.render_plan(
        &mut statements,
        &facts.plan,
        owner,
        &LeafSource::Projected(Vec::new()),
        &owner_ty,
        0,
    )?;

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
        // A payload binding aliases the parameter's own storage, and a staged
        // element is a bitwise copy of storage the container still owns;
        // neither takes ownership. This body has no drop schedule at all, so
        // declaring them non-owning states that rather than establishing it.
        borrow_slots: builder
            .element_slots
            .iter()
            .filter(|(_, width)| *width > 0)
            .map(|(slot, _)| *slot)
            .chain((payload_slots > 0).then_some(builder_payload_slot))
            .collect::<Vec<_>>()
            .into(),
        num_locals: builder_payload_slot.saturating_add(payload_slots),
        num_param_slots,
        cleanup_owner: None,
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

/// The element type a container renders, as the place reads see it.
fn element_type(view: &ContainerRender) -> Ty {
    semantic_type_from_instance(
        &view
            .element
            .as_ref()
            .expect("an element source renders elements")
            .ty,
    )
}

/// The runtime's `{ptr, cap, len}` formatting result, as three machine words.
///
/// The helper writes that triple through an out-pointer whose shape the runtime
/// ABI owns ([`rue_runtime_abi::AggregateShapeId::StrBufResult`]). Receiving it
/// as `[u64; 3]` rather than as `StrBuf` is what keeps this body free of any
/// standard-library declaration, which it must be: the printer is demanded by a
/// `?` or an assertion, not by a call the request's closure walked.
fn float_text_type() -> Ty {
    Ty::Array {
        element: Arc::new(Ty::U64),
        len: FLOAT_TEXT_WORDS,
    }
}

/// Words of [`float_text_type`], and the two this module reads.
const FLOAT_TEXT_WORDS: u64 = 3;
const FLOAT_TEXT_POINTER: u64 = 0;
const FLOAT_TEXT_LEN: u64 = 2;

/// The exact pointer type a byte view's pointer field has. A place read is
/// typed by the field it lands on, so the two spellings are not interchangeable.
fn byte_view_pointer_type(view: &ByteView) -> Ty {
    if view.pointer_is_mut {
        byte_pointer_type()
    } else {
        Ty::PtrConst(Arc::new(Ty::U8))
    }
}

/// The base a printer place is rooted at.
enum PlaceRoot {
    /// The one parameter.
    Param,
    /// A local the printer bound a payload to.
    Local(u32),
    /// The value one pointer addresses. Used for a container element, whose
    /// address is recomputed from the container and the loop index at each
    /// use, so no value crosses a branch boundary.
    Indirect(u32),
}

/// One element of a container being rendered.
#[derive(Clone)]
struct ElementSource {
    /// How to reach the container the element belongs to.
    container: Box<LeafSource>,
    view: Arc<ContainerRender>,
    /// The loop index slot naming which element is being rendered.
    index_slot: u32,
    /// Field steps taken from the element itself, for an element whose own
    /// rendering is a struct.
    prefix: Vec<PrinterProjection>,
}

/// Where one rendered leaf lives inside the printer's single parameter.
#[derive(Clone)]
enum LeafSource {
    /// Reached from the parameter by field projections: the parameter itself
    /// when the list is empty, one of its fields otherwise.
    Projected(Vec<PrinterProjection>),
    /// One payload of the variant an arm matched, projected out of the enum
    /// value `base`. A payload is a value rather than a place, so a byte view
    /// binds it to the payload slot first.
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
    /// One element of a standard container.
    Element(Box<ElementSource>),
    /// A local this body bound a value to, so that a rendering needing a place
    /// has one, optionally reached through a field prefix.
    Local {
        slot: u32,
        /// The type the local holds, which is what a place rooted there is
        /// typed by — never the type of the leaf a projection ends on.
        root_ty: Ty,
        prefix: Vec<PrinterProjection>,
    },
}

impl LeafSource {
    /// The same value's field, one step further in.
    fn with_field(&self, step: PrinterProjection) -> Self {
        let extend = |prefix: &Vec<PrinterProjection>| {
            let mut prefix = prefix.clone();
            prefix.push(step.clone());
            prefix
        };
        match self {
            Self::Projected(prefix) => Self::Projected(extend(prefix)),
            Self::Local {
                slot,
                root_ty,
                prefix,
            } => Self::Local {
                slot: *slot,
                root_ty: root_ty.clone(),
                prefix: extend(prefix),
            },
            Self::Element(element) => {
                let mut element = element.clone();
                element.prefix = extend(&element.prefix);
                Self::Element(element)
            }
            // A payload is a value, not a place; every caller that projects
            // stages it into a local first.
            Self::Payload { .. } => {
                unreachable!("a payload is staged into a local before it is projected")
            }
        }
    }
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
    /// First slot of the float formatter's result; see [`slot::float`].
    float_slot: u32,
    /// First slot of the local each nesting level stages a struct element in,
    /// by depth; zero-width where that depth stages nothing.
    element_slots: Vec<(u32, u32)>,
    /// First slot of the payload binding; see [`slot::payload`].
    payload_slot: u32,
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
        // Every container loop's index and count is declared here rather than
        // where the loop is rendered: a nested container's loop sits inside its
        // parent's body, and a slot introduced there would be introduced once
        // per iteration.
        for slot in slot::QUOTED..self.float_slot {
            let zero = self.constant(0, Ty::U64);
            statements.push(self.bind(slot, zero, Ty::U64));
        }
        let empty = (0..slot::FLOAT_STRIDE)
            .map(|_| self.constant(0, Ty::U64))
            .collect::<Vec<_>>();
        let empty = self.add(
            Data::ArrayInit {
                elements: empty.into(),
                shape: rue_air::ArrayInitShape::Elementwise,
            },
            float_text_type(),
        );
        let slot = self.float_slot;
        statements.push(self.bind(slot, empty, float_text_type()));
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

    /// Emit the rendering of one whole value.
    ///
    /// The same three shapes serve the error type itself and every container
    /// element, because a container is transparent to the one-level rule: an
    /// element struct renders as `{ x: 1 }` at the level a top-level struct
    /// would, and it is that struct's own aggregate fields that fall to the
    /// type-name rule.
    fn render_plan(
        &mut self,
        statements: &mut Vec<u32>,
        plan: &ErrorPrinterPlan,
        value_key: &crate::TypeInstanceKey,
        source: &LeafSource,
        owner_ty: &Ty,
        depth: u32,
    ) -> Result<(), Arc<str>> {
        let value_ty = semantic_type_from_instance(value_key);
        match plan {
            ErrorPrinterPlan::Leaf(render) => {
                self.render_leaf(statements, render, source, owner_ty, &value_ty, depth)?;
            }
            ErrorPrinterPlan::Struct { fields } => {
                let nominal = type_nominal(value_key)
                    .ok_or_else(|| Arc::<str>::from("a struct rendering names a nominal"))?;
                let source =
                    self.stage_struct_source(statements, source, owner_ty, &value_ty, depth);
                self.append_literal(statements, "{");
                for (index, field) in fields.iter().enumerate() {
                    self.append_literal(statements, if index == 0 { " " } else { ", " });
                    self.append_literal(statements, &format!("{}: ", field.name));
                    let field_source = source.with_field(PrinterProjection {
                        nominal: nominal.clone(),
                        field_index: u32::try_from(index).unwrap_or(u32::MAX),
                    });
                    let field_ty = semantic_type_from_instance(&field.ty);
                    self.render_leaf(
                        statements,
                        &field.render,
                        &field_source,
                        owner_ty,
                        &field_ty,
                        depth,
                    )?;
                }
                self.append_literal(statements, if fields.is_empty() { "}" } else { " }" });
            }
            ErrorPrinterPlan::Enum { variants } => {
                let nominal = type_nominal(value_key)
                    .ok_or_else(|| Arc::<str>::from("an enum rendering names a nominal"))?;
                // Matching the value itself, rather than an integer read out of
                // a parameter slot, leaves the discriminant's position and
                // width to the same lowering an ordinary `match` uses.
                let scrutinee = self.read_leaf(source, owner_ty, &value_ty);
                let mut arms = Vec::with_capacity(variants.len());
                for (index, variant) in variants.iter().enumerate() {
                    let variant_index = u32::try_from(index).unwrap_or(u32::MAX);
                    let mut arm = Vec::new();
                    self.append_literal(&mut arm, &variant.name);
                    if !variant.fields.is_empty() {
                        self.append_literal(&mut arm, "(");
                        for (position, field) in variant.fields.iter().enumerate() {
                            if position > 0 {
                                self.append_literal(&mut arm, ", ");
                            }
                            let payload = LeafSource::Payload {
                                base: scrutinee,
                                enum_key: nominal.clone(),
                                variant_index,
                                field_index: u32::try_from(position).unwrap_or(u32::MAX),
                            };
                            let field_ty = semantic_type_from_instance(&field.ty);
                            self.render_leaf(
                                &mut arm,
                                &field.render,
                                &payload,
                                owner_ty,
                                &field_ty,
                                depth,
                            )?;
                        }
                        self.append_literal(&mut arm, ")");
                    }
                    let unit = self.add(Data::UnitConst, Ty::Unit);
                    let body = self.add(
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
                statements.push(self.add(
                    Data::Match {
                        scrutinee,
                        arms: arms.into(),
                    },
                    Ty::Unit,
                ));
            }
        }
        Ok(())
    }

    /// Emit the rendering of one leaf.
    fn render_leaf(
        &mut self,
        statements: &mut Vec<u32>,
        render: &LeafRender,
        source: &LeafSource,
        owner_ty: &Ty,
        leaf_ty: &Ty,
        depth: u32,
    ) -> Result<(), Arc<str>> {
        match render {
            LeafRender::Literal(text) | LeafRender::Opaque(text) => {
                self.append_literal(statements, text)
            }
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
            LeafRender::Float => {
                self.append_float(statements, source, owner_ty, leaf_ty);
            }
            LeafRender::Bytes(view) => {
                let source = self.stage_place_source(statements, source, owner_ty, leaf_ty);
                let pointer_ty = byte_view_pointer_type(view);
                let length = self.read_through(&source, owner_ty, &view.length.clone(), Ty::U64);
                let view = view.clone();
                let owner_ty = owner_ty.clone();
                self.append_bytes(statements, length, move |builder| {
                    builder.read_through(&source, &owner_ty, &view.pointer, pointer_ty)
                });
            }
            LeafRender::QuotedBytes(view) => {
                let source = self.stage_place_source(statements, source, owner_ty, leaf_ty);
                self.append_quoted(statements, view, &source, owner_ty);
            }
            LeafRender::Container(container) => {
                let source = self.stage_place_source(statements, source, owner_ty, leaf_ty);
                self.render_container(statements, container, &source, owner_ty, depth)?;
            }
        }
        Ok(())
    }

    /// Give `source` a place to be read through, when it is not one already.
    ///
    /// A projected leaf and a container element already name storage. An enum
    /// payload is a value, so it is bound to the payload slot first. That
    /// binding is declared non-owning (`borrow_slots`): the error value the
    /// payload came from is the parameter, which this body never destroys, so a
    /// second owner here would only add a destructor call to a path that traps
    /// before it could matter — and one this request never rooted.
    fn stage_place_source(
        &mut self,
        statements: &mut Vec<u32>,
        source: &LeafSource,
        owner_ty: &Ty,
        leaf_ty: &Ty,
    ) -> LeafSource {
        match source {
            LeafSource::Projected(_) | LeafSource::Element(_) | LeafSource::Local { .. } => {
                source.clone()
            }
            LeafSource::Payload { .. } => {
                let value = self.read_leaf(source, owner_ty, leaf_ty);
                let slot = self.payload_slot;
                statements.push(self.bind(slot, value, leaf_ty.clone()));
                LeafSource::Local {
                    slot,
                    root_ty: leaf_ty.clone(),
                    prefix: Vec::new(),
                }
            }
        }
    }

    /// Stage `source` so a struct's fields can be read as ordinary storage.
    ///
    /// A container element is addressed through a raw pointer, and an indirect
    /// place read of a narrower-than-word field loads a whole word today
    /// (`ArrayBuf(u8).get_ref(0)` has the same defect), so an element whose
    /// rendering is a struct is copied into a local of its own first and read
    /// from there. One local per nesting level: a nested container rebinds its
    /// own, leaving the level above it intact.
    fn stage_struct_source(
        &mut self,
        statements: &mut Vec<u32>,
        source: &LeafSource,
        owner_ty: &Ty,
        value_ty: &Ty,
        depth: u32,
    ) -> LeafSource {
        let LeafSource::Element(_) = source else {
            return self.stage_place_source(statements, source, owner_ty, value_ty);
        };
        let value = self.read_leaf(source, owner_ty, value_ty);
        let (slot, _) = self.element_slots[depth as usize];
        statements.push(self.bind(slot, value, value_ty.clone()));
        LeafSource::Local {
            slot,
            root_ty: value_ty.clone(),
            prefix: Vec::new(),
        }
    }

    /// Read `projections` out of the value `source` names.
    ///
    /// The root is resolved afresh on every call, so a read emitted inside a
    /// branch or a loop body carries its own address arithmetic instead of
    /// naming a value computed outside it.
    fn read_through(
        &mut self,
        source: &LeafSource,
        owner_ty: &Ty,
        projections: &[PrinterProjection],
        ty: Ty,
    ) -> u32 {
        let (root, prefix, base_type) = match source {
            LeafSource::Projected(prefix) => (PlaceRoot::Param, prefix.clone(), owner_ty.clone()),
            LeafSource::Local {
                slot,
                root_ty,
                prefix,
            } => (PlaceRoot::Local(*slot), prefix.clone(), root_ty.clone()),
            LeafSource::Payload { .. } => {
                unreachable!("a payload is staged into a local before it is read through")
            }
            LeafSource::Element(element) => {
                let pointer = self.element_pointer(element, owner_ty);
                if element.prefix.is_empty() && projections.is_empty() {
                    // A whole element is read through the pointer intrinsic
                    // rather than an indirect place: an indirect place read of
                    // a narrower-than-word scalar loads a whole word today —
                    // `ArrayBuf(u8).get_ref(0)` has the same defect — and
                    // `@ptr_read` is the path the standard library's own
                    // element reads already take.
                    return self.add(
                        Data::Intrinsic {
                            operation: IntrinsicOperation::PtrRead,
                            name: Arc::from(IntrinsicOperation::PtrRead.expected_spelling()),
                            args: Arc::new([argument(pointer)]),
                        },
                        ty,
                    );
                }
                (
                    PlaceRoot::Indirect(pointer),
                    element.prefix.clone(),
                    element_type(&element.view),
                )
            }
        };
        let steps = prefix
            .iter()
            .chain(projections)
            .cloned()
            .collect::<Vec<_>>();
        self.rooted_read(&root, &base_type, &steps, ty)
    }

    /// The address of the container element `source` names.
    fn element_pointer(&mut self, source: &ElementSource, owner_ty: &Ty) -> u32 {
        let view = source.view.clone();
        let element_ty = element_type(&view);
        let pointer_ty = if view.base_is_mut {
            Ty::PtrMut(Arc::new(element_ty))
        } else {
            Ty::PtrConst(Arc::new(element_ty))
        };
        let base = self.read_through(&source.container, owner_ty, &view.base, pointer_ty.clone());
        let mut index = self.load(source.index_slot, Ty::U64);
        if let Some(start) = view.start.as_ref() {
            let start = self.read_through(&source.container, owner_ty, start, Ty::U64);
            index = self.add(Data::WrappingAdd(start, index), Ty::U64);
        }
        if let Some(modulus) = view.modulus.as_ref() {
            // A zero modulus would trap the division, so it is replaced by one
            // rather than guarded: a ring with no capacity has no elements, and
            // this loop does not run.
            let read = self.read_through(&source.container, owner_ty, modulus, Ty::U64);
            let zero = self.constant(0, Ty::U64);
            let usable = self.add(Data::Ne(read, zero), Ty::Bool);
            let read = self.read_through(&source.container, owner_ty, modulus, Ty::U64);
            let one = self.constant(1, Ty::U64);
            let modulus = self.add(
                Data::Branch {
                    cond: usable,
                    then_value: read,
                    else_value: Some(one),
                },
                Ty::U64,
            );
            index = self.add(Data::Mod(index, modulus), Ty::U64);
        }
        self.add(
            Data::Intrinsic {
                operation: IntrinsicOperation::PtrOffset,
                name: Arc::from(IntrinsicOperation::PtrOffset.expected_spelling()),
                args: Arc::new([argument(base), argument(index)]),
            },
            pointer_ty,
        )
    }

    /// How many elements a container renders: the count it keeps, or what is
    /// left of its backing store after its first live index.
    fn element_count(&mut self, view: &ContainerRender, source: &LeafSource, owner_ty: &Ty) -> u32 {
        if let Some(count) = view.count.as_ref() {
            return self.read_through(source, owner_ty, count, Ty::U64);
        }
        let length = self.read_through(source, owner_ty, &view.length, Ty::U64);
        let Some(start) = view.start.as_ref() else {
            return length;
        };
        let begin = self.read_through(source, owner_ty, start, Ty::U64);
        let live = self.add(Data::Ge(length, begin), Ty::Bool);
        let length = self.read_through(source, owner_ty, &view.length, Ty::U64);
        let begin = self.read_through(source, owner_ty, start, Ty::U64);
        let span = self.add(Data::WrappingSub(length, begin), Ty::U64);
        let empty = self.constant(0, Ty::U64);
        self.add(
            Data::Branch {
                cond: live,
                then_value: span,
                else_value: Some(empty),
            },
            Ty::U64,
        )
    }

    /// Render a standard container: `[a, b, c]`, `[[a, b], [c, d]]` for a
    /// grid, or `Name <n units>` when its elements are values these rules
    /// cannot look inside (6.7:15).
    fn render_container(
        &mut self,
        statements: &mut Vec<u32>,
        view: &Arc<ContainerRender>,
        source: &LeafSource,
        owner_ty: &Ty,
        depth: u32,
    ) -> Result<(), Arc<str>> {
        let Some(element) = view.element.as_ref() else {
            self.append_literal(statements, &format!("{} <", view.name));
            let count = self.element_count(view, source, owner_ty);
            statements.push(self.store(slot::VALUE, count));
            // The decimal renderer reads the magnitude without consuming it,
            // so the count is still there to pick the noun's number.
            self.append_decimal(statements);
            let rendered = self.load(slot::VALUE, Ty::U64);
            let one = self.constant(1, Ty::U64);
            let single = self.add(Data::Eq(rendered, one), Ty::Bool);
            let mut singular = Vec::new();
            self.append_literal(&mut singular, &format!(" {}>", view.noun.0));
            let mut plural = Vec::new();
            self.append_literal(&mut plural, &format!(" {}>", view.noun.1));
            let branch = self.choose(single, singular, plural);
            statements.push(branch);
            return Ok(());
        };
        let index_slot = slot::sequence_index(depth);
        let count_slot = slot::sequence_count(depth);
        let zero = self.constant(0, Ty::U64);
        statements.push(self.store(index_slot, zero));
        let count = self.element_count(view, source, owner_ty);
        statements.push(self.store(count_slot, count));
        self.append_literal(statements, "[");

        // The loop stops as soon as the budget is spent: a container with a
        // million elements would otherwise keep iterating over appends that
        // copy nothing.
        let index = self.load(index_slot, Ty::U64);
        let count = self.load(count_slot, Ty::U64);
        let remaining = self.add(Data::Lt(index, count), Ty::Bool);
        let overflowed = self.load(slot::OVERFLOWED, Ty::U64);
        let zero = self.constant(0, Ty::U64);
        let room = self.add(Data::Eq(overflowed, zero), Ty::Bool);
        let condition = self.add(Data::And(remaining, room), Ty::Bool);

        let mut body = Vec::new();
        let index = self.load(index_slot, Ty::U64);
        let zero = self.constant(0, Ty::U64);
        let later = self.add(Data::Gt(index, zero), Ty::Bool);
        let mut separator = Vec::new();
        self.append_literal(&mut separator, ", ");
        body.push(self.guard(later, separator));
        if view.row.is_some() {
            let starts = self.row_boundary(view, source, owner_ty, index_slot);
            let mut open = Vec::new();
            self.append_literal(&mut open, "[");
            body.push(self.guard(starts, open));
        }
        let element_source = LeafSource::Element(Box::new(ElementSource {
            container: Box::new(source.clone()),
            view: view.clone(),
            index_slot,
            prefix: Vec::new(),
        }));
        self.render_plan(
            &mut body,
            &element.plan,
            &element.ty,
            &element_source,
            owner_ty,
            depth + 1,
        )?;
        let index = self.load(index_slot, Ty::U64);
        let one = self.constant(1, Ty::U64);
        let next = self.add(Data::Add(index, one), Ty::U64);
        body.push(self.store(index_slot, next));
        if view.row.is_some() {
            let ends = self.row_boundary(view, source, owner_ty, index_slot);
            let mut close = Vec::new();
            self.append_literal(&mut close, "]");
            body.push(self.guard(ends, close));
        }
        let body = self.block(body);
        statements.push(self.add(
            Data::Loop {
                cond: condition,
                body,
            },
            Ty::Unit,
        ));
        self.append_literal(statements, "]");
        Ok(())
    }

    /// Whether the loop index sits on a row boundary — `index % cols == 0`.
    fn row_boundary(
        &mut self,
        view: &ContainerRender,
        source: &LeafSource,
        owner_ty: &Ty,
        index_slot: u32,
    ) -> u32 {
        let row = view.row.as_ref().expect("a row boundary needs a row width");
        let read = self.read_through(source, owner_ty, row, Ty::U64);
        let zero = self.constant(0, Ty::U64);
        let usable = self.add(Data::Ne(read, zero), Ty::Bool);
        let read = self.read_through(source, owner_ty, row, Ty::U64);
        let one = self.constant(1, Ty::U64);
        // A zero-width row would trap the division. A grid with no columns has
        // no cells, so the loop this guards never runs; one keeps the
        // arithmetic total either way.
        let width = self.add(
            Data::Branch {
                cond: usable,
                then_value: read,
                else_value: Some(one),
            },
            Ty::U64,
        );
        let index = self.load(index_slot, Ty::U64);
        let position = self.add(Data::Mod(index, width), Ty::U64);
        let boundary = self.constant(0, Ty::U64);
        self.add(Data::Eq(position, boundary), Ty::Bool)
    }

    /// Append a byte string double-quoted, with `\` and `"` escaped.
    fn append_quoted(
        &mut self,
        statements: &mut Vec<u32>,
        view: &ByteView,
        source: &LeafSource,
        owner_ty: &Ty,
    ) {
        let pointer_ty = byte_view_pointer_type(view);
        let pointer = self.read_through(source, owner_ty, &view.pointer, pointer_ty);
        let address = self.add(
            Data::Intrinsic {
                operation: IntrinsicOperation::PtrToInt,
                name: Arc::from(IntrinsicOperation::PtrToInt.expected_spelling()),
                args: Arc::new([argument(pointer)]),
            },
            Ty::U64,
        );
        statements.push(self.store(slot::QUOTED, address));
        let length = self.read_through(source, owner_ty, &view.length, Ty::U64);
        statements.push(self.store(slot::QUOTED_LEN, length));
        let zero = self.constant(0, Ty::U64);
        statements.push(self.store(slot::QUOTED_CURSOR, zero));
        self.append_literal(statements, "\"");

        let cursor = self.load(slot::QUOTED_CURSOR, Ty::U64);
        let limit = self.load(slot::QUOTED_LEN, Ty::U64);
        let remaining = self.add(Data::Lt(cursor, limit), Ty::Bool);
        let overflowed = self.load(slot::OVERFLOWED, Ty::U64);
        let zero = self.constant(0, Ty::U64);
        let room = self.add(Data::Eq(overflowed, zero), Ty::Bool);
        let condition = self.add(Data::And(remaining, room), Ty::Bool);

        let mut body = Vec::new();
        let byte = self.quoted_byte();
        let quote = self.constant(u64::from(b'"'), Ty::U8);
        let is_quote = self.add(Data::Eq(byte, quote), Ty::Bool);
        let byte = self.quoted_byte();
        let backslash = self.constant(u64::from(b'\\'), Ty::U8);
        let is_backslash = self.add(Data::Eq(byte, backslash), Ty::Bool);
        let escaped = self.add(Data::Or(is_quote, is_backslash), Ty::Bool);
        let mut prefix = Vec::new();
        self.append_literal(&mut prefix, "\\");
        body.push(self.guard(escaped, prefix));
        let one = self.constant(1, Ty::U64);
        self.append_bytes(&mut body, one, |builder| builder.quoted_pointer());
        let cursor = self.load(slot::QUOTED_CURSOR, Ty::U64);
        let one = self.constant(1, Ty::U64);
        let next = self.add(Data::Add(cursor, one), Ty::U64);
        body.push(self.store(slot::QUOTED_CURSOR, next));
        let body = self.block(body);
        statements.push(self.add(
            Data::Loop {
                cond: condition,
                body,
            },
            Ty::Unit,
        ));
        self.append_literal(statements, "\"");
    }

    /// Append the shortest round-trip decimal of an `f32`/`f64`.
    ///
    /// The formatting itself is the runtime's, through the one helper
    /// `@to_string` and `@dbg` already share, so there is exactly one float
    /// spelling in the language. The helper answers through an out-pointer with
    /// the runtime ABI's own `{ptr, cap, len}` shape; this body receives it as
    /// three machine words rather than as a `StrBuf`, which is a
    /// standard-library type the printer may not name.
    fn append_float(
        &mut self,
        statements: &mut Vec<u32>,
        source: &LeafSource,
        owner_ty: &Ty,
        leaf_ty: &Ty,
    ) {
        let narrow = *leaf_ty == Ty::F32;
        let read = self.read_leaf(source, owner_ty, leaf_ty);
        let pattern_ty = if narrow { Ty::U32 } else { Ty::U64 };
        let bits = self.add(
            Data::Intrinsic {
                operation: IntrinsicOperation::BitCast,
                name: Arc::from(IntrinsicOperation::BitCast.expected_spelling()),
                args: Arc::new([argument(read)]),
            },
            pattern_ty.clone(),
        );
        // An `f32` crosses the ABI zero-extended into the same `u64` the
        // helper's width discriminator then interprets.
        let bits = self.widen(bits, &pattern_ty, Ty::U64);
        let width = self.constant(
            u64::from(if narrow {
                rue_runtime_abi::FLOAT_WIDTH_F32
            } else {
                rue_runtime_abi::FLOAT_WIDTH_F64
            }),
            Ty::U32,
        );
        let text = self.add(
            Data::RuntimeCall {
                runtime: RuntimeCallKind::ToStringFloat,
                args: Arc::new([argument(bits), argument(width)]),
            },
            float_text_type(),
        );
        let slot = self.float_slot;
        statements.push(self.store(slot, text));
        let length = self.float_word(FLOAT_TEXT_LEN);
        self.append_bytes(statements, length, |builder| {
            let address = builder.float_word(FLOAT_TEXT_POINTER);
            builder.add(
                Data::Intrinsic {
                    operation: IntrinsicOperation::IntToPtr,
                    name: Arc::from(IntrinsicOperation::IntToPtr.expected_spelling()),
                    args: Arc::new([argument(address)]),
                },
                byte_pointer_type(),
            )
        });
    }

    /// One word of the float formatter's result.
    fn float_word(&mut self, index: u64) -> u32 {
        let position = self.constant(index, Ty::U64);
        let slot = self.float_slot;
        let place = self.place(Place {
            base: AirPlaceBase::Local(slot),
            base_type: float_text_type(),
            projections: Arc::new([Projection::Index {
                array_type: float_text_type(),
                index: position,
            }]),
        });
        self.add(Data::PlaceRead { place }, Ty::U64)
    }

    /// The address of the byte a quoted rendering is at.
    fn quoted_pointer(&mut self) -> u32 {
        let address = self.load(slot::QUOTED, Ty::U64);
        let base = self.add(
            Data::Intrinsic {
                operation: IntrinsicOperation::IntToPtr,
                name: Arc::from(IntrinsicOperation::IntToPtr.expected_spelling()),
                args: Arc::new([argument(address)]),
            },
            byte_pointer_type(),
        );
        let cursor = self.load(slot::QUOTED_CURSOR, Ty::U64);
        self.add(
            Data::Intrinsic {
                operation: IntrinsicOperation::PtrOffset,
                name: Arc::from(IntrinsicOperation::PtrOffset.expected_spelling()),
                args: Arc::new([argument(base), argument(cursor)]),
            },
            byte_pointer_type(),
        )
    }

    /// The byte a quoted rendering is at.
    fn quoted_byte(&mut self) -> u32 {
        let pointer = self.quoted_pointer();
        self.add(
            Data::Intrinsic {
                operation: IntrinsicOperation::PtrRead,
                name: Arc::from(IntrinsicOperation::PtrRead.expected_spelling()),
                args: Arc::new([argument(pointer)]),
            },
            Ty::U8,
        )
    }

    /// Read one leaf's value.
    ///
    /// Re-emitted per use rather than shared: a value is lowered where it is
    /// first named, and the decimal renderer names its subject inside two
    /// different branch arms.
    fn read_leaf(&mut self, source: &LeafSource, owner_ty: &Ty, leaf_ty: &Ty) -> u32 {
        match source {
            LeafSource::Projected(_) | LeafSource::Local { .. } | LeafSource::Element(_) => {
                self.read_through(source, owner_ty, &[], leaf_ty.clone())
            }
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
        let base = match root {
            PlaceRoot::Param => AirPlaceBase::Param(0),
            PlaceRoot::Local(slot) => AirPlaceBase::Local(*slot),
            PlaceRoot::Indirect(pointer) => {
                AirPlaceBase::Indirect(rue_air::AirRef::from_raw(*pointer))
            }
        };
        let place = self.place(Place {
            base,
            base_type: base_type.clone(),
            projections: projections
                .iter()
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
