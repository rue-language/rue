//! The exact body-fact provider boundary (RUE-1091, ADR-0066 §4).
//!
//! Under the activated per-body context repair, AIR body analysis no longer
//! receives a complete declaration slice or a prepared semantic epoch. It
//! receives a narrow [`BodyFactProvider`] capability whose typed operations
//! answer exactly the questions a body asks: name and import lookup, method and
//! operator candidates, declaration identity/signature/comptime/well-formedness/
//! anonymous facts, language-item identity, drop/`@copy` metadata, and
//! producer/trusted-toolchain facts.
//!
//! Two properties are load-bearing and enforced by this module's shape:
//!
//! - **Candidate sets, not winners.** Name, import, method, and operator
//!   operations return the full candidate set (empty, unique, ambiguous,
//!   visibility- and kind-filtered), so type-directed selection stays overlay-
//!   local. The provider never resolves a winner on the body's behalf.
//! - **Owned data in, owned data out.** Every operation takes owned inputs and
//!   returns owned facts. rue-air stays independent of the query runtime: the
//!   candidate-set facts are modeled here in rue-air, and the heavier durable
//!   declaration facts are associated types the compiler-side implementation
//!   binds to its own owned durable projections. No query-runtime handle,
//!   live analyzer state, merged program, or borrowed table crosses this
//!   boundary.
//!
//! The compiler-side implementation (`rue-compiler`) runs each operation inside
//! the `BodyTransaction` query context and records the corresponding query edge
//! before returning, so the typed provider call *is* the dependency observation.

use std::sync::Arc;

use crate::types::LangItem;

/// The name-resolution namespace a lookup consults. Owned mirror of the
/// compiler's `DefinitionNamespace`: it lets a body name a namespace without
/// depending on the compiler's presemantic candidate model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ProviderNamespace {
    /// Module-item candidates: functions, structs, enums, and constants share
    /// this key space until semantic evaluation distinguishes them.
    ModuleItem,
    /// Destructor declarations, keyed by the type name following `drop fn`.
    Destructor,
}

/// The syntactic kind of a resolved candidate. Owned mirror of the compiler's
/// `DefinitionKind`, carried on every candidate so a body can distinguish a
/// same-named struct and function without re-consulting the index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ProviderDefinitionKind {
    /// A free function.
    Function,
    /// A named structure type.
    Struct,
    /// A named enumeration type.
    Enum,
    /// A destructor.
    Destructor,
    /// A file-level constant.
    Const,
}

/// One owned name-resolution candidate: its namespace, kind, visibility,
/// unqualified spelling, and language-item identity. Position-free — spans stay
/// in the module-index projection, so a trivia-only edit preserves this fact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameCandidate {
    /// The namespace the candidate was found in.
    pub namespace: ProviderNamespace,
    /// The candidate's syntactic kind.
    pub kind: ProviderDefinitionKind,
    /// Whether the candidate is declared `pub`. Rue visibility is binary
    /// (public versus module-private), so a single flag captures the fact a
    /// cross-module lookup filters on; rue-air needs no dependency on the
    /// parser's `Visibility` spelling to model it.
    pub is_public: bool,
    /// The candidate's unqualified source name.
    pub name: Arc<str>,
    /// The candidate's language-item identity, classified from trusted
    /// standard-library provenance alone.
    pub language_item: Option<LangItem>,
}

impl NameCandidate {
    /// Whether this candidate is `pub`.
    #[must_use]
    pub fn is_public(&self) -> bool {
        self.is_public
    }
}

/// The canonical §4 name-resolution outcome for one consulted
/// `(module, namespace, name)` key: a candidate *set*, never a chosen winner.
/// Success, absence, ambiguity, and index unavailability are first-class typed
/// variants so a body distinguishes them without re-deriving the classification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NameResolution {
    /// The consulted module's index was unavailable (its parse failed).
    IndexUnavailable,
    /// No candidate matches the consulted key.
    Absent,
    /// Exactly one candidate matches.
    Unique(NameCandidate),
    /// More than one candidate shares the consulted key.
    Ambiguous(Arc<[NameCandidate]>),
}

impl NameResolution {
    /// Construct a resolution from an owned candidate set, collapsing to the
    /// canonical variant. An empty set is [`NameResolution::Absent`].
    #[must_use]
    pub fn from_candidates(mut candidates: Vec<NameCandidate>) -> Self {
        match candidates.len() {
            0 => Self::Absent,
            1 => Self::Unique(candidates.pop().expect("length checked")),
            _ => Self::Ambiguous(Arc::from(candidates)),
        }
    }

    /// The candidates carried by this resolution, in candidate order. Empty for
    /// [`NameResolution::Absent`] and [`NameResolution::IndexUnavailable`].
    #[must_use]
    pub fn candidates(&self) -> &[NameCandidate] {
        match self {
            Self::Unique(candidate) => std::slice::from_ref(candidate),
            Self::Ambiguous(candidates) => candidates,
            Self::Absent | Self::IndexUnavailable => &[],
        }
    }

    /// Re-classify keeping only candidates of the requested syntactic kind. Two
    /// lookups over one candidate set that request different kinds yield
    /// distinct canonical records. An unavailable index stays unavailable.
    #[must_use]
    pub fn of_kind(&self, kind: ProviderDefinitionKind) -> Self {
        if matches!(self, Self::IndexUnavailable) {
            return Self::IndexUnavailable;
        }
        Self::from_candidates(
            self.candidates()
                .iter()
                .filter(|candidate| candidate.kind == kind)
                .cloned()
                .collect(),
        )
    }

    /// Re-classify keeping only candidates visible across a visibility boundary.
    /// `public_only` retains public candidates when accessed from another
    /// module; passing `false` retains every candidate (same-module access). A
    /// visibility-filtered view is a distinct record from the unfiltered one
    /// whenever a private candidate is dropped.
    #[must_use]
    pub fn visible(&self, public_only: bool) -> Self {
        if matches!(self, Self::IndexUnavailable) {
            return Self::IndexUnavailable;
        }
        Self::from_candidates(
            self.candidates()
                .iter()
                .filter(|candidate| !public_only || candidate.is_public())
                .cloned()
                .collect(),
        )
    }
}

/// The canonical §4 module-binding / import resolution for one consulted
/// import-path key. Absent, rejected, and ambiguous are first-class results and
/// dependency edges: an edit that makes the path resolve changes this outcome
/// and invalidates exactly the consumers of the failed lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportResolution {
    /// Exactly one directive names the specifier and it normalizes to a
    /// non-empty module path.
    Resolved {
        /// The lexically normalized module path the binding resolves to.
        normalized_specifier: Arc<str>,
    },
    /// No `@import` directive in the consulting module names this specifier.
    Absent,
    /// Exactly one directive names it, but the specifier is malformed (it
    /// normalizes to an empty module path).
    Rejected,
    /// Multiple directives in the consulting module name the same target.
    Ambiguous,
}

/// Whether a receiver member is an instance method (takes a `self` receiver) or
/// an associated function (does not). Both share the compiler's method table and
/// are reached through the same member lookup; a consumer discriminates on this
/// to reproduce the production `MethodCalledAsAssocFn` / `AssocFnCalledAsMethod`
/// diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MemberKind {
    /// A method: takes a `self` receiver.
    Method,
    /// An associated function: takes no `self` receiver.
    AssociatedFunction,
}

/// One member candidate on a receiver type, keyed on
/// `(receiver-type identity, member name)`. Position-free.
///
/// The candidate carries a `declaration` follow-up handle (the implementation's
/// [`BodyFactProvider::DeclarationRef`]) so a consumer performing type-directed
/// selection can fetch the member's full signature — parameter modes, receiver
/// mode, and return type — through [`BodyFactProvider::signature`] without a
/// second lookup. `has_self_receiver` is sourced from that signature, never
/// inferred syntactically, so a method miscalled as an associated function is
/// still classified honestly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberCandidate<Decl> {
    /// Follow-up handle to the member declaration, accepted by
    /// [`BodyFactProvider::signature`] and the other declaration ops.
    pub declaration: Decl,
    /// The member's unqualified source name.
    pub name: Arc<str>,
    /// Whether the member takes a `self` receiver, sourced from its signature.
    pub has_self_receiver: bool,
    /// The syntactic member kind the candidate was declared under.
    pub kind: MemberKind,
    /// Whether the member is `pub`.
    pub is_public: bool,
}

/// One operator-overload candidate on a receiver type. Like [`MemberCandidate`]
/// it carries a `declaration` follow-up handle so the overload's signature is
/// reachable, plus the operator it overloads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperatorMemberCandidate<Decl> {
    /// Follow-up handle to the overload declaration.
    pub declaration: Decl,
    /// The operator the candidate overloads.
    pub operator: OperatorName,
    /// Whether the overload takes a `self` receiver, sourced from its signature.
    pub has_self_receiver: bool,
    /// Whether the overload is `pub`.
    pub is_public: bool,
}

/// A built-in operator whose candidates a body may consult on a receiver type.
/// Rue models operator overloading as specially named member methods; this
/// enum names the operator independently of that spelling so the provider op is
/// keyed on `(receiver-type identity, operator)` rather than a raw string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum OperatorName {
    /// Binary `+`.
    Add,
    /// Binary `-`.
    Sub,
    /// Binary `*`.
    Mul,
    /// Binary `/`.
    Div,
    /// Binary `==`.
    Eq,
    /// Indexing `[]`.
    Index,
}

impl OperatorName {
    /// The member-method spelling Rue uses for this operator's overload.
    #[must_use]
    pub fn method_name(self) -> &'static str {
        match self {
            Self::Add => "add",
            Self::Sub => "sub",
            Self::Mul => "mul",
            Self::Div => "div",
            Self::Eq => "eq",
            Self::Index => "index",
        }
    }
}

/// Whether a nominal declaration is well-formed. A body observes this fact
/// exactly (not the underlying diagnostic) so an ill-formed nominal invalidates
/// only the bodies that consulted its well-formedness.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NominalWellFormedness {
    /// The nominal analyzed without a well-formedness error.
    WellFormed,
    /// The nominal has a deterministic well-formedness failure.
    IllFormed,
}

/// Drop and `@copy` metadata for a receiver type, sourced from its exact
/// destructor lookup and struct signature terminals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DropCopyMetadata {
    /// Whether a `drop fn` destructor is declared for the type.
    pub has_destructor: bool,
    /// Whether the type is marked `@copy`.
    pub is_copy: bool,
}

/// The exact body-fact provider boundary.
///
/// Every operation returns owned facts and, in the compiler-side
/// implementation, records the exact backing query edge before returning. The
/// candidate-set facts (name/import/method/operator resolution, language item,
/// drop/`@copy`, well-formedness) are modeled fully in rue-air. The heavier
/// durable declaration facts are associated types the implementation binds to
/// its own owned durable projections, keeping rue-air independent of the query
/// runtime while still returning owned data.
pub trait BodyFactProvider {
    /// The implementation's owned handle for a consulted module.
    type ModuleRef: Clone;
    /// The implementation's owned handle for a consulted declaration.
    type DeclarationRef: Clone;
    /// The implementation's owned identity for one exact body instance.
    ///
    /// Producer and trusted-toolchain facts are properties of the body that
    /// actually runs, not merely of the declaration that supplied its source.
    /// In particular, a specialization, anonymous member, or drop-glue body
    /// must keep its wrapper identity when it crosses this boundary.
    type BodyInstanceRef: Clone;
    /// The implementation's owned handle for a receiver type's identity.
    type ReceiverType: Clone;

    /// The implementation's owned declaration-identity fact.
    type DeclarationIdentity;
    /// The implementation's owned resolved-signature fact.
    type Signature;
    /// The implementation's owned constant/comptime-result fact.
    type ConstComptime;
    /// The implementation's owned comptime type/value argument (the durable type
    /// and value algebra a comptime call binds). Named as associated types so
    /// rue-air stays independent of the compiler's durable representation while a
    /// consumer still passes owned arguments into
    /// [`BodyFactProvider::reduce_comptime_call`].
    type ComptimeType;
    /// The implementation's owned comptime value argument.
    type ComptimeValue;
    /// The implementation's owned comptime-call reduction fact (the resolved
    /// type-or-value a reduced constructor call yields).
    type ComptimeCall;
    /// The implementation's owned anonymous-nominal fact.
    type AnonymousFacts;
    /// The implementation's owned producer-body fact.
    type ProducerBodyFacts;
    /// The implementation's owned trusted-toolchain fact set.
    type ToolchainFacts;

    /// Exact unqualified name lookup: the full candidate set for
    /// `(module, namespace, name)`, including empty and ambiguous results.
    fn lookup_unqualified(
        &self,
        module: &Self::ModuleRef,
        namespace: ProviderNamespace,
        name: &str,
    ) -> NameResolution;

    /// Exact qualified name lookup against the named target module. Same
    /// candidate-set semantics as [`BodyFactProvider::lookup_unqualified`];
    /// callers apply visibility filtering with [`NameResolution::visible`].
    fn lookup_qualified(
        &self,
        module: &Self::ModuleRef,
        namespace: ProviderNamespace,
        name: &str,
    ) -> NameResolution;

    /// Exact member candidates on a receiver type, keyed on
    /// `(receiver-type identity, member name)`. Returns the candidate *set*
    /// spanning both methods and associated functions (never a selected
    /// overload); each candidate carries a follow-up handle to its signature and
    /// the `self`-receiver classification sourced from that signature.
    fn method_candidates(
        &self,
        receiver: &Self::ReceiverType,
        name: &str,
    ) -> Vec<MemberCandidate<Self::DeclarationRef>>;

    /// Exact operator-overload candidates on a receiver type, keyed on
    /// `(receiver-type identity, operator)`. Returns the candidate set; each
    /// candidate carries a follow-up handle to its signature.
    fn operator_candidates(
        &self,
        receiver: &Self::ReceiverType,
        operator: OperatorName,
    ) -> Vec<OperatorMemberCandidate<Self::DeclarationRef>>;

    /// Exact declaration identity for the consulted declaration, or `None` when
    /// no such declaration resolves.
    fn declaration_identity(
        &self,
        decl: &Self::DeclarationRef,
    ) -> Option<Self::DeclarationIdentity>;

    /// Exact resolved signature for the consulted declaration.
    fn signature(&self, decl: &Self::DeclarationRef) -> Option<Self::Signature>;

    /// Exact constant/comptime result for the consulted declaration.
    fn const_comptime(&self, decl: &Self::DeclarationRef) -> Option<Self::ConstComptime>;

    /// Reduce a comptime type-constructor call at the boundary: given the
    /// resolved constructor declaration and its argument bindings (already
    /// resolved to owned comptime types and values, keyed by the constructor's
    /// own parameter names), answer the reduced type-or-value fact, or `None`
    /// when the declaration does not resolve or does not reduce.
    ///
    /// This is the argument-parameterized analog of [`Self::const_comptime`]:
    /// where `const_comptime` reduces a *nullary* declaration-time constant, this
    /// reduces a call with bound type/value arguments. The reduction itself stays
    /// in the implementation's query machinery; the caller supplies only the
    /// selected head declaration and the bound arguments, so the boundary never
    /// re-derives the argument list from syntax.
    fn reduce_comptime_call(
        &self,
        decl: &Self::DeclarationRef,
        type_arguments: &[(Arc<str>, Self::ComptimeType)],
        value_arguments: &[(Arc<str>, Self::ComptimeValue)],
    ) -> Option<Self::ComptimeCall>;

    /// Exact nominal well-formedness for the consulted declaration, or `None`
    /// when it is not a nominal.
    fn nominal_well_formedness(&self, decl: &Self::DeclarationRef)
    -> Option<NominalWellFormedness>;

    /// Exact anonymous-nominal facts produced by the consulted declaration.
    fn anonymous_facts(&self, decl: &Self::DeclarationRef) -> Option<Self::AnonymousFacts>;

    /// Exact language-item identity for a consulted `(module, namespace, name)`
    /// nominal candidate.
    fn language_item(
        &self,
        module: &Self::ModuleRef,
        namespace: ProviderNamespace,
        name: &str,
    ) -> Option<LangItem>;

    /// Exact drop/`@copy` metadata for a receiver type.
    fn drop_copy_metadata(&self, receiver: &Self::ReceiverType) -> Option<DropCopyMetadata>;

    /// Exact module-binding / import resolution for one consulted import path,
    /// including absent, rejected, and ambiguous results.
    fn resolve_import(&self, module: &Self::ModuleRef, specifier: &str) -> ImportResolution;

    /// Exact producer-body facts for body analysis instance.
    fn producer_body_facts(
        &self,
        instance: &Self::BodyInstanceRef,
    ) -> Option<Self::ProducerBodyFacts>;

    /// Exact trusted-toolchain facts already required by body analysis instance.
    fn trusted_toolchain_facts(&self, instance: &Self::BodyInstanceRef) -> Self::ToolchainFacts;
}
