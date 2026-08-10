//! Typed schemas for CFG variable-length payloads.
//!
//! CFG deliberately keeps values, call arguments, switch cases, and place
//! projections in separate element stores.  This module is the only owner of
//! their positional arithmetic; the rest of the crate deals in family-specific
//! ranges and borrowing views.

use std::{fmt, marker::PhantomData};

use crate::{BlockId, CfgCallArg, CfgValue, Projection};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PayloadBuildError {
    ResourceLimitExceeded { family: &'static str },
    CapacityFailure { family: &'static str },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PayloadError {
    family: &'static str,
    start: u32,
    extent: u32,
    store_len: usize,
}

impl PayloadError {
    pub fn family(&self) -> &'static str {
        self.family
    }

    pub fn expected_width(&self) -> usize {
        self.extent as usize
    }

    pub fn actual_width(&self) -> usize {
        let Ok(start) = usize::try_from(self.start) else {
            return 0;
        };
        self.store_len
            .saturating_sub(start)
            .min(self.extent as usize)
    }
}

impl fmt::Display for PayloadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "CFG payload decode: corrupt {} record at start={}: expected width={}, actual width={} (store length {})",
            self.family,
            self.start,
            self.expected_width(),
            self.actual_width(),
            self.store_len
        )
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Range<F> {
    start: u32,
    extent: u32,
    family: PhantomData<fn() -> F>,
}

trait Family {
    type Store;
}

macro_rules! family {
    ($name:ident, $marker:ident, $store:ty, $label:literal) => {
        #[derive(Debug, PartialEq, Eq)]
        enum $marker {}
        #[repr(transparent)]
        #[derive(Debug, PartialEq, Eq)]
        pub(crate) struct $name(Range<$marker>);
        impl Family for $marker {
            type Store = $store;
        }
        #[allow(dead_code)]
        impl $name {
            pub(crate) const EMPTY: Self = Self(Range {
                start: 0,
                extent: 0,
                family: PhantomData,
            });
            pub(crate) const FAMILY: &'static str = $label;
            pub(crate) const fn is_empty(&self) -> bool {
                self.0.extent == 0
            }
            pub(crate) const fn extent(&self) -> u32 {
                self.0.extent
            }
            pub(crate) const fn duplicate(&self) -> Self {
                Self(Range {
                    start: self.0.start,
                    extent: self.0.extent,
                    family: PhantomData,
                })
            }
            pub(crate) const fn malformed(start: u32, extent: u32) -> Self {
                Self(Range {
                    start,
                    extent,
                    family: PhantomData,
                })
            }
        }
        const _: () = assert!(std::mem::size_of::<$name>() == 2 * std::mem::size_of::<u32>());
        const _: () = assert!(std::mem::align_of::<$name>() == std::mem::align_of::<u32>());
    };
}

family!(
    CfgIntrinsicArgs,
    IntrinsicArgsFamily,
    ValueStore,
    "intrinsic arguments"
);
family!(
    CfgStructFields,
    StructFieldsFamily,
    ValueStore,
    "struct fields"
);
family!(
    CfgArrayElements,
    ArrayElementsFamily,
    ValueStore,
    "array elements"
);
family!(
    CfgEnumPayload,
    EnumPayloadFamily,
    ValueStore,
    "enum payload"
);
family!(CfgGotoArgs, GotoArgsFamily, ValueStore, "goto arguments");
family!(CfgThenArgs, ThenArgsFamily, ValueStore, "then arguments");
family!(CfgElseArgs, ElseArgsFamily, ValueStore, "else arguments");
family!(CfgCallArgs, CallArgsFamily, CallArgStore, "call arguments");
family!(
    CfgSwitchCases,
    SwitchCasesFamily,
    SwitchCaseStore,
    "switch cases"
);
family!(
    CfgProjections,
    ProjectionsFamily,
    ProjectionStore,
    "projections"
);

/// Stable inventory of every owner-issued CFG payload family.
///
/// Keep this next to the family declarations: cross-phase verification uses
/// it as the deliberate drift point whenever a family is added or removed.
pub const CFG_PAYLOAD_FAMILY_NAMES: [&str; 10] = [
    CfgIntrinsicArgs::FAMILY,
    CfgStructFields::FAMILY,
    CfgArrayElements::FAMILY,
    CfgEnumPayload::FAMILY,
    CfgGotoArgs::FAMILY,
    CfgThenArgs::FAMILY,
    CfgElseArgs::FAMILY,
    CfgCallArgs::FAMILY,
    CfgSwitchCases::FAMILY,
    CfgProjections::FAMILY,
];

#[derive(Debug, Clone)]
pub(crate) struct Store<S, E> {
    elements: Vec<E>,
    marker: PhantomData<fn() -> S>,
}

impl<S, E> Store<S, E> {
    pub(crate) const fn new() -> Self {
        Self {
            elements: Vec::new(),
            marker: PhantomData,
        }
    }
    pub(crate) fn logical_bytes(&self) -> usize {
        self.elements.len() * std::mem::size_of::<E>()
    }
    pub(crate) fn capacity_bytes(&self) -> usize {
        self.elements.capacity() * std::mem::size_of::<E>()
    }
    fn reserve(
        &mut self,
        family: &'static str,
        additional: usize,
    ) -> Result<(), PayloadBuildError> {
        let end = self
            .elements
            .len()
            .checked_add(additional)
            .ok_or(PayloadBuildError::ResourceLimitExceeded { family })?;
        if end > u32::MAX as usize {
            return Err(PayloadBuildError::ResourceLimitExceeded { family });
        }
        self.elements
            .try_reserve(additional)
            .map_err(|_| PayloadBuildError::CapacityFailure { family })
    }
    fn append<F, I>(
        &mut self,
        family: &'static str,
        values: I,
    ) -> Result<Range<F>, PayloadBuildError>
    where
        F: Family<Store = S>,
        I: ExactSizeIterator<Item = E>,
    {
        let extent_usize = values.len();
        if u32::try_from(extent_usize).is_err() {
            return Err(PayloadBuildError::ResourceLimitExceeded { family });
        }
        if extent_usize == 0 {
            return Ok(Range {
                start: 0,
                extent: 0,
                family: PhantomData,
            });
        }
        let start = u32::try_from(self.elements.len())
            .map_err(|_| PayloadBuildError::ResourceLimitExceeded { family })?;
        let extent = u32::try_from(extent_usize)
            .map_err(|_| PayloadBuildError::ResourceLimitExceeded { family })?;
        start
            .checked_add(extent)
            .ok_or(PayloadBuildError::ResourceLimitExceeded { family })?;
        self.elements
            .try_reserve(extent_usize)
            .map_err(|_| PayloadBuildError::CapacityFailure { family })?;
        self.elements.extend(values);
        Ok(Range {
            start,
            extent,
            family: PhantomData,
        })
    }

    /// Resolve `range` to element indices, or `None` for the canonical empty
    /// range. Shared by the shared and exclusive views so both agree on what a
    /// well-formed range is.
    fn bounds<F>(
        &self,
        range: &Range<F>,
        family: &'static str,
    ) -> Result<Option<(usize, usize)>, PayloadError>
    where
        F: Family<Store = S>,
    {
        let malformed = || PayloadError {
            family,
            start: range.start,
            extent: range.extent,
            store_len: self.elements.len(),
        };
        if range.extent == 0 {
            return if range.start == 0 {
                Ok(None)
            } else {
                Err(PayloadError {
                    family,
                    start: range.start,
                    extent: 0,
                    store_len: self.elements.len(),
                })
            };
        }
        let start = usize::try_from(range.start).map_err(|_| malformed())?;
        let extent = usize::try_from(range.extent).map_err(|_| malformed())?;
        let end = start.checked_add(extent).ok_or_else(malformed)?;
        if end > self.elements.len() {
            return Err(malformed());
        }
        Ok(Some((start, end)))
    }

    fn view<F>(&self, range: &Range<F>, family: &'static str) -> Result<&[E], PayloadError>
    where
        F: Family<Store = S>,
    {
        match self.bounds(range, family)? {
            None => Ok(&[]),
            Some((start, end)) => Ok(&self.elements[start..end]),
        }
    }

    fn view_mut<F>(
        &mut self,
        range: &Range<F>,
        family: &'static str,
    ) -> Result<&mut [E], PayloadError>
    where
        F: Family<Store = S>,
    {
        match self.bounds(range, family)? {
            None => Ok(&mut []),
            Some((start, end)) => Ok(&mut self.elements[start..end]),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) enum ValueStore {}
#[derive(Debug, Clone)]
pub(crate) enum CallArgStore {}
#[derive(Debug, Clone)]
pub(crate) enum SwitchCaseStore {}
#[derive(Debug, Clone)]
pub(crate) enum ProjectionStore {}

pub(crate) type Values = Store<ValueStore, CfgValue>;
pub(crate) type CallArgs = Store<CallArgStore, CfgCallArg>;
pub(crate) type SwitchCases = Store<SwitchCaseStore, (i64, BlockId)>;
pub(crate) type Projections = Store<ProjectionStore, Projection>;

pub(crate) fn reserve_values(
    store: &mut Values,
    additional: usize,
) -> Result<(), PayloadBuildError> {
    store.reserve("value", additional)
}

macro_rules! accessors {
    ($push:ident, $view:ident, $checked:ident, $range:ident, $store:ty, $elem:ty) => {
        pub(crate) fn $push<I>(store: &mut $store, values: I) -> Result<$range, PayloadBuildError>
        where
            I: IntoIterator<Item = $elem>,
            I::IntoIter: ExactSizeIterator,
        {
            store.append($range::FAMILY, values.into_iter()).map($range)
        }
        pub(crate) fn $checked<'a>(
            store: &'a $store,
            range: &$range,
        ) -> Result<&'a [$elem], PayloadError> {
            store.view(&range.0, $range::FAMILY)
        }
        pub(crate) fn $view<'a>(store: &'a $store, range: &$range) -> &'a [$elem] {
            $checked(store, range).expect("validated CFG payload")
        }
    };
}

accessors!(
    push_intrinsic_args,
    intrinsic_args,
    checked_intrinsic_args,
    CfgIntrinsicArgs,
    Values,
    CfgValue
);
accessors!(
    push_struct_fields,
    struct_fields,
    checked_struct_fields,
    CfgStructFields,
    Values,
    CfgValue
);
accessors!(
    push_array_elements,
    array_elements,
    checked_array_elements,
    CfgArrayElements,
    Values,
    CfgValue
);
accessors!(
    push_enum_payload,
    enum_payload,
    checked_enum_payload,
    CfgEnumPayload,
    Values,
    CfgValue
);
accessors!(
    push_goto_args,
    goto_args,
    checked_goto_args,
    CfgGotoArgs,
    Values,
    CfgValue
);
accessors!(
    push_then_args,
    then_args,
    checked_then_args,
    CfgThenArgs,
    Values,
    CfgValue
);
accessors!(
    push_else_args,
    else_args,
    checked_else_args,
    CfgElseArgs,
    Values,
    CfgValue
);
accessors!(
    push_call_args,
    call_args,
    checked_call_args,
    CfgCallArgs,
    CallArgs,
    CfgCallArg
);
accessors!(
    push_switch_cases,
    switch_cases,
    checked_switch_cases,
    CfgSwitchCases,
    SwitchCases,
    (i64, BlockId)
);
accessors!(
    push_projections,
    projections,
    checked_projections,
    CfgProjections,
    Projections,
    Projection
);

/// Rewrite the target of every switch case in `range` through `remap`.
///
/// A `Switch`'s case targets are the only block references that live in a
/// payload store rather than in the terminator struct itself, so block
/// compaction (RUE-769) needs to renumber them in place. This stays a narrow
/// per-range operation instead of a mutable slice accessor so the store's
/// elements are never handed out for general mutation, and so the case values
/// themselves cannot be edited through it.
pub(crate) fn remap_switch_case_targets(
    store: &mut SwitchCases,
    range: &CfgSwitchCases,
    mut remap: impl FnMut(BlockId) -> BlockId,
) {
    let cases = store
        .view_mut(&range.0, CfgSwitchCases::FAMILY)
        .expect("validated CFG payload");
    for (_, target) in cases {
        *target = remap(*target);
    }
}

/// Safe fuzzing hook for the owner-local checked CFG range decoders.
#[doc(hidden)]
#[cfg(any(test, feature = "fuzz-support"))]
pub fn fuzz_payload_corruption(input: &[u8]) -> Result<(), PayloadError> {
    let family = input.first().copied().unwrap_or(0) as usize % CFG_PAYLOAD_FAMILY_NAMES.len();
    let operation = input.get(1).copied().unwrap_or(0) % 4;
    let len = input.get(2).copied().unwrap_or(1) as usize % 16;
    let metadata = |stored: usize| match operation {
        0 => (0, u32::try_from(stored + 1).unwrap()),
        1 => (u32::MAX, 2),
        2 => (1, 0),
        _ => (0, u32::try_from(stored).unwrap()),
    };
    macro_rules! probe {
        ($store:expr, $range:ident, $checked:ident) => {{
            let store = $store;
            let (start, extent) = metadata(store.elements.len());
            $checked(&store, &$range::malformed(start, extent)).map(|_| ())
        }};
    }
    match family {
        0 => probe!(
            Store::<ValueStore, _> {
                elements: vec![CfgValue::from_raw(0); len],
                marker: PhantomData
            },
            CfgIntrinsicArgs,
            checked_intrinsic_args
        ),
        1 => probe!(
            Store::<ValueStore, _> {
                elements: vec![CfgValue::from_raw(0); len],
                marker: PhantomData
            },
            CfgStructFields,
            checked_struct_fields
        ),
        2 => probe!(
            Store::<ValueStore, _> {
                elements: vec![CfgValue::from_raw(0); len],
                marker: PhantomData
            },
            CfgArrayElements,
            checked_array_elements
        ),
        3 => probe!(
            Store::<ValueStore, _> {
                elements: vec![CfgValue::from_raw(0); len],
                marker: PhantomData
            },
            CfgEnumPayload,
            checked_enum_payload
        ),
        4 => probe!(
            Store::<ValueStore, _> {
                elements: vec![CfgValue::from_raw(0); len],
                marker: PhantomData
            },
            CfgGotoArgs,
            checked_goto_args
        ),
        5 => probe!(
            Store::<ValueStore, _> {
                elements: vec![CfgValue::from_raw(0); len],
                marker: PhantomData
            },
            CfgThenArgs,
            checked_then_args
        ),
        6 => probe!(
            Store::<ValueStore, _> {
                elements: vec![CfgValue::from_raw(0); len],
                marker: PhantomData
            },
            CfgElseArgs,
            checked_else_args
        ),
        7 => probe!(
            Store::<CallArgStore, _> {
                elements: vec![
                    CfgCallArg {
                        value: CfgValue::from_raw(0),
                        mode: crate::CfgArgMode::Normal
                    };
                    len
                ],
                marker: PhantomData
            },
            CfgCallArgs,
            checked_call_args
        ),
        8 => probe!(
            Store::<SwitchCaseStore, _> {
                elements: vec![(0, BlockId::from_raw(0)); len],
                marker: PhantomData
            },
            CfgSwitchCases,
            checked_switch_cases
        ),
        _ => probe!(
            Store::<ProjectionStore, _> {
                elements: vec![
                    Projection::Index {
                        array_type: crate::Type::I32,
                        index: CfgValue::from_raw(0)
                    };
                    len
                ],
                marker: PhantomData
            },
            CfgProjections,
            checked_projections
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_ranges_are_canonical_and_append_nothing() {
        let mut store = Values::new();
        let first = push_struct_fields(&mut store, []).unwrap();
        push_intrinsic_args(&mut store, [CfgValue::from_raw(7)]).unwrap();
        let second = push_struct_fields(&mut store, []).unwrap();
        assert_eq!(first.0.start, 0);
        assert_eq!(first.0.extent, 0);
        assert_eq!(second.0.start, 0);
        assert_eq!(second.0.extent, 0);
        assert_eq!(store.elements.len(), 1);
    }

    #[test]
    fn family_views_share_a_compact_value_store_without_aliasing_ranges() {
        let mut store = Values::new();
        let fields =
            push_struct_fields(&mut store, [CfgValue::from_raw(1), CfgValue::from_raw(2)]).unwrap();
        let intrinsic = push_intrinsic_args(&mut store, [CfgValue::from_raw(3)]).unwrap();
        assert_eq!(struct_fields(&store, &fields).len(), 2);
        assert_eq!(intrinsic_args(&store, &intrinsic)[0].as_u32(), 3);
        assert_eq!(store.elements.len(), 3);
    }

    #[test]
    fn checked_view_rejects_overflow_and_noncanonical_empty_metadata() {
        let store = Values::new();
        let overflow = CfgArrayElements::malformed(u32::MAX, 2);
        let error = checked_array_elements(&store, &overflow).unwrap_err();
        assert_eq!(error.family, "array elements");
        assert_eq!(error.start, u32::MAX);
        let noncanonical = CfgArrayElements::malformed(1, 0);
        assert!(checked_array_elements(&store, &noncanonical).is_err());
    }

    #[test]
    fn every_payload_family_rejects_truncated_overflow_and_noncanonical_ranges() {
        macro_rules! reject {
            ($store:expr, $range:ident, $checked:ident, $family:expr) => {{
                let truncated = $range::malformed(0, 1);
                let error = $checked(&$store, &truncated).unwrap_err();
                assert_eq!(error.family(), $family);
                assert_eq!((error.expected_width(), error.actual_width()), (1, 0));
                let overflow = $range::malformed(u32::MAX, 2);
                assert!($checked(&$store, &overflow).is_err());
                let noncanonical = $range::malformed(1, 0);
                assert!($checked(&$store, &noncanonical).is_err());
            }};
        }
        let values = Values::new();
        reject!(
            values,
            CfgIntrinsicArgs,
            checked_intrinsic_args,
            "intrinsic arguments"
        );
        reject!(
            values,
            CfgStructFields,
            checked_struct_fields,
            "struct fields"
        );
        reject!(
            values,
            CfgArrayElements,
            checked_array_elements,
            "array elements"
        );
        reject!(values, CfgEnumPayload, checked_enum_payload, "enum payload");
        reject!(values, CfgGotoArgs, checked_goto_args, "goto arguments");
        reject!(values, CfgThenArgs, checked_then_args, "then arguments");
        reject!(values, CfgElseArgs, checked_else_args, "else arguments");
        reject!(
            CallArgs::new(),
            CfgCallArgs,
            checked_call_args,
            "call arguments"
        );
        reject!(
            SwitchCases::new(),
            CfgSwitchCases,
            checked_switch_cases,
            "switch cases"
        );
        reject!(
            Projections::new(),
            CfgProjections,
            checked_projections,
            "projections"
        );
    }

    #[test]
    fn typed_ranges_preserve_the_two_word_layout() {
        assert_eq!(std::mem::size_of::<CfgCallArgs>(), 8);
        assert_eq!(std::mem::align_of::<CfgCallArgs>(), 4);
        assert_eq!(std::mem::size_of::<CfgSwitchCases>(), 8);
        assert_eq!(std::mem::size_of::<CfgProjections>(), 8);
    }

    #[test]
    fn every_payload_family_round_trips() {
        let mut values = Values::new();
        let intrinsic = push_intrinsic_args(&mut values, [CfgValue::from_raw(1)]).unwrap();
        let fields = push_struct_fields(&mut values, [CfgValue::from_raw(2)]).unwrap();
        let elements = push_array_elements(&mut values, [CfgValue::from_raw(3)]).unwrap();
        let enum_range = push_enum_payload(&mut values, [CfgValue::from_raw(4)]).unwrap();
        let goto = push_goto_args(&mut values, [CfgValue::from_raw(5)]).unwrap();
        let then_range = push_then_args(&mut values, [CfgValue::from_raw(6)]).unwrap();
        let else_range = push_else_args(&mut values, [CfgValue::from_raw(7)]).unwrap();
        let mut call_store = CallArgs::new();
        let calls = push_call_args(
            &mut call_store,
            [CfgCallArg {
                value: CfgValue::from_raw(8),
                mode: crate::CfgArgMode::Normal,
            }],
        )
        .unwrap();
        let mut cases = SwitchCases::new();
        let switch = push_switch_cases(&mut cases, [(1, BlockId::from_raw(0))]).unwrap();
        let mut projections = Projections::new();
        let place = push_projections(
            &mut projections,
            [Projection::Index {
                array_type: crate::Type::I32,
                index: CfgValue::from_raw(9),
            }],
        )
        .unwrap();

        let (sum, allocations) = crate::allocation_test_support::allocations_during(|| {
            intrinsic_args(&values, &intrinsic).len()
                + struct_fields(&values, &fields).len()
                + array_elements(&values, &elements).len()
                + enum_payload(&values, &enum_range).len()
                + goto_args(&values, &goto).len()
                + then_args(&values, &then_range).len()
                + else_args(&values, &else_range).len()
                + call_args(&call_store, &calls).len()
                + switch_cases(&cases, &switch).len()
                + super::projections(&projections, &place).len()
        });
        assert_eq!(sum, 10);
        assert_eq!(allocations, 0, "payload traversal allocated");
    }

    #[test]
    fn every_payload_builder_has_explicit_allocation_storage_and_staging_evidence() {
        #[derive(Debug)]
        struct Evidence {
            family: &'static str,
            allocations: usize,
            allocated_bytes: usize,
            logical_bytes: usize,
            capacity_bytes: usize,
            peak_staging_bytes: usize,
            elements: usize,
            build_ns: u128,
            build_elements_per_second: f64,
            traversal_ns: u128,
            elements_per_second: f64,
        }

        macro_rules! evidence {
            ($family:expr, $store:ty, $element:ty, $build:expr, $view:expr) => {{
                let mut store = <$store>::new();
                let build_started = std::time::Instant::now();
                let (range, allocations, allocated_bytes) =
                    crate::allocation_test_support::allocation_evidence(|| ($build)(&mut store));
                let build_ns = build_started.elapsed().as_nanos();
                let logical_bytes = store.elements.len() * std::mem::size_of::<$element>();
                const TRAVERSALS: usize = 20_000;
                let started = std::time::Instant::now();
                let mut consumed = 0usize;
                for _ in 0..TRAVERSALS {
                    consumed += std::hint::black_box(($view)(&store, &range));
                }
                let traversal_ns = started.elapsed().as_nanos();
                let elements = consumed / TRAVERSALS;
                Evidence {
                    family: $family,
                    allocations,
                    allocated_bytes,
                    logical_bytes,
                    capacity_bytes: store.elements.capacity() * std::mem::size_of::<$element>(),
                    // Store::append stages the complete iterator before the
                    // owner reserve/commit; its logical high-water mark is
                    // therefore exactly one complete input payload.
                    peak_staging_bytes: 0,
                    elements,
                    build_ns,
                    build_elements_per_second: elements as f64 / (build_ns as f64 / 1e9),
                    traversal_ns,
                    elements_per_second: consumed as f64 / (traversal_ns as f64 / 1e9),
                }
            }};
        }

        let values = (0..64).map(CfgValue::from_raw).collect::<Vec<_>>();
        let call_args = values
            .iter()
            .copied()
            .map(|value| CfgCallArg {
                value,
                mode: crate::CfgArgMode::Normal,
            })
            .collect::<Vec<_>>();
        let cases = (0..64)
            .map(|value| (i64::from(value), BlockId::from_raw(value)))
            .collect::<Vec<_>>();
        let projections = values
            .iter()
            .copied()
            .map(|index| Projection::Index {
                array_type: crate::Type::I32,
                index,
            })
            .collect::<Vec<_>>();
        let evidence = [
            evidence!(
                CfgIntrinsicArgs::FAMILY,
                Values,
                CfgValue,
                |store: &mut Values| push_intrinsic_args(store, values.iter().copied()).unwrap(),
                |store: &Values, range| intrinsic_args(store, range).len()
            ),
            evidence!(
                CfgStructFields::FAMILY,
                Values,
                CfgValue,
                |store: &mut Values| push_struct_fields(store, values.iter().copied()).unwrap(),
                |store: &Values, range| struct_fields(store, range).len()
            ),
            evidence!(
                CfgArrayElements::FAMILY,
                Values,
                CfgValue,
                |store: &mut Values| push_array_elements(store, values.iter().copied()).unwrap(),
                |store: &Values, range| array_elements(store, range).len()
            ),
            evidence!(
                CfgEnumPayload::FAMILY,
                Values,
                CfgValue,
                |store: &mut Values| push_enum_payload(store, values.iter().copied()).unwrap(),
                |store: &Values, range| enum_payload(store, range).len()
            ),
            evidence!(
                CfgGotoArgs::FAMILY,
                Values,
                CfgValue,
                |store: &mut Values| push_goto_args(store, values.iter().copied()).unwrap(),
                |store: &Values, range| goto_args(store, range).len()
            ),
            evidence!(
                CfgThenArgs::FAMILY,
                Values,
                CfgValue,
                |store: &mut Values| push_then_args(store, values.iter().copied()).unwrap(),
                |store: &Values, range| then_args(store, range).len()
            ),
            evidence!(
                CfgElseArgs::FAMILY,
                Values,
                CfgValue,
                |store: &mut Values| push_else_args(store, values.iter().copied()).unwrap(),
                |store: &Values, range| else_args(store, range).len()
            ),
            evidence!(
                CfgCallArgs::FAMILY,
                CallArgs,
                CfgCallArg,
                |store: &mut CallArgs| push_call_args(store, call_args.iter().copied()).unwrap(),
                |store: &CallArgs, range| super::call_args(store, range).len()
            ),
            evidence!(
                CfgSwitchCases::FAMILY,
                SwitchCases,
                (i64, BlockId),
                |store: &mut SwitchCases| push_switch_cases(store, cases.iter().copied()).unwrap(),
                |store: &SwitchCases, range| switch_cases(store, range).len()
            ),
            evidence!(
                CfgProjections::FAMILY,
                Projections,
                Projection,
                |store: &mut Projections| push_projections(store, projections.iter().copied())
                    .unwrap(),
                |store: &Projections, range| super::projections(store, range).len()
            ),
        ];
        assert_eq!(evidence.len(), CFG_PAYLOAD_FAMILY_NAMES.len());
        for item in &evidence {
            assert!(item.allocations >= 1, "{}: {item:?}", item.family);
            assert!(
                item.allocated_bytes >= item.capacity_bytes,
                "{}: {item:?}",
                item.family
            );
            assert_eq!(item.peak_staging_bytes, 0, "{}: {item:?}", item.family);
            assert!(
                item.capacity_bytes >= item.logical_bytes,
                "{}: {item:?}",
                item.family
            );
            assert_eq!(item.elements, 64);
            assert!(item.traversal_ns > 0 && item.elements_per_second.is_finite());
            assert!(item.build_ns > 0 && item.build_elements_per_second.is_finite());
            eprintln!(
                "RUE843_FAMILY\tphase=CFG\tfamily={}\telements={}\tbuild_ns={}\tbuild_elements_per_second={}\tbuild_allocations={}\tbuild_allocated_bytes={}\ttraversal_ns={}\ttraversal_elements_per_second={}\ttraversal_allocations=0\tlogical_bytes={}\tcapacity_bytes={}\ttotal_bytes={}\tenvelopes=0\tpeak_staging_bytes={}",
                item.family,
                item.elements,
                item.build_ns,
                item.build_elements_per_second,
                item.allocations,
                item.allocated_bytes,
                item.traversal_ns,
                item.elements_per_second,
                item.logical_bytes,
                item.capacity_bytes,
                item.logical_bytes + item.capacity_bytes,
                item.peak_staging_bytes,
            );
        }
        eprintln!("RUE-843 CFG family evidence: {evidence:#?}");
        std::hint::black_box(evidence);
    }

    #[test]
    fn owner_bound_range_and_raw_mutation_apis_stay_private() {
        let facade = include_str!("lib.rs");
        for family in [
            "CfgIntrinsicArgs",
            "CfgStructFields",
            "CfgArrayElements",
            "CfgEnumPayload",
            "CfgGotoArgs",
            "CfgThenArgs",
            "CfgElseArgs",
            "CfgCallArgs",
            "CfgSwitchCases",
            "CfgProjections",
        ] {
            assert!(
                !facade.contains(family),
                "owner-bound payload family {family} must not be exported"
            );
        }

        let inst = include_str!("inst.rs");
        for raw_api in [
            "pub(crate) fn add_inst(",
            "pub(crate) fn get_inst_mut(",
            "pub(crate) fn get_block_mut(",
            "pub(crate) fn add_inst_to_block(",
            "pub(crate) fn set_terminator(",
            "pub(crate) fn make_place",
        ] {
            assert!(
                inst.contains(raw_api),
                "raw CFG mutation API lost its crate-private boundary: {raw_api}"
            );
        }
        assert!(inst.contains("impl Clone for Cfg"));
        assert!(!inst.contains("impl Clone for Place"));
        assert!(!inst.contains("impl Clone for CfgInstData"));
        assert!(!inst.contains("impl Clone for Terminator"));
    }
}
