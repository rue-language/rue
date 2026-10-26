// Twin of examples/dijkstra (graph.rue, heap.rue, dijkstra.rue, main.rue).
//
// The Rue version hand-rolls a binary min-heap over ArrayBuf. Here the heap is
// std's BinaryHeap. All data is owned; the only borrows are call arguments and
// a loop over `&g.adj[u]`, so this program never touches the rule's edges.
//
// Expected: the asserted distances of the Rue example, exit code 42 like it.

use std::cmp::Reverse;
use std::collections::BinaryHeap;

pub const INF: u64 = u64::MAX;

pub struct Graph {
    adj: Vec<Vec<(usize, u64)>>,
}

impl Graph {
    pub fn new(n: usize) -> Graph {
        let mut adj = Vec::new();
        adj.resize_with(n, Vec::new);
        Graph { adj }
    }
    pub fn edge(&mut self, from: usize, to: usize, w: u64) {
        self.adj[from].push((to, w));
    }
}

// C1 signature: borrows in, owned out.
pub fn shortest(g: &Graph, src: usize) -> Vec<u64> {
    let mut dist = Vec::new();
    dist.resize(g.adj.len(), INF);
    let mut heap = BinaryHeap::new();
    dist[src] = 0;
    heap.push(Reverse((0u64, src)));
    while let Some(Reverse((d, u))) = heap.pop() {
        if d > dist[u] {
            continue;
        }
        for &(v, w) in &g.adj[u] {
            // calls Rust's `impl<'a, T> IntoIterator for &'a Vec<T>`
            let nd = d + w;
            if nd < dist[v] {
                dist[v] = nd;
                heap.push(Reverse((nd, v)));
            }
        }
    }
    dist
}

fn main() {
    let mut g = Graph::new(6);
    for (a, b, w) in [(0, 1, 7), (0, 2, 9), (0, 5, 14), (1, 2, 10), (1, 3, 15), (2, 3, 11), (2, 5, 2), (3, 4, 6), (5, 4, 9)] {
        g.edge(a, b, w);
    }
    let dist = shortest(&g, 0);
    let ok = dist[4] == 20 && dist[3] == 20 && dist[5] == 11 && dist[1] == 7;
    let unreachable = shortest(&g, 4);
    if !ok || unreachable[0] != INF {
        std::process::exit(1);
    }
    std::process::exit((dist[4] + dist[3] + dist[5] - dist[1] - 2) as i32);
}
