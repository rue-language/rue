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
}

impl fmt::Display for PayloadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "CFG {} payload range {}+{} is outside store of length {}",
            self.family, self.start, self.extent, self.store_len
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
            #[cfg(test)]
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
    fn append<F>(
        &mut self,
        family: &'static str,
        values: impl IntoIterator<Item = E>,
    ) -> Result<Range<F>, PayloadBuildError>
    where
        F: Family<Store = S>,
    {
        let values = values.into_iter();
        let mut staged = Vec::new();
        let (lower_bound, _) = values.size_hint();
        if u32::try_from(lower_bound).is_err() {
            return Err(PayloadBuildError::ResourceLimitExceeded { family });
        }
        staged
            .try_reserve(lower_bound)
            .map_err(|_| PayloadBuildError::CapacityFailure { family })?;
        for value in values {
            if staged.len() == u32::MAX as usize {
                return Err(PayloadBuildError::ResourceLimitExceeded { family });
            }
            staged
                .try_reserve(1)
                .map_err(|_| PayloadBuildError::CapacityFailure { family })?;
            staged.push(value);
        }
        if staged.is_empty() {
            return Ok(Range {
                start: 0,
                extent: 0,
                family: PhantomData,
            });
        }
        let start = u32::try_from(self.elements.len())
            .map_err(|_| PayloadBuildError::ResourceLimitExceeded { family })?;
        let extent = u32::try_from(staged.len())
            .map_err(|_| PayloadBuildError::ResourceLimitExceeded { family })?;
        start
            .checked_add(extent)
            .ok_or(PayloadBuildError::ResourceLimitExceeded { family })?;
        self.elements
            .try_reserve(staged.len())
            .map_err(|_| PayloadBuildError::CapacityFailure { family })?;
        self.elements.extend(staged);
        Ok(Range {
            start,
            extent,
            family: PhantomData,
        })
    }

    fn view<F>(&self, range: &Range<F>, family: &'static str) -> Result<&[E], PayloadError>
    where
        F: Family<Store = S>,
    {
        if range.extent == 0 {
            return if range.start == 0 {
                Ok(&[])
            } else {
                Err(PayloadError {
                    family,
                    start: range.start,
                    extent: 0,
                    store_len: self.elements.len(),
                })
            };
        }
        let start = usize::try_from(range.start).map_err(|_| PayloadError {
            family,
            start: range.start,
            extent: range.extent,
            store_len: self.elements.len(),
        })?;
        let extent = usize::try_from(range.extent).map_err(|_| PayloadError {
            family,
            start: range.start,
            extent: range.extent,
            store_len: self.elements.len(),
        })?;
        let end = start.checked_add(extent).ok_or(PayloadError {
            family,
            start: range.start,
            extent: range.extent,
            store_len: self.elements.len(),
        })?;
        self.elements.get(start..end).ok_or(PayloadError {
            family,
            start: range.start,
            extent: range.extent,
            store_len: self.elements.len(),
        })
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
        pub(crate) fn $push(
            store: &mut $store,
            values: impl IntoIterator<Item = $elem>,
        ) -> Result<$range, PayloadBuildError> {
            store.append($range::FAMILY, values).map($range)
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
    fn typed_ranges_preserve_the_two_word_layout() {
        assert_eq!(std::mem::size_of::<CfgCallArgs>(), 8);
        assert_eq!(std::mem::align_of::<CfgCallArgs>(), 4);
        assert_eq!(std::mem::size_of::<CfgSwitchCases>(), 8);
        assert_eq!(std::mem::size_of::<CfgProjections>(), 8);
    }

    #[test]
    fn every_payload_family_read_traverses_with_exactly_zero_allocations() {
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
            "pub(crate) fn make_place(",
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
