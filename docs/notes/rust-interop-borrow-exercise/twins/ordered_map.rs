// A library-author twin: an insertion-ordered map, the type wordfreq's
// `FirstSeen` pool and the six example `pool.rue` files approximate today.
// This is where the survey says the rule bites (types and impls, not calls),
// so it implements the full std-style API surface and marks each piece:
//
//   ok      expressible under R1-R3 with no named lifetime
//   refined needs the refined rule (a borrowing type, or impl-header elision)

use std::borrow::Borrow;
use std::collections::HashMap;
use std::fmt;
use std::hash::Hash;
use std::io::Write;
use std::ops::Index;

fn out(s: &str) {
    let _ = std::io::stdout().write_all(s.as_bytes());
}

pub struct OrderedMap<K, V> {
    entries: Vec<(K, V)>,
    index: HashMap<K, usize>,
}

impl<K: Hash + Eq + Clone, V> OrderedMap<K, V> {
    pub fn new() -> Self {
        OrderedMap { entries: Vec::new(), index: HashMap::new() }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    // ok: C0
    pub fn insert(&mut self, k: K, v: V) -> Option<V> {
        match self.index.get(&k) {
            Some(&i) => Some(std::mem::replace(&mut self.entries[i].1, v)),
            None => {
                self.index.insert(k.clone(), self.entries.len());
                self.entries.push((k, v));
                None
            }
        }
    }

    // ok: C2, output borrows from &self
    pub fn get<Q: Hash + Eq + ?Sized>(&self, k: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
    {
        self.index.get(k).map(|&i| &self.entries[i].1)
    }

    // ok: C2
    pub fn get_mut<Q: Hash + Eq + ?Sized>(&mut self, k: &Q) -> Option<&mut V>
    where
        K: Borrow<Q>,
    {
        match self.index.get(k) {
            Some(&i) => Some(&mut self.entries[i].1),
            None => None,
        }
    }

    // ok: C2, the entry-API replacement that needs no Entry<'a> type
    pub fn get_or_insert_with(&mut self, k: K, f: impl FnOnce() -> V) -> &mut V {
        let i = match self.index.get(&k) {
            Some(&i) => i,
            None => {
                self.index.insert(k.clone(), self.entries.len());
                self.entries.push((k, f()));
                self.entries.len() - 1
            }
        };
        &mut self.entries[i].1
    }

    // ok: borrowing iterators as opaque types; the closure carries the borrow
    pub fn iter(&self) -> impl Iterator<Item = (&K, &V)> {
        self.entries.iter().map(|(k, v)| (k, v))
    }
    pub fn keys(&self) -> impl Iterator<Item = &K> {
        self.entries.iter().map(|(k, _)| k)
    }
    pub fn values_mut(&mut self) -> impl Iterator<Item = &mut V> {
        self.entries.iter_mut().map(|(_, v)| v)
    }

    // ok: C1 with a closure bound (Fn sugar elides its own lifetimes)
    pub fn retain(&mut self, mut keep: impl FnMut(&K, &mut V) -> bool) {
        self.entries.retain_mut(|(k, v)| keep(k, v));
        self.index.clear();
        for (i, (k, _)) in self.entries.iter().enumerate() {
            self.index.insert(k.clone(), i);
        }
    }

    // ok: a draining iterator borrows &mut self; opaque, so no Drain<'a> type
    pub fn drain(&mut self) -> impl Iterator<Item = (K, V)> {
        self.index.clear();
        self.entries.drain(..)
    }
}

// ok: impl-header elision in trait arguments (`Index<&Q>`) is plain Rust
impl<K: Hash + Eq + Clone + Borrow<str>, V> Index<&str> for OrderedMap<K, V> {
    type Output = V;
    fn index(&self, k: &str) -> &V {
        self.get(k).expect("key not present")
    }
}

// ok
impl<K: Hash + Eq + Clone, V> Extend<(K, V)> for OrderedMap<K, V> {
    fn extend<I: IntoIterator<Item = (K, V)>>(&mut self, iter: I) {
        for (k, v) in iter {
            self.insert(k, v);
        }
    }
}

// ok
impl<K: Hash + Eq + Clone, V> FromIterator<(K, V)> for OrderedMap<K, V> {
    fn from_iter<I: IntoIterator<Item = (K, V)>>(iter: I) -> Self {
        let mut m = OrderedMap::new();
        m.extend(iter);
        m
    }
}

// ok: by-value IntoIterator, no borrow in Item
impl<K, V> IntoIterator for OrderedMap<K, V> {
    type Item = (K, V);
    type IntoIter = std::vec::IntoIter<(K, V)>;
    fn into_iter(self) -> Self::IntoIter {
        self.entries.into_iter()
    }
}

// ok: Debug through DebugMap, a lifetime-carrying builder held as a temporary
impl<K: fmt::Debug + Hash + Eq + Clone, V: fmt::Debug> fmt::Debug for OrderedMap<K, V> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_map().entries(self.iter()).finish()
    }
}

// refined: `for (k, v) in &map` needs `IntoIterator for &OrderedMap`. Its
// `Item = (&'a K, &'a V)` must name the header's lifetime (rustc: "the lifetime
// must come from the implemented type"), and `IntoIter` must be a nameable
// type, so it needs a borrowing type `Iter<'a, K, V>` too.
pub struct Iter<'a, K, V> {
    inner: std::slice::Iter<'a, (K, V)>,
}

impl<'a, K, V> Iterator for Iter<'a, K, V> {
    type Item = (&'a K, &'a V);
    fn next(&mut self) -> Option<(&'a K, &'a V)> {
        self.inner.next().map(|(k, v)| (k, v))
    }
}

impl<'a, K, V> IntoIterator for &'a OrderedMap<K, V> {
    type Item = (&'a K, &'a V);
    type IntoIter = Iter<'a, K, V>;
    fn into_iter(self) -> Iter<'a, K, V> {
        Iter { inner: self.entries.iter() }
    }
}
// end refined

fn main() {
    let text = "the quick brown fox jumps over the lazy dog the fox";
    let mut counts: OrderedMap<&str, u64> = OrderedMap::new(); // R1: keys borrow `text`
    for w in text.split_whitespace() {
        *counts.get_or_insert_with(w, || 0) += 1;
    }
    let mut line = String::new();
    for (k, v) in counts.iter() {
        line.push_str(k);
        line.push('=');
        line.push_str(&v.to_string());
        line.push(' ');
    }
    out(&(line + "\n"));

    counts.retain(|_, v| *v > 1);
    for v in counts.values_mut() {
        *v *= 10;
    }
    let mut via_ref = String::new();
    for (k, v) in &counts {
        // the refined IntoIterator for &OrderedMap
        via_ref.push_str(k);
        via_ref.push(':');
        via_ref.push_str(&v.to_string());
        via_ref.push(' ');
    }
    out(&(via_ref + "\n"));

    let the = counts["the"];
    let owned: OrderedMap<String, usize> = counts.keys().map(|k| (k.to_string(), k.len())).collect();
    let debug = format_debug(&owned);
    out(&(debug + "\n"));
    let drained: Vec<(&str, u64)> = counts.drain().collect();
    if the != 30 || drained.len() != 2 || counts.len() != 0 {
        std::process::exit(1);
    }
}

// Debug output without the `format!` macro: drive the Debug impl through a
// tiny fmt::Write sink.
fn format_debug(v: &impl fmt::Debug) -> String {
    struct Sink(String);
    impl fmt::Write for Sink {
        fn write_str(&mut self, s: &str) -> fmt::Result {
            self.0.push_str(s);
            Ok(())
        }
    }
    let mut sink = Sink(String::new());
    // `format_args!` is the one macro these twins cannot avoid: it is how std
    // builds `fmt::Arguments` for `{:?}`. Rue would need a builtin for it.
    let _ = fmt::Write::write_fmt(&mut sink, format_args!("{:?}", v));
    sink.0
}
