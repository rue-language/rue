// Twin of examples/bst.rue ("An arena-backed binary search tree").
//
// Today's Rue version stores nodes in an ArrayBuf arena with u64 child indices
// and a sentinel slot, and reads nodes by copy-out/copy-in, because a node
// cannot be borrowed in place and held. Under R1-R3 the tree is the ordinary
// owning-pointer tree:
//
//   R1  `insert` walks down with a local `&mut Option<Box<Node>>` cursor that is
//       re-borrowed each iteration (NLL accepts this).
//   R1  `sum` keeps an explicit stack of `&Node` in a local Vec.
//   R2  `find(&self, v) -> Option<&Node>` and `iter(&self) -> impl Iterator<Item = &i64>`
//       are elision-shaped.
//   R3  no user type has a lifetime. The in-order iterator is a closure
//       inside `iter::from_fn`: the compiler-generated closure type carries the
//       borrow, so no named borrowing type is needed.
//
// Expected output: 42 (in-order sum), then the sorted keys.

use std::io::Write;

fn out(s: &str) {
    let _ = std::io::stdout().write_all(s.as_bytes());
}

pub struct Node {
    val: i64,
    left: Option<Box<Node>>,
    right: Option<Box<Node>>,
}

pub struct Tree {
    root: Option<Box<Node>>,
}

impl Tree {
    pub fn new() -> Tree {
        Tree { root: None }
    }

    pub fn insert(&mut self, v: i64) {
        let mut cur = &mut self.root; // R1: local &mut cursor
        while let Some(node) = cur {
            cur = if v < node.val { &mut node.left } else { &mut node.right };
        }
        *cur = Some(Box::new(Node { val: v, left: None, right: None }));
    }

    // R2: output borrows from &self
    pub fn find(&self, v: i64) -> Option<&Node> {
        let mut cur = self.root.as_deref(); // R1: Option<&Node> local
        while let Some(n) = cur {
            if v == n.val {
                return Some(n);
            }
            cur = if v < n.val { n.left.as_deref() } else { n.right.as_deref() };
        }
        None
    }

    pub fn sum(&self) -> i64 {
        let mut stack: Vec<&Node> = Vec::new(); // R1: Vec of borrows
        let mut total = 0;
        let mut cur = self.root.as_deref();
        loop {
            while let Some(n) = cur {
                stack.push(n);
                cur = n.left.as_deref();
            }
            match stack.pop() {
                None => break,
                Some(n) => {
                    total += n.val;
                    cur = n.right.as_deref();
                }
            }
        }
        total
    }

    // R2 + R3: a borrowing in-order iterator without a named iterator type.
    pub fn iter(&self) -> impl Iterator<Item = &i64> {
        let mut stack: Vec<&Node> = Vec::new();
        let mut cur = self.root.as_deref();
        std::iter::from_fn(move || {
            while let Some(n) = cur {
                stack.push(n);
                cur = n.left.as_deref();
            }
            let n = stack.pop()?;
            cur = n.right.as_deref();
            Some(&n.val)
        })
    }
}

fn main() {
    let mut t = Tree::new();
    for v in [8, 3, 10, 1, 6, 14] {
        t.insert(v);
    }
    let s = t.sum();
    out(&(s.to_string() + "\n"));
    let mut line = String::new();
    for v in t.iter() {
        line.push_str(&v.to_string());
        line.push(' ');
    }
    out(&(line + "\n"));
    let found = t.find(6).map(|n| n.val).unwrap_or(-1);
    let via_iter: i64 = t.iter().sum();
    if s != 42 || via_iter != 42 || found != 6 {
        std::process::exit(1);
    }
}
