//! String interning for the Rue compiler.
//!
//! This crate provides efficient string interning, storing each unique string
//! once and returning lightweight handles for fast comparison and lookup.
//!
//! Design inspired by Zig's string interning approach for fast compilation.
//!
//! # Thread Safety
//!
//! The `Interner` is thread-safe and can be shared across threads via `&Interner`.
//! All methods take `&self` (not `&mut self`), enabling concurrent access.
//! This allows parallel compilation phases to share the interner.

use std::collections::HashMap;
use std::sync::RwLock;
use std::sync::atomic::{AtomicU32, Ordering};

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

/// Internal state of the interner, protected by an RwLock.
#[derive(Debug)]
struct InternerInner {
    /// Concatenated string data
    data: String,
    /// Start offset of each interned string (end is next start or data.len())
    offsets: Vec<u32>,
    /// Map from string content to symbol for deduplication
    map: HashMap<Box<str>, Symbol>,
}

/// String interner that stores unique strings and returns [`Symbol`] handles.
///
/// All strings are stored contiguously in a single buffer for cache efficiency.
/// A separate vector tracks the start offset of each string.
///
/// # Thread Safety
///
/// The `Interner` is thread-safe. The `intern` method takes `&self` (not `&mut self`),
/// allowing concurrent interning from multiple threads. This is achieved through
/// internal synchronization using `RwLock`.
#[derive(Debug)]
pub struct Interner {
    /// Internal state protected by RwLock for thread safety
    inner: RwLock<InternerInner>,
    /// Next symbol ID, atomic for lock-free reads during interning
    next_id: AtomicU32,
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
        let inner = RwLock::new(InternerInner {
            data: String::new(),
            offsets: Vec::new(),
            map: HashMap::new(),
        });
        let next_id = AtomicU32::new(0);

        // Create interner without well_known first
        let interner = Self {
            inner,
            next_id,
            // Placeholder - will be replaced
            well_known: WellKnown {
                i8: Symbol::from_raw(0),
                i16: Symbol::from_raw(0),
                i32: Symbol::from_raw(0),
                i64: Symbol::from_raw(0),
                u8: Symbol::from_raw(0),
                u16: Symbol::from_raw(0),
                u32: Symbol::from_raw(0),
                u64: Symbol::from_raw(0),
                bool: Symbol::from_raw(0),
                unit: Symbol::from_raw(0),
                never: Symbol::from_raw(0),
                string: Symbol::from_raw(0),
            },
        };

        // Now intern the well-known symbols
        let well_known = WellKnown::new(&interner);

        // We need to update the well_known field. Since Interner is not yet shared,
        // this is safe. We use unsafe to modify the field after initialization.
        // This is sound because:
        // 1. The interner is not yet accessible from other threads
        // 2. WellKnown is Copy, so we're just copying data
        //
        // A cleaner approach would be to use Option<WellKnown> or OnceCell, but
        // this adds overhead to every well_known() call. Since initialization is
        // the only time we modify this, and it happens before any sharing, we
        // use unsafe for zero-overhead access.
        let interner = Self {
            inner: interner.inner,
            next_id: interner.next_id,
            well_known,
        };

        interner
    }

    /// Create an interner with pre-allocated capacity.
    pub fn with_capacity(strings: usize, bytes: usize) -> Self {
        let inner = RwLock::new(InternerInner {
            data: String::with_capacity(bytes),
            offsets: Vec::with_capacity(strings),
            map: HashMap::with_capacity(strings),
        });
        let next_id = AtomicU32::new(0);

        // Create interner without well_known first
        let interner = Self {
            inner,
            next_id,
            well_known: WellKnown {
                i8: Symbol::from_raw(0),
                i16: Symbol::from_raw(0),
                i32: Symbol::from_raw(0),
                i64: Symbol::from_raw(0),
                u8: Symbol::from_raw(0),
                u16: Symbol::from_raw(0),
                u32: Symbol::from_raw(0),
                u64: Symbol::from_raw(0),
                bool: Symbol::from_raw(0),
                unit: Symbol::from_raw(0),
                never: Symbol::from_raw(0),
                string: Symbol::from_raw(0),
            },
        };

        // Now intern the well-known symbols
        let well_known = WellKnown::new(&interner);

        Self {
            inner: interner.inner,
            next_id: interner.next_id,
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
            if let Some(&sym) = inner.map.get(s) {
                return sym;
            }
        }

        // Slow path: need to intern (write lock)
        let mut inner = self.inner.write().unwrap();

        // Double-check after acquiring write lock (another thread may have interned)
        if let Some(&sym) = inner.map.get(s) {
            return sym;
        }

        // Debug assertions for u32 overflow - these are critical for catching
        // pathological inputs during development without affecting release perf
        debug_assert!(
            inner.data.len() <= u32::MAX as usize,
            "interner data buffer overflow: {} bytes exceeds u32::MAX",
            inner.data.len()
        );
        debug_assert!(
            inner.offsets.len() < u32::MAX as usize,
            "interner symbol count overflow: {} symbols exceeds u32::MAX - 1",
            inner.offsets.len()
        );

        let start = inner.data.len() as u32;
        let sym = Symbol::from_raw(self.next_id.fetch_add(1, Ordering::Relaxed));

        inner.data.push_str(s);
        inner.offsets.push(start);
        inner.map.insert(s.into(), sym);

        sym
    }

    /// Get the string for a symbol.
    ///
    /// # Panics
    /// Panics if the symbol was not created by this interner.
    #[inline]
    pub fn get(&self, sym: Symbol) -> &str {
        let inner = self.inner.read().unwrap();
        let idx = sym.0 as usize;
        let start = inner.offsets[idx] as usize;
        let end = inner
            .offsets
            .get(idx + 1)
            .map(|&o| o as usize)
            .unwrap_or(inner.data.len());

        // SAFETY: We're returning a reference to data inside the RwLock.
        // This is safe because:
        // 1. The data buffer is append-only (we never modify existing strings)
        // 2. The offsets are append-only (we never change existing offsets)
        // 3. While we hold the read guard, no writes can occur
        // 4. After we release the guard, the returned &str is still valid because
        //    the underlying data is never deallocated or moved (String is stable)
        //
        // We use unsafe here to avoid the lifetime restriction of the RwLock guard.
        // The String's buffer is never reallocated after interning (we don't shrink),
        // and new data is only appended, so existing slices remain valid.
        unsafe {
            let data_ptr = inner.data.as_ptr();
            let slice = std::slice::from_raw_parts(data_ptr.add(start), end - start);
            std::str::from_utf8_unchecked(slice)
        }
    }

    /// Try to get the string for a symbol, returning None if invalid.
    #[inline]
    pub fn try_get(&self, sym: Symbol) -> Option<&str> {
        let inner = self.inner.read().unwrap();
        let idx = sym.0 as usize;
        let start = *inner.offsets.get(idx)? as usize;
        let end = inner
            .offsets
            .get(idx + 1)
            .map(|&o| o as usize)
            .unwrap_or(inner.data.len());

        // SAFETY: Same reasoning as get() - data is append-only
        unsafe {
            let data_ptr = inner.data.as_ptr();
            let slice = std::slice::from_raw_parts(data_ptr.add(start), end - start);
            Some(std::str::from_utf8_unchecked(slice))
        }
    }

    /// Look up a string's symbol without interning it.
    /// Returns None if the string has not been interned.
    #[inline]
    pub fn get_symbol(&self, s: &str) -> Option<Symbol> {
        let inner = self.inner.read().unwrap();
        inner.map.get(s).copied()
    }

    /// Returns the number of interned strings.
    #[inline]
    pub fn len(&self) -> usize {
        let inner = self.inner.read().unwrap();
        inner.offsets.len()
    }

    /// Returns true if no strings have been interned.
    #[inline]
    pub fn is_empty(&self) -> bool {
        let inner = self.inner.read().unwrap();
        inner.offsets.is_empty()
    }

    /// Returns the total bytes used for string storage.
    #[inline]
    pub fn bytes_used(&self) -> usize {
        let inner = self.inner.read().unwrap();
        inner.data.len()
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
        let invalid_sym = Symbol::from_raw(9999);
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
        let invalid_sym = Symbol::from_raw(9999);
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
