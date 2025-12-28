#![cfg(feature = "backends")]

use core::ptr::NonNull;

/// Reference to an interned string.
///
/// It is inherently `unsafe` to use instances of this type and should not be
/// done outside of the `string-interner` crate itself.
#[derive(Debug)]
#[repr(transparent)]
pub struct InternedStr {
    ptr: NonNull<str>,
}

impl InternedStr {
    /// Creates a new interned string from the given `str`.
    #[inline]
    pub fn new(val: &str) -> Self {
        InternedStr {
            ptr: NonNull::from(val),
        }
    }

    /// Returns a shared reference to the underlying string.
    ///
    /// # Safety
    ///
    /// The user has to make sure that no lifetime guarantees are invalidated.
    #[inline]
    pub(super) fn as_str(&self) -> &str {
        // SAFETY: This is safe since we only ever operate on interned `str`
        //         that are never moved around in memory to avoid danling
        //         references.
        unsafe { self.ptr.as_ref() }
    }
}

impl Eq for InternedStr {}

impl PartialEq for InternedStr {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        self.as_str() == other.as_str()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_of() {
        use core::mem;
        assert_eq!(mem::size_of::<InternedStr>(), mem::size_of::<&str>());
    }
}
