//! String interning for the Rue compiler.
//!
//! This crate provides efficient string interning, storing each unique string
//! once and returning lightweight handles for fast comparison and lookup.
//!
//! Design inspired by Zig's string interning approach for fast compilation.

use std::collections::HashMap;

/// A handle to an interned string.
///
/// This is a lightweight (4 bytes) handle that can be cheaply copied and compared.
/// The actual string data is stored in the [`Interner`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Symbol(u32);

impl Symbol {
    /// Create a symbol from a raw index. Only for internal use.
    #[inline]
    const fn from_raw(index: u32) -> Self {
        Self(index)
    }

    /// Get the raw index of this symbol.
    #[inline]
    pub const fn as_u32(self) -> u32 {
        self.0
    }
}

/// Well-known symbols for built-in types.
///
/// These are pre-interned when creating a new interner, allowing
/// fast symbol comparison instead of string comparison for type resolution.
#[derive(Debug, Clone, Copy)]
pub struct WellKnown {
    /// The `i8` type
    pub i8: Symbol,
    /// The `i16` type
    pub i16: Symbol,
    /// The `i32` type
    pub i32: Symbol,
    /// The `i64` type
    pub i64: Symbol,
    /// The `u8` type
    pub u8: Symbol,
    /// The `u16` type
    pub u16: Symbol,
    /// The `u32` type
    pub u32: Symbol,
    /// The `u64` type
    pub u64: Symbol,
    /// The `bool` type
    pub bool: Symbol,
    /// The `()` unit type (currently not used in syntax)
    pub unit: Symbol,
    /// The `!` never type
    pub never: Symbol,
    /// The `String` type
    pub string: Symbol,
}

impl WellKnown {
    /// Create well-known symbols by interning them.
    fn new(interner: &mut Interner) -> Self {
        Self {
            i8: interner.intern_inner("i8"),
            i16: interner.intern_inner("i16"),
            i32: interner.intern_inner("i32"),
            i64: interner.intern_inner("i64"),
            u8: interner.intern_inner("u8"),
            u16: interner.intern_inner("u16"),
            u32: interner.intern_inner("u32"),
            u64: interner.intern_inner("u64"),
            bool: interner.intern_inner("bool"),
            unit: interner.intern_inner("()"),
            never: interner.intern_inner("!"),
            string: interner.intern_inner("String"),
        }
    }
}

/// String interner that stores unique strings and returns [`Symbol`] handles.
///
/// All strings are stored contiguously in a single buffer for cache efficiency.
/// A separate vector tracks the start offset of each string.
#[derive(Debug)]
pub struct Interner {
    /// Concatenated string data
    data: String,
    /// Start offset of each interned string (end is next start or data.len())
    offsets: Vec<u32>,
    /// Map from string content to symbol for deduplication
    map: HashMap<Box<str>, Symbol>,
    /// Well-known symbols for built-in types
    well_known: Option<WellKnown>,
}

impl Default for Interner {
    fn default() -> Self {
        Self::new()
    }
}

impl Interner {
    /// Create a new interner with well-known symbols pre-interned.
    pub fn new() -> Self {
        let mut interner = Self {
            data: String::new(),
            offsets: Vec::new(),
            map: HashMap::new(),
            well_known: None,
        };
        let well_known = WellKnown::new(&mut interner);
        interner.well_known = Some(well_known);
        interner
    }

    /// Create an interner with pre-allocated capacity.
    pub fn with_capacity(strings: usize, bytes: usize) -> Self {
        let mut interner = Self {
            data: String::with_capacity(bytes),
            offsets: Vec::with_capacity(strings),
            map: HashMap::with_capacity(strings),
            well_known: None,
        };
        let well_known = WellKnown::new(&mut interner);
        interner.well_known = Some(well_known);
        interner
    }

    /// Get the well-known symbols.
    #[inline]
    pub fn well_known(&self) -> &WellKnown {
        self.well_known
            .as_ref()
            .expect("well_known not initialized")
    }

    /// Internal interning used during initialization.
    fn intern_inner(&mut self, s: &str) -> Symbol {
        if let Some(&sym) = self.map.get(s) {
            return sym;
        }

        let start = self.data.len() as u32;
        let sym = Symbol::from_raw(self.offsets.len() as u32);

        self.data.push_str(s);
        self.offsets.push(start);
        self.map.insert(s.into(), sym);

        sym
    }

    /// Intern a string, returning its symbol.
    ///
    /// If the string was already interned, returns the existing symbol.
    /// Otherwise, stores the string and returns a new symbol.
    pub fn intern(&mut self, s: &str) -> Symbol {
        self.intern_inner(s)
    }

    /// Get the string for a symbol.
    ///
    /// # Panics
    /// Panics if the symbol was not created by this interner.
    #[inline]
    pub fn get(&self, sym: Symbol) -> &str {
        let idx = sym.0 as usize;
        let start = self.offsets[idx] as usize;
        let end = self
            .offsets
            .get(idx + 1)
            .map(|&o| o as usize)
            .unwrap_or(self.data.len());
        &self.data[start..end]
    }

    /// Try to get the string for a symbol, returning None if invalid.
    #[inline]
    pub fn try_get(&self, sym: Symbol) -> Option<&str> {
        let idx = sym.0 as usize;
        let start = *self.offsets.get(idx)? as usize;
        let end = self
            .offsets
            .get(idx + 1)
            .map(|&o| o as usize)
            .unwrap_or(self.data.len());
        Some(&self.data[start..end])
    }

    /// Returns the number of interned strings.
    #[inline]
    pub fn len(&self) -> usize {
        self.offsets.len()
    }

    /// Returns true if no strings have been interned.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.offsets.is_empty()
    }

    /// Returns the total bytes used for string storage.
    #[inline]
    pub fn bytes_used(&self) -> usize {
        self.data.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_symbol_size() {
        assert_eq!(std::mem::size_of::<Symbol>(), 4);
    }

    #[test]
    fn test_intern_and_get() {
        let mut interner = Interner::new();
        let sym1 = interner.intern("hello");
        let sym2 = interner.intern("world");
        let sym3 = interner.intern("hello"); // duplicate

        assert_eq!(sym1, sym3); // same string -> same symbol
        assert_ne!(sym1, sym2);

        assert_eq!(interner.get(sym1), "hello");
        assert_eq!(interner.get(sym2), "world");
    }

    #[test]
    fn test_empty_string() {
        let mut interner = Interner::new();
        let sym = interner.intern("");
        assert_eq!(interner.get(sym), "");
    }

    #[test]
    fn test_len() {
        let mut interner = Interner::new();
        // Well-known symbols (i8, i16, i32, i64, u8, u16, u32, u64, bool, (), !, String) are pre-interned
        let initial_len = interner.len();
        assert_eq!(initial_len, 12);

        interner.intern("a");
        assert_eq!(interner.len(), initial_len + 1);

        interner.intern("b");
        assert_eq!(interner.len(), initial_len + 2);

        interner.intern("a"); // duplicate
        assert_eq!(interner.len(), initial_len + 2);
    }
}
