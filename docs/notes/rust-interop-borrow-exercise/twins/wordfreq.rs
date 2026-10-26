// Twin of examples/wordfreq (main.rue + firstseen.rue, 243 lines of Rue).
//
// Same behavior: count ASCII-letter words (lowercased), print
//   words=<total>  distinct=<n>  top=<word> count=<c>
// with ties broken by first appearance.
//
// Rule notes:
//   R1  `counts: HashMap<&[u8], u64>` and `order: Vec<&[u8]>` are LOCALS that
//       hold borrows of `data`. No string pool, no copying of keys.
//   R1  `counts.entry(w)` returns `Entry<'_, ..>`, a lifetime-carrying value
//       held for one match.
//   R2  every signature here is either borrow-free or elision-shaped.
// No macros: output goes through `write_all`, numbers through `to_string`.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::io::Write;

fn out(bytes: &[u8]) {
    let _ = std::io::stdout().write_all(bytes);
}

fn main() {
    let path = match std::env::args().nth(1) {
        Some(p) => p,
        None => {
            out(b"usage: wordfreq <file>\n");
            std::process::exit(1);
        }
    };
    let mut data = match std::fs::read(&path) {
        Ok(d) => d,
        Err(_) => {
            out(b"error: cannot read input file\n");
            std::process::exit(1);
        }
    };
    data.make_ascii_lowercase();

    let mut counts: HashMap<&[u8], u64> = HashMap::new(); // R1: map of borrows
    let mut order: Vec<&[u8]> = Vec::new(); // R1: first-seen list of borrows
    let mut total: u64 = 0;

    // R1: `split` returns a borrowing iterator (core::slice::Split<'_, u8, F>)
    for w in data.split(|b| !b.is_ascii_alphabetic()) {
        if w.is_empty() {
            continue;
        }
        total += 1;
        match counts.entry(w) {
            Entry::Occupied(mut e) => *e.get_mut() += 1,
            Entry::Vacant(e) => {
                e.insert(1);
                order.push(w);
            }
        }
    }

    out(("words=".to_string() + &total.to_string() + "\n").as_bytes());
    out(("distinct=".to_string() + &order.len().to_string() + "\n").as_bytes());

    // Earliest word wins ties: scan in first-seen order, replace on strictly greater.
    let mut best: Option<(&[u8], u64)> = None; // R1: a local Option holding a borrow
    for &w in &order {
        let c = counts[w];
        match best {
            Some((_, bc)) if c <= bc => {}
            _ => best = Some((w, c)),
        }
    }
    if let Some((w, c)) = best {
        out(b"top=");
        out(w);
        out((" count=".to_string() + &c.to_string() + "\n").as_bytes());
    }
}
