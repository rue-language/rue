use std::ffi::OsStr;

pub(crate) trait OsStrExt: private::Sealed {
    /// Converts to a string slice.
    /// The `Utf8Error` is guaranteed to have a valid UTF8 boundary
    /// in its `valid_up_to()`
    fn try_str(&self) -> Result<&str, std::str::Utf8Error>;
    /// Returns `true` if the given pattern matches a sub-slice of
    /// this string slice.
    ///
    /// Returns `false` if it does not.
    #[allow(dead_code)]
    fn contains(&self, needle: &str) -> bool;
    /// Returns the byte index of the first character of this string slice that
    /// matches the pattern.
    ///
    /// Returns [`None`] if the pattern doesn't match.
    fn find(&self, needle: &str) -> Option<usize>;
    /// Returns a string slice with the prefix removed.
    ///
    /// If the string starts with the pattern `prefix`, returns substring after the prefix, wrapped
    /// in `Some`.
    ///
    /// If the string does not start with `prefix`, returns `None`.
    fn strip_prefix(&self, prefix: &str) -> Option<&OsStr>;
    /// Returns `true` if the given pattern matches a prefix of this
    /// string slice.
    ///
    /// Returns `false` if it does not.
    fn starts_with(&self, prefix: &str) -> bool;
    /// An iterator over substrings of this string slice, separated by
    /// characters matched by a pattern.
    #[allow(dead_code)]
    fn split<'s, 'n>(&'s self, needle: &'n str) -> Split<'s, 'n>;
    /// Splits the string on the first occurrence of the specified delimiter and
    /// returns prefix before delimiter and suffix after delimiter.
    fn split_once(&self, needle: &'_ str) -> Option<(&OsStr, &OsStr)>;
}

impl OsStrExt for OsStr {
    fn try_str(&self) -> Result<&str, std::str::Utf8Error> {
        let bytes = self.as_encoded_bytes();
        std::str::from_utf8(bytes)
    }

    fn contains(&self, needle: &str) -> bool {
        self.find(needle).is_some()
    }

    fn find(&self, needle: &str) -> Option<usize> {
        let bytes = self.as_encoded_bytes();
        (0..=self.len().checked_sub(needle.len())?)
            .find(|&x| bytes[x..].starts_with(needle.as_bytes()))
    }

    fn strip_prefix(&self, prefix: &str) -> Option<&OsStr> {
        let bytes = self.as_encoded_bytes();
        bytes.strip_prefix(prefix.as_bytes()).map(|s| {
            // SAFETY:
            // - This came from `as_encoded_bytes`
            // - Since `prefix` is `&str`, any split will be along UTF-8 boundary
            unsafe { OsStr::from_encoded_bytes_unchecked(s) }
        })
    }
    fn starts_with(&self, prefix: &str) -> bool {
        let bytes = self.as_encoded_bytes();
        bytes.starts_with(prefix.as_bytes())
    }

    fn split<'s, 'n>(&'s self, needle: &'n str) -> Split<'s, 'n> {
        assert_ne!(needle, "");
        Split {
            haystack: Some(self),
            needle,
        }
    }

    fn split_once(&self, needle: &'_ str) -> Option<(&OsStr, &OsStr)> {
        let start = self.find(needle)?;
        let end = start + needle.len();
        let haystack = self.as_encoded_bytes();
        let first = &haystack[0..start];
        let second = &haystack[end..];
        // SAFETY:
        // - This came from `as_encoded_bytes`
        // - Since `needle` is `&str`, any split will be along UTF-8 boundary
        unsafe {
            Some((
                OsStr::from_encoded_bytes_unchecked(first),
                OsStr::from_encoded_bytes_unchecked(second),
            ))
        }
    }
}

mod private {
    pub(crate) trait Sealed {}

    impl Sealed for std::ffi::OsStr {}
}

#[allow(dead_code)]
pub(crate) struct Split<'s, 'n> {
    haystack: Option<&'s OsStr>,
    needle: &'n str,
}

impl<'s> Iterator for Split<'s, '_> {
    type Item = &'s OsStr;

    fn next(&mut self) -> Option<Self::Item> {
        let haystack = self.haystack?;
        match haystack.split_once(self.needle) {
            Some((first, second)) => {
                if !haystack.is_empty() {
                    debug_assert_ne!(haystack, second);
                }
                self.haystack = Some(second);
                Some(first)
            }
            None => {
                self.haystack = None;
                Some(haystack)
            }
        }
    }
}

/// Split an `OsStr`
///
/// # Safety
///
/// `index` must be at a valid UTF-8 boundary
pub(crate) unsafe fn split_at(os: &OsStr, index: usize) -> (&OsStr, &OsStr) {
    unsafe {
        let bytes = os.as_encoded_bytes();
        let (first, second) = bytes.split_at(index);
        (
            OsStr::from_encoded_bytes_unchecked(first),
            OsStr::from_encoded_bytes_unchecked(second),
        )
    }
}
