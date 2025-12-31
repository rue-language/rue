#![cfg(feature = "multi-threaded")]

use crate::{Interner, Key, ThreadedRodeo};
use core::hash::{BuildHasher, Hash};

impl<K, S> Interner<K> for &ThreadedRodeo<K, S>
where
    K: Key + Hash,
    S: BuildHasher + Clone,
{
    fn get_or_intern(&mut self, val: &str) -> K {
        ThreadedRodeo::get_or_intern(self, val)
    }

    fn try_get_or_intern(&mut self, val: &str) -> crate::LassoResult<K> {
        ThreadedRodeo::try_get_or_intern(self, val)
    }

    fn get_or_intern_static(&mut self, val: &'static str) -> K {
        ThreadedRodeo::get_or_intern_static(self, val)
    }

    fn try_get_or_intern_static(&mut self, val: &'static str) -> crate::LassoResult<K> {
        ThreadedRodeo::try_get_or_intern_static(self, val)
    }
}

#[cfg(test)]
mod test {
    use super::super::tests::{filled_threaded_rodeo, INTERNED_STRINGS, UNINTERNED_STRINGS};
    use crate::{Key, Resolver, Spur};

    #[test]
    fn threaded_rodeo_ref_trait_implementations() {
        let interner = filled_threaded_rodeo();
        let shared_ref1 = &interner;
        let shared_ref2 = &interner;
        for (key, string) in INTERNED_STRINGS
            .iter()
            .copied()
            .enumerate()
            .map(|(i, s)| (Spur::try_from_usize(i).unwrap(), s))
        {
            assert!(shared_ref1.get(string).is_some());
            assert!(shared_ref2.get(string).is_some());
            assert!(shared_ref1.contains(string));
            assert!(shared_ref2.contains(string));

            assert!(shared_ref1.contains_key(&key));
            assert!(shared_ref2.contains_key(&key));
            assert_eq!(shared_ref1.resolve(&key), string);
            assert_eq!(shared_ref2.resolve(&key), string);
            assert!(shared_ref1.try_resolve(&key).is_some());
            assert!(shared_ref2.try_resolve(&key).is_some());
            assert_eq!(shared_ref1.try_resolve(&key), Some(string));
            assert_eq!(shared_ref2.try_resolve(&key), Some(string));

            unsafe {
                assert_eq!(shared_ref1.resolve_unchecked(&key), string);
                assert_eq!(shared_ref2.resolve_unchecked(&key), string);
            }
        }

        assert_eq!(interner.len(), INTERNED_STRINGS.len());
        for string in UNINTERNED_STRINGS.iter().copied() {
            let key = interner.get_or_intern(string);
            assert_eq!(shared_ref1.try_get_or_intern(string), Ok(key));
            assert_eq!(shared_ref2.try_get_or_intern(string), Ok(key));
            assert_eq!(shared_ref1.get_or_intern_static(string), key);
            assert_eq!(shared_ref2.get_or_intern_static(string), key);
            assert_eq!(shared_ref1.try_get_or_intern_static(string), Ok(key));
            assert_eq!(shared_ref2.try_get_or_intern_static(string), Ok(key));

            assert!(shared_ref1.get(string).is_some());
            assert!(shared_ref2.get(string).is_some());
            assert!(shared_ref1.contains(string));
            assert!(shared_ref2.contains(string));

            assert!(shared_ref1.contains_key(&key));
            assert!(shared_ref2.contains_key(&key));
            assert_eq!(shared_ref1.resolve(&key), string);
            assert_eq!(shared_ref2.resolve(&key), string);
            assert!(shared_ref1.try_resolve(&key).is_some());
            assert!(shared_ref2.try_resolve(&key).is_some());
            assert_eq!(shared_ref1.try_resolve(&key), Some(string));
            assert_eq!(shared_ref2.try_resolve(&key), Some(string));

            unsafe {
                assert_eq!(shared_ref1.resolve_unchecked(&key), string);
                assert_eq!(shared_ref2.resolve_unchecked(&key), string);
            }
        }
        assert_eq!(
            interner.len(),
            INTERNED_STRINGS.len() + UNINTERNED_STRINGS.len(),
        );
    }
}
