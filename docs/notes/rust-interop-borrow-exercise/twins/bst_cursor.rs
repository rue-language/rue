// The loop/match form of the bst insert cursor, as the Rue port writes it.
// rustc accepts it like the `while let` form in bst.rs. Exits with 2.
struct Node { val: i64, left: Option<Box<Node>>, right: Option<Box<Node>> }
struct Tree { root: Option<Box<Node>> }
impl Tree {
    fn insert(&mut self, v: i64) {
        let mut cur = &mut self.root;
        loop {
            match cur {
                Some(node) => cur = if v < node.val { &mut node.left } else { &mut node.right },
                None => break,
            }
        }
        *cur = Some(Box::new(Node { val: v, left: None, right: None }));
    }
}
fn main() { let mut t = Tree { root: None }; for v in [3, 1, 2] { t.insert(v); } std::process::exit(t.root.unwrap().left.unwrap().right.unwrap().val as i32); }
