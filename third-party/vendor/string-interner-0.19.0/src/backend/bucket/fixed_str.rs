use super::InternedStr;
use alloc::string::String;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct FixedString {
    contents: String,
}

impl FixedString {
    /// Creates a new fixed string with the given fixed capacity.
    #[inline]
    pub fn with_capacity(cap: usize) -> Self {
        Self {
            contents: String::with_capacity(cap),
        }
    }

    /// Returns the underlying [`Box<str>`].
    ///
    /// Guarantees not to perform any reallocations in this process.
    ///
    /// [`Box<str>`]: alloc::boxed::Box
    #[inline]
    pub fn finish(self) -> String {
        self.contents
    }

    /// Returns the capacity in bytes of the fixed string.
    #[inline]
    pub fn capacity(&self) -> usize {
        self.contents.capacity()
    }

    /// Returns the length in bytes of the fixed string.
    #[inline]
    pub fn len(&self) -> usize {
        self.contents.len()
    }

    /// Pushes the given string into the fixed string if there is enough capacity.
    ///
    /// Returns a reference to the pushed string if there was enough capacity to
    /// perform the operation. Otherwise returns `None`.
    #[inline]
    pub fn push_str(&mut self, string: &str) -> Option<InternedStr> {
        let len = self.len();
        if self.capacity() < len + string.len() {
            return None;
        }
        self.contents.push_str(string);
        debug_assert_eq!(self.contents.len(), len + string.len());
        Some(InternedStr::new(
            // SAFETY: We convert from bytes to utf8 from which we know through the
            //         input string that they must represent valid utf8.
            unsafe {
                core::str::from_utf8_unchecked(&self.contents.as_bytes()[len..len + string.len()])
            },
        ))
    }
}
