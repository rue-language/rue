//! String interning for the Rue compiler.
//!
//! This crate provides efficient string interning, storing each unique string
//! once and returning lightweight handles for fast comparison and lookup.
//!
//! Built on the [`string_interner`] crate with a thread-safe wrapper.
//!
//! # Thread Safety
//!
//! The `Interner` is thread-safe and can be shared across threads via `&Interner`.
//! All methods take `&self` (not `&mut self`), enabling concurrent access.
//! This allows parallel compilation phases to share the interner.

use std::sync::RwLock;
use string_interner::backend::BufferBackend;
use string_interner::symbol::SymbolU32;
use string_interner::{StringInterner, Symbol as SymbolTrait};

/// A handle to an interned string.
///
/// This is a lightweight (4 bytes) handle that can be cheaply copied and compared.
/// The actual string data is stored in the [`Interner`].
///
/// Note: This is a newtype wrapper around string_interner's SymbolU32 to maintain
/// our public API and allow for future customization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Symbol(SymbolU32);

impl Symbol {
    /// Create a symbol from a usize index. Only for internal use.
    #[inline]
    fn from_usize(index: usize) -> Option<Self> {
        SymbolU32::try_from_usize(index).map(Symbol)
    }

    /// Create a symbol from a raw index.
    ///
    /// # Panics
    /// Panics if the index is invalid (>= u32::MAX - 1).
    ///
    /// This is intended for use by the RIR extra data deserialization, where
    /// we know the indices are valid because they were serialized from valid symbols.
    #[inline]
    pub fn from_raw(index: u32) -> Self {
        Self::from_usize(index as usize).expect("invalid symbol index")
    }

    /// Get the raw index of this symbol.
    #[inline]
    pub fn as_u32(self) -> u32 {
        self.0.to_usize() as u32
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
    fn new(interner: &Interner) -> Self {
        Self {
            i8: interner.intern("i8"),
            i16: interner.intern("i16"),
            i32: interner.intern("i32"),
            i64: interner.intern("i64"),
            u8: interner.intern("u8"),
            u16: interner.intern("u16"),
            u32: interner.intern("u32"),
            u64: interner.intern("u64"),
            bool: interner.intern("bool"),
            unit: interner.intern("()"),
            never: interner.intern("!"),
            string: interner.intern("String"),
        }
    }
}

/// The underlying string interner type.
///
/// We use BufferBackend which has the best memory efficiency:
/// - Minimal memory footprint (all strings in one buffer)
/// - Fewest allocations
/// - Trade-off: slower symbol resolution (but we rarely resolve)
type InnerInterner = StringInterner<BufferBackend<SymbolU32>>;

/// String interner that stores unique strings and returns [`Symbol`] handles.
///
/// All strings are stored contiguously in a single buffer for cache efficiency.
///
/// # Thread Safety
///
/// The `Interner` is thread-safe. The `intern` method takes `&self` (not `&mut self`),
/// allowing concurrent interning from multiple threads. This is achieved through
/// internal synchronization using `RwLock`.
#[derive(Debug)]
pub struct Interner {
    /// Internal interner protected by RwLock for thread safety
    inner: RwLock<InnerInterner>,
    /// Well-known symbols for built-in types
    well_known: WellKnown,
}

impl Default for Interner {
    fn default() -> Self {
        Self::new()
    }
}

impl Interner {
    /// Create a new interner with well-known symbols pre-interned.
    pub fn new() -> Self {
        let inner = RwLock::new(InnerInterner::new());

        // Create a placeholder symbol for initialization
        let placeholder = Symbol::from_usize(0).unwrap();

        // Create interner without well_known first (placeholder values)
        let interner = Self {
            inner,
            well_known: WellKnown {
                i8: placeholder,
                i16: placeholder,
                i32: placeholder,
                i64: placeholder,
                u8: placeholder,
                u16: placeholder,
                u32: placeholder,
                u64: placeholder,
                bool: placeholder,
                unit: placeholder,
                never: placeholder,
                string: placeholder,
            },
        };

        // Now intern the well-known symbols
        let well_known = WellKnown::new(&interner);

        Self {
            inner: interner.inner,
            well_known,
        }
    }

    /// Create an interner with pre-allocated capacity.
    pub fn with_capacity(strings: usize, _bytes: usize) -> Self {
        // Note: string_interner's with_capacity only takes string count, not byte count
        let inner = RwLock::new(InnerInterner::with_capacity(strings));

        // Create a placeholder symbol for initialization
        let placeholder = Symbol::from_usize(0).unwrap();

        let interner = Self {
            inner,
            well_known: WellKnown {
                i8: placeholder,
                i16: placeholder,
                i32: placeholder,
                i64: placeholder,
                u8: placeholder,
                u16: placeholder,
                u32: placeholder,
                u64: placeholder,
                bool: placeholder,
                unit: placeholder,
                never: placeholder,
                string: placeholder,
            },
        };

        let well_known = WellKnown::new(&interner);

        Self {
            inner: interner.inner,
            well_known,
        }
    }

    /// Get the well-known symbols.
    #[inline]
    pub fn well_known(&self) -> &WellKnown {
        &self.well_known
    }

    /// Intern a string, returning its symbol.
    ///
    /// If the string was already interned, returns the existing symbol.
    /// Otherwise, stores the string and returns a new symbol.
    ///
    /// # Thread Safety
    ///
    /// This method is thread-safe. Multiple threads can call `intern` concurrently.
    /// The method first checks if the string is already interned (read lock), and
    /// only acquires a write lock if the string needs to be added.
    pub fn intern(&self, s: &str) -> Symbol {
        // Fast path: check if already interned (read lock)
        {
            let inner = self.inner.read().unwrap();
            if let Some(sym) = inner.get(s) {
                return Symbol(sym);
            }
        }

        // Slow path: need to intern (write lock)
        let mut inner = self.inner.write().unwrap();

        // Double-check after acquiring write lock (another thread may have interned)
        if let Some(sym) = inner.get(s) {
            return Symbol(sym);
        }

        Symbol(inner.get_or_intern(s))
    }

    /// Get the string for a symbol.
    ///
    /// # Panics
    /// Panics if the symbol was not created by this interner.
    #[inline]
    pub fn get(&self, sym: Symbol) -> &str {
        let inner = self.inner.read().unwrap();
        // SAFETY: We're returning a reference to data inside the RwLock.
        // This is safe because:
        // 1. The string_interner backend uses stable storage (BufferBackend)
        // 2. While we hold the read guard, no writes can occur
        // 3. The underlying buffer is never deallocated or moved
        //
        // We use unsafe here to avoid the lifetime restriction of the RwLock guard.
        unsafe {
            let s = inner
                .resolve(sym.0)
                .expect("invalid symbol: not from this interner");
            // Extend the lifetime beyond the guard
            &*(s as *const str)
        }
    }

    /// Try to get the string for a symbol, returning None if invalid.
    #[inline]
    pub fn try_get(&self, sym: Symbol) -> Option<&str> {
        let inner = self.inner.read().unwrap();
        // SAFETY: Same reasoning as get() - data is stable in BufferBackend
        unsafe { inner.resolve(sym.0).map(|s| &*(s as *const str)) }
    }

    /// Look up a string's symbol without interning it.
    /// Returns None if the string has not been interned.
    #[inline]
    pub fn get_symbol(&self, s: &str) -> Option<Symbol> {
        let inner = self.inner.read().unwrap();
        inner.get(s).map(Symbol)
    }

    /// Returns the number of interned strings.
    #[inline]
    pub fn len(&self) -> usize {
        let inner = self.inner.read().unwrap();
        inner.len()
    }

    /// Returns true if no strings have been interned.
    #[inline]
    pub fn is_empty(&self) -> bool {
        let inner = self.inner.read().unwrap();
        inner.is_empty()
    }

    /// Returns the total bytes used for string storage.
    ///
    /// Note: This is an approximation since string_interner doesn't expose
    /// the exact byte count. We iterate over all strings to calculate it.
    #[inline]
    pub fn bytes_used(&self) -> usize {
        let inner = self.inner.read().unwrap();
        inner.iter().map(|(_, s)| s.len()).sum()
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
        let interner = Interner::new();
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
        let interner = Interner::new();
        let sym = interner.intern("");
        assert_eq!(interner.get(sym), "");
    }

    #[test]
    fn test_len() {
        let interner = Interner::new();
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

    #[test]
    fn test_try_get_returns_none_for_invalid_symbol() {
        let interner = Interner::new();
        // Create an invalid symbol with an index beyond the interner's capacity
        let invalid_sym = Symbol::from_usize(9999).unwrap();
        assert!(interner.try_get(invalid_sym).is_none());
    }

    #[test]
    fn test_try_get_returns_some_for_valid_symbol() {
        let interner = Interner::new();
        let sym = interner.intern("test_string");
        assert_eq!(interner.try_get(sym), Some("test_string"));
    }

    #[test]
    #[should_panic]
    fn test_get_panics_for_invalid_symbol() {
        let interner = Interner::new();
        let invalid_sym = Symbol::from_usize(9999).unwrap();
        let _ = interner.get(invalid_sym); // Should panic
    }

    #[test]
    fn test_with_capacity() {
        let interner = Interner::with_capacity(100, 1000);

        // Well-known symbols should still be pre-interned
        assert_eq!(interner.len(), 12);

        // Should work normally after creation
        let sym = interner.intern("test");
        assert_eq!(interner.get(sym), "test");
        assert_eq!(interner.len(), 13);
    }

    #[test]
    fn test_get_symbol() {
        let interner = Interner::new();

        // Non-existent string returns None
        assert!(interner.get_symbol("nonexistent").is_none());

        // After interning, get_symbol returns the symbol
        let sym = interner.intern("hello");
        assert_eq!(interner.get_symbol("hello"), Some(sym));

        // Well-known symbols can be found
        let wk = interner.well_known();
        assert_eq!(interner.get_symbol("i32"), Some(wk.i32));
        assert_eq!(interner.get_symbol("bool"), Some(wk.bool));
    }

    #[test]
    fn test_well_known_symbols() {
        let interner = Interner::new();
        let wk = interner.well_known();

        // Verify all well-known symbols resolve to correct strings
        assert_eq!(interner.get(wk.i8), "i8");
        assert_eq!(interner.get(wk.i16), "i16");
        assert_eq!(interner.get(wk.i32), "i32");
        assert_eq!(interner.get(wk.i64), "i64");
        assert_eq!(interner.get(wk.u8), "u8");
        assert_eq!(interner.get(wk.u16), "u16");
        assert_eq!(interner.get(wk.u32), "u32");
        assert_eq!(interner.get(wk.u64), "u64");
        assert_eq!(interner.get(wk.bool), "bool");
        assert_eq!(interner.get(wk.unit), "()");
        assert_eq!(interner.get(wk.never), "!");
        assert_eq!(interner.get(wk.string), "String");
    }

    #[test]
    fn test_well_known_symbols_are_unique() {
        let interner = Interner::new();
        let wk = interner.well_known();

        // All well-known symbols should be distinct
        let symbols = [
            wk.i8, wk.i16, wk.i32, wk.i64, wk.u8, wk.u16, wk.u32, wk.u64, wk.bool, wk.unit,
            wk.never, wk.string,
        ];
        for (i, &a) in symbols.iter().enumerate() {
            for (j, &b) in symbols.iter().enumerate() {
                if i != j {
                    assert_ne!(a, b, "well-known symbols at {} and {} should differ", i, j);
                }
            }
        }
    }

    #[test]
    fn test_unicode_strings() {
        let interner = Interner::new();

        // Basic unicode
        let sym1 = interner.intern("héllo");
        assert_eq!(interner.get(sym1), "héllo");

        // Emoji
        let sym2 = interner.intern("🦀");
        assert_eq!(interner.get(sym2), "🦀");

        // CJK characters
        let sym3 = interner.intern("你好");
        assert_eq!(interner.get(sym3), "你好");

        // Mixed unicode and ASCII
        let sym4 = interner.intern("hello_世界_🌍");
        assert_eq!(interner.get(sym4), "hello_世界_🌍");

        // Duplicate unicode string returns same symbol
        let sym5 = interner.intern("héllo");
        assert_eq!(sym1, sym5);
    }

    #[test]
    fn test_bytes_used() {
        let interner = Interner::new();
        let initial_bytes = interner.bytes_used();

        // "test" is 4 bytes
        interner.intern("test");
        assert_eq!(interner.bytes_used(), initial_bytes + 4);

        // "🦀" is 4 bytes (U+1F980)
        interner.intern("🦀");
        assert_eq!(interner.bytes_used(), initial_bytes + 8);

        // Duplicate doesn't add more bytes
        interner.intern("test");
        assert_eq!(interner.bytes_used(), initial_bytes + 8);
    }

    #[test]
    fn test_concurrent_interning() {
        use std::sync::Arc;
        use std::thread;

        let interner = Arc::new(Interner::new());
        let mut handles = vec![];

        // Spawn multiple threads that intern overlapping strings
        for i in 0..4 {
            let interner = Arc::clone(&interner);
            handles.push(thread::spawn(move || {
                let mut symbols = vec![];
                for j in 0..100 {
                    // Each thread interns some unique and some shared strings
                    let unique = format!("thread_{}_string_{}", i, j);
                    let shared = format!("shared_string_{}", j % 10);

                    symbols.push((interner.intern(&unique), unique));
                    symbols.push((interner.intern(&shared), shared));
                }
                symbols
            }));
        }

        // Collect all results
        let mut all_symbols = vec![];
        for handle in handles {
            all_symbols.extend(handle.join().unwrap());
        }

        // Verify all symbols resolve correctly
        for (sym, expected) in &all_symbols {
            assert_eq!(interner.get(*sym), expected.as_str());
        }

        // Verify shared strings got the same symbol
        for i in 0..10 {
            let shared = format!("shared_string_{}", i);
            let sym = interner.get_symbol(&shared).unwrap();
            for (s, expected) in &all_symbols {
                if expected == &shared {
                    assert_eq!(*s, sym, "shared string should have same symbol");
                }
            }
        }
    }

    #[test]
    fn test_intern_takes_shared_ref() {
        // This test verifies the API change: intern takes &self, not &mut self
        let interner = Interner::new();

        // We can hold multiple references and intern
        let _ref1 = &interner;
        let _ref2 = &interner;

        // All of these should work without &mut
        let sym = interner.intern("test");
        let _ = interner.get(sym);
        let _ = interner.get_symbol("test");
        let _ = interner.len();
    }
}
